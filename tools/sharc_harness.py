"""Render one Digitakt II SHARC+ voice offline with tools/sharc_run.py and
write it to a WAV, for correctness- and speed-checking the emulator against
a real firmware code path rather than a hand-written test vector.

    uv run python tools/sharc_harness.py dt2-1.16 --blocks 8 \
        --out /tmp/voice0.wav

Library use:

    import sharc_harness as h
    memory = h.load_image_memory("dt2-1.16")
    blocks = h.render_blocks(memory, voice=0, n_blocks=8)
    h.write_wav("/tmp/voice0.wav", h.flatten(blocks), sample_rate=48000)

Per docs/findings/06-sharc-engine-and-startup.md's "The voice record
contract" and "Voices" sections (two agents checked the contract table
against the 1.16 bytes): one voice occupies a 0x1d8-byte record at
``profile.voice_records + voice*0x1d8``. FUN_1c4ecf (``profile.voice_render``,
called the way FUN_1c642a's dispatch loop calls it: **R4 = the record
address itself**, not record+4 -- see "Corrected voice call convention"
below) is one voice's render, producing 64 interpolated floats at
record+4..+0x103 and, through its own call to the decimator at 0xb80000, a
2:1-decimated 32-sample block.

**Corrected voice call convention.** An earlier version of this harness
passed R4 = record+4, compensating with two empirical pokes (record+0x1bc
set to 1, record+4's first float set to 1.0) to reach the render body at
all. Both were papering over the same off-by-4: FUN_1c642a's real call
site (sw 0x1c6ad3-0x1c6b00) loads ``I4 = DM(I6-4)`` (frame_workspace,
``profile.frame_workspace``), computes ``R13 = DM(I6-4) + 4`` once before
the 32-voice loop and passes ``R4 = R13`` unmodified to FUN_1c4ecf/
FUN_1c5576 -- i.e. R4 = frame_workspace + 4 = ``profile.voice_records``,
the record base, confirmed directly against ``out/sharcdb``'s decode at
those sw addresses. Inside FUN_1c4ecf/FUN_1c4f81, I4 is set to R4 once
(sw 0x1c4f0f) and never rebased before the fields the contract table
documents at record+4.., record+0x1b8 etc are read through it -- so those
offsets are relative to the *record base*, matching the contract table
exactly once R4 is corrected. Both empirical pokes are gone.

**The record+0 open problem, resolved: it is the raw sample-data
pointer.** FUN_1c4ecf's own entry gate (sw 0x1c4f15: ``R4 = DM(I4, M5)``,
i.e. word 0 of the record) was previously undocumented ("[O]" in the
contract table's context) and empirically zero-filled unless non-zero,
alongside the ACTIVE byte at +0x1b8. Tracing FUN_1c4f81 (out/sharcdb
decode, sw 0x1c504a): **before I4 is rebased to +0x1b8** (sw 0x1c504c),
``I0 = DM(I4, M5)`` reads that same word-0 field, and the DO 64
interpolation loop (sw 0x1c5096-0x1c5101) reloads ``I4 = I0`` every
iteration (sw 0x1c50a4) before indexing the raw samples it multiplies
against the polyphase taps. So record+0 is a pointer: null means "no
sample assigned" (the gate FUN_1c4ecf checks), non-null is the address the
DO 64 loop actually reads PCM through. ``setup_voice()`` now writes
``sample_base`` there instead of setting a dead I0 register (I0 was always
overwritten before the loop used it -- this is why seeding the register
directly, as the previous version did, had no effect).

Symbols (function/data addresses) come from ``tools/sharc_symbols.py``'s
``resolve()`` against ``out/sharcdb``, not a hardcoded dict -- see
``profile()`` below. Only record layout (byte offsets within one voice
record, and the coefficient table's phase stride) are constants here, since
those are not independently-addressed symbols.

**--run-init now actually feeds the render (2026-09-25).** Previously,
``run_init()``'s Runner and its State were thrown away: ``render_blocks()``/
``check_correctness()`` always called ``new_runner()``, which builds a
*bare* State with an empty overlay, so FUN_1c15e3's writes (finding 06's
"Init writes") never reached a render even with ``--run-init`` on the CLI.
``new_runner(..., init=...)`` (an ``InitResult`` from ``run_init()``) now
starts from a copy of init's own post-return State instead (same
overlay/uregs/mmrs FUN_1c15e3 itself left -- see ``_clone_state`` for why a
copy, and why not ``sharc_core.state._copy()``). The bare-state path stays
the default (``init`` omitted) for comparison. ``setup_voice()`` was also
still poking every field it wrote by hand regardless of what a real trigger
sets; it now writes the documented net effect of a trigger (arm +
position/step setter -- see its own docstring) instead, and reports which
fields still have no documented trigger-path writer.

**CLI semantics corrected; THD+N re-measured on a coherent-sampling render
(lane L17, task 1/2, 2026-09-25).** See ``SOURCE_SAMPLE_RATE``'s own
docstring and ``main()``'s own comment for task 1's CLI fix (the source is
now a --freq Hz tone at the firmware's own 96 kHz sample-playback rate,
--sample-len auto-sizes from --blocks/--pitch-step via
``_default_sample_len()``, and the audible output is documented as
freq * pitch_step Hz).

Task 2 re-measured THD+N at pitch_step in {0.5, 1.0, 2.0} on a long,
*coherently-sampled* render (128 blocks = 4096 decimated output samples per
case, one block discarded up front for the startup transient, --freq chosen
per case so the *output* lands on an exact DFT bin -- 996.09375 Hz, bin 85
of 4096 at the 48 kHz decimated rate -- so a plain time-domain Parseval
split (total mean-square power minus the exact-bin fundamental's own power,
via direct sine/cosine correlation, no FFT/numpy needed) has zero spectral
leakage) instead of the previous short, truncated-prefix measurement
(-35.5 dBc at step 1.0 vs -65 dBc at 0.5 and -74 dBc at 2.0). Result: **all
three steps measure within about 0.5 dB of each other**, at -92.2 dBc
(0.5), -92.6 dBc (1.0) and -92.8 dBc (2.0) -- consistent with this
harness's own 16-bit (Q15) ``sample_format="int16"`` quantization floor at
the rendered amplitude (~0.405 out of a possible 1.0: 16-bit full-scale SNR
is ~98 dB, less ``20*log10(0.405)`` ~= 8 dB of headroom gives ~90 dB, which
is what was measured), not a real interpolation-accuracy difference at
unity step. **Step 1.0 is not "genuinely worse"; the previous -35.5 dBc
reading was a measurement artifact** -- almost certainly scalloping loss
from correlating a short, non-coherently-sampled window against an
off-bin frequency (this lane's own Parseval-split fundamental-power
estimate underestimates the true fundamental whenever the analysis window
does not hold an exact integer number of cycles, inflating the apparent
residual/noise term with no change to the actual signal), not a property
of the render itself.

This lane also checked the other half of the task 2 brief -- the
coefficient row at frac=0 (``read_coeff_table()``'s phase 0, the only row
pitch_step=1.0 with an integer ``start`` ever selects) -- against both the
static image and a post-``run_init()`` runtime read of the same address
(``profile(image).coeff_table`` = 0x25d940, confirmed identical between
the two: static six taps
``[-0.0452, 0.0478, 0.3902, 0.0629, -0.0543, 0.0033]``, so "0x25d940 has no
bytes in the static image" did not hold for this image/loader -- the
static ``LoadedMemory`` already resolves it). **Row 0 is not a pure
delay/unit impulse** (that would read as one tap at 1.0 and the rest 0); it
is an ordinary 6-tap lowpass kernel with real energy on every tap, the same
shape every other phase has -- so unity pitch_step still runs the full
polyphase filter (not an identity passthrough), but that filtering is
present at every pitch_step via its own coefficient row, not something
that singles out step 1.0 -- consistent with the near-identical THD+N
measured across all three steps above, not a competing explanation for a
difference that this lane's own re-measurement did not find.
"""

from __future__ import annotations

import argparse
import itertools
import json
import math
import os
import struct
import sys
import time
import wave
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass, replace

HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)

import sharc as sharcmod  # noqa: E402
import sharc_dac  # noqa: E402
import sharc_run as sr  # noqa: E402
import sharc_survey as sv  # noqa: E402
import sharc_symbols  # noqa: E402
import sharc_trace as st  # noqa: E402
from sharcldr import LoadedMemory  # noqa: E402

# emu/ is a sibling of tools/, not on sys.path by default -- same pattern
# tools/sharc_replay.py's own import block already uses (lane E2's file;
# not duplicated here, just the same two-line sys.path fix).
if os.path.dirname(HERE) not in sys.path:
    sys.path.insert(0, os.path.dirname(HERE))
from emu import sharc_capture  # noqa: E402

# Firmware addresses (functions, tables) come from tools/sharc_symbols.py's
# resolve(), not a hardcoded dict: cached per image so repeated calls in one
# process (render_blocks() in a loop, the test suite) reuse one resolution.
_PROFILE_CACHE: dict[str, sharc_symbols.Profile] = {}


def profile(image: str) -> sharc_symbols.Profile:
    """This image's resolved symbol table (tools/sharc_symbols.py), cached.
    Raises sharc_symbols.SymbolResolutionError if a symbol this harness
    needs (voice_render, voice_records, coeff_table, ...) does not resolve
    -- loudly, at call time, rather than silently rendering from a stale
    hardcoded address."""
    if image not in _PROFILE_CACHE:
        _PROFILE_CACHE[image] = sharc_symbols.resolve(
            sharcmod.load(image), device="dt2"
        )
    return _PROFILE_CACHE[image]


# Voice record layout: not independently-addressed symbols, so these stay
# as constants here rather than moving into sharc_symbols.py.
#
# 32 voice records, 0x1d8 bytes apart, at profile(image).voice_records
# (docs/findings/06's voice record contract, two-agent-checked on the 1.16
# bytes).
VOICE_RECORD_STRIDE = 0x1D8
VOICE_RECORD_COUNT = 32

# Voice record field offsets, bytes, from docs/findings/06's voice record
# contract table (two-agent-checked on the 1.16 bytes) plus this lane's own
# byte-level trace of FUN_1c4ecf/FUN_1c4f81 (see this module's docstring)
# for FIELD_SAMPLE_PTR, previously undocumented ("[O]" in the contract
# table) and empirically mis-set.
FIELD_SAMPLE_PTR = 0x0  # word 0: pointer to raw PCM; null = zero-fill gate
FIELD_WORK_BUFFER = 0x4  # 64 floats, written by render_body
FIELD_PREVIOUS_SAMPLE = 0x180  # word 0x60
FIELD_RATE = 0x184  # word 0x61, u32
# Word 0x62-0x63 (bytes +0x188/+0x18C): this lane's own finding, task 3.
# docs/findings/06 only documents +0x188 as an *init* write ("+0x188 =
# 0x170" -- 368, matching the ROM default sample's length) and does not
# say what a real trigger does with it. FUN_1c4f81's past-limit test reads
# it back (this lane's task brief's static facts: "combined with words
# 0x62/0x63 (+0x188/+0x18C, merged as (w63<<31)|(w62>>1))") *before* the
# END/LOOP_START-derived span, so it is compared as an independent bound,
# not derived from END. Left at init's stale 368 while ``setup_voice()``
# points the record at a differently-sized buffer, this lane's own
# ``check_correctness()`` deactivated the voice after exactly one block
# regardless of the real END/START/LOOP_START span (this module's own
# test asserts against this regression); writing FIELD_SAMPLE_LENGTH here
# to the same length ``end``/FIELD_END uses removes that spurious
# deactivation in every case this lane tried (unity, half and double
# pitch, sine and ramp) -- see check_correctness()'s docstring for the
# margin this in turn requires. Not yet run against a real trigger, so
# this is this lane's own finding, not a docs/findings/06 correction --
# record it there once a second agent checks it against the image bytes.
FIELD_SAMPLE_LENGTH = 0x188  # word 0x62, u32 (not Q31-paired with 0x18C)
FIELD_SAMPLE_LENGTH_HI = 0x18C  # word 0x63; 0 in every case seen so far
FIELD_LOOP_START = 0x190  # words 0x64-65, Q31 int64 low-first
FIELD_START = 0x198  # words 0x66-67
FIELD_END = 0x1A0  # words 0x68-69
FIELD_STEP = 0x1A8  # words 0x6a-6b, signed Q31 int64
FIELD_PHASE = 0x1B0  # words 0x6c-6d, Q31 int64
FIELD_FADE_IN = 0x17C
FIELD_ZERO_CROSS_MUTE = 0x17D
FIELD_RESEED = 0x17E
FIELD_ACTIVE = 0x1B8
FIELD_SEED_PENDING = 0x1BA
FIELD_REVERSE = 0x1BB
FIELD_LOOP = 0x1BC

# 128-phase, 6-tap polyphase coefficient table layout at
# profile(image).coeff_table: 6 (swse) int16 taps per phase, one per 4-byte
# (normal-word) slot, 24 bytes/phase (docs/findings/06's voice record
# contract, "Render and declick").
COEFF_TABLE_TAPS = 6
COEFF_TABLE_STRIDE_BYTES = 24

# The real caller's second argument, found for this lane's task 1 by
# tracing FUN_1c642a's own call sites (out/sharcdb, tools/sharc.py's
# last_def()/print_slice()): R4 is the record base (already documented,
# see the module docstring's "Corrected voice call convention"), and R12
# is *not* a magic constant -- it is DM(SESSION_TICK_ADDR) at the moment
# of the call. Evidence:
#
#   sw 0x1c6501  R13 = pass(R2); R15 = DM(I2, M5)   -- I2 resolves (ptr
#                table, tools/sharcdb.py) to SESSION_TICK_ADDR; this read
#                happens once per FUN_1c642a call, before the 32-voice
#                dispatch loop, so it is loop-invariant across every voice
#                that loop calls.
#   sw 0x1c6aee  R4 = pass(R13); R12 = R15           -- selector 2's call
#   sw 0x1c6afd  R4 = pass(R13); R12 = R15           -- the "any other"
#                call (FUN_1c4ecf, the one this harness renders).
#
# Inside the callee, this is *not* read as R12 directly where the static
# facts' "R0" note describes it -- sw 0x1c4ef5 "R2 = pass(R12); DM(I6-8) =
# R2" copies it, and sw 0x1c4ef8 "R0 = add(R2, R2)" doubles it before
# FUN_1c4ecf's own zero-fill gate even runs. That doubled value is what
# sw 0x1c4f1b spills (DM(I6-5) = R0) and FUN_1c4f81's past-limit test later
# reloads three times (sw 0x1c4fb1/0x1c4fb7/0x1c4fbc, as R12/R14/R15) --
# so "the caller's R0" in this lane's brief is this harness's R12 argument,
# doubled by the callee itself, not a third register the caller sets.
#
# SESSION_TICK_ADDR itself: FUN_1c15e3 (init) seeds it to the *literal*
# 0x20 (32) -- sw 0x1c1621 "M3 = 0x20", sw 0x1c1643 "DM(0x252d3c) = M3",
# both inside init's own function body (img.func(0x1c1643) resolves to
# FUN_1c15e3) -- not a guess: this lane's own post-init snapshot reads back
# exactly 32 there. The only other writer in the covered call graph is
# FUN_1cdbb2 (docs/findings/06: "a conditional per-track state
# accumulator", called from FUN_1c642a itself, sibling to the render
# calls) -- img.reach(profile(image).init, FUN_1cdbb2's entry) is False,
# so init alone never reaches it and this harness, which never calls
# FUN_1c642a either, has no path to it either: SESSION_TICK_ADDR is read
# fresh at every call_render() (never advanced by this harness), which is
# an open point ([O]), not a claim that a real multi-block run holds it
# fixed at 32.
#
# Without this, call_render() left R12 at whatever register value
# run_init()'s own ~1.24M instructions happened to leave it at (observed:
# 0, on this lane's post-init snapshot -- a leftover, not a 0 the firmware
# ever intended), so the callee's doubled R0 was 0 instead of 64. That
# 0-vs-64 difference is this lane's root cause for the voice deactivating
# after block 0 -- see check_correctness()'s docstring and this module's
# test coverage for the before/after.
SESSION_TICK_ADDR = 0x252D3C

Q31 = 1 << 31

# The firmware's own sample-playback rate: the rate a raw PCM sample is
# assumed to be recorded at when pitch_step=1.0 means "native speed" in the
# DO-64 render domain FUN_1c4f81's interpolation loop and 0xb80000's 2:1
# decimator operate in. Two independent 96000.0 literals in the image, both
# cited in docs/findings/06-sharc-engine-and-startup.md:
#
#   - "The voice record contract"/"Step [C][V]" (line ~3385): the four
#     step-setters (0x1c4afe, 0x1c4bf9, 0x1c4d88 and, by the same words,
#     0x1c4a31) compute ``step = ratio * rate / 96000 * 2**31`` -- ``rate``
#     is the voice record's own word 0x61 (FIELD_RATE, "u32", "not checked"
#     in the contract table but read by exactly these three setters), so
#     96000 is the reference this per-voice sample rate is normalized
#     against to land in the same Q31 units as ``pitch_step`` here. This is
#     the direct evidence: it is the same voice-render engine's own step
#     computation, not an unrelated filter.
#   - A classifier false-positive investigation (line ~2152) independently
#     found the float immediates pi, pi/8 and 96000.0 together in a
#     different function (blk93@0x1c5ed4, a `2*pi*f/fs` filter-coefficient
#     calculator elsewhere in the DSP chain) and concluded "The sample rate
#     is 96 kHz" from that alone -- corroborating, but not this constant's
#     primary evidence (that function is not part of the voice-render path
#     this harness drives).
#
# Combined with 0xb80000's own documented 2:1 decimation ("Voices [V]":
# "`0xb80000` (state +0x104) decimates 2:1 to 32 outputs"), the DO-64
# interpolation domain runs at 96 kHz and this harness's own decimated
# output (decimate()/write_wav(), WriteWavTest's 48000 default) is 48 kHz --
# consistent, not a coincidence: write_wav()'s ``sample_rate`` default was
# already 48000 before this lane's task 1, it was just never derived from
# this constant. See main()'s own CLI semantics fix (task 1, 2026-09-25)
# for why this constant exists: the previous CLI generated the source PCM
# at ``pitch_step``-prescaled cycles/sample instead of at this fixed rate,
# so its ``--freq`` argument did not mean "the source is a --freq Hz tone"
# at all -- see main()'s own comment for the corrected math and the
# measured before/after.
SOURCE_SAMPLE_RATE = 96000.0


def load_image_memory(image: str):
    """The same LoadedMemory sharc_run.py's CLI reads from."""
    return sr._load_image_memory(image)


def _make_runner(
    memory: LoadedMemory,
    image: str,
    start: int,
    regs: Mapping[str | int, int | str],
    *,
    provisional_forms: Sequence[str] = (),
    provisional_interpretations: Mapping[str, str] | None = None,
) -> sr.Runner:
    """One place for the Runner options this harness always turns on: state
    that a real boot leaves at reset (0) rather than Unknown, and the numeric
    recips model (this render path's pitch/rate math divides).

    ``provisional_forms``/``provisional_interpretations`` default to empty
    (unchanged behaviour: an uncertain-decode or no-semantics form still
    halts) -- passed straight through to ``sr.Runner`` (see its own
    docstring, and ``sharc_core/state.py``'s ``State.provisional_forms``/
    ``State.provisional_interpretations``) for a caller that wants to opt
    into treating a specific, already-confirmed-decode-but-no-semantics
    form (e.g. ``21p_undoc16`` at 0x1c32b0, this repo's scratchpad
    STATE.md's "opt-in provisional interpretation") as a no-op instead of
    stopping the run -- lane E1's own real-frame-bytes renders hit exactly
    that form from frame 1 onward (a genuinely new fork, not present with
    an empty synthetic frame); see ``render_frames_to_ring_a()``'s own
    docstring for how this harness surfaces the same opt-in."""
    return sr.Runner(
        memory,
        start,
        regs=regs,
        explicit_memory_model=True,
        approx_recips=True,
        follow_loaded_calls=True,
        max_call_depth=64,
        provisional_forms=provisional_forms,
        provisional_interpretations=provisional_interpretations,
    )


def voice_record_address(image: str, voice: int) -> int:
    if not 0 <= voice < VOICE_RECORD_COUNT:
        raise ValueError("voice must be 0..%d" % (VOICE_RECORD_COUNT - 1))
    return profile(image).voice_records + voice * VOICE_RECORD_STRIDE


@dataclass
class InitResult:
    ran: bool
    result: sr.RunResult | None
    error: str | None
    # The Runner FUN_1c15e3 actually ran in, at its own post-return State
    # (same overlay/uregs/mmrs init itself left) -- present only when
    # ``ran`` is true. ``new_runner(..., init=this)`` hands out a *copy* of
    # this runner's state (see ``_clone_state``) so one run_init() can seed
    # several independent renders (render_blocks(), each check_correctness()
    # case) instead of each rebuilding the ~1.24M-instruction init run.
    runner: sr.Runner | None = None


def run_init(
    memory,
    image: str,
    *,
    max_steps: int = 2_000_000,
    provisional_forms: Sequence[str] = (),
    provisional_interpretations: Mapping[str, str] | None = None,
) -> InitResult:
    """Attempt FUN_1c15e3 (docs/findings/06: no arguments) to its return.

    On the 1.16 image it returns after about 1.24 million instructions.
    ``ran`` is true only when the function returned; reaching MAX_STEPS
    first is a failure, not a partial success. On success, the returned
    ``InitResult.runner`` is the same Runner init ran in -- its ``.state``
    is what a real boot leaves before any voice is triggered (see finding
    06's "Init writes": every voice's word +0 is the R8 argument, +0x1b8 is
    0). Pass this to ``new_runner(..., init=...)`` to render from it instead
    of a bare state. ``render_frames()``/``render_frames_to_ring_a()`` use
    ``init.runner`` directly (not ``new_runner()``), so
    ``provisional_forms``/``provisional_interpretations`` given here (see
    ``_make_runner``'s own docstring) apply to every frame call those
    functions make afterward, not just to init's own run.
    """
    runner = _make_runner(
        memory,
        image,
        profile(image).init,
        {"I6": 0x300000},
        provisional_forms=provisional_forms,
        provisional_interpretations=provisional_interpretations,
    )
    try:
        result = runner.run(max_steps=max_steps)
    except Exception as exc:  # pragma: no cover - defensive, see docstring
        return InitResult(False, None, str(exc))
    ok = result.halt.reason == "return without followed call"
    return InitResult(
        ok,
        result,
        None
        if ok
        else "%s at %#x (%s)"
        % (result.halt.reason, result.halt.pc_sw, result.halt.form),
        runner if ok else None,
    )


def _write_q31_pair(state, address: int, value: int) -> None:
    """A signed Q31 int64 (sample position, in either loop-start/start/end
    or step/phase's Q31 units), low word first (docs/findings/06)."""
    raw = value & 0xFFFFFFFFFFFFFFFF
    low, high = raw & 0xFFFFFFFF, (raw >> 32) & 0xFFFFFFFF
    _poke(state, address, low)
    _poke(state, address + 4, high)


