"""Capture real ColdFire<->SHARC DSPI2 frames (and, best-effort, SSI0 "audio
in" requests) from a running Digitakt II 1.16 snapshot, for
tools/sharc_replay.py.

    uv run python tools/sharc_capture_run.py SNAPSHOT --out CAPTURE.dt2cap \
        --kind idle|note|play --instrs N [--syx SYX] [--ssi0-hz N]
        [--trig-at N] [--force-period N] [--unblock]
        [--poke-track-type TRACK:TYPE ...] [--pre-instrs N]

Three things a run needs, all handled here:

**Getting a DSPI2 driver call to happen at all.** The vector-191 ISR
(docs/findings/04-coldfire-dsp-link.md, "The ColdFire tells the SHARC
through a periodic DSPI2 frame"; addresses from tools/framelink.py) builds
the TX payload and calls the DSPI2 driver (`FUN_400cd2bc`) with it. This
tool hooks that call directly the way tools/sharcframe.py already does
(intercept the stack args via `emu.dspiframe.read_driver_call`, fake the
return) rather than modelling eDMA/DSPI2 registers -- nothing about
capturing needs the transport modelled, only what crossed it. It also opens
the frame-build gate (docs/findings/04, "The frame capture runs; the frame
build is switched off": the long at the profile's `gate` address must be 0
or the handler sends stale/zero bytes) the same way `--open-gate` does.

Getting vector 191 to *fire*, so that call happens, is two independent
mechanisms, both always on:

1. **SSI0 model, for "audio in" only** (best-effort): `--ssi0-hz` wires up
   `emu.ssi.Ssi0Dma` via `emu.longrun.build(ssi0_request_hz=...,
   ssi0_legacy_upgrade=True)`, claiming the snapshot's already-programmed
   SSI0 TCDs (the same trick `tools/guirun.py` uses) so
   `emu.sharc_capture.CapturingPeer` can observe genuine SSI0 "audio in"
   requests (`peer.rx()`) as the run proceeds under `emu.longrun.spin()`.
   Measured on `boot400M.snap`, `--unblock` off: this reaches real
   TCD50 major-loop completions and a delivered vector 170 every time
   (`Ssi0Dma.vector170` increments normally), but the vector-170 ISR that
   ran there did not write `INTFRCH1`'s force bit even after ten
   deliveries (`Ssi0Dma.force_asserted` stayed `False` throughout) --
   so on this snapshot/config the documented "SSI0-paced eDMA50 completion
   -> vector 170 -> software-forced vector 191" chain (`emu/ssi.py`'s own
   docstring) does not by itself reach the DSPI2 driver call within a
   bounded run. Recorded here as a finding, not chased further (out of
   this lane's scope: `emu/ssi.py`/the vector-170 handler's *own*
   forward-progress conditions belong to whichever lane owns that model).
   If claiming the snapshot's SSI0 TCDs fails outright (a shape mismatch --
   `Ssi0Dma.arm_legacy()` raises `RuntimeError`), SSI0 capture is simply
   skipped; DSPI2 capture (below) never depends on it either way. Either
   outcome is recorded in the capture's own JSON header (`ssi0`: "natural"
   or "unavailable: <reason>").
2. **Forcing it directly, for DSPI2** (always on, and what actually
   produces every DSPI2 frame this tool captures): every `--force-period`
   instructions, from inside `spin()`'s own `on_chunk` hook, clear the
   handler's pacing counter and call `Machine.raise_vector(191, level=...)`
   directly -- the same "force it, run the handler like anything else"
   technique `tools/sharcframe.py` already uses for a single call/return,
   paced through the normal instruction stream here (so it shares one
   Machine and one instruction clock with kind='note' below) instead of a
   separate call/return dance per frame.

   **The repeated forcing must not re-enter the handler on top of itself.**
   Measured by execution (Lane H1): one call to `vector_191_handler`
   (`prof["handler"]`) costs on the order of 45,000-60,000 ColdFire
   instructions before it returns -- comparable to, or larger than,
   `--force-period`'s own default of 50,000. Forcing unconditionally, as
   this loop did before Lane H1, calls `Machine.raise_vector()` with no
   `level=` and no interrupt-mask check, unlike every legitimate timer
   source in this codebase (`emu.pit.Pits.service()`); it recurses into
   the still-running previous call, indefinitely, so the CPU never
   actually returns to RTOS/task code between forced frames. `on_chunk`
   now calls `ready_to_force()` (this module's own function) first and
   defers -- retrying next chunk, not dropping the frame -- when the
   current interrupt level is already at or above vector 191's own
   configured level. See docs/findings/04-coldfire-dsp-link.md, "Lane H1:
   the DSPI2 forcing loop re-entered its own handler", for the measurement
   and its consequence: fixing this alone does not make every downstream
   ColdFire task (kit-load, panel dispatch, sequencer scheduling) start
   running within a bounded capture, because a ~50,000-instruction-costing
   level-5 handler firing every `--force-period` instructions still spends
   most of the CPU's time in that one handler at the default period --
   raising `--force-period` trades frame density for RTOS progress, it
   does not remove the tension.

**Lane I1: forcing is still required from a genuinely running snapshot, not
just an artifact of never reaching one.** Every measurement above (point 1's
"did not by itself reach the DSPI2 driver call") was taken on `boot400M.snap`,
before Lane H1 established that snapshot's RTOS never actually runs (see
"Lane H1" above) -- leaving open whether the natural SSI0/vector-170 chain
would complete once a capture started from a snapshot with a genuinely
running RTOS instead. It does not: from `snapshots/dt2-1.16/running.snap`
(`tools/dt2_reach_running.py`, MAIN_OS_RUNNING per `tools/bootcheck.py`'s own
criteria), with the DSPI2 forcing loop *disabled entirely* and only the SSI0
model wired up (`--ssi0-hz 1000`, `ssi0_legacy_upgrade=True`), `Ssi0Dma`
delivered vector 170 400 times over 120,000,000 further ColdFire
instructions (~80 real board-rate periods at the placeholder 1 kHz request
rate) and `force_asserted` never once went `True` -- zero natural calls to
the DSPI2 driver (`prof["driver"]`, `FUN_400cd2bc`). So this is not "the
system was never running" the way `boot400M.snap`'s zero notes turned out to
be (Lane H1): it is a genuine gap in the vector-170 ISR model, or a real
firmware precondition for that ISR to force vector 191 that a truly-running
RTOS still does not by itself satisfy within a bounded run (the real SSI0
board clock rate is also still unrecovered -- see `emu/ssi.py`'s own
docstring -- so the *rate* of delivery, not just whether it happens, is a
further unknown). Forcing vector 191 (point 2) therefore remains the only
way this tool gets a DSPI2 frame at all, from either kind of snapshot; this
is recorded as a finding for whichever lane next owns `emu/ssi.py`'s
vector-170 handler, not fixed here.

**Starting from a snapshot with a genuinely running RTOS.** Pass
`snapshots/dt2-1.16/running.snap` (or any snapshot `tools/dt2_reach_running.py`
produced) as `SNAPSHOT` directly -- `run()` now declares
`deferred_components=('timers',)` on every `build()` call and restores the
snapshot's own Pits/Dtims cadence via `ev['restore_checkpoint_timers']()`
when it has one (falling back to a fresh `Timers` for a plain
`emu.checkpoint` ladder rung with no such component, so `boot400M.snap`
keeps working exactly as before). `running.snap`'s build manifest was saved
with `unblock=True`, so a capture resuming it must also pass `--unblock`, or
`restore_into`'s manifest check refuses the mismatch outright (a real
safeguard, not friction to route around -- see `emu.snapshot.restore_into`'s
own docstring on why a resumed run must match the configuration that
produced the snapshot). No other change is needed: kit/pattern state is
already real on `running.snap` (`--pre-instrs` is for a plain boot-ladder
rung where `KIT_LOAD_FN` has not fired yet; skip it here).

**Triggering a note deterministically, with no framebuffer reading.**
`--kind note` injects a panel "TRIG 1" press+release at ColdFire instruction
count `--trig-at` (default: a quarter of `--instrs`), via `emu/panelin.py`
(the front-panel UART8 input path -- no GPIO/key-matrix to model, see that
module's own docstring) -- the exact wire bytes
(`encode_buttons(3, 1)` then `encode_buttons(3, 0)`, delivered as one
`feed()` so the firmware's own ISR drains both from one raised vector, per
`emu/panelin.py`'s docstring) a prior lane host-replayed and confirmed reach
the firmware's real panel-input path end to end
(docs/findings/03-ui-and-panel.md, "A panel-path track trigger joins
machine invalidation to the refresh queue": panel machine commit -> ...
-> panel TRIG 1 -> record construction -> queue append -> vector 191 ->
`FUN_4002d438` -> track-0 row -> TX frame `0x94` = machine type). Track 0
in that chain is the panel's own "Track 1" (1-indexed on the hardware,
0-indexed internally) -- this is the "note triggered on track 1" this tool
captures. No UI/framebuffer read is needed: the whole chain is inferred
from the resulting TX frame bytes this tool already captures.

**Starting the sequencer instead of a single panel trigger.** `--kind play`
injects a panel "PLAY" press+release at `--trig-at` the same way (wire bytes
`encode_buttons(2, 8)` then `encode_buttons(2, 0)` -- PLAY is channel 2 bit
3, read back from the running image's own button-name table,
`emu/panelin.py`'s `control_names(m, profile, 'button')`: code 20 = "PLAY",
matching `code_for`'s `channel*8+bit+1` for channels 0-5). This does not
need a `--poke-track-type`: the snapshot's already-loaded pattern has real
active trigs on it (docs/findings/03-ui-and-panel.md, "A panel-path track
trigger joins machine invalidation to the refresh queue": the causal
control "machine change followed by `PLAY` scheduled records only for the
current pattern's tracks 3, 12, 13 and 15" -- so starting the transport on
a stock `boot400M.snap`-derived snapshot already reaches the same
record/queue path as a bare panel TRIG, for those four tracks, with no
pattern-data poke needed). Use it to test whether firmware-driven sequencer
playback reaches the SHARC by a path a single hand-pressed TRIG does not
exercise (real timing, real step advance) -- `--instrs` needs to cover at
least one full pattern loop (16 steps) for every active track to be hit at
least once; at the default tempo that is on the order of 10-20M ColdFire
instructions from the press, not the few million `--kind note` needs.

**Changing a track's machine, without re-running the real UI navigation.**
The same finding's own causal-control run (`out/experiments/a2-queue-
trigger/report.json`) already established, end to end and byte-checked,
that "machine change on track 0, then TRIG 1" is what actually flips the TX
frame's machine-type word (`0x94 + 2*track`) from `0x0000` to a real type --
"TRIG 1 without a machine change" or "machine change without TRIG 1" alone
do not. But reaching that "machine menu commit" state through the real panel
(`MACHINE SEL` chord, encoder turns, `YES`) took that prior run 46-160M
ColdFire instructions of UI navigation just to get there, before the part
this tool cares about (the DSPI2 frame) ever happens -- far more than this
capture tool's own budget. `--poke-track-type TRACK:TYPE` instead seeds the
*result* of that commit directly: the per-track machine-type mirror byte
`0x80003cd0 + track*0x9a` (`tools/framelink.py`'s `track_9a`, corrected base
per docs/findings/04) that `FUN_4002d438`/the vector-191 handler already
reads on every real commit -- the same address and mechanism
`tools/sharcframe.py` used for its own `[V]`-marked single-poke experiment
("Measured, not only read", docs/findings/04). The vector-191 handler does
not care why that byte changed; it copies it into the TX frame exactly the
same way whether a real MACHINE SEL commit, a MIDI program change, or this
poke set it. This is not synthesizing the frame -- the DSPI2 driver call is
still the real one, hooked the same way as always -- it only stands in for
the (already independently `[V]`-established, just prohibitively expensive
to re-run here) UI front-end that would normally produce that same byte
change. Combine with `--kind note` (`--trig-at`) for the full causal chain
that report.json's own `machine_trig1` run used: a poked machine change,
then a real panel TRIG 1.

**Getting the *real* per-track machine type and active flag, with no poke
at all.** Every capture this tool produced before `--pre-instrs` existed --
`--kind idle`, `--kind note`, and `--kind play` alike, on unmodified
`snapshots/dt2-1.16/boot400M.snap` -- shows every track's machine-type and
`0x73c` active fields at zero for the whole run, even though a real,
non-empty kit is already loaded at that snapshot (`_DAT_80004704` is
non-null and one track already has a real machine type; see
`run_natural_track_refresh()`'s own docstring). That data just never gets
copied into the SRAM row the TX frame is built from, because the function
that does that copy (`KIT_LOAD_FN`, `FUN_4002d9c4`) has not run yet at that
exact snapshot instant -- it fires naturally 10-12M instructions later, but
only when nothing is servicing real PIT/DTIM timer interrupts in the
meantime, which every timed capture (this tool's own `--force-period`
phase included) does. `--pre-instrs N` runs an untimed pre-phase (bounded by
N, stopped early by a code hook the moment `KIT_LOAD_FN` fires) before the
timed capture phase starts, so the row already carries the snapshot's real
per-track data -- no `--poke-track-type` needed, though the two combine
freely (poke is applied after the pre-phase, so it can still override
specific tracks on top of whatever the natural refresh produced).
"""

