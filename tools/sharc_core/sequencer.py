"""Decode, program flow, conditions, delayed branches, calls, returns and loops.

Moved verbatim from tools/sharc_trace.py.
"""

from __future__ import annotations

from sharc_disasm import (
    Instruction,
    decode_confident,
    decode_confident_loaded,
    decode_isa48,
)
from sharcldr import LoadedMemory

from .encoding import (
    AF_BIT,
    ALUSAT_BIT,
    AN_BIT,
    AV_BIT,
    AZ_BIT,
    SIMPLE_COND_BITS,
    UREG_CODES,
    _field,
)
from .memory import (
    _dm_read,
    _dossier,
)
from .state import (
    AFTER_DELAY_SLOTS,
    UNKNOWN_PC_STACK_ENTRY,
    Loop,
    Pending,
    State,
    _bank_complete,
    _copy,
    _event,
    _pc_stack_complete,
    _pc_stack_depth,
    _pc_stack_top,
    _pop_loop_resource,
    _push_loop_resource,
    _push_pc_stack,
    _simd_active,
    _stop,
    _sync_pc_stack,
    _sync_status_stack,
    _ureg,
    _ureg_raw,
    _write_ureg,
)
from .values import (
    Const,
    Unknown,
    _astatx_known_bit,
    _bitwise,
    _op_andnot,
    _op_or,
    _signed,
)


def decode_at(
    data: bytes | LoadedMemory, base_sw: int | None, pc_sw: int
) -> Instruction:
    """Decode exactly at PC_SW from a flat image or loader-backed memory,
    with sharc_disasm.resolve_confident_width()'s successor-confidence width
    correction applied -- the same one tools/sharcdb.py's whole-image build
    makes (via tools/sharcimm.py's decode_all()), so a PC that is a real,
    aligned instruction in the program database decodes identically here,
    from the image alone (see docs/findings/05-sharc-isa-and-decoding.md,
    "One decode path")."""
    if isinstance(data, LoadedMemory):
        if 0x90000 <= pc_sw < 0x90080:
            raw = data.read(0x28240000 + (pc_sw - 0x90000) * 6, 6)
            return decode_isa48(raw or b"")
        return decode_confident_loaded(data, pc_sw)
    if base_sw is None:
        raise ValueError("base_sw is required for flat image decoding")
    offset = (pc_sw - base_sw) * 2
    if offset < 0 or offset >= len(data):
        return Instruction(
            offset, None, "unknown", kind="unknown", note="PC outside image"
        )
    return decode_confident(data, offset)


def _instruction_stride(pc: int, length_bytes: int) -> int:
    # The supported L1 IVT is fixed-width ISA, addressed in 48-bit words.
    return 1 if 0x90000 <= pc < 0x90080 else length_bytes // 2


