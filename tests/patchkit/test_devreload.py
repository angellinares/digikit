"""Automatic development-loop DSP selection and mode guards."""

import argparse
from unittest.mock import Mock

import pytest

from tools.firmware import config
from tools.patchkit import pk_devreload as dev
from tools.patchkit import pk_dkrt


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


@pytest.mark.parametrize("device", ["dn2", "dt2"])
def test_machine_ui_preview_store_overrides_dsp_but_keeps_exact_tuple(device):
    store = ["digikit/pluck*"]
    assert dev.store_ui_preview_mode("emulator", device, store) == "zero"
    assert dev.target_dsp_mode("emulator", device, store) == "sharc"
    assert (
        "store-digikit_pluck"
        in dev.run_dir_of(device, "emulator", store, dsp="zero").name
    )


@pytest.mark.parametrize(
    "target,store",
    [
        ("sample-tracks", ["digikit/pluck*"]),
        ("emulator", []),
        ("emulator", ["file:anywhere=modules"]),
        ("emulator", ["synth:test:0"]),
        ("emulator", ["digikit/pluck*", "digikit/pluck"]),
        ("emulator", ["core/sample-pool"]),
    ],
)
def test_ui_preview_store_rejects_other_inputs(target, store):
    with pytest.raises(dev.H.HarnessError, match="UI preview"):
        dev.store_ui_preview_mode(target, "dn2", store)


@pytest.mark.parametrize("preview", [False, True])
def test_card_pack_uses_same_preview_variant_as_candidate(
    monkeypatch, tmp_path, preview
):
    pack_cli = argparse.ArgumentParser()
    pack_cli.add_argument("command", choices=["pack"])
    pack_cli.add_argument("module")
    pack_cli.add_argument("--device")
    pack_cli.add_argument("-o", type=dev.Path)
    pack_cli.add_argument("--ui-preview", action="store_true")

    def pack(*args):
        request = pack_cli.parse_args([str(arg) for arg in args])
        request.o.write_text(f"{request.module}:{request.device}:{request.ui_preview}")

    tools = Mock(patchkit_run=Mock(side_effect=pack))
    monkeypatch.setattr(pk_dkrt, "Tools", Mock(return_value=tools))
    monkeypatch.setattr(config, "stock", Mock(return_value=tmp_path / "stock.syx"))
    build_card = Mock()
    selection = Mock()
    monkeypatch.setattr(pk_dkrt, "build_card", build_card)
    monkeypatch.setattr(pk_dkrt, "write_selection", selection)
    card = dev.store_card("dn2", ["digikit/pluck*"], tmp_path, ui_preview=preview)
    staged = tmp_path / "plus_drive/digikit/modules/pluck.dkm"
    assert staged.read_text() == f"digikit/pluck:dn2:{preview}"
    build_card.assert_called_once()
    selection.assert_called_once_with(card, ["modules/pluck.dkm"], "dn2")


def test_ui_preview_run_uses_zero_mode_for_start_and_kept_directory(
    monkeypatch, tmp_path
):
    monkeypatch.setattr(dev, "build", Mock(return_value=(0, [])))
    monkeypatch.setattr(dev, "load_keys", Mock(return_value=[]))
    monkeypatch.setattr(dev, "connect", Mock(return_value=None))
    emu = Mock()
    emu.status.return_value = {"dsp": "zero"}
    emu.call.return_value = {}
    session = Mock()
    start = Mock(return_value=(emu, session))
    monkeypatch.setattr(dev, "start", start)
    release = Mock()
    monkeypatch.setattr(dev, "release", release)
    args = argparse.Namespace(
        target="emulator",
        store="digikit/pluck*",
        store_ui_preview=True,
        trace=False,
        out=str(tmp_path),
        keys=None,
    )
    report = dev.run_device("dn2", args)
    assert report["dsp"] == "zero"
    assert start.call_args.args[1] == dev.run_dir_of(
        "dn2", "emulator", ["digikit/pluck*"], dsp="zero"
    )
    assert start.call_args.args[2:] == (["digikit/pluck*"], False, "zero")
    assert start.call_args.kwargs == {"store_ui_preview": True}
    emu.pcm_start.assert_not_called()
    release.assert_called_once_with(emu)


def test_preview_trace_is_rejected_before_running(monkeypatch):
    run = Mock()
    monkeypatch.setattr(dev, "run_device", run)
    with pytest.raises(SystemExit):
        dev.main(
            ["emulator", "--store", "digikit/pluck*", "--store-ui-preview", "--trace"]
        )
    run.assert_not_called()
