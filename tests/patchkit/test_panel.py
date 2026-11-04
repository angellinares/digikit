"""Browser preview routing and launcher lifecycle, without booting firmware."""

import argparse
import signal
import subprocess
from unittest.mock import MagicMock, Mock, call

import pytest

from tools.patchkit import pk_panel as panel


@pytest.fixture(autouse=True)
def available_port(monkeypatch):
    listener = MagicMock()
    listener.__enter__.return_value.getsockname.return_value = ("127.0.0.1", 54321)
    monkeypatch.setattr(panel.socket, "socket", Mock(return_value=listener))


def test_navigation_isolates_new_daemon_and_keeps_group_on_success(monkeypatch):
    proc = Mock()
    proc.wait.return_value = 0
    popen = Mock(return_value=proc)
    killpg = Mock()
    monkeypatch.setattr(panel.subprocess, "Popen", popen)
    monkeypatch.setattr(panel.os, "killpg", killpg)
    assert panel.navigate(["emulator"]) == 0
    options = popen.call_args.kwargs
    assert options["start_new_session"] is True
    assert options["env"]["PYTHONPATH"].split(panel.os.pathsep)[0] == str(
        panel.dev.ROOT
    )
    assert options["cwd"] == panel.dev.ROOT
    proc.wait.assert_called_once_with(timeout=120)
    killpg.assert_not_called()


@pytest.mark.parametrize("interrupted", [False, True])
def test_unfinished_navigation_cleans_only_its_group(monkeypatch, interrupted):
    failure = (
        KeyboardInterrupt() if interrupted else subprocess.TimeoutExpired("dev", 120)
    )
    proc = Mock(pid=12345)
    proc.wait.side_effect = [failure, 0]
    monkeypatch.setattr(panel.subprocess, "Popen", Mock(return_value=proc))
    killpg = Mock()
    monkeypatch.setattr(panel.os, "killpg", killpg)
    expected = KeyboardInterrupt if interrupted else panel.dev.H.HarnessError
    with pytest.raises(expected):
        panel.navigate(["emulator"])
    killpg.assert_called_once_with(proc.pid, signal.SIGTERM)


@pytest.mark.parametrize("target", ["emulator", "test/ui-demo"])
def test_socket_comes_from_exact_auto_mode_daemon(monkeypatch, target):
    emu = Mock(path="/tmp/preview.sock")
    emu.status.return_value = {"dsp": "zero"}
    connect = Mock(return_value=emu)
    monkeypatch.setattr(panel.dev, "connect", connect)

    assert panel.panel_socket(target) == emu.path
    connect.assert_called_once_with(panel.dev.run_dir_of("dn2", target, dsp="zero"))
    emu.call.assert_called_once_with("run.mode", mode="free")
    emu.assert_has_calls(
        [call.status(), call.call("run.mode", mode="free"), call.close()]
    )
    emu.close.assert_called_once()
    emu.quit.assert_not_called()


def test_missing_preview_daemon_is_reported(monkeypatch, capsys):
    monkeypatch.setattr(panel, "navigate", Mock(return_value=0))
    monkeypatch.setattr(panel.dev, "connect", Mock(return_value=None))
    serve = Mock()
    monkeypatch.setattr(panel, "serve", serve)

    assert panel.main(["menu"]) == 2
    assert "preview daemon is unavailable" in capsys.readouterr().err
    serve.assert_not_called()


def test_wrong_mode_closes_connection(monkeypatch):
    emu = Mock()
    emu.status.return_value = {"dsp": "sharc", "audio": {}}
    monkeypatch.setattr(panel.dev, "connect", Mock(return_value=emu))
    with pytest.raises(panel.dev.H.HarnessError, match="expected DSP zero"):
        panel.panel_socket("emulator")
    emu.call.assert_not_called()
    emu.close.assert_called_once()


def test_failed_navigation_does_not_open_browser(monkeypatch):
    monkeypatch.setattr(panel, "navigate", Mock(return_value=2))
    socket = Mock()
    monkeypatch.setattr(panel, "panel_socket", socket)
    assert panel.main(["ui-demo"]) == 2
    socket.assert_not_called()


