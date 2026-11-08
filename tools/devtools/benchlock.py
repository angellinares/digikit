"""Readers-writer lock for builds and timed runs (out/.benchlock).

Heavy work (cargo builds, training runs) holds the lock shared: many at once.
Timed runs hold it exclusive: they wait for all heavy work to end, and new
heavy work waits until they finish -- including heavy work that arrives while
an exclusive acquirer is still waiting (writer preference: a stream of shared
holders cannot starve it). A second file, `<lock>.gate`, is held while an
acquirer is getting the lock: shared takes it only for the moment of
acquisition, exclusive keeps it for its whole wait. A shared hold exports
BENCHLOCK_SHARED_HELD=1 to its children (and this process): a nested shared
acquisition under it skips the gate, since queueing behind an exclusive waiter
that is itself waiting for the outer hold would deadlock. Each holder writes a sidecar
`<lock>.d/<pid>.<n>.json`; `status` lists live holders and removes stale ones.

    with benchlock.hold("exclusive", "pgo bench") as h: ...
    uv run python -m tools.devtools.benchlock status
    uv run python -m tools.devtools.benchlock run --shared --label build -- cargo build

Env: BENCHLOCK=0 disables (no-op); BENCHLOCK_PATH overrides the lock file.
"""

from __future__ import annotations

import argparse
import contextlib
import itertools
import json
import os
import subprocess
import sys
import time
from collections.abc import Iterator
from pathlib import Path
from typing import IO

from tools.devtools import portable_flock as fcntl

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_PATH = ROOT / "out" / ".benchlock"
POLL_S = 0.05
HELD_ENV = "BENCHLOCK_SHARED_HELD"
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


def _gate_path(path: Path) -> Path:
    return path.with_name(path.name + ".gate")


def _alive(pid: int) -> bool:
    if os.name == "nt":
        # os.kill(pid, 0) on Windows sends CTRL_C_EVENT instead of testing the pid.
        return _alive_windows(pid)
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _alive_windows(pid: int) -> bool:
    # Windows only; the check also stops mypy on other platforms from checking the ctypes names below
    if sys.platform != "win32":
        raise OSError("_alive_windows runs on Windows only")
    import ctypes

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    handle = kernel32.OpenProcess(
        0x1000, False, pid
    )  # PROCESS_QUERY_LIMITED_INFORMATION
    if not handle:
        return ctypes.get_last_error() == 5  # ERROR_ACCESS_DENIED: it exists
    try:
        code = ctypes.c_ulong()
        if not kernel32.GetExitCodeProcess(handle, ctypes.byref(code)):
            return True
        return code.value == 259  # STILL_ACTIVE
    finally:
        kernel32.CloseHandle(handle)


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
    last_notice = start

    def acquire(fd: IO[str], how: int) -> None:
        """Take a flock, polling so the timeout and the notice work."""
        nonlocal announced, last_notice
        while True:
            try:
                fcntl.flock(fd, how | fcntl.LOCK_NB)
                return
            except BlockingIOError:
                pass
            now = time.monotonic()
            if not announced:
                # A holder writes its sidecar just after it takes the lock, so the
                # first poll can see no holder yet: wait up to 1 s for the sidecar.
                infos = holders(lpath)
                if infos or now - start >= 1.0:
                    announced = True
                    last_notice = now
                    print(
                        f"benchlock: waiting for {mode} (held by: {_describe(infos)})",
                        file=sys.stderr,
                        flush=True,
                    )
            elif now - last_notice >= 60.0:
                last_notice = now
                print(
                    f"benchlock: still waiting for {mode} after {now - start:.0f}s "
                    f"(held by: {_describe(holders(lpath))})",
                    file=sys.stderr,
                    flush=True,
                )
            if timeout is not None and time.monotonic() - start >= timeout:
                raise LockTimeout(f"benchlock: {mode} not acquired in {timeout}s")
            time.sleep(POLL_S)

    nested = mode == "shared" and os.environ.get(HELD_ENV) == "1"
    with open(lpath, "a+") as handle, open(_gate_path(lpath), "a+") as gate:
        # Gate first: an exclusive waiter keeps it until it holds the lock, so
        # new shared acquirers queue behind it instead of overtaking it.
        if not nested:
            acquire(gate, fcntl.LOCK_EX)
        try:
            acquire(handle, flag)
        finally:
            if not nested:
                fcntl.flock(gate, fcntl.LOCK_UN)
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
        previous = os.environ.get(HELD_ENV)
        if mode == "shared":
            os.environ[HELD_ENV] = "1"
        try:
            yield h
        finally:
            if mode == "shared":
                if previous is None:
                    os.environ.pop(HELD_ENV, None)
                else:
                    os.environ[HELD_ENV] = previous
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
