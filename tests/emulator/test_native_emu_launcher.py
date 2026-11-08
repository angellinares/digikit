"""The launcher restores local DN2 audio by default, with CF-only escape hatches."""

import os
import runpy
import subprocess
from pathlib import Path

import pytest
from support.paths import REPO_ROOT

ROOT = REPO_ROOT
from tools.emulator import digi_env  # noqa: E402


@pytest.fixture(autouse=True)
def pinned_rustc(monkeypatch):
    """Never ask rustup for the toolchain from these tests."""
    monkeypatch.setattr(digi_env, "toolchain_rustc", lambda: "/fake/bin/rustc")


@pytest.fixture(autouse=True)
def fresh_gen(monkeypatch):
    """The fake gen dirs here carry no CORE_FINGERPRINT."""
    monkeypatch.setenv("DIGI_ALLOW_STALE_GEN", "1")


@pytest.fixture(autouse=True)
def no_plus_drive_folder(monkeypatch, tmp_path_factory):
    """The repository's plus_drive/ never reaches these tests."""
    monkeypatch.setenv("DIGI_PLUS_DRIVE", str(tmp_path_factory.mktemp("no-plus-drive")))


@pytest.fixture(autouse=True)
def _restore_sharc_gen_dir(monkeypatch):
    """native_emu sets SHARC_GEN_DIR with os.environ.setdefault; record it so
    monkeypatch restores it after each test."""
    monkeypatch.setenv("SHARC_GEN_DIR", "x")
    monkeypatch.delenv("SHARC_GEN_DIR")


@pytest.fixture(autouse=True)
def _isolate_second_core(monkeypatch):
    """Single-core tests must not discover local cores or inherit a dual build."""
    monkeypatch.setenv("DIGI_EMU_SECOND_CORE", "0")
    monkeypatch.setenv("SHARC_GEN_DIR_2", "x")
    monkeypatch.delenv("SHARC_GEN_DIR_2")


def launcher():
    return runpy.run_path(
        str(ROOT / "tools/emulator/native_emu.py"), run_name="launcher_test"
    )


def test_default_ready_inputs_select_live_coupled_audio(monkeypatch, tmp_path):
    scope = launcher()
    ready = tmp_path / "snapshots" / "dn2-audio-ready-2026-10-03"
    ready.mkdir(parents=True)
    for name in [
        "Digitone_II_OS1.12.syx",
        "digi-audio-dn2-image.bin",
        "digi-audio-m5.snap.dsp",
        "digi-audio-m5.snap",
    ]:
        (tmp_path / name if name.endswith(".syx") else ready / name).touch()
    generated = tmp_path / "generated"
    generated.mkdir()
    monkeypatch.setattr(digi_env, "DEFAULT_SYX", tmp_path / "Digitone_II_OS1.12.syx")
    monkeypatch.setattr(digi_env, "READY", ready)
    monkeypatch.setattr(digi_env, "DEFAULT_GENERATED", generated)
    monkeypatch.delenv("SHARC_GEN_DIR", raising=False)
    args = scope["default_audio_args"]()
    assert args[1] == "--auto-coupled"
    assert args[-2:] == ["--audio-buffer", "0.3"]
    assert Path(__import__("os").environ["SHARC_GEN_DIR"]) == generated


def test_default_no_audio_keeps_coupling_but_mutes_playback(monkeypatch, tmp_path):
    scope = launcher()
    ready = tmp_path / "ready"
    ready.mkdir()
    for name in [
        "digi-audio-dn2-image.bin",
        "digi-audio-m5.snap.dsp",
        "digi-audio-m5.snap",
    ]:
        (ready / name).touch()
    (tmp_path / "Digitone_II_OS1.12.syx").touch()
    generated = tmp_path / "generated"
    generated.mkdir()
    monkeypatch.setattr(digi_env, "DEFAULT_SYX", tmp_path / "Digitone_II_OS1.12.syx")
    monkeypatch.setattr(digi_env, "READY", ready)
    monkeypatch.setattr(digi_env, "DEFAULT_GENERATED", generated)
    monkeypatch.delenv("SHARC_GEN_DIR", raising=False)
    assert scope["default_audio_args"](["--no-audio"])[-1] == "0.3"


def test_the_mirror_folder_is_not_taken_for_the_firmware(monkeypatch, tmp_path):
    scope = launcher()
    monkeypatch.setattr(digi_env, "DEFAULT_SYX", tmp_path / "Digitone_II_OS1.12.syx")
    syx_from_args = scope["syx_from_args"]
    assert syx_from_args(["--mirror-dir", "/m"]) == tmp_path / "Digitone_II_OS1.12.syx"
    assert syx_from_args(["--mirror-dir", "/m", "a.syx"]) == Path("a.syx")