def _poke(state, address: int, value: int, width: int = 4) -> None:
    ok = st._dm_write(state, address, width, st.Const(value & ((1 << (8 * width)) - 1)))
    if not ok:
        raise ValueError(
            "poke at %#x (width %d) did not take effect" % (address, width)
        )


def _read_q31_pair(state, address: int) -> int:
    """The inverse of ``_write_q31_pair``: a signed Q31 int64, low word
    first. Unwritten (explicit_memory_model) reads as 0."""
    lo = st._dm_read(state, address, 4)
    hi = st._dm_read(state, address + 4, 4)
    low = lo.value & 0xFFFFFFFF if lo is not None else 0
    high = hi.value & 0xFFFFFFFF if hi is not None else 0
    raw = (high << 32) | low
    if raw & (1 << 63):
        raw -= 1 << 64
    return raw


# Bytes between successive encoded samples in DM, by sample_format --
# *not* the same as the number of bytes this lane actually writes per
# sample (see ``_SAMPLE_WRITE_WIDTH_BYTES``).
#
# **Corrected (2026-09-25) after tools/sharc_core/forms_dag.py's Type 7a
# MODIFY fix (w/l honoured, w=bit39, l=bit23) landed**: every MODIFY in
# this loop is (sw) (see the module docstring's wave-2 note), so
# sw 0x1c50af's ``modify(I4, M3)`` now steps I4 in *short-word* (2-byte)
# units, not 4. A concrete, register-level single-step of FUN_1c4f81's DO
# 64 loop against the fixed decoder (this lane's own scratch trace) shows
# I4 landing at 0x310000, 0x310002, 0x310004, ... for successive raw-tap
# reads within one iteration -- packed int16 PCM, two bytes apart, not one
# per 4-byte slot. The previous "4 bytes apart" claim on file here was
# measured *before* the MODIFY fix, when the (sw)/(nw) distinction was not
# yet honoured and every MODIFY silently scaled as if raw-byte, landing 4
# bytes apart by coincidence (still a smoothly-varying, plausible-looking
# int16 run -- not garbage -- which is why the wrong stride survived
# uncaught). It also now matches ``read_coeff_table()``'s own
# already-validated convention for the polyphase table stride (24
# bytes/phase, 6 taps): that address math goes through the *byte-addressed*
# ``memory.read()`` path directly, never through a (sw) MODIFY, so it was
# never affected by this bug either way.
_SAMPLE_STRIDE_BYTES = {"float32": 4, "int16": 2}
# Bytes actually written at each sample's own address: "int16" writes only
# the low 2 bytes of its 4-byte slot (a plain short-word poke); the slot's
# high 2 bytes are left unwritten (explicit_memory_model: reads back 0),
# matching what a short-word-sign-extended *read* of that same address
# looks at -- the DO 64 loop's own reads never touch the high half.
_SAMPLE_WRITE_WIDTH_BYTES = {"float32": 4, "int16": 2}


def _sample_to_word(value: float, sample_format: str) -> int:
    """Encode one synthetic sample as the raw DM word this harness writes
    at ``sample_base`` -- what FUN_1c4f81's DO 64 loop reads through
    record+0 as PCM (see ``_SAMPLE_STRIDE_BYTES``'s docstring for the
    address-space reasoning).

    ``"int16"`` (this lane's own finding, now the default -- see
    ``_SAMPLE_STRIDE_BYTES``) packs VALUE as a signed Q15 fixed-point word
    (round(value * 32768), clamped to the int16 range, masked to 16 bits):
    the same (swse) int16 / 2**15 convention ``read_coeff_table()`` already
    uses for the polyphase taps (docs/findings/06's "Render and declick"),
    which this lane's static facts also give for the raw sample reads
    themselves ("short-word sign-extended (int16)", sw 0x1c50b4 etc).

    Confirmed empirically, not just by the decode-table type name: with
    ``"float32"`` (this harness's previous default) at a 4-byte stride, a
    rendered block's values had no resemblance to the input sine at all.
    Switching the encoding alone to this Q15 int16 packing was necessary
    but not sufficient: a large mismatch against ``reference_render()``
    remained even after this fix and ``read_coeff_table(phases=256)`` --
    tracked down (this lane, 2026-09-25) to ``read_coeff_table()`` itself
    reading the wrong half of each 4-byte coefficient slot (see its own
    docstring), not to ``tools/sharc_core``'s MR-accumulator housekeeping
    or ``float_by``, both of which a register-level trace confirmed
    correct (``alu_float_convert_scaled``'s rx/ry operand order matches the
    raw instruction bytes exactly at sw 0x1c50e3 -- rn=2, rx=2, ry=6 --
    contradicting an earlier lane's manual byte decode that had rx and ry
    swapped; no fix was needed there).

    ``"float32"`` packs VALUE as an IEEE-754 float word instead -- kept
    only for an explicit before/after comparison (see the module's tests),
    not because any evidence supports it as the real format.
    """
    if sample_format == "float32":
        return struct.unpack("<I", struct.pack("<f", value))[0]
    if sample_format == "int16":
        quantized = max(-32768, min(32767, int(round(value * 32768.0))))
        return quantized & 0xFFFF
    raise ValueError("unknown sample_format %r" % (sample_format,))


def _dequantize_sample(value: float, sample_format: str) -> float:
    """The float value actually stored (and later read back) after
    ``_sample_to_word(value, sample_format)`` -- the inverse quantization,
    used to build ``reference_render()``'s input from exactly what the
    emulated read will see, so ``check_correctness()`` measures
    interpolation-math error rather than format-quantization noise on top
    of it. ``"float32"`` round-trips through the same IEEE-754 packing
    (a no-op for any value already representable, which every
    ``_make_test_input()`` value is)."""
    word = _sample_to_word(value, sample_format)
    if sample_format == "float32":
        return struct.unpack("<f", struct.pack("<I", word))[0]
    if sample_format == "int16":
        signed = word - 0x10000 if word & 0x8000 else word
        return signed / 32768.0
    raise ValueError("unknown sample_format %r" % (sample_format,))


def _write_samples(
    state, base: int, values: Sequence[float], sample_format: str = "int16"
) -> None:
    stride = _SAMPLE_STRIDE_BYTES[sample_format]
    width = _SAMPLE_WRITE_WIDTH_BYTES[sample_format]
    for i, value in enumerate(values):
        _poke(
            state, base + i * stride, _sample_to_word(value, sample_format), width=width
        )


def _read_byte(state, address: int) -> int:
    raw = st._dm_read(state, address, 1)
    return raw.value & 0xFF if raw is not None else 0


def _clone_state(state: st.State) -> st.State:
    """A copy of STATE safe to run forward independently of the original --
    used so one ``run_init()`` (~1.24M instructions) can seed several
    independent renders (``render_blocks()``, each ``check_correctness()``
    case) without one's writes leaking into the next.

    Deliberately not ``sharc_core.state._copy()``: that helper (used by the
    symbolic tracer's own forks) builds its ``State(...)`` with positional
    args that stop before the ``explicit_memory_model`` field, so it always
    resets a clone to ``explicit_memory_model=False`` regardless of the
    source -- silently turning "unwritten DM reads as 0" back into
    "unwritten DM reads as Unknown" for every clone, which is exactly the
    thing that would make a post-init render fork/halt where a bare one
    would not. ``dataclasses.replace`` keeps every scalar config field
    (``explicit_memory_model``, ``approx_recips``, ``follow_loaded_calls``,
    ...) as the source had it, and only the mutable containers a step can
    write through are given fresh copies.
    """
    return replace(
        state,
        uregs=dict(state.uregs),
        trace=[dict(event) for event in state.trace],
        overlay=dict(state.overlay),
        call_stack=list(state.call_stack),
        pc_stack=list(state.pc_stack),
        loops=list(state.loops),
        status_stack=list(state.status_stack),
        mmrs=dict(state.mmrs),
        special=dict(state.special),
        bank_alt=dict(state.bank_alt),
    )


def setup_voice(
    state,
    image: str,
    voice: int,
    *,
    sample_len: int,
    pitch_step: float = 1.0,
    start: int = 0,
    end: int | None = None,
    loop_start: int = 0,
    loop: bool = False,
    reverse: bool = False,
    sample_base: int = 0x310000,
) -> int:
    """Write one voice record's fields per docs/findings/06's voice record
    contract (plus FIELD_SAMPLE_PTR, this lane's own finding -- see this
    module's docstring), and return its address. ``pitch_step`` is in input
    samples per output (pre-interpolation) step; 1.0 is unity speed.
    ``sample_base`` is the raw-PCM address stored at record+0 -- the caller
    is responsible for having written samples there (``call_render`` no
    longer pokes a register for this; see the docstring's "Corrected voice
    call convention").

    This writes the net effect finding 06's "Flags and seed" documents for
    a genuine trigger (the arm ``0x1c4eaf`` -- ACTIVE=1, SEED_PENDING=1 --
    immediately followed by one of the four position/step setters, which
    tests SEED_PENDING, clears it, and seeds PHASE to ``start`` or
    ``end - 1`` sample when REVERSE is set): ACTIVE, SEED_PENDING (ends
    cleared), REVERSE, LOOP, STEP, START/END/LOOP_START and PHASE are all
    set here from a documented writer. ``loop``/``reverse`` default to
    False/False, matching what an unset (explicit_memory_model) record
    would read as -- pass them explicitly once a real trigger's argument
    values are known.

    Fields this harness still has no documented trigger-path writer for --
    FIELD_SAMPLE_PTR (finding 06: "[O] the firmware writer of word +0 for a
    playing voice is not yet found"), FIELD_SAMPLE_LENGTH (this lane's own
    finding -- see its docstring), fade-in/zero-cross-mute/reseed (the
    contract table marks their writer "not checked"), and previous-sample
    (documented writer is the render function ``0x1c4f81`` itself, not a
    setter) -- are still poked here to a fixed value (sample_base; sample_len;
    0; 0) rather than left to whatever ``run_init``'s own FUN_1c7442 wrote,
    so a render from init state is comparable to one from bare state
    instead of differing for reasons unrelated to the trigger. This is a
    deliberate hand-poke, not a claim about what the real trigger writes
    there.
    """
    record = voice_record_address(image, voice)
    end = sample_len if end is None else end
    _poke(state, record + FIELD_SAMPLE_PTR, sample_base)
    _poke(state, record + FIELD_SAMPLE_LENGTH, sample_len)
    _poke(state, record + FIELD_SAMPLE_LENGTH_HI, 0)
    _poke(state, record + FIELD_ACTIVE, 1, width=1)
    _poke(state, record + FIELD_SEED_PENDING, 0, width=1)
    _poke(state, record + FIELD_REVERSE, 1 if reverse else 0, width=1)
    _poke(state, record + FIELD_LOOP, 1 if loop else 0, width=1)
    _poke(state, record + FIELD_FADE_IN, 0, width=1)
    _poke(state, record + FIELD_ZERO_CROSS_MUTE, 0, width=1)
    _poke(state, record + FIELD_RESEED, 0, width=1)
    _write_q31_pair(state, record + FIELD_LOOP_START, loop_start << 31)
    _write_q31_pair(state, record + FIELD_START, start << 31)
    _write_q31_pair(state, record + FIELD_END, end << 31)
    _write_q31_pair(state, record + FIELD_STEP, round(pitch_step * Q31))
    _write_q31_pair(
        state, record + FIELD_PHASE, ((end - 1) if reverse else start) << 31
    )
    _poke(state, record + FIELD_PREVIOUS_SAMPLE, 0)
    return record


def new_runner(
    memory: LoadedMemory,
    image: str,
    *,
    stack_base: int = 0x300000,
    init: InitResult | None = None,
) -> sr.Runner:
    """A Runner parked at profile(image).voice_render, ready for repeated
    calls through ``call_render`` -- one State/overlay is shared across
    those calls (a Runner's own State is otherwise created once and never
    reset; see ``call_render``'s docstring for why a *fresh* Runner per
    call, tried first, does not work: each Runner's State starts from an
    empty overlay, so a second Runner never sees the first's
    ``setup_voice()`` pokes).

    With ``init`` omitted (the default), the returned Runner's State is
    bare: every field reset-zero (``State.explicit_memory_model``) until
    ``setup_voice()`` pokes it -- this is the "bare-state" path kept for
    comparison against a real boot. With ``init`` given (an ``InitResult``
    from ``run_init()`` with ``ran=True``), the returned Runner starts from
    a *copy* of init's own post-return State (see ``_clone_state``): same
    overlay/uregs/mmrs FUN_1c15e3 itself left, so ``setup_voice()`` and
    ``call_render()`` see whatever init actually wrote (finding 06's "Init
    writes") for fields this harness does not poke, instead of a blank
    overlay silently discarding it. ``init.runner``/``init.runner.state``
    are never mutated by this -- ``run_init()`` can be called once and its
    ``InitResult`` handed to several ``new_runner(..., init=...)`` calls
    (``render_blocks()``, each ``check_correctness()`` case) independently.
    """
    if init is not None:
        if not init.ran or init.runner is None:
            raise ValueError(
                "new_runner: init did not complete (%s); cannot start a "
                "render from it" % init.error
            )
        runner = _make_runner(
            memory, image, profile(image).voice_render, {"I6": stack_base}
        )
        runner.state = _clone_state(init.runner.state)
        return runner
    return _make_runner(memory, image, profile(image).voice_render, {"I6": stack_base})


def call_render(
    runner: sr.Runner, image: str, record: int
) -> tuple[sr.RunResult, list[float]]:
    """Call FUN_1c4ecf the way FUN_1c642a's dispatch loop calls it -- R4 =
    the record address itself, not record+4 (see this module's docstring's
    "Corrected voice call convention") -- on RUNNER's already-set-up State
    (see ``new_runner``/``setup_voice``), and return (RunResult, the 64
    work-buffer floats).

    Resets RUNNER's control-flow state (pc_sw, call stack, loops, pending
    delay slot) as a fresh call needs, but deliberately keeps its DM
    overlay (the voice record ``setup_voice`` wrote) and every other
    register (the callee saves and restores the ones it uses -- see this
    module's docstring's trace of FUN_1c4ecf's own prologue/epilogue), so
    calling this again for the next block is a closer match to what
    FUN_1c642a's own dispatch loop does than starting over from
    ``new_runner`` would be.

    The raw sample pointer is not a register here (see ``setup_voice``'s
    ``sample_base``): it is read by the callee from record+0
    (FIELD_SAMPLE_PTR), a persistent record field, not reset per call.

    R4 is the record address (the real call convention -- see this
    module's docstring). R12 is the real caller's second argument too (see
    ``SESSION_TICK_ADDR``'s docstring): read fresh from DM every call
    (never advanced by this harness -- an open point, not a claim that a
    real multi-block run holds it fixed) instead of left at whatever
    register value the previous run (``run_init()`` or a prior
    ``call_render()``) happened to leave R12 at.

    **Loop-wrap ACTIVE (task 3, 2026-09-25; fixed at the source by lane
    W1, 2026-09-25).** docs/findings/06 claims "a wrap copies +0x1bc
    [FIELD_LOOP] to +0x1b8 [FIELD_ACTIVE]" (0x1c50fe forward, 0x1c530a/
    0x1c5325 reverse). The wrap's read at 0x1c50fb/0x1c5322
    (``R2 = DM(I1, M6)``, a Type3d instruction, fields l=0, x=0, w=0,
    ex=0) used to be misdecoded by ``tools/sharc_core/forms_move.py``'s
    ``_type_3d``, which hardcoded ``access_width = "normal-word"`` for
    its w=0/ex=0 branch instead of consulting ``ACCESS_WIDTHS[(l, x, 0)]``
    the way the sibling Type4d store at the very same site already did --
    see that function's own docstring for the PRM citation and the
    register-override confirmation. With that fixed, this run's own
    Type3d load now performs the correct 1-byte read, and ACTIVE tracks
    FIELD_LOOP across a wrap on its own, with no harness-side fixup
    needed; no ``call_render()`` code doctors ACTIVE any more.
    """
    from sharc_core.encoding import UREG_CODES

    state = runner.state
    state.pc_sw = profile(image).voice_render
    state.stopped = None
    state.pending = None
    state.call_stack = []
    state.loops = []
    state.status_stack = []
    state.at_loaded_entry = False
    tick = st._dm_read(state, SESSION_TICK_ADDR, 4)
    state.uregs[UREG_CODES["R4"]] = st.Const(record)
    state.uregs[UREG_CODES["R12"]] = st.Const(
        tick.value & 0xFFFFFFFF if tick is not None else 0
    )
    runner.instructions = 0
    runner.form_counts.clear()
    runner.max_call_depth_reached = 0

    result = runner.run(max_steps=20_000)
    floats = []
    for i in range(64):
        raw = st._dm_read(state, record + FIELD_WORK_BUFFER + i * 4, 4)
        floats.append(
            struct.unpack("<f", struct.pack("<I", raw.value & 0xFFFFFFFF))[0]
            if raw is not None
            else 0.0
        )

    return result, floats


def render_blocks(
    memory,
    image: str,
    voice: int,
    n_blocks: int,
    *,
    pitch_step: float = 1.0,
    sample_len: int = 4096,
    sample_base: int = 0x310000,
    samples: Sequence[float] | None = None,
    sample_format: str = "int16",
    init: InitResult | None = None,
) -> tuple[list[list[float]], list[sr.RunResult]]:
    """N call_render() calls against one freshly set-up voice record,
    sharing a single Runner/State (see ``new_runner``/``call_render``) so
    the record's own persistent fields (phase, previous sample, ...) carry
    over between calls the way they would across real render-loop
    invocations -- see ``check_correctness()`` for this measured against an
    independent reference, including phase continuity across blocks.

    ``samples``, if given, is written to ``sample_base`` before the first
    call (FIELD_SAMPLE_PTR then points at it), e.g. a synthetic sine for an
    audible WAV, encoded per ``sample_format`` (see ``_sample_to_word``). If
    omitted, the record still points at ``sample_base`` but nothing is
    written there, so unwritten DM reads as 0 (State.explicit_memory_model):
    a silent (but not zero-fill-gated, since FIELD_SAMPLE_PTR is still
    non-null) render, useful for an instructions/block measurement alone.

    ``init``, if given (an ``InitResult`` from ``run_init()``), starts the
    render from a copy of init's own post-return state instead of a bare
    one -- see ``new_runner``.
    """
    runner = new_runner(memory, image, init=init)
    state = runner.state
    if samples is not None:
        _write_samples(state, sample_base, samples, sample_format)
    record = setup_voice(
        state,
        image,
        voice,
        sample_len=sample_len,
        pitch_step=pitch_step,
        sample_base=sample_base,
    )
    blocks, results = [], []
    for _ in range(n_blocks):
        result, floats = call_render(runner, image, record)
        blocks.append(floats)
        results.append(result)
    return blocks, results


# --- Frame render entry point (lane L13, 2026-09-25) ------------------
#
# render_blocks()/call_render() above call FUN_1c4ecf (one voice's render)
# directly. This section instead calls FUN_1c2b24 (render_frame, the whole
# frame's orchestrator -- docs/findings/06's "FUN_1c2b24 is the per-frame
# render orchestrator") the way the firmware actually reaches it: through
# block_handler (0x1c74cd), the audio task's real command dispatcher,
# rather than jumping into render_frame bare with hand-set R4/R12/R8/stack
# registers. Concretely tracing the real dispatch (this lane's own
# byte-level read of block_handler/FUN_1c75d8's cmd_handler_3 body, cross-
# checked by running it) found register plumbing that a purely static
# read of "the call site" (this lane's own task brief) could not resolve
# from FUN_1c75d8 alone: R12 at the CALL 0x1c2b24 site is `I3`, and I3 is
# never assigned anywhere in cmd_handler_3's own body or in the dispatch
# preamble (0x1c7501-0x1c7521) that runs before it -- it is `DM(I6-3)`,
# one of *two* by-reference outputs command_dispatch_fn (0x1c778a) writes
# into block_handler's own stack frame (`R4=&(I6-3)`, `R8=&(I6-2)` passed
# into that CALL at 0x1c74f8); the *other* output, `DM(I6-2)`, is exactly
# the "R1 = DM(I6-2) of the task frame" this lane's brief already named.
# Getting both right by hand (let alone the ring-buffer/`command_word`
# plumbing above them) is exactly the kind of bookkeeping a *real* call
# gets for free -- so `setup_frame()` only pokes the three DM cells that
# select which of the 4 audio-task commands runs (docs/findings/06's
# "Task loop": `command_word`/`command_word_shift_src`/`ring_flag`), and
# `call_frame()` lets the concrete run compute everything else, the same
# way `run_init()` already does for FUN_1c15e3.
#
# Reaching render_frame this way (from a run_init() Runner, one voice
# already set up via setup_voice()) lands on the *same three* stops this
# lane's task brief already knew from a bare `--root 0x1c2b24` run
# (0xb88e4b, 0x1c4969, 0x1c088e) -- strong cross-check that they are real
# properties of an (almost) empty 16-track frame, not artifacts of calling
# render_frame bare. See FRAME_STOPS below and this lane's handover for
# the full stops table (real source found, or reported as a tools/
# sharc_core gap this lane must not fix itself).


def setup_frame(state, image: str, *, command: int = 3, ring_flag: int = 0) -> int:
    """Poke the audio task's command dispatch (docs/findings/06's "Task
    loop") so block_handler's own command-table jump selects `command`
    (3 = "renders from 0x1c7671", the only one that reaches render_frame)
    with ring half `ring_flag`, and return `profile(image).block_handler`
    -- the address `call_frame()` should `fresh_call()` at.

    `command_word_shift_src` (`DM(0x261ca4)`, the shift block_handler folds
    into the command word's address -- "0x1c7578..0x1c7586" in
    docs/findings/06) is left at 0 so the command word's address is
    `command_word` itself, with no per-track offset.

    This does not touch a voice record; call `setup_voice()` (or
    `render_frame()`'s own default) first on the same STATE so render_frame
    finds at least one active voice.
    """
    p = profile(image)
    _poke(state, p.command_word_shift_src, 0)
    _poke(state, p.command_word, command)
    _poke(state, p.ring_flag, ring_flag)
    return p.block_handler


