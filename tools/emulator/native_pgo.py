#!/usr/bin/env python3
"""Profile-guided optimisation for the coupled DN2 native desktop build.

State lives under ``out/native/pgo/dn2/<host-triple>/`` (ignored by git):

    manifest.json          what the profile of the last main-tree training was trained from
    by-key/<key>.json      the same record per profile key; every training writes one, and a
                           lane's training writes only this (``train --set-current`` also
                           overwrites manifest.json). Readers use the by-key file of the
                           tree's current key when there is one (status ok), else manifest.json.
    profiles/<sha>.profdata  merged profile, content addressed
    bench-<time>/          copied binaries, logs, bench.tsv, summary.json

Cargo target dirs come from tools/emulator/native_target.py, keyed by the generated
core (``out/native/target/<gen-key>``): the benchmark control is the plain
release build in the key dir itself (shared with the desktop app and the
fast-tier examples, so the generated core is not recompiled), the profile
builds differ by RUSTFLAGS and use ``pgo-instrument/`` and ``pgo-use/`` below it.

Commands: ``train``, ``clean [--dry-run] [--days N]``, ``check``, ``plan``, ``bench --pairs N``,
``bench-bins A B --pairs N`` (two prebuilt workload test binaries).

DT2 (``--device dt2`` on ``train``/``check``/``coupled-build``): the desktop app
has no DT2 workload (its gate restores the DN2 ready snapshot), so the profile is
trained on an instrumented ``coupled-run`` (packages/emulator/coupled) on the one-sample
card's ready state (tools/emulator/dt2_sample_check.py): ``dt2-sample-trig`` plus the FX
scenarios of :data:`DT2_WORKLOAD` (bit reduction, delay send, reverb send, master
compressor). State lives in ``out/native/pgo/dt2/<triple>/`` (own manifest and
profiles). The generated core is SHARC_GEN_DIR, else the one registered for the
DT2 DSP image. The profile applies to ``coupled-run`` only; ``coupled-build
--profile PATH|current`` builds it (any profile, e.g. the DN2 one for a
cross-device check) and tools/devtools/coupled_ab.py times it::

    uv run python -m tools.emulator.native_pgo train --device dt2 [--runs 3] [--variant NAME]
    uv run python -m tools.emulator.native_pgo check --device dt2
    uv run python -m tools.emulator.native_pgo coupled-build --device dt2 [--profile PATH|current] [--variant NAME]

DT2 (``--device dt2`` on ``train``/``check``/``coupled-build``): the desktop app
has no DT2 workload (the gate restores the DN2 ready snapshot), so the profile is
trained on an instrumented ``coupled-run`` (packages/emulator/coupled) on the one-sample
card's ready state (tools/emulator/dt2_sample_check.py): ``dt2-sample-trig`` plus the FX
scenarios of :data:`DT2_WORKLOAD` (bit reduction, delay send, reverb send,
master compressor), a few seconds each. State lives in
``out/native/pgo/dt2/<triple>/`` (own manifest and profiles). The generated core
is SHARC_GEN_DIR, else the one registered for the DT2 DSP image. The profile
applies to ``coupled-run`` (``coupled-build``, then tools/devtools/coupled_ab.py); the
desktop app's DT2 build is not trained here. ``coupled-build --profile PATH``
applies any profile, e.g. the DN2 one (a cross-device check)::

    uv run python -m tools.emulator.native_pgo train --device dt2 [--runs 3]
    uv run python -m tools.emulator.native_pgo check --device dt2
    uv run python -m tools.emulator.native_pgo coupled-build --device dt2 [--profile PATH] --variant NAME

Two cores (``--device both`` on ``train``/``check``/``verify``/``plan``/``coupled-build``): the
desktop build holds both generated cores (feature ``second-core``, ``SHARC_GEN_DIR`` and
``SHARC_GEN_DIR_2``) and has its own profile in ``out/native/pgo/both/<triple>/``. Slot 1
(``SHARC_GEN_DIR``) is always the DN2 core and slot 2 (``SHARC_GEN_DIR_2``) always the DT2 core,
in training and in the launcher; the engine is chosen by the firmware's device at run time.
Training runs an instrumented two-core ``coupled-run``, one process per workload item:
the DN2 ``exactness-100m`` scenario (the ``coupled_ready_exactness`` workload), the DT2
:data:`DT2_WORKLOAD`, and one runtime replay per device from a fixture directory
(``out/fixtures/runtime/<dev>/``, see :func:`load_fixture`; ``mise run perf:fixture -- --device DEV``
makes it)::

    uv run python -m tools.emulator.native_pgo train --device both [--fixture-dn2 DIR] [--fixture-dt2 DIR]
    uv run python -m tools.emulator.native_pgo check --device both

The training workload is the ``coupled_ready_exactness`` gate, run from the
instrumented desktop APP binary with ``--pgo-train`` (not the test binary:
cargo gives the test target's dependencies other crate hashes than the app's,
so a profile trained on it never matched the app). It needs the private ready fixtures and a generated DSP core (``SHARC_GEN_DIR``).
The launcher (``tools/emulator/native_emu.py``) does not retrain: it warns about a stale
profile and uses it anyway. Retrain with ``mise run perf:pgo`` or by launching with
DIGI_EMU_PGO_TRAIN=1. The key covers the emulation crates only (not apps/),
so a desktop-crate edit leaves the profile current; the fast-region lists are
read at run time and are not part of the key.

The toolchain must be the rustup 1.98.1 distribution: a Homebrew rustc has the
same version string but different LLVM, and cannot read these profiles.
"""

from __future__ import annotations

import argparse
import contextlib
import dataclasses
import datetime
import hashlib
import json
import os
import pathlib
import platform
import re
import shutil
import stat
import statistics
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parents[2]
from tools.emulator import digi_env  # noqa: E402

MANIFEST_PATH = "apps/desktop/src-tauri/Cargo.toml"
DEFAULT_GENERATED = digi_env.DEFAULT_GENERATED
READY = digi_env.READY
TOOLCHAIN = digi_env.TOOLCHAIN
TRIPLE = digi_env.TRIPLE
FEATURES = "coupled-audio"
BOTH = "both"
BOTH_FEATURES = "second-core"
BOTH_WORKLOAD_NAME = "both-v1"
TEST_NAME = "desktop_runtime::coupled::tests::coupled_ready_exactness"
WORKLOAD = "desktop-bin"
EXPECTED_FRAMES = 1139
# The workload is deterministic: one pass gave host instruction counts within 0.03% of four
# on dn2-reset, dn2 and dt2 (sharc_cells, 2026-11-07), so training runs it once.
TRAIN_RUNS = 1
FLAGS_TEMPLATE = "-Cprofile-use={profdata}"
SKIP_DIRS = {
    "target",
    "out",
    ".git",
    "node_modules",
}  # `out`: generated output that a test run leaves inside a crate (gitignored)
SKIP_FILES = {".DS_Store"}


def native_target():
    """tools/emulator/native_target.py: cargo target dirs keyed by generated core."""
    from tools.emulator import native_target as module

    return module


def benchlock(mode: str, label: str):
    """Lock for builds (shared) and timed runs (exclusive), see benchlock.py."""
    from tools.devtools import benchlock as module

    return module.hold(mode, label)


def supported_host() -> bool:
    return sys.platform == "darwin" and platform.machine() == "arm64"


def state_dir(triple: str = TRIPLE, device: str = "dn2") -> pathlib.Path:
    return ROOT / "out" / "native" / "pgo" / device / triple


def toolchain_dir() -> pathlib.Path:
    home = pathlib.Path(
        os.environ.get("RUSTUP_HOME") or pathlib.Path.home() / ".rustup"
    )
    return home / "toolchains" / f"{TOOLCHAIN}-{TRIPLE}"


def llvm_profdata() -> pathlib.Path:
    return toolchain_dir() / "lib" / "rustlib" / TRIPLE / "bin" / "llvm-profdata"


def generated_dir(
    environ: dict[str, str] | os._Environ[str] | None = None,
) -> pathlib.Path:
    return digi_env.generated_dir(environ)


def second_generated(
    environ: dict[str, str] | os._Environ[str] | None = None,
) -> pathlib.Path | None:
    """The second generated core (SHARC_GEN_DIR_2) of a two-core build, None for one core."""
    environ = os.environ if environ is None else environ
    value = environ.get("SHARC_GEN_DIR_2")
    return pathlib.Path(value) if value else None


def both_generated(
    environ: dict[str, str] | os._Environ[str] | None = None,
) -> tuple[pathlib.Path, pathlib.Path]:
    """The cores of a two-core build in slot order: (DN2 core, DT2 core).

    SHARC_GEN_DIR and SHARC_GEN_DIR_2 name them in that order; without them the
    core registered for each device's ready DSP image is used (DN2: the default
    core), as the launcher finds them (native_emu.second_core).
    """
    environ = os.environ if environ is None else environ
    dn2 = digi_env.gen_for_image(
        digi_env.ready_inputs(environ, device="dn2")["DSP image"]
    )
    dt2 = digi_env.gen_for_image(
        digi_env.ready_inputs(environ, device="dt2")["DSP image"]
    )
    gen1 = (
        pathlib.Path(environ["SHARC_GEN_DIR"])
        if environ.get("SHARC_GEN_DIR")
        else (dn2 or DEFAULT_GENERATED)
    )
    gen2 = second_generated(environ) or dt2
    if gen2 is None:
        raise SystemExit(
            "no generated DT2 core registered for the DT2 DSP image "
            "(mise run perf:aot -- dt2 --card-image IMG, or set SHARC_GEN_DIR_2)"
        )
    for name, gen in (("DN2", gen1), ("DT2", gen2)):
        if not gen.is_dir():
            raise SystemExit(f"generated {name} core not found: {gen}")
    return gen1, gen2


def rustc_version() -> str:
    rustc = toolchain_dir() / "bin" / "rustc"
    return subprocess.run(
        [str(rustc), "-vV"], capture_output=True, text=True, check=True
    ).stdout


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


class FileHasher:
    """sha256 of files, memoised on (size, mtime_ns) in a JSON cache."""

    def __init__(self, cache_path: pathlib.Path) -> None:
        self.cache_path = cache_path
        try:
            self.old: dict[str, list[object]] = json.loads(cache_path.read_text())
        except (OSError, ValueError):
            self.old = {}
        self.new: dict[str, list[object]] = {}

    def digest(self, path: pathlib.Path) -> str:
        st = path.stat()
        key = str(path)
        hit = self.old.get(key)
        if hit and hit[0] == st.st_size and hit[1] == st.st_mtime_ns:
            self.new[key] = hit
            return str(hit[2])
        sha = sha256_file(path)
        self.new[key] = [st.st_size, st.st_mtime_ns, sha]
        return sha

    def save(self) -> None:
        if self.new == self.old:
            return
        self.cache_path.parent.mkdir(parents=True, exist_ok=True)
        # The cache only saves time. The cache file is shared by all lanes: write a unique temp file and
        # rename it, so a concurrent writer cannot remove ours. A lost update is acceptable (it is rebuilt).
        tmp = self.cache_path.with_name(f"{self.cache_path.name}.{os.getpid()}.tmp")
        try:
            tmp.write_text(json.dumps(self.new))
            tmp.replace(self.cache_path)
        except OSError:
            tmp.unlink(missing_ok=True)


def tree_files(top: pathlib.Path) -> list[pathlib.Path]:
    found: list[pathlib.Path] = []
    for dirpath, dirnames, filenames in os.walk(top):
        dirnames[:] = sorted(d for d in dirnames if d not in SKIP_DIRS)
        found.extend(
            pathlib.Path(dirpath) / n for n in sorted(filenames) if n not in SKIP_FILES
        )
    return found


