"""Compute-only forms: 2a, 2a_short, 2b, 2c, 6a_mem, 6b_shiftimm.

Each handler executes one decoded instruction and returns the successor
states. FORMS maps form names to handlers; sharc_core.forms merges the
family tables.
"""

from __future__ import annotations

from collections.abc import Mapping

from tools.sharc_dev.sharc_disasm import Instruction

from .compute import (
    Result,
    _apply_compute,
    _apply_compute_pey,
    _apply_compute_simd,
    _compute,
    _compute_simd,
    _shift_immediate_simd,
)
from .encoding import (
    _field,
)
from .forms_move import (
    _companion_transfer,
    _pm_companion_transfer,
    _type_nw_companion,
)
from .memory import (
    _access_modifier_scale,
    _dm_write,
    _load_normal_ureg,
    _post_modify_index,
)
from .sequencer import (
    _advance,
    _predicate,
    _predicate_pe,
)
from .state import (
    State,
    _copy,
    _event,
    _render,
    _simd_active,
    _snapshot_uregs,
    _stop,
    _ureg,
)
from .values import (
    Const,
    _multiply,
)


def _type6_predicates(
    state: State, cond: int, simd: bool | None
) -> tuple[bool | None, bool | None]:
    """The (PEx, PEy) execution of a Type 6 instruction with condition COND.

    SHARC+ PRM p.13-43 (Type 6a, SIMD Mode): "The instruction is executed
    in a processing element if the specified condition tests true in that
    element independent of the condition result for the other element."
    In SISD mode only PEx runs (PEy is False)."""
    if cond == 0x1F:
        return True, simd is True
    if simd is True:
        return _predicate_pe(state, cond, "x"), _predicate_pe(state, cond, "y")
    return _predicate(state, cond), False


def _apply_shift_pes(
    state: State,
    insn: Instruction,
    result_x: Result,
    result_y: Result | None,
    run_x: bool,
    run_y: bool,
) -> None:
    if run_x:
        _apply_compute(state, insn, result_x)
    if run_y and result_y is not None:
        _apply_compute_pey(state, insn, result_y)


