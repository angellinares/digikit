#!/usr/bin/env python3
"""Shared configuration for the native build, launch, PGO and run tools.

One place for the pinned Rust toolchain (read from mise.toml), the default
paths of the private inputs, the firmware file, and the per-firmware fast-tier
tables (config/emulator.json). tools/emulator/native_emu.py, native_pgo.py,
native_target.py and coupled.py import it.

    uv run python -m tools.emulator.digi_env        # print the resolved settings
"""

from __future__ import annotations

import functools
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tomllib
from collections.abc import Callable

from tools.firmware import config

ROOT = pathlib.Path(__file__).resolve().parents[2]
# The DN2 ready-inputs directory: ready_dir in tools/emulator/ready_inputs.json (shared
# with the JS tools), overridden by DIGI_EMU_READY_DIR.
_READY_JSON = json.loads(
    (ROOT / "tools" / "emulator" / "ready_inputs.json").read_text()
)
READY = ROOT / (os.environ.get("DIGI_EMU_READY_DIR") or _READY_JSON["ready_dir"])
# The ready directory of the other devices (`ready_dirs` in the same file),
# overridden per device by DIGI_EMU_READY_DIR_<DEVICE> (DIGI_EMU_READY_DIR_DT2).
READY_DIRS = {
    "dn2": READY,
    **{
        k: ROOT / (os.environ.get(f"DIGI_EMU_READY_DIR_{k.upper()}") or v)
        for k, v in _READY_JSON.get("ready_dirs", {}).items()
    },
}
DEFAULT_GENERATED = ROOT / "out" / "native" / "dn2-audio" / "gen"
DEFAULT_SYX = config.stock("Digitone_II_OS1.12.syx")
FAST_TABLE = ROOT / "config" / "emulator.json"
TOOLCHAIN: str = tomllib.loads((ROOT / "mise.toml").read_text())["tools"]["rust"]
TRIPLE = "aarch64-apple-darwin"

Environ = dict[str, str] | os._Environ[str]


GEN_INDEX = ROOT / "out" / "native" / "gen-index.json"


def _image_sha(image: pathlib.Path | str) -> str:
    return hashlib.sha256(pathlib.Path(image).read_bytes()).hexdigest()


def gen_for_image(image: pathlib.Path | str) -> pathlib.Path | None:
    """The generated core (block library) made for a packed DSP image, found by
    the image's SHA-256 in out/native/gen-index.json; None when none is
    registered (the image then runs interpreted on any core)."""
    try:
        index = json.loads(GEN_INDEX.read_text())
        found = pathlib.Path(index[_image_sha(image)])
    except (OSError, KeyError, ValueError):
        return None
    return found if found.is_dir() else None


def register_gen(image: pathlib.Path | str, gen: pathlib.Path | str) -> None:
    """Record GEN as the core for IMAGE (tools/sharc_dev/aot_gen.py)."""
    try:
        index = json.loads(GEN_INDEX.read_text())
    except (OSError, ValueError):
        index = {}
    index[_image_sha(image)] = str(pathlib.Path(gen).resolve())
    GEN_INDEX.parent.mkdir(parents=True, exist_ok=True)
    GEN_INDEX.write_text(json.dumps(index, indent=1, sort_keys=True) + "\n")


def exe_name(name: str) -> str:
    """A cargo binary's file name on this host: NAME, or NAME.exe on Windows."""
    return name + ".exe" if os.name == "nt" else name


def generated_dir(
    environ: Environ | None = None, image: pathlib.Path | str | None = None
) -> pathlib.Path:
    """The generated core to build against: SHARC_GEN_DIR, else the one
    registered for IMAGE (a packed DSP image), else the DN2 prod default."""
    environ = os.environ if environ is None else environ
    configured = environ.get("SHARC_GEN_DIR")
    if configured:
        # Relative to the repo root (mise runs tasks there); build scripts run in their crate dir.
        path = pathlib.Path(configured)
        return path if path.is_absolute() else ROOT / path
    if image is not None and (registered := gen_for_image(image)) is not None:
        return registered
    return DEFAULT_GENERATED


