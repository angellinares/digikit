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
    assert dev.dsp_mode(["test/ui-demo"], ["test/sine*"]) == "sharc"
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
    assert dev.target_dsp_mode("emulator", device, ["test/sine*"]) == "sharc"
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
    assert dev.dsp_mode(["test/sine"], [], zero_framework=True) == "sharc"
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
    expected = dev.H.OUT / "run-devreload-dn2-test_ui-demo+store-test_sine+trace"

    def connect(path):
        visited.append(path)
        return legacy if path == expected else None

    monkeypatch.setattr(dev, "connect", connect)
    dev.stop("dn2", "test/ui-demo", ["test/sine"], trace=True)
    assert visited == [
        expected.with_name(expected.name + "-zero"),
        expected.with_name(expected.name + "-sharc"),
        expected,
    ]
    legacy.quit.assert_called_once()
    legacy.close.assert_called_once()


@pytest.mark.parametrize("device", ["dn2", "dt2"])
def test_machine_ui_preview_store_overrides_dsp_but_keeps_exact_tuple(device):
    store = ["test/sine*"]
    assert dev.store_ui_preview_mode("emulator", device, store) == "zero"
    assert dev.target_dsp_mode("emulator", device, store) == "sharc"
    assert (
        "store-test_sine" in dev.run_dir_of(device, "emulator", store, dsp="zero").name
    )


