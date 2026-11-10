#!/usr/bin/env python3
"""Upload, list and delete the files of the digikit module store over USB MIDI SysEx (docs/design/digikit-upload.md).

    uv run python -m tools.device.dkupload --emu info                       # the running emulator (out/native/run/current.json)
    uv run python -m tools.device.dkupload --emu ls [PREFIX]               # the store's entries (a folder with nothing in it: size 0)
    uv run python -m tools.device.dkupload --emu mkdir samples/drums
    uv run python -m tools.device.dkupload --emu modules                   # the loaded modules: ranges, contracts, assigned types, guard faults
    uv run python -m tools.device.dkupload --emu put build/m.dkm modules/m.dkm
    uv run python -m tools.device.dkupload --emu rm modules/m.dkm
    uv run python -m tools.device.dkupload --emu clear
    uv run python -m tools.device.dkupload --emu settings [-o FILE] [--json]   # the settings record (read only); FILE goes to `patchkit store dump --record FILE`
    uv run python -m tools.device.dkupload --emu --run-dir out/runs/patchkit/run-dev-DEVICE put build/m.dkm   # the daemon of `mise run patch:dev` (pk_dev), not the desktop app's
    uv run --with python-rtmidi python -m tools.device.dkupload --port Digitakt info    # a real device (not exercised in CI)
    uv run python -m tools.device.dkupload --winmm "Digitone II" info      # a real device on Windows, winmm (tools/device/winmidi.py, no package)
    uv run python -m tools.device.dkupload --winmm "Digitone II" put s.wav samples/s.wav --chunk 8192 --depth 1 --timing put.jsonl   # timed
    uv run python -m tools.device.dkupload --emu remote state              # digikit/remote (docs/plans/remote-control.md): the module state
    uv run python -m tools.device.dkupload --emu remote key 10             # press and release a key (the device file's panel code); --hold S keeps it down
    uv run python -m tools.device.dkupload --emu remote turn 1 -5          # turn encoder 1 by -5 detents

`put FILE [PATH]` streams FILE (read from disk in chunks, so a large sample is not held in memory) to PATH (default: `modules/<file name>`) in chunks of at most 32 KiB, two requests in flight, and
checks the file's hash on the device (END reads the staged file back from the card). `--chunk` and `--depth` set the chunk size and the
requests in flight; `put` prints a timing line (tools/device/dktiming.py) and `--timing FILE` keeps every request and reply as JSON lines. It changes only the module store; an uploaded
module appears in DIGIKIT > MODULES and loads after APPLY & RESTART (or `tools.device.dkrun`, which also selects and restarts).

Python (the kept scenarios use it): ``Uploader(EmuTransport(emu))`` or ``Uploader(RtMidiTransport("Digitakt"))``. The framing is
`frame()` / `parse_reply()`; the 8-in-7 packing is `tools.firmware.build.encode_8in7` and `tools.device.midisniff.unpack_8in7`.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import struct
import sys
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

from tools.device import dkproto
from tools.device.dkproto import (
    CAP,
    CMD_ABORT,
    CMD_BEGIN,
    CMD_CAPACITY,
    CMD_CLEAR,
    CMD_DATA,
    CMD_DELETE,
    CMD_END,
    CMD_INFO,
    CMD_LIST,
    CMD_MKDIR,
    CMD_MODULES,
    CMD_READ,
    CMD_RENAME,
    CMD_RESTART,
    CMD_SELECT,
    CMD_SETTINGS,
    CMD_VERIFY,
    LOAD_STATUS,
    MAX_CHUNK,
    READ_MAX,
    VERSION,
)
from tools.device.midisniff import unpack_8in7
from tools.firmware.build import encode_8in7

PREFIX = bytes((0xF0, 0x7D, 0x64, 0x6B))
CLEAR_MAGIC = 0x434C5221
DEPTH = 2
E_OFFSET = next(c for c, (n, _) in dkproto.STATUS.items() if n == "E_OFFSET")
HEADER_BYTES = 0xA0  # a .dkm header (docs/design/digikit-dkm.md)

# status code -> name: the upload statuses, and the load and selection codes (SELECT answers them as the menu's APPLY gets them)
STATUS: dict[int, str] = {
    0: "ok",
    **{c: n for c, (n, _) in LOAD_STATUS.items()},
    **{c: n for c, (n, _) in dkproto.STATUS.items()},
}

# The runtimes before INFO carried `caps` had these features from this packed version on (0.0.3 LOG and PEEK, 0.0.4 SELECT and
# RESTART, 0.1.0 LOGCFG). MODULES is known by `caps` only.
CAP_SINCE: dict[str, int] = {
    "LOG": 0x000003,
    "PEEK": 0x000003,
    "SELECT": 0x000004,
    "RESTART": 0x000004,
    "LOGCFG": 0x000100,
}


def fmt_runtime(packed: int) -> str:
    """A packed runtime version (major << 16 | minor << 8 | patch) as "major.minor.patch"; the plain integers 1 to 5 of old runtimes read as 0.0.N."""
    return f"{packed >> 16}.{packed >> 8 & 255}.{packed & 255}"


def fmt_caps(caps: int) -> str:
    """The INFO `caps` word as the names of its features, in bit order ("LOG PEEK ..."); a bit this tool does not know prints as `bitN`."""
    names = [n for n, b in sorted(CAP.items(), key=lambda x: x[1]) if caps >> b & 1]
    known = sum(1 << b for b in CAP.values())
    names += [f"bit{b}" for b in range(32) if caps >> b & 1 and not known >> b & 1]
    return " ".join(names)


def info_lines(info: dict[str, Any]) -> list[str]:
    """The `info` command's lines: every field as `key: value`, the runtime as `0.2.0 (512)` (version and packed number), the caps as names.
    The dict `Uploader.info` returns is unchanged."""
    out = []
    for k, v in info.items():
        if k == "runtime" and isinstance(v, int):
            v = f"{fmt_runtime(v)} ({v})"
        elif k == "caps" and isinstance(v, int):
            v = fmt_caps(v)
        out.append(f"{k}: {v}")
    return out


def has_cap(info: dict[str, Any], name: str) -> bool:
    """Does the device have the feature `name` (a key of `CAP`)? From the INFO `caps` word; a runtime without one is judged by its version
    (`CAP_SINCE`), and a feature known by `caps` only (MODULES) is absent there."""
    if name not in CAP:
        raise KeyError(name)
    if info.get("caps") is not None:
        return bool(info["caps"] >> CAP[name] & 1)
    since = CAP_SINCE.get(name)
    return since is not None and info["runtime"] >= since


class FileHasher:
    """The store's hash slot of a file, streaming (BEGIN, LIST, VERIFY): the +Drive content hash (`dksample::content_hash`, a lookup3 variant over
    big-endian words, seed 0x43fa243a) `| 1`, 4 bytes big-endian, then 28 zeros. The same value as `dksample::file_hash` (Rust), `fileHash`
    (TypeScript) and the runtime's `hash32.rs`, with the same carry: the variant keeps the last 1 to 12 bytes for the tail, so a block is mixed
    only when more than 12 bytes are known and up to 12 bytes wait between updates."""

    M = 0xFFFFFFFF

    def __init__(self) -> None:
        self.a = self.b = self.c = 0x43FA243A
        self.carry = b""

    @classmethod
    def _rot(cls, x: int, n: int) -> int:
        return ((x << n) | (x >> (32 - n))) & cls.M

    def _mix(self, k0: int, k1: int, k2: int) -> None:
        m, rot = self.M, self._rot
        a, b, c = self.a, self.b, self.c
        c = (c + k2) & m
        t2 = (k1 + b + c) & m
        u1 = rot(c, 4) ^ ((k0 + a - c) & m)
        t3 = (t2 + u1) & m
        u2 = ((k1 + b - u1) & m) ^ rot(u1, 6)
        t4 = (t3 + u2) & m
        u3 = ((t2 - u2) & m) ^ rot(u2, 8)
        t2b = (t4 + u3) & m
        u4 = ((t3 - u3) & m) ^ rot(u3, 16)
        a = (t2b + u4) & m
        u5 = ((t4 - u4) & m) ^ rot(u4, 19)
        self.a, self.b, self.c = a, (a + u5) & m, ((t2b - u5) & m) ^ rot(u5, 4)

    def update(self, data: bytes) -> None:
        buf = self.carry + data
        off = 0
        while len(buf) - off > 12:
            self._mix(*struct.unpack_from(">III", buf, off))
            off += 12
        self.carry = buf[off:]

    def digest(self) -> bytes:
        m, rot = self.M, self._rot
        a, b, c = self.a, self.b, self.c
        d, n = self.carry, len(self.carry)
        h = c
        if n:
            word = lambda x: struct.unpack(">I", x.ljust(4, b"\0"))[0]  # noqa: E731
            if n <= 4:
                a = (a + word(d)) & m
            elif n <= 8:
                a = (a + word(d[:4])) & m
                b = (b + word(d[4:])) & m
            else:
                a = (a + word(d[:4])) & m
                b = (b + word(d[4:8])) & m
                c = (c + word(d[8:])) & m
            u1 = ((b ^ c) - rot(b, 14)) & m
            u2 = ((u1 ^ a) - rot(u1, 11)) & m
            u3 = ((u2 ^ b) - rot(u2, 25)) & m
            u4 = ((u3 ^ u1) - rot(u3, 16)) & m
            a2 = ((u4 ^ u2) - rot(u4, 4)) & m
            b2 = ((a2 ^ u3) - rot(a2, 14)) & m
            h = ((b2 ^ u4) - rot(b2, 24)) & m
        return struct.pack(">I", h | 1) + bytes(28)


def file_hash(data: bytes) -> bytes:
    """The hash slot of `data` (see `FileHasher`)."""
    h = FileHasher()
    h.update(data)
    return h.digest()


class UploadError(RuntimeError):
    def __init__(self, status: int, what: str, body: bytes = b""):
        super().__init__(f"{what}: {STATUS.get(status, status)}")
        self.status, self.body = status, body


def frame(cmd: int, seq: int, body: bytes = b"", version: int = VERSION) -> bytes:
    """One request message, F0 to F7."""
    return (
        PREFIX + bytes((version, cmd & 0x7F, seq & 0x7F)) + encode_8in7(body) + b"\xf7"
    )


def select_body(paths: list[str]) -> bytes:
    """The body of SELECT: `u8 n`, then each store path and a NUL."""
    if len(paths) > 8:
        raise ValueError("a selection holds at most 8 paths")
    return bytes((len(paths),)) + b"".join(p.encode() + b"\0" for p in paths)


def parse_reply(msg: bytes) -> tuple[int, int, int, bytes] | None:
    """(cmd, seq, status, body after the status) of one of our replies, or None for any other message."""
    if len(msg) < 9 or msg[:4] != PREFIX or msg[-1] != 0xF7 or not msg[5] & 0x40:
        return None
    body = unpack_8in7(msg[7:-1])
    if not body:
        return None
    status = body[0] - 256 if body[0] >= 128 else body[0]
    return msg[5] & 0x3F, msg[6], status, body[1:]


def split_messages(stream: bytearray) -> list[bytes]:
    """Take the complete F0..F7 messages off the front of a byte stream (other bytes are dropped)."""
    out = []
    while True:
        a = stream.find(b"\xf0")
        if a < 0:
            stream.clear()
            return out
        b = stream.find(b"\xf7", a)
        if b < 0:
            del stream[:a]
            return out
        out.append(bytes(stream[a : b + 1]))
        del stream[: b + 1]


class EmuTransport:
    """The emulator's USB MIDI path: `midi.usb.in` hands a message to the firmware's receive callback, `midi.usb.out` returns what
    the firmware sent. Time passes only when stepped, so `recv` steps the emulator until a reply is there."""

    def __init__(
        self,
        emu: Any,
        step: int = 400_000,
        limit: int = 4_000_000_000,
        chunk: int | None = None,
        period: int | None = None,
    ):
        # CHUNK and PERIOD: the delivery model of `midi.usb.in` (default 64 bytes per 132,000 instructions, a full-speed frame; the
        # high-speed model is 384 and 16,500)
        self.emu, self.step, self.limit = emu, step, limit
        self.chunk, self.period = chunk, period
        self.buf = bytearray()
        self.ready: list[bytes] = []  # complete replies not yet handed out
        self.stepped = 0  # instructions stepped by recv (the host-side cost)

    def send(self, msg: bytes) -> None:
        self.emu.usb_in(msg, self.chunk, self.period)

    def recv(self, timeout_s: float = 0.0) -> bytes:
        spent = 0
        while True:
            self.buf += self.emu.usb_out()
            self.ready += [m for m in split_messages(self.buf) if parse_reply(m)]
            if self.ready:
                return self.ready.pop(0)
            if spent >= self.limit:
                raise TimeoutError("no reply from the emulated device")
            self.emu.step(self.step)
            spent += self.step
            self.stepped += self.step


class RtMidiTransport:
    """A real device through python-rtmidi (`pip` package `python-rtmidi`, not a repository dependency)."""

    def __init__(self, port_substring: str):
        import rtmidi  # type: ignore[import-not-found]

        self.mi, self.mo = rtmidi.MidiIn(), rtmidi.MidiOut()
        names_in, names_out = self.mi.get_ports(), self.mo.get_ports()
        pi = next(
            (i for i, n in enumerate(names_in) if port_substring.lower() in n.lower()),
            None,
        )
        po = next(
            (i for i, n in enumerate(names_out) if port_substring.lower() in n.lower()),
            None,
        )
        if pi is None or po is None:
            raise SystemExit(f"no MIDI port matching {port_substring!r}: {names_in}")
        self.mi.open_port(pi)
        self.mo.open_port(po)
        self.mi.ignore_types(sysex=False, timing=True, active_sense=True)

    def close(self) -> None:
        for p in (self.mi, self.mo):
            with contextlib.suppress(Exception):  # the device may be gone already
                p.close_port()

    def send(self, msg: bytes) -> None:
        self.mo.send_message(list(msg))

    def recv(self, timeout_s: float = 5.0) -> bytes:
        end = time.monotonic() + timeout_s
        while time.monotonic() < end:
            m = self.mi.get_message()
            if m:
                data = bytes(m[0])
                if parse_reply(data) is not None:
                    return data
            else:
                time.sleep(0.001)
        raise TimeoutError("no reply from the device")


class Uploader:
    """The host side of docs/design/digikit-upload.md over a transport with `send(bytes)` and `recv() -> bytes`.

    `skip_foreign` drops replies to other hosts' requests (another program polling the same MIDI port) instead of raising.
    """

    def __init__(
        self,
        transport: Any,
        chunk: int = MAX_CHUNK,
        depth: int = DEPTH,
        skip_foreign: bool = False,
    ):
        if chunk <= 0 or chunk > MAX_CHUNK or chunk % 512:
            raise ValueError("chunk is a multiple of 512 up to 32768")
        self.t, self.chunk, self.depth = transport, chunk, depth
        self.skip_foreign = skip_foreign
        self.seq = 0
        self.log: list[tuple[str, int]] = []
        self._file_hash_ok = False

    def _need_file_hash(self) -> None:
        """Fail at once for a runtime whose store keeps SHA-256 (asked once, before the first command that carries a file hash)."""
        if not self._file_hash_ok:
            if not has_cap(self.info(), "FILEHASH"):
                raise UploadError(
                    0, "Update the firmware: this runtime uses the old store format"
                )
            self._file_hash_ok = True

    # -- one request, one reply
    def _next_seq(self) -> int:
        self.seq = (self.seq + 1) & 0x7F
        return self.seq

    def post(self, cmd: int, body: bytes = b"", version: int = VERSION) -> int:
        seq = self._next_seq()
        self.t.send(frame(cmd, seq, body, version))
        return seq

    def wait(self, cmd: int, seq: int) -> tuple[int, bytes]:
        for _ in range(64 if self.skip_foreign else 1):
            r = parse_reply(self.t.recv())
            assert r is not None
            rcmd, rseq, status, body = r
            if rcmd == cmd and rseq == seq:
                break
        else:
            raise UploadError(
                status, f"reply for cmd {rcmd:#x} seq {rseq}, wanted {cmd:#x} seq {seq}"
            )
        self.log.append((f"{cmd:#x}", status))
        return status, body

    def request(
        self, cmd: int, body: bytes = b"", version: int = VERSION
    ) -> tuple[int, bytes]:
        """(status, body after the status) without raising on a non-zero status."""
        return self.wait(cmd, self.post(cmd, body, version))

    def _ok(self, cmd: int, what: str, body: bytes = b"") -> bytes:
        status, rb = self.request(cmd, body)
        if status != 0:
            raise UploadError(status, what, rb)
        return rb

    # -- commands
    def info(self) -> dict[str, Any]:
        b = self._ok(CMD_INFO, "info")
        proto, runtime = struct.unpack_from(">HH", b, 0)
        end = b.index(0, 4)
        port = b[4:end].decode()
        free, max_chunk, flags = struct.unpack_from(">IIB", b, end + 1)
        # runtime 3 appends two words for the log range of the old runtime; the runtime now sends zeros (the LOG command of the debug module has the range)
        oldest, newest = (
            struct.unpack_from(">II", b, end + 10) if len(b) >= end + 18 else (0, 0)
        )
        # appended after the log range: the packed runtime version (u32) and the capability bits (u32)
        caps = None
        image_id = None
        if len(b) >= end + 26:
            runtime, caps = struct.unpack_from(">II", b, end + 18)
        # runtime 0.5.2 appends the id of the flashed image (8 bytes, patchkit stamps it) after the capability bits
        if caps is not None and len(b) >= end + 34:
            image_id = b[end + 26 : end + 34].hex()
        return {
            "protocol": proto,
            "runtime": runtime,
            "caps": caps,
            "image_id": image_id,
            "port": port,
            "free_bytes": free,
            "max_chunk": max_chunk,
            "safe": bool(flags & 1),
            "upload_open": bool(flags & 2),
            "card_read_only": bool(flags & 4),
            "debug_on": bool(flags & 8),
            "crash_found": bool(flags & 16),
            "log_oldest": oldest,
            "log_newest": newest,
        }

    def ls(self, prefix: str | None = None) -> list[tuple[str, int, str]]:
        """Every file of the store, or (`prefix`, runtime 0.3.0, capability FOLDERS) the entries whose path starts with it: (path, size, hash hex),
        in the store's order. A folder with nothing in it is listed with size 0 and a zero hash."""
        self._need_file_hash()
        out: list[tuple[str, int, str]] = []
        first = 0
        tail = prefix.encode() + b"\0" if prefix else b""
        while True:
            b = self._ok(CMD_LIST, "ls", struct.pack(">I", first) + tail)
            total, n = struct.unpack_from(">IB", b, 0)
            at = 5
            for _ in range(n):
                end = b.index(0, at)
                path = b[at:end].decode()
                size = struct.unpack_from(">I", b, end + 1)[0]
                out.append((path, size, b[end + 5 : end + 37].hex()))
                at = end + 37
            first += n
            if first >= total or n == 0:
                return out

    def modules(self) -> dict[str, Any]:
        """MODULES (runtime 0.2.0, capability MODULES): the arena and DSP-area figures and the loaded modules, every page. Returns
        `{arena_free, arena_largest, dsp_free, dsp_largest, total, modules: [...]}`; a module is `{index, name, version, contracts
        ((major, minor) of api, machine, fx, service), cf_base, cf_size, dsp_off, dsp_len, rows (voice-row mask), types (assigned machine
        types), faults (the guard's count for those types), guard_off (the guard switched one of them off)}`."""
        out: dict[str, Any] = {"modules": []}
        start = 0
        while True:
            b = self._ok(CMD_MODULES, "modules", bytes((start,)))
            af, al, df, dl = struct.unpack_from(">IIII", b, 0)
            total, n = b[16], b[17]
            out.update(
                arena_free=af,
                arena_largest=al,
                dsp_free=df,
                dsp_largest=dl,
                total=total,
            )
            at = 18
            for _ in range(n):
                idx = b[at]
                name = b[at + 1 : at + 33].split(b"\0")[0].decode()
                version = struct.unpack_from(">I", b, at + 33)[0]
                cts = [(b[at + 37 + 2 * k], b[at + 38 + 2 * k]) for k in range(4)]
                cf_base, cf_size, dsp_off, dsp_len, rows = struct.unpack_from(
                    ">IIIII", b, at + 45
                )
                nt = b[at + 65]
                types = list(b[at + 66 : at + 66 + nt])
                faults = struct.unpack_from(">I", b, at + 66 + nt)[0]
                flags = b[at + 70 + nt]
                out["modules"].append(
                    {
                        "index": idx,
                        "name": name,
                        "version": version,
                        "contracts": cts,
                        "cf_base": cf_base,
                        "cf_size": cf_size,
                        "dsp_off": dsp_off,
                        "dsp_len": dsp_len,
                        "rows": rows,
                        "types": types,
                        "faults": faults,
                        "guard_off": bool(flags & 1),
                    }
                )
                at += 71 + nt
                start = idx + 1
            if len(out["modules"]) >= total or n == 0:
                return out

    def capacity(self) -> dict[str, dict[str, int]]:
        """CAPACITY (runtime 0.2.2, capability CAPACITY): per resource `{limit, used, largest}`, keyed by the kind's name (`MODULES`, `ARENA`,
        `DSP_AREA`, `MACHINE_ROWS`, `VOICE_ROWS` ..., sdk/sdk.toml `[capacity.kinds]`); a kind this tool does not know is `kind<N>`.
        Arena and DSP figures are bytes; `largest` is 0 where it has no meaning. Raises `UploadError` on a runtime without the command."""
        b = self._ok(CMD_CAPACITY, "capacity")
        names = {v: k for k, v in dkproto.CAPACITY_KINDS.items()}
        out: dict[str, dict[str, int]] = {}
        for i in range(b[1]):
            kind, limit, used, largest = struct.unpack_from(">BIII", b, 2 + 13 * i)
            out[names.get(kind, f"kind{kind}")] = {
                "limit": limit,
                "used": used,
                "largest": largest,
            }
        return out

    def log_page(
        self, from_seq: int
    ) -> tuple[int, int, list[tuple[int, int, int, int, tuple[int, ...]]]]:
        """LOG (digikit/debug; capability LOG): (oldest, newest, events) from `from_seq`; an event is (seq, time, kind, flags, (a0..a4)). Empty when the host has
        read everything."""
        b = self._ok(dkproto.SERVICE_CMD["LOG"], "log", struct.pack(">I", from_seq))
        oldest, newest, n = struct.unpack_from(">IIB", b, 0)
        events = []
        for i in range(n):
            seq, t, kind, flags, *a = struct.unpack_from(">IIHH5I", b, 9 + 32 * i)
            events.append((seq, t, kind, flags, tuple(a)))
        return oldest, newest, events

    def peek(self, addr: int, length: int) -> bytes:
        """PEEK (digikit/debug): `length` (1 to 512) bytes of RAM at `addr`, inside the ranges the device file lists (`[digikit] peek`)."""
        b = self._ok(
            dkproto.SERVICE_CMD["PEEK"], "peek", struct.pack(">IH", addr, length)
        )
        got, n = struct.unpack_from(">IH", b, 0)
        if got != addr or n != length or len(b) < 6 + n:
            raise UploadError(-50, "peek: malformed reply")
        return b[6 : 6 + n]

    def logcfg(
        self, default: int | None = None, modules: dict[int, int | None] | None = None
    ) -> dict[str, Any]:
        """LOGCFG (digikit/debug): the log levels. With no argument, read them; else set `default` and/or the levels of module records (`modules`: record index ->
        level, None for the default). Returns {"default": int, "modules": [8 levels, 0xff = the default], "debug": bool}."""

        def parse(b: bytes) -> dict[str, Any]:
            if len(b) < 10:
                raise UploadError(-50, "logcfg: malformed reply")
            return {"default": b[0], "modules": list(b[1:9]), "debug": bool(b[9])}

        cur = parse(self._ok(dkproto.SERVICE_CMD["LOGCFG"], "logcfg", b"\x00"))
        if default is None and not modules:
            return cur
        lv = list(cur["modules"])
        for i, v in (modules or {}).items():
            lv[i] = 0xFF if v is None else v
        d = cur["default"] if default is None else default
        return parse(
            self._ok(dkproto.SERVICE_CMD["LOGCFG"], "logcfg", bytes([1, d, *lv]))
        )

    def select(self, paths: list[str]) -> None:
        """SELECT (runtime 4): save the selection record (what the next boot loads); the menu's code path, so its card rules apply."""
        self._ok(CMD_SELECT, "select", select_body(paths))

    def restart(self) -> None:
        """RESTART (runtime 4): the reply comes first, then the device soft-resets (as APPLY & RESTART)."""
        self._ok(CMD_RESTART, "restart")

    # -- digikit/remote (docs/plans/remote-control.md): the commands of a run-time service module, `dkproto.SERVICE_CMD`
    def remote_state(self) -> dict[str, int]:
        """REMOTE_STATE: `enabled` (1 while the module is loaded), the state `format` and the `turn_limit` of one TURN. When digikit/remote
        is not loaded this raises `UploadError` with E_PROTO."""
        b = self._ok(dkproto.SERVICE_CMD["REMOTE_STATE"], "remote state")
        return {"enabled": b[0], "format": b[1], "turn_limit": b[2]}

    def remote_key(self, code: int, down: bool) -> None:
        """REMOTE_KEY: press (`down`) or release the key with the device file's panel code."""
        self._ok(
            dkproto.SERVICE_CMD["REMOTE_KEY"],
            "remote key",
            bytes((code, 1 if down else 0)),
        )

    def remote_turn(self, encoder: int, delta: int, pace_s: float = 0.15) -> None:
        """REMOTE_TURN: turn an encoder by `delta` detents. One request takes at most the state's `turn_limit`; the rest is sent again after `pace_s`."""
        left = delta
        stalled = 0
        while left:
            part = max(-127, min(127, left))
            b = self._ok(
                dkproto.SERVICE_CMD["REMOTE_TURN"],
                "remote turn",
                bytes((encoder, part & 0xFF)),
            )
            took = b[0] - 256 if b[0] > 127 else b[0]
            left -= took
            stalled = 0 if took else stalled + 1
            if stalled > 200:
                raise UploadError(0, "remote turn: the device takes no more")
            if left:
                time.sleep(pace_s)

    def remote_release(self) -> None:
        """REMOTE_RELEASE: let go of every key the module holds (a host sends it when it stops)."""
        self._ok(dkproto.SERVICE_CMD["REMOTE_RELEASE"], "remote release")

    def begin(self, path: str, size: int, hash: bytes) -> None:
        self._need_file_hash()
        self._ok(
            CMD_BEGIN,
            "begin",
            path.encode() + b"\0" + struct.pack(">I", size) + hash,
        )

    def end(self) -> None:
        self._ok(CMD_END, "end")

    def abort(self) -> None:
        self._ok(CMD_ABORT, "abort")

    def read(self, path: str, offset: int, length: int) -> tuple[int, bytes]:
        """READ (runtime 0.2.4, capability READ): (file size, the bytes) of a store file from `offset`. The device returns at most
        768 bytes and none past the end of the file, so the bytes may be fewer than `length`. Raises `UploadError`: E_NOFILE (no such
        file), E_OFFSET (offset at or past the end), E_LENGTH (length 0)."""
        b = self._ok(
            CMD_READ, "read", path.encode() + b"\0" + struct.pack(">II", offset, length)
        )
        if len(b) < 4:
            raise UploadError(-50, "read: malformed reply")
        return struct.unpack_from(">I", b, 0)[0], b[4:]

    def settings_record(self) -> tuple[int, int, int, bytes]:
        """SETTINGS (runtime 0.5.1, capability SETTINGS), read only: the settings record as the runtime holds it (docs/design/digikit-store.md), read in pieces of 768 bytes:
        `(flags, copy, seq, bytes)`. `flags` bit 0: the runtime has changes the card does not hold yet (the runtime writes 2 s after the last change); `copy`: the card's
        current copy (0 A, 1 B, 2 none); `seq`: the card's. The bytes are one record (`tools.device.dksettings.decode`, `patchkit store dump --record`). Raises
        `UploadError`: E_SET_NOKEY when the card has no record and none was made."""
        out = b""
        total = None
        head = (0, 2, 0)
        while total is None or len(out) < total:
            b = self._ok(
                CMD_SETTINGS, "settings", struct.pack(">IH", len(out), READ_MAX)
            )
            if len(b) < 10:
                raise UploadError(-50, "settings: malformed reply")
            flags, copy, seq, used = struct.unpack_from(">BBII", b, 0)
            if total is None:
                total, head = used, (flags, copy, seq)
            elif (flags, copy, seq, used) != (*head, total):
                raise UploadError(-50, "settings: the record changed while it was read")
            if len(b) == 10:
                raise UploadError(-50, "settings: empty piece")
            out += b[10:]
        return head[0], head[1], head[2], out

    def verify(
        self, path: str, offset: int | None = None, length: int | None = None
    ) -> str:
        """VERIFY (runtime 0.2.4, capability VERIFY): the hash slot (hex, `file_hash`) of a store file, computed by the device from a fresh read of the card.
        With `offset` (a multiple of 512) and `length` (runtime 0.3.0): of that range of the file, so a large file is checked in pieces.
        Raises `UploadError`: E_NOFILE, E_READ, E_OFFSET, E_LENGTH."""
        self._need_file_hash()
        body = path.encode() + b"\0"
        if offset is not None:
            body += struct.pack(">II", offset, length or 0)
        b = self._ok(CMD_VERIFY, "verify", body)
        if len(b) != 32:
            raise UploadError(-50, "verify: malformed reply")
        return b.hex()

    def verify_file(
        self,
        path: str,
        src: Path,
        piece: int = 1 << 20,
        progress: Callable[[int, int], None] | None = None,
    ) -> bool:
        """Is the file on the card the bytes of `src`? One VERIFY per `piece` bytes (default 1 MiB), each compared with the hash of the same
        range of `src`; stops at the first difference."""
        size = src.stat().st_size
        if size == 0:
            return self.verify(path) == file_hash(b"").hex()
        with src.open("rb") as f:
            for off in range(0, size, piece):
                n = min(piece, size - off)
                f.seek(off)
                want = file_hash(f.read(n)).hex()
                if self.verify(path, off, n) != want:
                    return False
                if progress:
                    progress(off + n, size)
        return True

    def header(self, path: str) -> bytes:
        """The first 0xa0 bytes of a store file (a .dkm header; fewer for a shorter file)."""
        return self.read(path, 0, HEADER_BYTES)[1]

    def read_file(
        self, path: str, progress: Callable[[int, int], None] | None = None
    ) -> bytes:
        """The whole file, in READ requests of 768 bytes (an empty file raises E_OFFSET: READ has nothing to return)."""
        out = bytearray()
        size = 1
        while len(out) < size:
            size, got = self.read(path, len(out), READ_MAX)
            if not got:
                raise UploadError(-50, "read: empty reply")
            out += got
            if progress:
                progress(len(out), size)
        return bytes(out)

    def rm(self, path: str) -> None:
        self._ok(CMD_DELETE, "rm", path.encode() + b"\0")

    def mkdir(self, path: str) -> None:
        """MKDIR (runtime 0.3.0, capability FOLDERS): make a folder. Ok when it exists; E_PATH when the path is a file or inside one."""
        self._ok(CMD_MKDIR, "mkdir", path.encode() + b"\0")

    def rename(self, src: str, dst: str) -> None:
        """RENAME (runtime 0.5.7, capability RENAME): rename or move a file or an empty folder. The data and its hash stay; the folder it
        leaves stays. E_NOFILE: no such entry; E_PATH: the new path is taken, inside a file, inside the entry itself, or breaks the rules."""
        self._ok(CMD_RENAME, "rename", src.encode() + b"\0" + dst.encode() + b"\0")

    def clear(self) -> None:
        self._ok(CMD_CLEAR, "clear", struct.pack(">I", CLEAR_MAGIC))

    def put(
        self,
        path: str,
        data: bytes,
        progress: Callable[[int, int], None] | None = None,
        hash: bytes | None = None,
    ) -> None:
        """Store `data` at `path`: BEGIN, DATA chunks with up to `depth` in flight, END. Raises `UploadError` at the first bad
        status (an open upload is then left for the caller to ABORT)."""
        self._put(
            path,
            len(data),
            lambda off, n: data[off : off + n],
            hash or file_hash(data),
            progress,
        )

    def put_file(
        self,
        path: str,
        src: Path,
        progress: Callable[[int, int], None] | None = None,
    ) -> None:
        """`put` from a file on disk without holding it in memory (a sample can be hundreds of MB): one pass feeds the streaming hash, one reads the chunks.
        The hash is pure Python, so a large sample takes a while: the workbench is the tool for those."""
        h = FileHasher()
        size = 0
        with src.open("rb") as f:
            while block := f.read(1 << 20):
                h.update(block)
                size += len(block)

            def read_at(off: int, n: int) -> bytes:
                f.seek(off)
                return f.read(n)

            self._put(path, size, read_at, h.digest(), progress)

    def _put(
        self,
        path: str,
        size: int,
        read_at: Callable[[int, int], bytes],
        hash: bytes,
        progress: Callable[[int, int], None] | None,
    ) -> None:
        self.begin(path, size, hash)
        inflight: list[int] = []
        sent = 0
        offsets = list(range(0, size, self.chunk))
        done = 0
        while done < len(offsets):
            while sent < len(offsets) and len(inflight) < self.depth:
                off = offsets[sent]
                body = struct.pack(">I", off) + read_at(off, self.chunk)
                inflight.append(self.post(CMD_DATA, body))
                sent += 1
            seq = inflight.pop(0)
            status, rb = self.wait(CMD_DATA, seq)
            if status != 0:
                raise UploadError(status, f"data at {offsets[done]}", rb)
            done += 1
            if progress:
                progress(min(size, offsets[done - 1] + self.chunk), size)
        self.end()


