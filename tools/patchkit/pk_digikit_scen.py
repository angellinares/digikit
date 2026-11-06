#!/usr/bin/env python3
"""Boot-time scenarios of the digikit runtime: data files and one runner (docs/findings/39-digikit-boot-tests.md).

    uv run python -m tools.patchkit.pk_digikit_scen list
    uv run python -m tools.patchkit.pk_digikit_scen run [NAME ...] [--image dt2-1.17] [--tier quick|full] [--keep DIR]

A scenario is a TOML file in `tests/digikit/scenarios/` or in a module's own `modules/<group>/<name>/tests/scenarios/` (found by
globbing; no list names them; the names are unique across both places):

    [scenario]
    name = "equiv-dyn-pluck"       # the file's stem
    kind = "equiv" | "menu" | "upload" | "pages" | "fx"
    devices = ["dt2"]              # the scenario runs on the images of these devices (`images` names them instead)
    tier = "quick" | "full"        # quick: part of the verify gate; full: only `--tier full` (merges to main)

and a table named like the kind with its data (module sets, expectations). The runner is built on `pk_dkrt_boot`
(`Env`, `BootRig`): the same cold boot of the patched firmware with the image's own DSP, the same mailbox, restart and
record views. Every scenario ends with the common checks: write-watch (no stray write into `cf.expansion`), flush check,
and the card sectors the guest wrote (in the store range only the two selection sectors; the rest is the firmware's own
project and working sectors).

Kinds:

  equiv      a selection loaded at boot gives the same machine, parameter, page and voice rows as the image patchkit builds
             with the static twin of the set patched in (compared by decoded content: names, ranges, defaults, slots, knobs
             by parameter name; the type numbers and the pointers differ by construction), and the same PCM for a note when
             the set has a sound (`play`). A set without a twin (`twin = ""`) must leave every table as a boot with no module.
             `pcm_sha256` (with `play`) replaces the PCM compare when no twin has the sound: the SHA-256 of the run-time note from its onset over
             `pcm_samples` (default 24000) interleaved float32 samples; "record" fails and prints the hash. `env_run` and `env_ref` set environment variables of the run-time and the reference boot (`SHARC_UNINIT` for a machine that
             reads voice state beyond its cleared bytes: the run-time state is uninitialised L2). `ignore_run_machines` drops a run-time
             machine (with its parameters and pages) that the twin has no counterpart of. `play_module` and `play_machine` (default: the machine of `tools/patchkit/pk_dkrt_machine.py`, a sine) name the run-time machine the note is played with.
  safe       a module whose init resets the machine, twice: the third boot is safe mode (no module, reported). With `[safe] part = "late"`: a
             module that hangs the UI task some time into a session, before (early) or after (late) the stable period that clears the boot
             counter: early leads to safe mode at the third boot, late never does (finding 37).
  menu       a set checked in SETTINGS > DIGIKIT > MODULES on a boot with no selection and applied with NO and YES on APPLY & RESTART? is
             loaded by the next boot and compares as `equiv` does (rows, machine in the list, PCM of a note equal to the patched-in set's).
  card       `[card] part = "torn"` only: a power cut inside the first writes to a blank card (finding 46, residual risk). The other card-safety
             cases are the TypeScript card-safety test.
  crash      a CPU exception in a module's slot: the runtime's hook records it in RAM that a soft reset keeps and resets; the next boot reports
             the record (right PC), two in a row lead to safe mode (before and after the stable period).
  pages      the parameter pages of a machine chosen on track 1 (SRC, SRC again, encoder turns): the run-time module's frames equal the patched-in twin's, an
             added machine never draws the sample view, stock sample machines still do (finding 50; tools/patchkit/pk_digikit_pages.py).
  upload     module upload over USB SysEx (docs/design/digikit-upload.md): `[upload] part` = rust, see
             tools/patchkit/pk_digikit_upload.py. The `menu` kind with `upload = true` is the through-the-menu proof.

Keys worth knowing: `[equiv] trig_frame` (the DSP frame the note starts at, after the setup). Time: `DIGIKIT_PROFILE=1` prints a phase table, `--jobs N` runs
(scenario, image) pairs in N processes, `DIGIKIT_RUN_SALT=x` forces cold boots, `DIGIKIT_SETTLE_MS` the emulated ms after a boot (default 300).

Cards are cached by content in `out/cache/digikit/cards/`, or under `DIGIKIT_CACHE` (they are read-only for the guest, its writes live in an overlay), and the
daemon's own boot cache is keyed by the card path, so a repeated scenario restores its boots instead of running them cold.
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
import shutil
import sys
import tempfile
import threading
import time
import tomllib
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]

from tools import python_env  # noqa: E402
from tools.patchkit import pk_device  # noqa: E402
from tools.patchkit import pk_dkrt_boot as B  # noqa: E402
from tools.patchkit import pk_dkrt_machine as M  # noqa: E402
from tools.patchkit import pk_harness as H  # noqa: E402

# a module's own scenarios: modules/<group>/<name>/tests/scenarios/*.toml
MODULE_SCENARIOS = "modules/*/*/tests/scenarios"
# DIGIKIT_CACHE moves the cache: the cards are sparse files of the card's full size (16.7 GiB for a
# DN2), and a file system without sparse files (WSL's mount of a Windows drive) writes each one out in full
CACHE = (
    Path(os.environ["DIGIKIT_CACHE"])
    if os.environ.get("DIGIKIT_CACHE")
    else ROOT / "out" / "cache" / "digikit"
)
# every device with a module store and its newest image that has a port (`port = true` in the device file)
QUICK_IMAGES = {d: pk_device.port_images(d)[-1] for d in pk_device.digikit_devices()}
RT = B.RT
CACHE_LIMIT = 20 * 2**30  # bytes on disk the card cache may hold


def card_used(card: Path, written: bool) -> None:
    """Mark a cached card as used now (its mtime orders the eviction); after a new card, delete the least recently used ones
    while the cache holds more than CACHE_LIMIT bytes on disk (the cards are sparse). The new card stays."""
    with contextlib.suppress(OSError):
        os.utime(card)
    if not written:
        return
    entries = []
    for f in (CACHE / "cards").glob("*.img"):
        with contextlib.suppress(OSError):  # a card another process deleted
            st = f.stat()
            entries.append((st.st_mtime, st.st_blocks * 512, f))
    total = sum(e[1] for e in entries)
    for _, size, f in sorted(entries):
        if total <= CACHE_LIMIT:
            break
        if f != card:
            f.unlink(missing_ok=True)
            total -= size


# emulated ms a rig runs after its boot before the scenario looks: the first UI-loop passes (a module's DSP part is waited for by
# `wait_dsp`); the boot tool's own proof keeps 1500
SETTLE_MS = int(os.environ.get("DIGIKIT_SETTLE_MS", "300"))
FIRMWARE_FIRST = (
    pk_device.working_project_sector()
)  # the working project; the project slots follow (finding 02, storage records)


class Fail(RuntimeError):
    pass


# ---- the scenario files -------------------------------------------------------------------------------------------------------


@dataclass
class Scenario:
    name: str
    kind: str
    tier: str
    devices: list[str]
    data: dict[str, Any]
    path: Path

    def images(self, tier: str) -> list[str]:
        """The images a run covers: the newest image of each device the scenario targets."""
        explicit = self.data["scenario"].get("images")
        if explicit:
            return [i for i in explicit if i in QUICK_IMAGES.values()]
        return [QUICK_IMAGES[dev] for dev in self.devices]


def scenario_files(root: Path = ROOT) -> list[Path]:
    """Every scenario file below ROOT: tests/digikit/scenarios/ first, then the modules' own, sorted by name."""
    own = sorted(root.glob(f"{MODULE_SCENARIOS}/*.toml"), key=lambda p: p.name)
    return [*sorted((root / "tests/digikit/scenarios").glob("*.toml")), *own]


