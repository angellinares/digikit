"""Run a cargo-built binary without racing the next `cargo build`.

Every `cargo build` replaces `target/release/<bin>` with a fresh copy, even when nothing was compiled (new inode
each time; measured 2026-10-18). A process that execs that path while another cargo rewrites it can sit in
`_dyld_start` at 0 % CPU for ever, or be killed (macOS code-signing); with 3 builders and 6 runners it happened
2 times in 1000 runs. `mise run gate` runs many test files at once and each one built patchkit first.

The fix: build under one lock (`build_lock`) and run an immutable copy named by the content hash
(`stable_copy`, written to a temp name and linked into place, never overwritten). cargo may rewrite
`release/<bin>` as often as it likes: nobody executes it.
"""

from __future__ import annotations

import contextlib
import hashlib
import os
import pathlib
import shutil
import time
from collections.abc import Iterator

from tools.devtools import portable_flock as fcntl

KEEP_SECONDS = 24 * 3600


@contextlib.contextmanager
def build_lock(target: pathlib.Path) -> Iterator[None]:
    """An exclusive inter-process lock for building into (and copying out of) `target`."""
    target.mkdir(parents=True, exist_ok=True)
    with open(
        target / ".stablebin.lock", "a"
    ) as handle:  # "a": never truncate a locked file
        fcntl.flock(handle, fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(handle, fcntl.LOCK_UN)


def stable_copy(
    binary: pathlib.Path, target: pathlib.Path | None = None
) -> pathlib.Path:
    """`binary` copied to `<target>/stable/<name>-<sha12>` once; the copy is never modified.

    Call it under `build_lock` (a concurrent cargo could be rewriting `binary`). Copies of other
    content older than a day are removed (unlinking a running binary is safe).
    """
    target = target if target is not None else binary.parent.parent
    data = binary.read_bytes()
    digest = hashlib.sha256(data).hexdigest()[:12]
    folder = target / "stable"
    folder.mkdir(parents=True, exist_ok=True)
    final = folder / f"{binary.name}-{digest}"
    if not final.is_file():
        partial = folder / f".{final.name}.{os.getpid()}.part"
        shutil.copyfile(binary, partial)
        partial.chmod(0o755)
        try:
            os.link(partial, final)  # fails when a racer won: its file stays untouched
        except FileExistsError:
            pass
        finally:
            partial.unlink(missing_ok=True)
    now = time.time()
    for other in folder.glob(f"{binary.name}-*"):
        with contextlib.suppress(OSError):
            if other != final and now - other.stat().st_mtime > KEEP_SECONDS:
                other.unlink()
    return final