def test_selected_firmware_keeps_its_path_but_profile_stays_known(
    monkeypatch, tmp_path
):
    scope = launcher()
    ready = tmp_path / "ready"
    ready.mkdir()
    for name in [
        "digi-audio-dn2-image.bin",
        "digi-audio-m5.snap.dsp",
        "digi-audio-m5.snap",
    ]:
        (ready / name).touch()
    profile = tmp_path / "Digitone_II_OS1.12.syx"
    profile.touch()
    generated = tmp_path / "generated"
    generated.mkdir()
    monkeypatch.setattr(digi_env, "DEFAULT_SYX", tmp_path / "Digitone_II_OS1.12.syx")
    monkeypatch.setattr(digi_env, "READY", ready)
    monkeypatch.setattr(digi_env, "DEFAULT_GENERATED", generated)
    monkeypatch.delenv("SHARC_GEN_DIR", raising=False)
    args = scope["default_audio_args"](["another.syx", "--card-image", "card.img"])
    assert args[:3] == ["another.syx", "--card-image", "card.img"]
    assert args[args.index("--audio-profile-syx") + 1] == str(profile)


def test_normal_launcher_arguments_get_audio_without_replacing_firmware(
    monkeypatch, tmp_path
):
    scope = launcher()
    ready = tmp_path / "ready"
    ready.mkdir()
    for name in [
        "digi-audio-dn2-image.bin",
        "digi-audio-m5.snap.dsp",
        "digi-audio-m5.snap",
    ]:
        (ready / name).touch()
    profile = tmp_path / "Digitone_II_OS1.12.syx"
    profile.touch()
    generated = tmp_path / "generated"
    generated.mkdir()
    monkeypatch.setattr(digi_env, "DEFAULT_SYX", tmp_path / "Digitone_II_OS1.12.syx")
    monkeypatch.setattr(digi_env, "READY", ready)
    monkeypatch.setattr(digi_env, "DEFAULT_GENERATED", generated)
    monkeypatch.delenv("SHARC_GEN_DIR", raising=False)
    monkeypatch.setenv("DIGI_EMU_PGO", "0")
    monkeypatch.setitem(scope["main"].__globals__, "build_panel", lambda environ: None)
    calls = []
    monkeypatch.setattr(
        "subprocess.run", lambda command, **_kwargs: calls.append(command)
    )
    scope["main"](["selected.syx", "--card-image", "card.img"])
    forwarded = calls[-1][1:]
    assert forwarded[:3] == ["selected.syx", "--card-image", "card.img"]
    assert "--auto-coupled" in forwarded
    assert forwarded[forwarded.index("--audio-profile-syx") + 1] == str(profile)


def test_invalid_configured_core_fails_before_build(monkeypatch):
    monkeypatch.setenv("SHARC_GEN_DIR", "/missing-generated-core")
    calls = []
    monkeypatch.setattr("subprocess.run", lambda *args, **kwargs: calls.append(args))
    with pytest.raises(SystemExit, match="SHARC_GEN_DIR"):
        launcher()["main"](["example.syx", "--coupled"])
    assert calls == []


def _set_table(monkeypatch, entries):
    """Make the tier config hold ENTRIES (digest -> entry) in place of config/emulator.json."""
    from tools.emulator import emulator_config

    config = {"_contexts": {"workbench": {"SHARC_FAST_AUTO": "4,2,0"}}, **entries}
    monkeypatch.setattr(digi_env, "tier_config", lambda: config)
    monkeypatch.setattr(digi_env, "fast_table", lambda: emulator_config.firmware_table(config))
    return entries


def _known_syx(tmp_path, scope, monkeypatch, content=b"dn2", **extra):
    syx = tmp_path / "fw.syx"
    syx.write_bytes(content)
    digest = __import__("hashlib").sha256(content).hexdigest()
    entry = {"fast_regions": "0x10,0x20", **extra}
    monkeypatch.setattr(digi_env, "_syx_digest", lambda path: digest if Path(path) == syx else "")
    _set_table(monkeypatch, {digest: entry})
    return syx


def test_fast_regions_follow_the_firmware_hash(monkeypatch, tmp_path):
    scope = launcher()
    syx = _known_syx(tmp_path, scope, monkeypatch)
    other = tmp_path / "other.syx"
    other.write_bytes(b"other")
    assert scope["fast_regions_for"](syx) == "0x10,0x20"
    assert scope["fast_regions_for"](other) == ""
    assert scope["fast_regions_for"](tmp_path / "missing.syx") == ""


def _known_roots(tmp_path, scope, monkeypatch, content=b"dn2"):
    return _known_syx(tmp_path, scope, monkeypatch, content, blocktrans="0x30,0x40")


def test_set_fast_regions_exports_the_cluster_roots(monkeypatch, tmp_path):
    scope = launcher()
    syx = _known_roots(tmp_path, scope, monkeypatch)
    args = ["--audio-profile-syx", str(syx)]
    environ: dict[str, str] = {}
    scope["set_fast_regions"](args, environ)
    assert environ["SHARC_BLOCKTRANS"] == "0x30,0x40"
    user = {"SHARC_BLOCKTRANS": "0x99"}
    scope["set_fast_regions"](args, user)
    assert user["SHARC_BLOCKTRANS"] == "0x99"
    assert user["SHARC_FAST_REGIONS"] == "0x10,0x20"
    off = {"DIGI_EMU_FAST": "0"}
    scope["set_fast_regions"](args, off)
    assert "SHARC_BLOCKTRANS" not in off
    other = tmp_path / "other.syx"
    other.write_bytes(b"other")
    unknown: dict[str, str] = {}
    scope["set_fast_regions"](["--audio-profile-syx", str(other)], unknown)
    assert unknown == {}


