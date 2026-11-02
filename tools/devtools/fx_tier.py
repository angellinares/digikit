"""FX tier comparator: FX scenarios must give the same PCM in the default and the no-AOT configuration.

    uv run python -m tools.devtools.fx_tier tier-quick                 # DN2, every 7th scenario with --all-no-aot
    uv run python -m tools.devtools.fx_tier tier-sweep --all-no-aot    # DN2, every scenario
    uv run python -m tools.devtools.fx_tier dt2-tier-quick             # DT2 (interpreter alone as no-AOT)
    uv run python -m tools.devtools.fx_tier dt2-tier-sweep [--out DIR] [--jobs N]

Without --all-no-aot the scenarios are NO_AOT_SUBSET. Chunk logs and reports go to --out (default
out/runs/fx-tier/<step>). The report is printed; the exit code is 1 on a problem, else 0.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from collections.abc import Callable, Mapping
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path

from tools import python_env
from tools.devtools import benchlock, fastcov
from tools.emulator import coupled

ROOT = Path(__file__).resolve().parent.parent.parent
PY = ["uv", "run", "python"]
FX_FILES = (
    "tests/regression/scenarios/dn2-trackfx.ts",
    "tests/regression/scenarios/dn2-sendfx.ts",
)
DT2_FX_FILES = (
    "tests/regression/scenarios/dt2-trackfx.ts",
    "tests/regression/scenarios/dt2-sendfx.ts",
)
# Every seventh FX scenario of each file: about 20 of 134, at least one per FX family. Only with --all-no-aot.
TIER_QUICK_EVERY = 7
# tier step -> (every EVERYth scenario with --all-no-aot, device)
TIER_STEPS: dict[str, tuple[int, str]] = {
    "tier-quick": (TIER_QUICK_EVERY, "dn2"),
    "tier-sweep": (1, "dn2"),
    "dt2-tier-quick": (TIER_QUICK_EVERY, "dt2"),
    "dt2-tier-sweep": (1, "dt2"),
}
# AOT versus interpreter (no-AOT) PCM equality runs on this fixed subset of the FX scenario files, not on
# all 134 (Em approved the reduction on 2026-10-19; `--all-no-aot` restores the full sets: every scenario
# of tier-sweep, every seventh of tier-quick, all oracle tests of check-no-aot). The pick covers distinct
# DSP paths, one or two scenarios each, always at the strongest setting (a clipped, saturated or
# fully-wet signal exercises the most code and shows a rounding difference first):
#   voice, no FX            tf-dry-hold       (the synth/sample voice alone: the base of every other scenario)
#   bit reduction           tf-br-127         track FX, quantiser
#   overdrive               tf-over-71        track FX, waveshaper; tf-over-127-flt-rt1: the filter and the
#   sample-rate reduction   tf-srr-127        routing-1 (256) paths that three track-FX scenarios share
#   delay                   tf-del-71 (track send); sf-del-ref, sf-del-fb-u127 (feedback above unity),
#                           sf-del-ppwidth-u127 (pingpong + stereo width)
#   reverb                  tf-rev-71; sf-rev-ref, sf-rev-decay-u127 (longest tail), sf-rev-shfreq-u127 (shimmer)
#   chorus                  tf-chr-71; sf-chr-ref, sf-chr-speed-u127
#   mixer / master          sf-mst-ratio      (the master compressor page: the mixer's last stage)
# The DT2 files have fewer scenarios (their numbers differ) and run in the interpreter alone (about 80 s per
# audio second), so the DT2 pick is the shortest one per path.
NO_AOT_SUBSET: dict[str, tuple[str, ...]] = {
    "tests/regression/scenarios/dn2-trackfx.ts": (
        "tf-dry-hold",
        "tf-br-127",
        "tf-over-71",
        "tf-over-127-flt-rt1",
        "tf-srr-127",
        "tf-del-71",
        "tf-rev-71",
        "tf-chr-71",
    ),
    "tests/regression/scenarios/dn2-sendfx.ts": (
        "sf-del-ref",
        "sf-del-fb-u127",
        "sf-del-ppwidth-u127",
        "sf-rev-ref",
        "sf-rev-decay-u127",
        "sf-rev-shfreq-u127",
        "sf-chr-ref",
        "sf-chr-speed-u127",
        "sf-mst-ratio",
    ),
    "tests/regression/scenarios/dt2-trackfx.ts": (
        "tf-dry-hold",
        "tf-br-127",
        "tf-over-127",
        "tf-srr-127",
        "tf-del-127",
        "tf-rev-127",
        "tf-chr-127",
    ),
    "tests/regression/scenarios/dt2-sendfx.ts": ("sf-mst-ratio",),
}
# no-AOT renders cost about 2x per simulated second (measured 1.9-2.0 on DN2 FX, 2026-10-14); weights chunk costs
TIER_NO_AOT_COST = 2.0


def no_aot_env() -> dict[str, str]:
    """The fast-tier-only configuration (fastcov `static-insns+follow`)."""
    _, env = fastcov.parse_config("static-insns+follow")
    return {**os.environ, **env}


def stream_process(
    cmd: list[str], log: Path, cwd: Path, env: dict[str, str]
) -> tuple[int, str]:
    """Run CMD with stdout and stderr merged; each line goes to LOG as it arrives (so `tail` follows a
    long step). The exit code and the whole output."""
    lines: list[str] = []
    with log.open("w") as out:
        proc = subprocess.Popen(
            cmd,
            cwd=cwd,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
        assert proc.stdout is not None
        for line in proc.stdout:
            lines.append(line)
            out.write(line)
            out.flush()
        returncode = proc.wait()
    return returncode, "".join(lines)


def tier_jobs_budget() -> int:
    """Coupled-run processes for all tier groups together: DIGI_TIER_JOBS, else cpus - 4."""
    env = os.environ.get("DIGI_TIER_JOBS")
    if env:
        jobs = int(env)
        if jobs < 1:
            raise SystemExit("fx_tier: DIGI_TIER_JOBS must be at least 1")
        return jobs
    return max(1, (os.cpu_count() or 1) - 4)


@dataclass(frozen=True)
class Chunk:
    """Consecutive scenarios of one (label, file) group, run by one coupled process."""

    label: str
    path: str
    names: tuple[str, ...]
    cost: float
    index: int


def plan_chunks(
    groups: list[tuple[str, str, list[str], dict[str, float], float]], budget: int
) -> list[Chunk]:
    """Cut each group (label, path, names, seconds by name, cost factor) into consecutive
    chunks of about total cost / (budget * 4), at least one scenario each; longest first."""
    costs = [
        [factor * seconds[n] for n in names] for _, _, names, seconds, factor in groups
    ]
    target = sum(sum(c) for c in costs) / (budget * 4)
    chunks: list[Chunk] = []
    for (label, path, names, _, _), group_costs in zip(groups, costs, strict=True):
        index = 0
        taken: list[str] = []
        cost = 0.0
        for name, c in zip(names, group_costs, strict=True):
            taken.append(name)
            cost += c
            if cost >= target:
                chunks.append(Chunk(label, path, tuple(taken), cost, index))
                index += 1
                taken, cost = [], 0.0
        if taken:
            chunks.append(Chunk(label, path, tuple(taken), cost, index))
    chunks.sort(key=lambda c: -c.cost)
    return chunks


def tier_names(
    path: str, seconds: dict[str, float], every: int, all_no_aot: bool
) -> list[str]:
    """The scenarios of one FX file a tier step compares: NO_AOT_SUBSET, or with ALL_NO_AOT every
    EVERYth of the file (the whole file for 1)."""
    if all_no_aot:
        return sorted(seconds)[::every]
    names = list(NO_AOT_SUBSET[path])
    missing = [n for n in names if n not in seconds]
    if missing:
        raise SystemExit(
            f"verify: NO_AOT_SUBSET names unknown scenarios in {path}: {missing}"
        )
    return names


def run(
    step: str,
    out: Path,
    all_no_aot: bool,
    jobs: int,
    base_env: Mapping[str, str],
    say: Callable[[str], None] = print,
) -> tuple[str | None, str]:
    """FX scenarios must give the same PCM in the default and no-AOT configurations.

    STEP is a key of TIER_STEPS. The scenarios are NO_AOT_SUBSET (documented there); with ALL_NO_AOT
    every EVERYth scenario of the FX files (tier-quick 7, tier-sweep 1). JOBS coupled processes run at
    once, with BASE_ENV as their environment; logs and reports go under OUT.

    DT2 runs its scenarios on the one-sample card; its no-AOT configuration is the
    interpreter alone (`SHARC_AOT=0`, about 80 s per audio second), as DT2 has no
    fast regions.

    Returns (problem or None, report text: the output of the failing chunk, else empty)."""
    every, device = TIER_STEPS[step]
    start = time.monotonic()
    shas: dict[str, dict[str, str]] = {}
    dt2 = device == "dt2"
    no_aot = {**os.environ, "SHARC_AOT": "0"} if dt2 else no_aot_env()
    envs: dict[str, dict[str, str] | None] = {"default": None, "no-aot": no_aot}
    groups: list[tuple[str, str, list[str], dict[str, float], float]] = []
    for label in envs:
        for path in DT2_FX_FILES if dt2 else FX_FILES:
            seconds = coupled.scenario_seconds(path)
            names = tier_names(path, seconds, every, all_no_aot)
            factor = TIER_NO_AOT_COST if label == "no-aot" else 1.0
            groups.append((label, path, names, seconds, factor))
    budget = jobs
    step_envs = {label: {**base_env, **(env or {})} for label, env in envs.items()}
    chunks = plan_chunks(groups, budget)

    def report_of(chunk: Chunk) -> Path:
        stem = Path(chunk.path).stem
        return (
            out
            / f"{step}-{chunk.label}-{stem}"
            / f"chunk-{chunk.index:03d}"
            / "report.json"
        )

    def run_chunk(chunk: Chunk) -> tuple[tuple[int, str], float]:
        report = report_of(chunk)
        report.parent.mkdir(parents=True, exist_ok=True)
        cmd = [
            *PY,
            "-m",
            "tools.emulator.coupled",
            "scenario",
            "--scenarios",
            chunk.path,
        ]
        cmd += [*chunk.names, "--no-build", "--jobs", "1", "--out", str(report)]
        if dt2:
            cmd += ["--device", "dt2", "--dt2-card"]
        with benchlock.hold("shared", f"verify {step}"):
            proc = stream_process(
                cmd,
                report.parent / "run.log",
                cwd=ROOT,
                env=python_env(step_envs[chunk.label]),
            )
        return proc, time.monotonic() - start

    with ThreadPoolExecutor(max_workers=budget) as pool:
        done = list(pool.map(run_chunk, chunks))
    ended: dict[tuple[str, str], float] = {}
    for chunk, (_, end) in zip(chunks, done, strict=True):
        key = (chunk.label, chunk.path)
        ended[key] = max(ended.get(key, 0.0), end)
    for label, path, names, _, _ in groups:
        count = sum(1 for c in chunks if (c.label, c.path) == (label, path))
        say(
            f"     {label} {Path(path).stem}: {len(names)} scenarios in "
            f"{count} chunks, {ended.get((label, path), 0.0):.0f} s"
        )
    say(f"     budget {budget} jobs, {len(chunks)} chunks")
    for chunk, ((returncode, text), _) in zip(chunks, done, strict=True):
        report = report_of(chunk)
        # Exit 2 is a failed gate in an unpinned FX scenario; only the PCM matters here.
        if returncode not in (0, 2) or not report.exists():
            problem = (
                f"{chunk.label} {chunk.path} chunk {chunk.index}: exit {returncode}"
            )
            return problem, text
        for result in coupled.scenario_results(json.loads(report.read_text())):
            shas.setdefault(result["name"], {})[chunk.label] = result["sha256"]
    for label, path, names, _, _ in groups:
        for n in names:
            if label not in shas.get(n, {}):
                return f"{label} {path}: no result for {n}", ""
    differ = [n for n, s in shas.items() if s.get("default") != s.get("no-aot")]
    if differ:
        return f"{len(differ)} of {len(shas)} differ: {', '.join(differ[:5])}", ""
    say(f"     {len(shas)} of {len(shas)} scenarios equal")
    return None, ""


def main(argv: list[str] | None = None) -> int:
    args_in = sys.argv[1:] if argv is None else argv
    if args_in[:2] == ["exec-no-aot", "--"] and len(args_in) > 2:
        cmd = args_in[2:]
        os.execvpe(cmd[0], cmd, no_aot_env())
    parser = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    parser.add_argument("step", choices=sorted(TIER_STEPS))
    parser.add_argument(
        "--all-no-aot",
        action="store_true",
        help="every scenario (tier-sweep) or every seventh (tier-quick) instead of NO_AOT_SUBSET",
    )
    parser.add_argument("--out", type=Path, help="default out/runs/fx-tier/<step>")
    parser.add_argument(
        "--jobs", type=int, help="coupled processes at once (default tier_jobs_budget)"
    )
    args = parser.parse_args(argv)
    out = args.out or ROOT / "out" / "runs" / "fx-tier" / args.step
    out.mkdir(parents=True, exist_ok=True)
    jobs = args.jobs if args.jobs is not None else tier_jobs_budget()
    problem, text = run(args.step, out, args.all_no_aot, jobs, dict(os.environ))
    if text:
        print(text, end="" if text.endswith("\n") else "\n")
    if problem is not None:
        print(f"FAIL {args.step}: {problem}")
        return 1
    print(f"PASS {args.step}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
