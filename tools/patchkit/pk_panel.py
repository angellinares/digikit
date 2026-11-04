"""Open the DIGIKIT menu or SDK UI demo in a live browser faceplate."""

from __future__ import annotations

import argparse
import contextlib
import os
import signal
import socket
import subprocess
import sys

from tools.patchkit import pk_devreload as dev

PREVIEWS = {
    "menu": ("emulator", "digikit-menu"),
    "ui-demo": ("test/ui-demo", "ui-demo"),
}


def panel_socket(target: str) -> str:
    """Resolve only this preview's kept daemon, with dk:dev's automatic DSP mode."""
    dsp = dev.target_dsp_mode(target, "dn2", [])
    run_dir = dev.run_dir_of("dn2", target, dsp=dsp)
    emu = dev.connect(run_dir)
    if emu is None:
        raise dev.H.HarnessError(f"preview daemon is unavailable in {run_dir}")
    try:
        dev.assert_mode(emu, dsp)
        return emu.path
    finally:
        emu.close()


def serve(socket_path: str) -> int:
    """Keep Vite in the foreground; stop only its process group on interruption."""
    env = os.environ.copy()
    env["DIGI_LIVE_SOCKET"] = socket_path
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    proc = subprocess.Popen(
        [
            "pnpm",
            "--filter",
            "@digi/desktop",
            "exec",
            "vite",
            "--config",
            "ui/preview/live.vite.config.ts",
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
            "--strictPort",
            "--open",
            "/panel.html",
        ],
        cwd=dev.ROOT,
        env=env,
        start_new_session=True,
    )
    try:
        return proc.wait()
    except KeyboardInterrupt:
        return 130
    finally:
        # pnpm spawns Vite, so terminate the group even if pnpm has already exited.
        with contextlib.suppress(ProcessLookupError):
            os.killpg(proc.pid, signal.SIGTERM)
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(proc.pid, signal.SIGKILL)
            proc.wait()


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("preview", choices=PREVIEWS)
    args = parser.parse_args(argv)
    target, shot = PREVIEWS[args.preview]
    keys = f"tap:NO,tap:SETTINGS,wait:400,tap:DOWN*8,tap:YES,wait:600,shot:{shot}"
    try:
        result = dev.main([target, "--device", "dn2", "--keys", keys])
        if result:
            return result
        socket_path = panel_socket(target)
        print(
            "Opening the live faceplate; Ctrl-C stops the browser server and keeps the emulator.",
            flush=True,
        )
        return serve(socket_path)
    except (OSError, dev.EmuError, dev.H.HarnessError) as e:
        print(f"dk:{args.preview}: {e}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    os.chdir(dev.ROOT)
    sys.exit(main(sys.argv[1:]))