def load_scenarios(root: Path = ROOT) -> list[Scenario]:
    out: list[Scenario] = []
    names: dict[str, Path] = {}
    for f in scenario_files(root):
        if f.stem in names:
            raise Fail(
                f"{f}: the scenario name {f.stem} is also used by {names[f.stem]}"
            )
        names[f.stem] = f
        d = tomllib.loads(f.read_text())
        s = d["scenario"]
        if s["name"] != f.stem:
            raise Fail(f"{f.name}: scenario.name must be the file's stem")
        if s["kind"] not in KINDS:
            raise Fail(f"{f.name}: unknown kind {s['kind']!r}")
        if s["kind"] not in d:
            raise Fail(f"{f.name}: no [{s['kind']}] table")
        tier = s.get("tier", "full")
        if tier not in ("quick", "full"):
            raise Fail(f"{f.name}: tier is quick or full")
        out.append(Scenario(s["name"], s["kind"], tier, s.get("devices", []), d, f))
    return out


def selected(tier: str, names: list[str] | None = None) -> list[Scenario]:
    """The scenarios of a tier (full: all; quick: those marked quick), or the named ones."""
    out = []
    for s in load_scenarios():
        if names:
            if s.name in names:
                out.append(s)
        elif tier == "full" or s.tier == "quick":
            out.append(s)
    return out


# ---- the context of one scenario on one image ------------------------------------------------------------------------------------


