"""Browser preview routing and launcher lifecycle, without booting firmware."""

import signal
import subprocess
from unittest.mock import MagicMock, Mock

import pytest

from tools.patchkit import pk_panel as panel


@pytest.fixture(autouse=True)
def available_port(monkeypatch):
    listener = MagicMock()
    listener.__enter__.return_value.getsockname.return_value = ("127.0.0.1", 54321)
    monkeypatch.setattr(panel.socket, "socket", Mock(return_value=listener))


@pytest.mark.parametrize("target", ["emulator", "test/ui-demo"])
def test_socket_comes_from_exact_auto_mode_daemon(monkeypatch, target):
    emu = Mock(path="/tmp/preview.sock")
    emu.status.return_value = {"dsp": "zero"}
    connect = Mock(return_value=emu)
    monkeypatch.setattr(panel.dev, "connect", connect)

    assert panel.panel_socket(target) == emu.path
    connect.assert_called_once_with(panel.dev.run_dir_of("dn2", target, dsp="zero"))
    emu.close.assert_called_once()
    emu.quit.assert_not_called()


def test_missing_preview_daemon_is_reported(monkeypatch, capsys):
    monkeypatch.setattr(panel.dev, "main", Mock(return_value=0))
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
    emu.close.assert_called_once()


def test_failed_navigation_does_not_open_browser(monkeypatch):
    monkeypatch.setattr(panel.dev, "main", Mock(return_value=2))
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

    monkeypatch.setattr(panel.dev, "main", navigate)
    monkeypatch.setattr(panel, "panel_socket", resolve)
    monkeypatch.setattr(panel, "serve", serve)
    assert panel.main([preview]) == 0
    assert events == ["navigate", "resolve", "serve"]


@pytest.mark.parametrize("interrupted", [False, True])
def test_server_has_exact_socket_and_owns_cleanup(monkeypatch, interrupted):
    proc = Mock(pid=12345)
    proc.wait.side_effect = [KeyboardInterrupt() if interrupted else 0, 0]
    popen = Mock(return_value=proc)
    killpg = Mock()
    monkeypatch.setattr(panel.subprocess, "Popen", popen)
    monkeypatch.setattr(panel.os, "killpg", killpg)

    assert panel.serve("/tmp/exact-preview.sock") == (130 if interrupted else 0)
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
    monkeypatch.setattr(panel.dev, "main", Mock(return_value=0))
    monkeypatch.setattr(
        panel, "panel_socket", Mock(return_value="/tmp/exact-preview.sock")
    )
    monkeypatch.setattr(
        panel.subprocess, "Popen", Mock(side_effect=FileNotFoundError("pnpm"))
    )
    assert panel.main(["menu"]) == 2
    assert "pnpm" in capsys.readouterr().err
