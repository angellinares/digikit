#!/usr/bin/env python3
"""patchkit smoke harness: drive a (patched) firmware in the control daemon and judge it.

    uv run python -m tools.patchkit.pk_harness run [--matrix FILE] [--cell DEV-FW:mod,mod ...] [--update]
                                          [--tests smoke|quick|all] [--seed N] [--stress-actions N]
    uv run python -m tools.patchkit.pk_harness list                 # the cells of the matrix
    mise run patch:test [-- --matrix FILE]

A *cell* is device x firmware x module set (matrix TOML `[[cell]]`: `device`, `firmware`, `modules`, optional
`stall_ms`). For each cell the harness builds the patched image (`tools/patchkit/patchkit.py patch`; stock when the set is
empty), spawns a daemon on the stock firmware's ready inputs, loads the image (`firmware.load`, DSP-only changes hot,
a MAIN change by the cached coupled cold boot), and runs *tours*: `tests/patchkit/smoke/tours/smoke.toml` (boot to the
readiness contract, track and pattern change, audition, play/stop with the sequencer running, machine menu open and
close, save project and reload, a seeded random-input stress) and the tests of the modules in the set
(`modules/**/tests/*.toml`). Every segment starts from `restart`, so segments do not depend on each other.

After every input and every segment the harness asserts: no `fault` (status.cf.error, GUEST_FAULT), no DSP halt, no
main-loop stall (status.heartbeat, the controller's `stall` events), and its own checks: `expect_frame` against the
golden SHA-256 (tests/patchkit/smoke/goldens.json, keyed `DEV-FW/<module set>/<segment>/<checkpoint>`; **only hashes are
committed**, the PNGs go to out/runs/patchkit/frames/), `expect_pcm_db` over the PCM tap's 100 ms windows.

Step format (TOML `[[segment.step]]` / `[[step]]`, exactly one action key each; names are the panel names of
`BUTTONS`, case-insensitive; numbers are raw codes):

    tap = "TRIG 1"                 press and release (hold_ms, default 20)
    tap = ["TRK", "TRIG 2"]        chord: the first key is held while the others are tapped
    hold = "PLAY"  ms = 500        press, run ms, release
    turn = "ENC A"  by = 2         encoder rotation (ENC A..H, LEVEL); negative = left; `events = 30` sends a burst of
                                   30 events 2 ms apart (the encoder accelerates by event spacing: one slow event
                                   moves a value by about nothing)
    wait_ms = 300                  run that many emulated milliseconds
    expect_frame = "name"          hash of the OLED frame against the golden (PNG to out/); `differs_from = "other"`
    expect_pcm_db = {min = -60, max = -3}   peak 100 ms-window RMS (dBFS) since the last input started
    expect_no_fault = true         assert the health now (it is also asserted after every input)
    expect_mem = {addr = 0x4265338c, mask = 0xff00, eq = 0x0700}   a ColdFire word (mem.cf.peek) under a mask;
                                   `symbol = "STORE", module = "core/digikit-menu"` instead of `addr`: a symbol of the
                                   image's plan (the module's static; `module` is optional)
    expect_text = {rows = "DK_ABOUT", count = "DK_ABOUT_N", stride = 24, has = ["RUNTIME {runtime}"]}
                                   the text rows a module keeps in RAM (NUL-terminated, STRIDE bytes each, COUNT rows) must
                                   have every `has` string as a row; `{runtime}` is the runtime version of sdk/sdk.toml
    expect_dsp = {addr = 0x255970, hex = "07"}                     DSP memory bytes (dsp.peek) from addr
    stress = {seed = 1, actions = 40}       seeded random inputs, health after each

`device = "dt2"` on a step limits it to one device. A module test file has `[test]` (`name`, `devices`, `quick`) and
`[[step]]`.

The daemon comes from `tools/emulator/native_emu.py --daemon --build-only` for the device's generated core (cached; shares the
machine through benchlock). Ready inputs must be fresh: SOURCE.json must say `intc_force: false` and name the base .syx.
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import math
import os
import random
import re
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
from collections.abc import Callable, Iterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent

from tools import python_env  # noqa: E402
from tools.control.digi_client import Emu, EmuError  # noqa: E402
from tools.emulator import card_check  # noqa: E402
from tools.firmware import config  # noqa: E402
from tools.patchkit import modroots, pk_dsp_budget  # noqa: E402

MS = 132_000  # ColdFire instructions per emulated millisecond (the oracle clock)
OUT = ROOT / "out" / "runs" / "patchkit"
GOLDENS = ROOT / "tests" / "patchkit" / "smoke" / "goldens.json"
TOURS = ROOT / "tests" / "patchkit" / "smoke" / "tours"
DEFAULT_MATRIX = ROOT / "tests" / "patchkit" / "smoke" / "matrix.toml"
DEFAULT_STALL_MS = 1000
SHARED_CACHE = (
    ROOT / "out" / "runs" / ".cache"
)  # boot pairs and cards shared by the daemons of all proof runs
# The last passing OLED frame of each golden, kept locally (never committed: it is firmware output): `<cell>/<segment>/<name>.<sha256>.oled`,
# the raw 1024 bytes. A mismatch is explained against it (`frame_diff`).
SMOKE_FRAMES = ROOT / "out" / "cache" / "smoke-frames"
MAX_JOBS = 6
MIN_JOBS = 4


def part_jobs(parts: int) -> int:
    """How many of PARTS independent parts of one scenario (each boots its own daemon) run at once:
    DIGIKIT_PART_JOBS, else all of them. verify sets it to 1: a pool of N files must not start 3 or 4
    daemons each (1.3 busy cores per daemon, measured) beyond the cores of its share."""
    env = os.environ.get("DIGIKIT_PART_JOBS")
    return max(1, min(parts, int(env))) if env else max(1, parts)


def performance_cores() -> int:
    """Logical performance cores (Apple silicon: the efficiency cores run a coupled daemon several times slower)."""
    try:
        out = subprocess.run(
            ["sysctl", "-n", "hw.perflevel0.logicalcpu"],
            capture_output=True,
            text=True,
            timeout=5,
        ).stdout.strip()
        return max(int(out), 1)
    except (OSError, ValueError, subprocess.SubprocessError):
        return os.cpu_count() or 2


def default_jobs(n: int) -> int:
    """Processes to run side by side: the free performance cores / 2 (a coupled daemon keeps two threads busy: the
    ColdFire and the DSP worker), at most MAX_JOBS, and never fewer than MIN_JOBS (bounded by the cores / 2) when
    there are two or more parts: a busy machine slows the parts down, but running them in sequence is slower.
    DIGI_PK_JOBS (the Nx gate targets set it) replaces all that."""
    cap = os.environ.get("DIGI_PK_JOBS")  # set by the Nx gate targets
    if cap:
        return max(1, min(n, int(cap)))
    cores = performance_cores()
    free = cores - os.getloadavg()[0]
    floor = min(MIN_JOBS, max(1, cores // 2))
    return max(1, min(n, max(int(free // 2), floor), MAX_JOBS))


# firmware: (device, version) -> base .syx and the ready directory (override: DIGI_PK_READY_<DEV>_<VER>)
FIRMWARE: dict[tuple[str, str], dict[str, str]] = {
    ("dt2", "1.17"): {
        "syx": "Digitakt_II_OS1.17.syx",
        "ready": "out/ready-pk-dt2-117",
    },
    ("dn2", "1.11"): {
        "syx": "Digitone_II_OS1.11.syx",
        "ready": "out/ready-dn2-111",
    },
    ("dn2", "1.12"): {
        "syx": "Digitone_II_OS1.12.syx",
        "ready": "out/ready-fix-dn2-112",
    },
}

BUTTONS: dict[str, int] = {
    "PAGE TRIG": 1,
    "SRC": 2,
    "SYN": 2,
    "FLTR": 3,
    "AMP": 4,
    "FX": 5,
    "MOD": 6,
    "PRESET": 7,
    "SETTINGS": 8,
    "TEMPO": 9,
    "YES": 10,
    "UP": 11,
    "NO": 12,
    "LEFT": 13,
    "DOWN": 14,
    "RIGHT": 15,
    "TRK": 16,
    "FUNC": 17,
    "KEYBOARD": 18,
    "RECORD": 19,
    "PLAY": 20,
    "STOP": 21,
    "PAGE": 22,
    "PTN": 23,
    "SONG": 24,
    "LEVEL PUSH": 49,
    **{f"TRIG {n}": 24 + n for n in range(1, 17)},
    **{f"PUSH {c}": 41 + i for i, c in enumerate("ABCDEFGH")},
}
ENCODERS: dict[str, int] = {
    **{f"ENC {c}": 1 + i for i, c in enumerate("ABCDEFGH")},
    "LEVEL": 9,
}


def _norm(name: str) -> str:
    return re.sub(r"[\s_]+", " ", name.strip().upper())


def button(name: str | int) -> int:
    if isinstance(name, int):
        return name
    try:
        return BUTTONS[_norm(name)]
    except KeyError:
        raise HarnessError(f"unknown button {name!r}") from None


def encoder(name: str | int) -> int:
    if isinstance(name, int):
        return name
    try:
        return ENCODERS[_norm(name)]
    except KeyError:
        raise HarnessError(f"unknown encoder {name!r}") from None


class HarnessError(RuntimeError):
    """A setup problem (missing input, bad step): not a firmware verdict."""


class LoadFailure(Exception):
    """The image did not come up (cold boot fault, loader refusal): a verdict on the image."""


@dataclass
class Cell:
    device: str
    firmware: str
    modules: list[str] = field(default_factory=list)
    stall_ms: int = DEFAULT_STALL_MS

    @property
    def modset(self) -> str:
        return "+".join(sorted(self.modules)) or "stock"

    @property
    def name(self) -> str:
        return f"{self.device}-{self.firmware}:{self.modset}"

    @property
    def slug(self) -> str:
        return re.sub(r"[^A-Za-z0-9.+-]+", "_", self.name)

    @property
    def golden_prefix(self) -> str:
        return f"{self.device}-{self.firmware}/{self.modset}"


def parse_cell(text: str) -> Cell:
    """`dt2-1.17:core/fx,test/cf-demo` (no module part: stock)."""
    head, _, mods = text.partition(":")
    device, _, firmware = head.partition("-")
    return Cell(device, firmware, [m for m in mods.split(",") if m and m != "stock"])


def load_matrix(path: Path) -> list[Cell]:
    data = tomllib.loads(path.read_text())
    cells = []
    for c in data.get("cell", []):
        cells.append(
            Cell(
                c["device"],
                c["firmware"],
                list(c.get("modules", [])),
                int(c.get("stall_ms", DEFAULT_STALL_MS)),
            )
        )
    return cells


# ----- inputs ---------------------------------------------------------------------------------------------------


def firmware_inputs(cell: Cell) -> tuple[Path, Path]:
    """(base .syx, ready directory), checked: the ready state must be fresh and belong to the .syx."""
    try:
        spec = FIRMWARE[(cell.device, cell.firmware)]
    except KeyError:
        raise HarnessError(
            f"no firmware entry for {cell.device} {cell.firmware}"
        ) from None
    syx = config.stock(spec["syx"])
    env = os.environ.get(
        f"DIGI_PK_READY_{cell.device.upper()}_{cell.firmware.replace('.', '')}"
    )
    ready = Path(env) if env else ROOT / spec["ready"]
    if not syx.is_file():
        raise HarnessError(f"{syx} is missing")
    try:
        source = json.loads((ready / "SOURCE.json").read_text())
    except (OSError, ValueError):
        raise HarnessError(
            f"{ready}: no SOURCE.json; build it with `uv run python -m tools.emulator.ready {cell.device} "
            f"--syx {syx.name} --out {ready} --force`"
        ) from None
    if source.get("intc_force") is not False:
        raise HarnessError(
            f"{ready}: built before the INTC-force fix (no intc_force: false); rebuild with tools/emulator/ready.py --force"
        )
    if source.get("syx_sha256") != hashlib.sha256(syx.read_bytes()).hexdigest():
        raise HarnessError(f"{ready} was built from another .syx than {syx.name}")
    for name in (
        "digi-audio-dn2-image.bin",
        "digi-audio-m5.snap",
        "digi-audio-m5.snap.dsp",
    ):
        if not (ready / name).is_file():
            raise HarnessError(f"{ready}/{name} is missing")
    return syx, ready


_BINARIES: dict[tuple[str, bool], Path] = {}


def daemon_binary(device: str, syx: Path, pgo: bool | None = None) -> Path:
    """The `digi-emud` for the device's generated core (built when stale; the build holds benchlock).

    The profile-guided build of tools/patchkit/pk_emud_pgo.py is used when it exists (PGO None); DIGI_PK_PGO=0 or `pgo=False`
    picks the plain build. DIGI_EMUD_<DEV> / DIGI_EMUD name a binary outright."""
    override = os.environ.get(f"DIGI_EMUD_{device.upper()}") or os.environ.get(
        "DIGI_EMUD"
    )
    if override:
        return Path(override)
    if pgo is None:
        pgo = os.environ.get("DIGI_PK_PGO") != "0"
    if pgo and (device, True) not in _BINARIES:
        try:
            from tools.patchkit import pk_emud_pgo

            path = pk_emud_pgo.binary_path(device)
            if path.is_file():
                # cargo is a no-op when the sources have not changed since the build
                path = pk_emud_pgo.build(device)
                _BINARIES[(device, True)] = path
        except (SystemExit, Exception) as e:  # noqa: BLE001  a PGO problem must not stop a proof
            print(
                f"note: no PGO daemon for {device} ({e}); using the plain build",
                file=sys.stderr,
            )
        if (device, True) not in _BINARIES:
            _BINARIES[(device, True)] = daemon_binary(device, syx, pgo=False)
    if pgo:
        return _BINARIES[(device, True)]
    if (device, False) not in _BINARIES:
        r = subprocess.run(
            [
                sys.executable,
                "-m",
                "tools.emulator.native_emu",
                "--daemon",
                "--build-only",
                str(syx),
            ],
            cwd=ROOT,
            env=python_env(),
            capture_output=True,
            text=True,
        )
        lines = [ln for ln in r.stdout.splitlines() if ln.strip()]
        if r.returncode or not lines or not Path(lines[-1]).is_file():
            raise HarnessError(
                f"cannot build digi-emud for {device}:\n{(r.stderr or r.stdout)[-1500:]}"
            )
        _BINARIES[(device, False)] = Path(lines[-1])
    return _BINARIES[(device, False)]


def patched_image(cell: Cell, syx: Path, dest: Path | None = None) -> Path:
    """The stock .syx, or the patch of the cell's modules (out/patch/pk-*.syx; DEST is a private directory for
    processes that run side by side, so none reads an image another is writing)."""
    if not cell.modules:
        return syx
    out = (dest or ROOT / "out" / "patch") / f"pk-{cell.slug}.syx"
    out.parent.mkdir(parents=True, exist_ok=True)
    r = subprocess.run(
        [
            sys.executable,
            "-m",
            "tools.patchkit.patchkit",
            "patch",
            "--firmware",
            str(syx),
            "--modules",
            ",".join(cell.modules),
            "-o",
            str(out),
        ],
        cwd=ROOT,
        env=python_env(),
        capture_output=True,
        text=True,
    )
    if r.returncode:
        raise HarnessError(
            f"patch failed for {cell.name}:\n{(r.stderr or r.stdout)[-1500:]}"
        )
    return out


# ----- shared boot cache ----------------------------------------------------------------------------------------


def temp_run_dir(name: str) -> Path:
    """A fresh run directory in the system temp dir; `finish_run_dir` removes it after a clean run."""
    return Path(tempfile.mkdtemp(prefix=f"pk-run-{name}-"))


def finish_run_dir(run_dir: Path, ok: bool) -> None:
    """Remove a temporary run dir after a clean run; keep it (and say so) after a failure. Symlinked caches stay."""
    if ok:
        shutil.rmtree(run_dir, ignore_errors=True)
    else:
        print(f"run dir kept: {run_dir}", file=sys.stderr)


@contextlib.contextmanager
def run_dir_scope(
    name: str, explicit: str | Path | None = None, cache_dir: Path = SHARED_CACHE
) -> Iterator[Path]:
    """The run dir of one daemon: EXPLICIT (kept), else a temp dir linked to CACHE_DIR, removed after a clean exit."""
    if explicit:
        yield Path(explicit)
        return
    run_dir = temp_run_dir(name)
    link_shared(run_dir, cache_dir)
    ok = False
    try:
        yield run_dir
        ok = True
    finally:
        finish_run_dir(run_dir, ok)


def link_shared(run_dir: Path, cache_dir: Path) -> None:
    """Make RUN_DIR/patched and RUN_DIR/cards the shared `cache_dir/patched` and `cache_dir/cards-store`. The daemon
    caches the coupled cold boot of a patched MAIN under `patched` (a pair of snapshots keyed by the MAIN, the DSP
    inputs and the card path), so every daemon of a proof run restores it (0.3 s) instead of booting cold (13 s);
    `cards` holds the sample card built from the sample folder (about 15 s) and its ready state, content-keyed."""
    run_dir.mkdir(parents=True, exist_ok=True)
    for name, shared_name in (("patched", "patched"), ("cards", "cards-store")):
        shared = cache_dir / shared_name
        shared.mkdir(parents=True, exist_ok=True)
        link = run_dir / name
        if link.is_symlink() and link.resolve() == shared.resolve():
            continue
        if link.is_symlink() or link.is_file():
            link.unlink()
        elif link.is_dir():  # an older private cache: keep it aside
            link.rename(run_dir / f"{name}.private-{int(time.time())}")
        link.symlink_to(shared.resolve())


def canonical_card(cache_dir: Path, card: str) -> str:
    """The card path the pair key hashes is the card file's path: give every daemon the same path (a symlink in
    CACHE_DIR to the first daemon's image; the card is read-only, the guest's writes live in an overlay)."""
    d = cache_dir / "cards"
    d.mkdir(parents=True, exist_ok=True)
    real = Path(card)
    link = d / (real.parent.name + "-" + real.name)
    for _ in range(3):
        if (
            link.is_symlink() and not link.exists()
        ):  # the first daemon's run dir is gone
            link.unlink(missing_ok=True)
        if not link.is_symlink():
            tmp = d / f".{link.name}.{os.getpid()}"
            tmp.unlink(missing_ok=True)
            tmp.symlink_to(real.resolve())
            os.replace(
                tmp, link
            )  # atomic: concurrent first daemons both end with a valid link
        if link.exists():
            return str(link)
    return card


@contextlib.contextmanager
def boot_lock(cache_dir: Path | None, image: Path) -> Iterator[None]:
    """One process at a time loads a given image: the first boots cold and caches the pair, the others wait and
    restore it. Without a cache dir: no lock."""
    if cache_dir is None:
        yield
        return
    import fcntl

    lock_dir = cache_dir / "locks"
    lock_dir.mkdir(parents=True, exist_ok=True)
    with (lock_dir / f"{image.name}.lock").open("w") as f:
        fcntl.flock(f, fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(f, fcntl.LOCK_UN)


# ----- frame diagnostics ------------------------------------------------------------------------------------------


def oled_pixel(frame: bytes, x: int, y: int) -> int:
    """Pixel (x, y) of a raw 128x64 OLED frame: byte (7 - y / 8) + 8 x, bit y % 8 (`digi_client.oled_png`)."""
    return frame[(7 - y // 8) + 8 * x] >> (y % 8) & 1


def frame_diff(
    got: bytes, want: bytes
) -> tuple[int, tuple[int, int, int, int] | None, bytes]:
    """(pixels that differ, their bounding box (x0, y0, x1, y1) inclusive in OLED pixels, a raw frame with just those pixels set)."""
    changed = bytearray(len(got))
    n = 0
    box: list[int] | None = None
    for x in range(128):
        for y in range(64):
            if oled_pixel(got, x, y) != oled_pixel(want, x, y):
                changed[(7 - y // 8) + 8 * x] |= 1 << (y % 8)
                n += 1
                box = (
                    [x, y, x, y]
                    if box is None
                    else [
                        min(box[0], x),
                        min(box[1], y),
                        max(box[2], x),
                        max(box[3], y),
                    ]
                )
    return n, (tuple(box) if box else None), bytes(changed)  # type: ignore[return-value]


def _frame_file(cell: str, segment: str, name: str, digest: str) -> Path:
    return SMOKE_FRAMES / cell / segment.replace("/", "_") / f"{name}.{digest}.oled"


def remember_frame(cell: str, segment: str, name: str, digest: str, raw: bytes) -> None:
    """Keep RAW as the frame of the golden DIGEST (the last one that matched or was recorded; an older frame of the same checkpoint goes)."""
    path = _frame_file(cell, segment, name, digest)
    path.parent.mkdir(parents=True, exist_ok=True)
    for old in path.parent.glob(f"{name}.*.oled"):
        if old != path:
            old.unlink(missing_ok=True)
    path.write_bytes(raw)


def explain_mismatch(
    cell: str,
    segment: str,
    name: str,
    got: bytes,
    got_digest: str,
    want_digest: str,
    out_dir: Path,
) -> tuple[str, dict[str, Any]]:
    """The detail text and the record of a frame that is not the golden: got.png always; want.png and diff.png and the changed box
    and pixel count when the frame of the golden is in the local cache (`remember_frame`)."""
    from tools.control.digi_client import oled_png

    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "got.png").write_bytes(oled_png(got))
    head = f"got {got_digest[:8]} want {want_digest[:8]}"
    cached = _frame_file(cell, segment, name, want_digest)
    if not cached.exists():
        return (
            f"{head}; no local frame for the golden (run once on main to fill the cache); got.png in {out_dir}",
            {"frames": str(out_dir), "want_cached": False},
        )
    want = cached.read_bytes()
    n, box, changed = frame_diff(got, want)
    (out_dir / "want.png").write_bytes(oled_png(want))
    (out_dir / "diff.png").write_bytes(oled_png(changed))
    where = f"x {box[0]}..{box[2]}, y {box[1]}..{box[3]}" if box else "nowhere"
    return (
        f"{n} px differ in {where} (OLED pixels; the PNGs are 4x) ({head}); frames: {out_dir}",
        {"frames": str(out_dir), "want_cached": True, "px": n, "box": box},
    )


# ----- state checks -----------------------------------------------------------------------------------------------


def plan_symbol(plan: dict[str, Any], name: str, module: str | None = None) -> int:
    """The address of a module symbol in the symbols of a patch plan: the exact name (`DK_ABOUT`) or, for a Rust static, the end of
    its mangled name (`..._15pk_digikit_menu5STORE`). MODULE (`core/digikit-menu`) limits the search to that module's symbols."""
    found = {
        v
        for k, v in plan["symbols"].items()
        if (module is None or k.startswith(f"{module}@"))
        and (k.endswith(":" + name) or re.search(r"\d%s$" % re.escape(name), k))
    }
    if len(found) != 1:
        raise HarnessError(
            f"symbol {name} ({module or 'any module'}): {len(found)} addresses in the plan"
        )
    return found.pop()


