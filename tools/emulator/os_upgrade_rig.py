#!/usr/bin/env python3
"""Bounded emulator rig for the MAIN OS upgrade receive path (finding 75).

Boots a stock image from a ready snapshot (zero DSP), opens SETTINGS > SYSTEM > OS UPGRADE with panel taps, streams a
`.syx` over the emulated USB MIDI input (`midi.usb.in`, the firmware's SysEx assembler), and records per step: the
receiver state words, `midi.usb.out` bytes, the OLED frame. Optional YES at the confirm dialog, then the run goes on
until the flash write ends in the soft reset.

Every run is bounded by `--limit` (guest instructions in total). It starts one private `digi-emud` and stops only that
PID. Example:

    uv run python -m tools.emulator.os_upgrade_rig dt2 --binary BIN --ready out/snapshots/dt2-audio-ready-cold-2026-10-20 \
        --syx firmware/Digitakt_II_OS1.17.syx --out out/runs/os-upgrade --yes --limit 12000000000

With `--patchkit BIN` the rig does not send the stream itself: after the panel steps it runs `patchkit flash STOCK --emulator SOCKET`, which sends
the patched image through the shared sender, compares the emulated flash with the image, waits for the restart and reads INFO. `--restore-after`
then opens OS UPGRADE again on the patched image and runs `patchkit flash --restore` (the stock file back).

With `--exec CMD` the rig runs CMD instead (arguments split like a shell, `{socket}` replaced by the control socket, `DIGI_FW_EMU_SOCKET` set), for
example the Firmware page's own flash logic headless: the Vitest file apps/web/tests/firmware-emulator.wasm.test.ts, which reads the socket from
`DIGI_FW_EMU_SOCKET` (`pnpm --filter @digi/web exec vitest run --root . --config ../../vitest.host.config.ts --project web-wasm FILE`).
`--exec-restore CMD` then opens OS UPGRADE again and runs that.

With `--rpc` the rig does not open the menu: it sends OsUpgradeStart, Write chunks and End over midi.usb.in at the main screen (finding 75, "The RPC route"),
prints each reply status and the turnaround, compares the flash container with the stream before the soft reset and follows the screens.

State words (MAIN OS receive state, finding 75): byte 0 = state (0 idle, 1 header seen, 2 receiving, 3 complete),
+8 = announced message count, +0xc = messages taken, +0x10 = next sequence number.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import shlex
import struct
import subprocess
import sys
import time
import zlib

from tools.control.digi_client import Emu

ROOT = pathlib.Path(__file__).resolve().parents[2]
YES, NO, DOWN = 10, 12, 14
SETTINGS = 8
# receive state object (FUN_40120bcc returns it) per image; the DT2 value is 1.17, the DN2 value 1.12
STATE = {"dt2": 0x403A0ED0, "dn2": 0x4038BEC0}
SYX = {"dt2": "Digitakt_II_OS1.17.syx", "dn2": "Digitone_II_OS1.12.syx"}


RPC_HEADER = bytes([0xF0, 0x00, 0x20, 0x3C, 0x10, 0x00])
RPC_PING, RPC_SOFTWARE_VERSION = 0x01, 0x02
RPC_OS_START, RPC_OS_WRITE, RPC_OS_END = 0x50, 0x51, 0x52
RPC_FILE_TYPE = b"sysex\0"


def pack_8in7(data: bytes) -> bytes:
    out = bytearray()
    for i in range(0, len(data), 7):
        part = data[i : i + 7]
        out.append(sum((b >> 7) << (6 - k) for k, b in enumerate(part)))
        out += bytes(b & 0x7F for b in part)
    return bytes(out)


def unpack_8in7(packed: bytes) -> bytes:
    out = bytearray()
    for i in range(0, len(packed), 8):
        top = packed[i]
        for k in range(min(7, len(packed) - i - 1)):
            out.append(packed[i + 1 + k] | (((top >> (6 - k)) & 1) << 7))
    return bytes(out)


def rpc_request(kind: int, seq: int, payload: bytes = b"") -> bytes:
    body = bytes([seq >> 8, seq & 255, 0, 0, kind]) + payload
    return RPC_HEADER + pack_8in7(body) + b"\xf7"


def rpc_start(total: int, auto: bool) -> bytes:
    """OsUpgradeStart payload: stream length (u32 BE), file type string, flag (1 = go on after the last byte, 0 = wait for End)."""
    return struct.pack(">I", total) + RPC_FILE_TYPE + bytes([1 if auto else 0])


def rpc_write(data: bytes, offset: int) -> bytes:
    """OsUpgradeWrite payload: CRC-32 of the data (init 0xffffffff, as the firmware's), data length, stream offset, data (all u32 BE)."""
    return struct.pack(">III", zlib.crc32(data, 0xFFFFFFFF), len(data), offset) + data


def rpc_end(commit: bool) -> bytes:
    return bytes([1 if commit else 0])


def parse_rpc_reply(msg: bytes) -> tuple[int, int, bytes]:
    """(seq, type without the reply bit, payload) of one reply message."""
    body = unpack_8in7(msg[6:-1])
    return (body[0] << 8) | body[1], body[4] & 0x7F, body[5:]


def split_messages(stream: bytes) -> list[bytes]:
    out, i = [], 0
    while i < len(stream):
        j = stream.index(0xF7, i)
        out.append(stream[i : j + 1])
        i = j + 1
    return out


class Rig:
    def __init__(self, emu: Emu, device: str, out: pathlib.Path, limit: int) -> None:
        self.emu, self.device, self.out, self.limit = emu, device, out, limit
        self.used = 0
        self.log: list[dict] = []

    def step(self, n: int) -> None:
        if self.used + n > self.limit:
            raise SystemExit(f"limit {self.limit} instructions reached")
        self.emu.step(n)
        self.used += n

    def tap(self, code: int, gap: int = 30_000_000) -> None:
        self.emu.tap(code, hold=2_000_000)
        self.step(2_000_000 + gap)

    def frame(self, name: str) -> str:
        self.out.mkdir(parents=True, exist_ok=True)
        (self.out / f"{name}.png").write_bytes(self.emu.frame_png())
        return self.emu.frame_hash()

    def state(self) -> dict:
        base = STATE[self.device]
        word = self.emu.peek32(base) or 0
        return {
            "state": word >> 24,
            "total": self.emu.peek32(base + 8),
            "taken": self.emu.peek32(base + 0xC),
            "next_seq": self.emu.peek32(base + 0x10),
        }

    def open_os_upgrade(self) -> None:
        self.tap(NO)
        self.tap(SETTINGS)
        for _ in range(7):
            self.tap(DOWN, 15_000_000)
        self.tap(YES, 40_000_000)  # SYSTEM
        self.tap(DOWN, 20_000_000)  # OS UPGRADE
        self.tap(YES, 60_000_000)
        self.step(100_000_000)


def rpc_exchange(
    rig: Rig, seq: int, kind: int, payload: bytes, max_wait: int = 600_000_000
) -> tuple[bytes | None, int]:
    """Send one MidiRpc request over the emulated USB and step until its reply is complete. Returns (reply payload or None, guest instructions used)."""
    rig.emu.usb_out()
    start = rig.used
    rig.emu.usb_in(rpc_request(kind, seq, payload), chunk=384, period=16_500)
    got = b""
    while rig.used - start < max_wait:
        rig.step(100_000)
        got += rig.emu.usb_out()
        if got.endswith(b"\xf7"):
            rpc_seq, rpc_kind, body = parse_rpc_reply(got)
            if rpc_seq != seq or rpc_kind != kind:
                raise SystemExit(
                    f"reply {rpc_seq:#x}/{rpc_kind:#x} for request {seq:#x}/{kind:#x}"
                )
            return body, rig.used - start
    return None, rig.used - start


def follow_screens(
    rig: Rig, prefix: str, rounds: int = 40, step: int = 100_000_000
) -> None:
    """Step and save every new OLED picture (the soft reset leaves no frame for a while)."""
    seen: set[str] = set()
    for _ in range(rounds):
        rig.step(step)
        try:
            h = rig.frame(f"{prefix}-latest")
        except Exception as err:
            print("no frame at", rig.used, err)
            return
        if h not in seen:
            seen.add(h)
            (rig.out / f"{prefix}-{len(seen):02d}.png").write_bytes(rig.emu.frame_png())
            print("screen", len(seen), h, "at", rig.used)


def check_flash(rig: Rig, stream: bytes) -> None:
    """When the device programs its flash (after End), compare the container area with the stream's container before the soft reset."""
    from tools.firmware.container import decode_syx

    emu = rig.emu
    last, same = None, 0
    for _ in range(60):
        rig.step(20_000_000)
        st = emu.call("flash.status")
        key = (st["erases"], st["programs"])
        if st["programs"] and key == last:
            same += 1
            if same >= 4:
                break
        else:
            same = 0
        last = key
    else:
        print("flash: no programming seen", emu.call("flash.status"))
        return
    tmp = rig.out / "stream.syx"
    tmp.write_bytes(stream)
    dec = decode_syx(str(tmp))
    total = int.from_bytes(dec[:4], "big")
    want = dec[8 : 8 + total]
    got = bytearray()
    while len(got) < len(want):
        n = min(1 << 20, len(want) - len(got))
        got += bytes.fromhex(
            emu.call("flash.read", offset=0x80000 + len(got), length=n)["hex"]
        )
    status = emu.call("flash.status")
    print(
        "flash container",
        "EQUALS" if bytes(got) == want else "DIFFERS",
        "the stream:",
        len(want),
        "bytes;",
        status["erases"],
        "erases",
        status["programs"],
        "programs",
    )


def run_rpc(rig: Rig, stream: bytes, a: argparse.Namespace) -> int:
    """The RPC route (finding 75): Start, Write chunks, End over midi.usb.in at the main screen, no menu."""
    emu = rig.emu
    seq = 1

    def call(kind: int, payload: bytes) -> tuple[bytes | None, int]:
        nonlocal seq
        reply, used = rpc_exchange(rig, seq, kind, payload)
        seq += 1
        return reply, used

    print("main screen", rig.frame("1-main"))
    version, _ = call(RPC_SOFTWARE_VERSION, b"")
    print("software version reply", version.hex() if version else None)
    flash0 = emu.call("flash.status")
    reply, used = call(RPC_OS_START, rpc_start(len(stream), a.rpc_auto))
    print(
        "start ->",
        reply.hex() if reply else None,
        "in",
        used,
        "instructions;",
        rig.frame("2-after-start"),
    )
    if reply is None or reply[0] != 0:
        return 1
    offset, turns, writes = 0, [], 0
    wall = time.monotonic()
    while offset < len(stream):
        if a.rpc_writes is not None and writes >= a.rpc_writes:
            break
        part = stream[offset : offset + a.rpc_chunk]
        retries = 0
        while True:
            reply, used = call(RPC_OS_WRITE, rpc_write(part, offset))
            if reply is None or len(reply) < 6:
                print("write", writes, "at", offset, "no reply")
                return 2
            taken, status = struct.unpack(">IB", reply[:5])
            if status in (0, 1) and taken == offset + len(part):
                break
            retries += 1
            print(
                "write",
                writes,
                "offset",
                offset,
                "taken",
                taken,
                "status",
                status,
                reply[5:].hex(),
            )
            if retries >= a.rpc_retries:
                return 3
        turns.append(used)
        offset += len(part)
        writes += 1
        if writes % 100 == 0 or offset >= len(stream):
            print(
                f"write {writes}: offset {offset} taken {taken} status {status} turnaround {used}"
            )
    print(
        "writes",
        writes,
        "bytes",
        offset,
        "last status",
        status if writes else None,
        "turnaround instructions min/median/max",
        min(turns) if turns else 0,
        sorted(turns)[len(turns) // 2] if turns else 0,
        max(turns) if turns else 0,
        "wall",
        round(time.monotonic() - wall, 1),
        "s",
    )
    print("after writes", rig.frame("3-after-writes"), rig.state())
    rig.step(300_000_000)
    print("settled", rig.frame("4-settled"))
    if a.rpc_writes is not None and offset < len(stream):
        print("cut transfer; flash", flash0, "->", emu.call("flash.status"))
        return 0
    if not a.rpc_auto and not a.rpc_no_end:
        reply, used = call(RPC_OS_END, rpc_end(not a.rpc_no_commit))
        print(
            "end ->",
            reply.hex() if reply else None,
            "in",
            used,
            "instructions;",
            rig.frame("5-after-end"),
        )
        rig.step(300_000_000)
        print("after end settle", rig.frame("6-after-end-settled"))
    if a.rpc_confirm != "none":
        rig.tap(YES if a.rpc_confirm == "yes" else NO, 100_000_000)
        print("after", a.rpc_confirm, rig.frame("7-after-key"))
    check_flash(rig, stream)
    follow_screens(rig, "8-screen")
    print("flash", flash0, "->", emu.call("flash.status"))
    return 0


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    p.add_argument("device", choices=["dt2", "dn2"])
    p.add_argument(
        "--binary", required=True, help="digi-emud binary (an immutable copy)"
    )
    p.add_argument("--ready", required=True, help="ready dir with digi-audio-m5.snap")
    p.add_argument(
        "--syx", help="stream to send (default: the stock file of the device)"
    )
    p.add_argument("--out", required=True)
    p.add_argument(
        "--limit", type=int, default=12_000_000_000, help="guest instructions in total"
    )
    p.add_argument(
        "--messages", type=int, help="send only the first N messages (a cut transfer)"
    )
    p.add_argument("--chunk", type=int, help="USB bytes per call (default 64)")
    p.add_argument(
        "--period", type=int, help="instructions between USB calls (default 132000)"
    )
    p.add_argument(
        "--poll",
        type=int,
        default=20_000_000,
        help="instructions between state samples",
    )
    p.add_argument(
        "--idle",
        type=int,
        default=25,
        help="stop after this many polls without a state change",
    )
    p.add_argument("--yes", action="store_true", help="press YES at the confirm dialog")
    p.add_argument(
        "--no-open",
        action="store_true",
        help="do not open the OS UPGRADE view (RPC-less control run)",
    )
    p.add_argument(
        "--patchkit",
        help="patchkit binary: flash with `patchkit flash --emulator` instead of sending the stream here",
    )
    p.add_argument(
        "--restore-after",
        action="store_true",
        help="with --patchkit: after the flash, restore the stock file the same way",
    )
    p.add_argument(
        "--exec",
        dest="exec_cmd",
        help="command to run at the OS UPGRADE screen instead of sending the stream here; {socket} is the control socket",
    )
    p.add_argument(
        "--exec-restore",
        dest="exec_restore",
        help="with --exec: open OS UPGRADE again after it and run this command",
    )
    p.add_argument(
        "--rpc",
        action="store_true",
        help="RPC route: OsUpgradeStart/Write/End at the main screen, no menu (finding 75)",
    )
    p.add_argument(
        "--rpc-chunk", type=int, default=2048, help="stream bytes per OsUpgradeWrite"
    )
    p.add_argument(
        "--rpc-auto", action="store_true", help="Start flag 1: no End, no confirm wait"
    )
    p.add_argument(
        "--rpc-no-end", action="store_true", help="send no End (the panel key decides)"
    )
    p.add_argument("--rpc-no-commit", action="store_true", help="End with commit 0")
    p.add_argument("--rpc-writes", type=int, help="send only N writes (a cut transfer)")
    p.add_argument("--rpc-retries", type=int, default=3)
    p.add_argument(
        "--rpc-confirm",
        choices=["none", "yes", "no"],
        default="none",
        help="panel key after End",
    )
    a = p.parse_args(argv)

    syx = pathlib.Path(a.syx or ROOT / "firmware" / SYX[a.device])
    msgs = split_messages(syx.read_bytes())
    if a.messages is not None:
        msgs = msgs[: a.messages]
    out = pathlib.Path(a.out)
    snap = pathlib.Path(a.ready) / "digi-audio-m5.snap"
    emu = Emu.spawn(
        "--dsp",
        "zero",
        "--cf-snapshot",
        str(snap),
        binary=a.binary,
        run_dir=out / "run",
    )
    t0 = time.monotonic()
    try:
        emu.call(
            "firmware.load",
            syx=str((ROOT / "firmware" / SYX[a.device]).resolve()),
            start="snapshot",
            force=True,
            auto_no=True,
        )
        rig = Rig(emu, a.device, out, a.limit)
        rig.step(300_000_000)
        # with --rpc and --exec or --patchkit the rig only boots to the main screen: the command speaks the RPC route itself
        if a.rpc and not (a.exec_cmd or a.patchkit):
            return run_rpc(rig, syx.read_bytes(), a)
        if not a.no_open and not a.rpc:
            rig.open_os_upgrade()
        print("waiting screen", rig.frame("1-waiting"), rig.state())
        if a.exec_cmd:
            runs = [("exec", a.exec_cmd)] + (
                [("exec-restore", a.exec_restore)] if a.exec_restore else []
            )
            env = {**os.environ, "DIGI_FW_EMU_SOCKET": emu.path}
            for n, (name, command) in enumerate(runs):
                if n and not a.rpc:
                    rig.open_os_upgrade()
                    print("waiting screen", rig.frame(f"{n + 1}-waiting-{name}"))
                argv_run = [
                    part.format(socket=emu.path) for part in shlex.split(command)
                ]
                t1 = time.monotonic()
                done = subprocess.run(argv_run, cwd=ROOT, env=env)
                print(
                    name,
                    "exit",
                    done.returncode,
                    "wall",
                    round(time.monotonic() - t1, 1),
                    "s",
                )
                print("after", name, rig.frame(f"{n + 1}-after-{name}"))
                if done.returncode:
                    return done.returncode
            return 0
        if a.patchkit:
            stock = ROOT / "firmware" / SYX[a.device]
            runs = [("flash", [])] + (
                [("restore", ["--restore"])] if a.restore_after else []
            )
            for n, (name, extra) in enumerate(runs):
                if n and not a.rpc:
                    rig.open_os_upgrade()
                    print("waiting screen", rig.frame(f"{n + 1}-waiting-{name}"))
                cmd = [
                    a.patchkit,
                    "flash",
                    str(stock),
                    "--emulator",
                    emu.path,
                    "--yes",
                    *extra,
                ]
                t1 = time.monotonic()
                done = subprocess.run(cmd, cwd=ROOT)
                print(
                    name,
                    "exit",
                    done.returncode,
                    "wall",
                    round(time.monotonic() - t1, 1),
                    "s",
                )
                print("after", name, rig.frame(f"{n + 1}-after-{name}"))
                if done.returncode:
                    return done.returncode
            return 0
        emu.usb_out()
        emu.usb_in(b"".join(msgs), chunk=a.chunk, period=a.period)
        sent_at = rig.used
        last = None
        out_bytes = b""
        done_at = None
        idle = 0
        events: list[dict] = []
        while rig.used < a.limit:
            rig.step(a.poll)
            st = rig.state()
            o = emu.usb_out()
            out_bytes += o
            if st != last or o:
                events.append({"instr": rig.used - sent_at, "usb_out": len(o), **st})
                last = st
                idle = 0
            else:
                idle += 1
                if idle >= a.idle:
                    print("no progress for", idle * a.poll, "instructions", st)
                    break
            if st["state"] == 3 and done_at is None:
                done_at = rig.used - sent_at
                print("stream complete", st, "instructions since first send", done_at)
                break
        rig.step(a.poll * 5)
        print(
            "after stream",
            rig.frame("2-after-stream"),
            rig.state(),
            "usb_out bytes",
            len(out_bytes),
        )
        if a.yes and done_at is not None:
            rig.tap(YES, 100_000_000)
            print("after YES", rig.frame("3-after-yes"))
            seen = set()
            while rig.used < a.limit:
                rig.step(100_000_000)
                try:
                    h = rig.frame("4-latest")
                except Exception as err:  # the soft reset after the flash write leaves no frame for a while
                    print("no frame at", rig.used, err)
                    break
                if h not in seen:
                    seen.add(h)
                    (out / f"5-screen-{len(seen)}.png").write_bytes(emu.frame_png())
                    print("screen", len(seen), h, "at", rig.used)
                if len(seen) >= 12:
                    break
        (out / "events.json").write_text(
            json.dumps({"messages": len(msgs), "events": events}, indent=1)
        )
        print(
            "wall",
            round(time.monotonic() - t0, 1),
            "s; guest",
            rig.used,
            "instructions; usb_out",
            out_bytes[:64].hex(),
        )
    finally:
        emu.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
