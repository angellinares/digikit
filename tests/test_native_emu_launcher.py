"""The launcher restores local DN2 audio by default, with CF-only escape hatches."""

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


def test_default_ready_inputs_select_live_coupled_audio(monkeypatch, tmp_path):
    scope = launcher()
    globals_ = scope["default_audio_args"].__globals__
    ready = tmp_path / "snapshots" / "dn2-audio-ready-2026-10-03"
    ready.mkdir(parents=True)
    for name in [
        "Digitone_II_OS1.11.syx",
        "digi-audio-dn2-image.bin",
        "digi-audio-m5.snap.dsp",
        "digi-audio-m5.snap",
    ]:
        (tmp_path / name if name.endswith(".syx") else ready / name).touch()
    generated = tmp_path / "generated"
    generated.mkdir()
    monkeypatch.setitem(globals_, "ROOT", tmp_path)
    monkeypatch.setitem(globals_, "READY", ready)
    monkeypatch.setitem(globals_, "DEFAULT_GENERATED", generated)
    monkeypatch.delenv("SHARC_GEN_DIR", raising=False)
    args = scope["default_audio_args"]()
    assert args[1] == "--auto-coupled"
    assert args[-2:] == ["--audio-buffer", "0"]
    assert Path(__import__("os").environ["SHARC_GEN_DIR"]) == generated


def test_default_no_audio_keeps_coupling_but_mutes_playback(monkeypatch, tmp_path):
    scope = launcher()
    globals_ = scope["default_audio_args"].__globals__
    ready = tmp_path / "ready"
    ready.mkdir()
    for name in ["digi-audio-dn2-image.bin", "digi-audio-m5.snap.dsp", "digi-audio-m5.snap"]:
        (ready / name).touch()
    (tmp_path / "Digitone_II_OS1.11.syx").touch()
    generated = tmp_path / "generated"
    generated.mkdir()
    monkeypatch.setitem(globals_, "ROOT", tmp_path)
    monkeypatch.setitem(globals_, "READY", ready)
    monkeypatch.setitem(globals_, "DEFAULT_GENERATED", generated)
    monkeypatch.delenv("SHARC_GEN_DIR", raising=False)
    assert scope["default_audio_args"](["--no-audio"])[-1] == "0"


def test_selected_firmware_keeps_its_path_but_profile_stays_known(monkeypatch, tmp_path):
    scope = launcher()
    globals_ = scope["default_audio_args"].__globals__
    ready = tmp_path / "ready"
    ready.mkdir()
    for name in ["digi-audio-dn2-image.bin", "digi-audio-m5.snap.dsp", "digi-audio-m5.snap"]:
        (ready / name).touch()
    profile = tmp_path / "Digitone_II_OS1.11.syx"
    profile.touch()
    generated = tmp_path / "generated"
    generated.mkdir()
    monkeypatch.setitem(globals_, "ROOT", tmp_path)
    monkeypatch.setitem(globals_, "READY", ready)
    monkeypatch.setitem(globals_, "DEFAULT_GENERATED", generated)
    monkeypatch.delenv("SHARC_GEN_DIR", raising=False)
    args = scope["default_audio_args"](["another.syx", "--card-image", "card.img"])
    assert args[:3] == ["another.syx", "--card-image", "card.img"]
    assert args[args.index("--audio-profile-syx") + 1] == str(profile)


def test_normal_launcher_arguments_get_audio_without_replacing_firmware(monkeypatch, tmp_path):
    scope = launcher()
    globals_ = scope["default_audio_args"].__globals__
    ready = tmp_path / "ready"
    ready.mkdir()
    for name in ["digi-audio-dn2-image.bin", "digi-audio-m5.snap.dsp", "digi-audio-m5.snap"]:
        (ready / name).touch()
    profile = tmp_path / "Digitone_II_OS1.11.syx"
    profile.touch()
    generated = tmp_path / "generated"
    generated.mkdir()
    monkeypatch.setitem(globals_, "ROOT", tmp_path)
    monkeypatch.setitem(globals_, "READY", ready)
    monkeypatch.setitem(globals_, "DEFAULT_GENERATED", generated)
    monkeypatch.delenv("SHARC_GEN_DIR", raising=False)
    calls = []
    monkeypatch.setattr("subprocess.run", lambda command, **_kwargs: calls.append(command))
    scope["main"](["selected.syx", "--card-image", "card.img"])
    command = calls[-1]
    forwarded = command[command.index("--") + 1 :]
    assert forwarded[:3] == ["selected.syx", "--card-image", "card.img"]
    assert "--auto-coupled" in forwarded
    assert forwarded[forwarded.index("--audio-profile-syx") + 1] == str(profile)


def test_coupled_command_selects_local_feature_and_preserves_arguments():
    args = ["example.syx", "--coupled", "--dsp-image", "image.bin"]
    command = launcher()["cargo_command"](args)
    assert command[command.index("--features") + 1] == "coupled-audio"
    assert command[command.index("--") + 1 :] == args


def test_invalid_configured_core_fails_before_build(monkeypatch):
    monkeypatch.setenv("SHARC_GEN_DIR", "/missing-generated-core")
    calls = []
    monkeypatch.setattr("subprocess.run", lambda *args, **kwargs: calls.append(args))
    with pytest.raises(SystemExit, match="SHARC_GEN_DIR"):
        launcher()["main"](["example.syx", "--coupled"])
    assert calls == []