def test_set_fast_regions_enables_known_firmware(monkeypatch, tmp_path):
    scope = launcher()
    syx = _known_syx(tmp_path, scope, monkeypatch)
    environ: dict[str, str] = {}
    scope["set_fast_regions"](["--audio-profile-syx", str(syx)], environ)
    assert environ["SHARC_FAST_REGIONS"] == "0x10,0x20"


def test_set_fast_regions_respects_user_choices(monkeypatch, tmp_path):
    scope = launcher()
    syx = _known_syx(tmp_path, scope, monkeypatch)
    args = ["--audio-profile-syx", str(syx)]
    user = {"SHARC_FAST_REGIONS": "0x99"}
    scope["set_fast_regions"](args, user)
    assert user == {"SHARC_FAST_REGIONS": "0x99"}
    off = {"DIGI_EMU_FAST": "0"}
    scope["set_fast_regions"](args, off)
    assert "SHARC_FAST_REGIONS" not in off
    empty = {"SHARC_FAST_REGIONS": ""}
    scope["set_fast_regions"](args, empty)
    assert empty["SHARC_FAST_REGIONS"] == ""


def test_set_fast_regions_leaves_unknown_firmware_off(tmp_path):
    scope = launcher()
    syx = tmp_path / "unknown.syx"
    syx.write_bytes(b"unknown")
    environ: dict[str, str] = {}
    scope["set_fast_regions"](["--audio-profile-syx", str(syx)], environ)
    assert "SHARC_FAST_REGIONS" not in environ


def test_main_sets_fast_regions_for_the_coupled_run(monkeypatch, tmp_path):
    scope = launcher()
    syx = _known_syx(tmp_path, scope, monkeypatch)
    generated = tmp_path / "gen"
    generated.mkdir()
    monkeypatch.setenv("SHARC_GEN_DIR", str(generated))
    monkeypatch.setenv("DIGI_EMU_PGO", "0")
    monkeypatch.delenv("SHARC_FAST_REGIONS", raising=False)
    monkeypatch.delenv("DIGI_EMU_FAST", raising=False)
    monkeypatch.setitem(scope["main"].__globals__, "build_panel", lambda environ: None)
    calls = []
    monkeypatch.setattr("subprocess.run", lambda command, **kw: calls.append(command))
    scope["main"](["x.syx", "--coupled", "--audio-profile-syx", str(syx)])
    assert __import__("os").environ["SHARC_FAST_REGIONS"] == "0x10,0x20"
    monkeypatch.delenv("SHARC_FAST_REGIONS")


def _build_run_setup(monkeypatch):
    scope = launcher()
    monkeypatch.setenv("DIGI_EMU_PGO", "0")
    monkeypatch.setitem(scope["main"].__globals__, "build_panel", lambda environ: None)
    calls = []

    def run(command, **_kwargs):
        calls.append(command)
        return subprocess.CompletedProcess(command, 0)

    monkeypatch.setattr("subprocess.run", run)
    return scope, calls


# --- PGO decisions --------------------------------------------------------------

COUPLED = ["x.syx", "--coupled"]


@pytest.fixture
def pgo(monkeypatch):
    """The real native_pgo module with its state, blocker and subprocess mocked."""
    from tools.emulator import native_pgo

    for name in ("DIGI_EMU_PGO", "DIGI_EMU_PGO_TRAIN"):
        monkeypatch.delenv(name, raising=False)
    box = type("Box", (), {})()
    box.states = [("stale", "changed: sources")]  # consumed one per pgo_state call
    box.blocker = ""
    box.train_exit = 0
    box.commands = []
    box.devices = []

    def pgo_state(_environ=None, device="dn2"):
        box.devices.append(device)
        return box.states.pop(0) if len(box.states) > 1 else box.states[0]

    def run(command, **_kwargs):
        box.commands.append(command)
        return subprocess.CompletedProcess(command, box.train_exit)

    monkeypatch.setattr(native_pgo, "pgo_state", pgo_state)
    monkeypatch.setattr(
        native_pgo, "training_blocker", lambda _env=None, device="dn2": box.blocker
    )
    monkeypatch.setattr(
        native_pgo, "plan_for", lambda detail, _env=None: ("plan", detail)
    )
    monkeypatch.setattr(native_pgo, "current_profile", lambda device="dn2": None)
    monkeypatch.setattr(subprocess, "run", run)
    return box


def trained(box):
    return [c for c in box.commands if c[-1] == "train"]


def test_stale_profile_trains_then_uses_the_new_one(pgo, monkeypatch, capsys):
    monkeypatch.setenv("DIGI_EMU_PGO_TRAIN", "1")
    pgo.states = [("stale", "changed: sources"), ("ok", "/p/abc.profdata")]
    plan = launcher()["pgo_plan"](COUPLED)
    assert plan == ("plan", "/p/abc.profdata")
    assert len(trained(pgo)) == 1
    err = capsys.readouterr().err.splitlines()
    assert err[0] == (
        "native PGO: profile stale (changed: sources); training (~6 min, "
        "DIGI_EMU_PGO_TRAIN=1)"
    )
    assert err[-1] == "native PGO: using profile abc.profdata"


