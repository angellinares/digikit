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

    tools = Mock()
    monkeypatch.setattr(dev, "pack_store", pack)
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

    tools = Mock()
    monkeypatch.setattr(dev, "pack_store", pack)
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
    store_build, tmp_path, failure, monkeypatch
):
    packed, _, card_build, selection, stop = store_build
    run_dir = tmp_path / "kept"
    _, card = refresh(run_dir)
    original = card.read_bytes()
    baseline = (run_dir / "store-request.json").read_bytes()
    packed["bytes"] = b"updated"
    ids = None
    if failure == "pack":
        monkeypatch.setattr(dev, "pack_store", Mock(side_effect=RuntimeError("pack")))
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
    monkeypatch.setattr(dev, "cached_runtime_image", Mock(return_value=None))
    monkeypatch.setattr(dev, "store_card", pack_card)
    assert dev.start(cell, tmp_path, ["test/sine*"], prepared_card=card) == (
        emu,
        session,
    )
    assert session.card_override == str(card)
    assert (tmp_path / "card.txt").read_text() == str(card)
    pack_card.assert_not_called()
    session.open.assert_called_once()


def test_same_session_launchers_wait_but_other_sessions_do_not(monkeypatch, tmp_path):
    from concurrent.futures import ThreadPoolExecutor
    from threading import Event

    blocked, entered = Event(), Event()
    real_flock = dev.flock

    def observe(lock, operation):
        try:
            return real_flock(lock, operation)
        except BlockingIOError:
            blocked.set()
            raise

    monkeypatch.setattr(dev, "flock", observe)

    def second():
        with dev.session_lock(tmp_path / "same"):
            entered.set()

    with ThreadPoolExecutor(max_workers=1) as pool:
        with dev.session_lock(tmp_path / "same"):
            pending = pool.submit(second)
            assert blocked.wait(5)
            assert not entered.is_set()
            with dev.session_lock(tmp_path / "other", timeout=0):
                pass
        pending.result(timeout=5)
    assert entered.is_set()


def test_session_startup_lock_timeout_leaves_holder_intact(tmp_path):
    with (
        dev.session_lock(tmp_path),
        pytest.raises(dev.H.HarnessError, match="startup did not finish"),
        dev.session_lock(tmp_path, timeout=0),
    ):
        pytest.fail("same session acquired twice")
    with dev.session_lock(tmp_path, timeout=0):
        pass


def test_reload_waits_for_busy_without_cancelling_job(monkeypatch):
    emu = Mock()
    emu.call.side_effect = [dev.EmuError(3, "a job is running"), {"path": "none"}]
    monkeypatch.setattr(dev.time, "sleep", Mock())
    assert dev.reload_ready(emu, Mock()) == {"path": "none"}
    assert emu.call.call_count == 2
    emu.quit.assert_not_called()


def test_reload_busy_deadline_preserves_running_job(monkeypatch):
    emu = Mock()
    emu.call.side_effect = dev.EmuError(3, "a job is running")
    monkeypatch.setattr(dev.time, "monotonic", Mock(side_effect=[0, 61]))
    with pytest.raises(dev.H.HarnessError, match="running job was left intact"):
        dev.reload_ready(emu, Mock())
    emu.quit.assert_not_called()


def test_reload_recovers_only_missing_record_and_reapplies_card(monkeypatch, tmp_path):
    from unittest.mock import call

    emu = Mock()
    emu.call.side_effect = [
        dev.EmuError(4, "dev.reload: the running image has no patch record"),
        {"start_used": "snapshot"},
        {"path": "none"},
    ]
    session = object.__new__(dev.H.Session)
    session.emu = emu
    session.cell = Mock(stall_ms=2000)
    session.syx = tmp_path / "stock.syx"
    session.run_dir = tmp_path
    session.card = str(tmp_path / "kept-card.img")
    session.cache_dir = None
    session.timings = {}
    image = tmp_path / "recovery/patched.syx"
    patch = Mock(return_value=image)
    monkeypatch.setattr(dev.H, "patched_image", patch)
    assert dev.reload_ready(emu, session) == {"path": "none"}
    patch.assert_called_once_with(session.cell, session.syx, tmp_path / "recovery")
    assert emu.call.call_args_list[1] == call(
        "firmware.load",
        syx=str(image),
        force=True,
        auto_no=True,
        start="auto",
        card=session.card,
    )
    emu.quit.assert_not_called()