@pytest.mark.parametrize(
    "preview,target", [("menu", "emulator"), ("ui-demo", "test/ui-demo")]
)
def test_navigation_then_serving_same_target(monkeypatch, preview, target):
    events = []

    def navigate(argv):
        events.append("navigate")
        assert argv[0] == target
        steps = panel.dev.load_keys(argv[argv.index("--keys") + 1])
        assert {"tap": "SETTINGS"} in steps
        assert {"tap": "YES"} in steps
        return 0

    def resolve(actual):
        events.append("resolve")
        assert actual == target
        return "/tmp/exact-preview.sock"

    def serve(path):
        events.append("serve")
        assert path == "/tmp/exact-preview.sock"
        return 0

    monkeypatch.setattr(panel, "navigate", navigate)
    monkeypatch.setattr(panel, "panel_socket", resolve)
    monkeypatch.setattr(panel, "serve", serve)
    assert panel.main([preview]) == 0
    assert events == ["navigate", "resolve", "serve"]


@pytest.mark.parametrize("interrupted", [False, True])
def test_server_has_exact_socket_and_owns_cleanup(monkeypatch, capsys, interrupted):
    proc = Mock(pid=12345)
    proc.wait.side_effect = [KeyboardInterrupt() if interrupted else 0, 0]
    popen = Mock(return_value=proc)
    killpg = Mock()
    monkeypatch.setattr(panel.subprocess, "Popen", popen)
    monkeypatch.setattr(panel.os, "killpg", killpg)

    assert panel.serve("/tmp/exact-preview.sock") == (130 if interrupted else 0)
    assert "http://127.0.0.1:54321/panel.html" in capsys.readouterr().out
    options = popen.call_args.kwargs
    assert options["env"]["DIGI_LIVE_SOCKET"] == "/tmp/exact-preview.sock"
    assert options["cwd"] == panel.dev.ROOT
    assert options["start_new_session"] is True
    killpg.assert_called_once_with(proc.pid, signal.SIGTERM)


def test_server_escalates_only_own_unresponsive_group(monkeypatch):
    proc = Mock(pid=12345)
    proc.wait.side_effect = [
        KeyboardInterrupt(),
        subprocess.TimeoutExpired("vite", 5),
        0,
    ]
    monkeypatch.setattr(panel.subprocess, "Popen", Mock(return_value=proc))
    killpg = Mock()
    monkeypatch.setattr(panel.os, "killpg", killpg)
    assert panel.serve("/tmp/exact-preview.sock") == 130
    assert [call.args for call in killpg.call_args_list] == [
        (proc.pid, signal.SIGTERM),
        (proc.pid, signal.SIGKILL),
    ]


def test_server_start_error_is_reported(monkeypatch, capsys):
    monkeypatch.setattr(panel, "navigate", Mock(return_value=0))
    monkeypatch.setattr(
        panel, "panel_socket", Mock(return_value="/tmp/exact-preview.sock")
    )
    monkeypatch.setattr(
        panel.subprocess, "Popen", Mock(side_effect=FileNotFoundError("pnpm"))
    )
    assert panel.main(["menu"]) == 2
    assert "pnpm" in capsys.readouterr().err


@pytest.fixture
def module_tree(monkeypatch, tmp_path):
    manifests = {}

    def add(mid, kind="service", requires=()):
        folder = tmp_path / mid
        folder.mkdir(parents=True, exist_ok=True)
        (folder / "module.toml").write_text(
            f'[module]\nid = "{mid}"\ntype = "{kind}"\nrequires = {list(requires)!r}\n'
        )
        manifests[mid] = folder

    monkeypatch.setattr(panel.dev.modroots, "manifests", lambda: manifests)
    monkeypatch.setattr(
        panel.dev, "resolve_modules", lambda target, device: ["core/base"]
    )
    add("core/base", "framework")
    return add


def test_module_alias_and_exact_id(module_tree):
    module_tree("example/voice")
    assert panel.resolve_module("voice", "dn2") == "example/voice"
    assert panel.resolve_module("example/voice", "dn2") == "example/voice"
    module_tree("other/voice")
    with pytest.raises(
        panel.dev.H.HarnessError, match="ambiguous.*example/voice.*other/voice"
    ):
        panel.resolve_module("voice", "dn2")
    assert panel.resolve_module("example/voice", "dn2") == "example/voice"
    with pytest.raises(panel.dev.H.HarnessError, match="unknown module"):
        panel.resolve_module("absent", "dn2")


@pytest.mark.parametrize("kind", ["framework", "raw"])
def test_module_rejects_non_runtime_kind(module_tree, kind):
    module_tree("example/preview", kind)
    with pytest.raises(
        panel.dev.H.HarnessError, match="runtime machine, fx or service"
    ):
        panel.resolve_module("preview", "dn2")