def _type_6b_shiftimm(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """6b_shiftimm."""
    cond = _field(f, "cond")
    simd = _simd_active(state)
    if cond != 0x1F and simd is None:
        return [_stop(state, insn, "unsupported conditional SIMD Type6b")]
    run_x, run_y = _type6_predicates(state, cond, simd)
    if run_x is None or run_y is None:
        return [_stop(state, insn, "unknown conditional Type6b predicate")]
    if not run_x and not run_y:
        _event(
            state,
            insn,
            "type6b-skipped",
            condition=cond,
            predicate_assumption=False,
        )
        return _advance(state, insn)
    try:
        result_x, result_y = _shift_immediate_simd(
            state, f, _snapshot_uregs(state.uregs), state.special
        )
    except ValueError as error:
        return [_stop(state, insn, str(error))]
    _apply_shift_pes(state, insn, result_x, result_y, run_x, run_y)
    return _advance(state, insn)


def _type_6a_mem(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """6a_mem."""
    # PRM p.13-42..13-44 Type 6a: a ShiftImm and a normal-word memory
    # transfer in parallel, then post-modify the selected I register by M.
    # In SIMD mode the shift runs in both PEs (R and S registers) and PEy
    # transfers its complementary register at Ia+k (k=1 normal word; k=0 in
    # broadcast load mode); only PEx post-modifies I.
    cond = _field(f, "cond")
    simd = _simd_active(state)
    if cond != 0x1F and simd is None:
        return [_stop(state, insn, "unsupported conditional SIMD Type6a")]
    run_x, run_y = _type6_predicates(state, cond, simd)
    if run_x is None or run_y is None:
        return [_stop(state, insn, "unknown conditional Type6a predicate")]
    if not run_x and not run_y:
        _event(
            state,
            insn,
            "type6a-skipped",
            condition=cond,
            predicate_assumption=False,
        )
        return _advance(state, insn)
    if run_x != run_y and simd is True:
        # The memory half and the index modify are not split per element.
        return [_stop(state, insn, "mixed-PE conditional SIMD Type6a")]
    old = _snapshot_uregs(state.uregs)
    try:
        result_x, result_y = _shift_immediate_simd(state, f, old, state.special)
    except ValueError as error:
        return [_stop(state, insn, str(error))]
    bank = 8 if _field(f, "g") else 0
    index, modifier = _field(f, "i") + bank, _field(f, "m") + bank
    iv, mv = _ureg(old, 16 + index), _ureg(old, 32 + modifier)
    scale = _access_modifier_scale("normal-word", state.assume_nw32, iv)
    scaled_mv = _multiply(mv, Const(scale), "M%d * %d" % (modifier, scale))
    space = "PM" if bank else "DM"
    dreg = _field(f, "dreg")
    store = bool(_field(f, "d"))
    companion = _type_nw_companion(old, index, dreg, iv, store, scale)
    if store:
        value = _ureg(old, dreg)
        wrote = _dm_write(state, iv, 4, value, normal_word=True)
        _event(
            state,
            insn,
            "store",
            space=space,
            dreg="R%d" % dreg,
            value=value,
            address=iv,
            expression=_render(iv),
            concrete_write=wrote,
            addressing_mode="post-modify",
            access_width="normal-word",
        )
    else:
        loaded = _load_normal_ureg(state, space, iv, dreg)
        _event(
            state,
            insn,
            "load",
            space=space,
            dreg="R%d" % dreg,
            address=iv,
            expression=_render(iv),
            concrete_value=loaded,
            addressing_mode="post-modify",
            access_width="normal-word",
        )
    if companion is not None and (run_y or (run_x and _simd_active(state) is not True)):
        if space == "DM":
            _companion_transfer(
                state, insn, companion, store, "normal-word", old, "post-modify"
            )
        else:
            _pm_companion_transfer(state, insn, companion, store, old, "post-modify")
    state.uregs[16 + index] = _post_modify_index(
        state,
        index,
        iv,
        scaled_mv,
        scale,
        "I%d + M%d * %d" % (index, modifier, scale),
    )
    _apply_shift_pes(state, insn, result_x, result_y, run_x, run_y)
    return _advance(state, insn)


def _type_2c(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """2c."""
    # Unconditional (no cond field): a SIMD-active MODE1 duplicates
    # this onto PEy's S register file too (PRM p.101, p.3-39).
    try:
        compute_x, compute_y = _compute_simd(
            state, f, True, _snapshot_uregs(state.uregs)
        )
    except ValueError as error:
        return [_stop(state, insn, str(error))]
    if compute_x is None:
        return [_stop(state, insn, "empty short compute")]
    _apply_compute_simd(state, insn, compute_x, compute_y)
    return _advance(state, insn)


def _type_2a_short(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """2a_short, 2b."""
    # Both are 32-bit unconditional full-compute forms with no
    # condition field (Type2b: PRM prefix 0xc0, decode_table.json
    # "prm figure (overrides PGR; firmware-confirmed)"): always execute.
    # A SIMD-active MODE1 duplicates this onto PEy too (PRM p.101).
    try:
        compute_x, compute_y = _compute_simd(
            state,
            f,
            False,
            _snapshot_uregs(state.uregs),
            state.special,
        )
    except ValueError as error:
        return [_stop(state, insn, str(error))]
    if compute_x is None:
        return [_stop(state, insn, "empty full compute")]
    _apply_compute_simd(state, insn, compute_x, compute_y)
    return _advance(state, insn)


def _type_2a(
    state: State, insn: Instruction, f: Mapping[str, int], name: str
) -> list[State]:
    """2a."""
    if _simd_active(state) is True:
        # PRM Table 4-22 (p.4-55): conditional compute is independent
        # in each PE. Snapshot both files before applying either result.
        cond = _field(f, "cond")
        predicate_x = _predicate_pe(state, cond, "x")
        predicate_y = _predicate_pe(state, cond, "y")
        if predicate_x is None or predicate_y is None:
            return [_stop(state, insn, "unknown conditional SIMD Type2a predicate")]
        if not predicate_x and not predicate_y:
            _event(state, insn, "compute-skipped", condition=cond)
            return _advance(state, insn)
        try:
            compute_x, compute_y = _compute_simd(
                state,
                f,
                False,
                _snapshot_uregs(state.uregs),
                state.special,
            )
        except ValueError as error:
            return [_stop(state, insn, str(error))]
        if compute_x is None or compute_y is None:
            return [_stop(state, insn, "empty conditional SIMD compute")]
        if predicate_x:
            _apply_compute(state, insn, compute_x)
        if predicate_y:
            _apply_compute_pey(state, insn, compute_y)
        return _advance(state, insn)
    # Type 2a conditionally executes a full compute.  Decode against the
    # pre-instruction register file before either predicate assumption mutates it.
    try:
        compute = _compute(
            f,
            False,
            _snapshot_uregs(state.uregs),
            state.special,
        )
    except ValueError as error:
        return [_stop(state, insn, str(error))]
    if compute is None:
        return [_stop(state, insn, "empty full compute")]
    cond = _field(f, "cond")
    predicate = _predicate(state, cond)
    if predicate is True:
        _apply_compute(state, insn, compute)
        state.trace[-1].update(condition=cond, predicate_assumption=True)
        return _advance(state, insn)
    if predicate is False:
        _event(
            state,
            insn,
            "compute-skipped",
            condition=cond,
            predicate_assumption=False,
        )
        return _advance(state, insn)
    executed, skipped = _copy(state), _copy(state)
    _apply_compute(executed, insn, compute)
    executed.trace[-1].update(condition=cond, predicate_assumption=True)
    _event(
        skipped,
        insn,
        "compute-skipped",
        condition=cond,
        predicate_assumption=False,
    )
    return _advance(executed, insn) + _advance(skipped, insn)


FORMS = {
    "6b_shiftimm": _type_6b_shiftimm,
    "6a_mem": _type_6a_mem,
    "2c": _type_2c,
    "2a_short": _type_2a_short,
    "2b": _type_2a_short,
    "2a": _type_2a,
}
