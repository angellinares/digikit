"""Headless check of the GUI's live DSP (emu/livesharc.py, `emu.gui
--live-audio`): run emu.gui's own worker thread, with no window and no
output device, from a snapshot; press a TRIG pad; pull the native SHARC
engine's output at the real-time rate (1,500 frames/s, what the device
would take); report what the engine received and rendered.

    DT2_SYX=Digitakt_II_OS1.16.syx uv run python tools/live_gui_check.py \\
        SNAPSHOT --card-image IMG [--lp0 FLEXBUS.raw] [--limit 12M] \\
        [--press-at 3M] [--hold 2M] [--trig 1] [--tail 1.0] \\
        [--frame-period N] [--wav OUT.wav] [--json OUT.json]

Passes (exit 0) when a frame with the pad's trig bit reached the engine,
the engine rendered it, every SHARC frame ended cleanly, and a voice
sounded after the press. Needs native/live and the native SHARC core built
(tools/live_audio.py) and the state pack (built on first use, about 1.5 min).
"""

from __future__ import annotations

import argparse
import array
import dataclasses
import hashlib
import json
import os
import struct
import sys
import time
import wave
from pathlib import Path

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)

# SHARC frames per second: 48 kHz / 32 samples.
FRAMES_PER_SEC = 1500
FRAME_SAMPLES = 32
# emu/panelin.py button codes: "TRIG 1".."TRIG 16" are 25..40
# (tools/sharc_capture_run.py, TRIG_CODE_BASE).
TRIG_1_CODE = 25
MAX_WIRE_FRAMES = 256
MAX_WIRE_BYTES = 4096


class _RecordingAudio:
    """Observe exactly the bytes accepted by the GUI's native frame queue."""

    def __init__(self, audio, frames: list[bytes]):
        self.audio = audio
        self.frames = frames

    def push_frame(self, frame: bytes) -> None:
        if len(self.frames) >= MAX_WIRE_FRAMES or len(frame) > MAX_WIRE_BYTES:
            raise ValueError("bounded live GUI frame trace exceeded")
        self.audio.push_frame(frame)
        self.frames.append(bytes(frame))

    def __getattr__(self, name: str):
        return getattr(self.audio, name)


class _RecordingLive:
    def __init__(self, live, frames: list[bytes]):
        self.live = live
        self.frames = frames

    def open(self):
        return _RecordingAudio(self.live.open(), self.frames)

    def __getattr__(self, name: str):
        return getattr(self.live, name)


def _write_frames(path: str, frames: list[bytes]) -> dict:
    out = (Path(ROOT) / "out").resolve()
    target = Path(path).resolve()
    if not target.is_relative_to(out):
        raise ValueError("frame trace must stay under ignored out/")
    if not frames or len(frames) > MAX_WIRE_FRAMES:
        raise ValueError("no bounded live GUI frames to save")
    blob = bytearray(b"DTFR") + struct.pack("<II", 1, len(frames))
    trig = []
    for index, frame in enumerate(frames):
        if len(frame) > MAX_WIRE_BYTES:
            raise ValueError("frame trace contains an oversized TX frame")
        blob += struct.pack("<I", len(frame)) + frame
        if frame[0x22:0x24] != b"\0\0":
            trig.append(index)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(blob)
    return {
        "path": str(target),
        "frames": len(frames),
        "sha256": hashlib.sha256(blob).hexdigest(),
        "trig_indices": trig,
        "trig_sha256": [hashlib.sha256(frames[i]).hexdigest() for i in trig],
    }


def _count(text: str) -> int:
    text = str(text)
    if text[-1:] in ("M", "m"):
        return int(float(text[:-1]) * 1_000_000)
    return int(text, 0)