CORE_FINGERPRINT_FILE = "CORE_FINGERPRINT"


def core_fingerprint(root: pathlib.Path | None = None) -> str:
    """SHA-256 over the paths and contents of the sources a generated core is
    made from: tools/sharc_dev/sharc_core/**/*.py, sharc_transpile/**/*.py,
    sharc_transpile_infer.py and sharc_rsgen.py, with paths relative to
    tools/sharc_dev."""
    root = ROOT if root is None else pathlib.Path(root)
    base = root / "tools" / "sharc_dev"
    files = [
        *base.joinpath("sharc_core").rglob("*.py"),
        *base.joinpath("sharc_transpile").rglob("*.py"),
        base / "sharc_transpile_infer.py",
        base / "sharc_rsgen.py",
    ]
    digest = hashlib.sha256()
    for path in sorted(files, key=lambda f: f.relative_to(base).as_posix()):
        digest.update(path.relative_to(base).as_posix().encode() + b"\0")
        digest.update(path.read_bytes() + b"\0")
    return digest.hexdigest()


def write_core_fingerprint(gen: pathlib.Path | str) -> None:
    """Stamp a gen dir with the fingerprint of the current sharc_core."""
    (pathlib.Path(gen) / CORE_FINGERPRINT_FILE).write_text(core_fingerprint() + "\n")


def check_generated(gen: pathlib.Path, environ: Environ | None = None) -> None:
    """Refuse a generated core not made from the current tools/sharc_dev/sharc_core
    (DIGI_ALLOW_STALE_GEN=1 turns the error into a warning)."""
    environ = os.environ if environ is None else environ
    gen = pathlib.Path(gen)
    try:
        have = (gen / CORE_FINGERPRINT_FILE).read_text().strip()
    except OSError:
        have = "missing"
    want = core_fingerprint()
    if have == want:
        return
    message = (
        f"generated core {gen} is stale: its {CORE_FINGERPRINT_FILE} is {have}, "
        f"tools/sharc_dev/sharc_core is {want}.\n"
        "Regenerate it: DT2: mise run perf:aot -- dt2 --card-image IMG; "
        "DN2: uv run python -m tools.sharc_dev.sharc_dn2_aot (recipe in gen/PROVENANCE.txt); "
        "shared core: mise run gen:core.\n"
        "Or set DIGI_ALLOW_STALE_GEN=1 to use it anyway."
    )
    if environ.get("DIGI_ALLOW_STALE_GEN") == "1":
        print(
            f"warning: {message.splitlines()[0]} (DIGI_ALLOW_STALE_GEN=1)",
            file=sys.stderr,
        )
        return
    raise SystemExit(message)


def firmware(environ: Environ | None = None) -> pathlib.Path:
    """The DN2 .syx: DIGI_EMU_SYX, then DIGI_COUPLED_SYX, then the firmware/ default."""
    environ = os.environ if environ is None else environ
    return pathlib.Path(
        environ.get("DIGI_EMU_SYX") or environ.get("DIGI_COUPLED_SYX") or DEFAULT_SYX
    )