def _advance(state: State, insn: Instruction) -> list[State]:
    state.steps += 1
    _bank_complete(state)
    if insn.length_bytes is None:
        raise ValueError("cannot advance an instruction without a decoded length")
    next_pc = state.pc_sw + _instruction_stride(state.pc_sw, insn.length_bytes)
    if state.pending is None:
        if state.loops and state.pc_sw == state.loops[-1].end_sw:
            loop = state.loops[-1]
            remaining = loop.remaining - 1
            state.uregs[UREG_CODES["CURLCNTR"]] = Const(max(remaining, 0))
            if remaining > 0:
                _event(
                    state,
                    insn,
                    "loop-back",
                    target_sw=loop.start_sw,
                    remaining=remaining,
                    mode=loop.mode,
                )
                state.loops[-1] = Loop(loop.start_sw, loop.end_sw, remaining, loop.mode)
                if state.stack_model:
                    address, _ = state.loop_slots[state.loop_depth - 1]
                    state.loop_slots[state.loop_depth - 1] = (address, Const(remaining))
                state.pc_sw = loop.start_sw
                _pc_stack_complete(state)
                return [state]
            _event(state, insn, "loop-exit", remaining=0, mode=loop.mode)
            state.loops.pop()
            if state.stack_model:
                _pop_loop_resource(state)
            if not _pc_stack_depth(state) or _pc_stack_top(state) != loop.start_sw:
                return [_stop(state, insn, "loop PC-stack mismatch")]
            _pop_pc_stack(state)
            state.uregs[UREG_CODES["CURLCNTR"]] = (
                Const(state.loops[-1].remaining) if state.loops else Const(0xFFFFFFFF)
            )
            if not state.loops:
                stkyx_code = UREG_CODES["STKYX"]
                state.uregs[stkyx_code] = _bitwise(
                    _ureg(state.uregs, stkyx_code),
                    Const(1 << 26),
                    "loop stacks empty",
                    _op_or,
                )
        state.pc_sw = next_pc
        _pc_stack_complete(state)
        return [state]
    p = state.pending
    if p.slots == 1:
        if p.return_from_call:
            if state.stack_model:
                if p.target is None:
                    return [_stop(state, insn, "return without architectural target")]
                state.pc_sw = p.target
                state.pending = None
                _pc_stack_complete(state)
                return [state]
            if state.call_stack and state.call_stack[-1] == UNKNOWN_PC_STACK_ENTRY:
                return [_stop(state, insn, "return through unwritten PC stack entry")]
            if not state.call_stack:
                return [_stop(state, insn, "return without followed call")]
            if state.loops and state.call_stack[-1] == state.loops[-1].start_sw:
                return [_stop(state, insn, "return reached loop PC-stack entry")]
            state.pc_sw = state.call_stack.pop() & 0xFFFFFF
            _sync_pc_stack(state)
            state.pending = None
            _event(state, insn, "loaded-call-return", return_sw=state.pc_sw)
            _pc_stack_complete(state)
            return [state]
        if p.call:
            if p.target is None:
                return [_stop(state, insn, "call without target")]
            target = p.target
            return_sw = next_pc if p.return_sw == AFTER_DELAY_SLOTS else p.return_sw
            loaded = False
            if (
                state.follow_loaded_calls
                and state.concrete is not None
                and target is not None
                and target >= 0
            ):
                decoded = decode_at(state.concrete, None, target)
                loaded = decoded.kind != "unknown"
            if loaded:
                followed_depth = len(state.call_stack) - len(state.loops)
                if followed_depth >= state.max_call_depth:
                    return [_stop(state, insn, "max-call-depth")]
                if return_sw is None:
                    return [_stop(state, insn, "call without architectural return")]
                state.call_stack.append(return_sw)
                if not state.stack_model:
                    _sync_pc_stack(state)
                state.pending = None
                state.pc_sw = target
                state.at_loaded_entry = True
                _event(
                    state,
                    insn,
                    "loaded-call-enter",
                    target_sw=target,
                    return_sw=return_sw,
                )
                _pc_stack_complete(state)
                return [state]
            if return_sw is None:
                return [_stop(state, insn, "call without architectural return")]
            dossier = _dossier(state, target, return_sw)
            if not state.continue_external_calls:
                _stop(state, insn, "external-call")
                # The default endpoint remains the historical stop event; its
                # dossier explicitly labels the otherwise opaque boundary.
                state.trace[-1].update(dossier)
                state.trace[-1]["opaque_external_call"] = True
                return [state]
            _event(state, insn, "opaque-external-call", **dossier)
            # Conservative ABI boundary: results can be clobbered; memory and
            # pointer arguments are deliberately untouched.
            for code in range(16):
                state.uregs[code] = Unknown("opaque-external-call result")
            state.special["MRF"] = Unknown("opaque-external-call result")
            state.pending = None
            state.pc_sw = return_sw
            _event(
                state,
                insn,
                "external-call-continue",
                clobbered=["R%d" % n for n in range(16)] + ["MRF"],
            )
            _pc_stack_complete(state)
            return [state]
        state.pending = None
        state.pc_sw = next_pc if p.target is None else p.target
        _pc_stack_complete(state)
        return [state]
    state.pending = Pending(
        p.target, p.call, p.slots - 1, p.return_from_call, p.return_sw
    )
    state.pc_sw = next_pc
    _pc_stack_complete(state)
    return [state]


