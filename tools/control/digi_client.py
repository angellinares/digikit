#!/usr/bin/env python3
"""Drive a running emulator over its control socket (docs/design/emulator-control.md).

The desktop app (`mise run workbench`) and the daemon (`mise run emu:serve`) listen
on `out/native/run/<pid>.sock`; `out/native/run/current.json` names the
current one. Standard library only.

    uv run python -m tools.control.digi_client status
    uv run python -m tools.control.digi_client inject PATCHED.syx     # hot DSP inject, or cold-boot a patched MAIN
    uv run python -m tools.control.digi_client step 1000000
    uv run python -m tools.control.digi_client tap 25 --hold 1000000
    uv run python -m tools.control.digi_client frame out.png
    uv run python -m tools.control.digi_client call input.button code=25 down=true
    uv run python -m tools.control.digi_client events

In a script:

    from tools.control.digi_client import Emu
    emu = Emu.connect()
    print(emu.inject("out/patched.syx")["start_used"])

`Emu.spawn(...)` starts a private daemon (the binary from DIGI_EMUD, or the
launcher's build) and quits it on exit.
"""

from __future__ import annotations

import argparse
import base64
import contextlib
import json
import os
import pathlib
import socket
import struct
import subprocess
import sys
import tempfile
import time
import zlib
from collections.abc import Iterator
from types import TracebackType
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[2]
DEFAULT_RUN_DIR = ROOT / "out" / "native" / "run"
SCHEMA_VERSION = 1

# Error codes (packages/emulator/control/src/proto.rs).
NO_FIRMWARE, STALE_SESSION, BUSY, REJECTED, AUDIO, FAULT, DSP_HALTED, IO, TIMEOUT = (
    range(1, 10)
)


class EmuError(Exception):
    """A JSON-RPC error answer."""

    def __init__(self, code: int, message: str, data: Any = None) -> None:
        super().__init__(f"{message} (code {code})")
        self.code = code
        self.message = message
        self.data = data


def discover(run_dir: str | os.PathLike[str] | None = None) -> str:
    """The socket of the current emulator: current.json, else the newest live socket."""
    directory = pathlib.Path(run_dir) if run_dir else DEFAULT_RUN_DIR
    current = directory / "current.json"
    try:
        info = json.loads(current.read_text())
        path = str(info["socket"])
        if _alive(path):
            return path
    except (OSError, ValueError, KeyError):
        pass
    socks = sorted(
        directory.glob("*.sock"), key=lambda p: p.stat().st_mtime, reverse=True
    )
    for candidate in socks:
        if _alive(str(candidate)):
            return str(candidate)
    raise FileNotFoundError(f"no running emulator found in {directory}")


def _alive(path: str) -> bool:
    probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        probe.settimeout(1.0)
        probe.connect(path)
        return True
    except OSError:
        return False
    finally:
        probe.close()


class _KeptDir:
    """A run directory that outlives the daemon (same surface as a TemporaryDirectory)."""

    def __init__(self, name: str) -> None:
        self.name = name

    def cleanup(self) -> None:
        pass