def write_wav(path: str, samples: array.array, rate: int = 48_000) -> None:
    """Interleaved stereo float samples -> 16-bit PCM WAV."""
    pcm = array.array("h", (max(-32768, min(32767, round(x * 32767))) for x in samples))
    if sys.byteorder != "little":
        pcm.byteswap()
    with wave.open(path, "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(pcm.tobytes())


def _pause_at_limit(emu, wait_s: float = 5.0) -> int:
    """Pause at the worker's next chunk boundary before starting audio tail."""
    emu.pause.set()
    deadline = time.monotonic() + wait_s
    while emu.is_alive():
        if emu.stats["status"] == "paused":
            return emu.stats["instrs"]
        if time.monotonic() >= deadline:
            raise TimeoutError("live GUI worker did not acknowledge instruction limit")
        time.sleep(0.005)
    raise RuntimeError("live GUI worker exited before acknowledging instruction limit")


def run(
    snapshot: str,
    card_image: str,
    *,
    syx: str | None = None,
    lp0: str | None = None,
    instrs: int = 12_000_000,
    press_at: int = 3_000_000,
    hold: int = 2_000_000,
    trig: int = 1,
    tail: float = 1.0,
    frame_period: int | None = None,
    wav: str | None = None,
    frames_out: str | None = None,
) -> dict:
    """See the module docstring. -> the report (its "ok" says pass/fail)."""
    from emu import gui, livesharc

    card_sha = gui.check_card_image_sidecar(snapshot, card_image)
    live = livesharc.prepare(
        snapshot,
        card_image,
        card_sha,
        lp0=lp0,
        period=frame_period or livesharc.FRAME_PERIOD,
        device=False,
    )
    wire_frames: list[bytes] = []
    if frames_out is not None:
        live = _RecordingLive(live, wire_frames)
    emu = gui.Emulator(snapshot, syx=syx, card_image=card_image, live=live)
    code = TRIG_1_CODE + trig - 1
    emu.start()
    if not emu.ready.wait(300) or emu.error or emu.live_audio is None:
        emu.stop_flag.set()
        emu.join(timeout=10)
        raise SystemExit("emulator failed to start: %s" % emu.error)
    audio = emu.live_audio
    samples = array.array("f")
    rendered = 0
    events: dict[str, dict] = {}
    t0 = time.time()
    stop_at = None

    def pull() -> None:
        nonlocal rendered
        due = int((time.time() - t0) * FRAMES_PER_SEC) - rendered
        if due > 0:
            samples.extend(audio.render(due))
            rendered += due

    try:
        while emu.is_alive():
            n = emu.stats["instrs"]
            if "press" not in events and n >= press_at:
                emu.inbox.append(("press", code, None))
                events["press"] = {"instrs": n, "sharc_frame": rendered}
            if "press" in events and "release" not in events and n >= press_at + hold:
                emu.inbox.append(("release", code, None))
                events["release"] = {"instrs": n, "sharc_frame": rendered}
            if stop_at is None and n >= instrs:
                # Keep the native renderer open for the audio tail, but stop
                # advancing the ColdFire beyond the requested threshold.
                # Wait for emu.gui to finish any in-progress chunk before
                # starting the audio tail or sampling queue counters.
                _pause_at_limit(emu)
                stop_at = time.time() + tail
            if stop_at is not None and time.time() >= stop_at:
                break
            pull()
            time.sleep(0.005)
        pull()
        frames = audio.frame_stats()
        render = audio.render_stats()
        stats = audio.stats()
        peer, forcer = emu.live_peer, emu.live_forcer
        report = {
            "snapshot": snapshot,
            "pack": live.pack,
            "instrs": emu.stats["instrs"],
            "wall_s": round(time.time() - t0, 1),
            "events": events,
            "peer": {
                "frames": peer.frames if peer else 0,
                "trig_frames": peer.trig_frames if peer else 0,
            },
            "forcer": {
                "forced": forcer.forced if forcer else 0,
                "deferred": forcer.deferred if forcer else 0,
                "period": live.period,
            },
            "queue": dataclasses.asdict(frames),
            "render": dataclasses.asdict(render),
            "first_stop": audio.first_stop(),
            "underruns": stats.underruns,
            "halted": emu.stats["status"],
        }
    finally:
        emu.stop_flag.set()
        emu.pause.clear()
        emu.join(timeout=30)
    if frames_out is not None:
        report["wire"] = _write_frames(frames_out, wire_frames)
        if report["wire"]["frames"] != report["queue"]["pushed"]:
            raise ValueError("recorded wire frames differ from queued frame count")
    press_frame = events.get("press", {}).get("sharc_frame")
    first = report["render"]["first_nonzero"]
    report["latency_s"] = (
        round((first - press_frame) / FRAMES_PER_SEC, 3)
        if press_frame is not None and first >= press_frame
        else None
    )
    peak = max((abs(x) for x in samples), default=0.0)
    report["peak"] = round(peak, 5)
    report["ok"] = bool(
        report["queue"]["trig_pushed"] >= 1
        and report["instrs"] >= instrs
        and report["queue"]["trig_taken"] >= 1
        and report["render"]["stopped"] == 0
        and report["render"]["nonzero_frames"] > 0
        and report["latency_s"] is not None
    )
    if wav:
        write_wav(wav, samples)
        report["wav"] = wav
    return report


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("snapshot")
    p.add_argument("--card-image", required=True)
    p.add_argument("--syx")
    p.add_argument("--lp0", help="the sample load's FlexBus log (.raw)")
    p.add_argument(
        "--limit", "--instrs", dest="instrs", type=_count, default=12_000_000
    )
    p.add_argument("--press-at", type=_count, default=3_000_000)
    p.add_argument("--hold", type=_count, default=2_000_000)
    p.add_argument("--trig", type=int, default=1, help="TRIG pad 1-16")
    p.add_argument("--tail", type=float, default=1.0, help="seconds after --instrs")
    p.add_argument("--frame-period", type=_count)
    p.add_argument("--wav", help="write the rendered output here")
    p.add_argument(
        "--frames-out", help="write bounded accepted TX frames under ignored out/"
    )
    p.add_argument("--json", help="write the report here")
    a = p.parse_args(argv)
    report = run(
        a.snapshot,
        a.card_image,
        syx=a.syx,
        lp0=a.lp0,
        instrs=a.instrs,
        press_at=a.press_at,
        hold=a.hold,
        trig=a.trig,
        tail=a.tail,
        frame_period=a.frame_period,
        wav=a.wav,
        frames_out=a.frames_out,
    )
    text = json.dumps(report, indent=1)
    if a.json:
        with open(a.json, "w") as fh:
            fh.write(text + "\n")
    print(text)
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