def _lt_ge_le_gt(state: State, cond: int) -> bool | None:
    """PGR Table 4-37 (p.4-93) / PRM p.4-53:

    X = (NOT AF AND (AN XOR (AV AND NOT ALUSAT))) OR (AF AND AN) OR AZ
    LE iff X, GT iff NOT X.
    Y = (NOT AF AND (AN XOR (AV AND NOT ALUSAT))) OR (AF AND AN AND NOT AZ)
    LT iff Y, GE iff NOT Y.

    (At AF=0 this is X = Y OR AZ, i.e. LE = LT OR EQ, matching intuition.)
    ALUSAT is only read when it would actually change the answer (AF=0 and
    AV=1); this lets a comparison that clearly did not overflow resolve
    without needing MODE1 to be known.
    """
    astatx = _ureg_raw(state.uregs, UREG_CODES["ASTATX"])
    af = _astatx_known_bit(astatx, AF_BIT)
    an = _astatx_known_bit(astatx, AN_BIT)
    az = _astatx_known_bit(astatx, AZ_BIT)
    if af is None or an is None or az is None:
        return None
    if af:
        x = an or az
        y = an and not az
    else:
        av = _astatx_known_bit(astatx, AV_BIT)
        if av is None:
            return None
        if not av:
            term = an  # AN xor (AV and not ALUSAT), with AV=0
        else:
            mode1 = _ureg(state.uregs, UREG_CODES["MODE1"])
            if not isinstance(mode1, Const):
                return None
            alusat = bool(mode1.value & (1 << ALUSAT_BIT))
            term = an != (not alusat)  # AN xor (True and not ALUSAT)
        x = term or az
        y = term
    if cond in (0x02, 0x12):  # LE / GT
        return x if cond == 0x02 else not x
    return y if cond == 0x01 else not y  # LT / GE


def _predicate(state: State, cond: int) -> bool | None:
    if cond == 0x1F:
        return True
    if cond in (0x00, 0x10):
        # Conditional branches in SIMD mode combine the PEx/PEy conditions.
        # The tracer does not yet model the companion PASS, so only consume
        # AZ when execution is concretely SISD.
        mode1 = _ureg(state.uregs, UREG_CODES["MODE1"])
        astatx = _ureg_raw(state.uregs, UREG_CODES["ASTATX"])
        equal = _astatx_known_bit(astatx, AZ_BIT)
        if not isinstance(mode1, Const) or mode1.value & (1 << 21) or equal is None:
            return None
        return equal if cond == 0x00 else not equal
    if cond in (0x01, 0x02, 0x11, 0x12):
        return _lt_ge_le_gt(state, cond)
    if cond in SIMPLE_COND_BITS:
        bit, negate = SIMPLE_COND_BITS[cond]
        astatx = _ureg_raw(state.uregs, UREG_CODES["ASTATX"])
        known = _astatx_known_bit(astatx, bit)
        if known is None:
            return None
        return (not known) if negate else known
    return None


def _lt_ge_le_gt_pe(state: State, cond: int, pe: str) -> bool | None:
    """_lt_ge_le_gt read against one PE's own status (SHARC+ PRM p.4-53's
    rule, applied to REGF_ASTATY when pe == "y" instead of REGF_ASTATX --
    p.66 Table 3-1 pairs them as the identical per-PE status)."""
    astat_code = UREG_CODES["ASTATX"] if pe == "x" else UREG_CODES["ASTATY"]
    astat = _ureg_raw(state.uregs, astat_code)
    af = _astatx_known_bit(astat, AF_BIT)
    an = _astatx_known_bit(astat, AN_BIT)
    az = _astatx_known_bit(astat, AZ_BIT)
    if af is None or an is None or az is None:
        return None
    if af:
        x = an or az
        y = an and not az
    else:
        av = _astatx_known_bit(astat, AV_BIT)
        if av is None:
            return None
        if not av:
            term = an
        else:
            mode1 = _ureg(state.uregs, UREG_CODES["MODE1"])
            if not isinstance(mode1, Const):
                return None
            alusat = bool(mode1.value & (1 << ALUSAT_BIT))
            term = an != (not alusat)
        x = term or az
        y = term
    if cond in (0x02, 0x12):
        return x if cond == 0x02 else not x
    return y if cond == 0x01 else not y


def _predicate_pe(state: State, cond: int, pe: str) -> bool | None:
    """Evaluate COND against exactly one processing element's own status
    (SHARC+ PRM p.4-54, Table 4-22: a conditional compute or register/
    memory move "[e]xecutes ... depending on condition test in each PE").
    Unlike _predicate, this never bails out because SIMD mode is active or
    unresolved -- reading a single PE's own condition is exactly what SIMD
    mode calls for, and callers that need the combined branch condition use
    _predicate_simd_branch instead."""
    if cond == 0x1F:
        return True
    if cond in (0x00, 0x10):
        astat_code = UREG_CODES["ASTATX"] if pe == "x" else UREG_CODES["ASTATY"]
        astat = _ureg_raw(state.uregs, astat_code)
        equal = _astatx_known_bit(astat, AZ_BIT)
        if equal is None:
            return None
        return equal if cond == 0x00 else not equal
    if cond in (0x01, 0x02, 0x11, 0x12):
        return _lt_ge_le_gt_pe(state, cond, pe)
    if cond in SIMPLE_COND_BITS:
        bit, negate = SIMPLE_COND_BITS[cond]
        astat_code = UREG_CODES["ASTATX"] if pe == "x" else UREG_CODES["ASTATY"]
        astat = _ureg_raw(state.uregs, astat_code)
        known = _astatx_known_bit(astat, bit)
        if known is None:
            return None
        return (not known) if negate else known
    return None