class Ctx:
    def __init__(self, scen: Scenario, image: str, work: Path):
        self.scen, self.image, self.work = scen, image, work
        self.device = image[:3]
        self.dt2 = self.device == "dt2"
        self.env = B.Env(image, work)
        # verify builds in the same target directory while its python tests run: a tool binary can be missing for a moment
        tools_run = self.env.tools.run

        def run_tool(*argv: object, **kw: Any) -> Any:
            for _ in range(12):
                try:
                    return tools_run(*argv, **kw)
                except FileNotFoundError:
                    time.sleep(15)
            return tools_run(*argv, **kw)

        self.env.tools.run = run_tool  # type: ignore[method-assign]
        self.checks: list[tuple[str, bool, str]] = []
        self.report: dict[str, Any] = {"scenario": scen.name, "image": image}
        self.imgs: dict[str, B.Img] = {}
        self.totals = {"emulators": 0, "write_events": 0, "flush_hits": 0}
        self.store = pk_device.store_of(
            self.env.device
        )  # the store area of this device
        self.store_writes: set[int] = set()
        self.all_writes: set[int] = set()
        self.used_cards: set[str] = set()
        self.tier = "full"
        self.lock = threading.Lock()  # rigs of one scenario may run in threads
        self.t0 = time.monotonic()

    def parallel(self, *fns: Callable[[], None]) -> None:
        """Run independent parts of a scenario (each boots its own daemon) in threads, at most
        pk_harness.part_jobs(parts) at once (DIGIKIT_PART_JOBS); the first error is raised after all have ended."""
        errors: list[BaseException] = []
        width = threading.Semaphore(H.part_jobs(len(fns)))

        def guarded(fn: Callable[[], None]) -> Callable[[], None]:
            def inner() -> None:
                with width:
                    try:
                        fn()
                    except BaseException as e:  # noqa: BLE001  raised in the scenario's thread
                        errors.append(e)

            return inner

        threads = [threading.Thread(target=guarded(fn)) for fn in fns]
        for th in threads:
            th.start()
        for th in threads:
            th.join()
        if errors:
            raise errors[0]

    def table(self) -> dict[str, Any]:
        """The scenario's data table of this kind, with the device's own sub-table laid over it."""
        d = dict(self.scen.data[self.scen.kind])
        d.update(d.pop(self.device, {}))
        for other in QUICK_IMAGES:
            d.pop(other, None)
        return d

    def check(self, name: str, ok: bool, detail: str = "") -> None:
        self.checks.append((name, bool(ok), detail))
        print(f"{'ok  ' if ok else 'FAIL'} {name} {detail}", flush=True)

    def image_for(self, static: str, name: str) -> B.Img:
        key = f"{name}:{static}"
        if key not in self.imgs:
            self.imgs[key] = self.env.build_image(static, name)
        return self.imgs[key]

    def pack(self, module: str, file: str) -> str:
        self.env.pack(module, file)
        return f"modules/{file}"

    def pack_synth(self, spec: str, file: str) -> str:
        """A synthetic module (`patchkit synth-dkm`) in the store folder; returns its store path."""
        self.env.synth(spec, file)
        return f"modules/{file}"

    def card(
        self,
        selection: list[str] | None,
        counter: int = 0,
        seq: int = 1,
        flags: int = 0,
        recs: tuple[bytes | None, bytes | None] | None = None,
        base: Path | None = None,
        stable_blocks: int = 0,
        foreign: dict[int, bytes] | None = None,
    ) -> Path:
        """A card image, cached by content: the store of the folder and the record copies (or SELECTION), optionally
        laid over a `base` card file (the card a scenario left behind). FOREIGN: sector images (absolute sector -> 512-byte multiple)
        written over the card, bytes that are not ours in the store range. Returns the cached file."""
        h = hashlib.sha256(self.image.encode())
        # the store format and area are part of the key: a card built for another area or version is not this card
        h.update(b"store-v3.1" + repr(self.store).encode())
        store_dir = self.env.folder / "digikit"
        for f in sorted(store_dir.rglob("*")):
            if f.is_file():
                h.update(str(f.relative_to(store_dir)).encode() + f.read_bytes())
            elif f.is_dir() and not any(
                f.iterdir()
            ):  # an empty folder is a folder entry of the store
                h.update(b"dir:" + str(f.relative_to(store_dir)).encode())
        a, b = recs if recs else (None, None)
        if recs is None and selection is not None:
            # a card laid over a base keeps the base's sticky machine types, as the menu's SELECT does (the new record is not a fresh card's)
            tmap = None
            if base is not None:
                cur = B.current(B.records_in(base, b"", self.env.device))
                tmap = cur["map"] if cur else None
            a = b = B.make_record(selection, seq, counter, flags, stable_blocks, tmap)
        for x in (a, b):
            h.update(x or b"-")
        if base is not None:  # a card made by `card_after` is named by its content
            h.update(base.name.encode())
        for s, data in sorted((foreign or {}).items()):
            h.update(b"foreign:%d:" % s + data)
        key = h.hexdigest()[:20]
        dst = CACHE / "cards" / f"{self.image}-{key}.img"
        written = not dst.exists()
        if written:
            dst.parent.mkdir(parents=True, exist_ok=True)
            tmp = self.work / f"card-{key}.img"
            if base is not None:
                from tools.emulator import card_overlay

                card_overlay.clone(base, tmp)
            else:
                tmp = self.env.card(None, tag=key)
            if a is not None or b is not None:
                B.write_record_sectors(tmp, a, b, self.env.device)
            if foreign:
                with open(tmp, "r+b") as f:
                    for s, data in sorted(foreign.items()):
                        f.seek(s * 512)
                        f.write(data)
            shutil.move(tmp, dst)
        card_used(dst, written)
        return dst

    def rig(
        self,
        img: B.Img,
        card: Path,
        tag: str,
        extra_env: dict[str, str] | None = None,
        cf_only: bool = False,
        settle_ms: int = SETTLE_MS,
    ) -> B.BootRig:
        """A rig cold-booted on CARD (SETTLE_MS: emulated ms it runs after the boot; a panel script that saves a project needs the boot tool's 1500); CF_ONLY: on a daemon without DSP and audio (about 3 times faster to boot, idle time free)."""
        ck = hashlib.sha256(card.name.encode() + img.syx.read_bytes()).hexdigest()[:10]
        if salt := os.environ.get(
            "DIGIKIT_RUN_SALT"
        ):  # a fresh run directory: every cold boot runs again (timing of a cold start)
            ck = hashlib.sha256((salt + ck).encode()).hexdigest()[:10]
        if cf_only:
            ck = "cf" + ck
        if "-after-" in card.name or card.name in self.used_cards:
            # a card the firmware saved a project on never runs the DT2 filesystem check the readiness contract waits for
            extra_env = {"DIGI_COLD_RELAX_FS": "1", **(extra_env or {})}
        return self.env.boot(
            img,
            card,
            f"scn-{ck}",
            watch=True,
            extra_env=extra_env,
            cf_only=cf_only,
            settle_ms=settle_ms,
        )  # one run directory per (firmware, card): its boot cache is reused

    def done(self, rig: B.BootRig) -> None:
        """Collect the card writes and the watch summaries of a rig that is finished."""
        store: set[int] = set()
        every: set[int] = set()
        try:
            store = set(rig.card_store_writes())
            from tools.coldfire import arena_safety

            ov = rig.work / f"{rig.tag}.all-overlay.bin"
            every = set(arena_safety.parse_overlay(ov.read_bytes()))
        except Exception as e:  # noqa: BLE001
            print(f"card.overlay: {e}", file=sys.stderr)
        w = rig.watch_summaries()
        with self.lock:
            self.store_writes |= store
            self.all_writes |= every
            for k in self.totals:
                self.totals[k] += w[k]

    def common_checks(
        self,
        firmware_areas: tuple[tuple[int, int], ...] = (),
        store_extra: frozenset[int] = frozenset(),
    ) -> None:
        """The checks every scenario ends with. FIRMWARE_AREAS: sector ranges (first, end) below 0x50000 that the scenario makes the firmware write on purpose (the kit library of a Save Kit).
        STORE_EXTRA: store-range sectors besides the two selection sectors that the scenario writes on purpose (the settings copies)."""
        from tools.coldfire import arena_safety

        t = self.totals
        self.check(
            "write-watch on cf.expansion over every boot: no stray write",
            t["write_events"] == 0 and t["emulators"] > 0,
            str(t),
        )
        self.check("flush check over every boot: no hit", t["flush_hits"] == 0, str(t))
        policy = arena_safety.runtime_card_policy(
            self.all_writes, self.device, firmware_areas, store_extra
        )
        stray = sorted(
            self.store_writes - {self.store.sel_a, self.store.sel_b} - store_extra
        )
        self.check(
            "card writes in the store range: only the two selection sectors"
            + (
                " and the sectors the scenario writes on purpose" if store_extra else ""
            ),
            not stray,
            f"wrote {sorted(hex(s) for s in self.store_writes)}",
        )
        outside = policy["invalid_low"]
        self.check(
            "card writes elsewhere: only the firmware's own working project and project slots (sectors from 0x50000)"
            + (
                " and the areas the scenario writes on purpose"
                if firmware_areas
                else ""
            ),
            not outside,
            f"{len(self.all_writes)} sectors, below 0x50000: {[hex(x) for x in outside[:6]]}"
            + "".join(
                f"; in {hex(lo)}..{hex(hi)}: {len(inside)} sectors {hex(min(inside))}..{hex(max(inside))}"
                for lo, hi in firmware_areas
                if (inside := [x for x in self.all_writes if lo <= x < hi])
            ),
        )

    def common_checks_upload(self, project_writes_ok: bool = True) -> None:
        """The checks of the `upload` scenarios: the same write-watch and flush check; the card writes are the store range (an upload
        writes data, tables and both header copies there) and, when PROJECT_WRITES_OK, the firmware's own sectors from 0x50000."""
        t = self.totals
        self.check(
            "write-watch on cf.expansion over every boot: no stray write",
            t["write_events"] == 0 and t["emulators"] > 0,
            str(t),
        )
        self.check("flush check over every boot: no hit", t["flush_hits"] == 0, str(t))
        lo, hi = self.store.first, self.store.end
        writes = self.all_writes | self.store_writes
        outside = sorted(
            x
            for x in writes
            if not (lo <= x < hi) and (x < FIRMWARE_FIRST or not project_writes_ok)
        )
        self.check(
            "card writes: the module store range"
            + (
                " and the firmware's own project sectors"
                if project_writes_ok
                else " only"
            ),
            not outside,
            f"{len(writes)} sectors, elsewhere: {[hex(x) for x in outside[:6]]}",
        )

    def result(self) -> dict[str, Any]:
        self.report["checks"] = [
            {"name": a, "ok": b, "detail": c} for a, b, c in self.checks
        ]
        self.report["pass"] = all(c[1] for c in self.checks) and bool(self.checks)
        self.report["seconds"] = round(time.monotonic() - self.t0, 1)
        return self.report


