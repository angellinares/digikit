"""benchlock: readers-writer lock for builds (shared) and timed runs (exclusive)."""
# ruff: noqa: SIM117

import importlib.util
import json
import os
import subprocess
import sys
import threading
from pathlib import Path

import pytest

TOOL = str(Path(__file__).resolve().parents[1] / "tools" / "benchlock.py")
_spec = importlib.util.spec_from_file_location("benchlock", TOOL)
assert _spec and _spec.loader
benchlock = importlib.util.module_from_spec(_spec)
sys.modules["benchlock"] = benchlock
_spec.loader.exec_module(benchlock)


@pytest.fixture
def lock(tmp_path, monkeypatch):
    path = tmp_path / ".benchlock"
    monkeypatch.setenv("BENCHLOCK_PATH", str(path))
    monkeypatch.delenv("BENCHLOCK", raising=False)
    return path


def test_shared_shared_concurrent(lock):
    with benchlock.hold("shared", "a", timeout=1):
        with benchlock.hold("shared", "b", timeout=1):
            assert len(benchlock.holders()) == 2
    assert benchlock.holders() == []


def test_exclusive_blocks_shared_and_back(lock):
    with benchlock.hold("exclusive", "bench"):
        with pytest.raises(benchlock.LockTimeout):
            with benchlock.hold("shared", "build", timeout=0.2):
                pass
        with pytest.raises(benchlock.LockTimeout):
            with benchlock.hold("exclusive", "bench2", timeout=0.2):
                pass
    with benchlock.hold("shared", "build"):
        with pytest.raises(benchlock.LockTimeout):
            with benchlock.hold("exclusive", "bench", timeout=0.2):
                pass


def test_waiter_proceeds_after_release(lock, capsys):
    got = []

    def waiter():
        with benchlock.hold("exclusive", "bench", timeout=5) as h:
            got.append(h.waited)

    with benchlock.hold("shared", "build"):
        thread = threading.Thread(target=waiter)
        thread.start()
        thread.join(0.3)
        assert thread.is_alive() and not got
    thread.join(5)
    assert got and got[0] >= 0.25
    err = capsys.readouterr().err
    assert "waiting for exclusive" in err and "build" in err
    assert "acquired exclusive" in err


def test_bad_mode(lock):
    with pytest.raises(ValueError):
        with benchlock.hold("both", "x"):
            pass


def test_stale_sidecar_cleanup(lock):
    sdir = lock.with_name(lock.name + ".d")
    sdir.mkdir()
    proc = subprocess.Popen([sys.executable, "-c", "pass"])
    proc.wait()
    stale = sdir / f"{proc.pid}.0.json"
    info = {"pid": proc.pid, "mode": "shared", "label": "dead", "start": 0}
    stale.write_text(json.dumps(info))
    live = sdir / f"{os.getpid()}.99.json"
    live.write_text(json.dumps({**info, "pid": os.getpid(), "label": "live"}))
    out = subprocess.run(
        [sys.executable, TOOL, "status"], capture_output=True, text=True, check=True
    ).stdout
    assert "live" in out and "dead" not in out
    assert not stale.exists() and live.exists()


def test_disabled_is_noop(lock, monkeypatch):
    monkeypatch.setenv("BENCHLOCK", "0")
    with benchlock.hold("exclusive", "a", timeout=0.1):
        with benchlock.hold("exclusive", "b", timeout=0.1):
            pass
    assert not lock.exists()


def test_cli_run_exit_code_and_blocking(lock):
    ok = subprocess.run([sys.executable, TOOL, "run", "--shared", "--", "true"])
    assert ok.returncode == 0
    bad = subprocess.run(
        [
            sys.executable,
            TOOL,
            "run",
            "--exclusive",
            "--label",
            "x",
            "--",
            "sh",
            "-c",
            "exit 7",
        ]
    )
    assert bad.returncode == 7
    with benchlock.hold("shared", "build"):
        blocked = subprocess.run(
            [
                sys.executable,
                TOOL,
                "run",
                "--exclusive",
                "--timeout",
                "0.3",
                "--",
                "true",
            ],
            capture_output=True,
            text=True,
        )
        assert blocked.returncode != 0
        assert "waiting for exclusive" in blocked.stderr
