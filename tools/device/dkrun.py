#!/usr/bin/env python3
"""`dk run MODULE`: build, pack, upload, select, restart and follow the log, in one command (docs/design/digikit-upload.md, runtime 4).

    uv run --with python-rtmidi python -m tools.device.dkrun <author/name> --port Digitakt
    uv run python -m tools.device.dkrun <author/name> --emu            # the running emulator (desktop app or `mise run emu:serve`)
    mise run dk:run -- <author/name> --port Digitakt --seconds 30

Steps, one line each: INFO (the runtime must be 4 or newer; its device id picks the pack target), build and pack (`patchkit pack` builds the objects that are stale, in memory), upload to `modules/<name>.dkm` (skipped when LIST shows the same sha256), SELECT (the selection becomes the debug module `digikit/debug`, which the log needs, and this module), RESTART, wait for the new boot, then the boot's MOD_LOAD / MOD_DSP results, then the log for `--seconds`.

It changes only the module store and the selection record (the menu's own code writes it). `--no-restart` stops after SELECT. Time on the emulator is stepped, not real: `--seconds` is emulated time, approximately.
"""

from __future__ import annotations

import argparse
import hashlib
import subprocess
import sys
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

from tools import python_env
from tools.device import dklog as L
from tools.device import dkneeds as N
from tools.device import dkupload as D
from tools.patchkit import modroots

ROOT = Path(__file__).resolve().parents[2]
MIN_RUNTIME = 4
DEBUG_MODULE = "digikit/debug"  # the log exists only while it is loaded
EMU_IPS = 100_000_000  # emulated instructions counted as one second of `--seconds` on the emulator
CMD_RESTART_KIND = L.K["UPLOAD"]


class RunError(RuntimeError):
    pass


def say(msg: str) -> None:
    print(f"dk run: {msg}", flush=True)


def build_objects(module: str) -> None:
    """Compile the module's objects (`patchkit build` skips an object whose source hash is unchanged)."""
    r = subprocess.run(
        [
            sys.executable,
            "-m",
            "tools.patchkit.patchkit",
            "build",
            str(modroots.module_dir(module)),
        ],
        cwd=ROOT,
        env=python_env(),
        capture_output=True,
        text=True,
    )
    sys.stdout.write(r.stdout)
    if r.returncode:
        sys.stderr.write(r.stderr)
        raise RunError(f"build of {module} failed")


def pack_module(module: str, device: str, out: Path | None = None) -> Path:
    """Pack the .dkm of MODULE for DEVICE (`patchkit pack`); returns its path (default out/patch/<name>-<device>.dkm)."""
    out = out or ROOT / "out" / "patch" / f"{module.rsplit('/', 1)[-1]}-{device}.dkm"
    out.parent.mkdir(parents=True, exist_ok=True)
    r = subprocess.run(
        [
            sys.executable,
            "-m",
            "tools.patchkit.patchkit",
            "pack",
            module,
            "--device",
            device,
            "-o",
            str(out),
        ],
        cwd=ROOT,
        env=python_env(),
        capture_output=True,
        text=True,
    )
    if r.returncode or not out.exists():
        why = (r.stderr.strip().splitlines() or ["?"])[-1]
        raise RunError(f"pack of {module} for {device} failed: {why}")
    return out


def build_pack(module: str, device: str) -> Path:
    """Pack the .dkm of MODULE for DEVICE; returns its path. `patchkit pack` builds the objects that are stale (they are cached in
    out/cache/dko) and packs them as they are in memory."""
    return pack_module(module, device)


def module_path(module: str) -> str:
    """The store path a module's .dkm is uploaded to."""
    return f"modules/{module.rsplit('/', 1)[-1]}.dkm"


def upload_if_changed(
    up: D.Uploader,
    path: str,
    data: bytes,
    have: dict[str, tuple[int, str]] | None = None,
    log: Callable[[str], None] = say,
    progress: Callable[[int, int], None] | None = None,
) -> bool:
    """PUT `data` at `path` unless LIST shows the same size and sha256 there (`have`: that listing, else asked for). True: uploaded.
    `progress(sent, total)` is called after each DATA chunk the device acknowledged (not for a skipped upload)."""
    if have is None:
        have = {p: (n, h) for p, n, h in up.ls()}
    sha = hashlib.sha256(data).hexdigest()
    if path in have and have[path] == (len(data), sha):
        log(f"{path} is on the device ({len(data)} bytes, same sha256): not uploaded")
        return False
    t0 = time.monotonic()
    up.put(path, data, progress)
    log(
        f"uploaded {path}: {len(data)} bytes ({time.monotonic() - t0:.1f} s, sha256 checked on the device)"
    )
    return True


