"""Readers-writer lock for builds and timed runs (out/.benchlock).

Heavy work (cargo builds, training runs) holds the lock shared: many at once.
Timed runs hold it exclusive: they wait for all heavy work to end, and new
heavy work waits until they finish. Each holder writes a sidecar
`<lock>.d/<pid>.<n>.json`; `status` lists live holders and removes stale ones.

    with benchlock.hold("exclusive", "pgo bench") as h: ...
    uv run python tools/benchlock.py status
    uv run python tools/benchlock.py run --shared --label build -- cargo build

Env: BENCHLOCK=0 disables (no-op); BENCHLOCK_PATH overrides the lock file.
"""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import itertools
import json
import os
import subprocess
import sys
import time
from collections.abc import Iterator
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_PATH = ROOT / "out" / ".benchlock"
POLL_S = 0.05
_counter = itertools.count()


class LockTimeout(TimeoutError):
    pass


def lock_path(path: str | os.PathLike[str] | None = None) -> Path:
    if path is not None:
        return Path(path)
    return Path(os.environ.get("BENCHLOCK_PATH") or DEFAULT_PATH)


def disabled() -> bool:
    return os.environ.get("BENCHLOCK") == "0"


def _sidecar_dir(path: Path) -> Path:
    return path.with_name(path.name + ".d")


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def holders(
    path: str | os.PathLike[str] | None = None, clean: bool = True
) -> list[dict]:
    """Live holders from the sidecars; entries of dead pids are removed."""
    sdir = _sidecar_dir(lock_path(path))
    found: list[dict] = []
    if not sdir.is_dir():
        return found
    for entry in sorted(sdir.glob("*.json")):
        try:
            info = json.loads(entry.read_text())
            pid = int(info["pid"])
        except (OSError, ValueError, KeyError):
            continue
        if _alive(pid):
            found.append(info)
        elif clean:
            with contextlib.suppress(OSError):
                entry.unlink()
    return found


def _describe(infos: list[dict]) -> str:
    if not infos:
        return "unknown"
    return ", ".join(
        f"pid {i['pid']} {i['mode']} '{i['label']}' {time.time() - i['start']:.0f}s"
        for i in infos
    )


class Hold:
    def __init__(self, mode: str, label: str) -> None:
        self.mode = mode
        self.label = label
        self.waited = 0.0


@contextlib.contextmanager
def hold(
    mode: str,
    label: str,
    timeout: float | None = None,
    path: str | os.PathLike[str] | None = None,
) -> Iterator[Hold]:
    if mode not in ("shared", "exclusive"):
        raise ValueError(f"mode must be shared or exclusive, not {mode!r}")
    h = Hold(mode, label)
    if disabled():
        yield h
        return
    lpath = lock_path(path)
    lpath.parent.mkdir(parents=True, exist_ok=True)
    flag = fcntl.LOCK_SH if mode == "shared" else fcntl.LOCK_EX
    start = time.monotonic()
    announced = False
    with open(lpath, "a+") as handle:
        while True:
            try:
                fcntl.flock(handle, flag | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                pass
            if not announced:
                announced = True
                print(
                    f"benchlock: waiting for {mode} "
                    f"(held by: {_describe(holders(lpath))})",
                    file=sys.stderr,
                    flush=True,
                )
            if timeout is not None and time.monotonic() - start >= timeout:
                raise LockTimeout(f"benchlock: {mode} not acquired in {timeout}s")
            time.sleep(POLL_S)
        h.waited = time.monotonic() - start
        if announced:
            print(
                f"benchlock: acquired {mode} '{label}' after {h.waited:.1f}s",
                file=sys.stderr,
                flush=True,
            )
        sdir = _sidecar_dir(lpath)
        sdir.mkdir(parents=True, exist_ok=True)
        side = sdir / f"{os.getpid()}.{next(_counter)}.json"
        info = {"pid": os.getpid(), "mode": mode, "label": label, "start": time.time()}
        side.write_text(json.dumps(info))
        try:
            yield h
        finally:
            with contextlib.suppress(OSError):
                side.unlink()
            fcntl.flock(handle, fcntl.LOCK_UN)


def cmd_status(args: argparse.Namespace) -> int:
    if disabled():
        print("benchlock: disabled (BENCHLOCK=0)")
    infos = holders()
    if not infos:
        print("benchlock: free")
    for i in infos:
        print(
            f"{i['mode']:9} pid {i['pid']:<7} {time.time() - i['start']:6.0f}s  {i['label']}"
        )
    return 0


def cmd_run(args: argparse.Namespace) -> int:
    command = args.command
    if command and command[0] == "--":
        command = command[1:]
    if not command:
        print("benchlock run: no command", file=sys.stderr)
        return 2
    mode = "exclusive" if args.exclusive else "shared"
    with hold(mode, args.label or " ".join(command)[:60], timeout=args.timeout):
        return subprocess.run(command).returncode


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    sub = parser.add_subparsers(dest="cmd", required=True)
    sub.add_parser("status", help="list live holders").set_defaults(func=cmd_status)
    run = sub.add_parser("run", help="run a command under the lock")
    group = run.add_mutually_exclusive_group(required=True)
    group.add_argument("--shared", action="store_true")
    group.add_argument("--exclusive", action="store_true")
    run.add_argument("--label", default="")
    run.add_argument("--timeout", type=float, default=None)
    run.add_argument("command", nargs=argparse.REMAINDER)
    run.set_defaults(func=cmd_run)
    args = parser.parse_args(argv)
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