# ---- decoding the firmware's tables from a booted rig ---------------------------------------------------------------------------


def cstr(rig: B.BootRig, addr: int, limit: int = 40) -> str:
    """The NUL-terminated string at ADDR (read by words; the registry's strings are in RAM or rodata)."""
    if not addr:
        return ""
    out = bytearray()
    for k in range(0, limit, 4):
        out += rig.w(addr + k).to_bytes(4, "big")
        if 0 in out:
            break
    return bytes(out).split(b"\0")[0].decode("latin-1")


def decode_tables(rig: B.BootRig) -> dict[str, Any]:
    """The machine, parameter and page rows with a name, decoded without pointers or type numbers: parameters are named,
    knobs name their parameter (or keep a stock code), pages name their machine."""
    plan = rig.plan
    o = "core/machines"
    rows, count = M.sym(plan, o, "machines.rows"), M.sym(plan, o, "machines.count")
    first, size = M.sym(plan, o, "machines.first"), M.sym(plan, o, "machines.row_size")
    pf = M.sym(plan, "core/params", "pf_table")
    pfirst, pcount = (
        M.sym(plan, "core/params", "params.first"),
        M.sym(plan, "core/params", "params.count"),
    )
    pnames: dict[int, str] = {}
    params: dict[str, Any] = {}
    for k in range(pcount):
        code = pfirst + k
        row = pf + 60 * code
        name = cstr(rig, rig.w(row + 40))
        if not name:
            continue
        pnames[code] = name
        words = [rig.w(row + 4 * i) for i in range(15)]
        params[name] = {
            "short": cstr(rig, words[12]),
            "mirror": words[1],
            "min": words[2],
            "max": words[3],
            "default": words[4],
            "template_words": [words[i] for i in (0, 5, 6, 7, 8, 9, 11, 13, 14)],
        }
    mnames: dict[int, str] = {}
    machines: dict[str, Any] = {}
    for k in range(count):
        row = rows + size * k
        name = cstr(rig, rig.w(row + 56))
        if not name:
            continue
        mnames[first + k] = name
        f = [rig.w(row + 8 + 4 * i) for i in range(9)]
        machines[name] = {
            "short": cstr(rig, rig.w(row + 60)),
            "clone_of": rig.w(row + 44),
            "own_dsp": rig.w(row + 52),
            "knobs": [pnames.get(c, c) for c in f[:8]],
            "level": f[8],
        }
    pages: list[Any] = []
    try:
        prow, pn = M.sym(plan, o, "pages.rows"), M.sym(plan, o, "pages.count")
    except Exception:  # noqa: BLE001  the Digitakt II has no page rows
        prow, pn = 0, 0
    for k in range(pn):
        row = prow + 84 * k
        mach, name = rig.w(row + 44), cstr(rig, rig.w(row))
        if not name or mach not in mnames:
            continue
        pages.append(
            {
                "name": name,
                "title": cstr(rig, rig.w(row + 4)),
                "machine": mnames[mach],
                "knobs": [
                    pnames.get(c, c) for c in (rig.w(row + 8 + 4 * i) for i in range(8))
                ],
                "level": rig.w(row + 8 + 32),
                "slots": [rig.w(row + 48 + 4 * i) for i in range(9)],
            }
        )
    return {"machines": machines, "params": params, "pages": pages}


def diff_tables(a: dict[str, Any], b: dict[str, Any]) -> list[str]:
    """Differences between two decoded table sets (empty: equal)."""
    out = []
    for kind in ("machines", "params"):
        for name in sorted(set(a[kind]) | set(b[kind])):
            if a[kind].get(name) != b[kind].get(name):
                out.append(
                    f"{kind[:-1]} {name}: {a[kind].get(name)} != {b[kind].get(name)}"
                )
    ka = sorted(a["pages"], key=lambda p: (p["machine"], p["name"]))
    kb = sorted(b["pages"], key=lambda p: (p["machine"], p["name"]))
    if ka != kb:
        out.append(f"pages: {ka} != {kb}")
    return out


class PlainRig(B.BootRig):
    """A reference image without the runtime (no mailbox, no boot info): only the firmware and the patched-in set."""

    def start(self) -> None:
        self.s.open(self.syx)
        self.s.segment = "boot"
        self.s.reset()
        self.s.run_ms(1500)


# ---- the kinds ------------------------------------------------------------------------------------------------------------------


