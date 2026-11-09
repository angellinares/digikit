"""Loading the firmware image and creating and initialising SHARC runners."""

from __future__ import annotations

import struct
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, replace

from tools.sharc_dev import sharc_run as sr
from tools.sharc_dev import sharc_trace as st
from tools.sharc_dev.sharcldr import LoadedMemory

from .layout import profile


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
        follow_loaded_calls=True,
        max_call_depth=64,
        provisional_forms=provisional_forms,
        provisional_interpretations=provisional_interpretations,
    )


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
    (``explicit_memory_model``, ``follow_loaded_calls``,
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


def _dm_word(state, address: int) -> int:
    """A plain 32-bit `DM` read, unwrapped from `Const` -- unwritten
    (`explicit_memory_model`) reads as 0, same convention as `_read_byte`/
    `_read_q31_pair` above."""
    raw = st._dm_read(state, address, 4)
    return raw.value & 0xFFFFFFFF if raw is not None else 0


def _float_to_word(value: float) -> int:
    return struct.unpack("<I", struct.pack("<f", value))[0]
