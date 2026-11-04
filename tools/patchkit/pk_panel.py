"""Open the DIGIKIT menu or SDK UI demo in a live browser faceplate."""

from __future__ import annotations

import argparse
import contextlib
import os
import re
import signal
import socket
import subprocess
import sys
import time
import tomllib
from pathlib import Path

from tools import python_env
from tools.patchkit import pk_devreload as dev
from tools.patchkit import pk_dkrt

PREVIEWS = {
    "menu": ("emulator", "digikit-menu"),
    "ui-demo": ("test/ui-demo", "ui-demo"),
}


def stop_group(proc: subprocess.Popen) -> None:
    """Stop only this launcher's child group, including pnpm's Vite child."""
    with contextlib.suppress(ProcessLookupError):
        os.killpg(proc.pid, signal.SIGTERM)
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        with contextlib.suppress(ProcessLookupError):
            os.killpg(proc.pid, signal.SIGKILL)
        proc.wait()


def navigate(argv: list[str]) -> int:
    """Isolate the startup group so a newly kept daemon survives terminal Ctrl-C."""
    proc = subprocess.Popen(
        [sys.executable, "-m", "tools.patchkit.pk_devreload", *argv],
        cwd=dev.ROOT,
        env=python_env(),
        start_new_session=True,
    )
    try:
        return proc.wait(timeout=120)
    except (KeyboardInterrupt, subprocess.TimeoutExpired) as e:
        stop_group(proc)
        if isinstance(e, subprocess.TimeoutExpired):
            raise dev.H.HarnessError("preview startup exceeded 120 seconds") from e
        raise


def panel_socket(
    target: str,
    device: str = "dn2",
    store: list[str] | None = None,
    *,
    dsp: str | None = None,
) -> str:
    """Resolve only this preview's kept daemon, with dk:dev's automatic DSP mode."""
    dsp = dsp or dev.target_dsp_mode(target, device, store or [])
    run_dir = dev.run_dir_of(device, target, store, dsp=dsp)
    emu = dev.connect(run_dir)
    if emu is None:
        raise dev.H.HarnessError(f"preview daemon is unavailable in {run_dir}")
    try:
        dev.assert_mode(emu, dsp)
        emu.call("run.mode", mode="free")
        return emu.path
    finally:
        emu.close()


def resolve_module_id(name: str, *, allow_missing: bool = False) -> str:
    """Resolve aliases; stopping by canonical id needs no surviving manifest."""
    if allow_missing and re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", name):
        return name
    manifests = dev.modroots.manifests()
    matches = [mid for mid in manifests if mid.rpartition("/")[2] == name]
    if name in manifests:
        mid = name
    elif len(matches) == 1:
        mid = matches[0]
    elif matches:
        raise dev.H.HarnessError(
            f"ambiguous module {name!r}: {', '.join(sorted(matches))}"
        )
    else:
        raise dev.H.HarnessError(f"unknown module {name!r}")
    return mid


def resolve_module(name: str, device: str) -> str:
    """Accept a canonical runtime module id or an unambiguous leaf alias."""
    mid = resolve_module_id(name)
    manifest = tomllib.loads((dev.modroots.module_dir(mid) / "module.toml").read_text())
    kind = manifest["module"].get("type")
    if kind not in ("machine", "fx", "service"):
        raise dev.H.HarnessError(
            f"{mid}: dk:module needs a runtime machine, fx or service (got {kind!r})"
        )
    available = set(dev.closure(dev.resolve_modules("emulator", device)))
    required = set(dev.closure([mid])) - {mid}
    missing = required - available
    if missing:
        raise dev.H.HarnessError(
            f"{mid}: dependencies absent from {device} emulator patch set: "
            + ", ".join(sorted(missing))
        )
    return mid


def stop_exact(run_dir: Path) -> None:
    """Ask only the daemon in this directory to quit, then await its shutdown."""
    emu = dev.connect(run_dir)
    if emu is None:
        return
    try:
        emu.quit()
    finally:
        emu.close()
    deadline = time.monotonic() + 5
    while True:
        emu = dev.connect(run_dir)
        if emu is None:
            return
        emu.close()
        if time.monotonic() >= deadline:
            raise dev.H.HarnessError(f"preview daemon did not stop in {run_dir}")
        time.sleep(0.05)