def run_equiv(c: Ctx) -> None:
    """The reference boot (the static twin patched in) and the run-time boot (the set loaded from the card) are independent daemons:
    they run in two threads and their results are compared afterwards. A set without a twin is compared with a boot of no module."""
    t = c.table()
    mods = [
        c.pack(m, f"m{i}.dkm") for i, m in enumerate(t["modules"])
    ]  # the run-time set, in selection order
    twin = t.get("twin", "")
    run_img = c.image_for(RT, "run")
    empty = c.card(None)
    play = t.get("play", "")
    trig = t.get("trig_frame", M.TRIG_FRAME if c.dt2 else M.TRIG_FRAME_DN2)
    # e.g. SHARC_UNINIT: a machine that keeps state beyond its cleared bytes reads uninitialised L2 at its first call, as on the device
    env_ref: dict[str, str] = dict(t.get("env_ref", {}))
    env_run: dict[str, str] = dict(t.get("env_run", {}))
    pin = t.get(
        "pcm_sha256", ""
    )  # the run-time note is compared with this hash, not with a twin's note
    plain = bool(
        t.get("ref_plain")
    )  # the patched-in set does not fit next to the runtime's DSP area (DN2 pluck)
    ref_img = (c.image_for(twin if plain else RT + "," + twin, "ref")) if twin else None
    card = c.card(mods)
    out: dict[str, Any] = {}

    def select(rig: Any, hint_key: str) -> None:
        if c.dt2:
            return
        if (
            play in ("pluck", "sine") or "play_machine" in t
        ):  # DN2: the machine list position (finding 33); the sound proves the type
            rig.select(t[hint_key] if hint_key in t else t["hint"])

    def base_boot() -> (
        None
    ):  # the tables of a boot with no module (the reference of a set with no twin)
        with c.rig(run_img, empty, "none") as rig:
            out["ref"] = decode_tables(rig)
            c.report["boot_none"] = rig.boot_info()
            c.done(rig)

    def ref_boot() -> None:
        assert ref_img is not None
        ref_rig = (
            PlainRig(
                c.env,
                ref_img,
                "ref",
                empty,
                False,
                {"DIGI_WRITE_WATCH_SEQ": "1", **env_ref},
            )
            if plain
            else c.rig(ref_img, empty, "ref", env_ref)
        )
        with ref_rig as rig:
            out["ref"] = decode_tables(rig)
            if play and not pin:
                if c.dt2:
                    rig.select_type(t["static_type"], t.get("hint", 7))
                select(rig, "hint")
                pcm = rig.note(c.work / "ref.wav", trig)
                out["ref_pcm"] = pcm
                c.check(
                    "the reference note is audible",
                    float(abs(pcm).max()) > 0.01,
                    f"peak {float(abs(pcm).max()):.3f}",
                )
            if not plain:
                c.done(rig)

    def run_boot() -> None:
        with c.rig(run_img, card, "run", env_run) as rig:
            info = rig.boot_info()
            n = len(mods)
            c.check(
                "the selection loaded at boot: every module result is 0",
                info["count"] == n
                and info["results"] == [0] * n
                and info["loaded"] == n
                and info["safe"] == 0,
                f"{info['results']}",
            )
            c.report[
                "boot_ready"
            ] = {  # finding 37: the DSP part at the cold boot's ready point
                "dsp_blocks": rig.frames_at_ready,
                "ms": round(rig.frames_at_ready * 64 / 48.0, 1),
                "results": rig.at_ready["results"],
                "voice_rows": len(rig.voice_at_ready),
                "blocks_ready_to_dsp_done": rig.blocks_to_dsp,  # DSP blocks (1.33 ms) from the ready point to the voice row being in
            }
            out["got"] = decode_tables(rig)
            used = [r for r in rig.voice_rows() if r[0] != B.FREE_TYPE]
            want_voices = t.get("voices", 0)
            c.check(
                "voice rows claimed",
                len(used) == want_voices,
                f"{[(hex(a), hex(b)) for a, b in used]}, want {want_voices}",
            )
            if play:
                if c.dt2:
                    rig.select_type(
                        rig.dyn_type(
                            t.get("play_module", M.MODULE),
                            t.get("play_machine", M.MACHINE),
                        ),
                        t.get("hint_run", t.get("hint", 7)),
                    )
                select(rig, "hint_run")
                out["run_pcm"] = rig.note(c.work / "run.wav", trig)
                out["run_rig_checks"] = rig  # pcm_check reports through a rig
            c.done(rig)

    c.parallel(run_boot, ref_boot if twin else base_boot)
    ref, got = out["ref"], out["got"]
    for name in t.get(
        "ignore_machines", []
    ):  # a static twin's machine with no run-time counterpart
        ref["machines"].pop(name, None)
    for name in t.get(
        "ignore_run_machines", []
    ):  # a run-time module's machine with no twin: its machine row, its parameters and its pages
        gone = got["machines"].pop(name, None) or {}
        mine = [p for p in got["pages"] if p["machine"] == name]
        for k in list(gone.get("knobs", [])) + [k for p in mine for k in p["knobs"]]:
            got["params"].pop(k, None)
        got["pages"] = [p for p in got["pages"] if p["machine"] != name]
    d = diff_tables(got, ref)
    c.check(
        "machine, parameter and page rows equal the "
        + ("static twin's" if twin else "no-module boot's"),
        not d,
        "; ".join(d[:3]),
    )
    if twin:
        c.check(
            "the run-time set added rows",
            bool(got["machines"]) or bool(got["params"]) or bool(got["pages"]),
        )
    if play and pin:
        rr = out["run_rig_checks"]
        M.pcm_pin(
            rr,
            out["run_pcm"],
            pin,
            c.report,
            "voice",
            t.get("pcm_samples", M.PIN_SAMPLES),
        )
        c.checks += rr.checks
    elif play:
        rr = out["run_rig_checks"]
        M.pcm_check(
            rr,
            out["ref_pcm"],
            out["run_pcm"],
            c.report,
            "machine" if c.dt2 else "voice",
        )
        c.checks += rr.checks
    c.common_checks()