def call_frame(
    runner: sr.Runner,
    image: str,
    *,
    patch_table: sv.PatchTable | None = None,
    max_steps: int = 4_000_000,
) -> tuple[sr.Runner, sr.RunResult]:
    """`Runner.fresh_call()` at `profile(image).block_handler` (see
    `setup_frame()`) on RUNNER's current State (typically a `run_init()`
    Runner with `setup_voice()`/`setup_frame()` already applied to its
    State) -- the real firmware call chain: block_handler ->
    command_dispatch_fn -> cmd_handler_3 -> render_frame -> ... -- instead
    of calling render_frame bare with hand-set registers (see this
    section's own module-level note for why that matters here).

    `patch_table`, if given, is `tools/sharc_survey.py`'s PatchTable
    convention (`{pc: [(kind, target, value), ...]}`), applied every time
    its pc recurs via `sharc_survey.run_with_patches()` -- this lane's own
    hypotheses for the stops an (almost) empty synthetic frame hits belong
    there (see FRAME_PATCH_TABLE), not hardcoded in this function. Without
    one, this is a plain `Runner.run()`: it stops at the first fork/mmr/
    return exactly like any other survey root.

    Returns (the fresh_call Runner, its RunResult) -- the Runner stays
    positioned at the halt (or at the return) for a caller to inspect
    registers/memory afterward (e.g. dumping the master mix buffer).
    """
    p = profile(image)
    new_runner = runner.fresh_call(p.block_handler, diagnose_unknown=True)
    if not patch_table:
        return new_runner, new_runner.run(max_steps=max_steps)
    start_pc_sw = new_runner.state.pc_sw
    t0 = time.perf_counter()
    halt = sv.run_with_patches(new_runner, patch_table, max_steps)
    elapsed = time.perf_counter() - t0
    result = sr.RunResult(
        halt=halt,
        instructions=new_runner.instructions,
        elapsed=elapsed,
        form_counts=new_runner.form_counts,
        start_pc_sw=start_pc_sw,
        final_pc_sw=new_runner.state.pc_sw,
        max_call_depth_reached=new_runner.max_call_depth_reached,
        watch_log=new_runner.watch_log,
    )
    return new_runner, result


def call_frame_collect_all(
    runner: sr.Runner,
    image: str,
    *,
    patch_table: sv.PatchTable | None = None,
    max_steps: int = 4_000_000,
    img: sharcmod.Image | None = None,
    watchpoints: Sequence[sr.Watchpoint] = (),
) -> tuple[sr.Runner, sv.CollectAllResult]:
    """Like `call_frame()`, but drives the call with
    `tools/sharc_survey.py`'s `run_collect_all()` instead of
    `run_with_patches()`: every stop this frame call passes is recorded
    (category, pc, form, unknowns), not only the first -- the same "collect
    every blocker on one path" behaviour `sharc_survey`'s own CLI gives a
    bare root, applied to a real frame call through `block_handler` instead.
    `patch_table` still resolves a fork the same way (a `"reg"`/`"mem"`
    entry unconditionally, before the instruction executes); a fork with no
    matching entry is continued through with the documented default
    (not-taken) rather than stopping the whole replay, so one frame's own
    report is a full list of what it depended on instead of only the
    nearest one.

    `watchpoints`, if given, are attached to the fresh_call Runner before it
    runs (`Runner.attach_watchpoints()` -- not carried over from `runner`
    itself, since `fresh_call()` never carries watchpoints either): a
    caller wanting to know which pc, if any, wrote a given DM range during
    this one frame call reads them back off the returned Runner's own
    `watch_log` (`sr.Runner.watch_log`), which `fresh_call()` always starts
    empty.

    Returns (the fresh_call Runner, positioned at the terminal stop, its
    `sv.CollectAllResult`) -- `result.terminal` is the same kind of Halt
    information `call_frame()`'s own `RunResult.halt` would have stopped
    at first.
    """
    p = profile(image)
    new_runner = runner.fresh_call(p.block_handler, diagnose_unknown=True)
    if watchpoints:
        new_runner.attach_watchpoints(list(watchpoints))
    result = sv.run_collect_all(new_runner, patch_table or {}, max_steps, img=img)
    return new_runner, result


def scan_voice_active(
    state, image: str, *, exclude: Sequence[int] = ()
) -> dict[int, int]:
    """The ACTIVE byte (`FIELD_ACTIVE`, record `+0x1b8`) of every voice
    record except `exclude`, keyed by voice index -- for telling a
    hand-set-up voice (this harness's own `setup_voice()`) apart from one
    the firmware itself activated while a replay ran. Returns only entries
    that read non-zero; an empty dict means no *other* voice was ever
    marked ACTIVE."""
    active = {}
    for voice in range(VOICE_RECORD_COUNT):
        if voice in exclude:
            continue
        record = voice_record_address(image, voice)
        value = _read_byte(state, record + FIELD_ACTIVE)
        if value:
            active[voice] = value
    return active


# --- The companding record (lane F2, 2026-09-26) --------------------------
#
# docs/findings/06's "Lane E2" traces the never-taken machine-type-compare
# branch (the one that would call FUN_1c60a2) back to `I5 = DM(0x254d78)`
# reading a null pointer, itself written (as one of four words,
# 0x254d78/80/88/90) by `render_frame` (FUN_1c2b24, sw 0x1c2c5e-0x1c2c67)
# from `M9 = DM(I6-18)` -- render_frame's own saved copy of `R1`, the SECOND
# by-reference output `command_dispatch_fn` (0x1c778a) writes into
# block_handler's stack frame before its CALL into render_frame (the FIRST
# being `command_word` itself, `DM(I6-3)`, per docs/findings/06's "Frame
# call path"). This lane's own task: find where `command_dispatch_fn` gets
# THIS pointer from, and whether it can be set up "the firmware's way".
#
# **Traced to a fixed, shift-selected table, not the SPI-slave control
# block at 0x261a10 (lane D2) and not a ring.** Reading
# `command_dispatch_fn`'s own raw bytes (sw 0x1c778a-0x1c77b0) end to end:
# it writes `DM(I6-3) = command_word + (DM(command_word_shift_src) << 12)`
# (the SAME formula docs/findings/06 already gives for the address
# block_handler reads ITS OWN pending command from -- `DM(I6-3)` literally
# IS the address of the command word, not a separate "ring C") and, right
# after, `DM(I6-2) = command_record_table + (DM(command_word_shift_src) <<
# 12)` -- a DIFFERENT, previously-unnamed base (`0x266220` on DT2 1.16, now
# `sharc_symbols.SYMBOLS`'s own `command_record_table`, resolved the same
# way `command_word` is: `LiteralAt('command_dispatch_fn', 0x1d,
# form='17a')`). With this harness's own `setup_frame()` (which always
# leaves `command_word_shift_src` at 0, per that function's own docstring),
# `command_record_table`'s shift is 0, so the record address is simply
# `command_record_table` itself -- confirmed by execution (a breakpoint at
# `0x1c2c51`, `I1 = M9`, reads `I1 == 0x266220` exactly).
#
# **The table is empty, and this lane found no writer of it anywhere
# reached from `run_init()` or a real capture-driven frame call.**
# `img.writers()` over `[0x266220, 0x266260)` returns only `base_only`
# (unresolved-offset) hits from `FUN_1c80f2`'s own unrelated master-mix
# ramp/crossfade loop (a `DO`-loop with a runtime-only trip count the tool
# can't bound, so it conservatively reports every address in the whole
# image as a possible target of that ONE loop -- confirmed by reading
# `FUN_1c80f2` itself, entry 0x1c80f2, called from `render_frame`'s own
# post-master-mix cleanup per docs/findings/06's "Master stage": not a real
# writer of this table). This corrects/pins down E2's own "no writer of a
# non-null value ... found" conclusion with the record's exact, now-named
# address, rather than the unresolved `M9`/`DM(I6-18)` chain E2 could not
# trace further. `command_word_shift_src` itself (the table's own selector)
# has exactly one writer, `FUN_1c77b4` (sw 0x1c77b4-0x1c77c8, `has_static_
# caller: 0` -- confirms docs/findings/06's own "no static caller" note),
# gated by a runtime condition (`JUMP IF NOT SZ` on `DM(I6,M7)`) this
# harness's own `setup_frame()` never drives, so even the SHIFT is not
# something a synthetic frame call exercises realistically.
#
# **Net for this lane's own task:** the record is very likely populated the
# same way the RX frame itself is (docs/findings/06: "The ColdFire frame is
# mapped into SHARC DM at 0x2558dc") -- written into DM directly by a DMA
# transfer this project's SHARC-side tracing cannot see (no SHARC store
# instruction needs to exist for a host-mapped region), OR by a SHARC
# "session start"/note-on preparation routine this project has never
# executed (not part of `FUN_1c15e3`, and not reached by any capture this
# lane or lane E2 replayed). Neither could be confirmed or ruled out within
# this lane's own scope; report, per this lane's own task brief, rather
# than fabricate a value for it (the same call E2 already made for
# `FUN_1c4eaf`'s own trigger site). `companding_record_address()` and
# `companding_record_fields()` below are the reusable, exact form of this
# finding for a future lane to build on (e.g. once a real writer is found,
# poking the SAME four fields these read would let `FUN_1c2b24`'s own
# companding loop reach `FUN_1c60a2` for the first time).

COMPANDING_RECORD_FIELD_OFFSETS: tuple[int, ...] = (
    8,
    16,
    24,
    32,
)  # bytes: +2,+4,+6,+8 words
COMPANDING_RECORD_TARGETS: tuple[int, ...] = (0x254D78, 0x254D80, 0x254D88, 0x254D90)


def _dm_word(state, address: int) -> int:
    """A plain 32-bit `DM` read, unwrapped from `Const` -- unwritten
    (`explicit_memory_model`) reads as 0, same convention as `_read_byte`/
    `_read_q31_pair` above."""
    raw = st._dm_read(state, address, 4)
    return raw.value & 0xFFFFFFFF if raw is not None else 0


def companding_record_address(state, image: str) -> int:
    """The address `render_frame` (`FUN_1c2b24`) itself computes for the
    "companding record" it reads via `M9` (`DM(I6-2)`, one of
    `command_dispatch_fn`'s two by-reference outputs to block_handler) --
    see this module's own section note above for the full trace.
    `profile(image).command_record_table + (DM(command_word_shift_src) <<
    12)`, matching `command_dispatch_fn`'s own raw bytes exactly."""
    p = profile(image)
    shift = _dm_word(state, p.command_word_shift_src)
    return (p.command_record_table + (shift << 12)) & 0xFFFFFFFF


def companding_record_fields(state, image: str) -> list[int]:
    """The four words `render_frame` copies from
    `companding_record_address()` (word offsets +2, +4, +6, +8 --
    `COMPANDING_RECORD_FIELD_OFFSETS`) into `COMPANDING_RECORD_TARGETS`
    (`DM(0x254d78)`/`0x254d80`/`0x254d88`/`0x254d90` -- the first of which
    is the pointer docs/findings/06's Lane E2 traces the never-taken
    machine-type-compare branch back to). All-zero on every state this
    lane found reachable from `run_init()` -- see this module's own section
    note above."""
    base = companding_record_address(state, image)
    return [_dm_word(state, base + off) for off in COMPANDING_RECORD_FIELD_OFFSETS]


# --- Delivering the DSPI2 transfer at its real hardware address (lane G1,
# 2026-09-26) --------------------------------------------------------------
#
# Every earlier replay (tools/sharc_replay.py) wrote a captured frame's
# bytes straight into `0x2558dc` (the CONFIRMED-by-execution buffer
# render_frame's own per-track decode reads, docs/findings/06's "The
# ColdFire frame is mapped into SHARC DM at 0x2558dc") and forced
# `command_word` to that frame's own header value directly -- bypassing the
# real SPI-slave receive DMA and command_dispatch_fn entirely. This lane
# traces where those two cells actually come from on real hardware, so a
# replay can write the transfer at THAT address instead and let render_frame
# (FUN_1c2b24) do its own copy, the way a real DMA-then-firmware sequence
# would.
#
# **The receive DMA's ADDRSTART is `command_word` itself -- confirmed from
# both ends.** `FUN_1c7bd4` (the SPI-slave driver's own setup routine,
# docs/findings/04's Lane D2 -- it also calls `FUN_1c9fd5(R12=0x261a10)`,
# the control-block zero-fill lane D2 already traced) builds a 2-node
# descriptor-list ring at DM `0x2641b0`/`0x2641cc` (`sw` 0x1c7c60-0x1c7cc8):
# each node's own DSCPTR_NXT/ADDRSTART/CFG/XCNT/XMOD fields match the
# SC58x/2158x Hardware Reference's own Descriptor-List Mode layout
# (out/refs/adsp-2156x-hwr, Table 27-10, offsets +0x00/+0x04/+0x08/+0x0c/
# +0x10) closely enough to identify the fields (this lane did not chase the
# exact byte width of CFG/XCNT further); node 1's own ADDRSTART
# (`sw 0x1c7c6f`, `I5 = 0x265220`) and node 2's (`sw 0x1c7c89`,
# `I1 = 0x264220`) are EXACTLY `profile(image).command_word`'s own two
# ping-pong instances (`command_word + (shift<<12)`, shift 0/1) -- the SAME
# formula and the SAME addresses `command_dispatch_fn`/block_handler already
# use to find the pending command. A second, identically-shaped ring at
# `0x2641e8`/(the same base again) targets `command_record_table`'s own two
# instances (`0x266220`/`0x267220`, `sw` 0x1c7cb9-0x1c7cbc) -- provisioned
# in hardware exactly like the first, but a captured DSPI2 transfer (2,748
# bytes, `dspiframe.FRAME_BYTES`) is smaller than one 0x1000-byte ring, so
# it never reaches this second ring: consistent with lane F2's own finding
# that `command_record_table` is unwritten everywhere this project has run.
#
# From the CONSUMER side, independently: `FUN_1c2b24`'s own prologue
# (`sw 0x1c2b79`/`0x1c2b83`) saves its incoming `R12` argument (`command_
# word + (shift<<12)`, command_dispatch_fn's first by-reference output) into
# `M10`, and `sw 0x1c2c71`-`0x1c2c8b` -- gated by the flag below -- copies
# 512 long-word (2,048-byte, `0x800`) reads starting AT `I4 = M10` into
# `I5 = 0x2558dc`. So `0x2558dc` is a WORKING COPY render_frame makes of the
# real landing zone every frame, not the landing zone itself: TX byte 0 (the
# command header docs/findings/04 already reads at offset 0) is copied
# to 0x2558dc's own byte 0 unchanged, matching `sharc_replay.py`'s existing
# `COMMAND_HEADER_OFFSET` convention exactly, because `command_word` (word 0
# of the ring) and 0x2558dc's own byte 0 are now the SAME transferred byte,
# not two independently-poked cells.
#
# **`FUN_1c77b4` (docs/findings/04's Lane D2 driver callback) is exactly 9
# instructions and does exactly one thing:** `R2 = btgl(R8, bit=5)` (toggle
# bit 5 of its own event-code argument R8, also testing that bit's ORIGINAL
# value into SZ), `JUMP IF NOT SZ` skips the rest -- so ONLY when bit 5 of
# R8 is set does it `R1 = DM(command_word_shift_src); R2 = btgl(R1, R2);
# DM(command_word_shift_src) = R2` -- an XOR toggle of the ping-pong shift.
# No other memory is touched: this lane reads that as "toggle the ring
# select on a transfer-complete event" (bit 5's own ADI SSL event-code
# meaning was not decoded -- `DMA_SHIFT_CALLBACK_COMPLETE_EVENT` below is
# simply a value with bit 5 set, not a cited constant), with the underlying
# SPI DMA channel's own descriptor-list auto-advance (SC58x/2158x HWR
# "Descriptor-List Mode": the channel loads DMA_DSCPTR_NXT into DMA_DSCPTR_
# CUR on its own at the end of a work unit) doing the actual double-buffer
# re-arming in hardware, invisibly to SHARC code -- consistent with
# `has_static_caller: 0` (docs/findings/04: reached only through the ADI
# SSL library's own generic ISR dispatch, never called by name).
#
# **A second, still-open gate this lane found and had to work around:**
# the whole companding-fields-plus-2,048-byte-copy block above is itself
# gated (`sw 0x1c2c41`-`0x1c2c6e`: `R2 = DM(0x2567dc) (bw); R2 =
# leftz(R2, R0)`, PRM p.521 "SV = result == 32" i.e. SV set iff the byte
# was 0; `JUMP IF SV` skips the copy) by a byte this lane did NOT expect
# from any earlier finding. Empirically, `DM(0x2567dc)` reads back `1`
# right after `run_init()` (so the FIRST frame call after boot does run the
# real copy), but the SAME block's own tail (`sw 0x1c2c88`-`0x1c2c8b`:
# `I4 = 0x2567dc; DM(I4, M5) = M13`) writes it back to `M13`'s OWN value at
# that point, which this lane's concrete runs always find to be `0` -- so
# the gate SELF-CLEARS after one successful copy, and `img.writers()` over
# `[0x2567dc, 0x2567dd)` finds no OTHER resolved writer (several `base_only`
# candidates sit inside `unpack_track`/`FUN_1c24e9`'s own per-track decode
# body, `sw` 0x1c255e-0x1c25cd, close enough to be plausible but not
# resolved to this exact byte within this lane's budget). Real hardware
# must re-arm this every completed transfer, the same way it re-arms the
# receive descriptor itself -- most likely the same opaque ADI SSL ISR
# dispatch `FUN_1c77b4` is reached through, not visible SHARC code. Per
# this project's own rule against guessing an unresolved trigger (docs/
# findings/06's Lane E2 made the same call for the mix gate and the
# companding record's own writer), `drive_dma_completion()` below pokes
# this byte back to nonzero explicitly, alongside the ping-pong toggle, as
# the most faithful stand-in this lane could build for "the DMA-completion
# event this project cannot single-step into" -- documented here as an
# **[O]** substitute, not claimed as a traced firmware write.

DMA_SHIFT_CALLBACK = 0x1C77B4  # FUN_1c77b4 -- see this section's own note.
DMA_SHIFT_CALLBACK_COMPLETE_EVENT = 0x20  # bit 5 set; the exact ADI SSL
# event code was not decoded (see this section's own note) -- only bit 5's
# own effect on this one function was confirmed by reading its 9 instructions.

COPY_GATE_ADDRESS = 0x2567DC  # see this section's own note; [O] real setter.

TRANSFER_COPY_BYTES = 0x800  # 2,048 bytes -- FUN_1c2b24's own DO 512 loop
# at sw 0x1c2c79-0x1c2c7f, confirmed by this section's own trace.

RING_SIZE_BYTES = 0x1000  # the shift<<12 stride both rings use.


def dma_landing_address(state, image: str) -> int:
    """Where the SPI-slave receive DMA's own ADDRSTART currently points for
    the transfer THIS replay is about to deliver -- see this module's own
    section note above. `FUN_1c77b4` toggles `command_word_shift_src` to
    point AT the buffer a transfer just completed into, so the buffer a NEW
    transfer lands in, before that toggle, is the OTHER one: `profile(image
    ).command_word + ((1 - (shift & 1)) << 12)`, `shift` being
    `command_word_shift_src`'s CURRENT (pre-toggle) value."""
    p = profile(image)
    shift = _dm_word(state, p.command_word_shift_src) & 1
    return (p.command_word + ((1 - shift) * RING_SIZE_BYTES)) & 0xFFFFFFFF


def _swap16(data: bytes) -> bytes:
    """Swap the two bytes of every 16-bit unit of DATA (an odd trailing
    byte, if any, is left in place) -- see `write_dma_transfer()`'s own
    docstring for why this is needed at all."""
    out = bytearray(data)
    for i in range(0, len(out) - 1, 2):
        out[i], out[i + 1] = out[i + 1], out[i]
    return bytes(out)


def write_dma_transfer(
    state, image: str, payload: bytes, *, swap16: bool = True
) -> int:
    """Write PAYLOAD (a captured frame's own raw TX bytes, `emu/sharc_
    capture.py`'s `Dspi2Frame.tx`, the SAME big-endian wire order
    `describe_frame()`/`frame_command()` already parse with `_u16be()`) at
    `dma_landing_address()` -- the real hardware receive-DMA landing zone
    this module's own section note traces, not `0x2558dc`/`command_word`
    directly.

    **Byte order, found empirically, not yet cited to a specific hardware
    bit.** A naive byte-for-byte copy (PAYLOAD's own wire order, address
    `base+i` for byte `i`) makes a plain 32-bit `DM` word read at `base`
    recover the WRONG value: this module's own dev checks found a header
    of `0x00, 0x03` (command 3, big-endian) reading back as `0x300`, not
    `3` -- `block_handler`'s own range check (`sw 0x1c750a`-`0x1c7511`:
    `R2 = DM(I3, M5); R1 = lshift(R2, -2); JUMP IF NOT SZ`) would then treat
    every real command as out-of-range and always take the "clear" path,
    never "render". Swapping each 16-bit unit's own two bytes before
    writing (this function's own default) fixes this exactly (confirmed:
    the same header now reads back as `3`) -- consistent with the SPI
    peripheral's own documented 16-bit transfer granularity (`SIZE=1`,
    docs/findings/04's Lane D2) landing each 16-bit unit in DM such that a
    NATIVE (little-endian) narrow read recovers the ColdFire's own
    big-endian-sent value, the same way `frame_command()`'s existing
    `_u16be()` convention already assumes for the wire bytes themselves.
    This is applied uniformly to the whole transfer (every per-track field
    docs/findings/04 documents is itself 16-bit), not only the command
    header -- **[O]**: the exact receive-side hardware/DMA mechanism that
    performs this on real silicon was not found in the public HWR at the
    bit level (`DMA_CFG`'s own fields, out/refs/adsp-2156x-hwr, have no
    documented byte-swap control); this function reproduces its observable
    effect, not a cited register setting. Pass `swap16=False` to write the
    raw wire order instead (e.g. to reproduce the old, pre-lane-G1 bug for
    an A/B comparison).

    Truncated to `RING_SIZE_BYTES` (one ring's own span) if longer; a real
    transfer (`dspiframe.FRAME_BYTES`, 2,748 bytes) already fits inside
    that with room to spare. Returns the address written."""
    base = dma_landing_address(state, image)
    truncated = payload[:RING_SIZE_BYTES]
    data = _swap16(truncated) if swap16 else truncated
    for i, byte in enumerate(data):
        _poke(state, base + i, byte, width=1)
    return base


