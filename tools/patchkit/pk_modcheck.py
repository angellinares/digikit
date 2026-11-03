#!/usr/bin/env python3
"""patchkit: check one module with its conformance suite and own tests.

    uv run python -m tools.patchkit.pk_modcheck NAME [--device dt2|dn2] [--trace]      (mise run module:check NAME)

NAME is a module id (`<author/name>`) or its last part (`<name>`). An effect module runs tests/modules/test_fx_conformance.py. A machine module (tools/patchkit/pk_conform.py) runs the conformance
suite, tests/modules/test_machine_conformance.py, for its devices (docs/TESTING.md "Machine conformance"). The same process includes the module's own tests: `modules/<group>/<name>/tests/**/test_*.py` and its scenarios in `tests/scenarios/*.toml` (only those run, at every tier). One pytest process runs those Python tests. A module-owned Nx `test-device` target then runs its declared TypeScript device acceptance; any failing stage stops the check before size/fit.
Own tests may build or launch an emulator. A service/raw module with no Python tests has its objects checked against their sources, then runs its owned device target if present. Size and fit follow only after both stages pass. This command does not prove every module loads in patched firmware.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

from tools import python_env  # noqa: E402
from tools.patchkit import modroots, pk_conform  # noqa: E402

CONFORMANCE = "tests/modules/test_machine_conformance.py"
FX_CONFORMANCE = "tests/modules/test_fx_conformance.py"


def resolve(name: str) -> str:
    mods = list(modroots.manifests())
    if name in mods:
        return name
    hit = [m for m in mods if m.split("/", 1)[1] == name]
    if len(hit) != 1:
        raise SystemExit(
            f"{name!r}: {'no such module' if not hit else 'ambiguous: ' + ', '.join(hit)}"
        )
    return hit[0]


def own_tests(mid: str) -> list[str]:
    """Test files of the module: `tests/**/test_*.py` in its folder."""
    found = sorted((modroots.module_dir(mid) / "tests").rglob("test_*.py"))
    return [p.relative_to(ROOT).as_posix() for p in found]


def _scenario_tests(kinds: set[str]) -> list[str]:
    """The tests/digikit test files that run scenarios of KINDS (their `cases((kinds), device)` call)."""
    files: set[str] = set()
    for path in sorted((ROOT / "tests/digikit").glob("test_*.py")):
        match = re.search(r"cases\(\(([^)]*)\)", path.read_text(encoding="utf-8"))
        if match and kinds.intersection(re.findall(r'"([a-z]+)"', match.group(1))):
            files.add(path.relative_to(ROOT).as_posix())
    return sorted(files)


def plan(mid: str, device: str | None) -> tuple[list[str], dict[str, str]]:
    """(pytest paths, extra environment) for module MID."""
    paths: list[str] = []
    env: dict[str, str] = {}
    if pk_conform.is_machine_module(mid):
        paths.append(CONFORMANCE)
        env["DIGI_CONFORM_MODULES"] = mid
        if device:
            env["DIGI_CONFORM_DEVICE"] = device
    if pk_conform.is_fx_module(mid):
        paths.append(FX_CONFORMANCE)
        env["DIGI_CONFORM_MODULES"] = mid
    paths += own_tests(mid)
    # the module's own scenarios (tests/scenarios/*.toml): the digikit test files of their kinds, limited to them, all tiers
    scen = sorted(
        p.stem for p in (modroots.module_dir(mid) / "tests/scenarios").glob("*.toml")
    )
    if scen:
        kinds = {
            tomllib.loads(p.read_text(encoding="utf-8"))
            .get("scenario", {})
            .get("kind", "")
            for p in (modroots.module_dir(mid) / "tests/scenarios").glob("*.toml")
        }
        paths += [t for t in _scenario_tests(kinds) if t not in paths]
        env["DIGIKIT_SCENARIOS"] = ",".join(scen)
        env["DIGIKIT_TIER"] = "full"
    return paths, env


def own_device_target(mid: str) -> str | None:
    """A module's own Nx test-device target (inferred by tools/nx/modules.mjs from its *.device.test.ts files)
    is its sole TypeScript acceptance declaration."""
    from tools.devtools import nx_graph

    node = nx_graph.nx_graph()["nodes"].get(mid)
    if node is None or "test-device" not in node["data"].get("targets", {}):
        return None
    return f"{mid}:test-device"


def run_device_target(mid: str, target: str) -> int:
    """Resolve the legacy both-device inputs, then let the owned Nx target run its existing preparation."""
    from tools.devtools import benchlock
    from tools.patchkit import pk_harness

    binaries = {}
    # The plain daemon build needs the same shared protection as the legacy harness path.
    # Nested shared resolution is supported; release this span before the target owns its work.
    with benchlock.hold("shared", f"module:check {mid} daemon resolution"):
        for device, version in (("dt2", "1.17"), ("dn2", "1.12")):
            syx, _ready = pk_harness.firmware_inputs(pk_harness.Cell(device, version))
            binaries[f"DIGI_EMUD_{device.upper()}"] = str(
                pk_harness.daemon_binary(device, syx, pgo=None)
            )
    print(f"{mid}: device acceptance {target} (DT2 and DN2)", flush=True)
    return subprocess.run(
        ["pnpm", "exec", "nx", "run", target],
        cwd=ROOT,
        env=python_env({**os.environ, **binaries}),
    ).returncode


def device_caps(dev: str) -> dict[str, dict[str, int]]:
    """CAPACITY of an empty device DEV as its device file sets the limits (nothing used): what a module must fit to load on it alone."""
    from tools.patchkit import pk_device

    lim = pk_device.limits(dev)
    sel = pk_device.contract_max()["modules"]

    def cap(limit: int, largest: int = 0) -> dict[str, int]:
        return {"limit": limit, "used": 0, "largest": largest}

    return {
        "MODULES": cap(lim.modules),
        "SELECTION": cap(sel),
        "ARENA": cap(lim.arena_bytes),
        "STAGE": cap(lim.stage_bytes, lim.stage_bytes),
        "DSP_AREA": cap(lim.dsp_area_bytes),
        "MACHINE_ROWS": cap(lim.machine_rows),
        "VOICE_ROWS": cap(lim.machine_rows),
        "PARAM_ROWS": cap(lim.param_rows),
        "PAGE_ROWS": cap(lim.page_rows),
        "FX_SLOTS": cap(lim.fx_slots),
    }


def needs_lines(dkm: bytes, dev: str) -> list[str]:
    """What a load of a packed module takes, inferred from the file (docs/design/digikit-dkm.md: `patchkit needs`, packages/dkneeds), and whether it
    fits the empty device DEV (the fit rule, `dkneeds.fit`)."""
    from tools.device import dkneeds

    try:
        n = dkneeds.needs_of(dkm)
    except dkneeds.NeedsError as e:
        return [
            f"{dev}: needs    none: {e}",
            f"{dev}: fits     NO: the loader refuses the file",
        ]
    shown = ", ".join(
        f"{label} {n[k]}"
        for label, k in (
            ("CPU memory", "ARENA"),
            ("CPU memory while the DSP code is sent", "ARENA_XFER"),
            ("DSP memory", "DSP_AREA"),
            ("machines", "MACHINE_ROWS"),
            ("voices", "VOICE_ROWS"),
            ("params", "PARAM_ROWS"),
            ("pages", "PAGE_ROWS"),
        )
        if n[k]
    )
    caps = device_caps(dev)
    short = dkneeds.fit(caps, [n], empty=True)["each"][0]
    verdict = (
        "fits an empty device"
        if short is None
        else f"DOES NOT FIT even an empty device: {short['describe']}"
    )
    return [
        f"{dev}: needs    {shown or 'nothing but its code'}",
        f"{dev}: fits     {verdict}",
    ]


def size_lines(mid: str, devices: list[str], trace: bool = False) -> list[str]:
    """The `size` line of `patchkit pack` for module MID, once per device (design rule D5), then what a load of it takes (inferred from the file) and whether it fits an
    empty device. A module that does not pack (no ColdFire object, or several, e.g. a patched-in module) gets a "not packed" line with the reason."""
    from tools.patchkit import patchkit

    lines: list[str] = []
    with tempfile.TemporaryDirectory(prefix="pk-modcheck-") as tmp:
        for dev in devices:
            out = Path(tmp) / f"{mid.rsplit('/', 1)[-1]}-{dev}.dkm"
            r = patchkit.run(
                [
                    "pack",
                    mid,
                    "--device",
                    dev,
                    "-o",
                    str(out),
                    *(["--trace"] if trace else []),
                ],
                capture=True,
            )
            if r.returncode:
                why = (r.stderr or r.stdout).strip().splitlines()
                lines.append(
                    f"{dev}: size     not packed ({why[-1] if why else 'pack failed'})"
                )
                continue
            lines += [
                f"{dev}: size     {ln.split(': ', 1)[-1]}"
                for ln in r.stdout.splitlines()
                if ln.startswith("size ")
            ]
            lines += needs_lines(out.read_bytes(), dev)
    return lines


def main() -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument("name")
    ap.add_argument("--device", choices=pk_conform.DEVICES)
    ap.add_argument(
        "--trace",
        action="store_true",
        help="pack the size lines with DK_TRACE defined (patchkit pack --trace); the conformance and own tests run the module as stored",
    )
    a = ap.parse_args()
    mid = resolve(a.name)
    paths, env = plan(mid, a.device)
    target = own_device_target(mid)
    kind = (
        "machine module"
        if CONFORMANCE in paths
        else "effect module"
        if FX_CONFORMANCE in paths
        else "module (no machine or effect DSP: no conformance suite)"
    )
    print(f"{mid}: {kind}; pytest {' '.join(paths) or '(nothing to run)'}", flush=True)
    devices = [a.device] if a.device else list(pk_conform.DEVICES)
    if not paths:
        from tools.patchkit import patchkit

        try:
            patchkit.build_module(modroots.module_dir(mid), check=True)
        except patchkit.BuildError as e:
            print(f"{mid}: {e}", file=sys.stderr)
            return 1
    else:
        cmd = [
            sys.executable,
            "-m",
            "pytest",
            "-q",
            "-p",
            "no:cacheprovider",
            "-rf",
            *paths,
        ]
        rc = subprocess.run(
            cmd, cwd=ROOT, env=python_env({**os.environ, **env})
        ).returncode
        if rc:
            return rc
    if target:
        rc = run_device_target(mid, target)
        if rc:
            return rc
    for line in size_lines(mid, devices, a.trace):
        print(line, flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