from __future__ import annotations

import argparse
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from unicorn import UC_HOOK_CODE, UC_HOOK_MEM_WRITE  # noqa: E402
from unicorn.m68k_const import (  # noqa: E402
    UC_M68K_REG_A7,
    UC_M68K_REG_D0,
    UC_M68K_REG_PC,
    UC_M68K_REG_SR,
)

import framelink  # noqa: E402
from emu import config, dspiframe, panelin, symbols  # noqa: E402
from emu.dtim import Dtims, Timers  # noqa: E402
from emu.longrun import build, spin  # noqa: E402
from emu.pit import Pits, interrupt_level  # noqa: E402
from emu.sharc_capture import CaptureWriter, CapturingPeer  # noqa: E402

# The DSPI1 driver, docs/findings/04-coldfire-dsp-link.md "eDMA and DSPI
# transfer inventory" (channel 14, `0xfc03c034` PUSHR -- a *different*
# physical DSPI block than the SHARC's own `0xec038034`). Confirmed at this
# address on DT2 1.16 only (`out/ghidra/dt2-1.16-emac`); not re-checked on
# 1.15C, so the hook below is gated on the resolved profile's name.
DSPI1_DRIVER_ADDR = 0x400CD48A
DSPI1_PROFILE_NAME = "Digitakt II 1.16"