def drive_dma_completion(
    runner: sr.Runner, image: str, *, rearm_copy_gate: bool = False
) -> sr.Runner:
    """Call `FUN_1c77b4` (`DMA_SHIFT_CALLBACK`) the way the SPI service's
    own ISR dispatch would on a receive-complete event (`R8` with bit 5
    set -- see this module's own section note for why), toggling
    `profile(image).command_word_shift_src` onto the buffer
    `write_dma_transfer()` just filled.

    REARM_COPY_GATE (default False) also writes 1 to `COPY_GATE_ADDRESS`,
    the old per-frame stand-in (see that note's own **[O]** paragraph).
    **[C] 2026-09-28: off by default.** With the gate set every frame,
    render_frame's start-of-frame copy makes the per-voice flag table
    0x2522ac (FUN_1c24e9, from RX_BASE) and the trig test at sw 0x1c2d1a
    (the landing ring's TX halfword 0x22) read the same frame, so the
    sample-load gate at sw 0x1c2d22 can only be true when 0x1c2d1a has
    already branched to the trigger block 0x1c33bc: the tail block
    0x1c3329 (FUN_1c3f78 -> FUN_1c7442 -> FUN_1c4e70, the only per-track
    sample load in the frame) is unreachable after a trig. Without the
    re-arm, the unconditional end-of-frame copy (sw 0x1c3064-0x1c3078)
    feeds the next frame, and a trig in frame N loads the track's slot and
    arms its voices in frame N+1 (scratchpad report sharc-trig-arm.md). The
    only SHARC writer of the gate is the clear at sw 0x1c2c8b; run_init()
    leaves it 1, so the first frame still copies.

    Returns a NEW Runner (`Runner.fresh_call()`, the same convention every
    other entry point in this module uses) positioned right after the
    callback's own return; DM memory -- including both writes above -- is
    shared with RUNNER's own State (`fresh_call_state()`'s `dataclasses.
    replace()` only gives fresh copies to control-flow/register containers,
    never to the memory backing -- the same sharing this whole module's
    multi-frame continuity already relies on), so a following `call_frame()`
    on either Runner sees them.
    """
    new_runner = runner.fresh_call(
        DMA_SHIFT_CALLBACK,
        regs={"R8": DMA_SHIFT_CALLBACK_COMPLETE_EVENT},
        diagnose_unknown=True,
    )
    result = new_runner.run(max_steps=64)
    if result.halt.reason != "return without followed call":
        raise RuntimeError(
            "drive_dma_completion: FUN_1c77b4 (%#x) did not return cleanly: %s"
            % (DMA_SHIFT_CALLBACK, result.halt)
        )
    if rearm_copy_gate:
        _poke(new_runner.state, COPY_GATE_ADDRESS, 1, width=1)
    return new_runner


def setup_frame_dma(state, image: str, *, ring_flag: int = 0) -> int:
    """Prepare STATE for the real-DMA replay path (`write_dma_transfer()`/
    `drive_dma_completion()` above) instead of `setup_frame()`'s own direct
    `command_word` poke: only `command_word_shift_src` (the ping-pong
    toggle's own starting position -- a cold-reset value, not a hack) and
    `ring_flag` (the DAC ring half, unrelated to this section) are touched.
    Returns `profile(image).block_handler`, the address `call_frame()`
    should `fresh_call()` at -- the same convention `setup_frame()` uses.
    """
    p = profile(image)
    _poke(state, p.command_word_shift_src, 0)
    _poke(state, p.ring_flag, ring_flag)
    return p.block_handler


# This lane's own hypotheses for the stops render_frame hits when called
# (via setup_frame()/call_frame() above) from a run_init() state with one
# voice set up and no other per-track frame data at all (an almost-empty
# 16-track frame) -- each keyed by the pc sharc_survey's diagnose_unknown
# names as the fork trigger, evidence in this module's git history/the
# lane's handover, not re-derived here. Two of these patch a REGISTER
# immediately after the read that should have produced it, rather than the
# DM cell a static read of the instruction suggests, because a direct
# `_dm_write()` at that raw address does not reach whatever the actual
# "(sw)" instruction resolves (confirmed empirically: identical instruction
# count patched vs unpatched) -- a `tools/sharc_core` address-canonicalisation
# question this lane could not resolve within its own scope; report it,
# do not guess further at it here.
#
# **[C] Lane F2 (2026-09-26): NEITHER entry is actually about missing frame
# data -- both are `tools/sharc_core` gaps, confirmed present even with a
# REAL captured RX frame (out/captures/dt2-1.16-play-pretracks-fulltx.dt2cap,
# frame 20, track 2, master-bus table UNMASKED so the real per-track gain
# table is nonzero).** This corrects the two entries' own comments below,
# which described the ORIGINAL (all-empty synthetic frame) hypothesis before
# this lane checked it against real data; both forks recur at the identical
# pc with identical evidence once real data is present, so the original
# "this lane's synthetic frame never populated it" framing undersold the
# actual cause. A breakpoint at each fork's own upstream write (`0xb88e47`,
# `0x1c4965`) and a direct read of the live symbolic registers there
# (`Runner.state.uregs`, not `img.print_slice()`'s own separate STATIC
# slice, which reports a different, looser "[unresolved: ...]" for values
# a backward slice can't follow past a call boundary) shows precisely:
#
# - `0xb88e4b` (the `0x1C2FEC` entry): `R8` (the divisor a shared
#   recips-based fixed-point divide helper, `FUN_b88e04`, receives) is a
#   genuine, concrete `Const(0)` -- one of the OTHER 15 tracks this real
#   capture's kit does not assign a machine to legitimately has zero gain,
#   same as the empty synthetic frame did. `recips(0.0)` is well-defined
#   (PRM p.19-16: +-zero input -> +-infinity, PRM p.417-418's own
#   IEEE-754-compatibility rules), but the Newton-Raphson refinement that
#   follows propagates that infinity into a NaN (`infinity * 0.0` inside the
#   helper's own iteration), and `tools/sharc_core`'s `fix()` handler
#   (`compute_alu.py`) has no modeled saturation rule for a NaN/infinity
#   input (`Unknown(reason='fix F0 (unsaturated NAN/infinity fix)')`) --
#   real SHARC+ hardware has a documented saturated result for this case
#   (PRM's own IEEE-754-compatibility section), `tools/sharc_core` just
#   does not implement it yet. Real per-track gain data does not remove
#   this: every genuinely-silent track (15 of 16, in this real capture)
#   hits the identical zero-divisor case a real device would also see, so
#   the gap is inherent to the FIX-of-non-finite path, not to a data gap
#   this lane could fill by tracing further.
# - `0x1c4969` (the `0x1C4965` entry): `F4`/`F6` are Unknown, and the write
#   at `0x1c4965` traces (through `F15=fsub(F15,F7)`, `F2=F10*F15`,
#   `F4=fmax(F2,F13)`) to `Unknown(reason='bitext: undefined (bitlen 95 >
#   32)')` -- a `tools/sharc_core` BITEXT decode that refuses a field wider
#   than 32 bits, hit while `FUN_1c4afe` (one of the four Step/Positions
#   setters, called from `FUN_1c642a`'s own per-voice "Trigger" dispatch
#   `0x1c6553` EVERY frame, not only at a real note trigger -- confirmed by
#   the fork's own call stack: `FUN_1c75d8 -> FUN_1c2b24 -> FUN_1c642a ->
#   FUN_1c4afe -> FUN_1c4914`) derives a note/tune-interpolated value from
#   an undocumented voice-record field pair (`DM(I4+0x62)`/`DM(I4+0x63)`,
#   NOT in "The voice record contract" table -- an `[O]` gap in its own
#   right) through a ~150-instruction pitch-table routine this lane read
#   but did not fully reverse (out of this lane's own scope: doing so would
#   mean hand-replicating that routine's arithmetic as a harness patch,
#   which is the same kind of guess this repo's `sharc_core`-fix rule
#   (a PRM page, a test, or a widthaudit 0-mismatch proof) exists to rule
#   out). Confirmed present with the SAME real capture/frame/track as above.
#
# **[C] Lane G2 (2026-09-26): the `0x1C2FEC` entry is REMOVED -- its
# `tools/sharc_core` gap is fixed.** `tools/sharc_core/floats.py`'s
# `_float_to_fixed`/`_double_to_fixed` (the shared FIX/TRUNC[ BY RY]
# helper) now implements the SC58x/2158x PRM's own FIX/TRUNC pages verbatim
# (out/refs/sc58x-2158x-prm/all.txt pp.20-11..20-14 for the 32-bit forms,
# pp.20-28..20-32 for the 64-bit `Fx:y` forms -- identical wording on all
# six pages): a NAN input is unconditional on MODE1.ALUSAT ("A NAN input
# returns a floating-point all 1s result" is its own sentence, not part of
# the following "If saturation mode is not set" clause) and returns
# `0xFFFFFFFF` with `invalid=True` either way; an infinity input or a
# result that numerically overflows saturates to `0x7FFFFFFF`/`0x80000000`
# when ALUSAT=1 (`invalid=False`, since the AI bullet's infinity/overflow
# clause is itself gated "when saturation mode is not set") or returns the
# same `0xFFFFFFFF` all-1s pattern with `invalid=True` when ALUSAT=0 (see
# `_float_to_fixed`'s own docstring for the full flag derivation). Checked
# live: removing this entry and re-running the SAME real-frame call this
# table's own module note above describes (capture
# `dt2-1.16-play-pretracks-fulltx.dt2cap`, frame 20, track 2, master-bus
# table masked) reaches the identical final halt ("return without followed
# call" at `0x1c75d3`) as with the entry present -- the fork at `0xb88e4b`
# no longer occurs because `fix()` on the genuine `Const(0)`-divisor NaN
# now resolves to a concrete `0xFFFFFFFF` instead of `Unknown`. The
# instruction count differs slightly (82,325 vs. 82,263 -- the real
# `0xFFFFFFFF`/AI=1 result take a different, but still successful, path
# than this entry's own arbitrary `0x4000` override did), so
# `RealFrameRingAMilestoneTest`'s own ring-A digest changes too (see
# `tests/test_sharc_harness.py`'s own note on that hash for why -- an
# intended fix, not a regression).
#
# **[C] Lane G2: `0x1C4965` stays, and corrects docs/findings/06's own
# "BITEXT, still open" `[V]` claim that "the BITEXT result R0 from the
# call at 0x1c4949 is overwritten before use."** That claim is INCOMPLETE,
# not a false trace: a live, single-instruction-stepped register trace
# through this exact fork (capture/frame/track as above, `0x1C4965`
# removed) shows `R0` genuinely does get a fresh value from the
# `Rn=comp(Rx,Ry); Rm=Rn`-shaped instruction at `0x1c4950` (form
# `5a_move`) that follows the `CALL 0xb88f70` at `0x1c4949` -- but
# `forms_move.py`'s own `_type_5a_move` comment ("The Type 5a data move and
# compute both consume the pre-instruction file") means the PARALLEL
# `R15 = R0` ureg-copy in that SAME instruction reads `R0`'s
# PRE-instruction value, not the compute's newly written one. Traced live:
# `0xb88fa4` (inside `FUN_b88f70`, the call target -- a leading-zero-count/
# normalize helper, not the fixed-point divide `0xb88e04` unrelated to it)
# is a genuine `BITEXT (NU)` (shiftimm opcode `0x19`) with `BITLEN12=95`
# (one of docs/findings/06's own five sightings), written to `R0` as
# `FUN_b88f70`'s own return value; `0x1c4950`'s `R15 = R0` copies that
# SAME still-`Unknown('bitext: undefined (bitlen 95 > 32)')` value into
# `R15` BEFORE `0x1c4950`'s own compute half gets to overwrite `R0` --
# confirmed by a register snapshot at every intervening pc: `R15` reads
# `Unknown('bitext: ...')` starting at `0x1c4953` (the very next step),
# then flows `F15=fsub(F15,F7)` (`0x1c4961`), `F2=F10*F15` (`0x1c495a`,
# `R10` itself a genuine, unrelated `Const`), `F4=fmax(F2,F13)`
# (`0x1c4963`), into the fork's own `F6=fcomp(F6,F4)` at `0x1c4965` --
# matching lane F2's own trace element for element. So the earlier "[V]"
# framing was right that `R0` itself gets clobbered, but wrong to treat
# that as discarding the BITEXT taint: this SAME instruction is what
# smuggles it into `R15` first. **This is still not a `tools/sharc_core`
# fix this lane may make**: PGR p.11-91 documents `BITLEN12 > 32` as
# "prohibited" for BITEXT's own FEXT-based pseudocode step and says only
# that SV is set, without documenting what numeric result (if any) real
# silicon produces for that width -- unlike the FIX/TRUNC NaN/infinity
# case above, there is no PRM/PGR page to cite for a specific value here,
# so implementing one would be exactly the undocumented guess this
# project's own rule (a PRM page, a test, or a widthaudit 0-mismatch
# proof) exists to rule out. The frame path's pitch bug (`FIELD_PHASE`
# advancing a fixed 40.43 samples/frame regardless of `pitch_step`,
# docs/findings/06 lane E1) is therefore UNCHANGED: `0x1C4965`'s forced
# `1.0f`/`1.0f` operands still discard the real (buggy) Positions
# computation every frame, so `render_frames_to_ring_a()`'s frame-path
# output still does not match `check_correctness()`'s `freq * pitch_step`.
FRAME_PATCH_TABLE: sv.PatchTable = {
    # 0x1c4965 -> fork at 0x1c4969: FUN_1c4914 (the voice record contract's
    # "Positions" setter helper) computes ASTATX.AN/AZ/AV from
    # `F6=fcomp(F6,F4)` over an fmax/fsub chain of BITEXT-decoded voice
    # record position fields. **[C] Lane F2, corrected further by Lane
    # G2 (see this table's own module-level note above for the full,
    # execution-confirmed trace):** the real gap is a genuine, architecturally
    # undocumented `tools/sharc_core` BITEXT(NU) result for `BITLEN12=95 >
    # 32`, hit on every frame's call into `FUN_1c4afe` (real note-trigger
    # call path or not) -- not specific to this lane's own synthetic voice.
    # Forces both fcomp operands to a finite 1.0f -- still a discard of the
    # real, buggy Positions computation (see the frame-path pitch bug note
    # above), not a fix.
    #
    # **[C] Lane H2 (2026-09-26): BITLEN12>32 is confirmed correctly decoded
    # (not a mis-extracted field) and cannot be resolved by reinterpreting
    # its length -- and doing so would not fix the frame-path pitch bug
    # anyway.** Task: use the frame-path pitch as the oracle for
    # `BITLEN12>32`'s undefined result. Checked against BOTH manuals this
    # project cites for the shifter (PGR `adsp-2136x_2137x_214xx` rev2.4
    # p.11-91 and the SHARC+ Core PRM `sc58x-2158x` p.24-18 -- byte-for-byte
    # identical wording): "the DATAEX field adds 4 MSBs to the DATA field,
    # creating a 12-bit immediate value" (PGR all.txt:19044-19046) is
    # exactly `tools/sharc_core/compute_shift.py`'s own
    # `(dataex[3:0] << 8) | data8` -- the SAME combined 12-bit field the
    # already-implemented, presumably-correct FEXT opcode (0x10) splits
    # into `position = X & 0x3F` / `length = (X >> 6) & 0x3F` bit-for-bit.
    # So `BITLEN12` genuinely is a 12-bit immediate as encoded, and this
    # image's five sightings (95, 384, 192, 256, 535) are not a decode bug;
    # neither manual defines a numeric BITEXT result for a value over 32,
    # only that SV is set ("prohibited").
    #
    # More importantly, length is not even the blocking variable here: `grep
    # -rn 'special\['` across `tools/sharc_core` shows `"BFF_HI"`/`"BFF_LO"`
    # are written ONLY by BITEXT/BITDEP's own handlers in
    # `compute_shift.py`, and no BITDEP instruction (the only other writer
    # the PRM/PGR document) ever decodes anywhere in dt2-1.16 -- so
    # `_bff_words()` returns Unknown at every BITEXT site in this image, and
    # `_bff_extract()`'s `isinstance(hi, Const)` guard discards ANY length
    # argument once the FIFO itself is Unknown. Checked live: monkeypatching
    # in each of the four length candidates the task named (mod 64, low 6
    # bits, clamp to 32, truncate a full 64-bit extract) without also
    # assuming a known FIFO reproduces today's exact halt/instruction count
    # for all four -- they cannot be distinguished from "stays Unknown" or
    # from each other this way. The ONLY assumption under which BITEXT
    # produces a concrete value at all here (the FIFO reset to Const(0), the
    # sole state consistent with "never deposited into") makes the four
    # candidates identical to EACH OTHER too (a 64-bit all-zero register
    # reads 0 at any length) -- so this task's own candidate list cannot be
    # discriminated between by any oracle test, only by an unrelated claim
    # about the FIFO's own reset value, which no manual documents either.
    # Implemented that one combined candidate as a throwaway (uncommitted)
    # opt-in anyway, removed this entry, and ran the harness's real-frame
    # render (`render_frames_to_ring_a`, freq=1000 Hz, `pitch_step` in
    # {1.0, 0.5}): the render completes with no new stop both times (so a
    # concrete BITEXT result does let `FUN_1c4afe`'s own fcomp chain run
    # unforced) -- but a coarse frequency scan of ring A finds the SAME
    # dominant tone, ~630 Hz (power_ratio 0.99), at BOTH pitch steps,
    # essentially unchanged from lanes E1/F2/G2's own 631.7191123962402 Hz
    # figure measured with this entry in place; direct correlation at the
    # REQUESTED 1000/500 Hz finds negligible power (ratio 0.006/0.039). So
    # resolving `BITEXT` here (however it is resolved) is necessary for the
    # frame to complete on its own but is NOT the frame-path pitch bug's
    # cause: retarget that bug's own next step at `FUN_1c4afe`'s
    # undocumented voice-record input pair `DM(I4+0x62)`/`DM(I4+0x63)` (an
    # `[O]` gap noted under lane F2's own Task 1) -- this harness's
    # `setup_voice()` has no documented writer for it, so it never reflects
    # the caller's own `pitch_step` argument regardless of what `BITEXT`
    # does. `0x1C4965` stays; no `tools/sharc_core` change was made (no
    # citable manual value exists, and the one testable candidate would not
    # have fixed the actual bug).
    #
    # **[C] 2026-09-28: the `0x1C4965` entry is REMOVED; there was no
    # BITEXT.** ShiftImm opcode 0x19 is `Rn = Rn or fdep Rx by bit6:len6`,
    # not BITEXT (NU): PGR Table 12-11 (pp.12-10/12-11) gives or-fdep as
    # 0110 0100 and a ShiftImm opcode as its upper six bits; the PRM's
    # Table 17-9 6-bit column is off by one row there
    # (tools/sharc_core/compute_shift.py). The five "BITLEN12" sightings
    # are bit6:len6 fields, e.g. 0xb88fa4 "R0 = R0 or fdep R12 by 31:1"
    # sets the sign bit of FUN_b88f70's integer-to-float result. With that
    # decoded, the fcomp at 0x1c4965 gets concrete operands and the frame
    # returns with no patch (tests/test_sharc_harness.py's
    # test_render_frames_without_patches_*).
    # 0x1C4965: [("reg", "R6", 0x3F800000), ("reg", "R4", 0x3F800000)],
}

# A further, DIAGNOSTIC-ONLY hypothesis for the third known stop
# (0x1c088e, FUN_1c0874's saturating fix<->float round-trip helper): not
# in FRAME_PATCH_TABLE because forcing it this way desyncs the run rather
# than fixing it (see this module's own handover). Confirmed root cause
# (not a hypothesis): forms_system.py's `_type_18a` bit-test handler
# (`bop in (4, 5)`) requires `isinstance(source, Const)` to produce a
# concrete BTF -- it never uses `_astatx_known_bit()`-style per-bit
# reasoning against a `PartialConst` source, so once ASTATX carries even
# one unrelated unknown bit (near-universal after any float op; confirmed
# by this lane: the tested bit itself, AI, was concretely known here) a
# `bit-test`/`xor-test` against ASTATX/ASTATY always reports Unknown. This
# is a `tools/sharc_core` gap this lane must not fix itself -- report it
# instead: `_type_18a` (tools/sharc_core/forms_system.py, the `bop in (4,
# 5)` branch) should determine `result` from the PartialConst's own
# mask/bits over exactly `mask`'s bits when `source` is a `PartialConst`,
# falling back to Unknown only when a bit `mask` needs is itself
# unresolved -- the same reasoning `_astatx_known_bit()` already applies
# one bit at a time. Setting this lane's diagnostic patch
# (`{"ASTATX": 0x400}` at 0x1c0885, replacing ASTATX with a plain Const
# that happens to match its own known bits) does get past 0x1c088e, but
# the run then hits a "return-mismatch" category a few hundred thousand
# instructions later (0xb82a08, a repeated call stack entry suggesting an
# IIR/limiter loop) -- discarding ASTATX's other tracked bits this way
# desyncs a later branch, not a step toward the frame returning. Left out
# of FRAME_PATCH_TABLE for that reason.
FRAME_DIAGNOSTIC_ASTATX_PATCH: sv.PatchTable = {0x1C0885: [("reg", "ASTATX", 0x400)]}

# Where one render_frames() call with FRAME_PATCH_TABLE stops today, from
# run_init() state. Tests compare against this one pin: update it (and say
# why in the commit) when a fix moves the stop.
#
# 2026-09-25: forms_move.py's Type3a handler previously refused l=1 (long-
# word) outright ("unsupported Type3a long-word access" at 0x1c2920, 86,740
# instructions). Implementing it (the Type14a-style neighbor-register-pair
# access PRM p.2-4 documents for the (LW) modifier) let the frame run on
# instead of stopping there. The run now reaches a genuine RETURN, not
# another gap: "return without followed call" at 0x1c75d3 -- inside
# FUN_1c74cd (block_handler itself, entry 0x1c74cd, end 0x1c75d8, per
# tools/sharc.py's img.func) -- is how tools/sharc_run.py's
# fresh_call_state()/Runner.fresh_call() (return_address=None, see its own
# docstring) signals that a call started with an empty call_stack has
# unwound all the way back through its own entry point's RTS. Since
# call_frame() enters at block_handler with no real caller pushed, this
# halt fires exactly when block_handler's own return executes -- the frame
# render call chain (block_handler -> command_dispatch_fn -> cmd_handler_3
# -> render_frame -> ...) completed. tools/sharc_widthaudit.py --root frame
# confirms 0 access-width mismatches across all load/store events checked
# in this run.
#
# 2026-09-26 (lane G2): the instruction count moved 95,982 -> 96,044 (+62)
# when the `0x1C2FEC` FRAME_PATCH_TABLE entry was removed (its
# `tools/sharc_core` gap -- FIX-of-NaN/infinity saturation -- is fixed; see
# that entry's own former docstring and docs/findings/06's "Lane G2"
# section). The real, documented `fix()` result (`0xFFFFFFFF`, `AI` set)
# takes a slightly different, still-successful path than the removed
# entry's own arbitrary `0x4000` register override did; the final halt
# (pc, reason) is unchanged. tools/sharc_widthaudit.py --root frame
# reconfirms 0 mismatches at the new count (17,475 load/store events).
#
# 2026-09-28: 96,044 -> 213,504 instructions, same halt, and no patch
# needed. Five tools/sharc_core fixes change the frame's path: Type4b's
# (l, x, w) = (1, 1, 1) is a normal word, not (lw) (PRM p.13-32); (lw)
# modifiers scale like normal words (PRM p.6-9, Table 6-2); the plain
# MODIFY (Type7b, Type7a without (sw)/(nw)) does not scale (PRM p.6-10);
# ShiftImm 0x19 is Rn = Rn or fdep, not BITEXT (NU) (PGR Table 12-11), so
# FRAME_PATCH_TABLE's last entry is gone; and byte/short SIMD companions
# are modelled (PRM p.7-5). tools/sharc_widthaudit.py --root frame:
# 0 mismatches over 46,128 load/store events.
#
# 2026-09-28 (later): 213,504 -> 213,247, same halt. The SIMD half of the
# move forms: Type3a's PEy companion (PRM p.13-15, Table 6-10) and the PEy
# compute of Type3a/4a/1a/5a (PRM p.101). With both switched off the count
# is 213,504 again (scratchpad report sharc-trig-arm.md).
FRAME_MILESTONE = {
    "pc_sw": 0x1C75D3,
    "instructions": 213247,
    "reason": "return without followed call",
}