def runtime_text() -> str:
    """The runtime version as the menu prints it (`major.minor.patch` of the packed value generated from sdk/sdk.toml)."""
    from tools.device.dklog_kinds import RUNTIME_PACKED as v

    return f"{v >> 16}.{v >> 8 & 255}.{v & 255}"


def text_rows(
    word: Callable[[int], int], base: int, count: int, stride: int
) -> list[str]:
    """COUNT NUL-terminated rows of STRIDE bytes at BASE, read by big-endian words (WORD(addr))."""
    rows = []
    for i in range(count):
        raw = b"".join(
            word(base + stride * i + 4 * k).to_bytes(4, "big")
            for k in range(stride // 4)
        )
        rows.append(raw.split(b"\0")[0].decode("latin-1"))
    return rows


def missing_rows(rows: list[str], has: list[str]) -> list[str]:
    """The strings of HAS (`{runtime}` expanded) that no row equals."""
    want = [h.replace("{runtime}", runtime_text()) for h in has]
    return [w for w in want if w not in rows]


# ----- goldens --------------------------------------------------------------------------------------------------


class Goldens:
    """Frame hashes (sha256 hex of the 1024 raw OLED bytes) keyed `DEV-FW/modset/segment/checkpoint`."""

    def __init__(self, path: Path = GOLDENS):
        self.path = path
        self.data: dict[str, str] = self._read()
        self.changes: dict[str, str] = {}
        self.dirty = False

    def _read(self) -> dict[str, str]:
        try:
            return json.loads(self.path.read_text())
        except (OSError, ValueError):
            return {}

    def get(self, key: str) -> str | None:
        return self.data.get(key)

    def set(self, key: str, value: str) -> None:
        if self.data.get(key) != value:
            self.data[key] = value
            self.changes[key] = value
            self.dirty = True

    def save(self) -> None:
        """Write our changes into the file as it is now (cells recorded by other processes meanwhile are kept)."""
        if not self.dirty:
            return
        import fcntl

        self.path.parent.mkdir(parents=True, exist_ok=True)
        OUT.mkdir(parents=True, exist_ok=True)
        with (OUT / "goldens.lock").open("w") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            merged = self._read()
            merged.update(self.changes)
            tmp = self.path.with_suffix(f".tmp{os.getpid()}")
            tmp.write_text(json.dumps(merged, indent=1, sort_keys=True) + "\n")
            os.replace(tmp, self.path)
        self.data = merged
        self.changes.clear()
        self.dirty = False


# ----- the session ----------------------------------------------------------------------------------------------


@dataclass
class Check:
    name: str
    ok: bool
    detail: str = ""


class Failure(Exception):
    """The firmware misbehaved (fault, halt, stall): the segment stops."""


class Session:
    """One daemon on one cell: spawn, load the image, run segments from `restart`."""

    def __init__(
        self,
        cell: Cell,
        goldens: Goldens,
        update: bool = False,
        log: Path | None = None,
        run_name: str | None = None,
        env: dict[str, str] | None = None,
        cache_dir: Path | None = None,
        run_dir: Path | None = None,
    ):
        self.cell, self.goldens, self.update = cell, goldens, update
        self.run_dir = run_dir  # explicit: kept; None: a temp dir made by open, removed after a clean close
        self.env = dict(env or {})
        self.cache_dir = cache_dir  # shared by the processes of one proof run: patched-boot pairs and the card
        self.syx, self.ready = firmware_inputs(cell)
        self.log_path = log
        self.run_name = run_name or cell.slug
        self._temp_run_dir = False
        self.emu: Emu | None = None
        self.events: list[dict[str, Any]] = []
        self.checks: list[Check] = []
        self.segment = ""
        self.pcm_mark = 0
        self.frames: dict[str, str] = {}
        self.load_report: dict[str, Any] = {}
        # wall seconds by kind (spawn_s, load_s, reset_s, run_s) and the emulated milliseconds run (emu_ms)
        self.timings: dict[str, float] = {}
        self.card: str | None = None
        self.card_override: str | None = (
            None  # a card image file used instead of the sample card (dk:dev --store)
        )
        self.image: Path | None = None

    # -- lifecycle
    def log(self, **entry: Any) -> None:
        if self.log_path is not None:
            with self.log_path.open("a") as f:
                f.write(
                    json.dumps({"t": round(time.time(), 3), **entry}, default=str)
                    + "\n"
                )

    def open(self, image: Path | None = None) -> None:
        cell = self.cell
        started = time.monotonic()
        exe = daemon_binary(cell.device, self.syx)
        if self.run_dir is None:
            self.run_dir = temp_run_dir(self.run_name)
            self._temp_run_dir = True
            if self.cache_dir is None:  # the boot caches must outlive the run dir
                self.cache_dir = SHARED_CACHE
        run_dir = self.run_dir
        if self.cache_dir is not None:
            link_shared(run_dir, self.cache_dir)
        self.emu = Emu.spawn_coupled(
            self.syx,
            self.ready,
            timeout=300,
            binary=exe,
            run_dir=run_dir,
            env=self.env,
        )
        emu = self.emu
        if emu.status().get("audio") is None:
            raise HarnessError("this digi-emud has no coupled audio")
        emu.subscribe()
        if self.card_override:
            self.card = self.card_override
        elif cell.device == "dt2":
            samples = OUT / "samples"
            card_check.make_samples(samples)
            with boot_lock(self.cache_dir, Path(f"card-{self.cell.device}")):
                self.card = emu.call("card.load", dir=str(samples))["card"]
            if self.cache_dir is not None:
                self.card = canonical_card(self.cache_dir, self.card)
        self.timings["spawn_s"] = time.monotonic() - started
        private = (
            self.cache_dir / "images" / str(os.getpid())
            if self.cache_dir is not None
            else None
        )
        self.load(image or patched_image(cell, self.syx, private))

    def load(self, image: Path, settle: bool = False) -> dict[str, Any]:
        """`firmware.load` the image (hot DSP, or the cached coupled cold boot); LoadFailure when it does not
        come up. Segments start from `restart` themselves; SETTLE does one here too."""
        emu = self.emu
        assert emu is not None
        started = time.monotonic()
        params: dict[str, Any] = {
            "syx": str(image),
            "force": True,
            "auto_no": True,
            "start": "auto",
        }
        if self.card:
            params["card"] = self.card
        try:
            with boot_lock(self.cache_dir, image):
                report = emu.call("firmware.load", **params)
        except EmuError as e:
            raise LoadFailure(f"firmware.load {image.name}: {e}") from e
        self.image = image
        self.load_report = report
        self.timings["load_s"] = time.monotonic() - started
        emu.watchdog(self.cell.stall_ms)
        if settle:
            self.reset()
        return report

    def close(self, ok: bool = True) -> None:
        if self.emu is not None:
            try:
                self.emu.__exit__(None, None, None)
            finally:
                self.emu = None
        if self._temp_run_dir and self.run_dir is not None:
            finish_run_dir(self.run_dir, ok)
            self._temp_run_dir = False

    def __enter__(self) -> Session:
        return self

    def __exit__(self, exc_type: object, *exc: object) -> None:
        self.close(ok=exc_type is None)

    # -- state and health
    def reset(self) -> None:
        """Back to the ready state, modal closed, PCM tap fresh: the start of every segment."""
        emu = self.emu
        assert emu is not None
        started = time.monotonic()
        emu.restart(auto_no=True)
        self.timings["reset_s"] = self.timings.get("reset_s", 0.0) + (
            time.monotonic() - started
        )
        emu.drain_events()
        self.run_ms(300)
        emu.pcm_start()
        self.pcm_mark = 0
        self.health("ready")
        status = emu.status()
        cf = status["cf"]
        ok = bool(cf["ready"]) and cf["phase"] == "ready" and cf["main_ui_reached"]
        ok = ok and cf.get("filesystem_verified") is not False
        self.check(
            "ready contract",
            ok,
            f"phase={cf['phase']} main_ui={cf['main_ui_reached']} fs={cf.get('filesystem_verified')}",
        )

    def run_ms(self, ms: float) -> None:
        emu = self.emu
        assert emu is not None
        n = int(ms * MS)
        self.timings["emu_ms"] = self.timings.get("emu_ms", 0.0) + ms
        started = time.monotonic()
        try:
            while n > 0:
                chunk = min(n, 200_000_000)
                try:
                    emu.step(chunk, timeout_wall_s=300)
                except EmuError as e:
                    if e.code == 6:
                        self.events.extend(emu.drain_events())
                        raise Failure(f"fault: {e}") from e
                    raise
                n -= chunk
        finally:
            self.timings["run_s"] = self.timings.get("run_s", 0.0) + (
                time.monotonic() - started
            )

    def health(self, where: str) -> None:
        """Fault, DSP halt and stall now? Raises Failure (the segment stops)."""
        emu = self.emu
        assert emu is not None
        status = emu.status()
        self.events.extend(e for e in emu.drain_events() if e["kind"] != "frame")
        cf, audio, beat = status["cf"], status.get("audio") or {}, status["heartbeat"]
        if cf.get("error") or cf["phase"] == "fault":
            raise Failure(f"fault at {where}: {cf.get('error')} pc={cf['pc']:#x}")
        if audio.get("halted"):
            raise Failure(f"DSP halt at {where}: {audio['halted']}")
        if beat["stalled"] or beat["stall_count"]:
            raise Failure(
                f"stall at {where}: mainloop_hits={beat['mainloop_hits']} stall_count={beat['stall_count']} "
                f"(threshold {beat['stall_ms']} ms)"
            )
        if any(e["kind"] == "lagged" for e in self.events):
            raise Failure(f"event queue lagged at {where}")

    def wait_event(self, kind: str, timeout_s: float = 3.0) -> bool:
        """Events are written by a socket thread and can follow their reply: poll `status` until one of KIND is
        in `events` (True) or TIMEOUT_S passes."""
        emu = self.emu
        assert emu is not None
        end = time.monotonic() + timeout_s
        while time.monotonic() < end:
            if any(e["kind"] == kind for e in self.events):
                return True
            emu.status()
            self.events.extend(e for e in emu.drain_events() if e["kind"] != "frame")
            time.sleep(0.05)
        return any(e["kind"] == kind for e in self.events)

    def check(self, name: str, ok: bool, detail: str = "", **extra: Any) -> None:
        self.checks.append(
            Check(f"{self.segment}/{name}" if self.segment else name, ok, detail)
        )
        self.log(check=name, segment=self.segment, ok=ok, detail=detail, **extra)

    # -- actions
    def mark_pcm(self) -> None:
        emu = self.emu
        assert emu is not None
        self.pcm_mark = len(emu.pcm_report().get("window_rms") or [])

    def do_tap(self, keys: str | int | list[Any], hold_ms: float = 20) -> None:
        emu = self.emu
        assert emu is not None
        self.mark_pcm()
        seq = keys if isinstance(keys, list) else [keys]
        held = [button(k) for k in seq[:-1]]
        for code in held:
            emu.button(code, True)
            self.run_ms(5)
        emu.tap(button(seq[-1]), hold=int(hold_ms * MS))
        self.run_ms(hold_ms + 5)
        for code in reversed(held):
            emu.button(code, False)
            self.run_ms(5)
        self.health(f"tap {keys}")

    def do_hold(self, key: str | int, ms: float) -> None:
        emu = self.emu
        assert emu is not None
        self.mark_pcm()
        code = button(key)
        emu.button(code, True)
        self.run_ms(ms)
        emu.button(code, False)
        self.run_ms(5)
        self.health(f"hold {key}")

    def do_turn(self, enc: str | int, by: int) -> None:
        emu = self.emu
        assert emu is not None
        self.mark_pcm()
        emu.turn(encoder(enc), by)
        self.run_ms(10)
        self.health(f"turn {enc} {by}")

    # -- parameter read-back and knob helpers (DT2: finding 02, "A machine is eight fixed engine slots")
    SOUND_PTRS = 0x8000470C  # 16 pointers to the tracks' Sound objects; the parameter array is at +0x14
    TX_FRAME = 0x80005348  # the ColdFire -> DSP frame; machine parameter words at 0xda + 0x60 * track + 2 * (idx - 25)

    def _u16(self, addr: int) -> int:
        assert self.emu is not None
        word = self.emu.peek32(addr & ~3) or 0
        return ((word >> 16) if addr % 4 == 0 else word) & 0xFFFF

    def mirror(self, idx: int, track: int = 0) -> int:
        """The s16 of parameter slot IDX (25..34: the machine's engine slots) of TRACK's Sound object, as unsigned."""
        assert self.emu is not None
        sound = self.emu.peek32(self.SOUND_PTRS + 4 * track) or 0
        return self._u16(sound + 0x14 + 2 * idx)

    def tx_word(self, idx: int, track: int = 0) -> int:
        """The word of engine slot IDX in the TX frame the DSP reads (valid after the next frame that follows a change)."""
        return self._u16(self.TX_FRAME + 0xDA + 0x60 * track + 2 * (idx - 25))

    def turn_burst(
        self, enc: str | int, detents: int, events: int, gap_ms: float = 2
    ) -> None:
        """EVENTS encoder events of DETENTS each, GAP_MS apart. The encoder accelerates by the time between events and
        a value moves about one raw step per slow event (finding 03): a slow single event can change nothing."""
        emu = self.emu
        assert emu is not None
        self.mark_pcm()
        for _ in range(events):
            emu.turn(encoder(enc), detents)
            self.run_ms(gap_ms)
        self.run_ms(100)
        self.health(f"burst {enc} {detents}x{events}")

    def turn_to(
        self,
        enc: str | int,
        idx: int,
        target: int,
        tol: int = 0x300,
        track: int = 0,
        rounds: int = 80,
    ) -> int:
        """Turn ENC until slot IDX reads within TOL of TARGET (bursts of 3 events, sign by the error); returns the
        value reached. Raises Failure when ROUNDS bursts do not get there."""
        for _ in range(rounds):
            v = self.mirror(idx, track)
            err = target - v
            if abs(err) <= tol:
                return v
            self.turn_burst(enc, 4 if err > 0 else -4, 3 if abs(err) < 0x1000 else 8)
        v = self.mirror(idx, track)
        raise Failure(f"turn_to {enc}: slot {idx} is {v:#x}, wanted {target:#x}")

    def frame(self) -> tuple[str, bytes]:
        assert self.emu is not None
        _, raw = self.emu.frame()
        return hashlib.sha256(raw).hexdigest(), raw

    def expect_frame(self, name: str, differs_from: str | None = None) -> None:
        digest, raw = self.frame()
        key = f"{self.cell.golden_prefix}/{self.segment}/{name}"
        png_dir = OUT / "frames" / self.cell.slug
        png_dir.mkdir(parents=True, exist_ok=True)
        from tools.control.digi_client import oled_png

        (png_dir / f"{self.segment}-{name}.png").write_bytes(oled_png(raw))
        self.frames[f"{self.segment}/{name}"] = digest
        if differs_from is not None:
            other = self.frames.get(f"{self.segment}/{differs_from}")
            self.check(
                f"{name} differs from {differs_from}",
                other is not None and other != digest,
            )
        if self.update:
            self.goldens.set(key, digest)
            remember_frame(self.cell.golden_prefix, self.segment, name, digest, raw)
            self.check(f"frame {name}", True, f"golden recorded {digest[:12]}")
            return
        want = self.goldens.get(key)
        if want is None:
            self.check(
                f"frame {name}", False, f"no golden for {key} (run with --update)"
            )
        elif want == digest:
            remember_frame(self.cell.golden_prefix, self.segment, name, digest, raw)
            self.check(f"frame {name}", True, f"got {digest[:12]} want {want[:12]}")
        else:
            detail, extra = explain_mismatch(
                self.cell.golden_prefix,
                self.segment,
                name,
                raw,
                digest,
                want,
                png_dir / "mismatch" / f"{self.segment.replace('/', '_')}-{name}",
            )
            self.check(f"frame {name}", False, detail, **extra)

    def expect_pcm_db(self, lo: float | None, hi: float | None) -> None:
        emu = self.emu
        assert emu is not None
        windows = (emu.pcm_report().get("window_rms") or [])[self.pcm_mark :]
        peak = max(windows, default=0.0)
        db = 20 * math.log10(peak) if peak > 0 else -math.inf
        ok = (lo is None or db >= lo) and (hi is None or db <= hi)
        self.check(
            "pcm",
            ok,
            f"peak window {db:.1f} dBFS over {len(windows)} windows (want {lo}..{hi})",
        )

    def expect_mem(self, want: dict[str, Any]) -> None:
        emu = self.emu
        assert emu is not None
        label = want.get("symbol")
        addr = self.symbol(label, want.get("module")) if label else int(want["addr"])
        mask = int(want.get("mask", 0xFFFFFFFF))
        got = emu.peek32(addr)
        ok = got is not None and got & mask == int(want["eq"])
        self.check(
            f"mem {label or f'{addr:#x}'}",
            ok,
            f"got {'unmapped' if got is None else f'{got & mask:#x}'} want {int(want['eq']):#x} (mask {mask:#x})",
        )

    def symbol(self, name: str, module: str | None = None) -> int:
        """The address of a symbol of the image's patch plan (`patchkit patch` wrote it next to the image)."""
        plan = self.image.with_suffix(".plan.json") if self.image else None
        if plan is None or not plan.is_file():
            raise HarnessError(f"symbol {name}: the image has no plan")
        return plan_symbol(json.loads(plan.read_text()), name, module)

    def expect_text(self, want: dict[str, Any]) -> None:
        emu = self.emu
        assert emu is not None
        base = self.symbol(want["rows"])
        count = emu.peek32(self.symbol(want["count"])) or 0
        rows = text_rows(
            lambda a: emu.peek32(a) or 0, base, count, int(want.get("stride", 24))
        )
        lost = missing_rows(rows, list(want["has"]))
        self.check(
            f"text {want['rows']}",
            not lost,
            f"rows {rows}" if not lost else f"no row {lost} in {rows}",
        )

    def expect_dsp(self, want: dict[str, Any]) -> None:
        emu = self.emu
        assert emu is not None
        raw = bytes.fromhex(want["hex"])
        got = emu.dsp_peek(int(want["addr"]), len(raw))["hex"]
        self.check(
            f"dsp {int(want['addr']):#x}",
            got == raw.hex(),
            f"got {got} want {raw.hex()}",
        )

    def stress(self, seed: int, actions: int) -> None:
        rng = random.Random(seed)
        keys = (
            [c for c in range(1, 25)]
            + [25 + i for i in range(16)]
            + [41 + i for i in range(8)]
        )
        trace = []
        for i in range(actions):
            roll = rng.random()
            if roll < 0.55:
                code = rng.choice(keys)
                trace.append(f"tap {code}")
                self.do_tap(code, hold_ms=rng.choice([10, 20, 40]))
            elif roll < 0.8:
                enc, by = rng.randint(1, 9), rng.choice([-3, -2, -1, 1, 2, 3])
                trace.append(f"turn {enc} {by}")
                self.do_turn(enc, by)
            elif roll < 0.9:
                a, b = rng.choice([16, 17, 23, 22]), rng.choice(keys)
                trace.append(f"chord {a}+{b}")
                self.do_tap([a, b])
            else:
                ms = rng.choice([20, 60, 150])
                trace.append(f"wait {ms}")
                self.run_ms(ms)
            if i % 8 == 7:
                self.health(f"stress step {i} ({trace[-1]})")
        self.log(stress_seed=seed, trace=trace)
        self.check(f"stress {actions} inputs (seed {seed})", True)

    # -- steps
    def run_steps(self, steps: list[dict[str, Any]]) -> None:
        for step in steps:
            devices = step.get("device")
            if devices and self.cell.device not in (
                devices if isinstance(devices, list) else [devices]
            ):
                continue
            self.log(step=step)
            if "tap" in step:
                self.do_tap(step["tap"], step.get("hold_ms", 20))
            elif "hold" in step:
                self.do_hold(step["hold"], step.get("ms", 500))
            elif "turn" in step:
                if (
                    "events" in step
                ):  # a burst: EVENTS events 2 ms apart (one slow event changes nothing)
                    self.turn_burst(
                        step["turn"], step.get("by", 1), int(step["events"])
                    )
                else:
                    self.do_turn(step["turn"], step.get("by", 1))
            elif "wait_ms" in step:
                self.run_ms(step["wait_ms"])
                self.health(f"wait {step['wait_ms']} ms")
            elif "expect_frame" in step:
                self.expect_frame(step["expect_frame"], step.get("differs_from"))
            elif "expect_pcm_db" in step:
                want = step["expect_pcm_db"]
                self.expect_pcm_db(want.get("min"), want.get("max"))
            elif "expect_mem" in step:
                self.expect_mem(step["expect_mem"])
            elif "expect_text" in step:
                self.expect_text(step["expect_text"])
            elif "expect_dsp" in step:
                self.expect_dsp(step["expect_dsp"])
            elif "expect_no_fault" in step:
                self.health("expect_no_fault")
                self.check("no fault/halt/stall", True)
            elif (
                "_call" in step
            ):  # tests only: a callable(session) that breaks or inspects the guest
                step["_call"](self)
            elif "stress" in step:
                s = step["stress"]
                self.stress(int(s.get("seed", 1)), int(s.get("actions", 40)))
            else:
                raise HarnessError(f"step without an action: {step}")

    def measure_dsp(
        self, window_ms: int = 1000, parts: int = 10
    ) -> tuple[float, float]:
        """(mean, max over PARTS sub-windows) of the DSP busy instructions per block with TRIG 1 held (the default
        voice), after the same settling as every segment. Counters: status.audio.dsp_tiers (idle skip excluded)."""
        emu = self.emu
        assert emu is not None

        def tiers() -> dict[str, Any]:
            return (emu.status().get("audio") or {}).get("dsp_tiers") or {}

        self.reset()
        self.run_ms(1500)
        emu.button(button("TRIG 1"), True)
        self.run_ms(100)  # the note-on settles
        first = last = tiers()
        worst = 0.0
        for _ in range(parts):
            self.run_ms(window_ms / parts)
            now = tiers()
            part = pk_dsp_budget.busy_per_block(last, now)
            if part is not None:
                worst = max(worst, part)
            last = now
        emu.button(button("TRIG 1"), False)
        self.health("dsp budget window")
        mean = pk_dsp_budget.busy_per_block(first, last)
        if mean is None:
            raise HarnessError(
                "no DSP block ran in the budget window (status.audio.dsp_tiers)"
            )
        return mean, worst

    def run_segment(self, name: str, steps: list[dict[str, Any]]) -> bool:
        """One segment from a fresh `restart`; True when every check passed."""
        self.segment = name
        before = len(self.checks)
        started = time.monotonic()
        try:
            self.reset()
            self.run_steps(steps)
            self.health("end")
        except Failure as e:
            self.check("health", False, str(e))
            self.log(failure=str(e), events=self.events[-20:])
        except (EmuError, HarnessError) as e:
            self.check("error", False, str(e))
        self.log(segment=name, seconds=round(time.monotonic() - started, 2))
        self.segment = ""
        return all(c.ok for c in self.checks[before:])


# ----- tours ----------------------------------------------------------------------------------------------------


def smoke_segments(path: Path | None = None) -> list[tuple[str, list[dict[str, Any]]]]:
    data = tomllib.loads((path or TOURS / "smoke.toml").read_text())
    return [(s["name"], s.get("step", [])) for s in data["segment"]]


def module_tests(
    modules: list[str], device: str, quick_only: bool
) -> Iterator[tuple[str, list[dict[str, Any]]]]:
    for mod in modules:
        for f in sorted((modroots.module_dir(mod) / "tests").glob("*.toml")):
            data = tomllib.loads(f.read_text())
            t = data.get("test", {})
            if device not in t.get("devices", ["dt2", "dn2"]):
                continue
            if quick_only and not t.get("quick", True):
                continue
            yield (
                f"{mod.replace('/', '.')}.{t.get('name', f.stem)}",
                data.get("step", []),
            )


def stock_busy(cell: Cell, cache_dir: Path = SHARED_CACHE) -> float:
    """Mean busy DSP instructions per block of the stock firmware of this device and OS, same window. Cached per
    daemon binary in OUT/dsp/stock-DEV-FW.json (the engine changes the number; the key is the binary's size and
    mtime)."""
    exe = daemon_binary(cell.device, firmware_inputs(cell)[0])
    st = exe.stat()
    key = f"{exe}:{st.st_size}:{st.st_mtime_ns}"
    path = OUT / "dsp" / f"stock-{cell.device}-{cell.firmware}.json"
    with contextlib.suppress(OSError, ValueError):
        rec = json.loads(path.read_text())
        if rec.get("key") == key:
            return float(rec["busy_mean"])
    stock = Cell(cell.device, cell.firmware, [], cell.stall_ms)
    with Session(
        stock, Goldens(), run_name=f"dsp-stock-{stock.slug}", cache_dir=cache_dir
    ) as session:
        session.open()
        mean, worst = session.measure_dsp()
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(f".{os.getpid()}.tmp")
    tmp.write_text(
        json.dumps({"key": key, "busy_mean": mean, "busy_max_window": worst})
    )
    tmp.replace(path)
    return mean


def memory_readout(session: Session) -> dict[str, Any] | None:
    """Static patch memory of the session's image (tools/patchkit/pk_mem.py) from the plan `patchkit patch` wrote next to
    it; None for the stock image or an image without a plan."""
    from tools.patchkit import pk_mem

    plan = session.image.with_suffix(".plan.json") if session.image else None
    if not session.cell.modules or plan is None or not plan.is_file():
        return None
    cell = session.cell
    return pk_mem.usage(
        pk_mem.port_regions(cell.device, cell.firmware), json.loads(plan.read_text())
    )


def dsp_budget_check(session: Session, budget_pct: float) -> dict[str, Any]:
    """Measure the cell, judge it against the stock baseline, record the check and OUT/dsp/CELL.json."""
    cell = session.cell
    session.segment = "dsp-budget"
    try:
        mean, worst = session.measure_dsp()
        stock = mean if not cell.modules else stock_busy(cell)
        report = pk_dsp_budget.judge(cell.device, mean, worst, stock, budget_pct)
    except (Failure, EmuError, HarnessError) as e:
        session.check("measure", False, str(e))
        session.segment = ""
        return {}
    report["cell"] = cell.name
    report["memory"] = memory_readout(session)
    session.check("projected high load", report["ok"], pk_dsp_budget.summary(report))
    session.segment = ""
    (OUT / "dsp").mkdir(parents=True, exist_ok=True)
    (OUT / "dsp" / f"{cell.slug}.json").write_text(json.dumps(report, indent=1))
    return report


def run_cell(
    cell: Cell,
    goldens: Goldens,
    update: bool = False,
    tests: str = "all",
    seed: int | None = None,
    stress_actions: int | None = None,
    log_dir: Path = OUT,
    dsp_budget: float | None = pk_dsp_budget.DEFAULT_BUDGET_PCT,
) -> tuple[bool, str, Session]:
    """Run a cell; (passed, one-line summary, session). The session's `checks` hold the detail."""
    log_dir.mkdir(parents=True, exist_ok=True)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    log = log_dir / f"{stamp}-{cell.slug}.jsonl"
    started = time.monotonic()
    session = Session(cell, goldens, update=update, log=log, cache_dir=SHARED_CACHE)
    failed: list[str] = []
    try:
        with session:
            session.open()
            segments: list[tuple[str, list[dict[str, Any]]]] = []
            if tests in ("all", "smoke"):
                for name, steps in smoke_segments():
                    steps = [dict(s) for s in steps]
                    for s in steps:
                        if "stress" in s:
                            s["stress"] = {
                                **s["stress"],
                                **({"seed": seed} if seed is not None else {}),
                                **(
                                    {"actions": stress_actions}
                                    if stress_actions
                                    else {}
                                ),
                            }
                    segments.append((name, steps))
            if tests in ("all", "quick"):
                segments.extend(
                    module_tests(cell.modules, cell.device, tests == "quick")
                )
            for name, steps in segments:
                if not session.run_segment(name, steps):
                    failed.append(name)
            if dsp_budget is not None:
                before = len(session.checks)
                dsp_budget_check(session, dsp_budget)
                if not all(c.ok for c in session.checks[before:]):
                    failed.append("dsp-budget")
    except HarnessError as e:
        return False, f"FAIL {cell.name}: setup: {e}", session
    except LoadFailure as e:
        return (
            False,
            f"FAIL {cell.name}: load: {e} [log {log.relative_to(ROOT)}]",
            session,
        )
    finally:
        if update:
            goldens.save()
    secs = time.monotonic() - started
    n = len(session.checks)
    if failed:
        first = next(c for c in session.checks if not c.ok)
        return (
            False,
            f"FAIL {cell.name}: {len(failed)} segment(s) failed ({', '.join(failed)}); first: {first.name}: "
            f"{first.detail} [{secs:.0f} s, log {log.relative_to(ROOT)}]",
            session,
        )
    return (
        True,
        f"PASS {cell.name}: {n} checks in {secs:.0f} s (load {session.timings.get('load_s', 0):.1f} s, "
        f"{session.load_report.get('start_used', '?')}) [log {log.relative_to(ROOT)}]",
        session,
    )


def _run_one(
    cell: Cell, goldens: Goldens, a: argparse.Namespace
) -> tuple[bool, str, list[str]]:
    """A cell in this process; (passed, summary line, failed check lines)."""
    ok, line, session = run_cell(
        cell,
        goldens,
        a.update,
        a.tests,
        a.seed,
        a.stress_actions,
        dsp_budget=None if a.no_dsp_budget else a.dsp_budget,
    )
    bad = [f"{c.name}: {c.detail}" for c in session.checks if not c.ok]
    return ok, line, bad


def _run_cells_parallel(
    cells: list[Cell], a: argparse.Namespace, argv: list[str], jobs: int
) -> list[tuple[bool, str, list[str]]]:
    """One child process per cell, JOBS at a time (each has its own daemons; the boot cache and card are shared and
    a lock lets the first process boot a given image cold while the others wait for the cached pair). Results come
    back in matrix order."""
    base = []
    skip = False
    for x in argv:
        if skip:
            skip = False
            continue
        if x in ("--jobs", "--index", "--result"):
            skip = True
            continue
        if x.startswith(("--jobs=", "--index=", "--result=")):
            continue
        if x != "run":
            base.append(x)
    results: list[tuple[bool, str, list[str]] | None] = [None] * len(cells)
    running: dict[int, tuple[subprocess.Popen[bytes], Path, Path, float]] = {}
    queue = list(range(len(cells)))
    work = OUT / "cells"
    work.mkdir(parents=True, exist_ok=True)
    print(f"{len(cells)} cells, {jobs} at a time", flush=True)
    while queue or running:
        while queue and len(running) < jobs:
            i = queue.pop(0)
            res = work / f"{os.getpid()}-{i}.json"
            res.unlink(missing_ok=True)
            log = work / f"{os.getpid()}-{i}.log"
            with log.open("wb") as f:
                p = subprocess.Popen(
                    [
                        sys.executable,
                        "-u",
                        "-m",
                        "tools.patchkit.pk_harness",
                        "run",
                        *base,
                        "--index",
                        str(i),
                        "--jobs",
                        "1",
                        "--result",
                        str(res),
                    ],
                    stdout=f,
                    stderr=subprocess.STDOUT,
                    cwd=ROOT,
                    env=python_env(),
                )
            running[i] = (p, res, log, time.monotonic())
        time.sleep(0.5)
        for i, (p, res, log, t0) in list(running.items()):
            rc = p.poll()
            if rc is None:
                continue
            del running[i]
            try:
                rec = json.loads(res.read_text())
                results[i] = (rec["ok"], rec["line"], rec["bad"])
            except (OSError, ValueError, KeyError):
                tail = " | ".join(
                    log.read_text(errors="replace").strip().splitlines()[-3:]
                )
                how = f"signal {-rc}" if rc < 0 else f"exit {rc}"
                results[i] = (
                    False,
                    f"FAIL {cells[i].name}: the cell process ended with no result ({how}) after "
                    f"{time.monotonic() - t0:.0f} s; last output: {tail} [{log.relative_to(ROOT)}]",
                    [],
                )
            print(results[i][1], flush=True)  # type: ignore[index]
    return [r for r in results if r is not None]


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--matrix", type=Path, default=DEFAULT_MATRIX)
    r.add_argument(
        "--cell",
        action="append",
        default=[],
        help="DEV-FW:mod,mod (stock when no modules)",
    )
    r.add_argument(
        "--update",
        action="store_true",
        help="record the frame goldens instead of comparing",
    )
    r.add_argument(
        "--goldens",
        type=Path,
        default=GOLDENS,
        help="the golden hash file (default tests/patchkit/smoke/goldens.json; a copy lets you see a mismatch without touching the real one)",
    )
    r.add_argument("--tests", choices=("smoke", "quick", "all"), default="all")
    r.add_argument("--seed", type=int)
    r.add_argument("--stress-actions", type=int)
    r.add_argument(
        "--jobs",
        type=int,
        default=0,
        help="cells in parallel processes (default: free cores / 2, at most 6; 1 runs them here, in order)",
    )
    r.add_argument(
        "--dsp-budget",
        type=float,
        default=pk_dsp_budget.DEFAULT_BUDGET_PCT,
        help="fail a cell whose projected real DSP load (high bound, held note) exceeds this percent (default 85)",
    )
    r.add_argument(
        "--no-dsp-budget", action="store_true", help="skip the DSP budget measurement"
    )
    r.add_argument("--result", type=Path, help=argparse.SUPPRESS)
    r.add_argument(
        "--index", type=int, help=argparse.SUPPRESS
    )  # a child: run cell N of the list
    sub.add_parser("list").add_argument("--matrix", type=Path, default=DEFAULT_MATRIX)
    a = p.parse_args(argv)
    cells = [parse_cell(c) for c in getattr(a, "cell", [])] or load_matrix(a.matrix)
    if a.cmd == "list":
        for c in cells:
            print(c.name)
        return 0
    started = time.monotonic()
    done: list[str] = []
    _announce_signals(cells, done)
    if (
        a.result is not None
    ):  # a child of a parallel run: one cell, the record goes to a file
        ok, line, bad = _run_one(cells[a.index], Goldens(a.goldens), a)
        print(line, flush=True)
        a.result.write_text(json.dumps({"ok": ok, "line": line, "bad": bad}))
        return 0 if ok else 1
    jobs = a.jobs or default_jobs(len(cells))
    if jobs > 1 and len(cells) > 1:
        results = _run_cells_parallel(cells, a, argv, jobs)
    else:
        goldens = Goldens(a.goldens)
        results = []
        for cell in cells:
            res = _run_one(cell, goldens, a)
            print(res[1], flush=True)
            done.append(cell.name)
            results.append(res)
    passed = sum(1 for ok, _, _ in results if ok)
    for ok, line, bad in results:
        if not ok:
            print(f"  {line[:240]}")
            for b in bad[:6]:
                print(f"    {b}")
    complete = len(results) == len(cells)
    _dsp_table(cells, a)
    print(
        f"{'PASS' if passed == len(cells) and complete else 'FAIL'}: {passed} of {len(cells)} cells passed"
        f"{'' if complete else f' ({len(cells) - len(results)} did not run)'}, wall {time.monotonic() - started:.0f} s",
        flush=True,
    )
    return 0 if passed == len(cells) and complete else 1


def _dsp_table(cells: list[Cell], a: argparse.Namespace) -> None:
    """The DSP budget of every cell that ran: a table on stdout and OUT/dsp-budget.json."""
    if getattr(a, "no_dsp_budget", False):
        return
    rows = []
    for c in cells:
        with contextlib.suppress(OSError, ValueError):
            rows.append(json.loads((OUT / "dsp" / f"{c.slug}.json").read_text()))
    if not rows:
        return
    print(
        "DSP budget (busy instructions per block; projected real load = busy x cycles/instruction factor / 666,667):"
    )
    print(f"  {'cell':<58} {'busy':>8} {'added':>7} {'nominal':>8} {'high':>7}  gate")
    for r in rows:
        print(
            f"  {r['cell']:<58} {r['busy_mean']:>8.0f} {r['added_instructions_per_block']:>+7.0f} "
            f"{r['load_nominal_pct']:>7.1f}% {r['load_high_pct']:>6.1f}%  {'ok' if r['ok'] else 'FAIL'} (<= {r['budget_pct']:g}%)"
        )
    (OUT / "dsp-budget.json").write_text(json.dumps(rows, indent=1))


def _announce_signals(cells: list[Cell], done: list[str]) -> None:
    """A signal (the shell's timeout, a closed terminal) used to end the run silently: say so, with the progress."""
    import signal

    def handler(signum: int, _frame: object) -> None:
        name = signal.Signals(signum).name
        print(
            f"INTERRUPTED by {name} after {len(done)} of {len(cells)} cells; not finished: "
            f"{[c.name for c in cells if c.name not in done][:6]}",
            file=sys.stderr,
            flush=True,
        )
        sys.exit(128 + signum)

    for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
        signal.signal(sig, handler)


if __name__ == "__main__":
    os.chdir(ROOT)
    sys.exit(main(sys.argv[1:]))
