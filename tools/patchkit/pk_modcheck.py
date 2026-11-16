#!/usr/bin/env python3
"""patchkit: check one module with its conformance suite and own tests.

    uv run python -m tools.patchkit.pk_modcheck NAME [--device dt2|dn2] [--trace]      (mise run module:check NAME)

NAME is a module id (`<author/name>`) or its last part (`<name>`), or `service/<name>` for a service crate of the runtime (`modules/core/services/<name>`: the structure check, `patchkit service-check`, then the crate's `rust-tests`). A machine or effect module runs the conformance suite on the SHARC-only rig
(packages/sharc-rig-kit, docs/TESTING.md "The Vitest kit"), for its devices, then `patchkit uses set` writes its measured DSP cost. Then one pytest process runs the module's own tests: `modules/<group>/<name>/tests/**/test_*.py` and its scenarios in `tests/scenarios/*.toml` (only those run, at every tier). Module-owned Nx `test-device-all` targets (full inputs) then run their declared TypeScript device acceptance; any failing stage stops the check before size/fit.
Own tests may build or launch an emulator. A service/raw module with no Python tests has its objects checked against their sources, then runs its owned device target if present. Size and fit follow only after both stages pass. This command does not prove every module loads in patched firmware.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

from tools import python_env  # noqa: E402
from tools.patchkit import modroots, pk_conform  # noqa: E402

USES_KEYS = ("machine_rows", "param_rows", "page_rows", "record_bytes")


# The DSP cost of a module (docs/plans/sdk.md section 9), instructions per block, from the conformance run: the worst steady block of one voice of a machine and the
# worst process call of an effect. They are keys of the same `[uses.<device>]` tables and become the `DSP_COST_VOICE` and `DSP_COST_FX` entries of the header table.
COST_VOICE = "dsp_cost_voice"
COST_FX = "dsp_cost_fx"


def write_uses(toml_path: Path, uses: dict[str, dict[str, int]]) -> bool:
    """Write the `[uses.<device>]` tables of a module.toml like a lock file (docs/plans/sdk.md 6.1): USES maps a device to the counts of
    its keys (USES_KEYS: committed rows read from that device's emulated run; COST_VOICE and COST_FX: the measured DSP cost). The file changes
    only when a count differs, and then only the lines of the keys given: other text, comments, other keys and the order of the tables stay.
    Returns True when the file was written."""
    text = toml_path.read_text(encoding="utf-8")
    for dev, counts in uses.items():
        text = _write_table(text, dev, counts)
    if text == toml_path.read_text(encoding="utf-8"):
        return False
    toml_path.write_text(text, encoding="utf-8")
    return True


def _write_table(text: str, dev: str, uses: dict[str, int]) -> str:
    head = f"[uses.{dev}]"
    keys = tuple(uses)
    want = [f"{k} = {int(uses[k])}" for k in keys]
    lines = text.split("\n")
    start = next((i for i, ln in enumerate(lines) if ln.strip() == head), None)
    if start is None:
        # a new table follows the last `[uses.*]` table, else goes to the end
        last = [i for i, ln in enumerate(lines) if ln.strip().startswith("[uses.")]
        if not last:
            return text.rstrip("\n") + f"\n\n{head}\n" + "\n".join(want) + "\n"
        at = last[-1] + 1
        while at < len(lines) and not lines[at].lstrip().startswith("["):
            at += 1
        while at > 0 and lines[at - 1].strip() == "":
            at -= 1
        return "\n".join(lines[:at] + ["", head, *want] + lines[at:])
    end = start + 1
    while end < len(lines) and not lines[end].lstrip().startswith("["):
        end += 1
    body = lines[start + 1 : end]
    have = {
        m.group(1): m.group(2)
        for ln in body
        if (m := re.match(r"\s*(\w+)\s*=\s*(\d+)\s*(?:#.*)?$", ln))
    }
    if all(have.get(k) == str(int(uses[k])) for k in keys):
        return text
    # keep comments and blank lines of the table; the count lines take their place, missing ones follow the last count
    out, seen = [], set()
    for ln in body:
        m = re.match(r"\s*(\w+)\s*=", ln)
        if m and m.group(1) in keys:
            seen.add(m.group(1))
            out.append(f"{m.group(1)} = {int(uses[m.group(1)])}")
        else:
            out.append(ln)
    missing = [w for w in want if w.split(" ")[0] not in seen]
    trail = 0
    while out and out[-1].strip() == "":
        out.pop()
        trail += 1
    out += missing + [""] * trail
    return "\n".join(lines[: start + 1] + out + lines[end:])


def _daemon(device: str) -> str:
    """The plain emulator daemon of DEVICE: `DIGI_EMUD_<DEV>`, else resolved as `run_device_targets` does."""
    given = os.environ.get(f"DIGI_EMUD_{device.upper()}")
    if given:
        return given
    from tools.devtools import benchlock
    from tools.patchkit import pk_harness

    version = {"dt2": "1.17", "dn2": "1.12"}[device]
    with benchlock.hold("shared", f"module:check daemon resolution {device}"):
        syx, _ready = pk_harness.firmware_inputs(pk_harness.Cell(device, version))
        return str(pk_harness.daemon_binary(device, syx, pgo=None))


def commit_stage(mid: str, devices: list[str]) -> bool:
    """For a module with `[[machine]]` or `[[fx]]`: load it on the emulated device, read the rows it committed (tools/devtools/commit_records.ts, decoded by
    `patchkit records`), write `[uses.<device>]` into its module.toml and `build/gen/gallery.json` from them (docs/plans/sdk.md 6.1 and 6.4). The
    conformance suite that runs next reads that gallery.json. A tool or a service has no rows to commit and is left alone: returns False."""
    moddir = modroots.module_dir(mid)
    manifest_path = moddir / "module.toml"
    original = manifest_path.read_text(encoding="utf-8")
    if "[[machine]]" not in original and "[[fx]]" not in original:
        return False  # declares nothing through builder calls: no rows to measure
    # The measuring build takes the device limits as its `[uses]`: the loader refuses a module whose tables are absent, zero or lower than what its
    # init declares, so a new or changed module could not be measured with its own numbers. The measured rows replace them below.
    write_uses(manifest_path, {d: seed_uses(d) for d in devices})
    try:
        per = _measure(mid, devices)
    except BaseException:
        manifest_path.write_text(original, encoding="utf-8")
        raise
    if not per:
        manifest_path.write_text(original, encoding="utf-8")
        return False
    kept = tomllib.loads(original).get("uses", {})
    write_uses(
        manifest_path,
        {
            d: {k: int(kept[d].get(k, 0)) for k in USES_KEYS}
            for d in devices
            if d not in per and d in kept
        },
    )
    return _write_measured(mid, moddir, per, original)


# Reports live in this worktree's dist/ (out/ is shared by every worktree: a report of another lane's run must never reach `write_costs_rust`).
RIG_REPORTS = (
    ROOT / "dist" / "conformance-rig"
)  # the SHARC-only rig writes here (packages/sharc-rig-kit/src/report.ts)


def measured_costs(
    mid: str, devices: list[str], since: float, reports: Path = RIG_REPORTS
) -> dict[str, dict[str, int]]:
    """The DSP cost of module MID per device from the reports of the conformance run that started at `since` (the kit writes them, `RIG_REPORTS`): the worst steady block over every case. A machine's cost is per voice; an effect's is the whole process call (the effect and
    core/fx around it, which every slot takes), measured once for both devices. A report older than the run, or none, leaves that cost out; a report of another worktree is an error."""
    out: dict[str, dict[str, int]] = {}

    def worst(path: Path) -> int | None:
        if not path.is_file() or path.stat().st_mtime < since:
            return None
        report = json.loads(path.read_text(encoding="utf-8"))
        owner = report.get("worktree")
        if owner != str(ROOT.resolve()):
            raise RuntimeError(
                f"{path} was written by the worktree {owner!r}, not by this one ({ROOT}): "
                "refusing to write its DSP cost into a module.toml; run the conformance suite here"
            )
        cases = report.get("cases") or []
        return max((int(c["steady"]) for c in cases), default=None)

    stem = mid.replace("/", "__")
    if pk_conform.is_machine_module(mid):
        for dev in devices:
            cost = worst(reports / f"{stem}@{dev}.json")
            if cost:
                out.setdefault(dev, {})[COST_VOICE] = cost
    if pk_conform.is_fx_module(mid):
        cost = worst(reports / f"{stem}@fx.json")
        if cost:
            for dev in devices:
                out.setdefault(dev, {})[COST_FX] = cost
    return out


def rig_conformance(mid: str, env: dict[str, str]) -> int:
    """The conformance suites on the SHARC-only rig (packages/sharc-rig-kit): builds the rig, then runs the Vitest project with the selection in ENV.
    The kit writes the reports to dist/conformance-rig (`RIG_REPORTS`), which `write_costs_rust` reads."""
    from tools.patchkit import patchkit

    r = subprocess.run(
        ["pnpm", "nx:pilot", "run", "sharc-rig:build"],
        cwd=ROOT,
        env=python_env(dict(os.environ)),
    )
    if r.returncode:
        return r.returncode
    kit_env = {
        **os.environ,
        **env,
        "DIGI_PATCHKIT": str(patchkit.binary()),
        "DIGI_CONFORM_REPORTS": str(RIG_REPORTS),
    }
    return subprocess.run(
        [
            "pnpm",
            "exec",
            "vitest",
            "run",
            "--config",
            "vitest.host.config.ts",
            "--project",
            "sharc-rig-conformance",
        ],
        cwd=ROOT,
        env=python_env(kit_env),
    ).returncode


def write_costs_rust(
    mid: str, devices: list[str], since: float
) -> dict[str, dict[str, int]]:
    """`write_costs` through `patchkit uses set` (packages/patchkit/src/uses.rs): the same lines change as with `write_uses`."""
    from tools.patchkit import patchkit

    costs = measured_costs(mid, devices, since)
    for dev, c in sorted(costs.items()):
        flags = [x for k, v in c.items() for x in (f"--{k.replace('_', '-')}", str(v))]
        r = patchkit.run(
            ["uses", "set", "--id", mid, "--device", dev, *flags], capture=True
        )
        if r.returncode:
            raise RuntimeError(
                f"patchkit uses set {mid} {dev} failed: {(r.stderr or '').strip()}"
            )
        print(
            f"{mid}: {dev} DSP cost {', '.join(f'{k} {v}' for k, v in c.items())} instructions per block",
            flush=True,
        )
    return costs


def seed_uses(dev: str) -> dict[str, int]:
    """The `[uses.<dev>]` a module takes while it is measured: every row the device has."""
    from tools.patchkit import pk_device

    lim = pk_device.limits(dev)
    return {
        "machine_rows": lim.machine_rows,
        "param_rows": lim.param_rows,
        "page_rows": lim.page_rows,
        "record_bytes": 0,  # the loader does not check it; the measured size replaces it
    }


def _measure(mid: str, devices: list[str]) -> dict[str, dict]:
    """Pack MID for each device and read the rows its init committed on the emulated device. {} when no device packs it."""
    from tools.patchkit import patchkit, pk_committed

    per: dict[str, dict] = {}
    with tempfile.TemporaryDirectory(prefix="pk-commit-") as tmp:
        for dev in devices:
            out = Path(tmp) / f"{mid.rsplit('/', 1)[-1]}-{dev}.dkm"
            r = patchkit.run(
                ["pack", mid, "--device", dev, "-o", str(out)], capture=True
            )
            if r.returncode:
                continue  # not packed for this device (ports); the size lines say why
            # module:check packs the debug module itself so it needs no Nx pack output
            dbg = Path(tmp) / f"debug-{dev}.dkm"
            d = patchkit.run(
                ["pack", "digikit/debug", "--device", dev, "-o", str(dbg)],
                capture=True,
            )
            if d.returncode:
                raise RuntimeError(
                    f"packing digikit/debug for {dev} failed:\n{(d.stderr or '').strip()}"
                )
            per[dev] = pk_committed.committed(
                mid,
                dev,
                out,
                str(patchkit.binary()),
                _daemon(dev),
                debug_dkm=dbg,
                services=_bound_services(dev, Path(tmp)),
            )
    return per


# The services that are files of the card (`[[service]]` without `rust` in sdk.toml, loaded from services/<name>.dks): a machine or an effect needs them.
CARD_SERVICES = ("core/services/machines", "core/services/fx")


def _bound_services(dev: str, tmp: Path) -> list[Path]:
    """Pack the card services for DEV and bind them to the stock firmware, as the Nx targets `<id>:pack` and `<id>:bind` do (module:check needs no Nx output)."""
    from tools.patchkit import patchkit, pk_harness

    version = {"dt2": "1.17", "dn2": "1.12"}[dev]
    syx, _ready = pk_harness.firmware_inputs(pk_harness.Cell(dev, version))
    bound: list[Path] = []
    for sid in CARD_SERVICES:
        leaf = sid.rsplit("/", 1)[-1]
        packed = tmp / "svc" / dev / f"{leaf}.dks"
        packed.parent.mkdir(parents=True, exist_ok=True)
        out = tmp / "svc-bound" / dev / f"{leaf}.dks"
        out.parent.mkdir(parents=True, exist_ok=True)
        for args in (
            ["pack", sid, "--device", dev, "-o", str(packed)],
            ["bind", str(packed), "--firmware", str(syx), "-o", str(out)],
        ):
            r = patchkit.run(args, capture=True)
            if r.returncode:
                raise RuntimeError(
                    f"patchkit {args[0]} {sid} for {dev} failed:\n{(r.stderr or '').strip()}"
                )
        bound.append(out)
    return bound


def _write_measured(
    mid: str, moddir: Path, per: dict[str, dict], original: str
) -> bool:
    """Write the measured `[uses.<device>]` and `build/gen/gallery.json` of MID."""
    from tools.patchkit import pk_committed, pk_device

    manifest = tomllib.loads((moddir / "module.toml").read_text(encoding="utf-8"))
    ids = [m["id"] for m in manifest.get("machine", [])]
    counts = pk_committed.uses(per)
    write_uses(moddir / "module.toml", counts)
    changed = (moddir / "module.toml").read_text(encoding="utf-8") != original
    models = {d: pk_device.load(d)["modules"]["knobs"] for d in per}
    text = (
        json.dumps(
            pk_committed.gallery(
                mid, ids, per, models, [f["id"] for f in manifest.get("fx", [])]
            ),
            indent=2,
            sort_keys=True,
        )
        + "\n"
    )
    gallery = moddir / "build/gen/gallery.json"
    if not gallery.is_file() or gallery.read_text(encoding="utf-8") != text:
        gallery.parent.mkdir(parents=True, exist_ok=True)
        gallery.write_text(text, encoding="utf-8")
    print(
        f"{mid}: committed rows {counts}; module.toml [uses.<device>] {'written' if changed else 'unchanged'}",
        flush=True,
    )
    return True


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
    """(pytest paths, extra environment) for module MID. The conformance suite runs on the rig (`rig_conformance`), not as a pytest path; the environment selects it."""
    paths: list[str] = []
    env: dict[str, str] = {}
    if pk_conform.is_machine_module(mid):
        env["DIGI_CONFORM_MODULES"] = mid
        if device:
            env["DIGI_CONFORM_DEVICE"] = device
    if pk_conform.is_fx_module(mid):
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


def own_device_targets(mid: str) -> list[str]:
    """Nx device acceptance targets at the module root and its nested test-only owners."""
    from tools.devtools import nx_graph

    nodes = nx_graph.nx_graph()["nodes"]
    node = nodes.get(mid)
    if node is None:
        return []
    data = node["data"]
    owners = [mid] if "test-device" in data.get("targets", {}) else []
    root = data.get("root")
    if root:
        for name, candidate in nodes.items():
            candidate = candidate["data"]
            if (
                "test-only" in candidate.get("tags", [])
                and candidate.get("root", "").startswith(root + "/")
                and "test-device" in candidate.get("targets", {})
            ):
                owners.append(name)
    roots: dict[str, str] = {}
    for owner in sorted(owners):
        owner_root = nodes[owner]["data"].get("root")
        if owner_root and owner_root in roots:
            raise SystemExit(
                f"{mid}: ambiguous device acceptance root {owner_root}: {roots[owner_root]}, {owner}"
            )
        if owner_root:
            roots[owner_root] = owner
    return [f"{owner}:test-device-all" for owner in sorted(owners)]


def run_device_targets(mid: str, targets: list[str], device: str | None = None) -> int:
    """Resolve the device inputs once, then schedule all owned suites serially through Nx. Without DEVICE both devices run through each
    project's `test-device-all`. With DEVICE only that device's inputs are resolved (a missing image of the other device does not matter) and
    only the run halves of the owners run: `test-device-all--<file>--<device>`, or `test-device-all--<device>` in a project with one test file;
    with none, there is nothing to run and the result is 0."""
    from tools.devtools import benchlock
    from tools.patchkit import pk_harness

    pairs = (("dt2", "1.17"), ("dn2", "1.12"))
    selected = [p for p in pairs if device is None or p[0] == device]
    binaries = {}
    # The plain daemon build needs the same shared protection as the legacy harness path.
    # Nested shared resolution is supported; release this span before the target owns its work.
    with benchlock.hold("shared", f"module:check {mid} daemon resolution"):
        for dev, version in selected:
            syx, _ready = pk_harness.firmware_inputs(pk_harness.Cell(dev, version))
            binaries[f"DIGI_EMUD_{dev.upper()}"] = str(
                pk_harness.daemon_binary(dev, syx, pgo=None)
            )
    projects = [t.rsplit(":", 1)[0] for t in targets]
    names = ["test-device-all"]
    if device is not None:
        pattern = re.compile(rf"^test-device-all(--.+)?--{re.escape(device)}$")
        found: set[str] = set()
        for project in projects:
            r = subprocess.run(
                ["pnpm", "nx:pilot", "show", "project", project, "--json"],
                cwd=ROOT,
                env=python_env(dict(os.environ)),
                capture_output=True,
                text=True,
                check=True,
            )
            lines = r.stdout.splitlines()
            start = next(i for i, ln in enumerate(lines) if ln.startswith("{"))
            tgts = json.loads("\n".join(lines[start:])).get("targets", {})
            found.update(n for n in tgts if pattern.match(n))
        if not found:
            print(
                f"{mid}: no {device.upper()} device tests in {', '.join(projects)}",
                flush=True,
            )
            return 0
        names = sorted(found)
    which = "DT2 and DN2" if device is None else f"{device.upper()} only"
    print(f"{mid}: device acceptance {', '.join(targets)} ({which})", flush=True)
    # The launcher sets no-daemon and the job budget and takes the shared lock, so the hold above must end first.
    return subprocess.run(
        [
            "pnpm",
            "nx:pilot",
            "run-many",
            "-t",
            ",".join(names),
            "--projects",
            ",".join(projects),
            "--parallel=1",
        ],
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
        "DSP_COST": cap(pk_conform.budget(dev).headroom),
        "DSP_VOICES": cap(pk_device.load(dev)["adapter"]["machine"]["voices"]),
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
            ("DSP load per voice", "DSP_COST_VOICE"),
            ("DSP load of effects", "DSP_COST_FX"),
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


def service_check(name: str) -> int:
    """A service crate: the structure check (layout, one exported entry, declarations equal to sdk.toml), then its own host tests."""
    from tools.patchkit import patchkit

    crate = ROOT / "modules/core/services" / name
    if not (crate / "Cargo.toml").is_file():
        print(
            f"service/{name}: no such service crate ({crate.relative_to(ROOT)})",
            file=sys.stderr,
        )
        return 1
    r = patchkit.run(["service-check", str(crate)])
    if r.returncode:
        return r.returncode
    return subprocess.run(
        ["pnpm", "nx:pilot", "run", f"dkrt-svc-{name.replace('_', '-')}:rust-tests"],
        cwd=ROOT,
    ).returncode


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
    if a.name.startswith("service/"):
        return service_check(a.name.split("/", 1)[1])
    mid = resolve(a.name)
    commit_stage(mid, [a.device] if a.device else list(pk_conform.DEVICES))
    paths, env = plan(mid, a.device)
    targets = own_device_targets(mid)
    machine, effect = pk_conform.is_machine_module(mid), pk_conform.is_fx_module(mid)
    kind = (
        "machine module"
        if machine
        else "effect module"
        if effect
        else "module (no machine or effect DSP: no conformance suite)"
    )
    on_rig = machine or effect
    print(
        f"{mid}: {kind}; conformance on {'the SHARC-only rig' if on_rig else 'nothing'}; pytest {' '.join(paths) or '(nothing to run)'}",
        flush=True,
    )
    devices = [a.device] if a.device else list(pk_conform.DEVICES)
    if on_rig:
        started = time.time()
        rc = rig_conformance(mid, env)
        if rc:
            return rc
        if paths:
            rc = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "pytest",
                    "-q",
                    "-p",
                    "no:cacheprovider",
                    "-rf",
                    *paths,
                ],
                cwd=ROOT,
                env=python_env({**os.environ, **env}),
            ).returncode
            if rc:
                return rc
        write_costs_rust(mid, devices, started)
    elif not paths:
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
    if targets:
        rc = run_device_targets(mid, targets, a.device)
        if rc:
            return rc
    for line in size_lines(mid, devices, a.trace):
        print(line, flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
