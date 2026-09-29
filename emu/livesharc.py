"""The GUI's live DSP: every DSPI2 frame the emulated ColdFire sends is
rendered by the native SHARC core in real time and played on the speakers
(`native/live`, driven over `tools/live_audio.py`).

    uv run python tools/dt2gui.py --live-audio
    uv run python -m emu.gui SNAPSHOT --card-image IMG --live-audio
        [--live-lp0 FLEXBUS.raw] [--live-frame-period N]

Four pieces, all opt-in behind `--live-audio`:

- **Frames out of the ColdFire.** `emu.dspi2.Dspi2Link` runs the real
  DSPI2 driver (`build(..., dspi2_peer=LiveFramePeer)`); the peer queues
  each TX frame, in wire order (the ColdFire's big-endian halfwords, byte
  for byte what `tools/sharc_capture_run.py` records from the driver's own
  buffer), for the native engine. The byte swap into the SHARC's receive
  ring happens once, at the SHARC edge (`native/live` `LiveSource`).
- **The reply.** The peer answers zeros, as every capture's recorded RX
  and `emu.dspi2.ZeroPeer` do: the trig-to-voice path was verified with
  zero replies, and the native frame step does not model the SHARC's SPI
  transmit side. The engine renders asynchronously, so a real reply would
  be at least a frame late anyway.
- **Making frames happen.** Vector 191 does not fire by itself in the GUI
  (no SSI0 model; `tools/sharc_capture_run.py`, "Getting a DSPI2 driver
  call to happen at all"), so `FrameForcer` does what the capture tool
  does: open the frame-build gate once, then every PERIOD instructions
  clear the handler's pacing counter and raise vector 191 at its own
  level, never on top of a handler that has not returned. Both are the
  documented hand steps (finding 15). A frame costs the ColdFire about
  50,000 instructions, so the period trades DSP control rate against UI
  speed; the default gives 10-20 frames per wall second.
- **The SHARC side.** A state pack (`tools/sharc_transpile_run.py
  state-pack`: `armed_start` with the LP0 feed of the FlexBus log the
  sample load produced) is the engine's start state. It is keyed by, and
  records, the card image the log came from; `native/live` refuses a pack
  built for another card. `tools/dt2gui.py` records the log next to its
  snapshots (`flexbus.raw`), which is the default here. Between ColdFire
  frames the engine repeats the last frame with its one-shot words
  (trig/release) cleared; frames that arrive in a burst are rendered in
  order (`native/live/src/repeater.rs`).
"""

from __future__ import annotations

import os
import sys
from dataclasses import dataclass
from typing import Any

from unicorn.m68k_const import UC_M68K_REG_SR

from emu import config
from emu.pit import interrupt_level

_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
_TOOLS = os.path.join(_ROOT, "tools")

# Instructions between forced frames (see the module docstring).
FRAME_PERIOD = 200_000
# The load run's FlexBus log next to a tools/dt2gui.py snapshot.
FLEXBUS_LOG = "flexbus.raw"
# The only image with a frame-link profile and a native SHARC build so far.
PROFILE_NAME = "Digitakt II 1.16"
SHARC_IMAGE = "dt2-1.16"
TRIG_MASK = 0x22


def _tools_path() -> None:
    if _TOOLS not in sys.path:
        sys.path.insert(0, _TOOLS)


def frame_profile(image: str | None = None) -> dict:
    """tools/framelink.py's profile for the MAIN OS image (vector, handler,
    driver, pacing counter, frame-build gate); SystemExit for an image the
    live DSP does not support."""
    _tools_path()
    import framelink

    _sha, prof = framelink.profile_for(image or config.main_image())
    if prof["name"] != PROFILE_NAME:
        raise SystemExit(
            "--live-audio supports %s only (this image: %s)"
            % (PROFILE_NAME, prof["name"])
        )
    return prof


class LiveFramePeer:
    """`emu.dspi2` peer: queue every TX frame for the live engine, reply
    zeros (see the module docstring)."""

    def __init__(self, audio: Any) -> None:
        self.audio = audio
        self.frames = 0
        self.trig_frames = 0

    def exchange(self, tx: bytes) -> bytes:
        frame = bytes(tx)
        self.audio.push_frame(frame)
        self.frames += 1
        if frame[TRIG_MASK : TRIG_MASK + 2] != b"\0\0":
            self.trig_frames += 1
        return bytes(len(frame))