def run_dkrun(
    c: Ctx, rig: B.BootRig, D: Any, blobs: dict[str, bytes], files: dict[str, str]
) -> None:
    """`tools.device.dkrun.run` against the emulator on a boot with no module: upload, SELECT, RESTART, the new boot's MOD_LOAD and MOD_DSP."""
    from tools.device import dkrun as R
    from tools.device import dkupload as DK
    from tools.patchkit import pk_digikit_upload as U

    (module,) = c.table()["modules"]
    (path,) = blobs
    dkm = c.work / "dkrun" / f"{module.split('/')[-1]}.dkm"
    dkm.parent.mkdir(exist_ok=True)
    dkm.write_bytes(blobs[path])
    up = DK.Uploader(U.transport(rig))
    steps = {"n": 0}

    def sleep(s: float) -> None:
        rig.emu.step(int(s * R.EMU_IPS))
        steps["n"] += int(s * R.EMU_IPS)

    out = R.run(
        up,
        module,
        wait_boot=lambda: D._wait_restart(rig),
        sleep=sleep,
        now=lambda: steps["n"] / R.EMU_IPS,
        seconds=0,
        dkm=dkm,
    )
    c.check(
        "dkrun: runtime 4 or newer",
        out["info"]["runtime"] >= R.MIN_RUNTIME,
        str(out["info"]),
    )
    c.check("dkrun: the module was uploaded", out["uploaded"] and out["path"] == path)
    res = {(d["kind"], d["args"][0]): d["args"][1] for d in out["results"]}
    c.check(
        "dkrun: after the reboot the debug module loads (MOD_LOAD result=0), then MOD_LOAD result=1 (LOADING) and MOD_DSP result=0 for the module",
        res.get(("MOD_LOAD", 0)) == 0
        and res.get(("MOD_LOAD", 1)) == 1
        and res.get(("MOD_DSP", 1)) == 0,
        str(res),
    )
    rec = B.current(rig.sel_records())
    c.check(
        "dkrun: SELECT wrote the record: the debug module, the module and nothing else",
        rec is not None and rec["paths"] == [R.module_path(R.DEBUG_MODULE), path],
        str(rec),
    )
    # a second run finds the file on the device (the same sha256) and uploads nothing (it stops after SELECT: one boot is enough here)
    out2 = R.run(
        DK.Uploader(U.transport(rig)),
        module,
        sleep=sleep,
        now=lambda: steps["n"] / R.EMU_IPS,
        seconds=0,
        dkm=dkm,
        restart=False,
    )
    c.check("dkrun: a second run uploads nothing (same sha256)", not out2["uploaded"])


def run_transfer(
    c: Ctx, rig: B.BootRig, D: Any, blobs: dict[str, bytes], files: dict[str, str]
) -> None:
    """`tools.device.dktransfer.install` against the emulator on a boot with no module: build only what changed (nothing: the committed
    objects are current), pack, upload, SELECT, RESTART, wait for MODULES. A second install builds and uploads nothing."""
    from tools.device import dkrun as R
    from tools.device import dktransfer as TR
    from tools.device import dkupload as DK
    from tools.patchkit import pk_digikit_upload as U

    (module,) = c.table()["modules"]
    (path,) = blobs
    steps = {"n": 0}

    def sleep(s: float) -> None:
        rig.emu.step(int(s * R.EMU_IPS))
        steps["n"] += int(s * R.EMU_IPS)

    def now() -> float:
        return steps["n"] / R.EMU_IPS

    up = DK.Uploader(U.transport(rig))
    rows = TR.repo_rows(up.info()["port"])
    before = TR.list_view(up, rows)
    row = next(r for r in before["modules"] if r["id"] == module)
    c.check(
        "transfer list: the module is listed, in a group, not on the card, needs no build",
        row["group"] == "Machines"
        and row["on_card"] == "no"
        and not row["loaded"]
        and row["build"] == "unchanged"
        and before["card_only"] == [],
        str(row),
    )
    events: list[dict[str, Any]] = []
    out = TR.install(up, [module], emit=events.append, rows=rows, select=True)
    c.check(
        "transfer install (no restart): nothing built (unchanged), uploaded, verified by the device, selected",
        out["built"] == []
        and out["uploaded"] == [module]
        and out["selected"] == [path]
        and not out["restarted"]
        and [e["ok"] for e in events if e["event"] == "verify"] == [True],
        str(events),
    )
    c.check(
        "transfer install (no restart): the module is not loaded yet",
        module not in [m["name"] for m in up.modules()["modules"]],
    )
    res = TR.restart_device(
        up,
        [module],
        emit=events.append,
        wait_boot=lambda: D._wait_restart(rig),
        sleep=sleep,
        now=now,
        paths=[path],
    )
    c.check(
        "transfer restart: MODULES lists the module after the boot",
        res["loaded"] == [module] and res["missing"] == [],
        str(res),
    )
    rec = B.current(rig.sel_records())
    c.check(
        "transfer install: SELECT wrote the module and nothing else",
        rec is not None and rec["paths"] == [path],
        str(rec),
    )
    up = DK.Uploader(U.transport(rig))
    after = TR.list_view(up, rows)
    row = next(r for r in after["modules"] if r["id"] == module)
    c.check(
        "transfer list after the install: on the card, loaded, fits",
        row["on_card"] == "yes" and row["loaded"] and row["fits"] is True,
        str(row),
    )

    plan = TR.plan_install(TR.repo_rows(up.info()["port"], [module]), [module], up.ls())
    c.check(
        "transfer install, second time: nothing to build or upload",
        plan["build"] == [] and plan["upload"] == [] and plan["select"] == [path],
        str(plan),
    )
    hdr = up.header(path)
    c.check(
        "READ: the file on the card has the .dkm header the repo packed",
        hdr == Path(row["dkm"]).read_bytes()[: DK.HEADER_BYTES],
    )


def memory_rows(cap: dict[str, dict[str, int]]) -> list[str]:
    """The rows the MEMORY page shows for a CAPACITY reply (modules/core/apps/digikit-menu `fill_mem`): label, ten-character bar, percent or used/limit."""
    out = []
    for kind, label, pct in (
        ("DSP_AREA", "DSP   ", True),
        ("ARENA", "CPU   ", True),
        ("MODULES", "MODS  ", False),
        ("MACHINE_ROWS", "MACH  ", False),
        ("VOICE_ROWS", "VOICE ", False),
        ("PARAM_ROWS", "PARAM ", False),
        ("PAGE_ROWS", "PAGE  ", False),
    ):
        e = cap.get(kind)
        if e is None or e["limit"] == 0:
            continue
        used = min(e["used"], e["limit"])
        full = used * 10 // e["limit"]
        bar = "|" * full + "." * (10 - full)
        tail = f"{used * 100 // e['limit']}%" if pct else f"{e['used']}/{e['limit']}"
        out.append(f"{label}{bar} {tail}")
    return out


