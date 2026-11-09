"""A dkupload transport over the Windows MIDI API (winmm through ctypes: no package to install).

    uv run python -m tools.device.dkupload --winmm "Digitone II" info
    uv run python -m tools.device.dkupload --winmm "Digitone II" put sample.wav samples/sample.wav --chunk 8192 --depth 1 --timing put.jsonl

The port pair is the one whose names contain the given text (case ignored); none or more than one is an error that lists the ports.
Windows lets one program at a time open a MIDI port: quit Elektron Transfer and any browser page that holds the port first.

Both directions use event callbacks. A reader thread waits on the input event and stamps each complete SysEx message when the thread
wakes after its last buffer comes back (`last_rx_t`, time.perf_counter). That is the thread's time, a little after the driver's: winmm
gives a long message a time stamp only in a function callback, which this module does not use. `send` returns when the driver hands
back the message's buffer. The winmm calls follow the winmidi module of the dn2_firmware project (derived from irpina/digihealth, GPL-2.0-or-later).
"""

from __future__ import annotations

import collections
import ctypes
import queue
import threading
import time
from typing import Any

from tools.device.dkupload import parse_reply, split_messages

MHDR_DONE = 0x1
CALLBACK_EVENT = 0x50000
WAIT_MS = 50


def _check(r: int, call: str) -> None:
    """Raise OSError when a winmm call returns an error code."""
    if r:
        raise OSError(f"{call} failed: {r}")


def _api() -> tuple[Any, Any, Any, Any, Any]:
    """(winmm, kernel32, MIDIHDR, MIDIINCAPSW, MIDIOUTCAPSW), loaded on first use so this module imports on any system."""
    from ctypes import wintypes

    class MIDIHDR(ctypes.Structure):
        pass

    MIDIHDR._fields_ = [
        ("lpData", ctypes.c_void_p),
        ("dwBufferLength", wintypes.DWORD),
        ("dwBytesRecorded", wintypes.DWORD),
        ("dwUser", ctypes.c_void_p),
        ("dwFlags", wintypes.DWORD),
        ("lpNext", ctypes.POINTER(MIDIHDR)),
        ("reserved", ctypes.c_void_p),
        ("dwOffset", wintypes.DWORD),
        ("dwReserved", ctypes.c_void_p * 8),
    ]

    class InCaps(ctypes.Structure):
        _fields_ = [
            ("wMid", wintypes.WORD),
            ("wPid", wintypes.WORD),
            ("vDriverVersion", wintypes.UINT),
            ("szPname", wintypes.WCHAR * 32),
            ("dwSupport", wintypes.DWORD),
        ]

    class OutCaps(ctypes.Structure):
        _fields_ = [
            ("wMid", wintypes.WORD),
            ("wPid", wintypes.WORD),
            ("vDriverVersion", wintypes.UINT),
            ("szPname", wintypes.WCHAR * 32),
            ("wTechnology", wintypes.WORD),
            ("wVoices", wintypes.WORD),
            ("wNotes", wintypes.WORD),
            ("wChannelMask", wintypes.WORD),
            ("dwSupport", wintypes.DWORD),
        ]

    w = ctypes.WinDLL("winmm")  # type: ignore[attr-defined,unused-ignore]
    k = ctypes.WinDLL("kernel32")  # type: ignore[attr-defined,unused-ignore]
    H = wintypes.HANDLE
    P = ctypes.POINTER(MIDIHDR)
    for f in ("midiInOpen", "midiOutOpen"):
        getattr(w, f).argtypes = [
            ctypes.POINTER(H),
            wintypes.UINT,
            ctypes.c_size_t,
            ctypes.c_size_t,
            wintypes.DWORD,
        ]
    for f in ("midiInPrepareHeader", "midiInUnprepareHeader", "midiInAddBuffer"):
        getattr(w, f).argtypes = [H, P, wintypes.UINT]
    for f in ("midiOutPrepareHeader", "midiOutUnprepareHeader", "midiOutLongMsg"):
        getattr(w, f).argtypes = [H, P, wintypes.UINT]
    for f in (
        "midiInStart",
        "midiInStop",
        "midiInReset",
        "midiInClose",
        "midiOutReset",
        "midiOutClose",
    ):
        getattr(w, f).argtypes = [H]
    k.CreateEventW.restype = H
    k.CreateEventW.argtypes = [
        ctypes.c_void_p,
        wintypes.BOOL,
        wintypes.BOOL,
        ctypes.c_wchar_p,
    ]
    k.WaitForSingleObject.argtypes = [H, wintypes.DWORD]
    k.CloseHandle.argtypes = [H]
    return w, k, MIDIHDR, InCaps, OutCaps