def restart_and_wait(
    up: D.Uploader,
    connect: Callable[[], D.Uploader] | None,
    wait_boot: Callable[[], None] | None,
    sleep: Callable[[float], None],
    now: Callable[[], float],
    log: Callable[[str], None] = say,
) -> D.Uploader:
    """RESTART, wait for the guest to boot again and for its log to be a new boot's; returns the uploader to use after it."""
    up.restart()
    log("restart requested; waiting for the boot")
    if wait_boot is not None:
        wait_boot()
    old = up
    up = wait_new_boot(
        connect or (lambda: old),
        sleep,
        now=now,
        connect_closes=connect is not None,
        booted=wait_boot is not None,
    )
    if connect is not None:
        _close(old)
    log("the device booted")
    return up


def require_runtime(info: dict[str, Any]) -> None:
    if info["runtime"] < MIN_RUNTIME:
        raise RunError(
            f"the device runs runtime {D.fmt_runtime(info['runtime'])}; dk run needs {D.fmt_runtime(MIN_RUNTIME)} "
            "(SELECT and RESTART): flash a runtime-4 image (docs/PATCHING.md, 'Runtime 4: dk run and screenshots')"
        )
    if info["card_read_only"]:
        raise RunError(
            "the card's store area is not ours and not blank: the runtime writes nothing there (ABOUT shows CARD READ-ONLY)"
        )


def restarted(events: list[Any]) -> bool | None:
    """Has the device booted since the RESTART was answered? True: this log has no RESTART event (a new boot, with BOOT_START);
    False: the old boot still runs; raises when the runtime refused the restart."""
    asked = False
    for _seq, _t, kind, flags, a in events:
        if kind == CMD_RESTART_KIND and a[0] == D.CMD_RESTART:
            if flags & 1 and a[1]:
                raise RunError(
                    f"the runtime refused the restart (status {a[1]:#x}; a card transfer is in flight, try again)"
                )
            asked = True
    if asked:
        return False
    return any(e[2] == L.K["BOOT_START"] for e in events)


def _close(up: D.Uploader | None) -> None:
    close = getattr(getattr(up, "t", None), "close", None)
    if close:
        close()


def wait_new_boot(
    connect: Callable[[], D.Uploader],
    sleep: Callable[[float], None],
    timeout_s: float = 90.0,
    now: Callable[[], float] = time.monotonic,
    connect_closes: bool = True,
    booted: bool = False,
) -> D.Uploader:
    """Poll until the device has booted again. With the debug module (which `run` selects) the log tells a new boot from the old one.
    Without it there is no log (INFO has no LOG capability), so the device counts as booted when it answers again after a failed read
    (the port is silent while it resets), or when the caller already stepped past the reset (`booted`). The TypeScript `waitForBoot`
    (packages/dk-session/src/dkupload-core.ts) uses the same rule. The port may vanish and return (USB re-enumerates): every failure
    is a reason to wait and connect again."""
    end = now() + timeout_s
    last: Exception | None = None
    silent = False
    while now() < end:
        up = None
        try:
            up = connect()
            info = up.info()
            if D.has_cap(info, "LOG"):
                _, _, evs = L.read_all(up)
                if restarted(evs):
                    return up
            elif silent or booted:
                return up
        except (TimeoutError, OSError, SystemExit, D.UploadError) as e:
            last = e
            silent = True
        if connect_closes:
            _close(up)
        sleep(0.5)
    raise RunError(f"no new boot within {timeout_s:.0f} s ({last})")