def render_frames(
    memory,
    image: str,
    *,
    n_frames: int = 1,
    voice: int = 0,
    pitch_step: float = 1.0,
    sample_len: int = 4096,
    samples: Sequence[float] | None = None,
    sample_format: str = "int16",
    command: int = 3,
    ring_flag: int = 0,
    patch_table: sv.PatchTable | None = FRAME_PATCH_TABLE,
    max_steps: int = 4_000_000,
) -> tuple[sr.Runner, list[sr.RunResult]]:
    """`run_init()`, then N `call_frame()` calls against one voice set up
    via `setup_voice()` (VOICE active, the rest silent) -- the frame-level
    analogue of `render_blocks()`. `patch_table` defaults to
    FRAME_PATCH_TABLE (this lane's own hypotheses); pass `None` for a bare
    diagnostic run that stops at the first fork.

    `run_init()` failing (`InitResult.ran` False) raises ValueError: unlike
    render_blocks()'s optional `init`, a frame call has no bare-state
    fallback (setup_frame()'s docstring: render_frame needs post-init
    state; nothing about it was ever designed to run from a blank State).

    Returns (the Runner positioned after the last call_frame(), one
    RunResult per frame) -- inspect `runner.state` afterward (e.g.
    `st._dm_read(runner.state, profile(image).master_mix ...)`, or
    whatever ring the master stage feeds -- docs/findings/06's "Master
    stage": `render_frame` writes the mix at `R4`, i.e. 0x25f180, the same
    address `setup_frame()` never touches) to see what the frame actually
    produced.
    """
    init = run_init(memory, image)
    if not init.ran or init.runner is None:
        raise ValueError("render_frames: run_init failed: %s" % init.error)
    runner = init.runner
    state = runner.state
    if samples is not None:
        _write_samples(state, 0x310000, samples, sample_format)
    setup_voice(
        state,
        image,
        voice,
        sample_len=sample_len,
        pitch_step=pitch_step,
        sample_base=0x310000,
    )
    setup_frame(state, image, command=command, ring_flag=ring_flag)
    results = []
    for _ in range(n_frames):
        runner, result = call_frame(
            runner, image, patch_table=patch_table, max_steps=max_steps
        )
        results.append(result)
    return runner, results


def read_master_mix(memory, image: str, runner: sr.Runner) -> list[float]:
    """The 64 floats at `profile(image).master_mix`'s own input buffer,
    0x25f180 (docs/findings/06's "Master stage": `0x1c207b` gets `R4 =
    0x25f180`; `setup_frame()`/`call_frame()` never write there themselves,
    so any nonzero content came from render_frame's own run) -- for
    checking whether an active voice's signal reached the mix after a
    frame call. 64 words matches the per-block work-buffer size
    (FIELD_WORK_BUFFER) render_frame's own per-track sum accumulates into;
    unlike that field, 0x25f180 is a fixed address, not a voice-record
    offset.
    """
    floats = []
    for i in range(64):
        raw = st._dm_read(runner.state, 0x25F180 + i * 4, 4)
        floats.append(
            struct.unpack("<f", struct.pack("<I", raw.value & 0xFFFFFFFF))[0]
            if raw is not None
            else 0.0
        )
    return floats


def read_ring_a(memory, image: str, runner: sr.Runner, count: int = 64) -> dict:
    """The DAC output ring: `tools/sharc_dac.py` owns the format itself
    (module docstring there has the full static + dynamic evidence for
    "FUN_1c74a1 converts DM(0x25f180)/DM(0x25f200) to Q31, L/R interleaved,
    into ring A half RING_A_BASE + flag*RING_A_HALF_BYTES") -- this
    function is now just the `Runner`-shaped adapter over it, for checking
    whether an active voice's signal reached the DMA-facing output buffer
    after a frame call, one step past `read_master_mix`'s pre-conversion
    float sum.

    `profile(image).ring_flag` resolves the literal DM address
    `setup_frame()` already pokes through (this module's own "Task loop"
    comment); `sharc_dac.ring_half_just_written()` applies the
    post-toggle-flag correction (see its own docstring) that used to be
    hand-rolled here.

    `count` is kept for backward compatibility with callers that pass it,
    but `sharc_dac` always reads a full 64-word (32 stereo sample) half --
    a partial COUNT only truncates the returned lists, it does not change
    how many words are actually read from memory.

    Returns {"flag_after": the current (post-toggle) ring_flag value,
    "half_written": the byte offset of the half the last frame wrote (None
    when `flag_after` itself is None, e.g. a Runner whose loader never
    mapped `ring_flag`), "q31": raw Q31 ints (interleaved L0,R0,L1,R1,...;
    an unmapped word reads as 0, matching `sharc_dac.decode_ring_a_words()`
    -- real SHARC+ SRAM after reset, not "unknown"), "float": the same
    words as -1.0..1.0 floats (Q31's own full-scale convention), plus
    "left"/"right" (32 de-interleaved floats each) and
    "left_raw"/"right_raw" (their Q31 ints) -- `sharc_dac.RingAFrame`'s own
    fields, for a caller that wants one channel without re-deriving the
    interleave.}
    """
    flag_raw = st._dm_read(runner.state, profile(image).ring_flag, 4)
    flag_after = flag_raw.value & 1 if flag_raw is not None else None

    def dm_read(address: int) -> int | None:
        raw = st._dm_read(runner.state, address, 4)
        return None if raw is None else raw.value & 0xFFFFFFFF

    frame = sharc_dac.read_ring_a_pcm(dm_read, flag_after)
    q31 = frame.interleaved_q31()[:count]
    floats = [v / sharc_dac.Q31_FULL_SCALE for v in q31]
    return {
        "flag_after": flag_after,
        "half_written": None if flag_after is None else frame.half_written,
        "q31": q31,
        "float": floats,
        "left": frame.left,
        "right": frame.right,
        "left_raw": frame.left_raw,
        "right_raw": frame.right_raw,
    }


def write_ring_a_wav(
    path: str,
    rings: Sequence[dict],
    *,
    sample_rate: int = 48000,
    stereo: bool = True,
) -> None:
    """Concatenate `read_ring_a()`'s own per-frame return dicts (RINGS, in
    frame order) into one WAV at PATH: stereo (both of ring A's own L/R
    channels, `sharc_dac.write_wav_stereo()`) by default, or a mono L+R
    average (`tools/sharc_harness.py`'s own `write_wav()`, the same
    downmix `render_frames_to_ring_a()` already computes for
    `measure_tone()`) when STEREO is False.

    This is ring A's own WAV writer -- `write_wav()` above stays the
    voice-render (mono-only) one; a caller with several frames' worth of
    `read_ring_a()` dicts (one call per rendered frame) uses this instead
    of hand-concatenating "left"/"right" itself."""
    left: list[float] = []
    right: list[float] = []
    for ring in rings:
        left.extend(ring["left"])
        right.extend(ring["right"])
    if stereo:
        sharc_dac.write_wav_stereo(path, left, right, sample_rate=sample_rate)
    else:
        mono = [
            0.5 * (l_sample + r_sample)
            for l_sample, r_sample in zip(left, right, strict=True)
        ]
        write_wav(path, mono, sample_rate=sample_rate)


# --- Real capture frame bytes (lane E1, 2026-09-25) -----------------------
#
# Every render_frames()/render_frames_to_ring_a() run above (and
# FRAME_PATCH_TABLE/CONTINUOUS_MIX_SCALAR_PATCH) was built against a
# completely EMPTY RX frame: setup_frame() only ever poked the three
# command-dispatch cells, never anything in [0x2558dc, 0x2560de) itself. Per
# this repo's scratchpad STATE.md ("KEY" item, 2026-09-25): the master-bus
# parameter table FUN_1c207b decodes every frame (0x255fb6-0x2560d0) and the
# per-track mix-gate source FUN_1c207b's own dispatch masks come from
# (0x255908-0x25600a) are BOTH inside that 2,050-byte RX frame at 0x2558dc
# (offsets 0x6da-0x7f4 and 0x2c-0x72e respectively) -- so every prior
# "reads all zero" finding about those tables was reading an RX frame this
# project never populated, not a property of the firmware. The functions
# below load real RX frame bytes from a tools/sharc_capture_run.py capture
# (a real device's own DSPI2 traffic) and write them at the same address a
# real transfer would, so a frame render sees whatever a real device
# actually put in its own mixer/gate tables.
#
# CAPTURE_FRAME_BASE/CAPTURE_FRAME_LEN repeat tools/sharc_replay.py's own
# RX_BASE/TX_PAYLOAD_BYTES and tools/sharc_framemap.py's FRAME_BASE/
# FRAME_LEN (docs/findings/06's "The ColdFire frame is mapped into SHARC DM
# at 0x2558dc", confirmed by execution in tools/sharc_framemap.py) rather
# than importing either -- tools/sharc_replay.py is lane E2's file for this
# task, and tools/sharc_framemap.py is a shared analysis tool neither lane
# owns; the literal itself is already established in three places, so a
# fourth citing the same evidence is not a new claim.
CAPTURE_FRAME_BASE = 0x2558DC
CAPTURE_FRAME_LEN = 0x802  # 2,050 bytes


def capture_frame_bytes(cap: sharc_capture.Capture, index: int) -> bytes:
    """The raw bytes a real device's DSPI2 TX frame ``cap.dspi2_frames[index]``
    puts at CAPTURE_FRAME_BASE: the first CAPTURE_FRAME_LEN bytes of that
    frame's own ``.tx`` payload (the ColdFire's TX = the SHARC's RX; see
    tools/sharc_replay.py's own module docstring point 1 for the same
    convention, and tools/sharc_framemap.py's ``_frame_tx`` for the same
    slice). Raises IndexError/ValueError for an out-of-range index or a
    payload shorter than CAPTURE_FRAME_LEN (should not happen for a
    ``--full-tx`` capture -- this lane's own capture,
    out/captures/dt2-1.16-play-pretracks-fulltx.dt2cap, has 2,748-byte
    payloads throughout)."""
    if not 0 <= index < len(cap.dspi2_frames):
        raise IndexError(
            "capture_frame_bytes: frame %d out of range (0..%d)"
            % (index, len(cap.dspi2_frames) - 1)
        )
    tx = cap.dspi2_frames[index].tx
    if len(tx) < CAPTURE_FRAME_LEN:
        raise ValueError(
            "capture_frame_bytes: frame %d payload is only %d bytes, need %d"
            % (index, len(tx), CAPTURE_FRAME_LEN)
        )
    return bytes(tx[:CAPTURE_FRAME_LEN])


def write_capture_frame(state, frame_bytes: bytes) -> None:
    """Poke FRAME_BYTES (CAPTURE_FRAME_LEN bytes) into DM at
    CAPTURE_FRAME_BASE, byte for byte -- the same convention
    tools/sharc_framemap.py's own run uses (``h._poke(state, FRAME_BASE + i,
    byte, width=1)``), so a real capture's per-track mixer/gate tables read
    exactly as a real device would have loaded them."""
    if len(frame_bytes) != CAPTURE_FRAME_LEN:
        raise ValueError(
            "write_capture_frame: expected %d bytes, got %d"
            % (CAPTURE_FRAME_LEN, len(frame_bytes))
        )
    for i, byte in enumerate(frame_bytes):
        _poke(state, CAPTURE_FRAME_BASE + i, byte, width=1)


# Frame offsets (relative to CAPTURE_FRAME_BASE) STATE.md's own "KEY" item
# names as the two ranges to check for the kit-load settle transient: the
# master-bus parameter table (0x255fb6-0x2560d0 = offset 0x6da-0x7f4) and
# the per-track mix-gate source (0x255908-0x25600a = offset 0x2c-0x72e).
SETTLED_FRAME_RANGES: tuple[tuple[int, int], ...] = ((0x6DA, 0x7F4), (0x2C, 0x72E))


def find_settled_capture_frame(
    cap: sharc_capture.Capture,
    *,
    ranges: Sequence[tuple[int, int]] = SETTLED_FRAME_RANGES,
    min_frame: int = 0,
) -> int | None:
    """The first frame index >= MIN_FRAME whose own frame bytes
    (``capture_frame_bytes``) have at least one nonzero byte in every one of
    RANGES -- past the kit-load settle transient (frame 0 of this lane's own
    capture reads all-zero in both ranges: the ColdFire has not yet
    populated the mixer/gate tables). Returns None if no such frame exists.

    This is a coarse gate (some byte nonzero in range), not a stability
    check. This lane's own scan of
    out/captures/dt2-1.16-play-pretracks-fulltx.dt2cap (291 frames) found
    both ranges already nonzero from frame 1 onward (62/561 nonzero bytes),
    but still drifting (68/659 at frame 2) before settling to a near-constant
    63/~593-596 from frame 20 on -- this module's own default pick
    (``DEFAULT_CAPTURE_FRAME_INDEX``) uses ``min_frame=20`` for that reason,
    not the first frame this function would accept unconstrained."""
    for index in range(min_frame, len(cap.dspi2_frames)):
        frame = capture_frame_bytes(cap, index)
        if all(any(frame[lo:hi]) for lo, hi in ranges):
            return index
    return None


# This lane's own capture and chosen start frame (see
# find_settled_capture_frame()'s docstring for why 20, not the first frame
# that satisfies SETTLED_FRAME_RANGES at all): a real 4-track kit, PLAY
# running, DSPI2 traffic captured with the full 2,748-byte TX/RX payload
# (out/captures/dt2-1.16-play-pretracks-fulltx.dt2cap's own header: kind
# "play", 291 frames). At this frame, the big-endian machine-type word at
# CAPTURE_FRAME_BASE offset 0x94+2i (docs/findings/04's "The machine type
# reaches the SHARC, at TX frame offset 0x94 + 2i") is nonzero (value 2)
# only for track index 2 of the 16 -- the one kit track among the four this
# capture's own kit populates that has a real machine assigned, per this
# lane's own report -- every other track offset reads 0.
DEFAULT_CAPTURE_PATH = "out/captures/dt2-1.16-play-pretracks-fulltx.dt2cap"
DEFAULT_CAPTURE_FRAME_INDEX = 20
DEFAULT_CAPTURE_TRACK = 2


def load_capture(path: str = DEFAULT_CAPTURE_PATH) -> sharc_capture.Capture:
    """``emu.sharc_capture.load(path)`` -- re-exported so a caller (this
    module's own CLI, a test, a caller script) does not need its own
    ``sys.path``/import dance for ``emu.sharc_capture`` just to load one
    capture file."""
    return sharc_capture.load(path)


# The master-bus parameter table range (STATE.md's own "KEY" item; see
# SETTLED_FRAME_RANGES), masked out by default when writing a capture's real
# frame bytes for more than one consecutive block_handler call.
#
# **Root cause of a brand-new gap, found by this lane's own bisection (see
# the report's full matrix): real, nonzero content in JUST this range --
# not the per-track gate source (0x2c-0x72e) and not the machine-type table
# (0x94+2i) -- deterministically trips a sharc_core gap this lane has never
# seen before ("unsupported full compute cu=0x3 opcode=0xe0") on the SECOND
# real-frame-driven block_handler call, at the same instruction count every
# time, even when every subsequent frame's own bytes are held IDENTICAL to
# the first (so this is not about frame-to-frame content changing; it is
# about running the per-track dispatch a second time after whatever it did
# on the first). ``tools/sharc_core/compute.py``'s own CU3_OPS comment
# already documents this exact caution for cu=3 in general ("PRM: cu=11 not
# used by SINGLEFN ... there is no evidence it is real or what it would
# mean") -- this lane extends that caution to opcode 0xe0 specifically, not
# a new finding about cu=3 as a whole. The likeliest explanation (not
# confirmed further, out of this lane's own scope) is that a real machine
# type reaching FUN_1c642a's per-track dispatch for the first time (this
# repo's scratchpad STATE.md: "FUN_1c60a2 (machine-type change detector)
# ... a replay with a real 0->2 change saw zero writes there" -- this lane's
# own real-frame render is, as far as this project's history shows, the
# first time that detector has actually fired) reaches genuine per-machine
# synthesis code this project has never executed before, using a compute
# form with no public semantics -- exciting as a "first contact" with real
# machine code, but not something this lane may guess an implementation
# for (this repo's CLAUDE.md: sharc_core fixes need a PRM page, a test, or
# a widthaudit 0-mismatch proof; none exists here). Masking this one range
# lets a capture's real machine-type and per-track gate-source data (this
# lane's own actual target, per STATE.md's KEY item) run for any number of
# continuous frames instead of stopping after one.
MASTER_BUS_TABLE_RANGE: tuple[int, int] = SETTLED_FRAME_RANGES[0]


def capture_frame_bytes_for_render(
    cap: sharc_capture.Capture,
    index: int,
    *,
    mask_master_bus_table: bool = True,
) -> bytes:
    """``capture_frame_bytes()``, with MASTER_BUS_TABLE_RANGE zeroed by
    default (see that constant's own docstring for why) -- what
    ``render_frames_to_ring_a()`` actually writes per frame when given a
    ``capture``. Pass ``mask_master_bus_table=False`` to get the completely
    unmodified real bytes (reproduces the new gap from the second real
    frame on -- useful for confirming the finding, not for a working
    multi-frame render)."""
    frame = bytearray(capture_frame_bytes(cap, index))
    if mask_master_bus_table:
        lo, hi = MASTER_BUS_TABLE_RANGE
        frame[lo:hi] = bytes(hi - lo)
    return bytes(frame)


# --- Voice -> track -> master mix -> ring A (lane A2, 2026-09-25) ---------
#
# render_frames()/call_frame() above reach a genuine RETURN (FRAME_MILESTONE)
# from a run_init() state with one voice set up, but the master mix
# (0x25f180) and both DAC rings stay zero: docs/findings/06's "the mix
# gate" -- FUN_1c642a's own per-track accumulate block (0x1c6b3c-0x1c6b8a)
# is skipped whenever the "shared context" word DM(0x252d3c) reads 0, which
# it does from any run_init() state (init writes the literal 32 at
# 0x1c1643, then FUN_1cb336 resets it to 0 later in the same init run,
# before FUN_1c15e3 returns -- confirmed by execution, see that function's
# own docstring).
#
# **This lane's own whole-image writer/reader scan (tools/sharc.py's
# writers()/readers(), not just run_init()'s own reach) finds one more
# writer and two more readers of DM(0x252d3c), none reported before:**
#
#   - A writer at 0x1cdc23 (``DM(I3, M6) = R11``, I3 = the passed-in
#     0x252d3c pointer), inside FUN_1cdbb2 (0x1cdbb2-0x1cdcab) -- an
#     IIR/all-pole recursion this lane did not fully characterize. It is
#     reached only dynamically, through FUN_1c642a's own out-of-line
#     dispatch FUN_1c71ec and two nested jump tables (0x8055c840 ->
#     0x8055c858 -> 0x8055c874, whose index 1 selects FUN_1cdbb2 --
#     docs/findings/06's "Table 0x8055c874 picks one stage per slot type"),
#     for whatever per-track "slot type" selects that stage -- which is why
#     ``img.callers()`` reports none (a real indirect-call gap, not
#     evidence this path is dead: see this repo's CLAUDE.md). Every capture
#     this project has (idle and play) reads every track's own machine
#     type/selector fields as 0, so this path was never exercised by
#     execution here, and this lane could not reach it from a synthetic
#     frame either (see the report). Reported as [O], not adopted.
#   - Two readers, previously undocumented: 0xb82696 (FUN_b82680, an
#     "orchestrator/dispatcher" in the same block group as the dynamics
#     stage) and 0xb82d54 (FUN_b82d41, the thin wrapper `0x1c207b` calls
#     as `0xb82d41 -> 0xb82cba`, docs/findings/06's "dynamics [D]" stage --
#     confirmed by disassembly: ``R0 = DM(I4, M5)`` at 0xb82d54, I4 = the
#     caller's R12 = the same 0x252d3c pointer, pushed as one of
#     FUN_b82cba's own stack arguments). So this word is not read only by
#     FUN_1c642a's accumulate gate: the dynamics/compressor stage consumes
#     it too. This lane's own experiment (see the report) is consistent
#     with FUN_b82cba's envelope math going degenerate when this input is
#     0 -- a hand-set-nonzero gate reached a NEW, unresolved fork
#     (FUN_b8037f, blk69) instead of restoring multi-frame signal, so this
#     is reported as a plausible explanation, not a fix.
#
# **This lane's own execution trace also corrects one specific claim in
# docs/findings/06's own "[V]" description of the accumulate loop.** A
# concrete, register-level single-step of 0x1c6b3c-0x1c6b8a (patched so
# DM(0x252d3c) reads 1, avoiding the skip) shows R8 -- the operand
# docs/findings/06 calls "per voice/track index R8" -- staying the SAME
# constant (0) for all 32 hardware DO-loop iterations; nothing in the loop
# body ever increments it. The per-iteration axis is instead I4, which
# walks a 32-entry workspace *pointer table* at 0x24ef2c (matching
# docs/findings/06's own "I5 = DM(I6-4) + 0xdc64 = 0x24ef2c" workspace
# confirmation), and the writes this lane observed through it land in
# per-voice SDRAM addresses (e.g. 0x8045b3c0 for voice 0 -- in the same
# 0x8045.... range as the documented voice sample pointer), not in
# 0x252df8 + t*0x100 (the master-stage per-track buffer). This needs a
# second agent's check before correcting docs/findings/06 -- it may mean
# the loop is walking *voices*, writing each one's own decimator-state
# scratch (record +0x104's target), rather than the master-stage per-track
# sum this lane initially assumed from the doc's own framing; reported
# here, not adopted as a doc correction, since this lane's own DM(0x252d3c)
# value (1) may not be the real one the doc's own confirmatory run used.
#
# **Given the above, this lane could not establish a real runtime path
# that sets DM(0x252d3c) to a value that both (a) avoids a new fork and
# (b) makes FUN_1c642a's own accumulate loop deposit a voice's decimated
# output at 0x252df8 + t*0x100.** Per this lane's own task brief ("record
# any remaining hand-set value in FRAME_PATCH_TABLE / a documented setup
# function with its evidence"), the functions below instead inject a
# voice's own ALREADY-FIRMWARE-RENDERED decimated buffer (FIELD_WORK_BUFFER,
# populated every frame by the firmware's own FUN_1c4ecf/FUN_1c4f81 call
# inside FUN_1c642a's per-voice dispatch loop -- this part needs no hand-
# set value at all) directly into one track's master-mix input, bypassing
# only the not-yet-resolved accumulate stage -- confirming, by execution,
# that the DOWNSTREAM path (track buffer -> master stage 0x1c207b -> ring
# A) is intact and unconditional once that one buffer has real content.
#
# --- Continuity across frames, root-caused (lane B2, 2026-09-25) ----------
#
# The frame-0-only problem above is NOT the "one independent post-init
# render per frame" workaround's own two original blockers (`state.trace`'s
# unbounded growth and the Type14a odd-UREG-pair stop at 0x1c32ad): both are
# fixed on this branch (7e5e8c4), and a genuinely continuous Runner (one
# `run_init()`, repeated `Runner.fresh_call()` at block_handler, checked by
# watchpoint on this lane's own multi-frame runs) now completes any number
# of frames cleanly. Ring A still goes silent from frame 1 onward anyway --
# root cause, traced with watchpoints across two frames of one continuous
# Runner (see the report):
#
# `FUN_1c207b`'s own per-track summation loop (`0x1c2353`, `DO ... UNTIL
# LCE` over all 16 tracks' `0x252df8`-range input) multiplies every track's
# contribution by three scalar coefficients held in R1/R2/R12 for the whole
# loop. Those scalars are computed a few hundred instructions earlier
# (`0x1c22d7`-`0x1c2353`) from `FUN_b82d41`/`FUN_b82cba`'s dynamics/
# compressor stage output (`0x254978`/`0x2549f8`, fed by `DM(0x252d3c)` --
# docs/findings/06's still-open mix gate) and from a ~26-field master-bus
# parameter block this SAME function decodes fresh every frame at
# `0x1c2e00`-`0x1c2fb0` (source table `0x255fb6`-`0x2560d0`; confirmed by
# execution to read all zero from a synthetic `run_init()` state -- no real
# kit/mixer configuration is ever loaded here). Frame 0's own compressor
# output is a plausible near-unity gain/trim (R1=0.953, R2=0.984,
# R12=-0.029 as IEEE floats); by frame 1 it has collapsed to a degenerate
# all-zero-multiplier state, independent of the injected signal's own
# amplitude (checked: scaling the injected tone down 10x does not change
# this). **This is real firmware behaviour given this lane's own state (a
# synthetic frame with a blank master-bus parameter table and no real
# per-track "slot type" ever reaching `FUN_1cdbb2`, docs/findings/06's mix
# gate), not an emulator bug**: with a genuinely unconfigured mix bus, the
# compressor's own steady state is silence, and frame 0 is riding on one
# frame's worth of "not yet converged" grace period, not a working master
# bus.
#
# **Hand-set value (documented hypothesis, not a discovered real gate).**
# Fully deriving the real per-frame scalars needs the master-bus parameter
# table's own bit layout and `FUN_b82cba`'s own 5-stage recursive state --
# out of this lane's own budget (see the report's "Open" section). Pinning
# R1/R2/R12 to frame 0's own real, execution-confirmed values at every
# recurrence of `pc=0x1c2353` -- the same "register-only override at one
# pc" technique docs/findings/06 already recommends for this exact
# situation, via the same `PatchTable` mechanism `FRAME_PATCH_TABLE` already
# uses -- keeps the compressor's own downstream multiply-accumulate at its
# frame-0 output indefinitely. This is a static, pc-keyed patch applied
# automatically every time this pc recurs (`sharc_survey.apply_patches()`),
# not a per-frame reset performed by this module's own Python loop: the
# cause (a value this lane could not derive) is what gets a fixed
# assumption, not the Runner's state. Known limitation: this freezes the
# compressor's own gain at whatever it computed for frame 0's specific
# transient, so it does not adapt to a different injected level the way a
# real compressor would -- checked and accepted for this lane's own
# single-tone deliverable, not a general fix.
CONTINUOUS_MIX_SCALAR_PC = 0x1C2353
CONTINUOUS_MIX_SCALAR_PATCH: sv.PatchTable = {
    CONTINUOUS_MIX_SCALAR_PC: [
        ("reg", "R1", 0x3F740000),  # ~0.953 -- frame 0's own computed value
        ("reg", "R2", 0x3F7C0000),  # ~0.984
        ("reg", "R12", 0xBCEE0000),  # ~-0.029
    ],
}