def test_reload_preserves_other_rejections():
    emu = Mock()
    error = dev.EmuError(4, "new module does not fit")
    emu.call.side_effect = error
    session = Mock()
    with pytest.raises(dev.EmuError, match="does not fit"):
        dev.reload_ready(emu, session)
    session.load.assert_not_called()


def test_build_only_invokes_builder_for_stale_modules(monkeypatch, tmp_path):
    folders = {mid: tmp_path / mid for mid in ("core/base", "core/ui")}
    monkeypatch.setattr(dev.modroots, "module_dir", folders.__getitem__)
    monkeypatch.setattr(
        dev.patchkit,
        "module_statuses",
        Mock(
            return_value={
                folders["core/base"]: "unchanged",
                folders["core/ui"]: "needs build",
            }
        ),
    )
    builder = Mock(return_value=["built ui"])
    monkeypatch.setattr(dev.patchkit, "build_module", builder)
    assert dev.build(list(folders))[1] == ["core/ui"]
    builder.assert_called_once_with(folders["core/ui"], check=False)


@pytest.mark.parametrize("change", ["none", "inputs", "image", "busy", "missing"])
def test_preview_warm_reuse_checks_inputs_and_live_image(monkeypatch, tmp_path, change):
    image = tmp_path / "patched.syx"
    image.write_bytes(b"runtime")
    digest = dev.hashlib.sha256(image.read_bytes()).hexdigest()
    cached = Mock(return_value=image)
    monkeypatch.setattr(dev, "cached_runtime_image", cached)
    reload = Mock(return_value={"path": "boot"})
    monkeypatch.setattr(dev, "reload_ready", reload)
    monkeypatch.setattr(dev.H, "patched_image", Mock(return_value=image))
    emu = Mock()
    status = {"firmware": {"syx_sha256": digest}}
    emu.status.return_value = status
    session = Mock(run_dir=tmp_path)
    assert dev.preview_reload(emu, session, [], True)["path"] == "boot"
    cached.assert_not_called()
    if change == "inputs":
        image.write_bytes(b"new runtime")
    elif change == "image":
        status["firmware"]["syx_sha256"] = "stock-image"
    elif change == "busy":
        emu.status.side_effect = [{"job": {"kind": "running"}}, status]
        monkeypatch.setattr(dev.time, "sleep", Mock())
    elif change == "missing":
        cached.return_value = None
    assert dev.preview_reload(emu, session, [], False)["path"] == (
        "kept" if change in ("none", "busy") else "boot"
    )
    reload.assert_not_called()
    assert session.load.call_count == (
        1 if change in ("inputs", "image", "missing") else 0
    )


def test_card_restart_reuses_firmware_but_changed_runtime_repatches(
    monkeypatch, tmp_path
):
    fingerprint = Mock(return_value="original")
    monkeypatch.setattr(dev, "runtime_fingerprint", fingerprint)
    image = tmp_path / "runtime/patched.syx"

    def patch(*args):
        image.parent.mkdir(exist_ok=True)
        image.write_bytes(fingerprint.return_value.encode())
        return image

    patcher = Mock(side_effect=patch)
    monkeypatch.setattr(dev.H, "patched_image", patcher)
    session = Mock(run_dir=tmp_path)
    assert dev.cached_runtime_image(session) == image
    assert dev.cached_runtime_image(session) == image
    patcher.assert_called_once()
    fingerprint.return_value = "core changed"
    assert dev.cached_runtime_image(session) == image
    assert patcher.call_count == 2
    image.write_bytes(b"corrupt cached image")
    assert dev.cached_runtime_image(session) == image
    assert patcher.call_count == 3