def module_preview(name: str, device: str, stop: bool, full_dsp: bool = False) -> int:
    mid = (
        resolve_module_id(name, allow_missing=True)
        if stop
        else resolve_module(name, device)
    )
    store = [mid + "*"]
    if stop:
        for mode in ("zero", "sharc", None):
            stop_exact(dev.run_dir_of(device, "emulator", store, dsp=mode))
        return 0
    manifest = tomllib.loads((dev.modroots.module_dir(mid) / "module.toml").read_text())
    kind = manifest["module"]["type"]
    ui_preview = not full_dsp and kind == "machine"
    if not full_dsp and kind == "fx":
        raise dev.H.HarnessError(f"{mid}: effect preview requires DSP; use --dsp")
    dsp = (
        dev.store_ui_preview_mode("emulator", device, store)
        if ui_preview
        else dev.target_dsp_mode("emulator", device, store)
    )
    if not full_dsp and dsp != "zero":
        raise dev.H.HarnessError(f"{mid}: module requires DSP; use --dsp")
    if not full_dsp:
        print(
            "UI preview: zero DSP, no audio. Use --dsp for the full module.",
            flush=True,
        )
    keys = (
        "tap:NO,tap:SETTINGS,wait:400,tap:DOWN*8,tap:YES,wait:600,"
        "tap:DOWN*3,tap:YES,wait:600,shot:module"
    )
    # Generic devreload prepares and compares the store before touching its daemon.
    result = navigate(
        [
            "emulator",
            "--device",
            device,
            "--store",
            store[0],
            "--keys",
            keys,
            *(["--store-ui-preview"] if ui_preview else []),
        ]
    )
    if result:
        return result
    # A changed manifest can move the module between modes. Only clean up after
    # navigation succeeds, so a failed candidate leaves existing daemons intact.
    for mode in ("zero", "sharc", None):
        if mode != dsp:
            stop_exact(dev.run_dir_of(device, "emulator", store, dsp=mode))
    return serve(panel_socket("emulator", device, store, dsp=dsp))


def serve(socket_path: str) -> int:
    """Keep Vite in the foreground; stop only its process group on interruption."""
    env = os.environ.copy()
    env["DIGI_LIVE_SOCKET"] = socket_path
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    print(f"Live faceplate: http://127.0.0.1:{port}/panel.html", flush=True)
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
        stop_group(proc)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("preview", choices=[*PREVIEWS, "module"])
    parser.add_argument("module", nargs="?")
    parser.add_argument("--device", choices=["dn2", "dt2"], default="dn2")
    parser.add_argument("--stop", action="store_true")
    parser.add_argument(
        "--dsp", action="store_true", help="pack the full module and enable its DSP"
    )
    args = parser.parse_args(argv)
    if args.preview != "module" and (
        args.module or args.stop or args.dsp or args.device != "dn2"
    ):
        parser.error(
            "module, --device, --dsp and --stop are only supported by dk:module"
        )
    if args.preview == "module" and not args.module:
        parser.error("dk:module requires a module id or leaf alias")
    try:
        if args.preview == "module":
            return module_preview(args.module, args.device, args.stop, args.dsp)
        target, shot = PREVIEWS[args.preview]
        keys = f"tap:NO,tap:SETTINGS,wait:400,tap:DOWN*8,tap:YES,wait:600,shot:{shot}"
        result = navigate([target, "--device", "dn2", "--keys", keys])
        if result:
            return result
        socket_path = panel_socket(target)
        print(
            "Opening the live faceplate; Ctrl-C stops the browser server and keeps the emulator.",
            flush=True,
        )
        return serve(socket_path)
    except (OSError, dev.EmuError, dev.H.HarnessError, pk_dkrt.ProofError) as e:
        print(f"dk:{args.preview}: {e}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    os.chdir(dev.ROOT)
    sys.exit(main(sys.argv[1:]))