# docs/findings/06's "Master stage": `0x1c207b` sums 16 tracks at
# `0x252df8 + t*0x100` bytes each (32 L floats then 32 R floats, plain
# IEEE float32 words -- the same format `read_master_mix()` already reads
# back at `0x25f180`).
TRACK_MIX_BASE = 0x252DF8
TRACK_MIX_STRIDE = 0x100
TRACK_MIX_CHANNEL_WORDS = 32

# The call site `CALL 0x1c207b` inside `FUN_1c2b24` (render_frame): disasm
# `0x1c3099 CALL (linked, delayed) target=0x1c207b`, immediately after
# `CALL 0x1c642a` (`0x1c3083`, the per-voice/accumulate stage) and
# `CALL 0x1c18a6` (`0x1c3090`), and *before* `CALL 0x1c14e7` (`0x1c30a0`,
# the end-of-frame zero-clear of the same 0x252df8 range -- confirmed by
# execution to run *after* this point, so a poke placed here survives into
# this frame's own master-stage read and is only cleared in preparation
# for the *next* frame, the same way a real accumulate would be). Not a
# resolved symbol in tools/sharc_symbols.py's Profile (this call site, not
# a function entry), so kept as a literal here like the voice record
# offsets above.
MASTER_STAGE_CALL_PC = 0x1C3099


def track_mix_address(track: int, *, channel: str = "L") -> int:
    """The DM address of TRACK's own 32-word L or R half of the master
    stage's per-track input buffer (see TRACK_MIX_BASE's docstring)."""
    if channel not in ("L", "R"):
        raise ValueError("channel must be 'L' or 'R', got %r" % (channel,))
    base = TRACK_MIX_BASE + track * TRACK_MIX_STRIDE
    return base if channel == "L" else base + TRACK_MIX_CHANNEL_WORDS * 4


def _float_to_word(value: float) -> int:
    return struct.unpack("<I", struct.pack("<f", value))[0]


def read_voice_work_buffer_decimated(state, record: int) -> list[float]:
    """RECORD's own work buffer (FIELD_WORK_BUFFER, 64 floats -- this
    frame's real interpolated render, already produced by the firmware's
    own per-voice dispatch call, whether or not FUN_1c642a's own accumulate
    stage ever sums it into a track buffer), decimated 2:1 the same way
    `decimate()`/the firmware's own `0xb80000` does. Read-only: this is
    "the voice's own output this frame" for measurement (lane E1's own
    per-stage comparison), factored out of `inject_track_buffer()` so a
    caller can read it without also writing it anywhere."""
    work = []
    for i in range(64):
        raw = st._dm_read(state, record + FIELD_WORK_BUFFER + i * 4, 4)
        work.append(
            struct.unpack("<f", struct.pack("<I", raw.value & 0xFFFFFFFF))[0]
            if raw is not None
            else 0.0
        )
    return decimate(work)


def inject_track_buffer(state, record: int, *, track: int = 0) -> list[float]:
    """Read RECORD's own work buffer (see `read_voice_work_buffer_decimated`)
    and write the 32 decimated samples as float32 words into both the L
    and R halves of TRACK's master-stage input buffer
    (`track_mix_address()`). Mono only (both channels get the same
    samples) -- there is no evidence yet for a real per-channel pan value
    to set instead. Returns the 32 decimated floats written, for a caller
    to inspect or assemble into a WAV alongside ring A."""
    decimated = read_voice_work_buffer_decimated(state, record)
    for i, value in enumerate(decimated):
        word = _float_to_word(value)
        _poke(state, track_mix_address(track, channel="L") + i * 4, word)
        _poke(state, track_mix_address(track, channel="R") + i * 4, word)
    return decimated


# --- Why the track buffer never reaches the master mix as audio (lane D1,
# 2026-09-25), and injecting past it instead -----------------------------
#
# `inject_track_buffer()`'s own format/address assumption (32 planar L
# floats then 32 planar R floats at `0x252df8 + t*0x100`, `TRACK_MIX_BASE`
# above) is CORRECT -- confirmed by execution: `FUN_1c207b`'s own per-track
# dispatch loop (`0x1c20e7`-`0x1c212a`, a `DO...UNTIL LCE` over all 16
# tracks) walks `I13` from `TRACK_MIX_BASE` in `TRACK_MIX_STRIDE` steps and
# reads/writes exactly the byte offsets `track_mix_address()` computes.
# **The problem is downstream of that: this loop's own per-track branch
# never carries the track's own sample VALUES into the master mix at all,
# under any of the three states this lane exhaustively tried.** Per track,
# `R2` (the loop counter, 0..15) is bit-tested against two masks read once
# per track: `R8 = DM(0x252728)` (`0x1c20e7`) and `R4 = DM(0x252538)`
# (`0x1c20b9`), giving exactly three outcomes, all checked by execution
# with watchpoints on `0x252df8`-`0x253df8` and `0x25f180`-`0x25f280`:
#
#   1. `btst(R8, R2)` bit set -> `JUMP IF NOT SZ` (`0x1c20f4`) skips
#      straight to `0x1c2121` (next track): the track buffer is never
#      touched at all.
#   2. Bit clear in (1), then `btst(R4, R2)` bit clear -> `JUMP IF SZ`
#      (`0x1c20f9`) falls through into `FUN_1c238a` (`0x1c245b`, reached by
#      a plain in-function JUMP, not a CALL -- `tools/sharcdb.py`'s own
#      function-boundary split puts it in a *different* `functions` row,
#      which is why an earlier pass of this lane's own pc-range filter
#      wrongly reported zero track-buffer activity here). This is the
#      DEFAULT outcome from any synthetic `run_init()` state (both masks
#      read all-zero -- see below), and it DOES read the track buffer
#      (confirmed: `0x1c246c`/`0x1c2472` fire on `DM(0x252df8)` range with
#      no patches at all).
#   3. Bit clear in (1), bit set in (2) -> falls through to `0x1c20fc`,
#      the "normal" path this lane originally assumed was the summer: also
#      reads the track buffer (via a hand patch setting `DM(0x252538)`
#      bit 0, since it never happens by default).
#
# Both (2) and (3) run the SAME 16-iteration copy idiom (`0x1c2106`-
# `0x1c211b` / `0x1c2466`-`0x1c247b`): per iteration the loop advances its
# own OWN track-side pointer by only 2 words (8 bytes), so across 16
# iterations it visits word offsets `{0, 2, 4, ..., 30}` of each 32-word
# channel half and NEVER `{1, 3, 5, ..., 31}` -- a real, confirmed
# half-decimation of whatever the track buffer holds. But that isn't even
# the dominant effect: dumping the copy's own destination scratch buffer
# (`0x254878` for path (3)) after a full run shows the copied VALUES are
# exactly the injected samples (still smooth, still correlated with a
# ramp/sine's own shape) -- so the copy itself is faithful, just to a
# SCRATCH buffer, not to the master mix. **The master mix's own 64 words
# are separately, unconditionally overwritten every frame by a THIRD piece
# of this same function** (`0x1c2200`-`0x1c2385`, `CALL 0xb82d41`/
# `CALL 0x1cb3d8` -- the dynamics/compressor stage docs/findings/06 and
# lane B2 already named, and the same `R1`/`R2`/`R12` scalars
# `CONTINUOUS_MIX_SCALAR_PATCH` pins), which reads/writes `DM(0x25f180)`/
# `DM(0x25f200)` directly (via `I2 = R4`, `FUN_1c207b`'s own first
# argument, confirmed `== 0x25f180` at its call site `0x1c771b`/`0x1c771e`
# in the block handler) through a recursive envelope-follower/soft-clip,
# NOT a read of the per-track copy's own destination buffer. **Proven by a
# discriminating experiment, not just by reading the disassembly:**
# injecting a CONSTANT level (0.2 into every one of a track's 64 samples)
# in place of the ramp/sine produces the SAME sparse addressing pattern
# (identical indices populated) but DIFFERENT, smoothly-decaying values at
# those indices (a decay trajectory shaped like an RC/compressor
# time-constant, not a repeated constant) -- so what reaches the master
# mix tracks the compressor's own internal envelope state, not the track
# buffer's per-sample content, regardless of stride or branch taken. Two of
# the three masks feeding this whole dispatch (`0x1c2e0e`'s own write of
# `DM(0x252728)`, `0x1c2e58`'s own write of `DM(0x252538)`) are themselves
# decoded every frame from a >0x6ac-byte span of DM memory
# (`0x255908`-`0x25600a`) that reads all-zero in every capture and every
# synthetic `run_init()` state this project has -- the same class of
# "no real kit/mixer configuration is ever loaded here" gap
# `CONTINUOUS_MIX_SCALAR_PATCH`'s own note and lane B2's report already
# flagged for the neighbouring master-bus parameter table. **This lane did
# not find a way to make the real per-track summation carry audio-rate
# content into the master mix** with the information available (the
# control table's real bit layout, and whether a genuine "no real mixer
# configured" state ever produces a clean signal on real hardware, are
# both still open -- see the report). Given that, and this lane's own
# brief (get a clean tone into ring A), the fix taken here is to inject
# PAST this entire unresolved stage, at the one place C1's own report
# already proved is a lossless, bit-exact copy of whatever the master mix
# holds (`tools/sharc_dac.py`'s own module docstring) -- the same kind of
# "bypass the not-yet-resolved stage, document the hypothesis" move
# `inject_track_buffer()` above already makes for `FUN_1c642a`'s own
# accumulate stage, one stage further downstream.
#
# `FUN_1c74a1` (the Q31/ring-A converter) is called at sw `0x1c7734`,
# inside the audio task's block handler, immediately after `FUN_1c2b24`
# (the whole render orchestrator -- `CALL` at sw `0x1c771e`, `R4 =
# 0x25f180` set at `0x1c771b`) has returned for this frame; the only code
# between those two calls (`0x1c7721`-`0x1c7731`) sets up the ring-A
# destination pointer/flag and touches nothing in `0x25f180`-`0x25f280`
# (confirmed by disassembly and by execution). A write placed exactly here
# lands after `FUN_1c207b`'s own dynamics loop has already finished
# clobbering the master mix for this frame, and before anything reads it
# back for conversion -- the same "confirmed by execution to run after
# this point" placement rule `MASTER_STAGE_CALL_PC` above already
# documents, one call frame up.
MASTER_MIX_INJECT_PC = 0x1C7734

# --- Ring A's own L channel is deliberately negated after conversion
# (lane D1, 2026-09-25; new finding, not a fix) ---------------------------
#
# The first render through `MASTER_MIX_INJECT_PC` produced a "silent" mono
# downmix (`ring_a_mono` all zero, `power_ratio` 0.0) even though
# `injected_max_abs` was clearly nonzero and `read_master_mix()` showed the
# expected samples, in BOTH the L and R halves (this lane writes the same
# value to both, matching `inject_track_buffer()`'s own mono convention).
# Traced with a write watchpoint on the ring-A destination word
# (`0x261cc8`, ring A L position 0): `FUN_1c74a1` itself stores the
# CORRECT, positive Q31 value there (confirmed: raw store value
# `214748368` for a `0.1` input, matching `0.1 * 2**31` to rounding) --
# `FUN_1c74a1`/`fix_by` is NOT the bug lane C1's own isolated test already
# said it wasn't. But a SECOND write to the SAME address happens moments
# later, before the frame's own `RETURN`: a dedicated 16-iteration loop at
# sw `0x1c758b`-`0x1c75a2` (`I4 = ring-half-offset + 0x261cc8`, i.e. the
# SAME half `FUN_1c74a1` just wrote) that does `R1 = neg(R2)` /
# `R2 = neg(R2)` on every word it touches and stores the results straight
# back -- reached unconditionally every frame between `CALL FUN_1c74a1`
# (`0x1c7734`) and `FRAME_MILESTONE` (`0x1c75d3`), confirmed by execution
# to touch only the L-channel (even) words of the half just written, never
# the R-channel (odd) ones (watched: only `0x261cc8` got a second write;
# `0x261ccc`, R position 0, got exactly one write, from `FUN_1c74a1`
# itself). **This is real, deliberate firmware code, not an emulator
# artifact** -- a plain `neg()` compute op, not a decode gap or a width
# mismatch, executed on data `FUN_1c74a1` had already written correctly.
# The most likely explanation is a genuine hardware/output convention
# (e.g. an inverting stage on the L channel only, or an intentional
# stereo-phase convention); this lane did not chase why, only confirmed
# that it happens, unconditionally, every frame, and that it means: ring
# A's own L channel is the exact negation of `read_master_mix()`'s own L
# channel, while R matches directly.
#
# **Consequence for measurement, not a bug to route around:** a coherent
# signal written identically to both master-mix channels (as
# `inject_master_mix()` does) arrives at ring A as `(-L, +R)` with equal
# magnitude -- so the OLD `ring_a_mono = 0.5*(left+right)` downmix
# `render_frames_to_ring_a()` used to compute cancels to exactly zero for
# such a signal (which is what made this lane's own first full render look
# completely silent). `render_frames_to_ring_a()` below now also returns
# `ring_a_left`/`ring_a_right` (undownmixed) so a caller measures each
# channel on its own -- exactly what the report's own brief asks for
# ("THD+N per channel") -- instead of through a downmix that assumes the
# channels are in phase. `ring_a_mono` is kept for callers that still want
# it (e.g. comparing against noise-like, non-phase-related content from an
# earlier lane), documented here as unreliable for a coherent tone.


def inject_master_mix(state, samples: Sequence[float]) -> list[float]:
    """Write SAMPLES (mono; up to `sharc_dac.MASTER_MIX_CHANNEL_WORDS`,
    zero-padded if shorter) as identical float32 words into both the L and
    R halves of the master mix (`sharc_dac.MASTER_MIX_BASE`/
    `MASTER_MIX_R_OFFSET_WORDS`, lane C1's own confirmed format) at
    `MASTER_MIX_INJECT_PC` -- see this section's own module note for why
    the master mix, not the track buffer, is the right injection point
    for a clean tone today. Returns the (possibly zero-padded) list
    actually written, for a caller to log or compare."""
    channel_words = sharc_dac.MASTER_MIX_CHANNEL_WORDS
    padded = list(samples[:channel_words])
    padded += [0.0] * (channel_words - len(padded))
    for i, value in enumerate(padded):
        word = _float_to_word(value)
        _poke(state, sharc_dac.MASTER_MIX_BASE + i * 4, word)
        _poke(
            state,
            sharc_dac.MASTER_MIX_BASE + (sharc_dac.MASTER_MIX_R_OFFSET_WORDS + i) * 4,
            word,
        )
    return padded


def call_frame_with_track_injection(
    runner: sr.Runner,
    image: str,
    record: int,
    *,
    track: int = 0,
    patch_table: sv.PatchTable | None = FRAME_PATCH_TABLE,
    max_steps: int = 4_000_000,
    write_master_mix: bool = True,
    inject_track: bool = True,
    frame_bytes: bytes | None = None,
    watchpoints: Sequence[sr.Watchpoint] = (),
    post_step_hook: Callable[[sr.Runner], None] | None = None,
) -> tuple[sr.Runner, sr.RunResult, list[float]]:
    """Like `call_frame()`, but (when INJECT_TRACK, the default) calls
    `inject_track_buffer()` the instant `MASTER_STAGE_CALL_PC` is about to
    execute -- see this section's own module note for why (the accumulate
    stage's real enable value was not established from a synthetic, empty
    frame; see FRAME_BYTES below for using a real one instead). Steps one
    instruction at a time (not `sharc_survey.run_with_patches()`/
    `run_collect_all()`) applying `patch_table` the same way `call_frame()`
    does; a fork this halts on is a genuinely new, unresolved gap (report
    it, do not add a guess here), not something to drive through with a
    default branch pick, since this function's whole point is to observe
    the master stage's own, unperturbed behaviour once it has real
    per-track input.

    `FRAME_BYTES`, if given (`capture_frame_bytes()`'s own return, lane E1),
    is written at CAPTURE_FRAME_BASE (`write_capture_frame()`) before
    `fresh_call` -- so this one frame's own render sees a real device's
    mixer/gate tables, not a blank synthetic frame, for everything except
    the one voice this harness itself drives via `record`.

    `INJECT_TRACK` (default True) controls whether `inject_track_buffer()`
    actually pokes TRACK's master-stage input; when False, the voice's own
    decimated output is still read every call (via
    `read_voice_work_buffer_decimated()`, the same 32 floats
    `inject_track_buffer()` would have written) for the returned
    `injected`/measurement value, but nothing is written to
    `track_mix_address()` -- for measuring whether real frame data alone
    (FRAME_BYTES) makes FUN_1c642a's own accumulate loop carry the voice
    there without this hack.

    When WRITE_MASTER_MIX is true (the default) and INJECT_TRACK is also
    true, the SAME 32 samples `inject_track_buffer()` just computed are
    also written straight into the master mix (`inject_master_mix()`) the
    instant `MASTER_MIX_INJECT_PC` is about to execute, later in this same
    step loop -- see the "why the track buffer never reaches the master
    mix as audio" module note above `MASTER_MIX_INJECT_PC` for why this was
    necessary to get a clean tone out of a blank synthetic frame. When
    INJECT_TRACK is False but WRITE_MASTER_MIX is still True, the voice's
    own read-only decimated output is written to the master mix directly
    (bypassing the track buffer and its gate entirely) instead of the
    track-buffer injection's own result, so the two hacks can be toggled
    independently. Pass WRITE_MASTER_MIX=False to get the old (pre-lane-D1)
    track-buffer-only behaviour, or both False to run with no injection
    hack at all (real frame data, real gate, real dispatch only).

    `WATCHPOINTS` (lane K1, 2026-09-26), if given, is attached to the
    `fresh_call()` Runner this makes internally (`Runner.attach_watchpoints()`
    -- `fresh_call()` itself never carries watchpoints over, see its own
    docstring), the same way `tools/sharc_replay.py`'s
    `call_frame_collect_all_with_hits()` already does for its own
    `target_watch` -- so a caller driving several frames through this
    function on one continuous Runner can log writes (e.g. a voice
    record's own fields) across the whole run without a second pass.

    `POST_STEP_HOOK` (lane N2, 2026-09-26), if given, is called as
    `post_step_hook(new_runner)` immediately after every single
    `new_runner.step()` in this function's own instruction loop (i.e. at
    genuine per-instruction granularity, the same loop `MASTER_STAGE_CALL_PC`/
    `MASTER_MIX_INJECT_PC` above already use, not a once-per-frame callback).
    This exists because `tools/sharc_replay.py`'s own voice keep-alive
    (`replay_armed_voice(inject_real_sample=True)`) needs to react to a
    `WATCHPOINTS` hit -- a voice record field the firmware's own per-frame
    "clear" utility (docs/findings/06 lane K1's `0x1c4e70`) just zeroed --
    and re-poke it *before* the same frame's own render check runs a few
    instructions later, which a poke applied only between frames (after
    `call_frame_with_track_injection` already returned) is too late for:
    the very same generic clear runs again at the *start* of next frame's
    dispatch, before this function would get another chance to intervene.
    This module deliberately does not know what the hook does (voice
    field semantics belong in `tools/sharc_replay.py`, this function only
    provides the instruction-granular timing hook) -- see that module's
    own `_make_voice_rearm_hook()`.

    Returns `(new_runner, RunResult, injected)` where `injected` is the 32
    decimated floats (written or not, depending on the flags above), or
    `[]` if `MASTER_STAGE_CALL_PC` was never reached (an earlier stop)."""
    p = profile(image)
    new_runner = runner.fresh_call(p.block_handler, diagnose_unknown=True)
    if watchpoints:
        new_runner.attach_watchpoints(list(watchpoints))
    if frame_bytes is not None:
        write_capture_frame(new_runner.state, frame_bytes)
    start_pc_sw = new_runner.state.pc_sw
    injected: list[float] = []
    steps = 0
    halt: sr.Halt | None = None
    t0 = time.perf_counter()
    while steps < max_steps:
        pc_sw = new_runner.state.pc_sw
        if pc_sw == MASTER_STAGE_CALL_PC:
            if inject_track:
                injected = inject_track_buffer(new_runner.state, record, track=track)
            else:
                injected = read_voice_work_buffer_decimated(new_runner.state, record)
        elif write_master_mix and pc_sw == MASTER_MIX_INJECT_PC:
            inject_master_mix(new_runner.state, injected)
        if patch_table:
            sv.apply_patches(new_runner, patch_table)
        try:
            new_runner.step()
        except sr.Halt as exc:
            halt = exc
            break
        if post_step_hook is not None:
            post_step_hook(new_runner)
        steps += 1
    else:
        halt = sr.Halt("max-steps", new_runner.state.pc_sw)
    result = sr.RunResult(
        halt=halt,
        instructions=new_runner.instructions,
        elapsed=time.perf_counter() - t0,
        form_counts=new_runner.form_counts,
        start_pc_sw=start_pc_sw,
        final_pc_sw=new_runner.state.pc_sw,
        max_call_depth_reached=new_runner.max_call_depth_reached,
        watch_log=new_runner.watch_log,
    )
    return new_runner, result, injected