class Emu:
    """One connection to an emulator. Calls are synchronous; a job method
    (`step`, `run_until`, a cold-boot load) returns when the job ends."""

    def __init__(self, sock: socket.socket, path: str) -> None:
        self._sock = sock
        self._file = sock.makefile("rwb", buffering=0)
        self._buffer = b""
        self._next_id = 1
        self._events: list[dict[str, Any]] = []
        self.path = path
        self._proc: subprocess.Popen[bytes] | None = None
        self._run_dir: tempfile.TemporaryDirectory[str] | _KeptDir | None = None

    # ----- connecting -------------------------------------------------------

    @classmethod
    def connect(
        cls,
        socket_path: str | None = None,
        run_dir: str | os.PathLike[str] | None = None,
    ) -> Emu:
        path = socket_path or discover(run_dir)
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.connect(path)
        emu = cls(sock, path)
        hello = emu.call("hello", min_schema=SCHEMA_VERSION)
        if hello["schema_version"] < SCHEMA_VERSION:
            raise EmuError(-32602, f"host schema {hello['schema_version']} is too old")
        return emu

    @classmethod
    def spawn(
        cls,
        *args: str,
        binary: str | os.PathLike[str] | None = None,
        timeout: float = 120.0,
        run_dir: str | os.PathLike[str] | None = None,
        env: dict[str, str] | None = None,
    ) -> Emu:
        """Start a private `digi-emud` (paused) with ARGS and connect to it.

        RUN_DIR keeps the daemon's run directory (and the card caches under
        it) between runs; the default is a temporary directory."""
        exe = str(binary or os.environ.get("DIGI_EMUD") or "")
        if not exe:
            raise FileNotFoundError(
                "set DIGI_EMUD to the digi-emud binary (`mise run emu:serve -- --build-only` builds it)"
            )
        if run_dir is not None:
            keep = pathlib.Path(run_dir)
            keep.mkdir(parents=True, exist_ok=True)
            for stale in keep.glob("*.sock"):
                stale.unlink(missing_ok=True)
            (keep / "current.json").unlink(missing_ok=True)
        run_dir_obj: _KeptDir | tempfile.TemporaryDirectory[str] = (
            _KeptDir(str(run_dir))
            if run_dir is not None
            else tempfile.TemporaryDirectory(prefix="digi-run-")
        )
        # stderr goes to a file in the run directory: an undrained pipe blocks the daemon once it fills, and the
        # file keeps the reason when the daemon dies.
        errlog = pathlib.Path(run_dir_obj.name) / "daemon.stderr"
        with errlog.open("wb") as err_file:
            proc = subprocess.Popen(
                [exe, "--run-dir", run_dir_obj.name, *args],
                stdout=subprocess.DEVNULL,
                stderr=err_file,
                cwd=ROOT,
                env=None if env is None else {**os.environ, **env},
            )
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if proc.poll() is not None:
                err = errlog.read_bytes().decode(errors="replace")
                run_dir_obj.cleanup()
                raise RuntimeError(
                    f"digi-emud exited {proc.returncode}: {err.strip()[-400:]}"
                )
            try:
                path = discover(run_dir_obj.name)
                break
            except FileNotFoundError:
                time.sleep(0.1)
        else:
            proc.kill()
            run_dir_obj.cleanup()
            raise TimeoutError("digi-emud did not open its socket")
        emu = cls.connect(path)
        emu._proc, emu._run_dir = proc, run_dir_obj
        return emu

    @classmethod
    def spawn_coupled(
        cls,
        syx: str | os.PathLike[str],
        ready_dir: str | os.PathLike[str],
        *args: str,
        **kwargs: Any,
    ) -> Emu:
        """`spawn` a daemon with DSP audio (no sound card): SYX is loaded, READY_DIR holds the ready inputs
        (`tools/emulator/ready.py`: digi-audio-dn2-image.bin, digi-audio-m5.snap, digi-audio-m5.snap.dsp). The fast tier
        is on as in the launcher."""
        from tools.emulator import digi_env

        syx = pathlib.Path(syx).resolve()
        ready = pathlib.Path(ready_dir).resolve()
        env = {}
        if regions := digi_env.fast_regions_for(syx):
            env["SHARC_FAST_REGIONS"] = regions
        if roots := digi_env.blocktrans_roots_for(syx):
            env["SHARC_BLOCKTRANS"] = roots
        return cls.spawn(
            str(syx),
            "--auto-coupled",
            "--audio-profile-syx",
            str(syx),
            "--dsp-image",
            str(ready / "digi-audio-dn2-image.bin"),
            "--dsp-state",
            str(ready / "digi-audio-m5.snap.dsp"),
            "--cf-snapshot",
            str(ready / "digi-audio-m5.snap"),
            "--no-audio",
            *args,
            env={**env, **kwargs.pop("env", {})},
            **kwargs,
        )

    @classmethod
    def spawn_zero(
        cls,
        syx: str | os.PathLike[str],
        ready_dir: str | os.PathLike[str],
        **kwargs: Any,
    ) -> Emu:
        """Spawn a ColdFire-only daemon using the ready ColdFire snapshot."""
        return cls.spawn(
            str(pathlib.Path(syx).resolve()),
            "--dsp",
            "zero",
            "--cf-snapshot",
            str(pathlib.Path(ready_dir).resolve() / "digi-audio-m5.snap"),
            **kwargs,
        )

    def close(self) -> None:
        try:
            self._file.close()
            self._sock.close()
        except OSError:
            pass
        if self._proc is not None:
            try:
                self._proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self._proc.kill()
            self._proc = None
        if self._run_dir is not None:
            self._run_dir.cleanup()
            self._run_dir = None

    def __enter__(self) -> Emu:
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        if self._proc is not None:
            with contextlib.suppress(EmuError, OSError):
                self.quit()
        self.close()

    # ----- the protocol -----------------------------------------------------

    def _read_line(self) -> dict[str, Any]:
        while b"\n" not in self._buffer:
            chunk = self._sock.recv(1 << 16)
            if not chunk:
                raise ConnectionError("emulator closed the connection")
            self._buffer += chunk
        line, self._buffer = self._buffer.split(b"\n", 1)
        message: dict[str, Any] = json.loads(line)
        return message

    def call(self, method: str, **params: Any) -> dict[str, Any]:
        """One request; the result object, or EmuError. Events that arrive
        on this connection meanwhile are kept for `events()`."""
        ident = self._next_id
        self._next_id += 1
        request = {"jsonrpc": "2.0", "id": ident, "method": method, "params": params}
        self._sock.sendall(json.dumps(request).encode() + b"\n")
        while True:
            message = self._read_line()
            if message.get("method") == "event":
                self._events.append(message["params"])
                continue
            if message.get("id") != ident:
                continue
            if "error" in message:
                err = message["error"]
                raise EmuError(err["code"], err["message"], err.get("data"))
            result: dict[str, Any] = message["result"]
            return result

    def events(
        self, *kinds: str, timeout: float | None = None
    ) -> Iterator[dict[str, Any]]:
        """Subscribe on this connection and yield events (those matching KINDS)."""
        self.call("events.subscribe", **({"kinds": list(kinds)} if kinds else {}))
        self._sock.settimeout(timeout)
        try:
            while True:
                if self._events:
                    yield self._events.pop(0)
                    continue
                try:
                    message = self._read_line()
                except TimeoutError:
                    return
                if message.get("method") == "event":
                    yield message["params"]
        finally:
            with contextlib.suppress(OSError):
                self._sock.settimeout(None)

    def subscribe(self, *kinds: str) -> None:
        """Start event delivery on this connection; `call()` keeps what arrives for `drain_events()`."""
        self.call("events.subscribe", **({"kinds": list(kinds)} if kinds else {}))

    def drain_events(self) -> list[dict[str, Any]]:
        """The events kept so far (after `subscribe`), oldest first; empties the list."""
        out, self._events = self._events, []
        return out

    # ----- wrappers ---------------------------------------------------------

    def status(self) -> dict[str, Any]:
        return self.call("status")

    def cpu_load(self) -> dict[str, Any] | None:
        """`status.load`: ColdFire and DSP load, model and real-chip estimate (docs/CONTROL.md); None until the
        first window (about 1 s of emulated time) has closed."""
        return self.status().get("load")

    def load(
        self,
        syx: str | os.PathLike[str],
        *,
        base_sha256: str | None = None,
        start: str = "auto",
        card: str | os.PathLike[str] | None = None,
        dsp_patch: str | os.PathLike[str] | None = None,
        runtime: bool = False,
        patch_set: str | None = None,
    ) -> dict[str, Any]:
        """`firmware.load`: the report says how the firmware was brought in. `runtime=True`: the controller patches the stock
        image with the device's patch set (`patch_set`, default `emulator`) and loads that; its report is `report["runtime"]`."""
        params: dict[str, Any] = {
            "syx": str(pathlib.Path(syx).resolve()),
            "start": start,
        }
        if base_sha256:
            params["base_sha256"] = base_sha256
        if card:
            params["card"] = str(pathlib.Path(card).resolve())
        if dsp_patch:
            params["dsp_patch"] = str(pathlib.Path(dsp_patch).resolve())
        if runtime:
            params["runtime"] = True
            if patch_set:
                params["patch_set"] = patch_set
        elif patch_set:
            raise ValueError("patch_set needs runtime=True")
        return self.call("firmware.load", **params)

    inject = load

    def restart(self, auto_no: bool = False) -> dict[str, Any]:
        """Back to the ready state of the running firmware; AUTO_NO presses NO there (DT2 FILE SYSTEM OK)."""
        return self.call("restart", **({"auto_no": True} if auto_no else {}))

    def step(self, instructions: int, **params: Any) -> dict[str, Any]:
        return self.call("step", instructions=instructions, **params)

    def run_until(self, **params: Any) -> dict[str, Any]:
        return self.call("run_until", **params)

    def button(
        self, code: int, down: bool = True, at_instr: int | None = None
    ) -> dict[str, Any]:
        return self.call(
            "input.button", **_opt(code=code, down=down, at_instr=at_instr)
        )

    def turn(
        self, encoder: int, detents: int = 1, at_instr: int | None = None
    ) -> dict[str, Any]:
        return self.call(
            "input.turn", **_opt(encoder=encoder, detents=detents, at_instr=at_instr)
        )

    def tap(
        self, code: int, hold: int = 40_000_000, at_instr: int | None = None
    ) -> dict[str, Any]:
        return self.call("input.tap", **_opt(code=code, hold=hold, at_instr=at_instr))

    def schedule(self, events: list[dict[str, Any]]) -> dict[str, Any]:
        return self.call("input.schedule", events=events)

    def recorded(self) -> dict[str, Any]:
        result: dict[str, Any] = self.call("inputs.recorded")["scenario"]
        return result

    def state_digest(self) -> str:
        return str(self.call("state.digest")["digest"])

    def state_save(self, path: str | os.PathLike[str]) -> dict[str, Any]:
        """Write the ColdFire state to PATH (docs/CONTROL.md `state.save`)."""
        return self.call("state.save", path=str(path))

    def state_load(self, path: str | os.PathLike[str]) -> dict[str, Any]:
        """Restore a `state_save` file into the runtime just loaded (docs/CONTROL.md `state.load`)."""
        return self.call("state.load", path=str(path))

    def audio(self) -> Any:
        return self.call("audio.report")["audio"]

    def pcm_start(self) -> dict[str, Any]:
        return self.call("pcm.tap", action="start")

    def pcm_report(self) -> dict[str, Any]:
        return self.call("pcm.tap", action="report")

    def patch(self, text: str | None = None, file: str | None = None) -> dict[str, Any]:
        return self.call("dsp.patch", **_opt(text=text, file=file))

    def dsp_peek(self, addr: int, length: int) -> dict[str, Any]:
        """DSP memory: `hex` holds `??` for bytes the engine does not know."""
        return self.call("dsp.peek", addr=addr, len=length)

    def frame(self) -> tuple[int, bytes]:
        """(revision, 1024 raw OLED bytes)."""
        result = self.call("framebuffer.get")
        return int(result["revision"]), base64.b64decode(result["data_b64"])

    def frame_hash(self) -> str:
        """SHA-256 (hex) of the 1024 raw OLED bytes, without transferring them."""
        return str(self.call("framebuffer.get", hash=True, data=False)["sha256"])

    def leds(self) -> dict[str, Any]:
        """`{led_revision, leds, rgb}`: 128 hex digits of LED values, 384 of RGB (None until set)."""
        return self.call("leds.get")

    def frame_png(self) -> bytes:
        return oled_png(self.frame()[1])

    def midi_in(self, data: bytes, period: int | None = None) -> dict[str, Any]:
        """Queue bytes on the MIDI DIN input; PERIOD = instructions between bytes (default: the 31250 baud wire rate)."""
        return self.call("midi.in", bytes=data.hex(), **_opt(period=period))

    def midi_out(self) -> bytes:
        """The bytes the firmware sent on MIDI DIN since the last call."""
        return bytes.fromhex(self.call("midi.out")["hex"])

    def usb_in(
        self, data: bytes, chunk: int | None = None, period: int | None = None
    ) -> dict[str, Any]:
        """Hand bytes to the firmware's USB MIDI receive callback (MidiRpc requests arrive only this way), CHUNK bytes per call
        (default 64) PERIOD instructions apart (default 132,000; a high-speed microframe is 384 and 16,500)."""
        return self.call(
            "midi.usb.in", bytes=data.hex(), **_opt(chunk=chunk, period=period)
        )

    def usb_out(self) -> bytes:
        """The bytes the firmware handed to its USB MIDI transmit function since the last call."""
        return bytes.fromhex(self.call("midi.usb.out")["hex"])

    def peek32(self, addr: int) -> int | None:
        """One guest longword (None when unmapped), read without side effects."""
        value = self.call("mem.cf.peek", addr=addr)["value"]
        return None if value is None else int(value)

    def poke32(self, addr: int, value: int) -> dict[str, Any]:
        """Write one SDRAM longword (a test aid; not recorded for replay)."""
        return self.call("mem.cf.poke", addr=addr, value=value)

    def watchdog(self, stall_ms: int | None = None) -> dict[str, Any]:
        """The heartbeat; STALL_MS sets the stall threshold in emulated ms (0 disables)."""
        return self.call("watchdog.config", **_opt(stall_ms=stall_ms))

    def cpu_exceptions(self, deliver: bool | None = None) -> dict[str, Any]:
        """`cpu.exceptions`: True makes a CPU exception run the guest's handler (the digikit runtime's crash capture) instead of stopping the run
        with an error (the default); kept for every runtime installed afterwards, a soft reset's cold boot included."""
        return self.call("cpu.exceptions", **_opt(deliver=deliver))

    def quit(self) -> dict[str, Any]:
        return self.call("quit")


