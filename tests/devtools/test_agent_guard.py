"""tools/devtools/agent_guard.py: the commands the Bash hook blocks and the ones it lets through."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]

from tools import python_env  # noqa: E402
from tools.devtools import agent_guard  # noqa: E402

BLOCKED = [
    "sleep 120",
    "sleep 2m",
    "sleep 61",
    "sleep 58; sleep 58; tail -3 /tmp/x",
    "sleep 30 && sleep 40",
    "cd /tmp && sleep 300; tail -n 5 x",
    "uv run pytest tests/patchkit",
    "uv run python -m pytest -q tests/digikit",
    "uv run python -m pytest tests/patchkit/",
    "pytest -x tests/devtools",
    "uv run python -m tools.devtools.test_outcomes run --out out/runs/x tests/patchkit",
    "mise run test:full",
    "pnpm nx:pilot run-many -t tier-sweep && mise run test:full",
    "mise run scenario -- --tier full",
    "uv run python -m tools.x --tier=full",
    "tail -f out/runs/x.log",
    "tail -n 20 -F x",
]

ALLOWED = [
    "sleep 30",
    "sleep 60",
    "sleep 0.5",
    "for i in $(seq 12); do grep -q DONE f && break; sleep 5; done; tail -n 5 f",
    "sleep 20; sleep 20",
    "uv run pytest tests/patchkit/test_patchkit.py",
    "uv run pytest tests/patchkit/test_patchkit.py::test_x -k 'a or b'",
    "uv run python -m tools.devtools.test_outcomes run --out out/runs/x tests/devtools/test_layercheck.py",
    "mise run test",
    "pnpm nx:pilot show projects --affected --base main",
    "mise run test:changed",
    "tail -n 5 out/runs/x.log",
    "echo pytest tests",
    "git log --oneline",
    "uv run pytest --rootdir tests tests/devtools/test_layercheck.py",
    "ls tests/patchkit",
    'git commit -m "the hook blocks --tier full, test:full and sleep 300"',
    "echo 'tail -f x'",
    'git commit -m "a \\"quoted\\" sleep 120"',
]


@pytest.mark.parametrize("cmd", BLOCKED)
def test_blocked(cmd: str) -> None:
    assert agent_guard.check(cmd, ROOT) is not None


@pytest.mark.parametrize("cmd", ALLOWED)
def test_allowed(cmd: str) -> None:
    assert agent_guard.check(cmd, ROOT) is None


def run_hook(stdin: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-m", "tools.devtools.agent_guard"],
        input=stdin,
        capture_output=True,
        text=True,
        cwd=ROOT,
        env=python_env(),
        check=False,
    )


def event(command: str, tool: str = "Bash") -> str:
    return json.dumps(
        {"tool_name": tool, "tool_input": {"command": command}, "cwd": str(ROOT)}
    )


def test_main_blocks_with_exit_2() -> None:
    r = run_hook(event("sleep 600"))
    assert r.returncode == 2
    assert "agent_guard" in r.stderr


def test_main_passes() -> None:
    assert run_hook(event("DIGI_GUARD=off sleep 600")).returncode == 0
    assert run_hook(event("sleep 600", tool="Read")).returncode == 0
    assert run_hook("not json").returncode == 0
