"""The default launcher remains CF-only; local audio is an explicit build choice."""

import runpy
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]


def launcher():
    return runpy.run_path(str(ROOT / "tools/native_emu.sh"), run_name="launcher_test")


def test_default_command_preserves_locked_cf_only_build():
    command = launcher()["cargo_command"](["example.syx", "--card-image", "card.img"])
    assert "--locked" in command
    assert "--features" not in command
    assert command[command.index("--") + 1 :] == [
        "example.syx",
        "--card-image",
        "card.img",
    ]


def test_coupled_command_selects_local_feature_and_preserves_arguments():
    args = ["example.syx", "--coupled", "--dsp-image", "image.bin"]
    command = launcher()["cargo_command"](args)
    assert command[command.index("--features") + 1] == "coupled-audio"
    assert command[command.index("--") + 1 :] == args


def test_missing_generated_core_fails_before_build(monkeypatch):
    monkeypatch.delenv("SHARC_GEN_DIR", raising=False)
    calls = []
    monkeypatch.setattr("subprocess.run", lambda *args, **kwargs: calls.append(args))
    with pytest.raises(SystemExit, match="SHARC_GEN_DIR"):
        launcher()["main"](["example.syx", "--coupled"])
    assert calls == []