def capacity_lines(cap: dict[str, dict[str, int]]) -> list[str]:
    """The `info` command's capacity lines: `KIND used/limit` (and the largest free piece where there is one)."""
    out = []
    for k, v in cap.items():
        tail = f"  largest free {v['largest']}" if v["largest"] else ""
        unit = (
            " KiB" if k == "STORE" else ""
        )  # the store is counted in KiB: it can be 22 GiB
        out.append(f"capacity {k.lower()}: {v['used']}/{v['limit']}{unit}{tail}")
    return out


def modules_lines(m: dict[str, Any]) -> list[str]:
    """The `modules` subcommand's output: a header with the free space, then one line per loaded module."""
    out = [
        f"arena free {m['arena_free']} (largest {m['arena_largest']}), dsp area free {m['dsp_free']} (largest {m['dsp_largest']}), {m['total']} module(s)"
    ]
    for x in m["modules"]:
        cts = " ".join(f"{a}.{b}" for a, b in x["contracts"])
        v = x["version"]
        out.append(
            f"{x['index']}  {x['name']}  v{v >> 16}.{v >> 8 & 255}.{v & 255}  contracts {cts}  "
            f"cf {x['cf_base']:#x}+{x['cf_size']:#x}  dsp {x['dsp_off']:#x}+{x['dsp_len']:#x}  rows {x['rows']:#x}  "
            f"types {','.join(map(str, x['types'])) or '-'}  faults {x['faults']}{'  GUARD OFF' if x['guard_off'] else ''}"
        )
    return out