# The FlexBus window this lane tested as a candidate sample-data/control
# path and ruled out: confirmed by `tools/refscan.py` on the 1.16 image plus
# a decompile of the two functions that touch it (`FUN_400ccda0`,
# `FUN_400ccf74`) to be the SHARC program loader's own boot-time GPIO/status
# handshake, not a data channel -- see docs/findings/04, "Ruled out as the
# control link" (1.16 correction). Watched by default on 1.16 so a capture
# still records if that reading is ever wrong, or if some other code starts
# using the window after boot.
FLEXBUS_WATCH = (0x8C000000, 0x8C000010)

# docs/findings/03-ui-and-panel.md's "A panel-path track trigger joins
# machine invalidation to the refresh queue": the host-replayed wire event
# was channel 3 bit 0 (bytes 0x23 0x01 press, 0x23 0x00 release), and it
# drove track 0 (the panel's "TRIG 1", 1-indexed on the hardware).
TRIG_1_CHANNEL = 3
TRIG_1_BIT = 0

# Lane J1 (2026-09-26): generalizes TRIG_1_CHANNEL/TRIG_1_BIT to any track.
# `emu/panelin.py`'s own `code_for(channel, bit) = channel*8+bit+1` for
# channels 0-5, and the button-name table this module's own PLAY_CHANNEL/
# PLAY_BIT comment already reads (codes 25-40 = "TRIG 1".."TRIG 16") gives
# TRIG (track+1)'s code as 25+track -- so channel/bit is the inverse of
# `code_for` at that code. track=0 must reproduce TRIG_1_CHANNEL/TRIG_1_BIT
# exactly (checked by this function's own test).
TRIG_CODE_BASE = 25  # code_for(TRIG_1_CHANNEL, TRIG_1_BIT) == 25


def trig_channel_bit(track: int) -> tuple[int, int]:
    """-> (channel, bit) for pressing "TRIG track+1" (0-indexed `track`,
    0..15) -- see `TRIG_CODE_BASE`'s own comment. `track=0` is exactly
    `(TRIG_1_CHANNEL, TRIG_1_BIT)`."""
    if not 0 <= track <= 15:
        raise ValueError("track must be 0..15, got %r" % (track,))
    code = TRIG_CODE_BASE + track
    channel, bit = divmod(code - 1, 8)
    return channel, bit


# PLAY's (channel, bit), read back from the running 1.16 image's own button
# name table (`emu/panelin.py`'s `control_names(m, profile, 'button')`:
# code 20 = "PLAY", code 19 = "RECORD", code 21 = "STOP", codes 25-40 =
# "TRIG 1".."TRIG 16" -- matching `panelin.code_for`'s channel*8+bit+1 for
# channels 0-5: code 20 -> channel 2, bit 3). Used for `--kind play`: start
# the sequencer on the pattern already loaded in the snapshot (the a2-queue-
# trigger experiment's "current pattern enqueues tracks 3,12,13,15 only"
# recorded in docs/findings/03-ui-and-panel.md, "A panel-path track trigger
# joins machine invalidation to the refresh queue" -- so a stock snapshot's
# default pattern already has active trigs, with no pattern-data poke
# needed) instead of a single panel TRIG, to test whether firmware-driven
# sequencer playback reaches the SHARC by a different path than a bare panel
# trigger.
PLAY_CHANNEL = 2
PLAY_BIT = 3

# tools/framelink.py TABLES's 'track_9a' entry (base corrected to 0x80003cd0
# in docs/findings/04-coldfire-dsp-link.md, "Corrections to the SRAM
# layout"): 16 rows of 0x9a bytes; byte 0 of each row is the track's machine
# type, per "The machine type reaches the SHARC, at TX frame offset
# 0x94 + 2i".
TRACK_9A_BASE = 0x80003CD0
TRACK_9A_STRIDE = 0x9A
TRACK_TYPE_OFFSET = 0