def test_missing_profile_trains_with_the_opt_in(pgo, monkeypatch, capsys):
    monkeypatch.setenv("DIGI_EMU_PGO_TRAIN", "1")
    pgo.states = [("missing", "no manifest"), ("ok", "/p/abc.profdata")]
    assert launcher()["pgo_plan"](COUPLED) is not None
    assert "profile missing (no manifest); training" in capsys.readouterr().err


def test_current_profile_does_not_train(pgo):
    pgo.states = [("ok", "/p/abc.profdata")]
    assert launcher()["pgo_plan"](COUPLED) == ("plan", "/p/abc.profdata")
    assert pgo.commands == []


def test_stale_profile_builds_without_pgo_and_without_training(
    pgo, monkeypatch, capsys
):
    assert launcher()["pgo_plan"](COUPLED) is None
    assert trained(pgo) == []
    err = capsys.readouterr().err
    assert "stale for this core" in err
    assert "mise run perf:pgo" in err


def test_missing_profile_does_not_train_by_default(pgo, capsys):
    pgo.states = [("missing", "no manifest")]
    assert launcher()["pgo_plan"](COUPLED) is None
    assert trained(pgo) == []
    assert "mise run perf:pgo" in capsys.readouterr().err


def test_pgo_opt_out_is_silent(pgo, monkeypatch, capsys):
    pgo.states = [("off", "")]
    monkeypatch.setenv("DIGI_EMU_PGO", "0")
    assert launcher()["pgo_plan"](COUPLED) is None
    assert pgo.commands == []
    assert capsys.readouterr().err == ""


def test_training_failure_falls_back_without_blocking(pgo, monkeypatch, capsys):
    monkeypatch.setenv("DIGI_EMU_PGO_TRAIN", "1")
    pgo.train_exit = 3
    assert launcher()["pgo_plan"](COUPLED) is None
    assert len(trained(pgo)) == 1
    err = capsys.readouterr().err
    assert "WARNING: native PGO: training failed (exit 3); building without PGO" in err


def test_training_that_leaves_the_profile_stale_falls_back(pgo, monkeypatch, capsys):
    monkeypatch.setenv("DIGI_EMU_PGO_TRAIN", "1")
    pgo.states = [("stale", "changed: sources"), ("stale", "changed: rustc")]
    assert launcher()["pgo_plan"](COUPLED) is None
    assert (
        "WARNING: native PGO: profile stale (changed: rustc)" in capsys.readouterr().err
    )


def test_missing_training_inputs_fall_back_without_training(pgo, monkeypatch, capsys):
    monkeypatch.setenv("DIGI_EMU_PGO_TRAIN", "1")
    pgo.blocker = "missing private input /x"
    assert launcher()["pgo_plan"](COUPLED) is None
    assert pgo.commands == []
    err = capsys.readouterr().err
    assert err.startswith("WARNING: native PGO:")
    assert "cannot train: missing private input /x" in err


def test_rustflags_skip_warns_and_never_trains(pgo, capsys):
    pgo.states = [("skipped", "RUSTFLAGS is set")]
    assert launcher()["pgo_plan"](COUPLED) is None
    assert pgo.commands == []
    assert capsys.readouterr().err.startswith("WARNING: native PGO: skipped")


def test_stale_profile_is_reported_to_the_child(pgo):
    scope = launcher()
    assert scope["pgo_plan"](COUPLED) is None
    assert scope["pgo_env"]() == {
        "DIGI_PGO_STATUS": "stale",
        "DIGI_PGO_DETAIL": "changed: sources",
    }


def test_current_profile_is_reported_ok_without_detail(pgo):
    pgo.states = [("ok", "/p/abc.profdata")]
    scope = launcher()
    assert scope["pgo_plan"](COUPLED) is not None
    assert scope["pgo_env"]() == {"DIGI_PGO_STATUS": "ok", "DIGI_PGO_DETAIL": ""}


def test_pgo_opt_out_reports_nothing(pgo, monkeypatch):
    pgo.states = [("off", "")]
    monkeypatch.setenv("DIGI_EMU_PGO", "0")
    scope = launcher()
    assert scope["pgo_plan"](COUPLED) is None
    assert scope["pgo_env"]() == {}


def dt2_syx(monkeypatch, tmp_path):
    syx = tmp_path / "dt2.syx"
    syx.write_bytes(b"dt2")
    monkeypatch.setattr(
        digi_env, "device_for_syx", lambda p: "dt2" if p == syx else None
    )
    return syx


def test_dt2_firmware_selects_the_dt2_profile(pgo, monkeypatch, tmp_path):
    syx = dt2_syx(monkeypatch, tmp_path)
    pgo.states = [("ok", "/p/dt2.profdata")]
    args = ["x.syx", "--coupled", "--audio-profile-syx", str(syx)]
    assert launcher()["pgo_plan"](args) == ("plan", "/p/dt2.profdata")
    assert pgo.devices == ["dt2"]
    assert launcher()["pgo_plan"](COUPLED) == ("plan", "/p/dt2.profdata")
    assert pgo.devices == ["dt2", "dn2"]


