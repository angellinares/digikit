#!/usr/bin/env python3
"""Every patchkit emulator proof on its quick tier, one after the other: one summary line per tool and a total.

    uv run python -m tools.patchkit.pk_proofs [--only voice,...] [--jobs N] [--out DIR]
    mise run patch:proofs

Each tool is `tools/patchkit/pk_proof_<name>.py --quick` (parts in parallel processes inside the tool, `--jobs`); the tools
run in sequence so that their timings do not disturb each other. Logs and the tools' output go under OUT (default
out/runs/proofs). Exit 0 only when every tool passes. The full tiers run only at integration (AGENTS.md).
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import time
from pathlib import Path

from tools import python_env

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent

# (label, script, extra arguments)
TOOLS = [
    ("voice dt2", "pk_proof_voice.py", []),
    ("voice dn2", "pk_proof_voice_dn2.py", []),
]


def selected(label: str, want: list[str] | None) -> bool:
    """Whether --only WANT (labels or first words) picks LABEL."""
    return not want or any(label == w or label.split()[0] == w for w in want)


def command(script: str, extra: list[str], out: Path, jobs: int) -> list[str]:
    """The quick-tier command of one tool; OUT is its output directory."""
    cmd = [
        sys.executable,
        "-u",
        "-m",
        f"tools.patchkit.{Path(script).stem}",
        *extra,
        "--quick",
    ]
    cmd += ["--out", str(out)]
    if jobs:
        cmd += ["--jobs", str(jobs)]
    return cmd


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument(
        "--only", help="comma-separated tool labels or first words (fx, voice, ...)"
    )
    ap.add_argument(
        "--jobs",
        type=int,
        default=0,
        help="passed to every tool (default: the tool's own)",
    )
    ap.add_argument("--out", type=Path, default=ROOT / "out" / "runs" / "proofs")
    a = ap.parse_args(argv)
    want = a.only.split(",") if a.only else None
    if want:
        unknown = [w for w in want if not any(selected(t[0], [w]) for t in TOOLS)]
        if unknown:
            labels = ", ".join(t[0] for t in TOOLS)
            print(
                f"pk_proofs: unknown --only names: {', '.join(unknown)} "
                f"(valid labels or first words: {labels})",
                file=sys.stderr,
            )
            return 2
    a.out.mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    failed: list[str] = []
    n = 0
    for label, script, extra in TOOLS:
        if not selected(label, want):
            continue
        n += 1
        slug = label.replace(" ", "-")
        cmd = command(script, extra, a.out / slug, a.jobs)
        t0 = time.monotonic()
        log = a.out / f"{slug}.log"
        with log.open("wb") as f:
            rc = subprocess.run(
                cmd, stdout=f, stderr=subprocess.STDOUT, cwd=ROOT, env=python_env()
            ).returncode
        wall = time.monotonic() - t0
        lines = log.read_text(errors="replace").strip().splitlines()
        bad = [
            ln.strip()
            for ln in lines
            if ln.startswith("     FAIL") or ln.strip().startswith("FAIL ")
        ]
        state = "PASS" if rc == 0 else "FAIL"
        note = "   usage error (rc 2)" if rc == 2 else ""
        print(f"{state}  {label:<13} {wall:>5.0f} s   {log}{note}", flush=True)
        for ln in bad[:6]:
            print(f"      {ln}", flush=True)
        if rc:
            failed.append(label)
    total = time.monotonic() - started
    print(
        f"{'PASS' if not failed else 'FAIL: ' + ', '.join(failed)}  patch:proofs: {n} tools, total {total:.0f} s",
        flush=True,
    )
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
