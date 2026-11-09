"""Timing of the upload protocol's requests and replies (tools/device/dkupload.py `put --timing`).

`TimedTransport` wraps any dkupload transport (`send(bytes)`, `recv() -> bytes`) and records one event per request and per reply, with
the monotonic high-resolution clock (time.perf_counter: time.monotonic ticks in 15.6 ms on some Windows Pythons). `summary` reduces the events of one `put` to the figures that tell host time from device time:

    bytes, wall seconds (BEGIN to the END reply), DATA seconds (first DATA sent to the last DATA reply), KB/s (1 KB = 1000 bytes) over the
    DATA phase and over the wall time, and per DATA request the mean and p95 of send time (the transport took the message) and of
    send-to-reply (the reply arrived), and the END reply time (the device checks the file's hash there).

With two requests in flight a reply's send-to-reply includes the time the request waited behind the one before it.
"""

from __future__ import annotations

import json
import math
import time
from collections.abc import Callable
from typing import IO, Any

from tools.device import dkproto
from tools.device.dkupload import parse_reply

CMD_NAMES = {
    v: k[4:]
    for k, v in vars(dkproto).items()
    if k.startswith("CMD_") and isinstance(v, int)
}


class TimedTransport:
    """A transport that records each message it passes. An event is a dict: `dir` ("tx" or "rx"), `t` (seconds since the first event),
    `cmd` (name), `seq`, `len` (wire bytes); a tx event has `t_done` (when `send` returned), an rx event has `status`. When the inner
    transport stamps its replies on arrival (`last_rx_t`, a `time.perf_counter` value) that time is used, not the time `recv` returned.
    When it counts emulated instructions (`stepped`, the emulator transport) each event carries `stepped`."""

    def __init__(
        self,
        inner: Any,
        clock: Callable[[], float] = time.perf_counter,
        sink: Callable[[dict[str, Any]], None] | None = None,
    ):
        self.t, self.clock, self.sink = inner, clock, sink
        self.events: list[dict[str, Any]] = []
        self.t0: float | None = None

    def _rel(self, t: float) -> float:
        if self.t0 is None:
            self.t0 = t
        return t - self.t0

    def _add(self, ev: dict[str, Any]) -> None:
        stepped = getattr(self.t, "stepped", None)
        if isinstance(stepped, int):
            ev["stepped"] = stepped
        self.events.append(ev)
        if self.sink:
            self.sink(ev)

    def send(self, msg: bytes) -> None:
        t = self.clock()
        self.t.send(msg)
        done = self.clock()
        cmd = msg[5] & 0x3F if len(msg) > 6 else -1
        self._add(
            {
                "dir": "tx",
                "t": self._rel(t),
                "t_done": self._rel(done),
                "cmd": CMD_NAMES.get(cmd, cmd),
                "seq": msg[6] if len(msg) > 6 else -1,
                "len": len(msg),
            }
        )

    def recv(self, *args: Any, **kwargs: Any) -> bytes:
        msg = self.t.recv(*args, **kwargs)
        stamp = getattr(self.t, "last_rx_t", None)
        t = stamp if isinstance(stamp, float) else self.clock()
        r = parse_reply(msg)
        cmd, seq, status = (r[0], r[1], r[2]) if r else (-1, -1, None)
        self._add(
            {
                "dir": "rx",
                "t": self._rel(t),
                "cmd": CMD_NAMES.get(cmd, cmd),
                "seq": seq,
                "len": len(msg),
                "status": status,
            }
        )
        return msg

    def close(self) -> None:
        close = getattr(self.t, "close", None)
        if close:
            close()


def jsonl_sink(f: IO[str]) -> Callable[[dict[str, Any]], None]:
    """A sink that writes each event as one JSON line and flushes it (a run that stops part way keeps its events)."""

    def write(ev: dict[str, Any]) -> None:
        f.write(json.dumps(ev) + "\n")
        f.flush()

    return write


def p95(xs: list[float]) -> float:
    """The 95th percentile by nearest rank (0 for no values)."""
    if not xs:
        return 0.0
    s = sorted(xs)
    return s[max(0, math.ceil(0.95 * len(s)) - 1)]


def _pairs(
    events: list[dict[str, Any]], cmd: str
) -> list[tuple[dict[str, Any], dict[str, Any]]]:
    """(request, reply) of each `cmd` request that got its reply: a reply matches the oldest open request with its seq."""
    open_: dict[int, list[dict[str, Any]]] = {}
    out = []
    for ev in events:
        if ev["cmd"] != cmd:
            continue
        if ev["dir"] == "tx":
            open_.setdefault(ev["seq"], []).append(ev)
        elif open_.get(ev["seq"]):
            out.append((open_[ev["seq"]].pop(0), ev))
    return out


def summary(events: list[dict[str, Any]], nbytes: int) -> dict[str, Any]:
    """The figures of one put (see the module text). Times are seconds; KB/s uses 1 KB = 1000 bytes."""
    data = _pairs(events, "DATA")
    rtt = [b["t"] - a["t"] for a, b in data]
    send = [a["t_done"] - a["t"] for a, _ in data]
    tx = [e for e in events if e["dir"] == "tx"]
    rx = [e for e in events if e["dir"] == "rx"]
    wall = (rx[-1]["t"] - tx[0]["t"]) if tx and rx else 0.0
    data_s = (data[-1][1]["t"] - min(a["t"] for a, _ in data)) if data else 0.0
    end = _pairs(events, "END")
    out: dict[str, Any] = {
        "bytes": nbytes,
        "chunks": len(data),
        "wall_s": wall,
        "data_s": data_s,
        "kbps": nbytes / data_s / 1000 if data_s > 0 else 0.0,
        "wall_kbps": nbytes / wall / 1000 if wall > 0 else 0.0,
        "send_mean_ms": 1000 * sum(send) / len(send) if send else 0.0,
        "send_p95_ms": 1000 * p95(send),
        "rtt_mean_ms": 1000 * sum(rtt) / len(rtt) if rtt else 0.0,
        "rtt_p95_ms": 1000 * p95(rtt),
        "end_s": (end[-1][1]["t"] - end[-1][0]["t"]) if end else 0.0,
    }
    if data and "stepped" in data[0][0] and "stepped" in data[-1][1]:
        out["data_stepped"] = data[-1][1]["stepped"] - data[0][0]["stepped"]
    return out


def summary_line(s: dict[str, Any]) -> str:
    """One line for the terminal."""
    line = (
        f"{s['bytes']} bytes, {s['chunks']} DATA, wall {s['wall_s']:.2f} s ({s['wall_kbps']:.1f} KB/s), "
        f"DATA {s['data_s']:.2f} s ({s['kbps']:.1f} KB/s), send mean {s['send_mean_ms']:.1f} ms p95 {s['send_p95_ms']:.1f} ms, "
        f"send-to-reply mean {s['rtt_mean_ms']:.1f} ms p95 {s['rtt_p95_ms']:.1f} ms, END {s['end_s']:.2f} s"
    )
    if "data_stepped" in s:
        line += f", DATA stepped {s['data_stepped']} instructions"
    return line