def check_memory_page(c: Ctx, rig: B.BootRig, plan: Any) -> None:
    """SETTINGS > DIGIKIT > MEMORY on a boot that loaded a machine: the rows the menu kept in `DK_MEM` equal the rows computed from the
    CAPACITY reply of the same boot (so the DSP percent is the same figure rounded the same way), and MACH and PARAM show the machine (a page row only where the device has page rows: the equality covers it)."""
    from tools.device import dkupload as DK
    from tools.patchkit import pk_digikit_upload as U

    def tap(key: str, gap_ms: int = 350) -> None:
        rig.s.do_tap(key)
        rig.s.run_ms(gap_ms)

    tap("NO", 400)  # SETTINGS > DIGIKIT (the ninth row) > MEMORY (the third row)
    tap("SETTINGS", 600)
    for _ in range(8):
        tap("DOWN", 200)
    tap("YES", 700)
    for _ in range(2):
        tap("DOWN", 200)
    tap("YES", 700)
    base, n = plan.addr("DK_MEM"), rig.w(plan.addr("DK_MEM_N"))
    got = []
    for i in range(n):
        raw = b"".join(
            rig.w(base + 24 * i + 4 * k).to_bytes(4, "big") for k in range(6)
        )
        got.append(raw.split(b"\0")[0].decode("latin-1"))
    from tools.control.digi_client import oled_png

    (c.work / "memory-page.png").write_bytes(
        oled_png(rig.s.frame()[1])
    )  # with `--keep`: to look at the labels
    cap = DK.Uploader(U.transport(rig)).capacity()
    want = memory_rows(cap)
    c.check(
        "MEMORY page: the rows equal the CAPACITY reply of the same boot, DSP row included",
        got == want and any(r.startswith("DSP   ") for r in got),
        f"page {got}; want {want}",
    )
    used = {k: cap[k]["used"] for k in ("MACHINE_ROWS", "PARAM_ROWS")}
    c.check(
        "MEMORY page: MACH and PARAM show the loaded machine",
        all(v >= 1 for v in used.values())
        and all(
            any(r.startswith(f"{k} ") and f" {cap[kk]['used']}/" in r for r in got)
            for k, kk in (("MACH", "MACHINE_ROWS"), ("PARAM", "PARAM_ROWS"))
        ),
        f"{used}; {got}",
    )


def run_menu(c: Ctx) -> None:
    """Boot with an empty selection, install the set over USB (`dk run` or `dktransfer install`, by the table), and after the boot
    compare with the patched-in set as `equiv` does (rows, machine in the list, PCM of a note). The set may hold a code-only module
    next to the machine."""
    from tools.coldfire import digikit_menu_proof as D

    t = c.table()
    names = {m: m.split("/")[-1] for m in t["modules"]}
    files = {m: c.pack(m, f"{names[m]}.dkm") for m in t["modules"]}
    rows = sorted(names.values())  # the menu lists the store's files in name order
    dkrun = bool(
        t.get("dkrun")
    )  # the selection and the restart come from `tools.device.dkrun` (SELECT, RESTART)
    transfer = bool(t.get("transfer"))  # dktransfer.install does what dkrun does
    assert dkrun or transfer, "the menu kind needs dkrun or transfer in its table"
    blobs = {}
    for path in files.values():  # the store is filled over USB SysEx, not by the card build: take the packed files out of the folder
        f = c.env.folder / "digikit" / path
        blobs[path] = f.read_bytes()
        f.unlink()
    menu_img = c.image_for(f"{RT},core/digikit-menu", "menu")
    plan = D.Plan(menu_img.plan)
    empty = c.card(None)
    trig = t.get("trig_frame", M.TRIG_FRAME if c.dt2 else M.TRIG_FRAME_DN2)
    twin = t["twin"]
    plain = bool(t.get("ref_plain"))
    ref_img = c.image_for(twin if plain else RT + "," + twin, "ref")
    ref_rig = (
        PlainRig(c.env, ref_img, "ref", empty, False, {"DIGI_WRITE_WATCH_SEQ": "1"})
        if plain
        else c.rig(ref_img, empty, "ref")
    )
    with ref_rig as rig:
        ref = decode_tables(rig)
        if c.dt2:
            rig.select_type(t["static_type"], t.get("hint", 7))
        else:
            rig.select(t["hint"])
        ref_pcm = rig.note(c.work / "ref.wav", trig)
        c.check(
            "the reference note is audible",
            float(abs(ref_pcm).max()) > 0.01,
            f"peak {float(abs(ref_pcm).max()):.3f}",
        )
        if not plain:
            c.done(rig)

    with c.rig(menu_img, empty, "menu") as rig:
        b0 = rig.boot_info()
        c.check("the first boot has no module", b0["count"] == 0 and b0["loaded"] == 0)
        if transfer:
            run_transfer(c, rig, D, blobs, files)
        elif dkrun:
            run_dkrun(c, rig, D, blobs, files)
        info = rig.boot_info()
        k = len(rows)
        extra = []  # the debug module `dkrun` selects next to the set
        if dkrun:
            from tools.device import dkrun as R

            extra = [R.module_path(R.DEBUG_MODULE)]
        kd = k + len(extra)
        c.check(
            "the boot after the install loaded the whole set",
            info["count"] == kd
            and info["loaded"] == kd
            and info["results"] == [0] * kd
            and info["safe"] == 0,
            str(info["results"]),
        )
        rec = B.current(rig.sel_records())
        c.check(
            "the record in use holds the ticked modules",
            rec is not None
            and sorted(rec["paths"]) == sorted([*files.values(), *extra]),
            str(rec),
        )
        got = decode_tables(rig)
        d = diff_tables(got, ref)
        c.check(
            "the machine is in the list: machine, parameter and page rows equal the patched-in set's",
            not d and bool(got["machines"]),
            "; ".join(d[:3]),
        )
        used = [r for r in rig.voice_rows() if r[0] != B.FREE_TYPE]
        c.check("voice row claimed", len(used) == t.get("voices", 1), str(used))
        if c.dt2:
            rig.select_type(
                rig.dyn_type(
                    t.get("play_module", M.MODULE), t.get("play_machine", M.MACHINE)
                ),
                t.get("hint_run", t.get("hint", 7)),
            )
        else:
            rig.select(t.get("hint_run", t["hint"]))
        # the DSP block count runs on through the panel script and the restart: the note starts a little after the setup
        pcm = rig.note(c.work / "run.wav", max(trig, rig.frames() + 300))
        M.pcm_check(rig, ref_pcm, pcm, c.report, "machine" if c.dt2 else "voice")
        check_memory_page(c, rig, plan)
        c.checks += rig.checks
        c.done(rig)
    c.common_checks_upload()


def run_upload(c: Ctx) -> None:
    from tools.patchkit import pk_digikit_upload as U

    U.run_upload(c)


def run_pages(c: Ctx) -> None:
    from tools.patchkit import pk_digikit_pages as PG

    PG.run_pages(c)


KINDS = {
    "pages": run_pages,
    "equiv": run_equiv,
    "upload": run_upload,
    "menu": run_menu,
}