def _predicate_and(a: bool | None, b: bool | None) -> bool | None:
    """Three-valued AND, used to combine PEx's and PEy's conditions for a
    SIMD branch (SHARC+ PRM p.4-54): a concrete False on either side makes
    the whole AND False even if the other side is unresolved; otherwise an
    unresolved side makes the result unresolved."""
    if a is False or b is False:
        return False
    if a is None or b is None:
        return None
    return a and b


def _predicate_simd_branch(state: State, cond: int) -> bool | None:
    """A branch/call/return's predicate (SHARC+ PRM p.4-54, Table 4-22:
    "Executes in sequencer depending on AND'ing condition test on both
    PEs"). SISD mode uses PEx's own condition only; SIMD mode ANDs PEx's
    and PEy's. With MODE1.PEYEN unresolved the predicate is still resolved
    whenever both rules give the same answer: a false PEx condition is false
    in both, and so is agreement between PEx and PEy."""
    if cond == 0x1F:
        return True
    pex = _predicate_pe(state, cond, "x")
    simd = _simd_active(state)
    if simd is False or pex is False:
        return pex
    both = _predicate_and(pex, _predicate_pe(state, cond, "y"))
    if simd:
        return both
    return pex if both == pex else None


def _check_return_target(state: State, pmi: int) -> str | None:
    """The firmware returns through JUMP (M14, I(8+pmi)) (DB). SHARC+ Core
    Programming Reference p.111 ("PC Stack Access", Table 4-3): only CALL
    pushes the hardware PC stack and only RTS/RTI pop it -- an ordinary
    JUMP (any addressing mode, including this one) has no PC-stack effect
    at all. So this idiom is a pure software convention, not a hardware
    return: the callee loads its own saved return address (from the
    compiler's manual return-address stack -- the DM(I7++, M7) push
    generated at the call site's delay slots) into whichever DAG2 index
    register (I8-I15) its own register allocation leaves free, then jumps
    back through that register plus M14 (the ABI's fixed return-address
    modifier: DEFAULT_REGS/CORE_MMR reset gives M14 == 1). I12 (pmi == 4)
    is what nearly every occurrence uses (tools/sharcdb census of dt2-1.16:
    1051 of 1052 cond=0x1F/b=0/j=1/pmm=6 Type9a_abs/9b_abs instructions);
    I13 (pmi == 5) appears once, at 0x1c0cb5, traced back to a DM load of
    the same manual return-address slot into I13 at that function's own
    entry (0x1c0c57) -- the identical idiom, a different register. When
    both registers are known, the jump target must equal the recorded
    return address."""
    index = _ureg(state.uregs, UREG_CODES["I%d" % (8 + pmi)])
    modifier = _ureg(state.uregs, UREG_CODES["M14"])
    if not isinstance(index, Const) or not isinstance(modifier, Const):
        return None
    target = (index.value + modifier.value) & 0xFFFFFF
    if target != (state.call_stack[-1] & 0xFFFFFF):
        return "return target %#x differs from recorded return %#x" % (
            target,
            state.call_stack[-1],
        )
    return None


def _pop_loop_stack(state: State) -> None:
    """Pop the loop (loop-address) stack once: the same single-level pop
    Type20a's LPO performs (sharc_core.forms_system's ``_type_20a``), and
    what JUMP (LA)'s loop-abort also does (see ``_apply_loop_abort``).
    Updates CURLCNTR and STKYX's loop-stacks-empty bit (bit 26) the way
    ``_advance``'s own loop-exit path does."""
    if state.stack_model:
        if state.loops and state.loop_depth != len(state.loops):
            raise ValueError("mixed active and reserved loop pops are not modeled")
        _pop_loop_resource(state)
        if state.loops:
            state.loops.pop()
        return
    if state.loops:
        state.loops.pop()
    state.uregs[UREG_CODES["CURLCNTR"]] = (
        Const(state.loops[-1].remaining) if state.loops else Const(0xFFFFFFFF)
    )
    if not state.loops:
        stkyx_code = UREG_CODES["STKYX"]
        state.uregs[stkyx_code] = _bitwise(
            _ureg(state.uregs, stkyx_code),
            Const(1 << 26),
            "loop stacks empty",
            _op_or,
        )