def test_missing_dt2_profile_says_how_to_train_it(pgo, monkeypatch, tmp_path, capsys):
    syx = dt2_syx(monkeypatch, tmp_path)
    pgo.states = [("missing", "no manifest")]
    assert (
        launcher()["pgo_plan"](["--coupled", "--audio-profile-syx", str(syx)]) is None
    )
    assert (
        "python -m tools.emulator.native_pgo train --device dt2"
        in capsys.readouterr().err
    )


def test_dt2_opt_in_training_trains_the_dt2_profile(pgo, monkeypatch, tmp_path):
    syx = dt2_syx(monkeypatch, tmp_path)
    monkeypatch.setenv("DIGI_EMU_PGO_TRAIN", "1")
    pgo.states = [("missing", "no manifest"), ("ok", "/p/dt2.profdata")]
    args = ["--coupled", "--audio-profile-syx", str(syx)]
    assert launcher()["pgo_plan"](args) is not None
    assert trained(pgo) == []
    assert pgo.commands[0][-3:] == ["train", "--device", "dt2"]


def test_cf_only_build_never_asks_about_pgo(pgo):
    assert launcher()["pgo_plan"](["x.syx"]) is None
    assert pgo.commands == []


def test_train_command_failure_to_start_falls_back(pgo, monkeypatch, capsys):
    monkeypatch.setenv("DIGI_EMU_PGO_TRAIN", "1")

    def boom(*_a, **_k):
        raise OSError("no python")

    monkeypatch.setattr(subprocess, "run", boom)
    assert launcher()["pgo_plan"](COUPLED) is None
    assert "could not start training: no python" in capsys.readouterr().err


def _dual_launch(monkeypatch, tmp_path, first):
    scope, calls = _build_run_setup(monkeypatch)
    inputs, cores = {}, {}
    for device in ("dn2", "dt2"):
        directory = tmp_path / device
        directory.mkdir()
        inputs[device] = {
            label: directory / name
            for label, name in (
                ("firmware", "firmware.syx"),
                ("DSP image", "image.bin"),
                ("DSP state", "state.dsp"),
                ("CF snapshot", "snapshot"),
            )
        }
        for path in inputs[device].values():
            path.touch()
        cores[device] = directory / "generated"
        cores[device].mkdir()
    monkeypatch.delenv("DIGI_EMU_SECOND_CORE")
    monkeypatch.setattr(
        digi_env, "firmware", lambda environ=None: inputs[first]["firmware"]
    )
    monkeypatch.setattr(digi_env, "device_for_syx", lambda path: Path(path).parent.name)
    monkeypatch.setattr(
        digi_env, "ready_inputs", lambda *, device: dict(inputs[device])
    )
    monkeypatch.setattr(
        digi_env, "gen_for_image", lambda image: cores[Path(image).parent.name]
    )
    glob = scope["main"].__globals__
    monkeypatch.setitem(glob, "pgo_trained_core", lambda device: None)
    # Track all variables the launcher can set so no test leaks fast settings.
    for suffix in ("", "_2"):
        for name in (
            "SHARC_FAST_REGIONS",
            "SHARC_BLOCKTRANS",
            "SHARC_FAST_FALLBACK_REGIONS",
        ):
            monkeypatch.setenv(name + suffix, "")
            monkeypatch.delenv(name + suffix)
    monkeypatch.delenv("DIGI_EMU_FAST", raising=False)
    monkeypatch.delenv("SHARC_FAST_AUTO", raising=False)
    _set_table(
        monkeypatch,
        {
            device: {
                "fast_regions": f"{device}-regions",
                "blocktrans": f"{device}-roots",
                "fallback_regions": f"{device}-fallback",
            }
            for device in ("dn2", "dt2")
        },
    )
    monkeypatch.setattr(digi_env, "_syx_digest", lambda path: Path(path).parent.name)
    return scope, calls, inputs, cores


@pytest.mark.parametrize("first", ["dn2", "dt2"])
@pytest.mark.parametrize("positional", [False, True])
def test_desktop_defaults_to_both_devices_with_per_core_fast_settings(
    monkeypatch, tmp_path, capsys, first, positional
):
    scope, calls, inputs, cores = _dual_launch(monkeypatch, tmp_path, first)
    other = "dt2" if first == "dn2" else "dn2"
    scope["main"]([str(inputs[first]["firmware"])] if positional else [])
    forwarded = calls[-1][1:]
    assert forwarded[0] == str(inputs[first]["firmware"])
    at = forwarded.index("--also-coupled")
    assert forwarded[at + 1 : at + 5] == [
        str(inputs[other][label])
        for label in ("DSP image", "DSP state", "CF snapshot", "firmware")
    ]
    # Slot order is fixed: slot 1 is the DN2 core, slot 2 the DT2 core, whichever firmware launches.
    assert os.environ["SHARC_GEN_DIR"] == str(cores["dn2"])
    assert os.environ["SHARC_GEN_DIR_2"] == str(cores["dt2"])
    for suffix, device in (("", "dn2"), ("_2", "dt2")):
        for name, value in (
            ("SHARC_FAST_REGIONS", "regions"),
            ("SHARC_BLOCKTRANS", "roots"),
            ("SHARC_FAST_FALLBACK_REGIONS", "fallback"),
        ):
            assert os.environ[name + suffix] == f"{device}-{value}"