@pytest.mark.parametrize(
    "target,store",
    [
        ("sample-tracks", ["test/sine*"]),
        ("emulator", []),
        ("emulator", ["file:anywhere=modules"]),
        ("emulator", ["synth:test:0"]),
        ("emulator", ["test/sine*", "test/sine"]),
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
    card = dev.store_card("dn2", ["test/sine*"], tmp_path, ui_preview=preview)
    staged = tmp_path / "plus_drive/digikit/modules/sine.dkm"
    assert staged.read_text() == f"test/sine:dn2:{preview}"
    build_card.assert_called_once()
    selection.assert_called_once_with(card, ["modules/sine.dkm"], "dn2")


def test_ui_preview_run_uses_zero_mode_for_start_and_kept_directory(
    monkeypatch, tmp_path
):
    monkeypatch.setattr(dev, "build", Mock(return_value=(0, [])))
    monkeypatch.setattr(dev, "load_keys", Mock(return_value=[]))
    monkeypatch.setattr(dev, "connect", Mock(return_value=None))
    card = tmp_path / "store-card.img"
    monkeypatch.setattr(dev, "refresh_store", Mock(return_value=(None, card)))
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
        store="test/sine*",
        store_ui_preview=True,
        trace=False,
        out=str(tmp_path),
        keys=None,
    )
    report = dev.run_device("dn2", args)
    assert report["dsp"] == "zero"
    assert start.call_args.args[1] == dev.run_dir_of(
        "dn2", "emulator", ["test/sine*"], dsp="zero"
    )
    assert start.call_args.args[2:] == (["test/sine*"], False, "zero")
    assert start.call_args.kwargs == {
        "store_ui_preview": True,
        "prepared_card": card,
    }
    emu.pcm_start.assert_not_called()
    release.assert_called_once_with(emu)


def test_preview_trace_is_rejected_before_running(monkeypatch):
    run = Mock()
    monkeypatch.setattr(dev, "run_device", run)
    with pytest.raises(SystemExit):
        dev.main(["emulator", "--store", "test/sine*", "--store-ui-preview", "--trace"])
    run.assert_not_called()


@pytest.fixture
def store_build(monkeypatch, tmp_path):
    """Exercise the real store parser and staging with a tiny deterministic packer/card."""
    packed = {"bytes": b"first"}
    requests = []

    def pack(*args):
        command = args[0]
        out = dev.Path(args[args.index("-o") + 1])
        requests.append(args)
        if command == "synth-dkm":
            out.write_bytes(dev.Path(args[1]).read_bytes())
        else:
            out.write_bytes(packed["bytes"])

    def build_card(_tools, folder, image, _syx, _device):
        image.write_bytes(
            b"card:"
            + b"".join(p.read_bytes() for p in sorted(folder.rglob("*")) if p.is_file())
        )

    tools = Mock(patchkit_run=Mock(side_effect=pack))
    monkeypatch.setattr(pk_dkrt, "Tools", Mock(return_value=tools))
    monkeypatch.setattr(config, "stock", Mock(return_value=tmp_path / "stock.syx"))
    card_build = Mock(side_effect=build_card)
    selection = Mock()
    stop = Mock()
    monkeypatch.setattr(pk_dkrt, "build_card", card_build)
    monkeypatch.setattr(pk_dkrt, "write_selection", selection)
    monkeypatch.setattr(dev, "stop_connected", stop)
    return packed, requests, card_build, selection, stop


def refresh(run_dir, emu=None, ids=None, **kwargs):
    return dev.refresh_store(
        emu,
        kwargs.get("device", "dn2"),
        kwargs.get("target", "emulator"),
        ids if ids is not None else ["test/sine*"],
        run_dir,
        kwargs.get("trace", False),
        kwargs.get("ui_preview", False),
    )


def test_unchanged_store_reuses_live_card_and_initial_selection(store_build, tmp_path):
    _, requests, card_build, selection, stop = store_build
    run_dir = tmp_path / "kept"
    _, card = refresh(run_dir)
    # Runtime selection and mirror edits must not become the requested-input baseline.
    card.write_bytes(b"runtime selection changed")
    overlay = run_dir / "overlay.bin"
    overlay.write_bytes(b"saved runtime writes")
    staged = run_dir / "plus_drive/digikit/modules/sine.dkm"
    staged.write_bytes(b"runtime mirror changed")
    emu = Mock()
    assert refresh(run_dir, emu) == (emu, card)
    assert card.read_bytes() == b"runtime selection changed"
    assert overlay.read_bytes() == b"saved runtime writes"
    assert staged.read_bytes() == b"runtime mirror changed"
    assert len(requests) == 2  # one candidate pack per invocation
    card_build.assert_called_once()
    selection.assert_called_once_with(
        card_build.call_args.args[2], ["modules/sine.dkm"], "dn2"
    )
    stop.assert_not_called()


def test_changed_module_prepares_once_before_stopping_only_connected_run(
    store_build, tmp_path
):
    packed, requests, card_build, _, stop = store_build
    run_dir = tmp_path / "kept"
    _, card = refresh(run_dir)
    old_card = card.read_bytes()
    other = tmp_path / "other"
    other.mkdir()
    (other / "store-card.img").write_bytes(b"unrelated")
    packed["bytes"] = b"updated"
    emu = Mock()

    def stopped(actual, directory):
        assert actual is emu and directory == run_dir
        assert card.read_bytes() == old_card
        assert card_build.call_count == 2
        assert card_build.call_args.args[2].read_bytes() == b"card:updated"
        (directory / "overlay.bin").write_bytes(b"quit exported old card writes")

    stop.side_effect = stopped
    assert refresh(run_dir, emu) == (None, card)
    stop.assert_called_once_with(emu, run_dir)
    assert card.read_bytes() == b"card:updated"
    assert not (run_dir / "overlay.bin").exists()
    assert len(requests) == 2
    assert (other / "store-card.img").read_bytes() == b"unrelated"


@pytest.mark.parametrize("change", ["content", "addition", "removal"])
def test_store_directory_changes_refresh_card(store_build, tmp_path, change):
    _, _, card_build, _, stop = store_build
    source = tmp_path / "samples"
    source.mkdir()
    sample = source / "one.wav"
    sample.write_bytes(b"one")
    ids = [f"file:{source}=samples"]
    run_dir = tmp_path / "kept"
    refresh(run_dir, ids=ids)
    if change == "content":
        sample.write_bytes(b"two")
    elif change == "addition":
        (source / "two.wav").write_bytes(b"two")
    else:
        sample.unlink()
    emu = Mock()
    assert refresh(run_dir, emu, ids)[0] is None
    assert card_build.call_count == 2
    stop.assert_called_once_with(emu, run_dir)


@pytest.mark.parametrize(
    "field,value",
    [
        ("ids", ["test/sine"]),
        ("trace", True),
        ("ui_preview", True),
        ("target", "different/target"),
        ("device", "dt2"),
    ],
)
def test_exact_request_and_initial_selection_refresh_even_with_identical_bytes(
    store_build, tmp_path, field, value
):
    _, _, card_build, _, stop = store_build
    run_dir = tmp_path / "kept"
    refresh(run_dir)
    emu = Mock()
    assert refresh(run_dir, emu, **{field: value})[0] is None
    assert card_build.call_count == 2
    stop.assert_called_once_with(emu, run_dir)


def test_synthetic_and_file_store_can_reuse_unchanged_card(store_build, tmp_path):
    _, requests, card_build, _, stop = store_build
    sample = tmp_path / "one.wav"
    sample.write_bytes(b"sample")
    ids = ["synth:arena:1024*", f"file:{sample}=samples/one.wav"]
    run_dir = tmp_path / "kept"
    refresh(run_dir, ids=ids)
    emu = Mock()
    assert refresh(run_dir, emu, ids)[0] is emu
    assert len(requests) == 2
    card_build.assert_called_once()
    stop.assert_not_called()


@pytest.mark.parametrize("failure", ["pack", "copy", "card", "selection", "shutdown"])
def test_prepare_or_shutdown_failure_preserves_mounted_card(
    store_build, tmp_path, failure
):
    packed, _, card_build, selection, stop = store_build
    run_dir = tmp_path / "kept"
    _, card = refresh(run_dir)
    original = card.read_bytes()
    baseline = (run_dir / "store-request.json").read_bytes()
    packed["bytes"] = b"updated"
    ids = None
    if failure == "pack":
        pk_dkrt.Tools().patchkit_run.side_effect = RuntimeError("pack")
    elif failure == "copy":
        ids = [f"file:{tmp_path / 'missing'}=samples/missing"]
    elif failure == "card":
        card_build.side_effect = RuntimeError("card")
    elif failure == "selection":
        selection.side_effect = RuntimeError("selection")
    else:
        stop.side_effect = RuntimeError("shutdown")
    emu = Mock()
    with pytest.raises((RuntimeError, FileNotFoundError)):
        refresh(run_dir, emu, ids)
    assert card.read_bytes() == original
    assert (run_dir / "store-request.json").read_bytes() == baseline
    if failure != "shutdown":
        stop.assert_not_called()
        emu.quit.assert_not_called()


def daemon_identity(run_dir, emu):
    emu.path = str(run_dir / "12345.sock")
    (run_dir / "current.json").write_text(
        dev.json.dumps({"pid": 12345, "socket": emu.path})
    )


def test_connected_shutdown_waits_for_process_exit_without_reconnecting(
    monkeypatch, tmp_path
):
    emu = Mock()
    daemon_identity(tmp_path, emu)
    probe = Mock(side_effect=[None, ProcessLookupError()])
    monkeypatch.setattr(dev.os, "kill", probe)
    monkeypatch.setattr(dev.time, "sleep", Mock())
    connect = Mock()
    monkeypatch.setattr(dev, "connect", connect)
    dev.stop_connected(emu, tmp_path)
    assert probe.call_count == 2
    assert all(c.args == (12345, 0) for c in probe.call_args_list)
    emu.quit.assert_called_once()
    emu._sock.settimeout.assert_called_once_with(5)
    connect.assert_not_called()


def test_shutdown_timeout_does_not_kill_or_replace_daemon(monkeypatch, tmp_path):
    emu = Mock()
    daemon_identity(tmp_path, emu)
    probe = Mock()
    monkeypatch.setattr(dev.os, "kill", probe)
    monkeypatch.setattr(dev.time, "monotonic", Mock(side_effect=[0, 6]))
    with pytest.raises(dev.H.HarnessError, match="did not stop"):
        dev.stop_connected(emu, tmp_path)
    probe.assert_called_once_with(12345, 0)
    emu.quit.assert_called_once()
    emu.close.assert_called_once()


def test_quit_socket_timeout_is_bounded_and_closes_connection(tmp_path):
    emu = Mock()
    daemon_identity(tmp_path, emu)
    emu.quit.side_effect = TimeoutError("quit timed out")
    with pytest.raises(dev.H.HarnessError, match="quit timed out"):
        dev.stop_connected(emu, tmp_path)
    emu._sock.settimeout.assert_called_once_with(5)
    emu.close.assert_called_once()


def test_shutdown_refuses_mismatched_discovery_identity(tmp_path):
    emu = Mock()
    daemon_identity(tmp_path, emu)
    emu.path = "/tmp/another.sock"
    with pytest.raises(dev.H.HarnessError, match="identity mismatch"):
        dev.stop_connected(emu, tmp_path)
    emu.quit.assert_not_called()


def test_start_mounts_prepared_card_without_repacking(monkeypatch, tmp_path):
    cell = dev.H.Cell("dn2", "1.12", ["test/ui-demo"])
    card = tmp_path / "store-card.img"
    emu = Mock()
    session = Mock(emu=emu, card=str(card))
    monkeypatch.setattr(dev.H, "Session", Mock(return_value=session))
    pack_card = Mock()
    monkeypatch.setattr(dev, "store_card", pack_card)
    assert dev.start(cell, tmp_path, ["test/sine*"], prepared_card=card) == (
        emu,
        session,
    )
    assert session.card_override == str(card)
    assert (tmp_path / "card.txt").read_text() == str(card)
    pack_card.assert_not_called()
    session.open.assert_called_once()