def _pop_pc_stack(state: State) -> None:
    """Pop the PC (call) stack once and resync PCSTK/PCSTKP/STKYX: the same
    single-level pop Type20a's PPO performs, and what JUMP (LA)'s loop-abort
    also does (see ``_apply_loop_abort``)."""
    if state.stack_model:
        if state.pc_stack:
            state.pc_stack.pop()
    elif state.call_stack:
        state.call_stack.pop()
    _sync_pc_stack(state)


def _apply_loop_abort(state: State) -> None:
    """JUMP ... (LA) -- SHARC+ Core Programming Reference p.4-44 ("Loop
    Abort"): "This instruction causes an automatic loop abort when it
    occurs inside a loop. When the loop aborts, the sequencer pops the PC
    and loop address stacks[,] ... only one pop is performed[; the] loop
    abort cannot be used to jump more than one level of loop nesting."
    Also PGR p.9-36: "(LA)-loop abort-causes the loop stacks and PC stack
    to be popped when the jump is executed." Exactly one pop of each stack,
    reusing Type20a's own LPO+PPO single-level-pop logic; applied only to
    the taken side of a jump/call, at the point the transfer is resolved as
    taken (this tracer does not model delay-slot pipeline timing, so the
    pop lands with the rest of the transfer's bookkeeping rather than
    exactly on the DB-delayed cycle)."""
    _pop_loop_stack(state)
    _pop_pc_stack(state)


def _transfer(
    state: State,
    insn: Instruction,
    target: int,
    call: bool,
    cond: bool | None,
    loop_abort: bool = False,
) -> list[State]:
    if state.pending:
        return [_stop(state, insn, "nested delayed transfer")]
    if insn.length_bytes is None:
        raise ValueError("cannot transfer from an instruction without a decoded length")
    fall = state.pc_sw + _instruction_stride(state.pc_sw, insn.length_bytes)
    _event(state, insn, "call" if call else "branch", target_sw=target, predicate=cond)
    state.steps += 1
    _bank_complete(state)
    if cond is False:
        state.pc_sw = fall
        _pc_stack_complete(state)
        return [state]
    # A delayed CALL returns to the instruction after its second delay slot.
    # The firmware's CJUMP idiom stores that address - 1 in the second slot, so
    # the short-word offset depends on the slot widths (7 after a 16-bit push,
    # 9 after a 48-bit one). Resolve it when the slots complete.
    return_sw = AFTER_DELAY_SLOTS if call else None
    if call and state.stack_model:
        return_sw = _delayed_call_return(state, fall)
    if cond is True:
        if loop_abort:
            _apply_loop_abort(state)
        if call and state.stack_model:
            if return_sw is None:
                raise ValueError("CALL without architectural return address")
            _push_pc_stack(state, 0x01000000 | return_sw)
        state.pc_sw, state.pending = fall, Pending(target, call, return_sw=return_sw)
        _pc_stack_complete(state)
        return [state]
    taken, not_taken = _copy(state), _copy(state)
    if loop_abort:
        _apply_loop_abort(taken)
    if call and taken.stack_model:
        if return_sw is None:
            raise ValueError("CALL without architectural return address")
        _push_pc_stack(taken, 0x01000000 | return_sw)
    taken.pc_sw, taken.pending = fall, Pending(target, call, return_sw=return_sw)
    not_taken.pc_sw, not_taken.pending = fall, Pending(None)
    not_taken.trace[-1]["action"] = "branch-not-taken"
    _pc_stack_complete(taken)
    _pc_stack_complete(not_taken)
    return [taken, not_taken]


def _delayed_call_return(state: State, first_slot: int) -> int:
    """Find the two slot widths before reserving CALL's physical entry."""
    if state.concrete is None:
        raise ValueError("delayed hardware CALL needs loaded delay slots")
    pc = first_slot
    for _ in range(2):
        slot = decode_at(state.concrete, None, pc)
        if slot.length_bytes is None or slot.kind == "unknown":
            raise ValueError("unknown delayed hardware CALL slot width")
        pc += _instruction_stride(pc, slot.length_bytes)
    return pc & 0xFFFFFF