def finish_work_dir(work: Path, ok: bool) -> None:
    """Remove a temporary work dir after a pass; keep it (and say so) after a failure or an exception."""
    if ok:
        shutil.rmtree(work, ignore_errors=True)
    else:
        print(f"work dir kept: {work}", file=sys.stderr, flush=True)


def run_scenario(
    scen: Scenario, image: str, keep: Path | None = None, tier: str | None = None
) -> dict[str, Any]:
    if keep:
        return _run_scenario(scen, image, keep, tier)
    work = Path(tempfile.mkdtemp(prefix=f"digikit-{scen.name}-"))
    ok = False
    try:
        rep = _run_scenario(scen, image, work, tier)
        ok = bool(rep["pass"])
        return rep
    finally:
        finish_work_dir(work, ok)


def _run_scenario(
    scen: Scenario, image: str, work: Path, tier: str | None
) -> dict[str, Any]:
    work.mkdir(parents=True, exist_ok=True)
    c = Ctx(scen, image, work)
    c.tier = tier or os.environ.get("DIGIKIT_TIER", "quick")
    print(f"--- {scen.name} on {image}", flush=True)
    prof = None
    if os.environ.get(
        "DIGIKIT_PROFILE"
    ):  # a per-phase wall-time table at the end (pk_digikit_prof)
        from tools.patchkit import pk_digikit_prof

        prof = pk_digikit_prof.install()
    try:
        KINDS[scen.kind](c)
    except Exception as e:  # noqa: BLE001  a scenario that cannot run is a failed scenario, with its reason
        import traceback

        traceback.print_exc()
        c.check(f"scenario error: {type(e).__name__}: {e}", False)
    rep = c.result()
    if prof is not None:
        print(prof.report(), flush=True)
    (work / "report.json").write_text(json.dumps(rep, indent=1, default=str))
    return rep


@dataclass
class Summary:
    results: list[dict[str, Any]] = field(default_factory=list)

    @property
    def ok(self) -> bool:
        return all(r["pass"] for r in self.results)


def run_pairs(
    pairs: list[tuple[Scenario, str]],
    tier: str,
    jobs: int,
    keep: Path | None = None,
    quiet: bool = False,
) -> dict[tuple[str, str], dict[str, Any]]:
    """The (scenario, image) pairs in child processes, JOBS at a time, the long ones first. Returns the report of each pair (its
    `report.json`; a pair whose process died without one gets a failed report that names its log). One log per pair in KEEP (or a
    temporary directory, removed when every pair passes)."""
    if keep:
        return _run_pairs(pairs, tier, jobs, keep, quiet)
    base = Path(tempfile.mkdtemp(prefix="digikit-jobs-"))
    ok = False
    try:
        reports = _run_pairs(pairs, tier, jobs, base, quiet)
        ok = all(r["pass"] for r in reports.values())
        return reports
    finally:
        finish_work_dir(base, ok)


def _run_pairs(
    pairs: list[tuple[Scenario, str]],
    tier: str,
    jobs: int,
    base: Path,
    quiet: bool,
) -> dict[tuple[str, str], dict[str, Any]]:
    import subprocess
    from concurrent.futures import ThreadPoolExecutor

    base.mkdir(parents=True, exist_ok=True)
    t0 = time.monotonic()
    rank = {
        "upload": 1,
        "equiv": 4,
    }
    order = sorted(pairs, key=lambda p: rank.get(p[0].kind, 9))

    def one(pair: tuple[Scenario, str]) -> tuple[tuple[str, str], dict[str, Any]]:
        s, image = pair
        log = base / f"{s.name}-{image}.log"
        cmd = [
            sys.executable,
            "-m",
            "tools.patchkit.pk_digikit_scen",
            "run",
            s.name,
            "--image",
            image,
            "--tier",
            tier,
            "--keep",
            str(base),
        ]
        with log.open("w") as f:
            subprocess.run(
                cmd, cwd=ROOT, stdout=f, stderr=subprocess.STDOUT, env=python_env()
            )
        report = base / f"{s.name}-{image}" / "report.json"
        try:
            rep = json.loads(report.read_text())
        except (OSError, ValueError):
            rep = {"scenario": s.name, "image": image, "pass": False, "seconds": 0,
                   "checks": [{"name": f"no result; see {log}", "ok": False, "detail": ""}]}  # fmt: skip
        if not quiet:
            bad = [c["name"] for c in rep["checks"] if not c["ok"]]
            print(
                f"{'PASS' if rep['pass'] else 'FAIL'} {s.name} {image} ({rep['seconds']} s)"
                + (f": {bad}" if bad else "")
                + f"   [{time.monotonic() - t0:.0f} s since start, log {log}]",
                flush=True,
            )
        return (s.name, image), rep

    with ThreadPoolExecutor(max_workers=max(1, jobs)) as pool:
        return dict(pool.map(one, order))


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("cmd", choices=["list", "run"])
    ap.add_argument("names", nargs="*")
    ap.add_argument("--image", action="append", default=[])
    ap.add_argument("--tier", choices=["quick", "full"], default="quick")
    ap.add_argument("--keep", type=Path)
    ap.add_argument(
        "--jobs",
        type=int,
        default=1,
        help="run the (scenario, image) pairs in this many processes at once (each prints to its own log; the summary follows)",
    )
    a = ap.parse_args(argv)
    scens = selected(a.tier, a.names)
    if a.cmd == "list":
        for s in scens:
            print(f"{s.name:28s} {s.kind:10s} {s.tier:5s} {','.join(s.images(a.tier))}")
        return 0
    pairs = [
        (s, image)
        for s in scens
        for image in a.image or s.images(a.tier)
        if image[:3] in s.devices or s.data["scenario"].get("images")
    ]
    if a.jobs > 1 and len(pairs) > 1:
        reports = run_pairs(pairs, a.tier, a.jobs, a.keep)
        return 0 if all(r["pass"] for r in reports.values()) else 1
    summary = Summary()
    for s, image in pairs:
        keep = a.keep / f"{s.name}-{image}" if a.keep else None
        rep = run_scenario(s, image, keep, a.tier)
        summary.results.append(rep)
        bad = [c["name"] for c in rep["checks"] if not c["ok"]]
        print(
            f"{'PASS' if rep['pass'] else 'FAIL'} {s.name} {image} ({rep['seconds']} s)"
            + (f": {bad}" if bad else ""),
            flush=True,
        )
    return 0 if summary.ok else 1


if __name__ == "__main__":
    sys.exit(main())