def module_results(
    up: D.Uploader,
    sleep: Callable[[float], None],
    timeout_s: float,
    now: Callable[[], float],
) -> list[dict[str, Any]]:
    """The boot's MOD_LOAD and MOD_DSP events, waiting for the DSP part (a module without one never logs MOD_DSP: UI_FIRST and a quiet
    second end the wait)."""
    end = now() + timeout_s
    seen_ui: float | None = None
    while True:
        _, _, evs = L.read_all(up)
        loads = [e for e in evs if e[2] == L.K["MOD_LOAD"]]
        dsps = [e for e in evs if e[2] == L.K["MOD_DSP"]]
        pending = any(e[4][1] == 1 for e in loads)  # LOADING: the DSP part follows
        if loads and not pending:
            return [L.event_dict(e) for e in loads + dsps]
        if loads and pending and len(dsps) >= sum(1 for e in loads if e[4][1] == 1):
            return [L.event_dict(e) for e in loads + dsps]
        if any(e[2] == L.K["UI_FIRST"] for e in evs):
            seen_ui = seen_ui or now()
        if now() >= end or (not loads and seen_ui and now() - seen_ui > 10):
            return [L.event_dict(e) for e in loads + dsps]
        sleep(0.5)


def check_fit(up: D.Uploader, info: dict[str, Any], data: bytes) -> str | None:
    """Why the packed module DATA will not load on the device, or None. `run` selects it as the only module of the next boot, which loads it into
    an empty runtime: its needs, inferred from the file (`patchkit needs`, docs/design/digikit-dkm.md), against the device's limits (CAPACITY; the
    fit rule, `dkneeds.fit`). A file that has no needs is one the loader refuses. A device without CAPACITY, or one that does not answer it, gets no
    verdict on the limits."""
    try:
        needs = N.needs_of(data)
    except N.NeedsError as e:
        return f"the file is not one the loader accepts: {e}"
    if not D.has_cap(info, "CAPACITY"):
        return None
    try:
        caps = up.capacity()
    except D.UploadError:
        return None
    short = N.fit(caps, [needs], empty=True)["each"][0]
    return short["describe"] if short else None


def check_results(res: list[dict[str, Any]]) -> None:
    """Raise RunError unless the boot's MOD_LOAD and MOD_DSP events (as `module_results` returns them) show every module loaded."""
    for d in res:
        if d["kind"] == "MOD_LOAD" and d["args"][1] not in (0, 1):
            raise RunError(
                f"MOD_LOAD result {d['fields'].get('result')}: the module did not load"
            )
        if d["kind"] == "MOD_DSP" and d["args"][1] != 0:
            raise RunError(
                f"MOD_DSP result {d['fields'].get('result')}: the DSP part did not load"
            )
    if not any(d["kind"] == "MOD_LOAD" for d in res):
        raise RunError(
            "no MOD_LOAD event: the boot did not load the module (safe mode? see the log)"
        )


def run(
    up: D.Uploader,
    module: str,
    connect: Callable[[], D.Uploader] | None = None,
    wait_boot: Callable[[], None] | None = None,
    sleep: Callable[[float], None] = time.sleep,
    now: Callable[[], float] = time.monotonic,
    seconds: float = 10.0,
    device: str | None = None,
    restart: bool = True,
    emit: Callable[[str, dict[str, Any] | None], None] | None = None,
    dkm: Path | None = None,
    force: bool = False,
) -> dict[str, Any]:
    """The whole sequence (a module that will not fit is refused before the upload, unless FORCE). UP talks to the device now; CONNECT returns an uploader after the restart (default: the same one);
    WAIT_BOOT (the emulator) returns when the guest has booted again. Returns a summary dict."""
    out: dict[str, Any] = {}
    info = up.info()
    require_runtime(info)
    out["info"] = info
    say(
        f"{info['port']}, runtime {D.fmt_runtime(info['runtime'])}, free {info['free_bytes']} bytes"
        + (", safe mode" if info["safe"] else "")
    )
    dev = device or info["port"]
    if dkm is None:
        t0 = time.monotonic()
        dkm = build_pack(module, dev)
        say(f"packed {dkm.name} ({time.monotonic() - t0:.1f} s)")
    data = dkm.read_bytes()
    why = check_fit(up, info, data)
    if why and not force:
        raise RunError(
            f"{module} will not fit this device: {why} (--force uploads it anyway)"
        )
    if why:
        say(
            f"WARNING {module} will not fit this device: {why}; uploading anyway (--force)"
        )
    path = module_path(module)
    out["path"] = path
    out["uploaded"] = upload_if_changed(up, path, data)
    # the log (LOG, which this tool reads) exists only while the debug module is loaded: it is selected first, next to the module
    selection = [path]
    if module != DEBUG_MODULE:
        debug_path = module_path(DEBUG_MODULE)
        upload_if_changed(up, debug_path, build_pack(DEBUG_MODULE, dev).read_bytes())
        selection = [debug_path, path]
    up.select(selection)
    say(f"selected {', '.join(selection)} (the modules of the next boot)")
    if not restart:
        return out
    up = restart_and_wait(up, connect, wait_boot, sleep, now)
    res = module_results(up, sleep, 60.0, now)
    out["results"] = res
    for d in res:
        say(L.text_of(d).strip())
        if emit:
            emit(L.text_of(d), d)
    check_results(res)
    if seconds > 0:
        say(f"following the log for {seconds:g} s")
        fol = L.Follower(
            up,
            emit or (lambda t, d: print(t if d is not None else f"# {t}", flush=True)),
        )
        fol.next = max([d["seq"] for d in res] + [0]) + 1
        end = now() + seconds
        while now() < end:
            fol.poll()
            sleep(1.0)
        fol.poll()
    return out