def test_module_preview_uploads_to_mounted_card_without_rebuilding_or_stopping(
    store_build, monkeypatch, tmp_path
):
    packed, _, card_build, _, stop = store_build
    run_dir = tmp_path / "kept"
    _, card = refresh(run_dir)
    original = card.read_bytes()
    packed["bytes"] = b"updated"
    update = Mock()
    monkeypatch.setattr(dev, "upload_store", update)
    emu = Mock()
    result = dev.refresh_store(
        emu, "dn2", "emulator", ["test/sine*"], run_dir, False, False, live_update=True
    )
    assert result == (emu, card)
    update.assert_called_once()
    assert update.call_args.args[0] is emu
    assert card.read_bytes() == original
    card_build.assert_called_once()
    stop.assert_not_called()
    assert refresh(run_dir, emu) == (emu, card)
    stop.assert_not_called()


def test_failed_upload_does_not_commit_candidate_or_replace_card(
    store_build, monkeypatch, tmp_path
):
    packed, _, card_build, _, stop = store_build
    run_dir = tmp_path / "kept"
    _, card = refresh(run_dir)
    baseline = (run_dir / "store-request.json").read_bytes()
    original = card.read_bytes()
    packed["bytes"] = b"updated"
    monkeypatch.setattr(
        dev, "upload_store", Mock(side_effect=dev.H.HarnessError("verify failed"))
    )
    with pytest.raises(dev.H.HarnessError, match="verify failed"):
        dev.refresh_store(
            Mock(),
            "dn2",
            "emulator",
            ["test/sine*"],
            run_dir,
            False,
            False,
            live_update=True,
        )
    assert (run_dir / "store-request.json").read_bytes() == baseline
    assert card.read_bytes() == original
    card_build.assert_called_once()
    stop.assert_not_called()


def test_uploaded_preview_keeps_saved_overlay_when_daemon_is_absent(
    store_build, tmp_path
):
    _, _, card_build, _, _ = store_build
    run_dir = tmp_path / "kept"
    _, card = refresh(run_dir)
    overlay = run_dir / "overlay.bin"
    overlay.write_bytes(b"uploaded DKM and user data")
    assert refresh(run_dir) == (None, card)
    assert overlay.read_bytes() == b"uploaded DKM and user data"
    card_build.assert_called_once()


@pytest.mark.parametrize("failure", [None, "missing", "boot", "no_boot"])
def test_upload_store_waits_for_boot_and_module_without_debug_log(
    monkeypatch, tmp_path, capsys, failure
):
    from tools.device import dkrun, dktransfer, dkupload

    source = tmp_path / "plus_drive/digikit/modules/voice.dkm"
    source.parent.mkdir(parents=True)
    source.write_bytes(b"packed module")
    emu = Mock()
    emu.status.return_value = {"job": None}
    emu.call.return_value = {"previous": "free"}
    elapsed = 0.0

    def step(_instructions):
        nonlocal elapsed
        elapsed += 0.5

    emu.step.side_effect = step
    monkeypatch.setattr(dev.time, "monotonic", lambda: elapsed)
    if failure == "no_boot":
        emu.drain_events.return_value = []
    else:
        emu.drain_events.side_effect = [
            [],  # Clear old events before requesting the restart.
            [{"kind": "firmware.loaded", "ok": failure != "boot"}],
        ]
    up = Mock()
    up.verify.return_value = dev.hashlib.sha256(source.read_bytes()).hexdigest()
    up.modules.side_effect = (
        None
        if failure == "missing"
        else [{"modules": []}, {"modules": [{"name": "VOICE"}]}]
    )
    up.modules.return_value = {"modules": []}
    up.ls.return_value = [("modules/voice.dkm", 0, "")]
    up.log.side_effect = AssertionError("preview must not require a debug log")
    monkeypatch.setattr(dkupload, "Uploader", Mock(return_value=up))
    monkeypatch.setattr(dkrun, "require_runtime", Mock())
    upload = Mock()
    monkeypatch.setattr(dkrun, "upload_if_changed", upload)
    monkeypatch.setattr(dktransfer, "header_info", Mock(return_value={"name": "VOICE"}))
    legacy_restart = Mock(
        side_effect=AssertionError("preview must not wait for BOOT_START")
    )
    monkeypatch.setattr(dktransfer, "restart_device", legacy_restart)
    events = Mock()
    for name, obj in (
        ("upload", upload),
        ("verify", up.verify),
        ("select", up.select),
        ("restart", up.restart),
    ):
        events.attach_mock(obj, name)
    if failure:
        reason = {
            "missing": "MODULES does not list VOICE.*files are on the card but did not load",
            "boot": "cold boot failed",
            "no_boot": "emulator did not boot again",
        }[failure]
        with pytest.raises(dev.H.HarnessError, match=reason):
            dev.upload_store(emu, tmp_path, {"selected": ["modules/voice.dkm"]})
    else:
        dev.upload_store(emu, tmp_path, {"selected": ["modules/voice.dkm"]})
        assert "preview: module ready (VOICE)" in capsys.readouterr().out
    if failure == "no_boot":
        assert elapsed == 90
    else:
        assert elapsed < 22
    if failure in ("boot", "no_boot"):
        up.modules.assert_not_called()
    assert [c[0] for c in events.mock_calls] == [
        "upload",
        "verify",
        "select",
        "restart",
    ]
    emu.quit.assert_not_called()
    emu.restart.assert_not_called()
    up.log.assert_not_called()
    legacy_restart.assert_not_called()
    assert emu.call.call_args_list[-1].kwargs == {"mode": "free"}