class FrameForcer:
    """Raise vector 191 every PERIOD instructions of the timers' clock
    (`spin(..., on_chunk=forcer.on_chunk)`), after opening the frame-build
    gate; `tools/sharc_capture_run.py`'s forcing loop and `ready_to_force`
    guard."""

    def __init__(self, m: Any, pits: Any, period: int = FRAME_PERIOD) -> None:
        prof = frame_profile()
        self.m = m
        self.pits = pits
        self.period = max(1, int(period))
        self.vector = prof["vector"]
        self.counter = prof["counter"]
        self.level = interrupt_level(m, self.vector, respect_mask=False)
        m.uc.mem_write(prof["gate"], bytes(4))  # open the frame-build gate
        self.due = pits.now
        self.forced = 0
        self.deferred = 0

    def on_chunk(self, pc: int, done: int) -> None:
        now = self.pits.now
        if now < self.due:
            return
        if self.level is not None:
            sr = self.m.uc.reg_read(UC_M68K_REG_SR)
            if ((sr >> 8) & 0x07) >= self.level:
                # Still inside a handler at this level: retry next boundary.
                self.deferred += 1
                return
        self.m.uc.mem_write(self.counter, bytes(4))
        self.m.raise_vector(self.vector, level=self.level)
        self.forced += 1
        self.due = now + self.period


@dataclass(frozen=True)
class LiveConfig:
    """What the GUI's worker needs to open the live engine (see prepare)."""

    pack: str
    card_sha256: str
    period: int = FRAME_PERIOD
    device: bool = True
    gain: float = 1.0

    def open(self) -> Any:
        """-> an open tools/live_audio.py LiveAudio on the native SHARC core."""
        _tools_path()
        from live_audio import LiveAudio

        return LiveAudio(
            frames_pack=self.pack,
            card_sha256=self.card_sha256,
            device=self.device,
            gain=self.gain,
        )


def default_lp0(snapshot: str) -> str:
    return os.path.join(os.path.dirname(snapshot) or ".", FLEXBUS_LOG)


def prepare(
    snapshot: str,
    card_image: str | None,
    card_sha256: str | None = None,
    *,
    lp0: str | None = None,
    period: int = FRAME_PERIOD,
    device: bool = True,
) -> LiveConfig:
    """Check the inputs and build (or find) the state pack. LP0 defaults to
    the FlexBus log next to SNAPSHOT (tools/dt2gui.py records it there);
    CARD_SHA256 defaults to the card image's own hash. SystemExit with the
    fix when something is missing."""
    frame_profile()
    if not card_image:
        raise SystemExit(
            "--live-audio needs --card-image: the DSP's samples come from it"
        )
    lp0 = lp0 or default_lp0(snapshot)
    if not os.path.exists(lp0):
        raise SystemExit(
            "--live-audio: no FlexBus log at %s. It is the sample load's "
            "log for this card image; tools/dt2gui.py --live-audio records "
            "it (rebuilding the ready snapshot once), or pass --live-lp0 "
            "PATH (a .raw log recorded from the same card image)." % lp0
        )
    if not card_sha256:
        from emu.checkpoint import sha256_file

        card_sha256 = sha256_file(card_image)
    _tools_path()
    from live_audio import state_pack

    print(
        "[gui] live audio: state pack for %s (built once, about 1.5 min) ..." % lp0,
        flush=True,
    )
    pack = state_pack(lp0, card_sha256, image=SHARC_IMAGE)
    print("[gui] live audio: %s" % pack, flush=True)
    return LiveConfig(pack, card_sha256, period=period, device=device)


def status(audio: Any, peer: LiveFramePeer, forcer: FrameForcer | None) -> str:
    """One status line (cheap calls only: no render-time percentiles, which
    sort the whole log under the producer's lock)."""
    f = audio.frame_stats()
    s = audio.stats()
    return (
        "DSP  %d frames in (%d trig), %d rendered + %d repeats, queue max %d, "
        "merged %d, underruns %d%s"
        % (
            f.pushed,
            f.trig_pushed,
            f.taken,
            f.repeats,
            f.max_depth,
            f.merged,
            s.underruns,
            ", %d forced/%d deferred" % (forcer.forced, forcer.deferred)
            if forcer is not None
            else "",
        )
    )


def summary(audio: Any) -> str:
    """The close-time report: queue and render statistics."""
    f = audio.frame_stats()
    r = audio.render_stats()
    s = audio.stats()
    return (
        "[gui] live audio: %d frames in (%d trig), %d SHARC frames rendered "
        "(%d clean, %d stopped, %d with sound), median %.0f us, p99 %.0f us, "
        "underruns %d"
        % (
            f.pushed,
            f.trig_pushed,
            r.frames,
            r.clean,
            r.stopped,
            r.nonzero_frames,
            r.median_us,
            r.p99_us,
            s.underruns,
        )
    )
