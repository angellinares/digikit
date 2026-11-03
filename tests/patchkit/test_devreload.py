"""Automatic development-loop DSP selection and mode guards."""

from unittest.mock import Mock

import pytest

from tools.patchkit import pk_devreload as dev


def test_cf_target_and_synthetic_store_use_zero():
    assert dev.dsp_mode(["test/ui-demo"], []) == "zero"
    assert dev.dsp_mode(["test/ui-demo"], ["synth:arena:1024*"]) == "zero"


def test_store_dsp_and_unknown_files_keep_sharc():
    assert dev.dsp_mode(["test/ui-demo"], ["digikit/pluck*"]) == "sharc"
    assert dev.dsp_mode(["test/ui-demo"], ["file:anywhere=modules"]) == "sharc"
    assert dev.dsp_mode(["unknown/missing"], []) == "sharc"


def test_dsp_dependency_keeps_sharc():
    assert dev.dsp_mode(["core/sample-pool"], []) == "sharc"


def test_modes_have_separate_kept_daemons():
    assert dev.run_dir_of("dn2", "test/ui-demo", dsp="zero") != dev.run_dir_of(
        "dn2", "test/ui-demo", dsp="sharc"
    )


@pytest.mark.parametrize(
    "dsp,audio,expected",
    [("zero", None, "zero"), ("sharc", {}, "sharc")],
)
def test_mode_guard_accepts_matching_status(dsp, audio, expected):
    dev.assert_mode(
        Mock(status=Mock(return_value={"dsp": dsp, "audio": audio})), expected
    )


@pytest.mark.parametrize(
    "status", [{"dsp": "zero", "audio": {}}, {"dsp": "sharc", "audio": None}, {}]
)
def test_mode_guard_rejects_inconsistent_status(status):
    with pytest.raises(dev.H.HarnessError, match="expected DSP"):
        dev.assert_mode(Mock(status=Mock(return_value=status)), "zero")


def test_zero_session_key_actions_skip_pcm():
    session = object.__new__(dev.H.Session)
    session.dsp = "zero"
    session.emu = Mock()
    session.mark_pcm()
    session.emu.pcm_report.assert_not_called()


@pytest.mark.parametrize("device", ["dn2", "dt2"])
def test_patch_set_hint_does_not_cover_store_or_other_sets(device):
    assert dev.target_dsp_mode("emulator", device, []) == "zero"
    assert dev.target_dsp_mode("emulator", device, ["digikit/pluck*"]) == "sharc"
    assert dev.target_dsp_mode("sample-tracks", device, []) == "sharc"
    assert dev.target_dsp_mode("test/ui-demo", device, []) == "zero"


def test_zero_patch_set_policy_names_existing_sets():
    policy = dev.tomllib.loads((dev.HERE / "pk_devreload.toml").read_text())
    for device in dev.DEVICE_FILE:
        data = dev.tomllib.loads(
            (dev.ROOT / "devices" / dev.DEVICE_FILE[device]).read_text()
        )
        assert set(policy["zero_dsp_patch_sets"]) <= data["patch_sets"].keys()


def test_zero_safe_base_still_requires_sharc_for_content_dsp():
    assert dev.dsp_mode(["digikit/pluck"], [], zero_framework=True) == "sharc"
    assert dev.dsp_mode(["test/patched-pluck"], [], zero_framework=True) == "sharc"
    assert (
        dev.dsp_mode(["test/ui-demo"], ["core/sample-pool"], zero_framework=True)
        == "sharc"
    )


def test_stop_checks_both_modes_without_resolving_changed_modules(monkeypatch):
    emulators = [Mock(), Mock()]
    connect = Mock(side_effect=[*emulators, None])
    monkeypatch.setattr(dev, "connect", connect)
    dev.stop("dn2", "removed/module")
    assert connect.call_count == 3
    for emu in emulators:
        emu.quit.assert_called_once()
        emu.close.assert_called_once()


def test_stop_reaches_exact_legacy_directory_for_same_device_target_store_and_trace(
    monkeypatch,
):
    legacy = Mock()
    visited = []
    expected = dev.H.OUT / "run-devreload-dn2-test_ui-demo+store-digikit_pluck+trace"

    def connect(path):
        visited.append(path)
        return legacy if path == expected else None

    monkeypatch.setattr(dev, "connect", connect)
    dev.stop("dn2", "test/ui-demo", ["digikit/pluck"], trace=True)
    assert visited == [
        expected.with_name(expected.name + "-zero"),
        expected.with_name(expected.name + "-sharc"),
        expected,
    ]
    legacy.quit.assert_called_once()
    legacy.close.assert_called_once()