def ports() -> tuple[list[str], list[str]]:
    """(input names, output names) in winmm's index order."""
    w, _, _, InCaps, OutCaps = _api()

    def names(count: Any, caps_fn: Any, struct: Any) -> list[str]:
        out = []
        for i in range(count()):
            c = struct()
            caps_fn(i, ctypes.byref(c), ctypes.sizeof(c))
            out.append(c.szPname)
        return out

    return (
        names(w.midiInGetNumDevs, w.midiInGetDevCapsW, InCaps),
        names(w.midiOutGetNumDevs, w.midiOutGetDevCapsW, OutCaps),
    )


def pick(names: list[str], text: str, kind: str) -> int:
    """The index of the one name that contains `text` (case ignored); raises OSError for none or several."""
    hits = [i for i, n in enumerate(names) if text.lower() in n.lower()]
    if len(hits) != 1:
        listed = "; ".join(f"{i} {n}" for i, n in enumerate(names)) or "none"
        raise OSError(
            f"{'no' if not hits else 'more than one'} MIDI {kind} port contains {text!r}. Ports: {listed}"
        )
    return hits[0]


class WinMidiTransport:
    """One MIDI in and one MIDI out port of a real device, through winmm."""

    def __init__(
        self, name: str, timeout_s: float = 30.0, nbuf: int = 16, size: int = 65536
    ):
        from ctypes import wintypes

        self.w, self.k, self.MIDIHDR, _, _ = _api()
        ins, outs = ports()
        i, o = pick(ins, name, "in"), pick(outs, name, "out")
        self.timeout_s = timeout_s
        self.last_rx_t: float | None = None
        self.q: queue.Queue[tuple[float, bytes]] = queue.Queue()
        self.ev_in = self.k.CreateEventW(None, False, False, None)
        self.ev_out = self.k.CreateEventW(None, False, False, None)
        self.hin, self.hout = wintypes.HANDLE(), wintypes.HANDLE()
        r = self.w.midiInOpen(ctypes.byref(self.hin), i, self.ev_in, 0, CALLBACK_EVENT)
        if r:
            raise OSError(
                f"midiInOpen({ins[i]!r}) failed: {r} (does another program hold the port?)"
            )
        r = self.w.midiOutOpen(
            ctypes.byref(self.hout), o, self.ev_out, 0, CALLBACK_EVENT
        )
        if r:
            self.w.midiInClose(self.hin)
            raise OSError(
                f"midiOutOpen({outs[o]!r}) failed: {r} (does another program hold the port?)"
            )
        # winmm fills the input buffers in the order they were added: the deque keeps that order
        self.bufs: collections.deque[tuple[Any, Any]] = collections.deque()
        # buffers the driver did not hand back: kept alive so their memory is never freed under it
        self.parked: list[tuple[Any, Any]] = []
        self.stream = bytearray()
        self.stop = threading.Event()
        self.reader = threading.Thread(
            target=self._read, name="winmidi-in", daemon=True
        )
        try:
            for _ in range(nbuf):
                mem = ctypes.create_string_buffer(size)
                h = self.MIDIHDR(
                    lpData=ctypes.cast(mem, ctypes.c_void_p), dwBufferLength=size
                )
                _check(
                    self.w.midiInPrepareHeader(
                        self.hin, ctypes.byref(h), ctypes.sizeof(h)
                    ),
                    "midiInPrepareHeader",
                )
                self.bufs.append((h, mem))
                _check(
                    self.w.midiInAddBuffer(self.hin, ctypes.byref(h), ctypes.sizeof(h)),
                    "midiInAddBuffer",
                )
            _check(self.w.midiInStart(self.hin), "midiInStart")
        except BaseException:
            self.close()
            raise
        self.reader.start()

    def _read(self) -> None:
        while not self.stop.is_set():
            self.k.WaitForSingleObject(self.ev_in, WAIT_MS)
            while self.bufs and self.bufs[0][0].dwFlags & MHDR_DONE:
                h, mem = self.bufs.popleft()
                t = time.perf_counter()
                self.stream += ctypes.string_at(mem, h.dwBytesRecorded)
                h.dwBytesRecorded = 0
                h.dwFlags &= ~MHDR_DONE
                if not self.stop.is_set():
                    self.w.midiInAddBuffer(self.hin, ctypes.byref(h), ctypes.sizeof(h))
                self.bufs.append((h, mem))
                for m in split_messages(self.stream):
                    self.q.put((t, m))

    def send(self, msg: bytes) -> None:
        mem = ctypes.create_string_buffer(bytes(msg), len(msg))
        h = self.MIDIHDR(
            lpData=ctypes.cast(mem, ctypes.c_void_p), dwBufferLength=len(msg)
        )
        _check(
            self.w.midiOutPrepareHeader(self.hout, ctypes.byref(h), ctypes.sizeof(h)),
            "midiOutPrepareHeader",
        )
        r = self.w.midiOutLongMsg(self.hout, ctypes.byref(h), ctypes.sizeof(h))
        if r:
            self.w.midiOutUnprepareHeader(self.hout, ctypes.byref(h), ctypes.sizeof(h))
            raise OSError(f"midiOutLongMsg failed: {r}")
        end = time.monotonic() + self.timeout_s
        try:
            while not h.dwFlags & MHDR_DONE:
                if time.monotonic() > end:
                    raise TimeoutError("the MIDI driver did not take the message")
                self.k.WaitForSingleObject(self.ev_out, WAIT_MS)
        except BaseException:  # a timeout or a Ctrl-C: the driver still owns the buffer
            self.w.midiOutReset(self.hout)
            self._reclaim(h, mem)
            raise
        self.w.midiOutUnprepareHeader(self.hout, ctypes.byref(h), ctypes.sizeof(h))

    def _reclaim(self, h: Any, mem: Any) -> None:
        """After midiOutReset: unprepare H once the driver marks it done, or park it with MEM if it never does."""
        end = time.monotonic() + 1.0
        while not h.dwFlags & MHDR_DONE and time.monotonic() < end:
            self.k.WaitForSingleObject(self.ev_out, WAIT_MS)
        if h.dwFlags & MHDR_DONE:
            self.w.midiOutUnprepareHeader(self.hout, ctypes.byref(h), ctypes.sizeof(h))
        else:
            self.parked.append((h, mem))

    def recv(self, timeout_s: float | None = None) -> bytes:
        """The next reply of the upload protocol (other messages are dropped); TimeoutError after `timeout_s` (default: the transport's)."""
        end = time.monotonic() + (self.timeout_s if timeout_s is None else timeout_s)
        while True:
            left = end - time.monotonic()
            if left <= 0:
                raise TimeoutError("no reply from the device")
            try:
                t, m = self.q.get(timeout=left)
            except queue.Empty:
                continue
            if parse_reply(m) is not None:
                self.last_rx_t = t
                return m

    def close(self) -> None:
        self.stop.set()
        if self.reader.is_alive():
            self.reader.join(timeout=1.0)
        self.w.midiInStop(self.hin)
        self.w.midiInReset(self.hin)
        for h, _mem in self.bufs:
            self.w.midiInUnprepareHeader(self.hin, ctypes.byref(h), ctypes.sizeof(h))
        self.w.midiInClose(self.hin)
        self.w.midiOutReset(self.hout)
        self.w.midiOutClose(self.hout)
        for e in (self.ev_in, self.ev_out):
            self.k.CloseHandle(e)