def _immediate_transfer(
    state: State,
    insn: Instruction,
    target: int,
    call: bool,
    cond: bool | None,
    loop_abort: bool = False,
) -> list[State]:
    """Execute a Type 8 transfer without the instruction's DB modifier."""
    if state.pending:
        return [_stop(state, insn, "nested delayed transfer")]
    if insn.length_bytes is None:
        raise ValueError("cannot transfer from an instruction without a decoded length")
    fall = state.pc_sw + _instruction_stride(state.pc_sw, insn.length_bytes)
    _event(state, insn, "call" if call else "branch", target_sw=target, predicate=cond)
    if cond is False:
        return _advance(state, insn)
    if cond is True:
        if loop_abort:
            _apply_loop_abort(state)
        if call and state.stack_model:
            _push_pc_stack(state, 0x01000000 | (fall & 0xFFFFFF))
        state.pending = Pending(target, call, slots=1, return_sw=fall if call else None)
        return _advance(state, insn)
    taken, not_taken = _copy(state), _copy(state)
    if loop_abort:
        _apply_loop_abort(taken)
    if call and taken.stack_model:
        _push_pc_stack(taken, 0x01000000 | (fall & 0xFFFFFF))
    taken.pending = Pending(target, call, slots=1, return_sw=fall if call else None)
    not_taken.trace[-1]["action"] = "branch-not-taken"
    return _advance(taken, insn) + _advance(not_taken, insn)


def _take_return(
    taken: State,
    insn: Instruction,
    delayed: bool,
    length_bytes: int,
    interrupt: bool = False,
) -> list[State]:
    """The taken half of an RTS on TAKEN."""
    if taken.stack_model:
        if not taken.pc_stack:
            return [_stop(taken, insn, "return with empty architectural PC stack")]
        entry = taken.pc_stack[-1]
        if entry == UNKNOWN_PC_STACK_ENTRY:
            return [_stop(taken, insn, "return through unwritten PC stack entry")]
        target = entry & 0xFFFFFF
        _pop_pc_stack(taken)
        # Followed-call records are observations, never an RTS target source.
        if not interrupt:
            if taken.call_stack and (taken.call_stack[-1] & 0xFFFFFF) == target:
                taken.call_stack.pop()
            else:
                while taken.call_stack:
                    taken.call_stack.pop()
        taken.steps += 1
        _bank_complete(taken)
        _pc_stack_complete(taken)
        if delayed:
            taken.pc_sw += _instruction_stride(taken.pc_sw, length_bytes)
            taken.pending = Pending(target, slots=2, return_from_call=True)
        else:
            taken.pc_sw = target
        return [taken]
    if taken.call_stack and taken.call_stack[-1] == UNKNOWN_PC_STACK_ENTRY:
        return [_stop(taken, insn, "return through unwritten PC stack entry")]
    if not taken.call_stack:
        return [_stop(taken, insn, "return without followed call")]
    if taken.loops and taken.call_stack[-1] == taken.loops[-1].start_sw:
        return [_stop(taken, insn, "return reached loop PC-stack entry")]
    if delayed:
        taken.steps += 1
        _bank_complete(taken)
        _pc_stack_complete(taken)
        taken.pc_sw += _instruction_stride(taken.pc_sw, length_bytes)
        taken.pending = Pending(None, slots=2, return_from_call=True)
    else:
        taken.steps += 1
        _bank_complete(taken)
        _pc_stack_complete(taken)
        taken.pc_sw = taken.call_stack.pop() & 0xFFFFFF
        _sync_pc_stack(taken)
        _event(taken, insn, "loaded-call-return", return_sw=taken.pc_sw)
    return [taken]


def _return_transfer(
    state: State,
    insn: Instruction,
    predicate: bool | None,
    delayed: bool,
    interrupt: bool = False,
) -> list[State]:
    """Execute a documented RTS against the tracer's followed-call stack."""
    if state.pending:
        return [_stop(state, insn, "nested delayed transfer")]
    if insn.length_bytes is None:
        raise ValueError("cannot return from an instruction without a decoded length")
    length_bytes = insn.length_bytes
    _event(state, insn, "return", predicate=predicate, delayed=delayed)
    if predicate is False:
        state.trace[-1]["action"] = "return-not-taken"
        return _advance(state, insn)

    if predicate is True:
        if interrupt:
            return _take_interrupt_return(state, insn, delayed, length_bytes)
        return _take_return(state, insn, delayed, length_bytes)
    taken, not_taken = _copy(state), _copy(state)
    not_taken.trace[-1]["action"] = "return-not-taken"
    if interrupt:
        return _take_interrupt_return(taken, insn, delayed, length_bytes) + _advance(
            not_taken, insn
        )
    return _take_return(taken, insn, delayed, length_bytes) + _advance(not_taken, insn)