def test_missing_card_rebuilds_before_stopping_live_preview(store_build, tmp_path):
    _, _, card_build, _, stop = store_build
    run_dir = tmp_path / "kept"
    _, card = refresh(run_dir)
    card.unlink()
    emu = Mock()
    assert dev.refresh_store(
        emu, "dn2", "emulator", ["test/sine*"], run_dir, False, False, live_update=True
    ) == (None, card)
    assert card.is_file()
    assert card_build.call_count == 2
    stop.assert_called_once_with(emu, run_dir)


@pytest.mark.parametrize(
    "changed", ["object", "manifest", "sdk", "port", "packer", "stock"]
)
def test_runtime_cache_key_tracks_patch_inputs(monkeypatch, tmp_path, changed):
    monkeypatch.setattr(dev, "ROOT", tmp_path)
    folder = tmp_path / "module"
    folder.mkdir()
    manifest = folder / "module.toml"
    manifest.write_text('[module]\nrequires = []\n[[object]]\npath = "object.json"\n')
    inputs = {
        "manifest": manifest,
        "object": folder / "object.json",
        "sdk": tmp_path / "sdk/sdk.toml",
        "port": tmp_path / "devices/ports/current.toml",
        "packer": tmp_path / "patchkit-bin",
        "stock": tmp_path / "stock.syx",
    }
    for path in [
        *inputs.values(),
        tmp_path / "Cargo.lock",
        tmp_path / "packages/patchkit/Cargo.toml",
    ]:
        path.parent.mkdir(parents=True, exist_ok=True)
        if path != manifest:
            path.write_bytes(b"original")
    monkeypatch.setattr(dev.modroots, "module_dir", lambda mid: folder)
    monkeypatch.setattr(
        dev.modroots, "object_path", lambda data, folder, obj: folder / obj["path"]
    )
    monkeypatch.setattr(dev.patchkit, "binary", lambda: inputs["packer"])
    before = dev.runtime_fingerprint(["core/base"], "dn2", inputs["stock"])
    assert before
    with inputs[changed].open("ab") as content:
        content.write(b"\n# changed")
    assert dev.runtime_fingerprint(["core/base"], "dn2", inputs["stock"]) != before
    inputs["object"].unlink()
    assert dev.runtime_fingerprint(["core/base"], "dn2", inputs["stock"]) is None


def test_lost_upload_overlay_rebuilds_card_instead_of_using_old_module(
    store_build, monkeypatch, tmp_path
):
    packed, _, card_build, _, _ = store_build
    run_dir = tmp_path / "kept"
    _, card = refresh(run_dir)
    packed["bytes"] = b"updated"
    monkeypatch.setattr(dev, "upload_store", Mock())
    dev.refresh_store(
        Mock(),
        "dn2",
        "emulator",
        ["test/sine*"],
        run_dir,
        False,
        False,
        live_update=True,
    )
    assert (run_dir / "store-overlay-required").is_file()
    assert refresh(run_dir) == (None, card)
    assert b"updated" in card.read_bytes()
    assert card_build.call_count == 2
    assert not (run_dir / "store-overlay-required").exists()