@pytest.mark.parametrize("first", ["dn2", "dt2"])
def test_two_core_launch_plans_the_both_profile_in_its_own_target_dir(
    monkeypatch, tmp_path, first
):
    """A two-core launch asks for the `both` profile and builds outside the single-core target dirs."""
    from tools.emulator import native_pgo

    scope, calls, inputs, cores = _dual_launch(monkeypatch, tmp_path, first)
    monkeypatch.setenv("DIGI_EMU_PGO", "1")
    monkeypatch.setattr(native_pgo, "supported_host", lambda: True)
    monkeypatch.setattr(native_pgo, "toolchain_rustc", lambda: "/fake/rustc")
    asked = []

    def pgo_state(_environ=None, device="dn2"):
        asked.append(device)
        return "ok", "/p/both.profdata"

    monkeypatch.setattr(native_pgo, "pgo_state", pgo_state)
    scope["main"]([])
    assert asked == ["both"]
    command = calls[0]
    assert command[command.index("--features") + 1] == "second-core"
    target = Path(command[command.index("--target-dir") + 1])
    # Keyed by both cores, in slot order, and never the single-core pgo-use dir.
    assert "+" in target.parent.name
    assert target.name == "pgo-use"
    assert target.parent != Path(scope["target_dir_for"](cores["dn2"]))


def test_missing_both_profile_falls_back_to_the_plain_two_core_build(
    monkeypatch, tmp_path, capsys
):
    from tools.emulator import native_pgo

    scope, calls, inputs, cores = _dual_launch(monkeypatch, tmp_path, "dt2")
    monkeypatch.setenv("DIGI_EMU_PGO", "1")
    monkeypatch.delenv("DIGI_EMU_PGO_TRAIN", raising=False)
    monkeypatch.setattr(
        native_pgo,
        "pgo_state",
        lambda _e=None, device="dn2": ("missing", "no manifest"),
    )
    scope["main"]([])
    command = calls[0]
    assert command[command.index("--features") + 1] == "second-core"
    assert "pgo-use" not in command[command.index("--target-dir") + 1]
    assert "train --device both" in capsys.readouterr().err


@pytest.mark.parametrize(
    ("daemon", "setting", "dual"),
    [(False, "0", False), (True, None, False), (True, "0", False), (True, "1", True)],
)
def test_second_core_opt_out_and_daemon_opt_in(
    monkeypatch, tmp_path, daemon, setting, dual
):
    scope, calls, _, _ = _dual_launch(monkeypatch, tmp_path, "dn2")
    if setting is not None:
        monkeypatch.setenv("DIGI_EMU_SECOND_CORE", setting)
    scope["main"](["--daemon"] if daemon else [])
    assert ("--also-coupled" in calls[-1]) is dual
    assert ("SHARC_GEN_DIR_2" in os.environ) is dual


@pytest.mark.parametrize(
    "missing", ["firmware", "DSP image", "DSP state", "CF snapshot", "generated core"]
)
def test_desktop_missing_second_inputs_keeps_first_core(
    monkeypatch, tmp_path, capsys, missing
):
    scope, calls, inputs, cores = _dual_launch(monkeypatch, tmp_path, "dn2")
    if missing == "generated core":
        cores["dt2"].rmdir()
    else:
        inputs["dt2"][missing].unlink()
    scope["main"]([])
    assert "--auto-coupled" in calls[-1]
    assert "--also-coupled" not in calls[-1]
    assert "SHARC_GEN_DIR_2" not in os.environ
    assert (
        f"second core: no DT2 inputs (missing {missing}); one core only"
        in capsys.readouterr().err
    )


def _dt2_launch(monkeypatch, tmp_path, manifest_core):
    """A DT2 launch with ready inputs, a registered core and a PGO manifest."""
    from tools.emulator import native_pgo

    syx = dt2_syx(monkeypatch, tmp_path)
    ready = tmp_path / "ready-dt2"
    ready.mkdir()
    for name in [
        "digi-audio-dn2-image.bin",
        "digi-audio-m5.snap.dsp",
        "digi-audio-m5.snap",
    ]:
        (ready / name).touch()
    registered = tmp_path / "registered"
    registered.mkdir()
    monkeypatch.setitem(digi_env.READY_DIRS, "dt2", ready)
    monkeypatch.setattr(digi_env, "gen_for_image", lambda _image: registered)
    monkeypatch.setattr(
        native_pgo,
        "read_manifest",
        lambda device="dn2": {"generated_dir": str(manifest_core)},
    )
    # default_audio_args exports SHARC_GEN_DIR: have monkeypatch undo it.
    monkeypatch.setenv("SHARC_GEN_DIR", "x")
    monkeypatch.delenv("SHARC_GEN_DIR")
    return syx, registered