@pytest.mark.parametrize("kind", ["machine", "fx", "service"])
def test_module_accepts_runtime_kinds_and_transitive_base_dependencies(
    module_tree, kind
):
    module_tree("core/dependency", "framework")
    module_tree("core/base", "framework", ["core/dependency"])
    module_tree("example/preview", kind, ["core/base"])
    assert panel.resolve_module("preview", "dn2") == "example/preview"


def test_module_rejects_dependency_missing_from_base(module_tree):
    module_tree("core/missing", "framework")
    module_tree("example/dependency", requires=["core/missing"])
    module_tree("example/preview", requires=["example/dependency"])
    with pytest.raises(
        panel.dev.H.HarnessError,
        match="dependencies absent.*core/missing.*example/dependency",
    ):
        panel.resolve_module("preview", "dn2")


@pytest.fixture
def module_launch(monkeypatch, tmp_path, module_tree):
    module_tree("example/preview", "machine")
    run_dir = tmp_path / "kept"
    staged = run_dir / "plus_drive" / "digikit" / "modules" / "preview.dkm"
    staged.parent.mkdir(parents=True)
    staged.write_bytes(b"previous")
    tools = Mock()
    monkeypatch.setattr(panel.pk_dkrt, "Tools", Mock(return_value=tools))
    directory = Mock(
        side_effect=lambda device, target, store, *, dsp: (
            run_dir if dsp == "zero" else tmp_path / f"kept-{dsp}"
        )
    )
    mode = Mock(return_value="zero")
    monkeypatch.setattr(panel.dev, "run_dir_of", directory)
    monkeypatch.setattr(panel.dev, "target_dsp_mode", mode)
    monkeypatch.setattr(panel.dev, "connect", Mock(return_value=Mock()))
    stop = Mock()
    monkeypatch.setattr(panel, "stop_exact", stop)

    launch_cli = argparse.ArgumentParser()
    launch_cli.add_argument("target")
    launch_cli.add_argument("--device")
    launch_cli.add_argument("--store")
    launch_cli.add_argument("--keys")
    launch_cli.add_argument("--store-ui-preview", action="store_true")

    def start(args):
        navigate.request = launch_cli.parse_args(args)
        staged.write_bytes(b"candidate")
        return 0

    navigate = Mock(side_effect=start)
    monkeypatch.setattr(panel, "navigate", navigate)
    socket = Mock(return_value="/tmp/module.sock")
    monkeypatch.setattr(panel, "panel_socket", socket)
    serve = Mock(return_value=0)
    monkeypatch.setattr(panel, "serve", serve)
    return run_dir, staged, tools, directory, mode, stop, navigate, socket, serve


@pytest.mark.parametrize("device", ["dn2", "dt2"])
def test_module_delegates_store_refresh_to_devreload(module_launch, device):
    run_dir, staged, tools, directory, mode, stop, navigate, socket, serve = (
        module_launch
    )
    assert panel.main(["module", "preview", "--device", device]) == 0
    mode.assert_called_once_with("emulator", device, [])
    assert directory.call_args_list == [
        call(device, "emulator", ["example/preview*"], dsp=mode)
        for mode in ("sharc", None)
    ]
    stopped = [entry.args[0] for entry in stop.call_args_list]
    assert stopped[:2] == [run_dir.parent / "kept-sharc", run_dir.parent / "kept-None"]
    assert run_dir not in stopped
    assert navigate.request.device == device
    assert navigate.request.target == "emulator"
    assert navigate.request.store == "example/preview*"
    socket.assert_called_once_with("emulator", device, ["example/preview*"], dsp="zero")
    assert navigate.request.store_ui_preview
    serve.assert_called_once_with("/tmp/module.sock")
    tools.patchkit_run.assert_not_called()


def test_module_missing_daemon_always_builds_fresh_card(monkeypatch, module_launch):
    run_dir, _, _, _, _, stop, navigate, _, serve = module_launch
    monkeypatch.setattr(panel.dev, "connect", Mock(return_value=None))
    assert panel.main(["module", "preview"]) == 0
    assert run_dir not in [entry.args[0] for entry in stop.call_args_list]
    navigate.assert_called_once()
    serve.assert_called_once()