def _merge_patch_tables(*tables: sv.PatchTable | None) -> sv.PatchTable:
    """Union TABLES' entries by pc, concatenating entry lists for a pc that
    appears in more than one (later tables' entries applied after earlier
    ones, at the same pc, by `sharc_survey.apply_patches()`'s own "list of
    entries" convention -- see its docstring). `None` entries are skipped,
    so a caller can pass an optional table straight through."""
    merged: sv.PatchTable = {}
    for table in tables:
        if not table:
            continue
        for pc, entries in table.items():
            merged.setdefault(pc, []).extend(entries)
    return merged


def read_track_mix(state, track: int) -> tuple[list[float], list[float]]:
    """TRACK's own 32+32 planar float32 (L, R) master-stage input words
    (`track_mix_address()`) -- the same address `inject_track_buffer()`
    writes and `FUN_1c207b`'s own per-track dispatch reads, for measuring
    that stage's own content directly (lane E1's "measure at each stage")
    instead of only inferring it from ring A/master-mix downstream."""

    def read_channel(channel: str) -> list[float]:
        base = track_mix_address(track, channel=channel)
        out = []
        for i in range(TRACK_MIX_CHANNEL_WORDS):
            raw = st._dm_read(state, base + i * 4, 4)
            out.append(
                struct.unpack("<f", struct.pack("<I", raw.value & 0xFFFFFFFF))[0]
                if raw is not None
                else 0.0
            )
        return out

    return read_channel("L"), read_channel("R")


def render_frames_to_ring_a(
    memory,
    image: str,
    *,
    n_frames: int = 32,
    voice: int = 0,
    freq: float = 1000.0,
    pitch_step: float = 1.0,
    track: int = 0,
    sample_format: str = "int16",
    patch_table: sv.PatchTable | None = FRAME_PATCH_TABLE,
    mix_scalar_patch: sv.PatchTable | None = CONTINUOUS_MIX_SCALAR_PATCH,
    max_steps: int = 4_000_000,
    write_master_mix: bool = True,
    inject_track: bool = True,
    capture: sharc_capture.Capture | None = None,
    capture_frame_start: int | None = None,
    mask_master_bus_table: bool = True,
    provisional_forms: Sequence[str] = (),
    provisional_interpretations: Mapping[str, str] | None = None,
) -> dict:
    """Render N frames of one voice's own `freq` Hz sine on ONE continuous
    Runner (one `run_init()`, N `Runner.fresh_call()`s at block_handler via
    `call_frame_with_track_injection()`), injecting each frame's real
    decimated output into `track`'s master-mix input (see
    `inject_track_buffer()`) AND, when WRITE_MASTER_MIX is true (the
    default), straight into the master mix itself (see
    `inject_master_mix()` and the module note above `MASTER_MIX_INJECT_PC`
    for why both are needed for a clean tone), and return ring A's own
    output -- the actual DAC-facing buffer, one stage past the master mix
    -- concatenated across frames as mono (L+R averaged) floats at the
    firmware's own decimated output rate (`SOURCE_SAMPLE_RATE / 2` =
    48 kHz).

    **One continuous Runner (lane B2, 2026-09-25; was N independent
    post-init renders under lane A2).** That version's own two blockers
    (`sharc_run.State.trace`'s unbounded growth across repeated calls on
    one Runner, and an "unsupported Type14a odd UREG pair" stop at
    0x1c32ad on the second call onward) are both fixed on this branch
    (7e5e8c4) -- checked here by running many frames on one Runner with no
    new stop and bounded memory. The frame-0-only silence this module's own
    "Continuity across frames, root-caused" note (above `TRACK_MIX_BASE`)
    documents was a THIRD, separate problem (the master stage's own
    per-track gain scalars collapsing to zero by frame 1, given this lane's
    blank synthetic mix configuration) -- fixed here via `mix_scalar_patch`
    (`CONTINUOUS_MIX_SCALAR_PATCH` by default; see that constant's own
    docstring for the full evidence and its documented-hypothesis status).
    Kept even though `write_master_mix` now overwrites the master mix
    downstream every frame: `FUN_1c207b` still runs its own compressor
    loop unconditionally either way, and pinning its own scalars is what
    keeps THAT loop itself (not our own override) from forking or
    otherwise misbehaving across many frames -- see the module note for
    why its own output is discarded rather than trusted.

    **Master mix is no longer trusted as an audio-rate mixer (lane D1,
    2026-09-25).** `FUN_1c207b`'s own per-track dispatch does not carry a
    track's sample values into the master mix under any state this lane
    found (see the module note above `MASTER_MIX_INJECT_PC`); the fix is
    to inject the SAME decimated samples a second time, downstream of that
    entire stage, directly into the master mix, at the point lane C1
    already proved is a lossless, bit-exact source for ring A.

    **Phase continuity comes from one long source buffer plus the voice
    record's own carried state**, both true to how the firmware actually
    plays a long sample: `setup_voice()`/`_write_samples()` run ONCE,
    before the frame loop, over a buffer sized for all `n_frames` (unlike
    the old per-frame-independent version, which needed the source tone
    itself to carry phase since every frame's record restarted at
    `start=0`).

    **CAPTURE (lane E1, 2026-09-25).** If given (an `emu.sharc_capture.load()`
    result), frame `i`'s own real RX bytes -- `capture_frame_bytes(capture,
    (capture_frame_start or find_settled_capture_frame(capture,
    min_frame=DEFAULT_CAPTURE_FRAME_INDEX)) + i)`, wrapping modulo
    `len(capture.dspi2_frames)` if `n_frames` runs past the end -- are
    written at CAPTURE_FRAME_BASE before that frame's own call (via
    `capture_frame_bytes_for_render()`, `MASK_MASTER_BUS_TABLE` -- default
    True -- zeroing MASTER_BUS_TABLE_RANGE; see that constant's own
    docstring for why: real content there deterministically trips a
    brand-new sharc_core gap on the second real-frame call), so FUN_1c642a's
    per-track dispatch and FUN_1c207b's master-stage compressor see a real
    device's own machine-type and per-track gate tables (not a blank
    synthetic frame) for every track except `voice`'s own synthetic tone.
    Without `capture` (the default, `None`), behaviour is unchanged from
    before this lane: no frame bytes are written at all.

    `INJECT_TRACK` (default True) and `WRITE_MASTER_MIX` (default True) are
    passed straight through to `call_frame_with_track_injection()` (see its
    own docstring): both True is this project's original hack combination;
    either or both False, together with `capture` given, is how this lane
    measures whether real frame data removes the need for a given hack --
    see this module's own report for the actual per-configuration results.

    **`PROVISIONAL_FORMS`/`PROVISIONAL_INTERPRETATIONS` (lane E1).** Passed
    to `run_init()` (see its own docstring), so they cover every frame this
    call makes, not just init's own run. Real CAPTURE bytes hit a genuinely
    new fork from frame 1 onward that an empty synthetic frame never
    reached: an "uncertain or undecodable form" halt at 0x1c32b0
    (`21p_undoc16`, this repo's scratchpad STATE.md's own "opt-in
    provisional interpretation ... stops at 0x1c32b1" note) -- pass
    `provisional_forms=["21p_undoc16"],
    provisional_interpretations={"21p_undoc16": "nop"}` to get past it (a
    `sharc_core`-provided, already-tested opt-in, not a fix made here); left
    off by default so a caller not using `capture` sees unchanged behaviour.

    Returns `{"ring_a_mono": [...], "ring_a_left": [...], "ring_a_right":
    [...], "per_frame": [...], "any_new_stop": bool}`; `per_frame[i]` adds
    `"track_mix_left"/"track_mix_right"` (`read_track_mix()`, TRACK's own
    master-stage input after this frame) and `"master_mix"` (`read_master_mix()`,
    64 floats) to `{"frame", "instructions", "halt", "injected_max_abs",
    "ring_a_nonzero"}`, for measuring each stage of voice -> track -> master
    -> ring A per frame, not just the final ring A output. Measure
    `ring_a_left`/`ring_a_right` separately, not `ring_a_mono` -- see the
    "ring A's own L channel is deliberately negated" module note above
    `MASTER_MIX_INJECT_PC` for why the mono downmix cancels to silence for
    a coherent tone.
    """
    init = run_init(
        memory,
        image,
        provisional_forms=provisional_forms,
        provisional_interpretations=provisional_interpretations,
    )
    if not init.ran or init.runner is None:
        raise ValueError("render_frames_to_ring_a: run_init failed: %s" % init.error)

    sample_len = _default_sample_len(n_frames, pitch_step)
    sample_base = 0x310000
    combined_patches = _merge_patch_tables(patch_table, mix_scalar_patch)

    capture_start: int | None = None
    if capture is not None:
        capture_start = (
            capture_frame_start
            if capture_frame_start is not None
            else find_settled_capture_frame(
                capture, min_frame=DEFAULT_CAPTURE_FRAME_INDEX
            )
        )
        if capture_start is None:
            raise ValueError(
                "render_frames_to_ring_a: capture has no frame past "
                "min_frame=%d satisfying SETTLED_FRAME_RANGES"
                % DEFAULT_CAPTURE_FRAME_INDEX
            )
        # Narrow for mypy: capture_start is int from here on (the loop
        # below re-checks `capture is not None`, a separate condition mypy
        # cannot tie back to this one across the intervening code).
        assert capture_start is not None

    runner = init.runner
    state = runner.state
    tone = [
        math.sin(2 * math.pi * (freq / SOURCE_SAMPLE_RATE) * i)
        for i in range(sample_len)
    ]
    _write_samples(state, sample_base, tone, sample_format)
    record = setup_voice(
        state,
        image,
        voice,
        sample_len=sample_len,
        pitch_step=pitch_step,
        sample_base=sample_base,
    )
    setup_frame(state, image, command=3, ring_flag=0)

    ring_a_mono: list[float] = []
    ring_a_left: list[float] = []
    ring_a_right: list[float] = []
    per_frame: list[dict] = []
    for frame_index in range(n_frames):
        frame_bytes = None
        if capture is not None:
            assert capture_start is not None  # established above; see its own note
            index = (capture_start + frame_index) % len(capture.dspi2_frames)
            frame_bytes = capture_frame_bytes_for_render(
                capture, index, mask_master_bus_table=mask_master_bus_table
            )
        runner, result, injected = call_frame_with_track_injection(
            runner,
            image,
            record,
            track=track,
            patch_table=combined_patches,
            max_steps=max_steps,
            write_master_mix=write_master_mix,
            inject_track=inject_track,
            frame_bytes=frame_bytes,
        )
        ring = read_ring_a(memory, image, runner)
        left = [v or 0.0 for v in ring["left"]]
        right = [v or 0.0 for v in ring["right"]]
        mono = [
            0.5 * (l_sample + r_sample)
            for l_sample, r_sample in zip(left, right, strict=True)
        ]
        ring_a_left.extend(left)
        ring_a_right.extend(right)
        ring_a_mono.extend(mono)
        track_left, track_right = read_track_mix(runner.state, track)
        per_frame.append(
            {
                "frame": frame_index,
                "instructions": result.instructions,
                "halt": result.halt.reason,
                "injected_max_abs": max((abs(v) for v in injected), default=0.0),
                "ring_a_nonzero": sum(1 for v in left if v)
                + sum(1 for v in right if v),
                "track_mix_left": track_left,
                "track_mix_right": track_right,
                "master_mix": read_master_mix(memory, image, runner),
            }
        )
    return {
        "ring_a_mono": ring_a_mono,
        "ring_a_left": ring_a_left,
        "ring_a_right": ring_a_right,
        "per_frame": per_frame,
        "any_new_stop": any(
            f["halt"] != "return without followed call" for f in per_frame
        ),
        "capture_frame_start": capture_start,
    }


def measure_tone(
    samples: Sequence[float], freq_hz: float, sample_rate: int, *, discard: int = 0
) -> dict:
    """This module's own direct-correlation tone measurement (the same
    technique `tests/test_sharc_harness.py`'s `CliFrequencyTest` uses):
    correlating SAMPLES against cos/sin at exactly `freq_hz` reconstructs
    that frequency's power with no spectral leakage regardless of whether
    the window holds a whole number of cycles (unlike an FFT bin readout),
    so this works on a short, non-coherently-sampled render.

    Returns `{"freq_hz", "power_ratio" (fundamental / total power, 1.0 =
    pure tone), "rms_dbfs", "peak_dbfs"}` -- both dBFS figures are relative
    to `1.0` (this module's own float/Q31 full-scale convention), `-inf`
    reported as `None` for a silent input."""
    values = list(samples[discard:])
    n = len(values)
    if n == 0:
        return {
            "freq_hz": freq_hz,
            "power_ratio": 0.0,
            "rms_dbfs": None,
            "peak_dbfs": None,
        }
    mean = sum(values) / n
    total_power = sum((v - mean) ** 2 for v in values) / n
    a_cos = sum(
        values[i] * math.cos(2 * math.pi * freq_hz * i / sample_rate) for i in range(n)
    ) * (2.0 / n)
    a_sin = sum(
        values[i] * math.sin(2 * math.pi * freq_hz * i / sample_rate) for i in range(n)
    ) * (2.0 / n)
    amplitude = math.hypot(a_cos, a_sin)
    fund_power = amplitude * amplitude / 2.0
    rms = math.sqrt(sum(v * v for v in values) / n)
    peak = max((abs(v) for v in values), default=0.0)
    return {
        "freq_hz": freq_hz,
        "power_ratio": fund_power / total_power if total_power else 0.0,
        "rms_dbfs": 20 * math.log10(rms) if rms > 0 else None,
        "peak_dbfs": 20 * math.log10(peak) if peak > 0 else None,
    }


def _thdn_dbc(power_ratio: float) -> float | None:
    """THD+N relative to the carrier (dBc), derived from
    `measure_tone()`'s own `power_ratio` (fundamental power / total power):
    THD+N power ratio = (total - fundamental) / fundamental =
    1/power_ratio - 1, expressed in dB. `None` for a silent or a perfectly
    pure (`power_ratio == 1.0`) signal (the latter -> -inf, not a finite
    number worth reporting)."""
    if power_ratio <= 0.0 or power_ratio >= 1.0:
        return None
    return 10 * math.log10((1.0 - power_ratio) / power_ratio)


def flatten(blocks: Sequence[Sequence[float]]) -> list[float]:
    return [sample for block in blocks for sample in block]