def test_dt2_default_core_is_the_one_the_profile_was_trained_on(
    monkeypatch, tmp_path, capsys
):
    trained = tmp_path / "trained"
    trained.mkdir()
    syx, _ = _dt2_launch(monkeypatch, tmp_path, trained)
    args = launcher()["default_audio_args"]([str(syx)])
    assert "--auto-coupled" in args
    assert os.environ["SHARC_GEN_DIR"] == str(trained)
    err = capsys.readouterr().err
    assert f"native core: {trained}" in err and "PGO profile" in err


def test_dt2_default_core_falls_back_to_the_registered_one(
    monkeypatch, tmp_path, capsys
):
    syx, registered = _dt2_launch(monkeypatch, tmp_path, tmp_path / "gone")
    launcher()["default_audio_args"]([str(syx)])
    assert os.environ["SHARC_GEN_DIR"] == str(registered)
    assert "registered core" in capsys.readouterr().err


def test_dt2_default_core_skips_a_stale_trained_core(monkeypatch, tmp_path):
    trained = tmp_path / "trained"
    trained.mkdir()
    syx, registered = _dt2_launch(monkeypatch, tmp_path, trained)
    # Only the registered core carries the current fingerprint.
    (registered / digi_env.CORE_FINGERPRINT_FILE).write_text(
        digi_env.core_fingerprint()
    )
    monkeypatch.delenv("DIGI_ALLOW_STALE_GEN")
    launcher()["default_audio_args"]([str(syx)])
    assert os.environ["SHARC_GEN_DIR"] == str(registered)


def test_dt2_explicit_gen_dir_wins(monkeypatch, tmp_path):
    trained = tmp_path / "trained"
    trained.mkdir()
    syx, _ = _dt2_launch(monkeypatch, tmp_path, trained)
    chosen = tmp_path / "chosen"
    chosen.mkdir()
    monkeypatch.setenv("SHARC_GEN_DIR", str(chosen))
    launcher()["default_audio_args"]([str(syx)])
    assert os.environ["SHARC_GEN_DIR"] == str(chosen)


# plus_drive/<device>/ is the +Drive input.


def _folder(root, device, files):
    for name, data in files.items():
        path = root / device / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    return root / device


def _card_run(monkeypatch, tmp_path, device):
    """main() with the card builder and the device lookup stubbed; returns the
    card calls and the launcher arguments."""
    scope, calls = _build_run_setup(monkeypatch)
    syx = tmp_path / "known.syx"
    syx.write_bytes(b"x")
    glob = scope["main"].__globals__
    monkeypatch.setattr(digi_env, "device_for_syx", lambda path: device)
    monkeypatch.setattr(digi_env, "patch_base", lambda path: None)
    built = []

    def card(folder, firmware, env, dev="dt2"):
        built.append((folder, firmware, dev))
        return tmp_path / "card.img"

    monkeypatch.setitem(glob, "plusdrive_card", card)
    return scope, calls, built, syx


def test_the_launcher_leaves_the_device_folder_to_the_controller(monkeypatch, tmp_path):
    scope, calls, built, syx = _card_run(monkeypatch, tmp_path, "dt2")
    root = tmp_path / "pd"
    monkeypatch.setenv("DIGI_PLUS_DRIVE", str(root))
    _folder(root, "dt2", {"digikit/modules/test.dkm": b"x"})
    scope["main"]([str(syx), "--cf-only"])
    # No card is built here and no folder is named: the controller builds the card of every load from the loaded device's folder
    # (plus_drive/<device>/ or the one the user chose in the Card panel) and mirrors the module store to it.
    assert built == []
    assert calls[-1][1:] == [str(syx), "--device-folders"]


def test_no_syx_argument_takes_the_same_way(monkeypatch, tmp_path):
    scope, calls, built, syx = _card_run(monkeypatch, tmp_path, "dt2")
    monkeypatch.setenv("DIGI_EMU_SYX", str(syx))
    scope["main"](["--cf-only"])
    assert built == []
    assert calls[-1][1:] == ["--device-folders"]


def test_no_sync_turns_the_exit_export_and_the_mirror_off(monkeypatch, tmp_path):
    scope, calls, built, syx = _card_run(monkeypatch, tmp_path, "dt2")
    scope["main"]([str(syx), "--cf-only", "--no-sync"])
    assert calls[-1][1:] == [str(syx), "--device-folders", "--no-card-sync"]


def test_a_folder_given_on_the_command_line_is_the_first_loads_card_and_mirror(
    monkeypatch, tmp_path
):
    scope, calls, built, syx = _card_run(monkeypatch, tmp_path, "dt2")
    here = tmp_path / "mine"
    here.mkdir()
    (here / "a.wav").write_bytes(b"x")
    scope["main"]([str(syx), "--cf-only", "--plus-drive", str(here)])
    assert built == [(here, syx, "dt2")]
    assert calls[-1][1:] == [
        str(syx),
        "--card-image",
        str(tmp_path / "card.img"),
        "--card-folder",
        str(here.resolve()),
        "--device-folders",
        # the folder of the first load only: the controller uses the device's own folder for the next device
        "--mirror-dir",
        str(here.resolve()),
    ]
    scope["main"]([str(syx), "--cf-only", "--plus-drive", str(here), "--no-sync"])
    assert calls[-1][1:] == [
        str(syx),
        "--card-image",
        str(tmp_path / "card.img"),
        "--card-folder",
        str(here.resolve()),
        "--no-card-sync",
        "--device-folders",
    ]