def read_range(
    up: Uploader, path: str, offset: int = 0, length: int | None = None
) -> bytes:
    """`length` bytes of a store file from `offset` (None: to the end), in READ requests of at most 768 bytes."""
    out = bytearray()
    while length is None or len(out) < length:
        want = READ_MAX if length is None else min(READ_MAX, length - len(out))
        try:
            size, got = up.read(path, offset + len(out), want)
        except UploadError as e:
            if e.status == E_OFFSET and out:
                break  # the range ran past the end of the file
            raise
        out += got
        if offset + len(out) >= size:
            break
    return bytes(out)


def hex_lines(data: bytes, base: int = 0) -> list[str]:
    """`OFFSET  16 hex bytes  text` lines, like hexdump -C."""
    out = []
    for i in range(0, len(data), 16):
        row = data[i : i + 16]
        txt = "".join(chr(c) if 32 <= c < 127 else "." for c in row)
        out.append(f"{base + i:08x}  {row.hex(' '):<47}  {txt}")
    return out


def _emu(run_dir: Path | None = None) -> Any:
    from tools.control.digi_client import Emu

    return Emu.connect(run_dir=run_dir)


def _chunk(s: str) -> int:
    n = int(s)
    if n <= 0 or n > MAX_CHUNK or n % 512:
        raise argparse.ArgumentTypeError(
            f"{n} is not a multiple of 512 up to {MAX_CHUNK}"
        )
    return n


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    where = ap.add_mutually_exclusive_group(required=True)
    where.add_argument(
        "--emu",
        action="store_true",
        help="the running emulator (the desktop app or `mise run emu:serve`)",
    )
    where.add_argument(
        "--port", help="a MIDI port name substring (a real device; python-rtmidi)"
    )
    where.add_argument(
        "--winmm",
        metavar="NAME",
        help="a MIDI port name substring (a real device on Windows; winmm, no package)",
    )
    ap.add_argument(
        "--run-dir",
        type=Path,
        help="with --emu: the run directory of a daemon (`mise run patch:dev` (pk_dev) keeps a fixed out/runs/patchkit/run-dev-<device>; other harness runs use temporary run dirs, printed when kept after a failure), not the one in out/native/run/current.json",
    )
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("info")
    p = sub.add_parser("ls", help="the store's entries (only those under PREFIX)")
    p.add_argument("prefix", nargs="?")
    p = sub.add_parser("mkdir", help="make a folder in the store")
    p.add_argument("path")
    p = sub.add_parser("mv", help="rename or move a file or an empty folder in the store")
    p.add_argument("src")
    p.add_argument("dst")
    sub.add_parser("modules")
    p = sub.add_parser("put")
    p.add_argument("file", type=Path)
    p.add_argument("path", nargs="?")
    p.add_argument(
        "--chunk",
        type=_chunk,
        default=MAX_CHUNK,
        help=f"bytes per DATA request, a multiple of 512 up to {MAX_CHUNK} (default)",
    )
    p.add_argument(
        "--depth",
        type=int,
        choices=(1, 2),
        default=DEPTH,
        help=f"DATA requests in flight (default {DEPTH})",
    )
    p.add_argument(
        "--timing",
        type=Path,
        metavar="FILE",
        help="write each request and reply (monotonic time, cmd, seq, length) here as JSON lines",
    )
    p = sub.add_parser("rm")
    p.add_argument("path")
    sub.add_parser("clear")
    p = sub.add_parser(
        "settings",
        help="read the settings record (digikit's own settings: fx routes and send amounts); read only",
    )
    p.add_argument(
        "-o",
        "--output",
        type=Path,
        help="save the record's bytes here (for `patchkit store dump --record FILE`)",
    )
    p.add_argument("--json", action="store_true")
    p = sub.add_parser(
        "read", help="print or save a store file (or a byte range of it)"
    )
    p.add_argument("path")
    p.add_argument("--offset", type=int, default=0)
    p.add_argument("--length", type=int, help="default: to the end of the file")
    p.add_argument(
        "-o", "--output", type=Path, help="write the bytes here (default: a hex dump)"
    )
    p = sub.add_parser(
        "remote", help="digikit/remote: state, key, turn, release (needs the module)"
    )
    rsub = p.add_subparsers(dest="rcmd", required=True)
    rsub.add_parser("state")
    rsub.add_parser("release")
    q = rsub.add_parser("key", help="press and release a key")
    q.add_argument("code", type=int)
    q.add_argument("--hold", type=float, default=0.1, help="seconds down (default 0.1)")
    q = rsub.add_parser("turn", help="turn an encoder")
    q.add_argument("encoder", type=int)
    q.add_argument("delta", type=int)
    args = ap.parse_args(argv)
    if args.emu:
        t: Any = EmuTransport(_emu(args.run_dir))
    elif args.winmm:
        from tools.device.winmidi import WinMidiTransport

        t = WinMidiTransport(args.winmm)
    else:
        t = RtMidiTransport(args.port)
    timing = None
    if args.cmd == "put":
        from tools.device import dktiming

        timing = open(args.timing, "w", encoding="utf-8") if args.timing else None  # noqa: SIM115
        t = dktiming.TimedTransport(
            t, sink=dktiming.jsonl_sink(timing) if timing else None
        )
    up = Uploader(t, args.chunk, args.depth) if args.cmd == "put" else Uploader(t)
    try:
        if args.cmd == "info":
            info = up.info()
            for line in info_lines(info):
                print(line)
            if has_cap(info, "CAPACITY"):
                for line in capacity_lines(up.capacity()):
                    print(line)
            else:
                print("capacity: not reported by this runtime (needs 0.2.2)")
        elif args.cmd == "ls":
            for path, size, sha in up.ls(args.prefix):
                print(f"{path}\t{size}\t{sha}")
        elif args.cmd == "mkdir":
            up.mkdir(args.path)
        elif args.cmd == "mv":
            up.rename(args.src, args.dst)
        elif args.cmd == "modules":
            for line in modules_lines(up.modules()):
                print(line)
        elif args.cmd == "put":
            path = args.path or f"modules/{args.file.name}"
            t0 = time.monotonic()
            up.put_file(path, args.file)
            size = args.file.stat().st_size
            print(
                f"{path}: {size} bytes in {time.monotonic() - t0:.1f} s (hash checked on the device)"
            )
            print(dktiming.summary_line(dktiming.summary(t.events, size)))
        elif args.cmd == "read":
            data = read_range(up, args.path, args.offset, args.length)
            if args.output:
                args.output.write_bytes(data)
                print(f"{args.path}: {len(data)} bytes to {args.output}")
            else:
                for line in hex_lines(data, args.offset):
                    print(line)
        elif args.cmd == "settings":
            from tools.device import dksettings

            flags, copy, seq, raw = up.settings_record()
            rec = dksettings.decode(raw)
            if args.output:
                args.output.write_bytes(raw)
                print(f"settings record: {len(raw)} bytes to {args.output}")
            if rec is None:
                print(
                    f"error: the record is not valid ({dksettings.why(raw)})",
                    file=sys.stderr,
                )
                return 1
            if not args.output:
                where = "none" if copy == 2 else "AB"[copy]
                state = "; changes not on the card yet" if flags & 1 else ""
                print(f"card copy {where}, seq {seq}{state}")
                print(
                    json.dumps(dksettings.as_json(rec), indent=2)
                    if args.json
                    else "\n".join(dksettings.lines(rec))
                )
        elif args.cmd == "rm":
            up.rm(args.path)
        elif args.cmd == "clear":
            up.clear()
        elif args.cmd == "remote":
            if args.rcmd == "state":
                print(up.remote_state())
            elif args.rcmd == "key":
                up.remote_key(args.code, True)
                time.sleep(args.hold)
                up.remote_key(args.code, False)
            elif args.rcmd == "turn":
                up.remote_turn(args.encoder, args.delta)
            else:
                up.remote_release()
    except UploadError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    finally:
        if timing:
            timing.close()
        close = getattr(t, "close", None)
        if close:
            close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