def decimate(work_buffer: Sequence[float]) -> list[float]:
    """The 64-float interpolator work buffer, decimated 2:1 by pairwise
    average (docs/findings/06: "0xb80000 reads 64, writes 32, x0.5") --
    the 32-sample block this module's WAV output actually is."""
    return [
        0.5 * (work_buffer[2 * i] + work_buffer[2 * i + 1])
        for i in range(len(work_buffer) // 2)
    ]


def write_wav(path: str, samples: Sequence[float], sample_rate: int = 48000) -> None:
    with wave.open(path, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(sample_rate)
        clipped = (max(-1.0, min(1.0, s)) for s in samples)
        wav.writeframes(
            b"".join(struct.pack("<h", int(round(s * 32767))) for s in clipped)
        )


# --- reference 6-tap polyphase interpolator + 2:1 decimator ----------------


def read_coeff_table(memory, image: str, phases: int = 256) -> list[list[float]]:
    """The firmware's own polyphase coefficients (docs/findings/06's
    "Render and declick": 6 taps per phase, one per 4-byte slot, 24
    bytes/phase), as float taps (signed 32-bit fraction, value / 2**31 --
    see this lane's own finding below, correcting the previous "int16 low
    half" and "int16 high half" guesses).

    ``phases=256`` is this lane's own finding, correcting the previous
    128-phase assumption (docs/findings/06 marks the phase count "[O]";
    128 was a guess "consistent with the 128-entry tables found elsewhere
    in the render chain", not evidence about this table specifically).
    Two independent checks: (1) a concrete, register-level single-step of
    FUN_1c4f81's DO 64 loop reads the coefficient pointer I5 right after
    sw 0x1c50b6's ``modify(I5, M2)`` and finds ``(I5 - coeff_table) / 24``
    (the row this function would read) landing exactly on
    ``int(fractional_phase * 256) % 256`` for every iteration checked
    (multiple non-power-of-2 pitch_step values) -- not the "% 128" a
    128-phase table would need, and not merely double by coincidence: rows
    computed this way went as high as 230, past any 128-row table
    entirely. (2) independent of any addressing question at all, the raw
    bytes at phase index 128+k equal phase index (128-k)'s six taps in
    *reverse* (e.g. row 128 == reversed(row 120), row 127 == reversed(row
    121)) -- the textbook mirror-image second half of a linear-phase
    polyphase filter bank, and not a pattern a wrong stride into unrelated
    data would produce by chance.

    **Each 4-byte slot is one 32-bit Q31 tap, not a 16-bit tap in either
    half (this lane's own finding, 2026-09-25, correcting an intermediate
    "upper 16 bits" guess made while chasing the same evidence).** Both
    16-bit halves of every slot independently satisfy check (2)'s mirror
    property (a 32-bit-symmetric table is trivially symmetric in each half
    separately, and so is the full 32-bit value), so that check alone
    cannot tell a 16-bit-slice reading from the true 32-bit one -- it is
    necessary but not sufficient. What actually distinguishes them: a
    register-level trace of FUN_1c4f81's DO 64 loop's MAC chain
    (``tools/sharc_core/compute_mult.py``'s ``_mr_product_raw``/
    ``_mr_extract32``, already correct and unchanged by this fix) computing
    ``(sample_raw * coeff_slot_raw32 << 1) >> 32`` and comparing that to
    the emulator's own concrete SAT-MRF result for a one-tap-nonzero
    impulse case: using the coefficient slot's *full* 32-bit raw value
    (e.g. ``0x31f09d09`` = 837852425 for phase 0/tap 2) as the multiplier
    input reproduces the emulator's exact SAT-MRF integer (6392, from
    sample_raw=16384) bit-for-bit; using only the slot's upper 16 bits
    (12784, an earlier version of this finding) does not feed the real
    multiplier at all -- SHARC's ``Rn = MR (+-) Rx*Ry`` reads Rx/Ry as
    whole 32-bit registers, and ``R0 = DM(I5+2), long`` (sw 0x1c50c4) loads
    the *whole* slot into R0, not a truncated half. Treating that raw
    32-bit slot as a signed Q31 fraction (``/2**31``) then gives a *float*
    coefficient that, multiplied by the sample as a plain float product
    (this module's ``reference_render``), reproduces the fixed-point
    result to about 2e-5 -- consistent with the one real remaining gap:
    ``_mr_extract32``'s bits-63:32 extraction is a PRM-documented *floor*,
    not a round (p.3-10: "using bits 63-32 for a fractional result"), so a
    pure-float reference is architecturally unable to match it bit-exactly
    without reimplementing the same truncation. The earlier "upper 16
    bits" reading was closer to that *already-truncated* single-tap
    result by coincidence (discarding the coefficient's own low 16 bits
    happens to land near where the hardware's own floor lands for some
    but not all sample/coefficient magnitudes) but is off by 4-8x more
    than the Q31 reading once several taps sum together (this lane's own
    ``check_correctness()`` run: max error dropped from ~1.1e-4 to ~3e-5,
    the SAT floor's own ~1-ULP-of-Q15 floor this module's tests assert
    once this fix landed). See
    ``/private/tmp/claude-501/-Users-em-src-digi-digitakt2/89c5e6c8-2a58-496e-84a0-527f96e6f55a/scratchpad/verify_fixedpoint.py``
    and ``verify_q31.py`` for the traces this is drawn from. This was the
    residual this lane's task brief pointed at ("this lane's evidence
    points at tools/sharc_core's MR-accumulator housekeeping ... and/or
    float_by ... not at this harness's own sample encoding or
    addressing") -- neither of those was actually at fault (both verified
    correct by direct trace); this function's own coefficient extraction
    was.
    """
    from sharcldr import SW_ALIAS_BASE

    base = profile(image).coeff_table
    table = []
    for phase in range(phases):
        taps = []
        for tap in range(COEFF_TABLE_TAPS):
            addr = SW_ALIAS_BASE + base + phase * COEFF_TABLE_STRIDE_BYTES + tap * 4
            raw = memory.read(addr, 4)
            value = 0 if raw is None else int.from_bytes(raw, "little", signed=True)
            taps.append(value / 2147483648.0)
        table.append(taps)
    return table


def reference_render(
    input_samples: Sequence[float],
    coeff_table: Sequence[Sequence[float]],
    step: float,
    n_output: int,
    *,
    start_phase: float = 0.0,
) -> list[float]:
    """A 6-tap polyphase interpolator (input_samples, indexed by a
    fractional position advancing ``step`` samples per interpolated
    output, phase selecting a row of ``coeff_table``) followed by 2:1
    decimation (pairwise average -- docs/findings/06's "0xb80000 reads 64,
    writes 32, x0.5"), computed independently of tools/sharc_core so it is
    a real cross-check rather than the same code re-run.

    **Forward taps, not centered (this lane's own finding, 2026-09-25,
    correcting an earlier ``base - half + 1 + t`` centered-window guess).**
    An impulse-response test against a concrete run of FUN_1c4f81's DO 64
    loop (single nonzero raw sample, unity pitch_step, six placements of
    the impulse relative to ``base`` swept one at a time) shows tap ``t``
    (0-indexed in the same order the loop reads them: the DO loop's first
    sample read pairs with ``coeff_table[phase][0]``, its last with
    ``coeff_table[phase][5]``) always lands on ``input_samples[base + t]``
    -- i.e. the filter is purely causal/forward from ``base``, never
    reaching behind it. Concretely: with the impulse at sample index K and
    ``base`` (the loop's own integer position, sw 0x1c50a4-0x1c50af's
    ``I4 = I0; modify(I4, M3)`` with M3 = floor(position)) swept from K-5
    to K, the nonzero output at each step reproduced
    ``coeff_table[0][K - base]`` (times the sample's own Q15 value) to
    within fixed-point rounding, for every one of the 6 tap positions --
    see ``read_coeff_table()``'s own docstring for the same trace this
    reused to fix the coefficient byte offset. See
    ``/private/tmp/claude-501/-Users-em-src-digi-digitakt2/89c5e6c8-2a58-496e-84a0-527f96e6f55a/scratchpad/trace_impulse2.py``.
    """
    phases = len(coeff_table)
    taps = len(coeff_table[0]) if coeff_table else 6

    def sample_at(index: int) -> float:
        if 0 <= index < len(input_samples):
            return input_samples[index]
        return 0.0

    interpolated = []
    pos = start_phase
    for _ in range(n_output * 2):
        base = int(math.floor(pos))
        frac = pos - base
        phase = int(frac * phases) % phases
        acc = 0.0
        for t in range(taps):
            acc += coeff_table[phase][t] * sample_at(base + t)
        interpolated.append(acc)
        pos += step
    return [
        0.5 * (interpolated[2 * i] + interpolated[2 * i + 1]) for i in range(n_output)
    ]


def _make_test_input(
    kind: str, length: int, *, freq_cycles_per_sample: float = 1.0 / 32
) -> list[float]:
    """A synthetic, bounded-amplitude input for ``check_correctness``.
    ``kind`` is "sine" (the same generator the previous version of this
    module used) or "ramp" (a periodic sawtooth at the same period, in
    [-1, 1)) -- a discontinuous waveform the 6-tap interpolator's frequency
    response treats very differently from a sine, so agreement on both is
    a much stronger cross-check than either alone."""
    if kind == "sine":
        return [
            math.sin(2 * math.pi * freq_cycles_per_sample * i) for i in range(length)
        ]
    if kind == "ramp":
        period = max(2, round(1.0 / freq_cycles_per_sample))
        return [2.0 * ((i % period) / period) - 1.0 for i in range(length)]
    raise ValueError("unknown check_correctness input kind: %r" % (kind,))


# (input kind, pitch_step) cases check_correctness() runs by default: unity
# speed, one downward and one upward pitch step (both exact powers of 2, so
# every phase lands on exactly 0 -- coeff row 0 only), one non-power-of-2
# step (0.75, added this lane, 2026-09-25 -- exercises every coeff row a
# quarter-integer step touches, not just row 0) on the sine, and unity on
# the ramp (a discontinuous waveform the 6-tap filter treats very
# differently from a sine).
_DEFAULT_CASES: tuple[tuple[str, float], ...] = (
    ("sine", 1.0),
    ("sine", 0.5),
    ("sine", 2.0),
    ("sine", 0.75),
    ("ramp", 1.0),
)


def _read_record_words(state, record: int) -> list[int]:
    """The full 0x1D8-byte voice record as 32-bit words (VOICE_RECORD_STRIDE
    is word-aligned: 0x1D8 / 4 = 118 exactly), for ``check_correctness()``'s
    per-block before/after diagnostic."""
    words = []
    for offset in range(0, VOICE_RECORD_STRIDE, 4):
        raw = st._dm_read(state, record + offset, 4)
        words.append(raw.value & 0xFFFFFFFF if raw is not None else 0)
    return words


def _diff_record_words(
    before: Sequence[int], after: Sequence[int]
) -> list[dict[str, int]]:
    """Word offsets (bytes, from the record base) where AFTER differs from
    BEFORE -- word, not byte, granularity (coarser than the named FIELD_*
    constants), but enough to see which part of the record one block's
    render actually touched without guessing which field it must have
    been."""
    return [
        {"offset": i * 4, "before": b, "after": a}
        for i, (b, a) in enumerate(zip(before, after, strict=True))
        if b != a
    ]


def check_correctness(
    memory,
    image: str,
    *,
    n_blocks: int = 8,
    cases: Sequence[tuple[str, float]] = _DEFAULT_CASES,
    freq_cycles_per_sample: float = 1.0 / 32,
    sample_format: str = "int16",
    init: InitResult | None = None,
) -> dict[str, dict]:
    """For each (input kind, pitch_step) in ``cases``, render ``n_blocks``
    consecutive 32-sample blocks from one voice and compare each against
    ``reference_render()`` run over the same input with its own phase
    accumulator carried forward the same way the firmware's FIELD_PHASE
    record field persists across ``call_render()`` calls -- so this checks
    both per-block numerical accuracy and block-to-block continuity (phase
    advancing by pitch_step*64 per block, matching the DO 64 loop's 64
    raw interpolation points; ACTIVE staying 1 with real sample data and a
    non-null FIELD_SAMPLE_PTR).

    ``init``, if given (an ``InitResult`` from ``run_init()``), starts every
    case from an independent copy of init's own post-return state (see
    ``new_runner``) instead of a bare one -- each case gets its own copy,
    so one is never affected by another's writes.

    Returns one dict per case, keyed "<kind>_step<pitch_step>", including a
    "record_diffs" list (one entry per block: the word offsets that block's
    render changed in the record, via ``_diff_record_words``) for seeing
    where a field such as ACTIVE actually gets written without re-deriving
    it from the ``active``/``phase`` summaries alone.

    **Sizing the buffer past deactivation, not just past the tap window.**
    FUN_1c4f81's past-limit test (this lane's task brief's static facts;
    FIELD_SAMPLE_LENGTH's own docstring) clears ACTIVE up to about one
    whole block *early*: empirically (this lane's own bisection, four
    pitch_step values), the render needs roughly ``pitch_step * 64 + 4``
    samples of buffer *past* the last one a block actually reads, not the
    fixed +32 tap-window pad alone -- a fixed pad sized for the slowest
    case (0.5x) silently deactivated the last of 8 blocks at 1x and 2x
    (this module's own test asserts against that regression). This sizes
    for one whole extra block at this case's own pitch_step, which covers
    the measured minimum with room to spare, plus the same tap-window pad
    as before.
    """
    sample_base = 0x310000
    coeff_table = None  # loaded once cases start, after profile() resolves
    results: dict[str, dict] = {}
    for kind, pitch_step in cases:
        sample_len = int(math.ceil((n_blocks + 1) * 64 * pitch_step)) + 32
        samples = _make_test_input(
            kind, sample_len, freq_cycles_per_sample=freq_cycles_per_sample
        )
        if coeff_table is None:
            coeff_table = read_coeff_table(memory, image)
        # reference_render() must compare against the same quantized values
        # the emulated render actually reads back (see
        # _dequantize_sample()'s docstring), not the ideal float input --
        # otherwise sample_format="int16"'s own Q15 rounding would show up
        # as "error" indistinguishable from a real interpolation mismatch.
        reference_samples = [_dequantize_sample(s, sample_format) for s in samples]

        runner = new_runner(memory, image, init=init)
        state = runner.state
        _write_samples(state, sample_base, samples, sample_format)
        record = setup_voice(
            state,
            image,
            0,
            sample_len=sample_len,
            pitch_step=pitch_step,
            sample_base=sample_base,
        )

        pos = 0.0
        halts, per_block_max, per_block_mean = [], [], []
        phases, actives = [], []
        record_diffs: list[list[dict[str, int]]] = []
        flat_errors: list[float] = []
        for _ in range(n_blocks):
            before_words = _read_record_words(state, record)
            result, rendered = call_render(runner, image, record)
            after_words = _read_record_words(state, record)
            record_diffs.append(_diff_record_words(before_words, after_words))
            decimated = decimate(rendered)
            reference = reference_render(
                reference_samples,
                coeff_table,
                step=pitch_step,
                n_output=len(decimated),
                start_phase=pos,
            )
            pos += pitch_step * 2 * len(decimated)  # = pitch_step * 64

            errors = [abs(a - b) for a, b in zip(decimated, reference, strict=True)]
            flat_errors.extend(errors)
            per_block_max.append(max(errors) if errors else None)
            per_block_mean.append(sum(errors) / len(errors) if errors else None)
            halts.append(result.halt.reason)
            phases.append(_read_q31_pair(state, record + FIELD_PHASE) / Q31)
            actives.append(_read_byte(state, record + FIELD_ACTIVE))

        phase_deltas = [b - a for a, b in itertools.pairwise(phases)]
        results["%s_step%g" % (kind, pitch_step)] = {
            "kind": kind,
            "pitch_step": pitch_step,
            "n_blocks": n_blocks,
            "halts": halts,
            "max_abs_error": max(flat_errors) if flat_errors else None,
            "mean_abs_error": sum(flat_errors) / len(flat_errors)
            if flat_errors
            else None,
            "per_block_max_error": per_block_max,
            "per_block_mean_error": per_block_mean,
            "phase": phases,
            "phase_deltas": phase_deltas,
            "phase_delta_expected": pitch_step * 64,
            "active": actives,
            "record_diffs": record_diffs,
        }
    return results


# Task 1's --sample-len auto-sizing, for main()'s CLI only. This is the same
# formula check_correctness() computes inline for each of its own cases
# (see that function's "Sizing the buffer past deactivation" docstring:
# FUN_1c4f81's past-limit test clears ACTIVE about one whole block *early*,
# empirically needing ~pitch_step*64+4 samples of buffer past the last one
# a block actually reads, not just the +32 tap-window pad) -- duplicated
# here rather than factored into a shared helper check_correctness() itself
# calls, so this lane's task 1 CLI change does not touch check_correctness()
# (kept unchanged per this lane's own task brief).
def _default_sample_len(n_blocks: int, pitch_step: float) -> int:
    """Auto-size a --blocks/--pitch-step render's source buffer with one
    whole block's look-ahead margin, so a long render does not run off the
    end of its own synthetic sample (task 1)."""
    return int(math.ceil((n_blocks + 1) * 64 * pitch_step)) + 32


def main(argv: Sequence[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("image", help='sharc.py image name, e.g. "dt2-1.16"')
    p.add_argument("--voice", type=int, default=0)
    p.add_argument("--blocks", type=int, default=8)
    p.add_argument(
        "--pitch-step",
        type=float,
        default=1.0,
        help="input samples per output (pre-interpolation) step; 1.0 plays "
        "the source at its own rate (SOURCE_SAMPLE_RATE), 2.0 at double "
        "speed/pitch, 0.5 at half",
    )
    p.add_argument(
        "--sample-len",
        type=int,
        default=None,
        help="source buffer length, in samples; default: auto-sized from "
        "--blocks/--pitch-step (--frame: 4096) so a long render does not "
        "run off the end of its own synthetic sample (see "
        "_default_sample_len())",
    )
    p.add_argument(
        "--freq",
        type=float,
        default=1000.0,
        help="the source sample's own frequency, in Hz, at "
        "SOURCE_SAMPLE_RATE (the firmware's sample-playback rate, "
        "96 kHz -- see that constant's docstring); playing it at "
        "--pitch-step produces an audible output frequency of "
        "freq * pitch_step Hz (Hz is invariant under the render's own 2:1 "
        "decimation -- see main()'s own comment below for the derivation)",
    )
    p.add_argument("--out", default=None, help="WAV output path")
    p.add_argument(
        "--sample-rate",
        type=int,
        default=48000,
        help="the WAV file's declared playback rate; should match the "
        "render's actual decimated output rate (SOURCE_SAMPLE_RATE / 2 = "
        "48000) for --freq * --pitch-step to sound right on playback",
    )
    p.add_argument("--run-init", action="store_true", default=False)
    p.add_argument("--check-correctness", action="store_true", default=False)
    p.add_argument(
        "--frame",
        action="store_true",
        default=False,
        help="call render_frame (FUN_1c2b24) via block_handler instead of "
        "one voice's render (see render_frames()/call_frame())",
    )
    p.add_argument(
        "--frames", type=int, default=1, help="--frame only: number of frame calls"
    )
    p.add_argument(
        "--no-frame-patches",
        dest="frame_patches",
        action="store_false",
        default=True,
        help="--frame only: run without FRAME_PATCH_TABLE (stop at the first fork)",
    )
    p.add_argument(
        "--ring-a",
        action="store_true",
        default=False,
        help="--frame only: render --frames frames of one voice, injecting "
        "each frame's real decimated output into --track's master-mix "
        "input (see inject_track_buffer()'s own module note for why this "
        "is needed, and render_frames_to_ring_a() for the technique), and "
        "write ring A (the DAC-facing buffer) to --out as a WAV instead of "
        "reporting the master mix",
    )
    p.add_argument(
        "--track",
        type=int,
        default=0,
        help="--ring-a only: which of the 16 master-mix track inputs to "
        "inject the voice's rendered output into",
    )
    p.add_argument("--json", action="store_true")
    a = p.parse_args(argv)

    memory = load_image_memory(a.image)

    if a.frame and a.ring_a:
        result = render_frames_to_ring_a(
            memory,
            a.image,
            n_frames=a.frames,
            voice=a.voice,
            freq=a.freq,
            pitch_step=a.pitch_step,
            track=a.track,
            patch_table=FRAME_PATCH_TABLE if a.frame_patches else None,
        )
        sample_rate = int(SOURCE_SAMPLE_RATE // 2)
        output_frequency_hz = a.freq * a.pitch_step
        # Discard the first frame's worth of decimated samples: this
        # lane's own experiments (see the report) found the master stage's
        # own gain/declick ramp attenuates the very first frame or two
        # before a steady level settles, so measuring frequency/level from
        # frame 0 alone would be measuring the ramp, not the tone.
        #
        # Per-channel, not a mono downmix: see the "ring A's own L channel
        # is deliberately negated" module note above MASTER_MIX_INJECT_PC --
        # a coherent tone written identically to both channels arrives at
        # ring A as (-L, +R), so a naive (L+R)/2 downmix cancels to silence.
        measurement_left = measure_tone(
            result["ring_a_left"], output_frequency_hz, sample_rate, discard=32
        )
        measurement_right = measure_tone(
            result["ring_a_right"], output_frequency_hz, sample_rate, discard=32
        )
        if a.out:
            sharc_dac.write_wav_stereo(
                a.out,
                result["ring_a_left"],
                result["ring_a_right"],
                sample_rate=sample_rate,
            )
        report = {
            "image": a.image,
            "voice": a.voice,
            "track": a.track,
            "frames": a.frames,
            "freq_hz": a.freq,
            "output_frequency_hz": output_frequency_hz,
            "any_new_stop": result["any_new_stop"],
            "per_frame": result["per_frame"],
            "measurement_left": measurement_left,
            "measurement_right": measurement_right,
            "thdn_dbc_left": _thdn_dbc(measurement_left["power_ratio"]),
            "thdn_dbc_right": _thdn_dbc(measurement_right["power_ratio"]),
            "out": a.out,
        }
        if a.json:
            print(json.dumps(report, indent=2, default=str))
        else:
            print(
                "image: %s  voice: %d  track: %d  frames: %d  output: %.2f Hz"
                % (a.image, a.voice, a.track, a.frames, output_frequency_hz)
            )
            print("any_new_stop:", result["any_new_stop"])
            for label, measurement, thdn in (
                ("L", measurement_left, report["thdn_dbc_left"]),
                ("R", measurement_right, report["thdn_dbc_right"]),
            ):
                print(
                    "measured %s: freq=%.2f Hz power_ratio=%.4f rms_dbfs=%s "
                    "peak_dbfs=%s thdn_dbc=%s"
                    % (
                        label,
                        measurement["freq_hz"],
                        measurement["power_ratio"],
                        "%.2f" % measurement["rms_dbfs"]
                        if measurement["rms_dbfs"] is not None
                        else "-inf",
                        "%.2f" % measurement["peak_dbfs"]
                        if measurement["peak_dbfs"] is not None
                        else "-inf",
                        "%.2f" % thdn if thdn is not None else "n/a",
                    )
                )
            if a.out:
                print("wrote", a.out)
        return 0

    if a.frame:
        frame_sample_len = a.sample_len if a.sample_len is not None else 4096
        runner, results = render_frames(
            memory,
            a.image,
            n_frames=a.frames,
            voice=a.voice,
            pitch_step=a.pitch_step,
            sample_len=frame_sample_len,
            # The source is a --freq Hz tone at SOURCE_SAMPLE_RATE (task 1;
            # see that constant's docstring and this function's own comment
            # below, before the non-frame render path) -- not pre-scaled by
            # pitch_step, which the DO-64 interpolation loop itself already
            # applies by advancing pitch_step raw samples per output step.
            samples=[
                math.sin(2 * math.pi * (a.freq / SOURCE_SAMPLE_RATE) * i)
                for i in range(frame_sample_len)
            ],
            patch_table=FRAME_PATCH_TABLE if a.frame_patches else None,
        )
        report = {
            "image": a.image,
            "voice": a.voice,
            "frames": a.frames,
            "halts": [r.halt.reason for r in results],
            "instructions": [r.instructions for r in results],
            "master_mix": read_master_mix(memory, a.image, runner),
        }
        if a.json:
            print(json.dumps(report, indent=2, default=str))
        else:
            print("image: %s  voice: %d  frames: %d" % (a.image, a.voice, a.frames))
            for i, r in enumerate(results):
                print(
                    "frame %d: halt=%s pc=%#x instructions=%d"
                    % (i, r.halt.reason, r.halt.pc_sw, r.instructions)
                )
            mix = report["master_mix"]
            print(
                "master_mix[0:8]=%s max_abs=%.4g"
                % (
                    ["%.4g" % v for v in mix[:8]],
                    max((abs(v) for v in mix), default=0.0),
                )
            )
        return 0

    init: InitResult | None = None
    if a.run_init:
        init = run_init(memory, a.image)
    # Only feed a *successful* init into the render/correctness paths --
    # new_runner(..., init=...) raises on an init that did not complete, and
    # a failed --run-init should still fall back to reporting rather than
    # crashing the whole invocation. init.ran/init.error are still reported
    # below either way.
    active_init = init if (init is not None and init.ran) else None

    # Task 1's CLI semantics fix (2026-09-25), correcting this harness's own
    # earlier bug: the source PCM this harness writes to sample_base is a
    # --freq Hz tone at SOURCE_SAMPLE_RATE (the firmware's own
    # sample-playback rate -- see that constant's docstring), not
    # pre-scaled by pitch_step. The DO-64 interpolation loop itself already
    # applies pitch_step (it advances that many raw samples per output
    # step; see setup_voice()'s FIELD_STEP), so folding pitch_step into the
    # source too (the previous version's ``(freq/sample_rate)*pitch_step``)
    # double-counted it. Measured before this fix, on the WAV output: with
    # --sample-rate left at its 48000 default, audible frequency was
    # ``2 * freq * pitch_step**2`` (the extra factor of 2 from the WAV
    # being written at 48000 while the pre-scaled source's own
    # cycles-per-sample was computed against that same 48000, one octave
    # below the 96 kHz the DO-64 domain and its 2:1 decimator actually
    # assume -- see decimate()'s docstring). Corrected: the output's
    # audible frequency is ``freq * pitch_step`` Hz, full stop -- Hz is
    # invariant under the render's own 2:1 decimation to
    # SOURCE_SAMPLE_RATE/2 (48 kHz, matching write_wav()'s own 48000
    # default), so it does not depend on --sample-rate at all; --sample-rate
    # only needs to match that 48 kHz decimated rate for the WAV to *play
    # back* at the frequency its own header claims (this lane verified the
    # corrected formula empirically: tests/test_sharc_harness.py's
    # CliFrequencyTest zero-crossing-counts a rendered WAV against
    # freq * pitch_step for several (freq, pitch_step) pairs).
    output_frequency_hz = a.freq * a.pitch_step
    samples = [
        math.sin(2 * math.pi * (a.freq / SOURCE_SAMPLE_RATE) * i)
        for i in range(
            a.sample_len
            if a.sample_len is not None
            else _default_sample_len(a.blocks, a.pitch_step)
        )
    ]
    sample_len = len(samples)

    start = time.perf_counter()
    blocks, results = render_blocks(
        memory,
        a.image,
        a.voice,
        a.blocks,
        pitch_step=a.pitch_step,
        sample_len=sample_len,
        samples=samples,
        init=active_init,
    )
    elapsed = time.perf_counter() - start
    total_instructions = sum(r.instructions for r in results)

    if a.out:
        write_wav(
            a.out,
            flatten([decimate(block) for block in blocks]),
            sample_rate=a.sample_rate,
        )

    correctness = (
        check_correctness(memory, a.image, init=active_init)
        if a.check_correctness
        else None
    )

    report = {
        "image": a.image,
        "voice": a.voice,
        "blocks": a.blocks,
        "pitch_step": a.pitch_step,
        "freq_hz": a.freq,
        "output_frequency_hz": output_frequency_hz,
        "sample_len": sample_len,
        "instructions_total": total_instructions,
        "instructions_per_block": total_instructions / a.blocks if a.blocks else None,
        "elapsed_s": elapsed,
        "instructions_per_second": total_instructions / elapsed
        if elapsed > 0
        else None,
        "halts": [r.halt.reason for r in results],
        "out": a.out,
        "init": None if init is None else {"ran": init.ran, "error": init.error},
        "correctness": correctness,
    }
    if a.json:
        print(json.dumps(report, indent=2, default=str))
    else:
        print("image: %s  voice: %d  blocks: %d" % (a.image, a.voice, a.blocks))
        print(
            "source: %.2f Hz @ %.0f Hz (sample_len=%d)  pitch_step=%g  "
            "output: %.2f Hz"
            % (
                a.freq,
                SOURCE_SAMPLE_RATE,
                sample_len,
                a.pitch_step,
                output_frequency_hz,
            )
        )
        if init is not None:
            print("init:", "ok" if init.ran else "failed: %s" % init.error)
        print("instructions/block: %.1f" % report["instructions_per_block"])
        print(
            "elapsed: %.3fs (%.0f instr/s)"
            % (elapsed, report["instructions_per_second"] or 0.0)
        )
        print("halts:", report["halts"])
        if a.out:
            print("wrote", a.out)
        if correctness is not None:
            for name, case in correctness.items():
                print(
                    "correctness[%s]: max_abs_error=%.4g mean_abs_error=%.4g "
                    "phase_deltas=%s (expected %.1f) active=%s"
                    % (
                        name,
                        case["max_abs_error"],
                        case["mean_abs_error"],
                        ["%.2f" % d for d in case["phase_deltas"]],
                        case["phase_delta_expected"],
                        case["active"],
                    )
                )
                print(
                    "  record diffs (changed word offsets per block): %s"
                    % [
                        [hex(d["offset"]) for d in diffs]
                        for diffs in case["record_diffs"]
                    ]
                )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