def emu_boot_waiter(emu: Any, timeout_s: float = 600.0) -> Callable[[], None]:
    """Step the emulator until its soft reset has cold-booted the firmware again (the emulator's `firmware.loaded` event)."""

    def wait() -> None:
        t0 = time.monotonic()
        while time.monotonic() - t0 < timeout_s:
            try:
                emu.step(400_000)
            except Exception:  # noqa: BLE001  a cold boot runs on its worker: steps are refused until it is done
                time.sleep(0.5)
            for ev in emu.drain_events():
                if ev.get("kind") == "cpu.reset" and ev.get("restart") == "unsupported":
                    raise RunError("the emulator cannot restart this build")
                if ev.get("kind") == "firmware.loaded":
                    if not ev.get("ok"):
                        raise RunError(f"restart: the cold boot failed {ev}")
                    return
        raise RunError("restart: the emulator did not boot again")

    return wait


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("module", help="module id, e.g. <author/name>")
    where = ap.add_mutually_exclusive_group(required=True)
    where.add_argument("--emu", action="store_true", help="the running emulator")
    where.add_argument(
        "--port", help="MIDI port name substring (a real device; python-rtmidi)"
    )
    ap.add_argument("--run-dir", type=Path, help="with --emu: a daemon's run directory")
    ap.add_argument(
        "--seconds",
        type=float,
        default=10.0,
        help="follow the log this long after the boot (default 10)",
    )
    ap.add_argument(
        "--device", help="pack target (default: the device id INFO reports)"
    )
    ap.add_argument("--no-restart", action="store_true", help="stop after SELECT")
    ap.add_argument(
        "--force",
        action="store_true",
        help="upload the module even when its needs do not fit the device (or the file is not one the loader accepts); the boot then refuses it",
    )
    ap.add_argument(
        "--out",
        type=Path,
        help="also write the followed events here (JSON lines, as dklog follow)",
    )
    args = ap.parse_args(argv)

    emu = None
    if args.emu:
        emu = D._emu(args.run_dir)
        emu.subscribe()  # the boot waiter reads the cpu.reset and firmware.loaded events
        t = D.EmuTransport(emu)
        up = D.Uploader(t)
        steps = {"n": 0}

        def sleep(s: float) -> None:
            n = int(s * EMU_IPS)
            emu.step(n)
            steps["n"] += n

        def now() -> float:
            return steps["n"] / EMU_IPS

        connect: Callable[[], D.Uploader] | None = None
        wait_boot = emu_boot_waiter(emu)
    else:
        up = D.Uploader(D.RtMidiTransport(args.port))
        sleep, now, wait_boot = time.sleep, time.monotonic, None

        def connect() -> D.Uploader:
            return D.Uploader(D.RtMidiTransport(args.port))

    emit = None
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        import json

        f = args.out.open("a")

        def emit(text: str, d: dict[str, Any] | None) -> None:
            print(text if d is not None else f"# {text}", flush=True)
            if d is not None:
                f.write(json.dumps(d) + "\n")
                f.flush()

    try:
        run(
            up,
            args.module,
            connect,
            wait_boot,
            sleep,
            now,
            args.seconds,
            args.device,
            not args.no_restart,
            emit,
            force=args.force,
        )
    except (RunError, D.UploadError, TimeoutError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