# The real per-track kit-load-and-refresh (docs/findings/04-coldfire-dsp-link.md,
# "Lane A3: the natural per-track kit-load-and-refresh event"): FUN_4002da7a
# sets the live kit pointer `_DAT_80004704` and zeroes the sync-cache/mute
# arrays; FUN_4002d9c4 (same live pointer) then calls FUN_4002d438 for every
# unmasked track, copying that track's real machine-type/parameter bytes
# from its live source object into the SRAM mirror row the vector-191
# handler reads into the TX frame. See run_natural_track_refresh()'s own
# docstring for when this fires and why it needs an untimed pre-phase.
KIT_LOAD_FN = 0x4002D9C4
ROW_REFRESH_FN = 0x4002D438

# Calibration for the unfixed fractional EMAC (1.16 only). vector_191_handler
# calls the parameter smoother FUN_400d92a2(&0x80003340) at 0x4002e8c6; it
# returns D0 = 0x80005b50, and the call's argument is still on the stack at
# the return address 0x4002e8cc (`addq.l #4,SP` follows). The smoother is a
# unity-gain one-pole in fractional EMAC mode over the first
# SMOOTHED_BYTES of the block: output halfword j (at D0 + 2j) smooths input
# halfword j (at arg + 2j). Patched Unicorn leaves out the fractional
# `<< 1` (MCF54418RM p.159), so the filter settles at ~0.029 x instead of
# x, and every smoothed TX field (sample slot, tune, level, filter, amp)
# decays towards 0. On hardware, with no parameter moving, the output
# settles at the input. SLOT_HALFWORD is track t's sample slot in that row
# (mirror index 28, TX 0xe0 + 0x60t); rows are TRACK_ROW_STRIDE apart.
SMOOTHER_RETURN = 0x4002E8CC
SMOOTHED_BYTES = 0x990
TRACK_ROW_STRIDE = 0x8E
SLOT_HALFWORD = 0x2D


def _resolve_panel_profile(main_img: bytes):
    return symbols.resolve(main_img)


def parse_track_type_poke(spec: str) -> tuple[int, int]:
    """'TRACK:TYPE' -> (track, type), both plain ints (0x-prefixed or
    decimal). Raises ValueError with SPEC in the message on a bad shape, so
    argparse reports a useful error."""
    if ":" not in spec:
        raise ValueError("bad --poke-track-type %r (want TRACK:TYPE)" % spec)
    track_s, type_s = spec.split(":", 1)
    track, type_ = int(track_s, 0), int(type_s, 0)
    if not 0 <= track < 16:
        raise ValueError("bad --poke-track-type %r: track must be 0..15" % spec)
    return track, type_


def parse_slot_poke(spec: str) -> tuple[int, int]:
    """'TRACK:VALUE' -> (track, value) for `--poke-slot`."""
    if ":" not in spec:
        raise ValueError("bad --poke-slot %r (want TRACK:VALUE)" % spec)
    track_s, value_s = spec.split(":", 1)
    track, value = int(track_s, 0), int(value_s, 0)
    if not 0 <= track < 16:
        raise ValueError("bad --poke-slot %r: track must be 0..15" % spec)
    if not -0x8000 <= value < 0x10000:
        raise ValueError("bad --poke-slot %r: value must fit 16 bits" % spec)
    return track, value


def parse_press(spec: str) -> tuple[str, int]:
    """'NAME@INSTR' -> (NAME, INSTR) for `--press`: a panel button by the
    firmware's own name (e.g. NO, YES, PLAY, "TRIG 1") and the capture-relative
    instruction count at which it is pressed and released."""
    if "@" not in spec:
        raise ValueError("bad --press %r (want NAME@INSTR)" % spec)
    name, at_s = spec.rsplit("@", 1)
    if not name:
        raise ValueError("bad --press %r: empty button name" % spec)
    return name, int(at_s, 0)


def button_channel_bit(m, profile, name: str) -> tuple[int, int]:
    """-> (channel, bit) of the panel button the running image calls NAME.
    Codes 1..48 are `channel * 8 + bit + 1` (`emu.panelin.code_for`)."""
    names = panelin.control_names(m, profile, "button")
    for code, text in names.items():
        if text == name and 1 <= code <= 48:
            return divmod(code - 1, 8)
    raise ValueError(
        "no panel button named %r (known: %s)" % (name, sorted(names.values()))
    )


def install_smoother_calibration(m, at, settle: bool, slot_pokes) -> None:
    """Hook the return from the parameter smoother (SMOOTHER_RETURN's own
    comment) and, every tick:

    - settle: copy the smoother's input over its output, i.e. what the
      hardware's unity-gain filter has settled to when no parameter moves;
    - slot_pokes: then write VALUE to track TRACK's smoothed slot halfword.

    Both are host calibration standing in for the unfixed fractional EMAC
    mode, not firmware behaviour; they are off by default and recorded in the
    capture header. The rest of the handler (the slot rewrite for
    FUN_4002dcee tracks, the frame copies) runs on the corrected row as it
    would on hardware."""

    def hook(uc, addr, size, data):
        out = uc.reg_read(UC_M68K_REG_D0)
        sp = uc.reg_read(UC_M68K_REG_A7)
        src = struct.unpack(">I", bytes(uc.mem_read(sp, 4)))[0]
        if settle:
            uc.mem_write(out, bytes(uc.mem_read(src, SMOOTHED_BYTES)))
        for track, value in slot_pokes:
            uc.mem_write(
                out + track * TRACK_ROW_STRIDE + 2 * SLOT_HALFWORD,
                struct.pack(">H", value & 0xFFFF),
            )

    at(SMOOTHER_RETURN, hook)


def parse_mem_range(spec: str) -> tuple[int, int]:
    """'LO:HI' -> (lo, hi), both plain ints (0x-prefixed or decimal), for
    `--watch-mem`: every write with lo <= address < hi becomes a
    `emu.sharc_capture.CaptureWriter.write_mem_write` record."""
    if ":" not in spec:
        raise ValueError("bad --watch-mem %r (want LO:HI)" % spec)
    lo_s, hi_s = spec.split(":", 1)
    lo, hi = int(lo_s, 0), int(hi_s, 0)
    if hi <= lo:
        raise ValueError("bad --watch-mem %r: HI must be > LO" % spec)
    return lo, hi