def tree_digest(
    hasher: FileHasher, top: pathlib.Path, base: pathlib.Path, skip_top: set[str]
) -> tuple[str, int]:
    """Digest of (relative path, sha256) over a tree; skip_top names top-level dirs to leave out."""
    outer = hashlib.sha256()
    count = 0
    for path in tree_files(top):
        rel = path.relative_to(top)
        if rel.parts[0] in skip_top and len(rel.parts) > 1:
            continue
        outer.update(f"{path.relative_to(base)}\0{hasher.digest(path)}\n".encode())
        count += 1
    return outer.hexdigest(), count


DESKTOP_DIR = str(pathlib.PurePosixPath(MANIFEST_PATH).parent)


def is_emulation_package(rel: str) -> bool:
    """True for a crate that shapes hot code; the UI shell under apps/ does not.

    The profile is keyed by the emulation code only. A slightly stale profile is
    safe: LLVM ignores profile data for functions whose CFG hash changed, so a
    desktop-crate edit costs nothing, while a profile treated as stale makes the
    launcher fall back to a non-PGO build (about 30% slower DSP).
    """
    return pathlib.PurePosixPath(rel).parts[:1] != ("apps",)


def local_package_dirs(features: str = FEATURES) -> list[str]:
    """Repo-relative directories of the emulation crates in the build graph of FEATURES
    (the two-core build adds the `-2` crates)."""
    out = subprocess.run(
        [
            "rustup",
            "run",
            TOOLCHAIN,
            "cargo",
            "metadata",
            "--format-version",
            "1",
            "--manifest-path",
            MANIFEST_PATH,
            "--features",
            features,
            "--filter-platform",
            TRIPLE,
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    dirs = set()
    for package in json.loads(out)["packages"]:
        if package["source"] is None:
            dirs.add(
                str(pathlib.Path(package["manifest_path"]).parent.relative_to(ROOT))
            )
    return sorted(d for d in dirs if is_emulation_package(d))


def _pgo_tiers(environ: dict[str, str] | os._Environ[str] | None):
    environ = os.environ if environ is None else environ
    return digi_env.resolve_tiers("pgo", digi_env.firmware(environ), "", environ)


def fast_regions(environ: dict[str, str] | os._Environ[str] | None = None) -> str:
    """The fast-tier region list a coupled run uses ("" when off).

    Rules in `emulator_config.resolve`; the firmware is DIGI_EMU_SYX, then DIGI_COUPLED_SYX.
    An explicit SHARC_FAST_REGIONS wins even under DIGI_EMU_FAST=0.
    """
    return _pgo_tiers(environ).effective("SHARC_FAST_REGIONS")


def fallback_regions(environ: dict[str, str] | os._Environ[str] | None = None) -> str:
    """Runtime fallback list, respecting explicit fast-region diagnostics."""
    return _pgo_tiers(environ).effective("SHARC_FAST_FALLBACK_REGIONS")


def blocktrans_roots(environ: dict[str, str] | os._Environ[str] | None = None) -> str:
    """The SHARC_BLOCKTRANS cluster roots a coupled run uses ("" when none)."""
    return _pgo_tiers(environ).effective("SHARC_BLOCKTRANS")


def compute_components(
    packages: list[str],
    gen: pathlib.Path,
    hasher: FileHasher,
    fast: str = "",
    blocktrans: str = "",
    gen2: pathlib.Path | None = None,
) -> tuple[dict[str, str], int]:
    """The profile key components.

    ``fast`` and ``blocktrans`` are kept for signature compatibility only: the
    fast-region lists are read at run time and do not change the binary, so
    they no longer enter the key. ``gen2`` is the second generated core of the
    two-core build (SHARC_GEN_DIR_2): its digest, the feature and the workload
    name of device ``both`` enter the key, and so does the slot order.
    """
    outer = hashlib.sha256()
    files = 0
    for rel in packages:
        if not is_emulation_package(rel):
            continue
        digest, count = tree_digest(hasher, ROOT / rel, ROOT, {"gen"})
        outer.update(f"{rel}\0{digest}\n".encode())
        files += count
    gen_digest, gen_count = (
        tree_digest(hasher, gen, gen, set()) if gen.is_dir() else ("absent", 0)
    )
    workspace = hashlib.sha256()
    for name in ("Cargo.toml", "Cargo.lock"):
        workspace.update(hasher.digest(ROOT / name).encode())
    components = {
        "sources": outer.hexdigest(),
        "workspace": workspace.hexdigest(),
        "generated": gen_digest,
        "rustc": hashlib.sha256(rustc_version().encode()).hexdigest(),
        "triple": TRIPLE,
        "features": FEATURES,
        "workload": WORKLOAD,
        "flags_template": FLAGS_TEMPLATE,
    }
    if gen2 is not None:
        gen2_digest, gen2_count = (
            tree_digest(hasher, gen2, gen2, set()) if gen2.is_dir() else ("absent", 0)
        )
        components["generated2"] = gen2_digest
        components["features"] = BOTH_FEATURES
        components["workload"] = BOTH_WORKLOAD_NAME
        components["slots"] = "generated=dn2,generated2=dt2"
        gen_count += gen2_count
    return components, files + gen_count


def key_of(components: dict[str, str]) -> str:
    return hashlib.sha256(json.dumps(components, sort_keys=True).encode()).hexdigest()


def device_state(device: str = "dn2") -> pathlib.Path:
    """The profile state directory of a device (DN2: the original location)."""
    return state_dir() if device == "dn2" else state_dir(device=device)


def read_manifest(device: str = "dn2") -> dict[str, object] | None:
    """The shared ``manifest.json`` of a device: the profile of the last main-tree trainer."""
    try:
        return json.loads((device_state(device) / "manifest.json").read_text())
    except (OSError, ValueError):
        return None


def by_key_dir(device: str = "dn2") -> pathlib.Path:
    return device_state(device) / "by-key"


def read_by_key(device: str, key: str) -> dict[str, object] | None:
    """The manifest of the profile trained for exactly KEY, or None."""
    try:
        return json.loads((by_key_dir(device) / f"{key}.json").read_text())
    except (OSError, ValueError):
        return None


def write_json_atomic(path: pathlib.Path, data: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + f".{os.getpid()}.tmp")
    tmp.write_text(json.dumps(data, indent=2) + "\n")
    tmp.replace(path)


def in_linked_worktree() -> bool:
    """True in a linked git worktree (a lane), as native_target decides it."""
    return bool(native_target().IS_WORKTREE)


def save_manifest(
    device: str, manifest: dict[str, object], set_current: bool = False
) -> None:
    """Record a trained profile: always ``by-key/<key>.json``, and ``manifest.json``
    from the main tree or with SET_CURRENT.

    ``manifest.json`` is one file per device that every worktree reads, so a lane
    that trained must not take it over silently. Its profile stays reachable: any
    tree whose current key equals the trained key finds it in by-key (see :func:`resolve`).
    """
    state = device_state(device)
    key = str(manifest["key"])
    write_json_atomic(by_key_dir(device) / f"{key}.json", manifest)
    if set_current or not in_linked_worktree():
        write_json_atomic(state / "manifest.json", manifest)
        print(f"wrote by-key/{key[:16]}.json and manifest.json")
    else:
        print(
            f"linked worktree: wrote by-key/{key[:16]}.json only; manifest.json "
            "is unchanged (--set-current overwrites it)"
        )


def profdata_path(manifest: dict[str, object], device: str = "dn2") -> pathlib.Path:
    """The manifest's profile file, preferring this tree's copy.

    A profile trained in a linked worktree records that tree's absolute path;
    the file itself is in the shared out/ directory, so the same name under
    this tree's state directory is used when it exists.
    """
    path = pathlib.Path(str(manifest["profdata"]["path"]))  # type: ignore[index]
    local = device_state(device) / "profiles" / path.name
    return local if local.is_file() else path


def current_profile(
    device: str = "dn2",
    gen: pathlib.Path | None = None,
    gen2: pathlib.Path | None = None,
) -> str | None:
    """The profile file for this tree (exact by-key match, else the shared manifest's, even
    when stale), or None."""
    manifest = resolve(device, gen, gen2).manifest
    if not manifest:
        return None
    path = profdata_path(manifest, device)
    return str(path) if path.is_file() else None


def desktop_digest(hasher: FileHasher) -> str:
    """Digest of the desktop crate: recorded for reporting, not part of the key."""
    return tree_digest(hasher, ROOT / DESKTOP_DIR, ROOT, set())[0]


@dataclasses.dataclass
class Resolved:
    """The profile chosen for this tree: its manifest (None when missing) and how well it fits."""

    manifest: dict[str, object] | None
    status: str  # ok, partial, stale or missing
    detail: str
    desktop_changed: bool = False


def resolve(
    device: str = "dn2",
    gen: pathlib.Path | None = None,
    gen2: pathlib.Path | None = None,
    fast: str = "",
    blocktrans: str = "",
) -> Resolved:
    """Choose the profile of DEVICE for this tree.

    1. Compute the current key. The package list comes from the shared manifest
       (no extra metadata call). If ``by-key/<key>.json`` exists and its profile
       file is there, that is an exact match: ``ok``. A key covers every input
       listed in compute_components, so an exact match cannot be stale. It does not
       cover the fixtures' content (only the workload name), the fast-region lists
       or the apps/ crates; a profile trained on other fixtures or lists is at worst
       slightly less tuned, never wrong (LLVM drops counters of changed functions).
    2. Otherwise the shared manifest decides as before: ok, partial (only the
       sources digest differs) or stale. If the package list itself changed (a
       crate added or removed), the computed key matches no by-key file and the
       shared manifest answers; that is safe, it only loses the exact match.

    DEVICE selects the profile (``train --device``); a DT2 profile's key also
    carries the DT2 workload name. For ``both``, GEN is the DN2 core (slot 1) and
    GEN2 the DT2 core (slot 2); they default to the registered cores.
    """
    manifest = read_manifest(device)
    if manifest is None:
        return Resolved(None, "missing", "no manifest")
    if not toolchain_dir().is_dir():
        profdata = profdata_path(manifest, device)
        if not profdata.is_file():
            return Resolved(None, "missing", f"profile file {profdata} is gone")
        return Resolved(
            manifest, "stale", f"rustup toolchain {TOOLCHAIN} is not installed"
        )
    hasher = FileHasher(device_state(device) / "hash-cache.json")
    if device == BOTH:
        default1, default2 = both_generated()
        gen, gen2 = gen or default1, gen2 or default2
    gen = gen or generated_dir()
    components, _ = compute_components(
        list(manifest["packages"]),  # type: ignore[call-overload]
        gen,
        hasher,
        fast,
        blocktrans,
        gen2 if device == BOTH else None,
    )
    if device == "dt2":
        components["workload"] = DT2_WORKLOAD_NAME
    if device == BOTH:
        components["workload"] = both_workload_name(reset_fixture_present())
    desktop = desktop_digest(hasher)
    hasher.save()
    exact = read_by_key(device, key_of(components))
    if exact is not None and profdata_path(exact, device).is_file():
        return Resolved(
            exact,
            "ok",
            str(profdata_path(exact, device)),
            exact.get("desktop_sha256") != desktop,
        )
    profdata = profdata_path(manifest, device)
    if not profdata.is_file():
        return Resolved(None, "missing", f"profile file {profdata} is gone")
    old = manifest["components"]
    changed = [name for name, value in components.items() if old.get(name) != value]  # type: ignore[attr-defined]
    if changed == ["sources"]:
        # Engine or tool edits only: LLVM skips profile data of changed functions.
        return Resolved(manifest, "partial", "changed: sources")
    if changed:
        return Resolved(manifest, "stale", "changed: " + ", ".join(changed))
    return Resolved(
        manifest, "ok", str(profdata), manifest.get("desktop_sha256") != desktop
    )


def check_state(
    gen: pathlib.Path | None = None,
    fast: str = "",
    blocktrans: str = "",
    device: str = "dn2",
    gen2: pathlib.Path | None = None,
) -> tuple[str, str, bool]:
    """Return (status, detail, desktop_changed); status is ok, partial, stale or missing.

    partial: only the sources digest differs (the profile is still used);
    stale: the rustc or generated core (or any other component) differs.
    See :func:`resolve` for the exact by-key match.
    """
    r = resolve(device, gen, gen2, fast, blocktrans)
    return r.status, r.detail, r.desktop_changed


def check(
    gen: pathlib.Path | None = None,
    fast: str = "",
    blocktrans: str = "",
    device: str = "dn2",
    gen2: pathlib.Path | None = None,
) -> tuple[str, str]:
    """Return (status, detail): status is ok, partial, stale or missing."""
    status, detail, _ = check_state(gen, fast, blocktrans, device, gen2)
    return status, detail


def profile_use_flags(profdata: str) -> str:
    # Not part of the profile key: it only silences partial-mismatch warnings.
    return (
        FLAGS_TEMPLATE.format(profdata=profdata) + " -Cllvm-args=-no-pgo-warn-mismatch"
    )


@dataclasses.dataclass
class LaunchPlan:
    prefix: list[str]
    cargo_args: list[str]
    env: dict[str, str]


def pgo_state(
    environ: dict[str, str] | os._Environ[str] | None = None,
    device: str = "dn2",
) -> tuple[str, str]:
    """The launcher's view of the profile: (status, detail).

    Status is off (opted out or unsupported host, stay silent), skipped
    (RUSTFLAGS is set), or what :func:`check` returns: ok, stale, missing.
    """
    environ = os.environ if environ is None else environ
    if environ.get("DIGI_EMU_PGO") == "0" or not supported_host():
        return "off", ""
    if environ.get("RUSTFLAGS") or environ.get("CARGO_ENCODED_RUSTFLAGS"):
        return "skipped", "RUSTFLAGS is set"
    return check(
        generated_dir(environ),
        environ.get("SHARC_FAST_REGIONS", ""),
        environ.get("SHARC_BLOCKTRANS", ""),
        device,
        second_generated(environ) if device == BOTH else None,
    )


def pgo_variant(environ: dict[str, str] | os._Environ[str] | None = None) -> str:
    """Target-dir variant of the launcher's profile-use build.

    DIGI_EMU_PGO_VARIANT builds somewhere else (a scratch copy for a
    measurement); the default ``pgo-use`` belongs to the launcher.
    """
    environ = os.environ if environ is None else environ
    return environ.get("DIGI_EMU_PGO_VARIANT") or "pgo-use"


def plan_for(
    profdata: str, environ: dict[str, str] | os._Environ[str] | None = None
) -> LaunchPlan:
    environ = os.environ if environ is None else environ
    return LaunchPlan(
        prefix=["rustup", "run", TOOLCHAIN],
        cargo_args=[
            "--target",
            TRIPLE,
            "--target-dir",
            str(
                native_target().target_dir(
                    generated_dir(environ),
                    pgo_variant(environ),
                    second_generated(environ),
                )
            ),
        ],
        env={"RUSTFLAGS": profile_use_flags(profdata), "RUSTC": toolchain_rustc()},
    )


def train_hint(device: str = "dn2") -> str:
    return (
        f"uv run python -m tools.emulator.native_pgo train --device {device}"
        if device in ("dt2", BOTH)
        else "uv run python -m tools.emulator.native_pgo train"
    )


def profile_for_tests(
    device: str,
    gen: pathlib.Path,
    environ: dict[str, str] | os._Environ[str] | None = None,
) -> tuple[str | None, str, str]:
    """(profile or None, state, message) for a test build of DEVICE's core GEN.

    state is ok, partial, stale, missing or off. It reads and reports nothing else; see report_profile.
    """
    environ = os.environ if environ is None else environ
    if environ.get("DIGI_EMU_PGO") == "0" or not supported_host():
        return None, "off", ""
    r = resolve(
        device,
        gen,
        None,
        environ.get("SHARC_FAST_REGIONS", ""),
        environ.get("SHARC_BLOCKTRANS", ""),
    )
    status, detail = r.status, r.detail
    if status in ("ok", "partial"):
        path = profdata_path(r.manifest, device) if r.manifest else None  # type: ignore[arg-type]
        profile = str(path) if path and path.is_file() else None
        return profile, status, PARTIAL_NOTICE if status == "partial" else ""
    return (
        None,
        status,
        f"WARNING: native PGO: profile {status} ({detail}). Retrain: {train_hint(device)}",
    )


def report_profile(
    device: str,
    state: str,
    message: str,
    applied: bool,
    environ: dict[str, str] | os._Environ[str] | None = None,
) -> None:
    """One loud stderr line for a stale or missing profile. DIGI_REQUIRE_PGO=1 makes it fatal."""
    environ = os.environ if environ is None else environ
    if state in ("stale", "missing"):
        suffix = (
            "; using the stale profile"
            if applied
            else "; building without PGO (about 30% slower DSP)"
        )
        print(message + suffix, file=sys.stderr, flush=True)
        if environ.get("DIGI_REQUIRE_PGO") == "1":
            raise SystemExit(
                f"DIGI_REQUIRE_PGO=1: the PGO profile for {device} is {state}"
            )
    elif state == "partial":
        print(message, file=sys.stderr, flush=True)


def fallback_notice(status: str, detail: str, device: str = "dn2") -> str:
    if status == "skipped":
        return f"WARNING: native PGO: skipped ({detail}); building without PGO"
    what = detail if status == "failed" else f"profile {status} ({detail})"
    hint = (
        f"({'mise run perf:pgo' if device == 'dn2' else train_hint(device)} to train)"
        if status == "missing"
        else f"Run {train_hint(device)}"
    )
    return (
        f"WARNING: native PGO: {what}; building without PGO "
        f"(about 30% slower DSP). {hint}"
    )


def stale_notice(detail: str = "", device: str = "dn2") -> str:
    """The one line printed when a stale profile is not used."""
    return (
        "WARNING: native PGO: profile is stale for this core; building without PGO "
        "(about 30% slower DSP). Retrain with "
        "`mise run perf:pgo` (all three), `mise run perf:pgo:dn2`, `mise run perf:pgo:dt2` or "
        "`mise run perf:pgo:both` (the two-core desktop build; "
        "`uv run python -m tools.emulator.native_pgo train --device both`)."
    )


PARTIAL_NOTICE = (
    "PGO profile partly stale (sources changed since training); using it. "
    "Retrain at milestones with `mise run perf:pgo`."
)


def training_blocker(
    environ: dict[str, str] | os._Environ[str] | None = None, device: str = "dn2"
) -> str:
    """Why `train` cannot run here ("" when it can)."""
    try:
        if device == BOTH:
            gen1, gen2 = both_generated(environ)
            digi_env.check_generated(gen1, environ)
            digi_env.check_generated(gen2, environ)
            for fixture_device in digi_env.SLOT_DEVICES:
                load_fixture(fixture_device)
        elif device == "dt2":
            digi_env.check_generated(dt2_generated(), environ)
        else:
            require_inputs(generated_dir(environ))
    except SystemExit as exc:
        return str(exc)
    return ""


def launch_plan(
    environ: dict[str, str] | os._Environ[str] | None = None,
    device: str = "dn2",
) -> tuple[LaunchPlan | None, str]:
    """Decide whether the launcher can use the PGO build; returns (plan, notice).

    A stale, missing or unusable profile gives (None, notice): the build runs
    without PGO. A partly stale profile (only the sources digest differs) is
    used, with the PARTIAL_NOTICE line. An empty notice means stay silent
    (opted out or unsupported host).
    """
    environ = os.environ if environ is None else environ
    status, detail = pgo_state(environ, device)
    if status == "off":
        return None, ""
    if status == "stale":
        return None, stale_notice(detail, device)
    if status == "partial":
        profile = current_profile(
            device,
            generated_dir(environ),
            second_generated(environ) if device == BOTH else None,
        )
        if profile:
            return plan_for(profile, environ), PARTIAL_NOTICE
        return None, fallback_notice("missing", "profile file is gone", device)
    if status != "ok":
        return None, fallback_notice(status, detail, device)
    return plan_for(detail, environ), "native PGO: using profile " + pathlib.Path(
        detail
    ).name


# --- crate-hash guard ------------------------------------------------------------

# PGO matches functions by mangled name, and a v0 name embeds the crate
# disambiguator (`Cs<hash>_<len><name>`). A profile trained from a build whose
# crate hashes differ from the app's silently applies to nothing.
GUARDED_CRATES = (
    "sharc_native",
    "sharc_gen",
    "sharc_rt",
    "elektron_native_boot",
    "sharc_native_2",
    "sharc_gen_2",
)
CRATE_NAME_RE = re.compile(
    r"C[sc]([0-9A-Za-z]+)_("
    + "|".join(f"{len(name)}{name}" for name in GUARDED_CRATES)
    + r")"
)


def crates_in(text: str) -> dict[str, set[str]]:
    """Crate name -> disambiguators seen in v0-mangled names in `text`."""
    found: dict[str, set[str]] = {}
    for match in CRATE_NAME_RE.finditer(text):
        name = match.group(2).lstrip("0123456789")
        found.setdefault(name, set()).add(match.group(1))
    return found


def profile_crates(profdata: str | os.PathLike[str]) -> dict[str, set[str]]:
    """Guarded crate disambiguators in a profile (cached as <profdata>.crates.json)."""
    profdata = pathlib.Path(profdata)
    cache = profdata.with_name(profdata.name + ".crates.json")
    try:
        if cache.stat().st_mtime >= profdata.stat().st_mtime:
            return {k: set(v) for k, v in json.loads(cache.read_text()).items()}
    except (OSError, ValueError):
        pass
    shown = subprocess.run(
        [str(llvm_profdata()), "show", "--all-functions", str(profdata)],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    found = crates_in(shown)
    with contextlib.suppress(OSError):
        cache.write_text(json.dumps({k: sorted(v) for k, v in found.items()}))
    return found


def binary_crates(path: str | os.PathLike[str]) -> dict[str, set[str]]:
    """Guarded crate disambiguators in a binary's symbols (via nm)."""
    shown = subprocess.run(
        ["nm", str(path)], check=True, capture_output=True, text=True
    ).stdout
    return crates_in(shown)


# A DT2 profile is trained on coupled-run, not on the desktop app, so the
# app's own boot crate never matches it; the engine crates carry the DSP cost.
# The two-core profile is trained on coupled-run too.
DT2_APP_UNPROFILED = ("elektron_native_boot",)


def crate_mismatches(
    profile: dict[str, set[str]],
    binary: dict[str, set[str]],
    ignore: tuple[str, ...] = (),
) -> list[str]:
    """One "MISMATCH crate X: ..." line per crate whose hashes do not overlap."""
    lines = []
    for name in GUARDED_CRATES:
        if name in ignore:
            continue
        want, have = profile.get(name, set()), binary.get(name, set())
        if want and have and not want & have:
            lines.append(
                f"MISMATCH crate {name}: profile {sorted(want)} binary {sorted(have)}"
            )
    return lines


def pgo_binary(
    environ: dict[str, str] | os._Environ[str] | None = None,
) -> pathlib.Path:
    """The app binary the launcher's PGO build produces (a two-core build has its own target dir)."""
    target = native_target().target_dir(
        generated_dir(environ), pgo_variant(environ), second_generated(environ)
    )
    return pathlib.Path(target) / TRIPLE / "release" / "digiemu-desktop"


def verify_binary(
    binary: pathlib.Path, profdata: str | os.PathLike[str], device: str = "dn2"
) -> list[str]:
    """Mismatch lines for `binary` against `profdata` (empty when they match).

    For DT2 and the two-core build the app's boot crate is expected to differ
    (see DT2_APP_UNPROFILED).
    """
    return crate_mismatches(
        profile_crates(profdata),
        binary_crates(binary),
        DT2_APP_UNPROFILED if device in ("dt2", BOTH) else (),
    )


# --- training and benchmarking -------------------------------------------------


def toolchain_rustc() -> str:
    """The rustup toolchain's rustc, for RUSTC in every build.

    Cargo otherwise takes `rustc` from PATH. A Homebrew rustc of the same
    version (`1.98.1 (...) (Homebrew)`) mangles every crate differently, so a
    profile trained with one compiler never matches a build made with the
    other, and PGO silently does nothing.
    """
    rustc = digi_env.toolchain_rustc()
    if not rustc:
        raise SystemExit(
            f"rustup toolchain {TOOLCHAIN} not found (rustup toolchain install "
            f"{TOOLCHAIN})"
        )
    return rustc


def base_env(gen: pathlib.Path, gen2: pathlib.Path | None = None) -> dict[str, str]:
    env = {
        k: v
        for k, v in os.environ.items()
        if k not in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS")
    }
    env["SHARC_GEN_DIR"] = str(gen)
    if gen2 is not None:
        env["SHARC_GEN_DIR_2"] = str(gen2)
    env["RUSTC"] = toolchain_rustc()
    return env


def run_env(deps_dir: pathlib.Path) -> dict[str, str]:
    return {
        "DYLD_LIBRARY_PATH": str(deps_dir),
        "NOTE_EVENTS": "trig",
        "DIGI_COUPLED_FIXTURES": os.environ.get("DIGI_COUPLED_FIXTURES", str(READY)),
        "DIGI_COUPLED_SYX": str(digi_env.firmware()),
        "DN2_PROFILE_LINK": "1",
        "SHARC_FAST_FALLBACK_REGIONS": fallback_regions(),
    }


def build_tests(
    target_dir: pathlib.Path,
    rustflags: str | None,
    gen: pathlib.Path,
    log: pathlib.Path,
    app: bool = False,
) -> pathlib.Path:
    """Build the workload test binary, or with app=True the desktop app binary."""
    from tools.emulator import native_emu

    # the desktop crate embeds the panel's build output
    native_emu.build_panel(os.environ)
    env = base_env(gen)
    if rustflags:
        env["RUSTFLAGS"] = rustflags
    command = [
        "rustup",
        "run",
        TOOLCHAIN,
        "cargo",
        *(["build", "--bin", "digiemu-desktop"] if app else ["test", "--no-run"]),
        "--release",
        "--locked",
        "--manifest-path",
        MANIFEST_PATH,
        "--features",
        FEATURES,
        # RUSTFLAGS builds use --target so the flags skip build scripts; the
        # plain build has none, and shares its key dir with the desktop app.
        *(["--target", TRIPLE] if rustflags else []),
        "--target-dir",
        str(target_dir),
        "--message-format=json",
    ]
    log.parent.mkdir(parents=True, exist_ok=True)
    with benchlock("shared", "pgo cargo build"), log.open("w") as handle:
        proc = subprocess.run(
            command, cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=handle, text=True
        )
    if proc.returncode:
        tail = "".join(log.read_text().splitlines(keepends=True)[-30:])
        raise SystemExit(f"test build failed (see {log}):\n{tail}")
    found = []
    for line in proc.stdout.splitlines():
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if (
            message.get("reason") == "compiler-artifact"
            and message.get("executable")
            and bool(message.get("profile", {}).get("test")) != app
            and (not app or "bin" in message.get("target", {}).get("kind", []))
            and "digiemu-desktop" in message.get("package_id", "")
        ):
            found.append(pathlib.Path(message["executable"]))
    if len(found) != 1:
        raise SystemExit(
            f"expected one digiemu-desktop {'app' if app else 'test'} binary, found {found}"
        )
    return found[0]


@dataclasses.dataclass
class Result:
    elapsed: float
    dsp_ns: int
    frames: int
    user: float
    sys: float
    real: float
    line: str


def run_workload(
    binary: pathlib.Path,
    log: pathlib.Path,
    extra_env: dict[str, str] | None = None,
    app: bool = False,
) -> Result:
    """Run the workload under /usr/bin/time and enforce the gates."""
    env_pairs = {**run_env(binary.parent), **(extra_env or {})}
    env_pairs["SHARC_FAST_FALLBACK_REGIONS"] = fallback_regions(
        {**os.environ, **(extra_env or {})}
    )
    command = [
        "/usr/bin/time",
        "-lp",
        "env",
        *(f"{k}={v}" for k, v in env_pairs.items()),
        str(binary),
        *(
            ["--pgo-train"]
            if app
            else [TEST_NAME, "--ignored", "--nocapture", "--test-threads=1"]
        ),
    ]
    proc = subprocess.run(
        command, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True
    )
    log.write_text(proc.stdout)
    if proc.returncode:
        raise SystemExit(
            f"workload failed (exit {proc.returncode}), see {log}:\n"
            + "\n".join(proc.stdout.splitlines()[-25:])
        )
    text = proc.stdout

    def grab(pattern: str) -> str:
        found = re.search(pattern, text)
        if not found:
            raise SystemExit(f"gate: {pattern!r} not found in {log}")
        return found.group(1)

    result = Result(
        elapsed=float(grab(r"coupled_workload_elapsed_seconds=([0-9.]+)")),
        dsp_ns=int(grab(r"worker_dsp_ns: (\d+)")),
        frames=int(grab(r"frames_completed: (\d+)")),
        user=float(grab(r"(?m)^user\s+([0-9.]+)")),
        sys=float(grab(r"(?m)^sys\s+([0-9.]+)")),
        real=float(grab(r"(?m)^real\s+([0-9.]+)")),
        line=next(
            line
            for line in text.splitlines()
            if line.startswith("native_audio_link_timing_window=")
        ),
    )
    if result.frames != EXPECTED_FRAMES:
        raise SystemExit(
            f"gate: frames_completed {result.frames} != {EXPECTED_FRAMES} in {log}"
        )
    return result


def require_inputs(gen: pathlib.Path) -> None:
    if not supported_host():
        raise SystemExit("native PGO is only set up for aarch64 macOS")
    if not toolchain_dir().is_dir():
        raise SystemExit(f"missing rustup toolchain {toolchain_dir()}")
    if not gen.is_dir():
        raise SystemExit(f"generated DN2 core not found: {gen} (set SHARC_GEN_DIR)")
    digi_env.check_generated(gen)
    for path in (
        run_env(pathlib.Path("."))["DIGI_COUPLED_FIXTURES"],
        run_env(pathlib.Path("."))["DIGI_COUPLED_SYX"],
    ):
        if not pathlib.Path(path).exists():
            raise SystemExit(f"missing private input {path}")


def fresh_raw_dir(target: pathlib.Path) -> pathlib.Path:
    """An empty directory for the training's .profraw files, inside the instrumented target dir.

    Its path is part of the instrumented build's RUSTFLAGS (``-Cprofile-generate=``), and
    cargo rebuilds every crate, the 80 MB generated core included, when the flags change.
    A path named by the profile key changed with every source edit; this one is stable per
    target dir (a worktree has its own), so only the edited crates rebuild. It is cleared at
    the start of each training so files of an earlier one never mix in.
    """
    raw = target / "profraw"
    shutil.rmtree(raw, ignore_errors=True)
    raw.mkdir(parents=True)
    return raw


def cmd_train(args: argparse.Namespace) -> None:
    gen = generated_dir()
    require_inputs(gen)
    state = state_dir()
    timings: dict[str, float] = {}
    started = time.monotonic()

    def phase(name: str, since: float) -> float:
        now = time.monotonic()
        timings[name] = round(now - since, 2)
        print(f"phase {name}: {timings[name]} s", flush=True)
        return now

    t = time.monotonic()
    packages = local_package_dirs()
    hasher = FileHasher(state / "hash-cache.json")
    fast = fast_regions()
    blocktrans = blocktrans_roots()
    components, nfiles = compute_components(packages, gen, hasher, fast, blocktrans)
    desktop = desktop_digest(hasher)
    hasher.save()
    key = key_of(components)
    t = phase("key", t)

    target = native_target().target_dir(gen, "pgo-instrument")
    raw = fresh_raw_dir(target)
    binary = build_tests(
        target,
        f"-Cprofile-generate={raw}",
        gen,
        state / "instrument-build.log",
        app=True,
    )
    t = phase("instrumented-build", t)

    gates = []
    for number in range(1, args.runs + 1):
        with benchlock("shared", "pgo train"):
            result = run_workload(
                binary,
                state / f"train-{number}.log",
                {
                    "LLVM_PROFILE_FILE": str(raw / f"train-{number}-%m.profraw"),
                    "SHARC_FAST_REGIONS": fast,
                    "SHARC_FAST_FALLBACK_REGIONS": fallback_regions(),
                    "SHARC_BLOCKTRANS": blocktrans,
                },
                app=True,
            )
        gates.append(result.line)
        print(
            f"train run {number}: elapsed {result.elapsed:.3f} s frames {result.frames}",
            flush=True,
        )
    t = phase("training-runs", t)

    merged = raw / "merged.profdata"
    raws = sorted(str(p) for p in raw.glob("*.profraw"))
    if not raws:
        raise SystemExit("training produced no .profraw files")
    merge = [str(llvm_profdata()), "merge", "-o", str(merged), *raws]
    subprocess.run(merge, check=True)
    digest = sha256_file(merged)
    profiles = state / "profiles"
    profiles.mkdir(parents=True, exist_ok=True)
    final = profiles / f"{digest}.profdata"
    shutil.move(str(merged), final)
    shutil.rmtree(raw, ignore_errors=True)
    t = phase("merge", t)

    manifest = {
        "key": key,
        "components": components,
        "packages": packages,
        "desktop_sha256": desktop,
        "generated_dir": str(gen),
        "fallback_regions": fallback_regions(),
        "fast_regions": fast,
        "blocktrans": blocktrans,
        "file_count": nfiles,
        "rustc_vV": rustc_version(),
        "sysroot": str(toolchain_dir()),
        "triple": TRIPLE,
        "features": FEATURES,
        "profdata": {
            "path": str(final),
            "sha256": digest,
            "size": final.stat().st_size,
        },
        "train_commands": {
            "build_rustflags": f"-Cprofile-generate={raw}",
            "runs": args.runs,
            "merge": merge[:4] + ["<raw>..."],
            "workload": f"{WORKLOAD} --pgo-train",
        },
        "gate_lines": gates,
        "phase_seconds": timings,
        "created": datetime.datetime.now(datetime.UTC).isoformat(),
    }
    save_manifest("dn2", manifest, args.set_current)
    print(
        f"trained {final} ({final.stat().st_size} bytes) key {key[:16]} in {time.monotonic() - started:.1f} s"
    )
    auto_clean()


def cmd_check(_: argparse.Namespace) -> int:
    began = time.monotonic()
    r = resolve("dn2", None, None, fast_regions(), blocktrans_roots())
    status, detail, desktop = r.status, r.detail, r.desktop_changed
    manifest = r.manifest or {}
    fast_changed = status == "ok" and (
        manifest.get("fast_regions", "") != fast_regions()
        or manifest.get("fallback_regions", "") != fallback_regions()
        or manifest.get("blocktrans", "") != blocktrans_roots()
    )
    if status == "ok" and desktop:
        print("ok (desktop crate changed; profile reused)")
    elif fast_changed:
        print(
            "ok (fast regions changed since training; the profile still applies, "
            "retrain at a milestone)"
        )
    elif status == "partial":
        print(f"partly stale ({detail}); the launcher still uses the profile")
    else:
        print(f"{status} {detail}" if status != "missing" else f"missing ({detail})")
    print(f"check took {time.monotonic() - began:.3f} s", file=sys.stderr)
    return 0 if status == "ok" else 1


def cmd_verify(args: argparse.Namespace) -> int:
    environ = os.environ
    if args.device == BOTH:
        gen, gen2 = both_generated()
        environ = {
            **os.environ,
            "SHARC_GEN_DIR": str(gen),
            "SHARC_GEN_DIR_2": str(gen2),
        }
    profile = current_profile(args.device)
    profdata = pathlib.Path(profile) if profile else None
    if profdata is None:
        print("no trained profile to verify against")
        return 1
    binary = pathlib.Path(args.binary) if args.binary else pgo_binary(environ)
    if not binary.is_file():
        print(f"no binary at {binary}")
        return 1
    lines = verify_binary(binary, profdata, args.device)
    print("\n".join(lines) if lines else "ok")
    return 1 if lines else 0


def cmd_plan(args: argparse.Namespace) -> int:
    environ = {
        **os.environ,
        "SHARC_FAST_REGIONS": fast_regions(),
        "SHARC_FAST_FALLBACK_REGIONS": fallback_regions(),
        "SHARC_BLOCKTRANS": blocktrans_roots(),
    }
    if args.device == "dt2" and not environ.get("SHARC_GEN_DIR"):
        environ["SHARC_GEN_DIR"] = str(dt2_generated())
    if args.device == BOTH:
        gen, gen2 = both_generated()
        environ["SHARC_GEN_DIR"], environ["SHARC_GEN_DIR_2"] = str(gen), str(gen2)
    plan, notice = launch_plan(environ, args.device)
    print(notice or "native PGO: not used")
    if plan:
        print(
            json.dumps(
                {"prefix": plan.prefix, "cargo_args": plan.cargo_args, "env": plan.env},
                indent=2,
            )
        )
    return 0 if plan else 1


# --- DT2: instrumented coupled-run on the one-sample card -----------------------

COUPLED_MANIFEST = "packages/emulator/coupled/Cargo.toml"
# (scenario file, scenario names): 3 s trig of the card's sample, then FX.
DT2_WORKLOAD: list[tuple[str, list[str]]] = [
    ("tests/scenarios/dt2.json", ["dt2-sample-trig"]),
    ("tests/regression/scenarios/dt2-trackfx.ts", ["tf-br-127"]),
    (
        "tests/regression/scenarios/dt2-sendfx.ts",
        ["sf-del-ppwidth-u50", "sf-rev-decay-u50", "sf-mst-thr0"],
    ),
]
DT2_WORKLOAD_NAME = "coupled-run-dt2:" + ",".join(
    f"{f}={'+'.join(n)}" for f, n in DT2_WORKLOAD
)


def coupled_module():
    from tools.emulator import coupled as module

    return module


def dt2_generated() -> pathlib.Path:
    """SHARC_GEN_DIR, else the core registered for the DT2 DSP image."""
    from tools.emulator import dt2_sample_check

    _, _, ready, _ = dt2_sample_check.prepare()
    return coupled_module().generated("dt2", ready / "digi-audio-dn2-image.bin")


def build_coupled(
    target_dir: pathlib.Path,
    rustflags: str | None,
    gen: pathlib.Path,
    log: pathlib.Path,
    gen2: pathlib.Path | None = None,
) -> pathlib.Path:
    """Build packages/emulator/coupled's ``coupled-run`` (RUSTFLAGS builds use --target).

    GEN2 (the DT2 core, slot 2) makes it the two-core build.
    """
    env = base_env(gen, gen2)
    if rustflags:
        env["RUSTFLAGS"] = rustflags
    command = [
        "rustup", "run", TOOLCHAIN, "cargo", "build", "--release", "--locked",
        "--manifest-path", COUPLED_MANIFEST,
        "--features", BOTH_FEATURES if gen2 is not None else coupled_module().FEATURES,
        "--bin", coupled_module().BINARY_NAME,
        *(["--target", TRIPLE] if rustflags else []),
        "--target-dir", str(target_dir),
    ]  # fmt: skip
    log.parent.mkdir(parents=True, exist_ok=True)
    with benchlock("shared", "pgo coupled-run build"), log.open("w") as handle:
        done = subprocess.run(command, cwd=ROOT, env=env, stderr=handle)
    if done.returncode:
        tail = "".join(log.read_text().splitlines(keepends=True)[-30:])
        raise SystemExit(f"coupled-run build failed (see {log}):\n{tail}")
    binary = (
        target_dir
        / (TRIPLE if rustflags else "")
        / "release"
        / coupled_module().BINARY_NAME
    )
    if not binary.is_file():
        raise SystemExit(f"build left no binary at {binary}")
    return binary


@dataclasses.dataclass(frozen=True)
class WorkItem:
    """One training process: a coupled-run on one device's inputs and some scenarios of a file."""

    label: str
    device: str
    scenarios_file: str
    names: tuple[str, ...]
    input_args: tuple[str, ...]
    fast_syx: pathlib.Path
    seconds: float | None = None


def ready_args(
    inputs: dict[str, pathlib.Path], card_args: list[str]
) -> tuple[str, ...]:
    """The coupled-run input arguments of a ready state (and its `--card-image`)."""
    return (
        "--syx", str(inputs["firmware"]),
        "--dsp-image", str(inputs["DSP image"]),
        "--dsp-state", str(inputs["DSP state"]),
        "--cf-snapshot", str(inputs["CF snapshot"]),
        *card_args,
    )  # fmt: skip


def dt2_items() -> list[WorkItem]:
    """:data:`DT2_WORKLOAD` on the one-sample card's ready state."""
    from tools.emulator import dt2_sample_check

    inputs, card_args = dt2_sample_check.card_inputs()
    return [
        WorkItem(
            f"dt2-{pathlib.Path(file).stem}",
            "dt2",
            file,
            tuple(names),
            ready_args(inputs, card_args),
            inputs["firmware"],
        )
        for file, names in DT2_WORKLOAD
    ]


def run_items(
    binary: pathlib.Path,
    outdir: pathlib.Path,
    extra_env: dict[str, str],
    items: list[WorkItem],
    slots: bool = False,
) -> float:
    """Run the items once, one process each; seconds. A failed process or gate aborts.

    Each item gets the fast tiers of its firmware (``digi_env.fast_env``, as the launcher
    sets them). SLOTS names the two-core build: the lists then go to the engine slot of
    the item's device.
    """
    started = time.monotonic()
    for item in items:
        report = outdir / f"{item.label}.json"
        argv = [
            str(binary),
            *item.input_args,
            "--scenarios", str(coupled_module().scenario_path(ROOT / item.scenarios_file)),
            *(x for n in item.names for x in ("--scenario", n)),
            *(["--seconds", str(item.seconds)] if item.seconds is not None else []),
            "--out", str(report),
        ]  # fmt: skip
        env = {**os.environ, **extra_env}
        fast = digi_env.fast_env(env, syx=item.fast_syx, context="pgo")
        env.update(digi_env.slot_env(item.device, fast) if slots else fast)
        done = subprocess.run(
            argv,
            cwd=ROOT,
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
        )
        if done.returncode or not report.is_file():
            raise SystemExit(
                f"{item.device.upper()} workload {item.label} failed (exit {done.returncode}):\n"
                + done.stderr[-1500:]
            )
        for sc in json.loads(report.read_text())["scenarios"]:
            if (sc.get("gates") or {}).get("ok") is False:
                raise SystemExit(
                    f"{item.device.upper()} workload gate failed: {sc['name']}"
                )
    return time.monotonic() - started


def run_dt2_workload(
    binary: pathlib.Path, outdir: pathlib.Path, extra_env: dict[str, str]
) -> float:
    """Run :data:`DT2_WORKLOAD` once (one process per scenario file); seconds."""
    return run_items(binary, outdir, extra_env, dt2_items())


def cmd_train_dt2(args: argparse.Namespace) -> None:
    gen = dt2_generated()
    digi_env.check_generated(gen)
    state = state_dir(device="dt2")
    started = time.monotonic()
    packages = local_package_dirs()
    hasher = FileHasher(state / "hash-cache.json")
    components, nfiles = compute_components(packages, gen, hasher)
    components["workload"] = DT2_WORKLOAD_NAME
    hasher.save()
    key = key_of(components)
    variant = args.variant or "pgo-dt2-instrument"
    target = native_target().target_dir(gen, variant)
    raw = fresh_raw_dir(target)
    binary = build_coupled(
        target,
        f"-Cprofile-generate={raw}",
        gen,
        state / "instrument-build.log",
    )
    print(f"instrumented build {time.monotonic() - started:.1f} s", flush=True)
    seconds = []
    for number in range(1, args.runs + 1):
        outdir = state / f"train-{number}"
        outdir.mkdir(exist_ok=True)
        with benchlock("shared", "pgo train dt2"):
            seconds.append(
                run_dt2_workload(
                    binary,
                    outdir,
                    {"LLVM_PROFILE_FILE": str(raw / f"train-{number}-%m.profraw")},
                )
            )
        print(f"train run {number}: {seconds[-1]:.1f} s", flush=True)
    raws = sorted(str(p) for p in raw.glob("*.profraw"))
    if not raws:
        raise SystemExit("training produced no .profraw files")
    merged = raw / "merged.profdata"
    merge = [str(llvm_profdata()), "merge", "-o", str(merged), *raws]
    subprocess.run(merge, check=True)
    digest = sha256_file(merged)
    (state / "profiles").mkdir(parents=True, exist_ok=True)
    final = state / "profiles" / f"{digest}.profdata"
    shutil.move(str(merged), final)
    shutil.rmtree(raw, ignore_errors=True)
    manifest = {
        "key": key,
        "components": components,
        "packages": packages,
        "generated_dir": str(gen),
        "fast_env": {
            key: value
            for key, value in coupled_module()
            .run_env(syx=coupled_module().inputs("dt2")["firmware"])
            .items()
            if key.startswith(("SHARC_FAST_", "SHARC_BLOCKTRANS"))
        },
        "file_count": nfiles,
        "rustc_vV": rustc_version(),
        "triple": TRIPLE,
        "profdata": {
            "path": str(final),
            "sha256": digest,
            "size": final.stat().st_size,
        },
        "train_commands": {
            "build_rustflags": f"-Cprofile-generate={raw}",
            "runs": args.runs,
            "workload": DT2_WORKLOAD,
        },
        "run_seconds": seconds,
        "created": datetime.datetime.now(datetime.UTC).isoformat(),
    }
    save_manifest("dt2", manifest, args.set_current)
    print(
        f"trained {final} ({final.stat().st_size} bytes) key {key[:16]} "
        f"in {time.monotonic() - started:.1f} s"
    )
    auto_clean()


def cmd_check_dt2() -> int:
    r = resolve("dt2", dt2_generated())
    if r.status == "missing":
        print(f"missing ({r.detail})")
        return 1
    if r.status == "ok":
        print(f"ok {r.detail}")
        return 0
    print(f"stale {r.detail}" if r.status == "stale" else f"{r.status} {r.detail}")
    return 1


# --- both: the two-core build, instrumented coupled-run -------------------------

# The DN2 half of the workload is the `coupled_ready_exactness` workload as a coupled-run
# scenario (QA prelude, Trig 1 down at +60M instructions, up at +100M, ended at +100M).
DN2_WORKLOAD: list[tuple[str, list[str]]] = [
    ("tests/scenarios/dn2.json", ["exactness-100m"]),
]
# One runtime replay per device (scenario file, scenario, seconds or None for the scenario's own).
# Instrumented runs are 2 to 3 times slower than plain ones: keep them short.
REPLAYS: dict[str, tuple[str, str, float | None]] = {
    "dn2": ("tests/scenarios/dn2.json", "trig1-hold", 5.0),
    "dt2": ("tests/scenarios/dt2.json", "dt2-sample-trig", None),
}
FIXTURE_ROOT = ROOT / "out" / "fixtures" / "runtime"
FIXTURE_FILES = ("firmware", "dsp_image", "dsp_state", "cf_snapshot", "card")


@dataclasses.dataclass(frozen=True)
class Fixture:
    """A runtime replay fixture (out/fixtures/runtime/<device>/, made by `mise run perf:fixture`)."""

    device: str
    directory: pathlib.Path
    files: dict[str, pathlib.Path | None]
    provenance: dict[str, object]

    def record(self) -> dict[str, object]:
        """What the profile manifest keeps of the fixture."""
        return {
            "device": self.device,
            "directory": str(self.directory),
            "firmware_version": self.provenance.get("firmware_version"),
            "patch_set": self.provenance.get("patch_set"),
            "sha256": self.provenance["sha256"],
            "source_head": self.provenance.get("source_head"),
            "created": self.provenance.get("created"),
        }

    def input_args(self) -> tuple[str, ...]:
        paths = self.files
        return (
            "--syx", str(paths["firmware"]),
            "--dsp-image", str(paths["dsp_image"]),
            "--dsp-state", str(paths["dsp_state"]),
            "--cf-snapshot", str(paths["cf_snapshot"]),
            *(("--card-image", str(paths["card"])) if paths["card"] else ()),
        )  # fmt: skip


def load_fixture(device: str, directory: pathlib.Path | str | None = None) -> Fixture:
    """Read a runtime fixture and check every file against the SHA-256 of its provenance.json."""
    directory = pathlib.Path(directory) if directory else FIXTURE_ROOT / device
    hint = f"`mise run perf:fixture -- --device {device}`"
    try:
        provenance = json.loads((directory / "provenance.json").read_text())
    except (OSError, ValueError) as exc:
        raise SystemExit(
            f"no runtime fixture for {device.upper()} at {directory} ({exc}); make it with {hint}"
        ) from exc
    if provenance.get("schema") != 1 or provenance.get("device") != device:
        raise SystemExit(
            f"{directory}/provenance.json is not a schema 1 fixture of {device.upper()}; remake it with {hint}"
        )
    files: dict[str, pathlib.Path | None] = {}
    for key in FIXTURE_FILES:
        name = provenance["files"].get(key)
        if name is None and key == "card":
            files[key] = None
            continue
        path = directory / str(name)
        if not name or not path.is_file():
            raise SystemExit(
                f"fixture file {key} is missing in {directory}; remake it with {hint}"
            )
        if sha256_file(path) != provenance["sha256"].get(key):
            raise SystemExit(
                f"fixture file {path} does not match its provenance sha256; remake it with {hint}"
            )
        files[key] = path
    return Fixture(device, directory, files, provenance)


RESET_FIXTURE_DIR = FIXTURE_ROOT / "dn2-reset"


def reset_fixture_present(directory: pathlib.Path | None = None) -> bool:
    """Is there a DN2 reset-state fixture (made by `runtime_fixture.ts --state reset`)? Cheap: no hashing."""
    try:
        provenance = json.loads(
            ((directory or RESET_FIXTURE_DIR) / "provenance.json").read_text()
        )
    except (OSError, ValueError):
        return False
    return provenance.get("state") == "reset"


def load_reset_fixture(directory: pathlib.Path | None = None) -> Fixture | None:
    """The DN2 reset-state fixture (the state after a guest soft reset, which users run), or None with one notice line.

    A fixture that is present but does not match its provenance is an error (the same as the other fixtures),
    so the workload identity never depends on a silently skipped input.
    """
    directory = directory or RESET_FIXTURE_DIR
    if not reset_fixture_present(directory):
        print(
            f"no DN2 reset fixture at {directory}; training without it "
            "(`node tools/devtools/runtime_fixture.ts --device dn2 --state reset`)"
        )
        return None
    return load_fixture("dn2", directory)


def both_workload_name(with_reset: bool) -> str:
    """The workload identity of the two-core profile; it names the reset input when that is trained."""
    return BOTH_WORKLOAD_NAME + ("+dn2-reset" if with_reset else "")


def both_items(
    fixtures: dict[str, Fixture], reset: Fixture | None = None
) -> list[WorkItem]:
    """The training workload of the two-core build: DN2 items, then DT2 items, then the replays (and the DN2 reset replay)."""
    inputs = digi_env.ready_inputs(device="dn2")
    missing = [f"{k}: {v}" for k, v in inputs.items() if not pathlib.Path(v).exists()]
    if missing:
        raise SystemExit("missing DN2 ready inputs:\n  " + "\n  ".join(missing))
    items = [
        WorkItem(
            f"dn2-{pathlib.Path(file).stem}",
            "dn2",
            file,
            tuple(names),
            ready_args(inputs, []),
            inputs["firmware"],
        )
        for file, names in DN2_WORKLOAD
    ]
    items += dt2_items()
    for device in digi_env.SLOT_DEVICES:
        file, name, seconds = REPLAYS[device]
        fixture = fixtures[device]
        firmware = fixture.files["firmware"]
        assert firmware is not None
        items.append(
            WorkItem(
                f"{device}-replay-{name}",
                device,
                file,
                (name,),
                fixture.input_args(),
                # The tier tables are keyed by the stock firmware.
                digi_env.base_firmware(firmware),
                seconds,
            )
        )
    if reset is not None:
        file, name, seconds = REPLAYS["dn2"]
        firmware = reset.files["firmware"]
        assert firmware is not None
        items.append(
            WorkItem(
                f"dn2-reset-replay-{name}",
                "dn2",
                file,
                (name,),
                reset.input_args(),
                digi_env.base_firmware(firmware),
                seconds,
            )
        )
    return items


def both_workload_text(items: list[WorkItem]) -> list[str]:
    return [f"{i.label}:{i.scenarios_file}={'+'.join(i.names)}" for i in items]


def cmd_train_both(args: argparse.Namespace) -> None:
    gen, gen2 = both_generated()
    digi_env.check_generated(gen)
    digi_env.check_generated(gen2)
    fixtures = {
        "dn2": load_fixture("dn2", args.fixture_dn2),
        "dt2": load_fixture("dt2", args.fixture_dt2),
    }
    reset = load_reset_fixture()
    items = both_items(fixtures, reset)
    state = device_state(BOTH)
    started = time.monotonic()
    packages = local_package_dirs(BOTH_FEATURES)
    hasher = FileHasher(state / "hash-cache.json")
    components, nfiles = compute_components(packages, gen, hasher, gen2=gen2)
    components["workload"] = both_workload_name(reset is not None)
    hasher.save()
    key = key_of(components)
    variant = args.variant or "pgo-both-instrument"
    target = native_target().target_dir(gen, variant, gen2)
    raw = fresh_raw_dir(target)
    binary = build_coupled(
        target,
        f"-Cprofile-generate={raw}",
        gen,
        state / "instrument-build.log",
        gen2,
    )
    print(f"instrumented build {time.monotonic() - started:.1f} s", flush=True)
    seconds = []
    for number in range(1, args.runs + 1):
        outdir = state / f"train-{number}"
        outdir.mkdir(exist_ok=True)
        with benchlock("shared", "pgo train both"):
            seconds.append(
                run_items(
                    binary,
                    outdir,
                    {"LLVM_PROFILE_FILE": str(raw / f"train-{number}-%m.profraw")},
                    items,
                    slots=True,
                )
            )
        print(f"train run {number}: {seconds[-1]:.1f} s", flush=True)
    raws = sorted(str(p) for p in raw.glob("*.profraw"))
    if not raws:
        raise SystemExit("training produced no .profraw files")
    merged = raw / "merged.profdata"
    merge = [str(llvm_profdata()), "merge", "-o", str(merged), *raws]
    subprocess.run(merge, check=True)
    digest = sha256_file(merged)
    (state / "profiles").mkdir(parents=True, exist_ok=True)
    final = state / "profiles" / f"{digest}.profdata"
    shutil.move(str(merged), final)
    shutil.rmtree(raw, ignore_errors=True)
    manifest = {
        "key": key,
        "components": components,
        "packages": packages,
        # Slot order: slot 1 (SHARC_GEN_DIR) is the DN2 core, slot 2 (SHARC_GEN_DIR_2) the DT2 core.
        "generated_dir": str(gen),
        "generated_dir_2": str(gen2),
        "fast_env": {
            item.label: {
                k: v
                for k, v in digi_env.slot_env(
                    item.device, digi_env.fast_env(syx=item.fast_syx, context="pgo")
                ).items()
                if k.startswith(("SHARC_FAST_", "SHARC_BLOCKTRANS"))
            }
            for item in items
        },
        "fixtures": {
            **{device: fx.record() for device, fx in fixtures.items()},
            **({"dn2-reset": reset.record()} if reset is not None else {}),
        },
        "file_count": nfiles,
        "rustc_vV": rustc_version(),
        "triple": TRIPLE,
        "features": BOTH_FEATURES,
        "profdata": {
            "path": str(final),
            "sha256": digest,
            "size": final.stat().st_size,
        },
        "train_commands": {
            "build_rustflags": f"-Cprofile-generate={raw}",
            "runs": args.runs,
            "workload": both_workload_text(items),
        },
        "run_seconds": seconds,
        "created": datetime.datetime.now(datetime.UTC).isoformat(),
    }
    save_manifest(BOTH, manifest, args.set_current)
    print(
        f"trained {final} ({final.stat().st_size} bytes) key {key[:16]} "
        f"in {time.monotonic() - started:.1f} s"
    )
    auto_clean()


def cmd_check_both() -> int:
    gen, gen2 = both_generated()
    status, detail, _ = check_state(gen, "", "", BOTH, gen2)
    if status == "partial":
        print(f"partly stale ({detail}); the launcher still uses the profile")
    else:
        print(f"{status} {detail}")
    return 0 if status == "ok" else 1


def cmd_coupled_build(args: argparse.Namespace) -> None:
    """Build ``coupled-run`` for a device's core (``both``: the two-core build), plain or with a profile."""
    gen2 = None
    if args.device == BOTH:
        gen, gen2 = both_generated()
        digi_env.check_generated(gen2)
    else:
        gen = (
            dt2_generated()
            if args.device == "dt2"
            else coupled_module().generated("dn2")
        )
    digi_env.check_generated(gen)
    flags = None
    profile = args.profile
    if profile == "current":
        profile = current_profile(args.device, gen, gen2)
        if profile is None:
            raise SystemExit(f"no {args.device} profile")
    if profile:
        profile = str(pathlib.Path(profile).resolve())
        flags = profile_use_flags(profile)
    target = native_target().target_dir(
        gen, args.variant or ("pgo-cr-" + args.device if flags else ""), gen2
    )
    binary = build_coupled(target, flags, gen, target / "build.log", gen2)
    print(binary)
    if profile:
        lines = crate_mismatches(profile_crates(profile), binary_crates(binary))
        print("\n".join(lines) if lines else "crate hashes: ok")


def median(values: list[float]) -> float:
    return statistics.median(values)


def cmd_bench(args: argparse.Namespace) -> None:
    gen = generated_dir()
    require_inputs(gen)
    fast = fast_regions()
    blocktrans = blocktrans_roots()
    status, detail = check(gen, fast, blocktrans)
    if status != "ok":
        raise SystemExit(f"profile is not current: {status} {detail}; run train first")
    state = state_dir()
    out = state / ("bench-" + datetime.datetime.now().strftime("%Y%m%d-%H%M%S"))
    out.mkdir(parents=True)
    t = time.monotonic()
    control_src = build_tests(
        native_target().target_dir(gen), None, gen, out / "control-build.log"
    )
    print(f"control build {time.monotonic() - t:.1f} s", flush=True)
    t = time.monotonic()
    use_src = build_tests(
        native_target().target_dir(gen, "pgo-use"),
        profile_use_flags(detail),
        gen,
        out / "use-build.log",
    )
    print(f"profile-use build {time.monotonic() - t:.1f} s", flush=True)
    binaries = {}
    hashes = {}
    for label, src in (("control", control_src), ("use", use_src)):
        dst = out / f"{label}-{src.name}"
        shutil.copy2(src, dst)
        binaries[label] = (dst, src.parent)
        hashes[label] = sha256_file(dst)
        print(f"{label} binary sha256 {hashes[label]}")
    paired_bench(
        out,
        binaries,
        "control",
        "use",
        args.pairs,
        {
            "binary_sha256": hashes,
            "profdata": detail,
            "fallback_regions": fallback_regions(),
            "fast_regions": fast,
            "blocktrans": blocktrans,
        },
        {
            "SHARC_FAST_REGIONS": fast,
            "SHARC_FAST_FALLBACK_REGIONS": fallback_regions(),
            "SHARC_BLOCKTRANS": blocktrans,
        },
    )


def _label_env(
    env: dict[str, str] | dict[str, dict[str, str]] | None, label: str
) -> dict[str, str]:
    """Extra env for one label: either shared, or a {label: env} map."""
    if not env:
        return {}
    if label in env and isinstance(env[label], dict):
        return env[label]  # type: ignore[return-value]
    if all(isinstance(v, dict) for v in env.values()):
        return {}
    return env  # type: ignore[return-value]


def paired_bench(
    out: pathlib.Path,
    binaries: dict[str, tuple[pathlib.Path, pathlib.Path]],
    label_a: str,
    label_b: str,
    pairs: int,
    extra: dict[str, object],
    env: dict[str, str] | dict[str, dict[str, str]] | None = None,
) -> None:
    """Alternate A,B / B,A runs of the workload; write bench.tsv, summary.json."""
    rows = []
    with benchlock("exclusive", "pgo bench"):
        for pair in range(1, pairs + 1):
            order = [label_a, label_b] if pair % 2 else [label_b, label_a]
            results = {}
            for label in order:
                dst, deps = binaries[label]
                results[label] = run_workload(
                    dst,
                    out / f"run-{pair:02d}-{label}.log",
                    {
                        "DYLD_LIBRARY_PATH": str(deps),
                        **_label_env(env, label),
                    },
                )
            for label in (label_a, label_b):
                r = results[label]
                rows.append(
                    (pair, label, r.elapsed, r.dsp_ns, r.frames, r.real, r.user, r.sys)
                )
            print(
                f"pair {pair} order {','.join(order)}: ratio wall {results[label_b].elapsed / results[label_a].elapsed:.3f}",
                flush=True,
            )
    with (out / "bench.tsv").open("w") as handle:
        handle.write(
            "pair\tlabel\telapsed_s\tworker_dsp_ns\tframes\treal_s\tuser_s\tsys_s\n"
        )
        for row in rows:
            handle.write("\t".join(str(v) for v in row) + "\n")

    def ratios(index: int | tuple[int, int]) -> list[float]:
        def value(row: tuple[object, ...]) -> float:
            if isinstance(index, tuple):
                return float(row[index[0]]) + float(row[index[1]])  # type: ignore[arg-type]
            return float(row[index])  # type: ignore[arg-type]

        out_ratios = []
        for pair in range(1, pairs + 1):
            by = {row[1]: value(row) for row in rows if row[0] == pair}
            out_ratios.append(by[label_b] / by[label_a])
        return out_ratios

    summary = {
        "pairs": pairs,
        "order": f"{label_a},{label_b} then {label_b},{label_a} alternating",
        **extra,
        f"median_ratio_{label_b}_over_{label_a}": {
            "workload_wall": median(ratios(2)),
            "worker_dsp": median(ratios(3)),
            "user_plus_sys": median(ratios((6, 7))),
        },
        "ratios": {
            "workload_wall": ratios(2),
            "worker_dsp": ratios(3),
            "user_plus_sys": ratios((6, 7)),
        },
        "rustc_vV": rustc_version(),
    }
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(f"pair  {label_a}_wall  {label_b}_wall  {label_a}_dsp_ns  {label_b}_dsp_ns")
    for pair in range(1, pairs + 1):
        c = next(r for r in rows if r[0] == pair and r[1] == label_a)
        u = next(r for r in rows if r[0] == pair and r[1] == label_b)
        print(f"{pair:>4}  {c[2]:>12.3f}  {u[2]:>8.3f}  {c[3]:>14}  {u[3]:>10}")
    for name, value in summary[f"median_ratio_{label_b}_over_{label_a}"].items():  # type: ignore[attr-defined]
        print(f"median {label_b}/{label_a} {name}: {value:.3f}")
    print(f"results in {out}")


def cmd_bench_bins(args: argparse.Namespace) -> None:
    """Paired comparison of two prebuilt workload test binaries."""
    gen = generated_dir()
    require_inputs(gen)
    state = state_dir()
    out = state / ("bench-" + datetime.datetime.now().strftime("%Y%m%d-%H%M%S"))
    out.mkdir(parents=True)
    labels = {"a": args.label_a, "b": args.label_b}
    if labels["a"] == labels["b"]:
        raise SystemExit("--label-a and --label-b must differ")
    binaries = {}
    hashes = {}
    for key, source in (("a", args.a), ("b", args.b)):
        src = pathlib.Path(source).resolve()
        if not src.is_file():
            raise SystemExit(f"not a file: {src}")
        label = labels[key]
        dst = out / f"{label}-{src.name}"
        shutil.copy2(src, dst)
        binaries[label] = (dst, src.parent)
        hashes[label] = sha256_file(dst)
        print(f"{label} binary sha256 {hashes[label]} ({src})")
    env = {
        labels["a"]: {"SHARC_FAST_REGIONS": args.fast_a, **parse_env(args.env_a)},
        labels["b"]: {"SHARC_FAST_REGIONS": args.fast_b, **parse_env(args.env_b)},
    }
    paired_bench(
        out,
        binaries,
        labels["a"],
        labels["b"],
        args.pairs,
        {
            "binary_sha256": hashes,
            "sources": [args.a, args.b],
            "env": env,
        },
        env,
    )


def parse_env(items: list[str]) -> dict[str, str]:
    out = {}
    for item in items:
        key, sep, value = item.partition("=")
        if not sep:
            raise SystemExit(f"--env-*: expected KEY=VALUE, got {item!r}")
        out[key] = value
    return out


def cmd_run(args: argparse.Namespace) -> None:
    """Run the workload once under the exclusive lock; print the gates."""
    gen = generated_dir()
    require_inputs(gen)
    binary = pathlib.Path(args.binary).resolve()
    log = pathlib.Path(args.log).resolve()
    log.parent.mkdir(parents=True, exist_ok=True)
    deps = pathlib.Path(args.deps).resolve() if args.deps else binary.parent
    extra = {"DYLD_LIBRARY_PATH": str(deps), **parse_env(args.env)}
    with benchlock("exclusive", "pgo run"):
        r = run_workload(binary, log, extra)
    print(
        f"elapsed_s={r.elapsed} worker_dsp_ns={r.dsp_ns} frames={r.frames} "
        f"real={r.real} user={r.user} sys={r.sys}"
    )


def cmd_build(args: argparse.Namespace) -> None:
    """Plain release coupled test binary for a generated core (SHARC_GEN_DIR)."""
    gen = pathlib.Path(args.gen).resolve()
    digi_env.check_generated(gen)
    target = (
        pathlib.Path(args.target_dir).resolve()
        if args.target_dir
        else native_target().target_dir(gen)
    )
    binary = build_tests(target, None, gen, target / "build.log")
    print(binary)


# --- clean: remove old experiment output and unreferenced profiles -------------------------

CLEAN_DAYS = 14
CLEAN_KEEP_BY_KEY = 4
# Run :func:`clean_all` at the end of every ``train``.
AUTO_CLEAN = True
# Directories directly in a device state dir that no command reads back. ``bench-*`` and
# ``train-N`` are written by bench and train, the others by older versions of this tool
# (``target`` and ``instrument-target`` are old cargo target dirs).
CLEAN_DIR_NAMES = re.compile(
    r"(bench-.*|ab-.*|control-target|raw-.*|train-\d+|target|instrument-target)\Z"
)


def pgo_root() -> pathlib.Path:
    return ROOT / "out" / "native" / "pgo"


@dataclasses.dataclass
class CleanItem:
    kind: str  # experiment, profile or by-key
    group: str  # a name pattern for experiment dirs, else the kind
    paths: list[pathlib.Path]
    size: int


@dataclasses.dataclass
class CleanPlan:
    state: pathlib.Path
    items: list[CleanItem] = dataclasses.field(default_factory=list)
    kept: dict[str, int] = dataclasses.field(default_factory=dict)
    notes: list[str] = dataclasses.field(default_factory=list)

    def keep(self, reason: str, count: int = 1) -> None:
        self.kept[reason] = self.kept.get(reason, 0) + count


def _tree_stats(path: pathlib.Path) -> tuple[float, int]:
    """Newest mtime and disk bytes of PATH (a file, link or tree); links are never followed."""
    seen: set[tuple[int, int]] = set()
    newest = 0.0
    total = 0

    def visit(entry: pathlib.Path) -> None:
        nonlocal newest, total
        try:
            st = entry.lstat()
        except OSError:
            return
        newest = max(newest, st.st_mtime)
        if stat.S_ISLNK(st.st_mode) or (st.st_ino, st.st_dev) in seen:
            return
        seen.add((st.st_ino, st.st_dev))
        total += getattr(st, "st_blocks", 0) * 512 or st.st_size

    visit(path)
    if path.is_dir() and not path.is_symlink():
        for base, dirs, files in os.walk(path, followlinks=False):
            for name in (*dirs, *files):
                visit(pathlib.Path(base) / name)
    return newest, total


def _inside(root: pathlib.Path, directory: pathlib.Path) -> bool:
    real = pathlib.Path(os.path.realpath(directory))
    return real == root or root in real.parents


def _entries(directory: pathlib.Path) -> list[pathlib.Path]:
    try:
        return sorted(directory.iterdir())
    except OSError:
        return []


def _profile_name(record: object) -> str | None:
    try:
        return pathlib.Path(str(record["profdata"]["path"])).name  # type: ignore[index]
    except (KeyError, TypeError):
        return None


def plan_clean(
    state: pathlib.Path,
    days: float,
    now: float | None = None,
    root: pathlib.Path | None = None,
) -> CleanPlan:
    """What :func:`clean_state` would remove from one device state dir.

    Age is the newest mtime anywhere inside a candidate, so anything a running training or
    benchmark touches stays. Profiles named by manifest.json or by a kept by-key entry stay
    (matched by file name: a manifest trained in another lane records that lane's path).
    """
    plan = CleanPlan(state)
    root = pathlib.Path(os.path.realpath(root or pgo_root()))
    cutoff = (time.time() if now is None else now) - days * 86400
    if not _inside(root, state):
        plan.notes.append(f"skipped: {state} resolves outside {root}")
        return plan

    # Experiment output, by name only.
    groups: dict[str, list[CleanItem]] = {}
    for entry in _entries(state):
        if not CLEAN_DIR_NAMES.match(entry.name):
            continue
        if not entry.is_dir():  # a real directory, or a link to one
            continue
        newest, size = _tree_stats(entry)
        if newest >= cutoff:
            plan.keep("young experiment dirs")
            continue
        group = (
            entry.name
            if entry.name.endswith("target")
            else entry.name.split("-")[0] + "-*"
        )
        groups.setdefault(group, []).append(
            CleanItem("experiment", group, [entry], size)
        )
    for group in sorted(groups):
        plan.items.extend(groups[group])

    # Profiles: the manifest and the newest by-key entries name those to keep.
    referenced: dict[str, str] = {}
    manifest_ok = False
    try:
        manifest = json.loads((state / "manifest.json").read_text())
        name = _profile_name(manifest)
        manifest_ok = name is not None
        if name:
            referenced[name] = "manifest"
    except (OSError, ValueError):
        pass
    if not manifest_ok:
        plan.notes.append("manifest.json unreadable: all profiles kept")

    by_key = state / "by-key"
    if by_key.exists() and not _inside(root, by_key):
        plan.notes.append(f"by-key skipped: resolves outside {root}")
    elif by_key.exists():
        valid: list[tuple[float, pathlib.Path, str]] = []
        for entry in _entries(by_key):
            if entry.suffix != ".json" or entry.is_symlink() or not entry.is_file():
                continue
            try:
                name = _profile_name(json.loads(entry.read_text()))
                mtime = entry.stat().st_mtime
            except (OSError, ValueError):
                name = None
            if name is None:
                plan.notes.append(f"corrupt by-key entry left in place: {entry.name}")
                continue
            valid.append((mtime, entry, name))
        valid.sort(key=lambda item: (item[0], item[1].name), reverse=True)
        for rank, (mtime, entry, name) in enumerate(valid):
            if rank < CLEAN_KEEP_BY_KEY or mtime >= cutoff:
                referenced.setdefault(name, "by-key")
                plan.keep("by-key entries")
            else:
                plan.items.append(
                    CleanItem("by-key", "by-key", [entry], entry.stat().st_size)
                )

    profiles = state / "profiles"
    if profiles.exists() and not _inside(root, profiles):
        plan.notes.append(f"profiles skipped: resolves outside {root}")
    elif profiles.exists():
        files: dict[str, list[pathlib.Path]] = {}
        for entry in _entries(profiles):
            if entry.is_symlink() or entry.is_file():
                for suffix in (".profdata.crates.json", ".profdata"):
                    if entry.name.endswith(suffix) and len(entry.name) > len(suffix):
                        base = entry.name[: -len(suffix)] + ".profdata"
                        files.setdefault(base, []).append(entry)
                        break
        for base in sorted(files):
            if base in referenced:
                plan.keep(f"profiles named by {referenced[base]}")
                continue
            if not manifest_ok:
                plan.keep("profiles kept: manifest unreadable")
                continue
            stats = [_tree_stats(path) for path in files[base]]
            if max(newest for newest, _ in stats) >= cutoff:
                plan.keep("young profiles")
                continue
            plan.items.append(
                CleanItem(
                    "profile", "profile", files[base], sum(size for _, size in stats)
                )
            )
    return plan


def apply_clean(plan: CleanPlan, root: pathlib.Path | None = None) -> list[CleanItem]:
    """Delete the items of PLAN; the ones that were removed. A failure leaves that item."""
    root = pathlib.Path(os.path.realpath(root or pgo_root()))
    done = []
    for item in plan.items:
        try:
            for path in item.paths:
                if path.parent not in (
                    plan.state,
                    plan.state / "profiles",
                    plan.state / "by-key",
                ) or not _inside(root, path.parent):
                    raise OSError(f"refusing {path}: outside {root}")
                if path.is_symlink() or not path.is_dir():
                    path.unlink()
                else:
                    shutil.rmtree(path)
            done.append(item)
        except OSError as error:
            plan.notes.append(f"not removed: {item.paths[0].name}: {error}")
    return done


def human_bytes(size: float) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if size < 1000 or unit == "GB":
            return f"{size:.0f} B" if unit == "B" else f"{size:.1f} {unit}"
        size /= 1000
    return ""


def clean_states(root: pathlib.Path | None = None) -> list[pathlib.Path]:
    """Every ``<root>/{dn2,dt2,both}/<triple>`` state dir."""
    root = root or pgo_root()
    return [
        triple
        for device in ("dn2", "dt2", BOTH)
        for triple in _entries(root / device)
        if triple.is_dir() and not triple.is_symlink()
    ]


def summary_line(done: list[CleanItem], dry_run: bool) -> str:
    profiles = sum(1 for item in done if item.kind == "profile")
    experiments = sum(1 for item in done if item.kind == "experiment")
    entries = sum(1 for item in done if item.kind == "by-key")
    freed = sum(item.size for item in done)
    verb = "would remove" if dry_run else "removed"
    extra = f", {entries} by-key entries" if entries else ""
    return (
        f"pgo clean: {verb} {profiles} profiles, {experiments} experiment dirs"
        f"{extra}, freed {human_bytes(freed)}"
    )


def print_plan(plan: CleanPlan) -> None:
    print(f"== {plan.state}")
    by_group: dict[str, list[CleanItem]] = {}
    for item in plan.items:
        by_group.setdefault(item.group, []).append(item)
    if not by_group:
        print("  nothing to remove")
    for group, items in by_group.items():
        names = [item.paths[0].name for item in items]
        more = f" (+{len(names) - 3} more)" if len(names) > 3 else ""
        label = {"profile": "profiles", "by-key": "by-key entries"}.get(group, group)
        print(
            f"  remove {label}: {len(items)}, {human_bytes(sum(i.size for i in items))}; "
            f"first: {', '.join(names[:3])}{more}"
        )
    print(f"  total to free: {human_bytes(sum(item.size for item in plan.items))}")
    for reason in sorted(plan.kept):
        print(f"  kept {reason}: {plan.kept[reason]}")
    for note in plan.notes:
        print(f"  note: {note}")


def clean_all(days: float = CLEAN_DAYS, dry_run: bool = False) -> list[CleanItem]:
    """Clean every device state dir; prints a dry-run report or one summary line."""
    done: list[CleanItem] = []
    for state in clean_states():
        plan = plan_clean(state, days)
        if dry_run:
            print_plan(plan)
            done.extend(plan.items)
        else:
            done.extend(apply_clean(plan))
            for note in plan.notes:
                print(f"pgo clean: {state.parent.name}: {note}")
    print(summary_line(done, dry_run))
    return done


def auto_clean() -> None:
    """The cleanup at the end of a training: never fails it."""
    if not AUTO_CLEAN:
        return
    try:
        clean_all()
    except Exception as error:  # noqa: BLE001
        print(f"pgo clean failed: {error}", file=sys.stderr)


def cmd_clean(args: argparse.Namespace) -> None:
    clean_all(args.days, args.dry_run)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    train = sub.add_parser(
        "train", help="build instrumented, run the workload, merge a profile"
    )
    train.add_argument("--runs", type=int, default=TRAIN_RUNS)
    train.add_argument(
        "--set-current",
        action="store_true",
        help="from a linked worktree: also overwrite the shared manifest.json (default: by-key only)",
    )
    train.add_argument("--device", choices=["dn2", "dt2", BOTH], default="dn2")
    train.add_argument(
        "--variant", help="dt2, both: target-dir variant of the instrumented build"
    )
    train.add_argument(
        "--fixture-dn2",
        help="both: DN2 runtime fixture (default out/fixtures/runtime/dn2)",
    )
    train.add_argument(
        "--fixture-dt2",
        help="both: DT2 runtime fixture (default out/fixtures/runtime/dt2)",
    )
    clean = sub.add_parser(
        "clean",
        help="remove old experiment dirs and unreferenced profiles under out/native/pgo",
    )
    clean.add_argument("--days", type=float, default=CLEAN_DAYS)
    clean.add_argument("--dry-run", action="store_true")
    check = sub.add_parser(
        "check", help="print ok/stale/missing for the current profile"
    )
    check.add_argument("--device", choices=["dn2", "dt2", BOTH], default="dn2")
    cb = sub.add_parser(
        "coupled-build", help="build coupled-run (plain, or with --profile)"
    )
    cb.add_argument("--device", choices=["dn2", "dt2", BOTH], default="dt2")
    cb.add_argument("--profile", help="profdata path, or 'current' for the device's")
    cb.add_argument("--variant", help="target-dir variant (default pgo-cr-<device>)")
    verify = sub.add_parser(
        "verify", help="check the profile's crate hashes against the app binary"
    )
    verify.add_argument("--binary", help="default: the launcher's pgo-use app binary")
    verify.add_argument("--device", choices=["dn2", "dt2", BOTH], default="dn2")
    plan = sub.add_parser("plan", help="show what the launcher would do")
    plan.add_argument("--device", choices=["dn2", "dt2", BOTH], default="dn2")
    bench = sub.add_parser("bench", help="paired control vs profile-use benchmark")
    bench.add_argument("--pairs", type=int, default=5)
    build = sub.add_parser(
        "build", help="plain release workload test binary for a generated core"
    )
    build.add_argument("--gen", required=True, help="generated core directory")
    build.add_argument(
        "--target-dir", help="default: out/native/target/<gen-key> (native_target.py)"
    )
    run = sub.add_parser("run", help="run the workload once (exclusive lock)")
    run.add_argument("binary")
    run.add_argument("--log", required=True)
    run.add_argument("--deps", help="directory with the dylibs (default: the binary's)")
    run.add_argument("--env", action="append", default=[], metavar="KEY=VALUE")
    bins = sub.add_parser(
        "bench-bins", help="paired benchmark of two prebuilt workload test binaries"
    )
    bins.add_argument("a", help="baseline test binary")
    bins.add_argument("b", help="test binary compared against it")
    bins.add_argument("--pairs", type=int, default=5)
    bins.add_argument("--label-a", default="a")
    bins.add_argument("--label-b", default="b")
    bins.add_argument(
        "--fast-a", default="", help="SHARC_FAST_REGIONS for the baseline binary"
    )
    bins.add_argument(
        "--fast-b", default="", help="SHARC_FAST_REGIONS for the compared binary"
    )
    for side in ("a", "b"):
        bins.add_argument(
            f"--env-{side}",
            action="append",
            default=[],
            metavar="KEY=VALUE",
            help=f"extra environment for the {side} binary (e.g. SHARC_FAST_AUTO=64,64)",
        )
    args = parser.parse_args(argv)
    if args.command == "run":
        cmd_run(args)
    elif args.command == "train":
        if args.device == BOTH:
            cmd_train_both(args)
        elif args.device == "dt2":
            cmd_train_dt2(args)
        else:
            cmd_train(args)
    elif args.command == "clean":
        cmd_clean(args)
    elif args.command == "coupled-build":
        cmd_coupled_build(args)
    elif args.command == "bench":
        cmd_bench(args)
    elif args.command == "build":
        cmd_build(args)
    elif args.command == "bench-bins":
        cmd_bench_bins(args)
    elif args.command == "check":
        if args.device == BOTH:
            return cmd_check_both()
        return cmd_check_dt2() if args.device == "dt2" else cmd_check(args)
    elif args.command == "verify":
        return cmd_verify(args)
    else:
        return cmd_plan(args)
    return 0


if __name__ == "__main__":
    sys.exit(main())