def test_module_failed_navigation_leaves_other_modes_running(module_launch):
    _, _, tools, _, _, stop, navigate, socket, serve = module_launch
    navigate.side_effect = None
    navigate.return_value = 7
    assert panel.main(["module", "preview"]) == 7
    stop.assert_not_called()
    tools.patchkit_run.assert_not_called()
    socket.assert_not_called()
    serve.assert_not_called()


def test_module_cleans_other_modes_after_navigation_before_serving(module_launch):
    _, _, _, _, _, stop, navigate, _, serve = module_launch
    events = Mock()
    events.attach_mock(navigate, "navigate")
    events.attach_mock(stop, "stop")
    events.attach_mock(serve, "serve")
    assert panel.main(["module", "preview"]) == 0
    assert [entry[0] for entry in events.mock_calls] == [
        "navigate",
        "stop",
        "stop",
        "serve",
    ]


def test_module_stop_does_not_pack_navigate_or_serve(module_launch):
    run_dir, _, tools, _, _, stop, navigate, socket, serve = module_launch
    assert panel.main(["module", "preview", "--stop"]) == 0
    assert stop.call_args_list == [
        call(run_dir),
        call(run_dir.parent / "kept-sharc"),
        call(run_dir.parent / "kept-None"),
    ]
    tools.patchkit_run.assert_not_called()
    navigate.assert_not_called()
    socket.assert_not_called()
    serve.assert_not_called()


def test_exact_stop_waits_for_only_named_daemon(monkeypatch, tmp_path):
    emu = Mock()
    lingering = Mock()
    connect = Mock(side_effect=[emu, lingering, None])
    monkeypatch.setattr(panel.dev, "connect", connect)
    monkeypatch.setattr(panel.time, "sleep", Mock())
    panel.stop_exact(tmp_path)
    assert connect.call_args_list == [call(tmp_path)] * 3
    emu.quit.assert_called_once()
    emu.close.assert_called_once()
    lingering.quit.assert_not_called()
    lingering.close.assert_called_once()


def test_exact_stop_failure_is_bounded(monkeypatch, tmp_path):
    emu = Mock()
    monkeypatch.setattr(panel.dev, "connect", Mock(return_value=emu))
    monkeypatch.setattr(panel.time, "monotonic", Mock(side_effect=[0, 6]))
    with pytest.raises(panel.dev.H.HarnessError, match="did not stop"):
        panel.stop_exact(tmp_path)
    emu.quit.assert_called_once()


def test_module_socket_uses_same_device_store_and_dsp(monkeypatch, tmp_path):
    emu = Mock(path="/tmp/module.sock")
    emu.status.return_value = {"dsp": "sharc", "audio": {}}
    directory = Mock(return_value=tmp_path)
    mode = Mock(return_value="sharc")
    connect = Mock(return_value=emu)
    monkeypatch.setattr(panel.dev, "run_dir_of", directory)
    monkeypatch.setattr(panel.dev, "target_dsp_mode", mode)
    monkeypatch.setattr(panel.dev, "connect", connect)
    assert panel.panel_socket("emulator", "dt2", ["example/preview*"]) == emu.path
    mode.assert_called_once_with("emulator", "dt2", ["example/preview*"])
    directory.assert_called_once_with(
        "dt2", "emulator", ["example/preview*"], dsp="sharc"
    )
    connect.assert_called_once_with(tmp_path)
    emu.close.assert_called_once()


@pytest.mark.parametrize("old_mode,new_mode", [("zero", "sharc"), ("sharc", "zero")])
@pytest.mark.parametrize("stop_requested", [False, True])
def test_module_mode_transition_cleans_only_matching_module_daemons(
    monkeypatch, tmp_path, module_launch, old_mode, new_mode, stop_requested
):
    _, _, _, directory, mode, stop, navigate, _, serve = module_launch

    def location(device, target, store, *, dsp):
        return tmp_path / f"{device}-{target}-{','.join(store)}-{dsp}"

    directory.side_effect = location
    mode.return_value = new_mode
    store = ["example/preview*"]
    previous = location("dn2", "emulator", store, dsp=old_mode)
    current = location("dn2", "emulator", store, dsp=new_mode)
    legacy = location("dn2", "emulator", store, dsp=None)
    unrelated = {
        location("dt2", "emulator", store, dsp=old_mode),
        location("dn2", "emulator", ["other/preview*"], dsp=old_mode),
        location("dn2", "other-set", store, dsp=old_mode),
    }
    active = {previous, current, legacy, *unrelated}
    stop.side_effect = lambda path: active.discard(path)
    monkeypatch.setattr(
        panel.dev, "connect", lambda path: Mock() if path in active else None
    )
    staged = current / "plus_drive" / "digikit" / "modules" / "preview.dkm"
    staged.parent.mkdir(parents=True)
    staged.write_bytes(b"candidate")

    def start(args):
        assert previous in active  # failed preparation must leave prior modes alive
        assert legacy in active
        assert current in active  # identical current card stays alive
        return 0

    navigate.side_effect = start
    args = ["module", "preview"] + (["--stop"] if stop_requested else [])
    if new_mode == "sharc":
        args.append("--dsp")
    assert panel.main(args) == 0
    assert unrelated <= active
    assert previous not in active
    assert legacy not in active
    assert (current in active) is not stop_requested
    if stop_requested:
        navigate.assert_not_called()
        serve.assert_not_called()
    else:
        navigate.assert_called_once()
        serve.assert_called_once()


