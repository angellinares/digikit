"""Remove old device-test run directories from the temp dir (dry run unless --yes).

    uv run python -m tools.devtools.clean_runs [--hours 24] [--yes]

Only directories directly under the temp dir whose name starts with `digi-` and whose mtime is older than
`--hours` are candidates. A directory is skipped when a running `digi-emud` or node process names it in its
command line or has it as working directory. Nothing is killed: orphan `digi-emud` processes (parent PID 1) are
only listed, so the user can decide.
"""

from __future__ import annotations

import argparse
import contextlib
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path

PREFIX = "digi-"


@dataclass(frozen=True)
class Proc:
    pid: int
    ppid: int
    age_s: int
    args: str
    cwd: str = ""


def _etime_seconds(text: str) -> int:
    """`ps` etime: [[dd-]hh:]mm:ss."""
    days, _, rest = text.rpartition("-")
    parts = [int(p) for p in rest.split(":")]
    while len(parts) < 3:
        parts.insert(0, 0)
    h, m, s = parts
    return (int(days or 0) * 24 + h) * 3600 + m * 60 + s


def _is_relevant(args: str) -> bool:
    return "digi-emud" in args or re.search(r"(^|/)node(\s|$)", args) is not None


def list_procs() -> list[Proc]:
    out = subprocess.run(
        ["ps", "-axo", "pid=,ppid=,etime=,args="],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    procs: list[Proc] = []
    for line in out.splitlines():
        fields = line.split(None, 3)
        if len(fields) < 4:
            continue
        try:
            procs.append(
                Proc(
                    int(fields[0]), int(fields[1]), _etime_seconds(fields[2]), fields[3]
                )
            )
        except ValueError:
            continue
    relevant = [p for p in procs if _is_relevant(p.args)]
    cwds = _cwds([p.pid for p in relevant])
    return [Proc(p.pid, p.ppid, p.age_s, p.args, cwds.get(p.pid, "")) for p in procs]


def _cwds(pids: list[int]) -> dict[int, str]:
    if not pids:
        return {}
    try:
        out = subprocess.run(
            ["lsof", "-a", "-d", "cwd", "-Fpn", "-p", ",".join(map(str, pids))],
            capture_output=True,
            text=True,
            check=False,
        ).stdout
    except OSError:
        return {}
    cwds: dict[int, str] = {}
    pid = 0
    for line in out.splitlines():
        if line.startswith("p"):
            pid = int(line[1:])
        elif line.startswith("n") and pid:
            cwds[pid] = line[1:]
    return cwds


def in_use(path: Path, procs: list[Proc]) -> bool:
    """A digi-emud or node process names the directory in its command line or runs inside it."""
    text = str(path)
    for p in procs:
        if not _is_relevant(p.args):
            continue
        if text in p.args or p.cwd == text or p.cwd.startswith(text + os.sep):
            return True
    return False


def candidates(
    root: Path, hours: float, procs: list[Proc], now: float | None = None
) -> list[Path]:
    """Directories that may be removed. Anything that resolves outside `root` is refused."""
    now = time.time() if now is None else now
    real_root = root.resolve()
    found: list[Path] = []
    for entry in sorted(root.iterdir()):
        if (
            not entry.name.startswith(PREFIX)
            or entry.is_symlink()
            or not entry.is_dir()
        ):
            continue
        real = entry.resolve()
        if real.parent != real_root:
            continue
        if now - entry.stat().st_mtime < hours * 3600:
            continue
        if in_use(real, procs) or in_use(entry, procs):
            continue
        found.append(real)
    return found


def orphans(procs: list[Proc]) -> list[Proc]:
    return [p for p in procs if p.ppid == 1 and "digi-emud" in p.args]


def tree_bytes(path: Path) -> int:
    """Allocated bytes (st_blocks); APFS clones count as their full size, as `du` does."""
    total = 0
    for dirpath, _dirs, files in os.walk(path):
        for name in files:
            with contextlib.suppress(OSError):
                total += os.lstat(os.path.join(dirpath, name)).st_blocks * 512
    return total


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument(
        "--hours", type=float, default=24, help="minimum age by mtime (default 24)"
    )
    ap.add_argument("--yes", action="store_true", help="delete; without it only list")
    ap.add_argument(
        "--root", type=Path, default=Path(tempfile.gettempdir()), help=argparse.SUPPRESS
    )
    args = ap.parse_args(argv)

    procs = list_procs()
    found = candidates(args.root, args.hours, procs)
    total = 0
    for path in found:
        size = tree_bytes(path)
        total += size
        if args.yes:
            shutil.rmtree(path, ignore_errors=True)
        print(
            f"{'removed' if args.yes else 'would remove'} {path} ({size / 1e9:.2f} GB allocated)"
        )
    print(
        f"{len(found)} directories, {total / 1e9:.2f} GB allocated"
        + ("" if args.yes else " (dry run; --yes deletes)")
    )

    for p in orphans(procs):
        run_dir = next((w for w in p.args.split() if w.startswith(str(args.root))), "?")
        print(
            f"orphan digi-emud pid {p.pid} age {p.age_s // 3600} h {p.age_s % 3600 // 60} min run dir {run_dir} (not killed)"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
