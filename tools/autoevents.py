#!/usr/bin/env python3
"""Bounded Python-ColdFire Oracle host-event and DSPI2 trace.

Counted mode is a NEW exact-counted reference, not the GUI's fast trace.
Optional fast-observed mode mirrors GUI's approximate instruction count for
diagnosis ONLY; fast stepping is not a deterministic pass/fail clock gate.
Input requests are fixed guest-clock thresholds; delivery occurs at the next
existing outer spin boundary, before the next spin, as in emu.gui. Clock
credit for skipped idle instructions is included in the reported count.
Hashes check local integrity only, not firmware/fixture authentication.
All firmware-derived output must remain under ignored out/.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from unicorn.m68k_const import UC_M68K_REG_PC  # noqa: E402

from emu import gui, panelin  # noqa: E402
from emu.livesharc import FrameForcer  # noqa: E402
from emu.longrun import build, spin  # noqa: E402
from emu.symbols import resolve  # noqa: E402
from tools import framelink, wiretrace  # noqa: E402

MAX_LIMIT = 20_000_000
MAX_EVENTS = 5_000
BUDGET = 400_000


def _sha256(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def feed_spec(text: str) -> tuple[int, bytes]:
    clock, sep, encoded = text.partition(":")
    if not sep:
        raise ValueError("feed must be CLOCK:HEX")
    at = int(clock, 0)
    data = bytes.fromhex(encoded)
    if not 0 <= at < MAX_LIMIT or not 1 <= len(data) <= 64:
        raise ValueError("feed clock/length outside bounded range")
    return at, data


def due_feeds(feeds: list[tuple[int, bytes]], next_feed: int, now: int) -> int:
    """Return exclusive end of requests due at a pre-existing outer boundary."""
    end = next_feed
    while end < len(feeds) and feeds[end][0] <= now:
        end += 1
    return end


def _output(path: Path) -> Path:
    result = path.resolve()
    if not result.parent.is_relative_to((ROOT / "out").resolve()):
        raise ValueError("derived outputs must stay under ignored out/")
    result.parent.mkdir(parents=True, exist_ok=True)
    return result


class FramePeer:
    def __init__(self, events: list[dict]) -> None:
        self.frames: list[bytes] = []
        self.events = events
        self.clock = lambda: 0
        self.forced = lambda: 0

    def exchange(self, tx: bytes) -> bytes:
        frame = bytes(tx)
        if (
            len(self.frames) >= wiretrace.MAX_FRAMES
            or len(frame) > wiretrace.MAX_FRAME_BYTES
        ):
            raise ValueError("bounded wire trace exceeded")
        index = len(self.frames)
        self.frames.append(frame)
        if len(self.events) >= MAX_EVENTS:
            raise ValueError("bounded Oracle event log exceeded")
        self.events.append(
            {
                "kind": "tx",
                "index": index,
                "after_force": self.forced(),
                # exchange occurs during an emu_start: this is the LAST serviced
                # clock, not an exact TX-instruction clock.
                "service_clock_floor": self.clock(),
                "length": len(frame),
                "sha256": hashlib.sha256(frame).hexdigest(),
                "release_window_hex": frame[0x24:0x2B].hex(),
            }
        )
        return bytes(len(frame))


def capture(
    snapshot: Path,
    source: Path,
    sections: Path,
    card: Path,
    state: Path,
    frames_out: Path,
    events_out: Path,
    feeds: list[tuple[int, bytes]],
    limit: int,
    wanted: int,
    stepping: str = "counted",
) -> dict:
    if not 1 <= limit <= MAX_LIMIT or not 1 <= wanted <= wiretrace.MAX_FRAMES:
        raise ValueError("explicit limit and frame count must be bounded")
    if feeds != sorted(feeds, key=lambda item: item[0]):
        raise ValueError("feeds must be in request-clock order")
    if stepping not in ("counted", "fast-observed"):
        raise ValueError("unsupported stepping mode")
    frames_out, events_out = _output(frames_out), _output(events_out)
    if frames_out == events_out:
        raise ValueError("events and frames need separate paths")
    if _sha256(source) != (sections / ".source-sha256").read_text().strip():
        raise ValueError("source differs from sections source marker")
    gui.check_card_image_sidecar(str(snapshot), str(card))
    saved = state.read_bytes()
    if saved[:8] != b"MSTATE\0\x02":
        raise ValueError("ready state is not compact MSTATE v2")
    size = int.from_bytes(saved[8:12], "little")
    if not 0 < size <= 1 << 20:
        raise ValueError("MSTATE header exceeds bound")
    header = json.loads(saved[12 : 12 + size])
    main = sections / "section_3_MAIN_OS.bin"
    main_sha, profile = framelink.profile_for(main)
    if header["manifest"]["main_sha256"] != main_sha:
        raise ValueError("ready state and loaded MAIN OS differ")

    events: list[dict] = []
    peer = FramePeer(events)
    previous = os.environ.get("DT2_SECTIONS")
    os.environ["DT2_SECTIONS"] = str(sections.resolve())
    try:
        m, ev, _st, pc, _inq, _at = build(
            str(snapshot),
            syx=str(source),
            card_image=str(card),
            unblock=True,
            softfloat=True,
            bitmap=True,
            dsp=True,
            deferred_components=("timers",),
            dspi2_peer=peer,
        )
        try:
            pits = ev["restore_checkpoint_timers"]()
            if pits is None or ev["checkpoint_manifest"]["main_sha256"] != main_sha:
                raise ValueError("ready snapshot lacks matching MAIN OS/timers")
            if pc != header["regs"]["pc"]:
                raise ValueError("ready snapshot and MSTATE PC differ")
            forcer = FrameForcer(m, pits, 200_000)
            origin = pits.now
            peer.clock = lambda: pits.now - origin
            peer.forced = lambda: forcer.forced
            symbols = resolve(main.read_bytes())
            done, fed = 0, 0

            def on_chunk(before: int, _chunk_done: int) -> None:
                old_forced, old_deferred = forcer.forced, forcer.deferred
                forcer.on_chunk(before, _chunk_done)
                if forcer.forced == old_forced and forcer.deferred == old_deferred:
                    return
                if len(events) >= MAX_EVENTS:
                    raise ValueError("bounded Oracle event log exceeded")
                events.append(
                    {
                        "kind": "force" if forcer.forced != old_forced else "defer",
                        "clock": pits.now - origin,
                        "pre_pc": before,
                        "post_pc": m.uc.reg_read(UC_M68K_REG_PC),
                        # A fresh Unicorn SR read can overwrite guest flags; do not
                        # change the run just to log an unreliable pre-IRQ SR.
                        "pre_sr": None,
                        "forced": forcer.forced,
                        "counter_zeroed": forcer.forced != old_forced,
                    }
                )

            while done < limit and len(peer.frames) < wanted:
                end = due_feeds(feeds, fed, done)
                for index in range(fed, end):
                    requested, data = feeds[index]
                    before = pc
                    pc = panelin.feed(m, symbols, data)
                    if len(events) >= MAX_EVENTS:
                        raise ValueError("bounded Oracle event log exceeded")
                    events.append(
                        {
                            "kind": "feed",
                            "index": index,
                            "request_clock": requested,
                            "clock": done,
                            "pre_pc": before,
                            "post_pc": pc,
                            "pre_sr": None,
                            "bytes_hex": data.hex(),
                        }
                    )
                fed = end
                pc, executed, stop = spin(
                    m,
                    pc,
                    min(BUDGET, limit - done),
                    pits=pits,
                    fast=stepping == "fast-observed",
                    on_chunk=on_chunk,
                )
                done += executed
                if stop != "limit":
                    raise RuntimeError(
                        f"Oracle stopped at {pc:#x} after {done}: {stop}"
                    )
            if len(peer.frames) < wanted or fed != len(feeds):
                raise RuntimeError(
                    f"bounded Oracle incomplete: {len(peer.frames)}/{wanted} frames, "
                    f"{fed}/{len(feeds)} feeds after {done} credited instructions; "
                    f"forces={forcer.forced}, deferrals={forcer.deferred}, pc={pc:#x}, "
                    f"last_events={events[-3:]}"
                )
        finally:
            m.close()
    finally:
        if previous is None:
            os.environ.pop("DT2_SECTIONS", None)
        else:
            os.environ["DT2_SECTIONS"] = previous

    blob = bytearray(b"DTFR") + struct.pack("<II", 1, len(peer.frames))
    for frame in peer.frames:
        blob += struct.pack("<I", len(frame)) + frame
    frames_out.write_bytes(blob)
    report = {
        "format_version": 1,
        "policy": f"new-{stepping}-oracle-host-schedule",
        "guest_clock": "counted plus credited idle skips, relative to restored timers",
        "sr_note": "not read for logging; extra Unicorn SR reads can perturb flags",
        "tx_clock_note": "service_clock_floor is not exact TX instruction clock",
        "identity": {
            "source_sha256": _sha256(source),
            "snapshot_sha256": _sha256(snapshot),
            "card_sha256": _sha256(card),
            "state_sha256": _sha256(state),
            "main_sha256": main_sha,
            "profile": profile["name"],
        },
        "limit": limit,
        "stepping": stepping,
        "actual_credited_instructions": done,
        "frames": len(peer.frames),
        "dtfr_sha256": hashlib.sha256(blob).hexdigest(),
        "events": events,
    }
    events_out.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    return report


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    for key in (
        "snapshot",
        "source",
        "sections",
        "card",
        "state",
        "frames-out",
        "events-out",
    ):
        p.add_argument(f"--{key}", required=True, type=Path)
    p.add_argument(
        "--feed", action="append", default=[], type=feed_spec, help="CLOCK:HEX"
    )
    p.add_argument("--limit", type=int, required=True)
    p.add_argument("--frames", type=int, required=True)
    p.add_argument(
        "--stepping", choices=("counted", "fast-observed"), default="counted"
    )
    a = p.parse_args()
    result = capture(
        a.snapshot,
        a.source,
        a.sections,
        a.card,
        a.state,
        a.frames_out,
        a.events_out,
        a.feed,
        a.limit,
        a.frames,
        a.stepping,
    )
    print(
        f"{result['frames']} frames, {result['actual_credited_instructions']} "
        f"credited instructions; DTFR SHA-256 {result['dtfr_sha256']}"
    )


if __name__ == "__main__":
    main()