def install_dspi1_observer(at, writer, counter) -> None:
    """Log every call to the DSPI1 driver (`DSPI1_DRIVER_ADDR`'s own
    docstring), without altering control flow: unlike the DSPI2 driver hook
    in `run()`, which must fake a reply since nothing here answers as a real
    SHARC would, this is a plain observer -- the same idiom
    `emu.longrun.build()`'s own `task_create`/`do_print` hooks use -- so
    whatever the real firmware does next (including a real hang, if this
    turns out to matter) happens exactly as it would with no hook at all."""

    def dspi1_hook(uc, addr, size, data):
        sp = uc.reg_read(UC_M68K_REG_A7)
        _ret, length, src, callback = struct.unpack(">IIII", bytes(uc.mem_read(sp, 16)))
        writer.write_dspi1_call(counter(), length, src, callback)

    at(DSPI1_DRIVER_ADDR, dspi1_hook)


def install_mem_write_watch(m, writer, counter, lo: int, hi: int) -> None:
    """Record every guest write in `[lo, hi)` as a
    `emu.sharc_capture.CaptureWriter.write_mem_write` record. `value` comes
    straight from Unicorn's own `UC_HOOK_MEM_WRITE` callback (the same
    convention `emu/console.py`'s `on_write` uses for UART8), packed
    big-endian to `size` bytes -- the ColdFire's own byte order, matching
    every other field this format records."""

    def mem_write_hook(uc, access, address, size, value, data):
        writer.write_mem_write(
            counter(), address, (value & ((1 << (8 * size)) - 1)).to_bytes(size, "big")
        )

    m.uc.hook_add(UC_HOOK_MEM_WRITE, mem_write_hook, begin=lo, end=hi - 1)


def poke_track_type(m, track: int, type_: int) -> None:
    """Set TRACK's machine-type mirror byte directly (see TRACK_9A_BASE's
    own docstring) -- the same address/mechanism docs/findings/04's
    tools/sharcframe.py experiment used, standing in for a real (but, per
    this module's own docstring, prohibitively expensive to re-run here)
    MACHINE SEL commit. Does not touch the frame itself: the next real
    vector-191 firing reads this byte through FUN_4002d438/the handler's
    own copy exactly as it would a firmware-driven change."""
    m.uc.mem_write(
        TRACK_9A_BASE + track * TRACK_9A_STRIDE + TRACK_TYPE_OFFSET,
        bytes([type_ & 0xFF]),
    )


def ready_to_force(m, level) -> bool:
    """-> False if forcing vector 191 now would re-enter a same-or-higher-
    priority handler that has not returned yet.

    `run()`'s own docstring says forcing raises vector 191 directly every
    `--force-period` instructions, `Machine.raise_vector()`-style, "the
    same technique tools/sharcframe.py already uses and has proven safe" --
    true for a single call/return, but not for the periodic forcing here.
    Measured by execution (Lane H1, boot400M.snap, no `--syx` bytes stored
    since the .syx is Elektron's copyright): one call to
    `vector_191_handler` (`prof["handler"]`) costs on the order of
    45,000-60,000 ColdFire instructions before it returns to its
    interrupted caller -- comparable to, or larger than, `--force-period`'s
    own default of 50,000. `Machine.raise_vector()` unconditionally pushes
    a fresh exception frame and jumps to the handler regardless of the
    current interrupt mask; a real DSPI2 controller signalling vector 191
    at its own configured level would not do that while the CPU is
    already servicing that same level (`emu.pit.Pits.service()` checks
    this for every legitimate timer source, via this same
    `emu.pit.interrupt_level()` -- forcing here previously did not).
    Left unguarded, the periodic force recurses into the handler on top of
    an unfinished previous call, indefinitely, so the CPU never actually
    returns to RTOS/task code between forced frames -- which starves every
    ColdFire task this project has tried to observe during a forced
    capture, not only sequencer-related ones. See docs/findings/04-
    coldfire-dsp-link.md, "Lane H1: the DSPI2 forcing loop re-entered its
    own handler".

    LEVEL is the vector's own configured INTC level (this module's own
    callers pass `interrupt_level(m, prof["vector"], respect_mask=False)`,
    read once; the mask itself is irrelevant here since this is a forced,
    not a real, delivery). `None` (the level could not be read, e.g. an
    unresolved profile) keeps the old unconditional behaviour -- silently
    refusing every forced frame would be worse than the rare unprotected
    call this predates."""
    if level is None:
        return True
    sr = m.uc.reg_read(UC_M68K_REG_SR)
    return ((sr >> 8) & 0x07) < level


def run_natural_track_refresh(m, pc: int, budget: int, *, chunk: int = 200_000) -> int:
    """Run up to BUDGET instructions, untimed, so the real per-track
    kit-load-and-refresh (KIT_LOAD_FN's own docstring) has a chance to fire
    before the timed capture phase starts. -> the PC to resume from.

    Every capture this project has produced before this function existed
    used ``snapshots/dt2-1.16/boot400M.snap`` unmodified: at that exact
    point the live kit pointer (``_DAT_80004704``) is already set to a real,
    non-null project (one track already carries a real machine type, and
    every other per-track object already holds plausible non-default
    parameter bytes -- this is not an empty/unloaded project), but the SRAM
    mirror row the TX frame is built from is still at its zero-initialized
    reset value: KIT_LOAD_FN has not run since boot, so nothing has ever
    copied that real data into the row. Confirmed by execution: resuming
    ``boot400M.snap`` for 10-12M further ColdFire instructions, with no
    panel input and no forced vector 191, reaches KIT_LOAD_FN exactly once,
    which calls ROW_REFRESH_FN for all 16 tracks and makes the mismatch
    disappear (the row byte for the one track with a real machine type
    changes from 0 to that type).

    This needs its own untimed, pits-less phase because the interaction is
    with time-of-check, not just elapsed instructions: the same resume
    driven through ``emu.longrun.spin()`` with a real
    ``emu.dtim.Timers``/``emu.pit.Pits`` object servicing PIT/DTIM
    interrupts -- what every capture/replay tool in this project, including
    this module's own timed capture phase below, normally does -- did not
    reach KIT_LOAD_FN within at least 60M further instructions in the same
    control. Only removing the PIT/DTIM timer service (not chunking, not
    the SSI0 model, not the idle-yield reschedule ``emu.longrun.build()``
    always installs -- each checked in isolation) restores the ~12M timing.
    Why real PIT/DTIM service changes which task the guest OS schedules
    enough to block or badly delay this one function was not chased further
    here: this pre-phase is a bounded, opt-in workaround for capturing real
    per-track data, not a fix to the timer/scheduling model. Flagged open in
    docs/findings/04-coldfire-dsp-link.md.

    BUDGET is a cap, not a fixed cost: a code hook on KIT_LOAD_FN stops the
    run as soon as it fires, so a generous budget does not slow down a
    capture where the event already happened early. If it never fires
    within BUDGET, this returns the PC reached anyway -- the caller's own
    capture proceeds exactly as it would have with pre_instrs=0, just having
    spent BUDGET instructions first.
    """

    def on_kit_load(uc, addr, size, data):
        uc.emu_stop()

    handle = m.uc.hook_add(
        UC_HOOK_CODE, on_kit_load, begin=KIT_LOAD_FN, end=KIT_LOAD_FN
    )
    try:
        # Deliberately no `pits=`/`async_events=` here -- see this
        # function's own docstring for why a timed phase does not reach
        # KIT_LOAD_FN.
        pc, _done, _stop = spin(m, pc, budget, chunk)
    finally:
        # One-shot: a hook left installed would also fire on any later,
        # legitimate re-entry of KIT_LOAD_FN during the timed capture phase
        # that follows, silently truncating it via the same emu_stop().
        m.uc.hook_del(handle)
    return pc