@functools.cache
def _patch_base(path: str, mtime_ns: int, size: int) -> tuple[str, str] | None:
    """(device short id, base sha256) of a patched image, read by the patchkit binary's
    `inspect` (the one reader of the DKPR record); None when the file has no patch record."""
    from tools.patchkit import patchkit

    try:
        with open(path, "rb") as handle:
            if (
                handle.read(1) != b"\xf0" or size < 100_000
            ):  # not a firmware .syx: skip the patchkit build
                return None
        done = subprocess.run(
            [str(patchkit.binary()), "--root", str(ROOT), "inspect", path],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
    except (OSError, SystemExit):
        return None
    if done.returncode:
        return None
    device = base = None
    for line in done.stdout.splitlines():
        words = line.split()
        if words[:1] == ["device"] and len(words) > 1:
            device = words[1]
        elif words[:3] == ["record", "patched", "from"] and len(words) > 3:
            base = words[3]
    return (device, base) if device and base else None


def patch_base(path: pathlib.Path) -> tuple[str, str] | None:
    """(device, base sha256) when `path` is a patched image (it carries a `DKPR` record), else None."""
    try:
        st = pathlib.Path(path).stat()
    except OSError:
        return None
    return _patch_base(str(path), st.st_mtime_ns, st.st_size)


def stock_for_sha(digest: str) -> pathlib.Path | None:
    """The firmware/ file of a stock image by SHA-256 (devices/*.toml `filename`); None when it is not listed or not present."""
    for toml_path in sorted((ROOT / "devices").glob("*.toml")):
        doc = tomllib.loads(toml_path.read_text())
        for f in doc.get("firmware", []):
            if f.get("sha256") == digest:
                path = config.stock(f["filename"])
                return path if path.is_file() else None
    return None


def base_firmware(path: pathlib.Path) -> pathlib.Path:
    """The stock image a patched image was made from (for the +Drive builder and anything else
    that needs the stock bytes); `path` itself when it is stock or the base is not in firmware/."""
    info = patch_base(path)
    return (stock_for_sha(info[1]) if info else None) or pathlib.Path(path)


def device_for_syx(path: pathlib.Path) -> str | None:
    """The device short id (`dn2`, `dt2`) of a firmware file, by SHA-256 against
    devices/*.toml; a patched image by the device of its patch record; None when
    no profile knows it."""
    try:
        digest = hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()
    except OSError:
        return None
    for toml_path in sorted((ROOT / "devices").glob("*.toml")):
        doc = tomllib.loads(toml_path.read_text())
        if any(f.get("sha256") == digest for f in doc.get("firmware", [])):
            return doc["device"]["short"]
    info = patch_base(path)
    return info[0] if info else None


def firmware_for(device: str, environ: Environ | None = None) -> pathlib.Path:
    """The .syx of a device: DIGI_EMU_SYX_<DEVICE>, else the firmware/ default
    (DN2 also honours DIGI_EMU_SYX and DIGI_COUPLED_SYX, see `firmware`)."""
    environ = os.environ if environ is None else environ
    if device == "dn2":
        return firmware(environ)
    default = {"dt2": "Digitakt_II_OS1.17.syx"}[device]
    return pathlib.Path(
        environ.get(f"DIGI_EMU_SYX_{device.upper()}") or config.stock(default)
    )


def ready_inputs(
    environ: Environ | None = None, device: str = "dn2"
) -> dict[str, pathlib.Path]:
    """The private audio inputs of a device, each overridable by its DIGI_EMU_*
    variable (DN2)."""
    environ = os.environ if environ is None else environ
    if device != "dn2":
        base = READY_DIRS[device]
        return {
            "firmware": firmware_for(device, environ),
            "DSP image": base / "digi-audio-dn2-image.bin",
            "DSP state": base / "digi-audio-m5.snap.dsp",
            "CF snapshot": base / "digi-audio-m5.snap",
        }
    return {
        "firmware": firmware(environ),
        "DSP image": pathlib.Path(
            environ.get("DIGI_EMU_DSP_IMAGE", READY / "digi-audio-dn2-image.bin")
        ),
        "DSP state": pathlib.Path(
            environ.get("DIGI_EMU_DSP_STATE", READY / "digi-audio-m5.snap.dsp")
        ),
        "CF snapshot": pathlib.Path(
            environ.get("DIGI_EMU_CF_SNAPSHOT", READY / "digi-audio-m5.snap")
        ),
    }


@functools.cache
def _syx_digest(path: str) -> str:
    try:
        return hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()
    except OSError:
        return ""


def fast_table() -> dict[str, dict[str, str]]:
    table = json.loads(FAST_TABLE.read_text())
    return {k: v for k, v in table.items() if not k.startswith("_")}


def _entry(syx: pathlib.Path) -> dict[str, str]:
    return fast_table().get(_syx_digest(str(syx)), {})


def fast_regions_for(syx: pathlib.Path) -> str:
    """The verified fast regions for this firmware file, or "" if unknown."""
    return _entry(syx).get("fast_regions", "")


def fallback_regions_for(syx: pathlib.Path) -> str:
    """Runtime fallback roots for invalidated generated entries, or empty."""
    return _entry(syx).get("fallback_regions", "")


def blocktrans_roots_for(syx: pathlib.Path) -> str:
    """The verified block-translator cluster roots for this firmware, or ""."""
    return _entry(syx).get("blocktrans", "")


def fast_env(
    environ: Environ | None = None, *, syx: pathlib.Path | None = None
) -> dict[str, str]:
    """Fast regions, fallback regions and cluster roots for a coupled run.

    User-set values win; DIGI_EMU_FAST=0 suppresses defaults; otherwise use
    the firmware table. Explicit regular regions or auto selection suppress
    fallback defaults. Keys with nothing to set are left out.
    """
    environ = os.environ if environ is None else environ
    out: dict[str, str] = {}
    if environ.get("DIGI_EMU_FAST") == "0":
        return out
    syx = firmware(environ) if syx is None else syx
    if "SHARC_FAST_FALLBACK_REGIONS" in environ:
        out["SHARC_FAST_FALLBACK_REGIONS"] = environ["SHARC_FAST_FALLBACK_REGIONS"]
    # Explicit auto selection must still be able to arm these as ordinary roots.
    elif (
        "SHARC_FAST_REGIONS" not in environ
        and "SHARC_FAST_AUTO" not in environ
        and (fallback := fallback_regions_for(syx))
    ):
        out["SHARC_FAST_FALLBACK_REGIONS"] = fallback
    for name, lookup in (
        ("SHARC_FAST_REGIONS", fast_regions_for),
        ("SHARC_BLOCKTRANS", blocktrans_roots_for),
    ):
        if name in environ:
            out[name] = environ[name]
        elif value := lookup(syx):
            out[name] = value
    return out


# Engine slots of a two-core build. Slot 1 (SHARC_GEN_DIR, unsuffixed region lists) is always the DN2 core and
# slot 2 (SHARC_GEN_DIR_2, `_2` lists) always the DT2 core, whichever firmware boots. The region lists are
# addresses in one device's DSP image and each engine instance reads the list of its own slot.
SLOT_DEVICES = ("dn2", "dt2")
SLOT_SUFFIX = {"dn2": "", "dt2": "_2"}
SLOT_LISTS = ("SHARC_FAST_REGIONS", "SHARC_FAST_FALLBACK_REGIONS", "SHARC_BLOCKTRANS")


def slot_env(device: str, env: dict[str, str]) -> dict[str, str]:
    """ENV (a device's `fast_env`) with its region lists renamed for the device's engine slot of a two-core build."""
    suffix = SLOT_SUFFIX[device]
    return {
        (key + suffix if key in SLOT_LISTS else key): value
        for key, value in env.items()
    }


@functools.cache
def toolchain_rustc() -> str:
    """The pinned rustup toolchain's rustc ("" when rustup cannot find it).

    Cargo otherwise takes `rustc` from PATH; a Homebrew rustc of the same
    version mangles crates differently, so PGO profiles silently stop matching.
    """
    try:
        done = subprocess.run(
            ["rustup", "which", "--toolchain", TOOLCHAIN, "rustc"],
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError:
        return ""
    return done.stdout.strip() if done.returncode == 0 else ""


def rust_env(environ: Environ | None = None) -> dict[str, str]:
    """A copy of the environment with RUSTC/RUSTDOC pinned to the toolchain."""
    environ = os.environ if environ is None else environ
    env = dict(environ)
    if env.get("SHARC_GEN_DIR"):
        # Cargo build scripts run in their crate directory, so pass the generated core as an absolute path.
        env["SHARC_GEN_DIR"] = str(generated_dir(env))
    rustc = toolchain_rustc()
    if rustc:
        env["RUSTC"] = rustc
        env["RUSTDOC"] = str(pathlib.Path(rustc).with_name("rustdoc"))
        env["PATH"] = f"{pathlib.Path(rustc).parent}:{env.get('PATH', '')}"
    else:
        print(
            f"WARNING: rustup toolchain {TOOLCHAIN} not found; cargo will use the "
            "rustc on PATH (PGO profiles may not match)",
            file=sys.stderr,
        )
    return env


# Run output and diagnostics


RUNS_ROOT = ROOT / "out" / "runs"


def run_dir(kind: str, stamp: str | None = None) -> pathlib.Path:
    """A new out/runs/<kind>/<UTC stamp>/ (created); kind/latest points at it.

    The run layout of every run tool: report.json, perf.json, audio.wav and
    logs/ (diagnostics, see `diag_env`) inside it.
    """
    import datetime

    kind = "".join(c if c.isalnum() or c in "._+-" else "_" for c in kind)[:80]
    stamp = stamp or datetime.datetime.now(datetime.UTC).strftime("%Y%m%d-%H%M%S")
    base = RUNS_ROOT / kind
    base.mkdir(parents=True, exist_ok=True)
    path = base / stamp
    n = 1
    # mkdir decides, so parallel runs with the same stamp get different directories
    while True:
        try:
            path.mkdir()
            break
        except FileExistsError:
            n += 1
            path = base / f"{stamp}-{n}"
    link = base / f".latest-{os.getpid()}"
    link.unlink(missing_ok=True)
    link.symlink_to(path.name)
    link.replace(base / "latest")
    _prune_runs(base, path)
    return path


KEEP_RUNS = 30
_STAMP_RE = re.compile(r"\d{8}-\d{6}(-\d+)?")


def _prune_runs(base: pathlib.Path, current: pathlib.Path) -> None:
    """Keep the newest KEEP_RUNS stamped run dirs of one kind.

    Symlinks (`latest`), the new
    dir and the latest target are never removed.
    """
    try:
        keep = {current.name, os.readlink(base / "latest")}
        runs = sorted(
            e.name
            for e in base.iterdir()
            if _STAMP_RE.fullmatch(e.name) and e.is_dir() and not e.is_symlink()
        )
    except OSError:
        return
    for name in runs[:-KEEP_RUNS]:
        if name not in keep:
            shutil.rmtree(base / name, ignore_errors=True)


def _w(**pairs: str) -> Callable[[str, pathlib.Path], dict[str, str]]:
    """Env builder: each value is a file name under logs/ (or "" for the arg)."""

    def build(arg: str, logs: pathlib.Path) -> dict[str, str]:
        return {k: (str(logs / v) if v else arg) for k, v in pairs.items()}

    return build


# name -> (takes an argument, output goes to stderr, env builder, what it records).
# `--diag LIST` of the run tools; the builder gets the argument and logs/.
DIAG: dict[
    str, tuple[bool, bool, Callable[[str, pathlib.Path], dict[str, str]], str]
] = {
    "dspi": (False, False, _w(DIGI_DSPI_LOG="dspi.bin"), "DSPI traffic log"),
    "dspwatch": (
        True,
        False,
        _w(DIGI_DSP_WATCH="", DIGI_DSP_WATCH_OUT="dspwatch.csv"),
        "DSP words ADDR[,ADDR] sampled per frame",
    ),
    "model": (
        False,
        False,
        _w(DIGI_DSP_MODEL_LOG="model.csv"),
        "per-frame DSP tier counters",
    ),
    "wwatch": (
        True,
        False,
        _w(SHARC_WWATCH="", SHARC_WWATCH_OUT="wwatch.csv"),
        "writes to DSP words ADDR[:N],..: icount, PC, unit",
    ),
    "pcdump": (
        True,
        False,
        _w(SHARC_PCDUMP="", SHARC_PCDUMP_OUT="pcdump.csv"),
        "registers at the listed PCs (SHARC_PCDUMP_ICOUNT/LIMIT/MEM tune it)",
    ),
    "fastcov": (
        False,
        True,
        lambda a, logs: {
            "SHARC_FAST_LOG": "summary",
            "SHARC_FAST_COV": "1",
            "SHARC_FAST_HIST": "1",
        },
        "fast-tier coverage, refusals, exits and the interpreter histogram",
    ),
    "fastlog": (
        False,
        True,
        lambda a, logs: {"SHARC_FAST_LOG": "summary"},
        "fast-tier summary",
    ),
    "hist": (
        False,
        True,
        lambda a, logs: {"SHARC_FAST_HIST": "1"},
        "interpreted instructions per PC",
    ),
    "dump": (
        False,
        False,
        _w(SHARC_FAST_DUMP="fastdump"),
        "fast-tier kernels and calls (build feature fast-dump; coupled.py adds it)",
    ),
    "pcm": (
        False,
        False,
        lambda a, logs: {},
        "audio.wav next to the report (coupled.py --wav)",
    ),
}


def parse_diag(text: str) -> list[tuple[str, str]]:
    """`dspi,wwatch=1a0:4,2b0,hist` -> [(name, arg)]; a token that is not a
    name continues the previous argument (specs contain commas)."""
    items: list[list[str]] = []
    for token in (t.strip() for t in text.split(",")):
        if not token:
            continue
        head, _, arg = token.partition("=")
        if head in DIAG:
            items.append([head, arg])
        elif items and DIAG[items[-1][0]][0]:
            items[-1][1] += "," + token
        else:
            raise SystemExit(
                f"--diag: unknown name {token!r} (known: {', '.join(DIAG)})"
            )
    for name, arg in items:
        if DIAG[name][0] and not arg:
            raise SystemExit(f"--diag: {name} needs a value ({name}=SPEC)")
    return [(n, a) for n, a in items]


def diag_env(names: str | list[tuple[str, str]], run: pathlib.Path) -> dict[str, str]:
    """The environment for `--diag` names: each diagnostic's variables, writing
    under RUN/logs/, plus DIGI_DIAG_DIR (the native default for unset *_OUT)."""
    items = parse_diag(names) if isinstance(names, str) else names
    if not items:
        return {}
    logs = pathlib.Path(run) / "logs"
    logs.mkdir(parents=True, exist_ok=True)
    env = {"DIGI_DIAG_DIR": str(logs)}
    for name, arg in items:
        env.update(DIAG[name][2](arg, logs))
    return env


def diag_stderr(names: str | list[tuple[str, str]]) -> bool:
    """True when a named diagnostic prints to stderr (keep it as logs/stderr.log)."""
    items = parse_diag(names) if isinstance(names, str) else names
    return any(DIAG[n][1] for n, _ in items)


def main() -> int:
    if sys.argv[1:2] == ["exec"] and len(sys.argv) > 3 and sys.argv[2] == "--":
        cmd = sys.argv[3:]
        os.execvpe(cmd[0], cmd, {**rust_env(), "SHARC_GEN_DIR": str(generated_dir())})
    print(f"toolchain   {TOOLCHAIN} ({toolchain_rustc() or 'not installed'})")
    print(f"generated   {generated_dir()}")
    for label, path in ready_inputs().items():
        print(f"{label:<11} {path} ({'ok' if path.is_file() else 'missing'})")
    for key, value in fast_env().items():
        print(f"{key}={value}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
