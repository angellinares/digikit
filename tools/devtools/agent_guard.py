"""A Claude Code PreToolUse hook for the Bash tool: blocks the commands that cost agents the most time.

Blocked (docs/TESTING.md, AGENTS.md "Verify"): pytest on a test directory, `--tier full`, `gate:full`, `sleep` that adds up to over 60 s and `tail -f`. A command that starts with `DIGI_GUARD=off `
passes (the coordinator, deliberately). Registered in .claude/settings.json. Stdlib only.

The hook reads one JSON object from stdin. Exit 2 with a reason on stderr blocks the command; exit 0 lets it
run. Malformed input never blocks (a hook bug must not stop work).
Text inside quotes (commit messages, echo) is not checked; `bash -c '...'` is therefore not checked either.
"""

from __future__ import annotations

import json
import re
import shlex
import sys
from pathlib import Path

SLEEP_RE = re.compile(r"(?<![\w-])sleep\s+(\d+(?:\.\d+)?)([smhd]?)(?![\w.])")
SLEEP_UNIT = {"": 1, "s": 1, "m": 60, "h": 3600, "d": 86400}
SLEEP_LIMIT_S = 60

TAIL_FOLLOW_RE = re.compile(
    r"\btail\b[^|;&\n]*\s(?:-[a-zA-Z]*[fF][a-zA-Z]*|--follow\S*)(?=\s|$)"
)
TIER_FULL_RE = re.compile(r"--tier[ =]full\b")
GATE_FULL_RE = re.compile(r"\bgate:full\b")
QUOTED_RE = re.compile(r"'[^']*'|\"(?:[^\"\\\\]|\\\\.)*\"")
SEGMENT_SPLIT_RE = re.compile(r"\s*(?:&&|\|\||[;|\n])\s*")
GUARD_OFF_RE = re.compile(r"^\s*DIGI_GUARD=off\s")

# Options of pytest and of the test runners that take a separate value; the value is not a test path.
VALUE_OPTS = {
    "-k",
    "-m",
    "-p",
    "-c",
    "-o",
    "-n",
    "-W",
    "--rootdir",
    "--basetemp",
    "--confcutdir",
    "--junitxml",
    "--junit-xml",
    "--deselect",
    "--ignore",
    "--ignore-glob",
    "--override-ini",
    "--durations",
    "--maxfail",
    "--timeout",
    "--out",
    "--root",
    "--jobs",
    "--pytest-arg",
    "--log-file",
    "--cov",
    "--cov-report",
    "-r",
}


def runner_args(tokens: list[str]) -> list[str] | None:
    """The tokens after the test runner in one command segment, or None when it runs no test runner."""
    for i, tok in enumerate(tokens):
        prev = tokens[i - 1] if i else ""
        if tok.rsplit("/", 1)[-1] in ("pytest", "py.test") and (
            i == 0 or prev in ("-m", "run", "exec")
        ):
            return tokens[i + 1 :]
        if (
            tok == "tools.devtools.test_outcomes"
            and prev == "-m"
            and tokens[i + 1 : i + 2] == ["run"]
        ):
            return tokens[i + 2 :]
    return None


def unquoted(command: str) -> str:
    return QUOTED_RE.sub("''", command)


def check(command: str, cwd: Path) -> str | None:
    """The reason to block `command`, or None. The first match wins."""
    bare = unquoted(command)
    total = 0.0
    for m in SLEEP_RE.finditer(bare):
        seconds = float(m.group(1)) * SLEEP_UNIT[m.group(2)]
        if seconds > SLEEP_LIMIT_S:
            return (
                f"sleep {m.group(1)}{m.group(2)} is over 60 s: run the command in the background and wait "
                "for its completion notification, or poll with a bound (sleep 60 or less, then tail -n)"
            )
        total += seconds
    if total > SLEEP_LIMIT_S:
        return (
            f"sleep adds up to {total:g} s (over 60 s): poll with an exit condition instead, e.g. "
            "for i in $(seq 12); do grep -q DONE FILE && break; sleep 5; done; tail -n 5 FILE, "
            "or run the command in the background and wait for its completion notification"
        )
    if TAIL_FOLLOW_RE.search(bare):
        return "tail -f never exits: poll with a bound (sleep 60 or less, then tail -n)"
    if TIER_FULL_RE.search(bare):
        return "--tier full runs only before a merge to main; iterate on the quick tier"
    if GATE_FULL_RE.search(bare):
        return (
            "gate:full runs only after generated-core changes, by the coordinator; "
            "lanes run mise run test:changed, the coordinator mise run gate"
        )
    for segment in SEGMENT_SPLIT_RE.split(command):
        try:
            tokens = shlex.split(segment)
        except ValueError:
            continue
        # Leading NAME=value assignments are not part of the command.
        while tokens and re.match(r"[A-Za-z_]\w*=", tokens[0]):
            tokens.pop(0)
        if not tokens:
            continue
        if tokens[0] == "cd" and len(tokens) > 1:
            cwd = cwd / tokens[1]  # best effort; an absolute DIR replaces cwd
            continue
        args = runner_args(tokens)
        if args is None:
            continue
        skip = False
        for arg in args:
            if skip:
                skip = False
                continue
            if arg.startswith("-"):
                skip = arg in VALUE_OPTS
                continue
            path = arg.split("::", 1)[0]
            if (cwd / path).is_dir():
                return (
                    f"pytest on a directory ({arg}) runs every file in it: "
                    "run mise run test:changed, or name the test files"
                )
    return None


def main() -> int:
    try:
        event = json.load(sys.stdin)
        if event.get("tool_name") != "Bash":
            return 0
        command = event.get("tool_input", {}).get("command")
        if not isinstance(command, str) or not command:
            return 0
        cwd = Path(event.get("cwd") or Path.cwd())
    except (ValueError, AttributeError):
        return 0
    if GUARD_OFF_RE.match(command):
        return 0
    reason = check(command, cwd)
    if reason:
        print(
            f"agent_guard: {reason} (prefix DIGI_GUARD=off only if the coordinator really means it)",
            file=sys.stderr,
        )
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