def run(
    snapshot: str,
    out_path: str,
    *,
    kind: str,
    instrs: int,
    syx: str | None = None,
    ssi0_hz: int = 1000,
    trig_at: int | None = None,
    trig_track: int = 0,
    force_period: int = 50_000,
    chunk: int = 200_000,
    unblock: bool = False,
    poke_track_types: tuple[tuple[int, int], ...] = (),
    watch_mem: tuple[tuple[int, int], ...] = (),
    pre_instrs: int = 0,
    card_image: str | None = None,
    presses: tuple[tuple[str, int], ...] = (),
    trig_hold: int = 0,
    settle_smoother: bool = False,
    slot_pokes: tuple[tuple[int, int], ...] = (),
) -> dict:
    if kind not in ("idle", "note", "play"):
        raise ValueError("kind must be 'idle', 'note' or 'play', got %r" % (kind,))
    if kind in ("note", "play") and trig_at is None:
        trig_at = instrs // 4

    image = config.main_image()
    image_sha256, prof = framelink.profile_for(image)
    with open(image, "rb") as fh:
        main_img = fh.read()

    ssi0_status = "natural"
    build_kwargs: dict[str, object] = dict(
        unblock=unblock,
        softfloat=True,
        bitmap=True,
        dsp=True,
        ssi0_request_hz=ssi0_hz,
        ssi0_legacy_upgrade=True,
        # A snapshot from tools/dt2_reach_running.py (or any stateful
        # emu.checkpoint.save_longrun-style save) carries its own Pits/Dtims
        # cadence as a deferred 'timers' component; a plain emu.checkpoint
        # ladder rung (e.g. boot400M.snap) has none. Declaring the name here
        # makes both resumable from the same call -- see the timer
        # construction below, which reads back whichever case applies via
        # ev['restore_checkpoint_timers']().
        deferred_components=("timers",),
    )
    if syx:
        build_kwargs["syx"] = syx
    if card_image:
        build_kwargs["card_image"] = card_image
    try:
        m, ev, _st, pc, _inq, at = build(snapshot, **build_kwargs)
    except RuntimeError as exc:
        # Ssi0Dma.arm_legacy() shape mismatch, or the force-RTE symbol
        # missing -- fall back to no SSI0 model at all (see module
        # docstring, "Fallback"): DSPI2 capture must not depend on it.
        ssi0_status = "unavailable: %s" % exc
        build_kwargs.pop("ssi0_request_hz")
        build_kwargs.pop("ssi0_legacy_upgrade")
        m, ev, _st, pc, _inq, at = build(snapshot, **build_kwargs)

    panel_profile = _resolve_panel_profile(main_img)
    m.uc.mem_write(prof["gate"], bytes(4))  # open the frame-build gate
    press_plan = sorted(
        (at_, name, button_channel_bit(m, panel_profile, name)) for name, at_ in presses
    )
    if (settle_smoother or slot_pokes) and prof["name"] != DSPI1_PROFILE_NAME:
        raise ValueError("--settle-smoother/--poke-slot addresses are 1.16 only")

    if pre_instrs:
        # Let the real per-track kit-load-and-refresh run before the timed
        # capture phase starts, so the SRAM row (and hence the TX frame)
        # carries genuine machine-type/active data instead of the
        # zero-initialized reset values -- see run_natural_track_refresh()'s
        # own docstring for what this waits for and why it needs its own
        # untimed phase.
        pc = run_natural_track_refresh(m, pc, pre_instrs, chunk=chunk)

    for track, type_ in poke_track_types:
        poke_track_type(m, track, type_)

    # Reuse the snapshot's own timer cadence when it has one (a
    # tools/dt2_reach_running.py-style stateful save), instead of always
    # constructing fresh Pits/Dtims -- restoring fresh ones onto a snapshot
    # that already carries a 'timers' component both loses its held/fired/
    # missed state and, since deferred_components=('timers',) is now always
    # passed above, would leave that component permanently unclaimed
    # (Machine._checkpoint_deferred_restore.require_claimed() raises the
    # moment spin() is called). Falls back to a fresh Timers, exactly the
    # prior behaviour, for a plain ladder rung with no such component.
    pits = ev["restore_checkpoint_timers"]()
    if pits is None:
        pits = Timers(Pits(m), Dtims(m, channels=(3,)))
    ssi0 = ev.get("ssi0_dma")
    if ssi0 is not None:
        ssi0.align(pits.now)

    writer = CaptureWriter(
        out_path,
        frame_bytes=dspiframe.FRAME_BYTES,
        kind=kind,
        device="dt2",
        source_sha256=image_sha256,
        extra={
            "profile": prof["name"],
            "ssi0": ssi0_status,
            "ssi0_hz": ssi0_hz if ssi0 is not None else None,
            "poke_track_types": ["%d:%d" % (t, y) for t, y in poke_track_types],
            "pre_instrs": pre_instrs,
            "trig_track": trig_track if kind == "note" else None,
            "presses": ["%s@%d" % (name, at_) for at_, name, _cb in press_plan],
            "trig_hold": trig_hold,
            # Host calibration, not firmware behaviour (SMOOTHER_RETURN).
            "settle_smoother": settle_smoother,
            "poke_slots": ["%d:%d" % (t, v) for t, v in slot_pokes],
        },
    )
    peer = CapturingPeer(writer, counter=lambda: pits.now)
    if ssi0 is not None:
        ssi0.peer = peer

    def driver_hook(uc, addr, size, data):
        sp = uc.reg_read(UC_M68K_REG_A7)
        ret, tx_len, tx, rx_len, rx = dspiframe.read_driver_call(uc, sp)
        # Capture the *whole* `dspiframe.FRAME_BYTES`-byte wire frame from
        # `tx`, not only the driver call's own `tx_len` (0x802 real payload
        # words) argument -- see this module's own docstring, "Getting a
        # DSPI2 driver call to happen at all", and dspiframe.py's "TX length"
        # note: the driver's TX source buffer is one persistent
        # `FRAME_BYTES`-byte on-chip SRAM region (TX_BASE_DT2/DN2), built
        # once at boot with `0x8001xxxx` PUSHR tag-only entries past
        # `tx_len` and only the first `tx_len` bytes refreshed by a memcpy
        # each cycle; reading the whole region lets a capture consumer (see
        # tools/sharc_replay.py) check that tail for a signal docs/findings/
        # 04-coldfire-dsp-link.md's "Note trigger and sample data" section
        # did not find in the payload alone, instead of assuming it is
        # constant tag padding. Falls back to `tx_len` bytes if the extra
        # read is not safely mapped (e.g. the test-mode driver's own
        # differently based, possibly shorter TX buffer), so this is a
        # strict superset of the old behaviour on every real DT2/DN2 capture.
        want = max(tx_len, dspiframe.FRAME_BYTES)
        try:
            tx_bytes = bytes(uc.mem_read(tx, want)) if tx and want else b""
        except Exception:
            tx_bytes = bytes(uc.mem_read(tx, tx_len)) if tx and tx_len else b""
        rx_bytes = peer.exchange(tx_bytes)
        if rx and rx_len:
            fill = rx_bytes[:rx_len].ljust(rx_len, b"\x00")
            uc.mem_write(rx, fill)
        uc.reg_write(UC_M68K_REG_D0, 0)
        uc.reg_write(UC_M68K_REG_A7, sp + 4)
        uc.reg_write(UC_M68K_REG_PC, ret)

    at(prof["driver"], driver_hook)
    if settle_smoother or slot_pokes:
        install_smoother_calibration(m, at, settle_smoother, slot_pokes)

    if prof["name"] == DSPI1_PROFILE_NAME:
        install_dspi1_observer(at, writer, lambda: pits.now)

    # Watch the FlexBus boot-loader window by default on 1.16 (see
    # FLEXBUS_WATCH's own docstring), plus whatever the caller asked for.
    mem_ranges = tuple(watch_mem)
    if prof["name"] == DSPI1_PROFILE_NAME:
        mem_ranges = (FLEXBUS_WATCH, *mem_ranges)
    for lo, hi in mem_ranges:
        install_mem_write_watch(m, writer, lambda: pits.now, lo, hi)

    triggered: dict = {"done": False, "released": not trig_hold}
    forced = {"last": 0, "skipped": 0}
    # Read once: the vector's configured INTC level, ignoring its mask bit
    # (this is a forced, not a real, delivery -- see ready_to_force()'s own
    # docstring for why the mask is irrelevant here).
    vector_level = interrupt_level(m, prof["vector"], respect_mask=False)

    pressed = {"n": 0}

    def on_chunk(pc_, done):
        while pressed["n"] < len(press_plan) and done >= press_plan[pressed["n"]][0]:
            _at, _name, (channel, bit) = press_plan[pressed["n"]]
            panelin.feed(
                m,
                panel_profile,
                panelin.encode_buttons(channel, 1 << bit)
                + panelin.encode_buttons(channel, 0),
            )
            pressed["n"] += 1
        if kind in ("note", "play") and not triggered["done"] and done >= trig_at:
            channel, bit = (
                trig_channel_bit(trig_track)
                if kind == "note"
                else (PLAY_CHANNEL, PLAY_BIT)
            )
            data = panelin.encode_buttons(channel, 1 << bit)
            if not trig_hold:
                data += panelin.encode_buttons(channel, 0)
            else:
                triggered["release"] = (trig_at + trig_hold, channel)
            panelin.feed(m, panel_profile, data)
            triggered["done"] = True
        release = triggered.get("release")
        if release is not None and done >= release[0]:
            panelin.feed(m, panel_profile, panelin.encode_buttons(release[1], 0))
            triggered["release"] = None
            triggered["released"] = True
        if done - forced["last"] >= force_period:
            if ready_to_force(m, vector_level):
                m.uc.mem_write(prof["counter"], bytes(4))
                m.raise_vector(prof["vector"], level=vector_level)
                forced["last"] = done
            else:
                # Still inside a previous forced call (see ready_to_force()'s
                # own docstring) -- try again next chunk instead of
                # re-entering the handler on top of itself. Not counted
                # against `forced["last"]`, so the next chunk boundary
                # retries immediately once the handler actually returns,
                # rather than silently dropping this frame for good.
                forced["skipped"] += 1

    async_events = tuple(e for e in (ssi0,) if e is not None)
    try:
        pc, done, stop = spin(
            m,
            pc,
            instrs,
            chunk,
            on_chunk=on_chunk,
            pits=pits,
            async_events=async_events,
        )
    finally:
        writer.close()
        m.close()

    return {
        "stop": stop,
        "instructions": done,
        "frames": writer.counts["dspi2_frames"],
        "ssi0_rx": writer.counts["ssi0_rx"],
        "dspi1_calls": writer.counts["dspi1_calls"],
        "mem_writes": writer.counts["mem_writes"],
        "ssi0_status": ssi0_status,
        "forced_frames_deferred": forced["skipped"],
        "triggered": triggered["done"] if kind in ("note", "play") else None,
        "presses_delivered": pressed["n"],
        "released": triggered["released"] if kind in ("note", "play") else None,
        "image_sha256": image_sha256,
        "out": out_path,
    }


