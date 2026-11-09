#!/usr/bin/env python3
"""Render and check the DT2 FX scenarios (tests/regression/scenarios/dt2-trackfx.ts, dt2-sendfx.ts).

    uv run python -m tools.emulator.dt2fx render [--out DIR] [--jobs N] [--env K=V ...] [NAME ...]
    uv run python -m tools.emulator.dt2fx check DIR [DIR ...]
    uv run python -m tools.emulator.dt2fx tiers DIR_A DIR_B

`render` runs both files on the one-sample card (tools/emulator/dt2_sample_check.py) with
the Sound and Fx arrays of track 1 peeked at the end of every scenario (finding
02), WAVs kept, into DIR/trackfx and DIR/sendfx (default out/runs/dt2fx/<stamp>).
The core is the one SHARC_GEN_DIR names (else the registered DT2 core).
`--env SHARC_AOT=0` renders the no-AOT configuration. `check` prints the
tools/emulator/fxcheck.py tables for rendered runs. `tiers` compares the PCM hashes of
two renders (default against no-AOT) scenario by scenario; exit 1 on a mismatch.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parents[2]
from tools.emulator import coupled, digi_env, fxcheck  # noqa: E402

FILES = ("trackfx", "sendfx")


def peeks() -> tuple[str, str]:
    fxcheck.configure("dt2")
    return fxcheck.SND_PEEK, fxcheck.FX_PEEK


def render(
    out: pathlib.Path, jobs: int, names: list[str], no_build: bool = True
) -> dict[str, dict]:
    coupled.enable_dt2_card()
    binary = coupled.ensure_binary(no_build, "dt2", pgo=True)
    reports = {}
    for kind in FILES:
        scn = ROOT / "tests" / "regression" / "scenarios" / f"dt2-{kind}.ts"
        todo = [
            sc["name"]
            for sc in coupled.scenario_document(scn)["scenarios"]
            if not names or sc["name"] in names
        ]
        if not todo:
            continue
        reports[kind] = coupled.run_batch(
            "dt2", scn, todo, out / kind, peeks=peeks(), binary=binary, jobs=jobs
        )
    return reports


def shas(run: pathlib.Path) -> dict[str, str]:
    out = {}
    for kind in FILES:
        f = run / kind / "report.json"
        if f.exists():
            for r in coupled.scenario_results(json.loads(f.read_text())):
                out[r["name"]] = r["sha256"]
    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("render")
    r.add_argument("names", nargs="*")
    r.add_argument("--out", type=pathlib.Path)
    r.add_argument("--jobs", type=int, default=coupled.default_jobs())
    r.add_argument("--env", action="append", default=[], metavar="K=V")
    r.add_argument("--build", action="store_true", help="build coupled-run first")
    c = sub.add_parser("check")
    c.add_argument("runs", nargs="+", type=pathlib.Path)
    t = sub.add_parser("tiers")
    t.add_argument("a", type=pathlib.Path)
    t.add_argument("b", type=pathlib.Path)
    a = ap.parse_args(argv)
    if a.cmd == "render":
        os.environ.update(kv.partition("=")[::2] for kv in a.env)
        out = a.out or digi_env.run_dir("dt2fx")
        start = time.monotonic()
        reports = render(out, a.jobs, a.names, not a.build)
        n = sum(len(r["scenarios"]) for r in reports.values())
        bad = [
            x["name"]
            for r in reports.values()
            for x in coupled.scenario_results(r)
            if x["ok"] is False
        ]
        print(
            f"{n} scenarios in {time.monotonic() - start:.0f} s -> {out}; failed gates: {bad}"
        )
        return 2 if bad else 0
    if a.cmd == "check":
        fxcheck.configure("dt2")
        dirs = [
            d / k for d in a.runs for k in FILES if (d / k / "report.json").exists()
        ]
        print(fxcheck.table(fxcheck.check(dirs, None, False, fxcheck.DEFAULT_IMAGE)))
        return 0
    x, y = shas(a.a), shas(a.b)
    differ = sorted(n for n in x if x[n] != y.get(n))
    missing = sorted(set(x) ^ set(y))
    print(
        f"{len(x) - len(differ)} of {len(x)} equal; differ: {differ}; only in one: {missing}"
    )
    return 1 if differ or missing else 0


if __name__ == "__main__":
    sys.exit(main())