def _take_interrupt_return(
    state: State, insn: Instruction, delayed: bool, length_bytes: int
) -> list[State]:
    active = _ureg(state.uregs, UREG_CODES["IMASKP"])
    if not state.stack_model or not isinstance(active, Const) or active.value == 0:
        return [_stop(state, insn, "RTI without modeled active interrupt")]
    if not state.pc_stack or not state.status_stack:
        return [_stop(state, insn, "RTI with empty architectural stack")]
    if state.pc_stack[-1] == UNKNOWN_PC_STACK_ENTRY:
        return [_stop(state, insn, "RTI through unwritten PC stack entry")]
    astatx, astaty, mode1 = state.status_stack.pop()
    state.uregs[UREG_CODES["ASTATX"]] = astatx
    state.uregs[UREG_CODES["ASTATY"]] = astaty
    _write_ureg(state, UREG_CODES["MODE1"], mode1)
    # RTI clears the highest-priority active interrupt and its latch.
    bit = active.value & -active.value
    state.uregs[UREG_CODES["IMASKP"]] = Const(active.value & ~bit)
    code = UREG_CODES["IRPTL"]
    state.uregs[code] = _bitwise(
        _ureg(state.uregs, code), Const(bit), "RTI latch clear", _op_andnot
    )
    _sync_status_stack(state)
    if not state.status_stack:
        code = UREG_CODES["STKYX"]
        state.uregs[code] = _bitwise(
            _ureg(state.uregs, code), Const(1 << 24), "status stack empty", _op_or
        )
    return _take_return(state, insn, delayed, length_bytes, interrupt=True)


def _software_interrupt_candidate(state: State) -> int:
    """Software-only diagnostic candidate, independent of timer policy."""
    return _interrupt_candidate(state, 0xF0000000)


def _interrupt_candidate(state: State, allowed_mask: int) -> int:
    """Supported IRQ permitted at this functional instruction boundary."""
    if state.pending or state.loops:
        return 0
    mode = _ureg(state.uregs, UREG_CODES["MODE1"])
    latch = _ureg(state.uregs, UREG_CODES["IRPTL"])
    mask = _ureg(state.uregs, UREG_CODES["IMASK"])
    active = _ureg(state.uregs, UREG_CODES["IMASKP"])
    if (
        not isinstance(mode, Const)
        or not isinstance(latch, Const)
        or not isinstance(mask, Const)
        or not isinstance(active, Const)
    ):
        raise ValueError("unknown software interrupt control")
    if not mode.value & 0x1000:
        return 0
    candidates = latch.value & mask.value & allowed_mask
    if active.value:
        if not mode.value & 0x800:
            return 0
        candidates &= (active.value & -active.value) - 1
    return candidates & -candidates


def _enter_software_interrupt(state: State, mask: int) -> None:
    """Software-only entry helper for diagnostics/tests."""
    if mask == 0 or mask & ~0xF0000000:
        raise ValueError("unsupported software interrupt source")
    _enter_interrupt(state, mask)