def parse_args(argv=None):
    p = argparse.ArgumentParser(
        description="Capture real ColdFire<->SHARC DSPI2/SSI0 traffic from a snapshot."
    )
    p.add_argument("snapshot")
    p.add_argument("--out", required=True)
    p.add_argument(
        "--kind",
        choices=("idle", "note", "play"),
        required=True,
        help="idle: no panel input. note: panel TRIG 1 press+release at "
        "--trig-at. play: panel PLAY press+release at --trig-at (starts "
        "the loaded pattern; see this module's own docstring)",
    )
    p.add_argument("--instrs", type=lambda s: int(s, 0), default=2_000_000)
    p.add_argument("--syx")
    p.add_argument(
        "--card-image",
        dest="card_image",
        default=None,
        help="+Drive image built by tools/plusdrive.py to serve behind the "
        "eSDHC/eMMC model (see emu.esdhc.Card.from_file); default: the "
        "blank, all-zero card",
    )
    p.add_argument("--ssi0-hz", type=int, default=1000)
    p.add_argument("--trig-at", type=lambda s: int(s, 0), default=None)
    p.add_argument(
        "--trig-track",
        dest="trig_track",
        type=lambda s: int(s, 0),
        default=0,
        metavar="N",
        help="0-indexed track TRIG (N+1) presses under --kind note "
        "(see trig_channel_bit()'s own docstring); default 0 (TRIG 1), "
        "the prior hardcoded behaviour",
    )
    p.add_argument("--force-period", type=lambda s: int(s, 0), default=50_000)
    p.add_argument("--chunk", type=lambda s: int(s, 0), default=200_000)
    p.add_argument("--unblock", action="store_true")
    p.add_argument(
        "--poke-track-type",
        dest="poke_track_type",
        action="append",
        default=[],
        metavar="TRACK:TYPE",
        help="seed a track's machine-type mirror byte before capturing (see "
        "poke_track_type()'s own docstring); repeatable",
    )
    p.add_argument(
        "--watch-mem",
        dest="watch_mem",
        action="append",
        default=[],
        metavar="LO:HI",
        help="record every write in [LO, HI) as a REC_MEM_WRITE record (see "
        "parse_mem_range()'s own docstring); repeatable. On the 1.16 "
        "profile, FLEXBUS_WATCH (0x8c000000-0x8c000010) is always added, "
        "in addition to whatever this gives",
    )
    p.add_argument(
        "--pre-instrs",
        dest="pre_instrs",
        type=lambda s: int(s, 0),
        default=0,
        metavar="N",
        help="run up to N instructions, untimed, before the timed capture "
        "phase, to let the real per-track kit-load-and-refresh fire (see "
        "run_natural_track_refresh()'s own docstring); 0 (default) skips "
        "this and keeps every existing capture's exact behaviour. On "
        "snapshots/dt2-1.16/boot400M.snap this fires within 10-12M "
        "instructions, so 15_000_000 is a reasonable value; a code hook "
        "stops the phase as soon as it fires, so a larger budget costs "
        "nothing when it fires early",
    )
    p.add_argument(
        "--press",
        dest="press",
        action="append",
        default=[],
        metavar="NAME@INSTR",
        help="press and release the panel button the image calls NAME (e.g. "
        "NO, YES, PLAY) at capture-relative instruction INSTR; repeatable. "
        "Use it to close a modal window (loaded +Drive snapshots show 'FILE "
        "SYSTEM OK / PRESS Y/N TO CLOSE', which swallows TRIG keys)",
    )
    p.add_argument(
        "--trig-hold",
        dest="trig_hold",
        type=lambda s: int(s, 0),
        default=0,
        metavar="N",
        help="hold the --kind note/play button for N instructions before "
        "releasing it (0, the default: press and release in one panel "
        "message, the old behaviour). A hold past --instrs never releases",
    )
    p.add_argument(
        "--settle-smoother",
        dest="settle_smoother",
        action="store_true",
        help="CALIBRATION (1.16): after each parameter-smoother call, copy "
        "its input over its output, i.e. the hardware's settled value. Stands "
        "in for the unfixed fractional EMAC mode, which makes every smoothed "
        "TX field decay to ~0.03x (see SMOOTHER_RETURN). Off by default",
    )
    p.add_argument(
        "--poke-slot",
        dest="poke_slot",
        action="append",
        default=[],
        metavar="TRACK:VALUE",
        help="CALIBRATION (1.16): after each smoother call, set 0-indexed "
        "TRACK's smoothed sample-slot word (TX 0xe0 + 0x60*TRACK) to VALUE; "
        "repeatable; applied after --settle-smoother. Stands in for the "
        "unfixed fractional EMAC mode. Off by default",
    )
    return p.parse_args(argv)