def test_plus_drive_and_samples_options(monkeypatch, tmp_path):
    scope, calls, built, syx = _card_run(monkeypatch, tmp_path, "dt2")
    here = tmp_path / "mine"
    here.mkdir()
    scope["main"]([str(syx), "--cf-only", "--plus-drive", str(here)])
    scope["main"]([str(syx), "--cf-only", "--samples", str(here)])
    assert [b[0] for b in built] == [here, here]
    with pytest.raises(SystemExit):
        scope["main"]([str(syx), "--plus-drive", str(here), "--samples", str(here)])
    with pytest.raises(SystemExit):
        scope["main"]([str(syx), "--samples", str(here), "--card-image", "c.img"])
    with pytest.raises(SystemExit):
        scope["main"]([str(syx), "--plus-drive"])


def test_card_cache_key_follows_every_file(monkeypatch, tmp_path):
    scope = launcher()
    glob = scope["plusdrive_card"].__globals__
    target = tmp_path / "target"
    (target / "release").mkdir(parents=True)
    (target / "release" / "plusdrive").write_bytes(b"binary")
    monkeypatch.setitem(glob, "target_dir_for", lambda gen: str(target))
    monkeypatch.setitem(glob, "ROOT", tmp_path / "repo")
    syx = tmp_path / "fw.syx"
    syx.write_bytes(b"fw")
    folder = _folder(
        tmp_path, "dt2", {"a/b.bin": b"1", "c.dkm": b"2", ".mmcfs/kits/0000.bin": b"3"}
    )
    built = []

    def run(command, **_kwargs):
        if command[0].endswith("plusdrive"):
            built.append(command)
            Path(command[command.index("-o") + 1]).write_bytes(b"img")
        return subprocess.CompletedProcess(command, 0)

    monkeypatch.setattr("subprocess.run", run)
    card = scope["plusdrive_card"]
    first = card(folder, syx, {}, "dt2")
    assert card(folder, syx, {}, "dt2") == first and len(built) == 1
    assert first.name == "dt2.img"
    assert "--device" in built[0] and built[0][built[0].index("--device") + 1] == "dt2"
    (folder / "a" / "b.bin").write_bytes(b"changed")
    second = card(folder, syx, {}, "dt2")
    assert second != first and len(built) == 2
    (folder / "c.dkm").rename(folder / "d.dkm")
    assert card(folder, syx, {}, "dt2") not in (first, second)
    assert card(folder, syx, {}, "dn2").name == "dn2.img"
    with pytest.raises(SystemExit):
        card(tmp_path / "missing", syx, {}, "dt2")


def test_fallback_defaults_single_and_dual_core(monkeypatch, tmp_path):
    scope = launcher()
    first = tmp_path / "fw.syx"
    first.write_bytes(b"dn2")
    second = tmp_path / "second.syx"
    second.write_bytes(b"second")
    sha = __import__("hashlib").sha256
    _set_table(
        monkeypatch,
        {
            sha(b"dn2").hexdigest(): {"fallback_regions": "0x50"},
            sha(b"second").hexdigest(): {"fallback_regions": "0x60"},
        },
    )
    args = [
        "--audio-profile-syx",
        str(first),
        "--also-coupled",
        "image",
        "state",
        "snapshot",
        str(second),
    ]
    env = {}
    scope["set_fast_regions"](args, env)
    assert env["SHARC_FAST_FALLBACK_REGIONS"] == "0x50"
    assert env["SHARC_FAST_FALLBACK_REGIONS_2"] == "0x60"
    for explicit in ["", "0x99"]:
        env = {"SHARC_FAST_REGIONS": explicit, "SHARC_FAST_FALLBACK_REGIONS_2": ""}
        scope["set_fast_regions"](args, env)
        assert "SHARC_FAST_FALLBACK_REGIONS" not in env
        assert env["SHARC_FAST_FALLBACK_REGIONS_2"] == ""
    env = {"SHARC_FAST_REGIONS": "", "SHARC_FAST_FALLBACK_REGIONS": "0x77"}
    scope["set_fast_regions"](args, env)
    assert env["SHARC_FAST_FALLBACK_REGIONS"] == "0x77"
    env = {"SHARC_FAST_AUTO": "10,100"}
    scope["set_fast_regions"](args, env)
    assert "SHARC_FAST_FALLBACK_REGIONS" not in env
    assert "SHARC_FAST_FALLBACK_REGIONS_2" not in env
    env = {"DIGI_EMU_FAST": "0"}
    scope["set_fast_regions"](args, env)
    assert env == {"DIGI_EMU_FAST": "0"}
    unknown = tmp_path / "unknown.syx"
    unknown.write_bytes(b"unknown")
    env = {}
    scope["set_fast_regions"](["--audio-profile-syx", str(unknown)], env)
    assert "SHARC_FAST_FALLBACK_REGIONS" not in env
