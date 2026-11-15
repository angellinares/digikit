"""Every tracked path can be checked out on Windows.

Windows refuses a file or folder named CON, PRN, AUX, NUL, COM1-COM9 or LPT1-LPT9, with any extension and in any case
(`aux.rs` too), a name that ends in a dot or a space, and the characters < > : " | ? * and backslash. Git for Windows then
stops the whole checkout ("invalid path"), so one such file blocks every Windows clone of the repository.
"""

from __future__ import annotations

import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

RESERVED = re.compile(r"^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\..*)?$", re.IGNORECASE)
BAD_CHARS = re.compile(r'[<>:"|?*\\]')


def windows_problem(name: str) -> str | None:
    """Why Windows refuses this one path component, or None."""
    if RESERVED.match(name):
        return "reserved device name"
    if name.endswith((".", " ")):
        return "ends in a dot or a space"
    if BAD_CHARS.search(name):
        return "character Windows does not allow"
    return None


def test_windows_problem_rules() -> None:
    for bad in [
        "aux.rs",
        "AUX",
        "con",
        "nul.txt",
        "Com1.log",
        "lpt9",
        "x.",
        "x ",
        "a:b",
        "q?",
    ]:
        assert windows_problem(bad), bad
    for good in [
        "auxiliary.rs",
        "aux_task.rs",
        "console.py",
        "com10",
        "lpt",
        "nullable.rs",
        "a.b",
    ]:
        assert windows_problem(good) is None, good


def test_tracked_paths_check_out_on_windows() -> None:
    out = subprocess.run(
        ["git", "ls-files", "-z"], cwd=ROOT, capture_output=True, check=True
    ).stdout.decode("utf-8")
    bad = []
    for path in filter(None, out.split("\0")):
        for part in path.split("/"):
            why = windows_problem(part)
            if why:
                bad.append(f"{path}: {why}")
                break
    assert not bad, "rename these paths, Windows cannot check them out:\n" + "\n".join(
        bad
    )