def main(argv=None) -> int:
    args = parse_args(argv)
    os.makedirs(os.path.dirname(os.path.abspath(args.out)) or ".", exist_ok=True)
    poke_track_types = tuple(
        parse_track_type_poke(spec) for spec in args.poke_track_type
    )
    watch_mem = tuple(parse_mem_range(spec) for spec in args.watch_mem)
    result = run(
        args.snapshot,
        args.out,
        kind=args.kind,
        instrs=args.instrs,
        syx=args.syx,
        card_image=args.card_image,
        ssi0_hz=args.ssi0_hz,
        trig_at=args.trig_at,
        trig_track=args.trig_track,
        force_period=args.force_period,
        chunk=args.chunk,
        unblock=args.unblock,
        poke_track_types=poke_track_types,
        watch_mem=watch_mem,
        pre_instrs=args.pre_instrs,
        presses=tuple(parse_press(spec) for spec in args.press),
        trig_hold=args.trig_hold,
        settle_smoother=args.settle_smoother,
        slot_pokes=tuple(parse_slot_poke(spec) for spec in args.poke_slot),
    )
    print(
        "%s: %d frame(s), %d ssi0-rx, %d dspi1-calls, %d mem-writes, "
        "%d forced-frame(s) deferred (re-entrant), stop=%s, instructions=%d, "
        "ssi0=%s%s"
        % (
            args.out,
            result["frames"],
            result["ssi0_rx"],
            result["dspi1_calls"],
            result["mem_writes"],
            result["forced_frames_deferred"],
            result["stop"],
            result["instructions"],
            result["ssi0_status"],
            ""
            if result["triggered"] is None
            else (", triggered" if result["triggered"] else ", NOT triggered"),
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