@pytest.mark.parametrize("edit", ["type", "dependency"])
def test_module_stop_skips_launch_eligibility(module_tree, module_launch, edit):
    _, _, tools, _, mode, stop, navigate, _, serve = module_launch
    if edit == "type":
        module_tree("example/preview", "framework")
    else:
        module_tree("core/not-in-base", "framework")
        module_tree("example/preview", requires=["core/not-in-base"])
    assert panel.main(["module", "preview", "--stop"]) == 0
    assert stop.call_count == 3
    mode.assert_not_called()
    tools.patchkit_run.assert_not_called()
    navigate.assert_not_called()
    serve.assert_not_called()
    with pytest.raises(panel.dev.H.HarnessError):
        panel.resolve_module("preview", "dn2")


def test_module_stop_with_removed_manifest_needs_canonical_id(
    monkeypatch, module_launch
):
    _, _, tools, directory, mode, stop, navigate, _, serve = module_launch
    monkeypatch.setattr(panel.dev.modroots, "manifests", lambda: {})
    assert panel.main(["module", "example/preview", "--stop"]) == 0
    assert directory.call_args_list == [
        call("dn2", "emulator", ["example/preview*"], dsp=dsp)
        for dsp in ("zero", "sharc", None)
    ]
    assert stop.call_count == 3
    mode.assert_not_called()
    tools.patchkit_run.assert_not_called()
    navigate.assert_not_called()
    serve.assert_not_called()
    stop.reset_mock()
    assert panel.main(["module", "preview", "--stop"]) == 2
    stop.assert_not_called()


@pytest.mark.parametrize("kind,mode", [("fx", "zero"), ("service", "sharc")])
def test_default_rejects_effects_and_dsp_services(
    module_tree, module_launch, capsys, kind, mode
):
    _, _, tools, _, auto_mode, _, navigate, _, serve = module_launch
    module_tree("example/preview", kind)
    auto_mode.return_value = mode
    assert panel.main(["module", "preview"]) == 2
    assert "use --dsp" in capsys.readouterr().err
    tools.patchkit_run.assert_not_called()
    navigate.assert_not_called()
    serve.assert_not_called()


def test_default_zero_service_uses_normal_pack(module_tree, module_launch, capsys):
    _, _, tools, _, mode, _, navigate, socket, _ = module_launch
    module_tree("example/preview", "service")
    assert panel.main(["module", "preview"]) == 0
    mode.assert_called_once_with("emulator", "dn2", ["example/preview*"])
    tools.patchkit_run.assert_not_called()
    assert not navigate.request.store_ui_preview
    assert "zero DSP, no audio" in capsys.readouterr().out
    socket.assert_called_once_with("emulator", "dn2", ["example/preview*"], dsp="zero")


@pytest.mark.parametrize("kind", ["machine", "fx", "service"])
def test_explicit_dsp_keeps_full_pack(module_tree, module_launch, kind):
    run_dir, _, tools, directory, mode, _, navigate, socket, _ = module_launch
    mode.return_value = "sharc"
    directory.side_effect = lambda device, target, store, *, dsp: (
        run_dir if dsp == "sharc" else run_dir.parent / f"kept-{dsp}"
    )
    module_tree("example/preview", kind)
    assert panel.main(["module", "preview", "--dsp"]) == 0
    mode.assert_called_once_with("emulator", "dn2", ["example/preview*"])
    tools.patchkit_run.assert_not_called()
    assert not navigate.request.store_ui_preview
    socket.assert_called_once_with("emulator", "dn2", ["example/preview*"], dsp="sharc")