def _enter_interrupt(state: State, mask: int) -> None:
    """Functional L1-IVT entry, preserving the guest's stacks and masks."""
    if not state.stack_model or not state.bank_model:
        raise ValueError(
            "software interrupt entry requires architectural stacks and banks"
        )
    if mask == 0 or mask & (mask - 1) or mask & ~0xF0400800:
        raise ValueError("unsupported interrupt source")
    if state.pending or state.loops or state.pc_stack_pending >= 0:
        raise ValueError("software interrupt entry during deferred control effect")
    if len(state.pc_stack) >= 30 or len(state.status_stack) >= 15:
        raise ValueError("interrupt entry stack overflow is not modeled")
    sysctl = _dm_read(state, 0x30024, 4)
    if not isinstance(sysctl, Const) or sysctl.value & 0xC != 4:
        raise ValueError("software interrupt IVT location is not modeled")
    mode = _ureg(state.uregs, UREG_CODES["MODE1"])
    mmask = _ureg(state.uregs, UREG_CODES["MMASK"])
    latch = _ureg(state.uregs, UREG_CODES["IRPTL"])
    active = _ureg(state.uregs, UREG_CODES["IMASKP"])
    if (
        not isinstance(mode, Const)
        or not isinstance(mmask, Const)
        or not isinstance(latch, Const)
        or not isinstance(active, Const)
    ):
        raise ValueError("unknown software interrupt entry state")
    _push_pc_stack(state, 0x01000000 | (state.pc_sw & 0xFFFFFF))
    state.status_stack.append(
        (
            _ureg_raw(state.uregs, UREG_CODES["ASTATX"]),
            _ureg_raw(state.uregs, UREG_CODES["ASTATY"]),
            mode,
        )
    )
    _sync_status_stack(state)
    _write_ureg(state, UREG_CODES["MODE1"], Const(mode.value & ~mmask.value))
    state.uregs[UREG_CODES["IRPTL"]] = Const(latch.value & ~mask)
    state.uregs[UREG_CODES["IMASKP"]] = Const(active.value | mask)
    code = UREG_CODES["STKYX"]
    state.uregs[code] = _bitwise(
        _ureg(state.uregs, code), Const(1 << 24), "status stack nonempty", _op_andnot
    )
    level = 0
    for bit in range(32):
        if mask == 1 << bit:
            level = bit
    state.pc_sw = 0x90000 + level * 4
    # Entry is an architectural transition, not an executed instruction.
    # Its pipeline latency settles bank selection before the vector executes.
    _bank_complete(state)
    _bank_complete(state)


def _core_timer_tick(state: State) -> bool:
    """PRM chapter 5 countdown/reload; run policy supplies one functional clock.

    Pipeline TIMEN latency is omitted. Explicit timer-register writes in
    this instruction win over countdown/reload. No clock on a trapped instruction.
    """
    if state.timer_written:
        state.timer_written = False
        return False
    mode = _ureg(state.uregs, UREG_CODES["MODE2"])
    if not isinstance(mode, Const):
        raise ValueError("unknown timer enable state")
    if not mode.value & 0x20:
        return False
    count = _ureg(state.uregs, UREG_CODES["TCOUNT"])
    if not isinstance(count, Const):
        raise ValueError("unknown timer count")
    value = count.value
    if value == 0:
        period = _ureg(state.uregs, UREG_CODES["TPERIOD"])
        if not isinstance(period, Const):
            raise ValueError("unknown timer period")
        value = period.value
    else:
        value -= 1
    if value == 0:
        latch = _ureg(state.uregs, UREG_CODES["IRPTL"])
        if not isinstance(latch, Const):
            raise ValueError("unknown timer interrupt latch")
        _write_ureg(state, UREG_CODES["IRPTL"], Const(latch.value | 0x00400800))
    state.uregs[UREG_CODES["TCOUNT"]] = Const(value)
    return value == 0


def _start_counted_loop(state: State, insn: Instruction, count: int) -> list[State]:
    if count == 0:
        return [_stop(state, insn, "unsupported zero-count Type12a loop")]
    reladdr = (_field(insn.fields, "reladdr[22:16]") << 16) | _field(
        insn.fields, "reladdr[15:0]"
    )
    if insn.length_bytes is None:
        raise ValueError("cannot start a loop from an instruction without a length")
    end_sw = state.pc_sw + _signed(reladdr, 23)
    start_sw = state.pc_sw + _instruction_stride(state.pc_sw, insn.length_bytes)
    mode = _field(insn.fields, "mode")
    state.uregs[UREG_CODES["LCNTR"]] = Const(count)
    state.uregs[UREG_CODES["CURLCNTR"]] = Const(count)
    stkyx_code = UREG_CODES["STKYX"]
    state.uregs[stkyx_code] = _bitwise(
        _ureg(state.uregs, stkyx_code),
        Const(1 << 26),
        "loop stacks nonempty",
        _op_andnot,
    )
    if state.stack_model:
        if state.loop_depth != len(state.loops):
            raise ValueError("DO with reserved loop resources is not modeled")
        _push_loop_resource(state)
        state.loop_slots[state.loop_depth - 1] = (
            Unknown("packed DO loop address"),
            Const(count),
        )
        state.uregs[UREG_CODES["CURLCNTR"]] = Const(count)
    state.loops.append(Loop(start_sw, end_sw, count, mode))
    _push_pc_stack(state, start_sw)
    _event(
        state,
        insn,
        "loop-setup",
        start_sw=start_sw,
        end_sw=end_sw,
        count=count,
        mode=mode,
    )
    return _advance(state, insn)