def _opt(**params: Any) -> dict[str, Any]:
    return {k: v for k, v in params.items() if v is not None}


def oled_png(frame: bytes, scale: int = 4) -> bytes:
    """A grayscale PNG of the 128x64 OLED (byte (7 - y/8) + 8*x, bit y%8)."""
    width, height = 128, 64
    rows = bytearray()
    for y in range(height):
        row = bytearray()
        for x in range(width):
            on = frame[(7 - y // 8) + 8 * x] >> (y % 8) & 1
            row += bytes([255 if on else 0]) * scale
        rows += (b"\x00" + bytes(row)) * scale

    def chunk(tag: bytes, data: bytes) -> bytes:
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    header = struct.pack(">IIBBBBB", width * scale, height * scale, 8, 0, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(bytes(rows)))
        + chunk(b"IEND", b"")
    )


# ----- command line ---------------------------------------------------------


def _parse_value(text: str) -> Any:
    try:
        return json.loads(text)
    except ValueError:
        return text


def describe_load(report: dict[str, Any]) -> str:
    """The injection report as a few lines for a terminal."""
    lines = [
        f"start_used     {report.get('start_used')}",
        f"main_changed   {report.get('main_changed')}",
        f"sections       {report.get('sections_changed')}",
    ]
    dsp = report.get("dsp")
    if isinstance(dsp, dict):
        if dsp.get("applied"):
            inj = dsp.get("injected") or {}
            lines.append(
                f"dsp            {dsp.get('changed_bytes')} bytes in {dsp.get('ranges')} range(s)"
                f" at DSP frame {inj.get('at_frame')}"
            )
        else:
            lines.append(f"dsp            not applied: {dsp.get('reason')}")
    rt = report.get("runtime")
    if isinstance(rt, dict):
        if rt.get("patched"):
            lines.append(
                f"runtime        patch set {rt.get('set')}, {len(rt.get('modules', []))} modules, "
                f"{'cached image' if rt.get('cache_hit') else 'patched now'} ({rt.get('seconds', 0):.2f} s)"
            )
        else:
            lines.append(f"runtime        not patched: {rt.get('reason')}")
    if report.get("cache_hit"):
        lines.append("cache          snapshot cache hit")
    lines.append(f"seconds        {report.get('seconds', 0):.3f}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--socket", help="socket path (default: discover)")
    parser.add_argument("--run-dir", help="where to look for the current emulator")
    sub = parser.add_subparsers(dest="cmd", required=True)
    sub.add_parser("status")
    p = sub.add_parser("load", help="firmware.load")
    p.add_argument("syx")
    p.add_argument("--base-sha256")
    p.add_argument("--start", default="auto", choices=["auto", "snapshot", "cold"])
    p.add_argument("--card")
    p.add_argument("--dsp-patch")
    p.add_argument(
        "--runtime",
        action="store_true",
        help="patch the stock .syx with the device's patch set here and load that (digikit runtime)",
    )
    p.add_argument(
        "--patch-set", help="with --runtime: the patch set (default emulator)"
    )
    p = sub.add_parser(
        "inject",
        help="load a patched .syx into the running emulator (hot DSP inject when only the DSP changed)",
    )
    p.add_argument("syx")
    p.add_argument("--base-sha256")
    p.add_argument("--start", default="auto", choices=["auto", "snapshot", "cold"])
    p.add_argument("--dsp-patch")
    p.add_argument(
        "--runtime",
        action="store_true",
        help="patch the stock .syx with the device's patch set here and load that (digikit runtime)",
    )
    p.add_argument(
        "--patch-set", help="with --runtime: the patch set (default emulator)"
    )
    sub.add_parser("restart")
    p = sub.add_parser("step")
    p.add_argument("instructions", type=int)
    p = sub.add_parser("tap")
    p.add_argument("code", type=int)
    p.add_argument("--hold", type=int, default=40_000_000)
    p.add_argument("--at-instr", type=int)
    p = sub.add_parser("frame")
    p.add_argument("out", help="PNG path")
    p = sub.add_parser("call", help="any method: call METHOD key=json ...")
    p.add_argument("method")
    p.add_argument("params", nargs="*")
    p = sub.add_parser("events")
    p.add_argument("kinds", nargs="*")
    sub.add_parser("quit")
    args = parser.parse_args(argv)
    try:
        emu = Emu.connect(args.socket, args.run_dir)
    except (OSError, EmuError) as exc:
        print(f"digi_client: {exc}", file=sys.stderr)
        return 2
    try:
        if args.cmd in ("load", "inject"):
            report = emu.load(
                args.syx,
                base_sha256=args.base_sha256,
                start=args.start,
                card=getattr(args, "card", None),
                dsp_patch=args.dsp_patch,
                runtime=args.runtime,
                patch_set=args.patch_set,
            )
            print(describe_load(report))
        elif args.cmd == "frame":
            pathlib.Path(args.out).write_bytes(emu.frame_png())
            print(args.out)
        elif args.cmd == "events":
            for event in emu.events(*args.kinds):
                print(json.dumps(event), flush=True)
        else:
            if args.cmd == "call":
                params = dict(item.split("=", 1) for item in args.params)
                result = emu.call(
                    args.method, **{k: _parse_value(v) for k, v in params.items()}
                )
            elif args.cmd == "tap":
                result = emu.tap(args.code, args.hold, args.at_instr)
            elif args.cmd == "step":
                result = emu.step(args.instructions)
            else:
                result = emu.call(args.cmd)
            print(json.dumps(result, indent=2))
    except EmuError as exc:
        print(f"digi_client: {exc}", file=sys.stderr)
        if exc.data:
            print(json.dumps(exc.data, indent=2)[:800], file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        return 130
    finally:
        emu.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
