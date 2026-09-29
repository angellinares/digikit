"""Compute operations (ALU, multiplier, shifter, multifunction) and their
application.

The four operation-body modules below this one in the layer order --
compute_alu (cu=0), compute_mult (cu=1, plus the MRDATAMOVE encoding and
the two multiply-accumulate-into-MRF patterns), compute_shift (cu=2, plus
_shift_immediate) and compute_multi (multifunction and short compute) --
hold one handler function per operation, in tables keyed by the field bits
that select it. This module keeps ``_compute`` as the single entry point:
field extraction, and dispatch through those tables.

Three encodings are undocumented in both public sources this project cites
(PRM and PGR; see each handler's own docstring for the page-by-page
absence): multiplier opcode 0x10, shifter opcodes 0x14 and 0xb0, and the
cu=3 reserved unit's only observed opcode 0xd6. Each is still decoded (so
the instruction walk does not desync) with its result and flags left
Unknown; tests/test_sharc_compute_table.py's cross-check against
tools/sharcspec/compute_table.json treats exactly this set as expected
gaps, not as table entries the JSON has to justify.

Four field patterns are checked directly on the raw field bits, before the
mf/cu decomposition the rest of this module dispatches on, and so are not
folded into the general (cu, opcode) tables even though two of them
(0xb4, 0xb0) reuse that same cu/opcode bit layout:

- The MRDATAMOVE encoding (PRM Table 18-29) is a different field layout
  entirely (direction/opcode/rn packed under fixed bits 22:17, not
  cu/opcode/rn/rx/ry), so it cannot share the general table's key shape.
- The two multiply-accumulate-into-MRF rows (opcode 0xb4/0xb0 at cu=1)
  are checked by field pattern *before* the mf bit (22) is even read, so
  they also match when mf=1 -- i.e. they take priority over multifunction
  dispatch for that field pattern. Moving them into the post-mf cu=1
  table would silently stop intercepting that mf=1 case. This also means
  multifunction categories 0 and 1 are unreachable dead encoding space
  (shadowed by the MRDATAMOVE check above, which is checked first): a
  fact of the original if-chain's priority, preserved here rather than
  "fixed", since fixing it would change behaviour.

Moved (and reorganised, not rewritten) from tools/sharc_trace.py.
"""

from __future__ import annotations

from collections.abc import Callable, Mapping

from sharc_disasm import Instruction

from .compute_alu import ALU_OPS, dual_add_subtract
from .compute_mult import (
    MR_DATAMOVE_REGISTERS,
    MULT_FIXED_OPS,
    MULT_OPS,
    MULTIPLY_ACCUMULATE_MRF,
    MULTIPLY_ADD_MRF,
    _mr_data_move,
    mult_fixed,
)
from .compute_multi import (
    MULTIFN_MUL_ALU_OPS,
    SHORT_OPS,
    MultifnOperands,
    multifn_dual_mul_add_subtract,
)
from .compute_shift import SHIFT_OPS, _field_deposit_or, _shift_immediate
from .encoding import (
    ALU_FLAGS_MASK,
    MI_BIT,
    MU_BIT,
    MV_BIT,
    SHIFT_FLAGS_MASK,
    UREG_CODES,
    _field,
)
from .flags import _astatx_define
from .state import (
    MR,
    State,
    _event,
    _json_value,
    _pey_special,
    _pey_view,
    _simd_active,
    _ureg,
    _ureg_raw,
)
from .values import (
    ComputeDest,
    ComputeValue,
    FlagUpdate,
    Operand,
    Unknown,
    Value,
    _apply_flag_update,
    _astatx_known_bit,
    _flags_forget,
)

# MULT_OPS handlers may return a full 80-bit MR value (state.py, a layer
# above values.py's Operand/ComputeResult by design -- values.py cannot
# import MR without an import cycle), so this module's own dispatch/apply
# functions use this locally widened result type instead of values.py's
# ComputeResult for every compute unit they handle, not just cu=1.
Result = tuple[ComputeDest, "ComputeValue | MR", str, FlagUpdate]

__all__ = [
    "MR_DATAMOVE_REGISTERS",
    "_apply_compute",
    "_apply_compute_pey",
    "_apply_compute_simd",
    "_compute",
    "_compute_pey",
    "_compute_pey_values",
    "_compute_simd",
    "_field_deposit_or",
    "_mr_data_move",
    "_shift_immediate",
]


# PRM top-level compute selector (tools/sharcspec/compute_table.json's
# top_level_structure.single_function_selector): the reserved cu=3 (called
# "cu=11" there, its 2-bit field value) is "not used by SINGLEFN" -- no
# compute unit is documented for it, in either public source. Two opcodes
# this image's coverage scan actually found are decoded (like
# compute_shift.py's undocumented 0xb0/0x14 and compute_mult.py's
# undocumented 0x10), rather than guessed at; every other cu=3 opcode
# still raises, since there is no evidence it is real or what it would
# mean:
#   - 0xd6, `sw 0x1c32ba` in FUN_1c3289, form 7a.
#   - 0xe0, `sw 0x1c32b1`, TWO INSTRUCTIONS EARLIER in the SAME function,
#     form 1a. Lane F1 checked the SC58x/2158x PRM's 64-bit floating-point
#     chapters (out/refs/sc58x-2158x-prm) specifically for this one, since
#     0xe0 is also the opcode the real per-machine synthesis path hits
#     once the master-bus table is unmasked -- they do not explain it: the
#     manual's 64-bit float ops occupy cu=0 opcode 0x11-0x1f (Table 18-6,
#     compute_alu.py's ALU_OPS) and cu=1 opcode 0x31-0x33 (Table 18-8,
#     compute_mult.py's MULT_OPS), never cu=3, and tools/sharcspec/
#     compute_table.json's own top_level_structure confirms cu=11 has no
#     "format" row at all. Neither opcode is used by any of the 64-bit ops
#     this project implemented from that chapter, in any of dt2-1.16,
#     dt2-1.15C, dn2-1.11 or dn2-1.10E's aligned, in-function code either
#     (same census). Both instructions sit in a run bracketed by an index
#     clamped to 0-15 (0x1c32a0-0x1c32a9), the undocumented multiplier
#     opcode 0x10 (0x1c32ab), a USTAT2 read (0x1c32ad), an undocumented
#     16-bit form (0x1c32b0, operand 0x25) and an undocumented 48-bit form
#     (0x1c32b4) -- a cluster worth treating as "possibly a table read
#     through the instruction stream, not real compute" in a future lane,
#     not chased further here.
def _reserved_cu3(opcode: int, rn: int, rx: int, ry: int) -> tuple:
    label = (
        "reserved compute unit cu=3 opcode=%#x R%d, R%d "
        "(PRM: cu=11 not used by SINGLEFN; not the SC58x/2158x PRM's "
        "64-bit float ops either, which live at cu=0/cu=1)" % (opcode, rx, ry)
    )
    return (
        rn,
        Unknown(label),
        "compute-reserved-cu3",
        _flags_forget(ALU_FLAGS_MASK | SHIFT_FLAGS_MASK),
    )


def _reserved_cu3_d6(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    return _reserved_cu3(0xD6, rn, rx, ry)


def _reserved_cu3_e0(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    return _reserved_cu3(0xE0, rn, rx, ry)


CU3_OPS: dict[int, Callable] = {
    0xD6: _reserved_cu3_d6,
    0xE0: _reserved_cu3_e0,
}

# REGF_STKYX/REGF_STKYY sticky bit positions this module latches for
# multiplier ops (SHARC+ PRM p.28-83/28-84, Table 28-46): MOS latches the
# fixed-point multiplier's overflow (ASTATX.MV) -- PRM Table 3-7's STKY
# columns are all "-" except MOS, "**" (sets but does not clear), on every
# multiply/accumulate/subtract/round row. MUS/MVS/MIS instead latch the
# floating-point multiply's MU/MV/MI (PRM Table 3-9); fixed-point ops never
# touch those three, and the float row never touches MOS.
_MOS_BIT, _MVS_BIT, _MUS_BIT, _MIS_BIT = 6, 7, 8, 9
_MULT_MOS_OPERATIONS = frozenset(
    {
        "multiply",
        "multiply-mrf",
        "multiply-mrb",
        "multiply-accumulate",
        "multiply-subtract",
        "round-mrf",
        "round-mrb",
    }
)


def _sticky_set(old: Value, bit: int, trigger: bool | None) -> Value:
    """A sticky bit only ever moves 0->1 (PRM p.28-82: "sticky bits do not
    clear themselves after the condition is no longer true"); an
    instruction that does not concretely trigger BIT (TRIGGER False or
    unknown) leaves OLD's bit exactly as it already was."""
    if trigger is not True:
        return old
    return _astatx_define(old, 1 << bit, 1 << bit)


def _apply_mult_sticky(
    state: State, stky_name: str, astatx: Value, operation: str
) -> None:
    """Latch REGF_STKYX/REGF_STKYY from the ASTATX/ASTATY this instruction
    just computed (see ``_MOS_BIT`` group's docstring): a multiplier op's
    sticky bits are purely a function of its own new ASTATX bits, so no
    extra threading through the (Dest, Value, operation, astatx_update)
    ``ComputeResult`` contract every compute handler returns is needed."""
    code = UREG_CODES[stky_name]
    if operation == "float-multiply":
        value = _ureg_raw(state.uregs, code)
        value = _sticky_set(value, _MUS_BIT, _astatx_known_bit(astatx, MU_BIT))
        value = _sticky_set(value, _MVS_BIT, _astatx_known_bit(astatx, MV_BIT))
        value = _sticky_set(value, _MIS_BIT, _astatx_known_bit(astatx, MI_BIT))
    elif operation in _MULT_MOS_OPERATIONS:
        value = _ureg_raw(state.uregs, code)
        value = _sticky_set(value, _MOS_BIT, _astatx_known_bit(astatx, MV_BIT))
    else:
        return
    state.uregs[code] = value


# (cu, opcode) -> operation table, one per compute unit; cu itself selects
# which table (PRM top-level compute selector, tools/sharcspec/
# compute_table.json's top_level_structure.single_function_selector: 0=ALU,
# 1=multiplier, 2=shifter, 3=reserved).
_SINGLE_FUNCTION_TABLES: dict[int, dict[int, Callable]] = {
    0: ALU_OPS,
    1: MULT_OPS,
    2: SHIFT_OPS,
    3: CU3_OPS,
}


def _compute(
    f: Mapping[str, int],
    short: bool,
    values: Mapping[int, Value],
    special: Mapping[str, Operand | MR] | None = None,
    *,
    approx_recips: bool = False,
) -> Result | None:
    """Decode the small public-table subset, reading every operand from VALUES.

    The 4th element of a non-None result is an ASTATX updater: a function
    from the old ASTATX Value to the new one, computed here (where the
    operands are in scope) and applied by ``_apply_compute``.
    """
    field = (
        _field(f, "compute")
        if short
        else ((_field(f, "compute[22:16]") << 16) | _field(f, "compute[15:0]"))
    )
    if not short and field == 0:
        return None
    # PRM Table 18-29: fixed bits 22:17=100000 select an MR data move.
    # The target-guided SPORT setup path uses the register-to-MR direction.
    if not short and field >> 17 == 0b100000:
        direction = (field >> 16) & 1
        opcode = (field >> 12) & 0xF
        rn = (field >> 8) & 0xF
        mr_name = MR_DATAMOVE_REGISTERS.get(opcode)
        if mr_name is None:
            raise ValueError("unsupported MR data move %#x" % field)
        return _mr_data_move(mr_name, rn, direction, values, special)
    # PRM multiplier compute table: MRF = MRF + RX * RY (MOD1).  Preserve
    # the accumulator separately from the UREG file so later MR transfers do
    # not masquerade as architectural UREGs.
    if not short and ((field >> 20) & 3) == 1 and ((field >> 12) & 0xFF) == 0xB4:
        rx, ry = (field >> 4) & 0xF, field & 0xF
        left, right = _ureg(values, rx), _ureg(values, ry)
        return mult_fixed(MULTIPLY_ACCUMULATE_MRF, 0, rx, ry, left, right, special)
    if not short and ((field >> 20) & 3) == 1 and ((field >> 12) & 0xFF) == 0xB0:
        rn, rx, ry = (field >> 8) & 0xF, (field >> 4) & 0xF, field & 0xF
        left, right = _ureg(values, rx), _ureg(values, ry)
        return mult_fixed(MULTIPLY_ADD_MRF, rn, rx, ry, left, right, special)
    if short:
        opcode, rn, rx = (field >> 8) & 0xF, (field >> 4) & 0xF, field & 0xF
        left, right = _ureg(values, rn), _ureg(values, rx)
        short_handler = SHORT_OPS.get(opcode)
        if short_handler is None:
            raise ValueError("unsupported short compute opcode %#x" % opcode)
        return short_handler(rn, rx, left, right)
    # PRM Table 18-1/Figure 18-1 (p.423): bit22 is MF, the multifunction
    # selector. A multifunction op's register sub-fields (PRM Table
    # 18-15..18-19, p.434-435) do not line up with the SINGLEFN rn/rx/ry
    # layout computed below, so it is decoded separately and always
    # returns/raises before falling through to that layout.
    mf = (field >> 22) & 1
    if mf:
        category = (field >> 16) & 0x3F
        rm, ra = (field >> 12) & 0xF, (field >> 8) & 0xF
        # PRM Table 18-16/18-17 (p.434): the four multifunction INPUT
        # operands are 2-bit fields, each selecting within a fixed quad --
        # Fxm in F0-3, Fym in F4-7, Fxa in F8-11, Fya in F12-15.
        rxm_reg = (field >> 6) & 0x3
        rym_reg = 4 + ((field >> 4) & 0x3)
        rxa_reg = 8 + ((field >> 2) & 0x3)
        rya_reg = 12 + (field & 0x3)
        fxm, fym = _ureg(values, rxm_reg), _ureg(values, rym_reg)
        fxa, fya = _ureg(values, rxa_reg), _ureg(values, rya_reg)
        operands = MultifnOperands(
            rm, ra, rxm_reg, rym_reg, rxa_reg, rya_reg, fxm, fym, fxa, fya
        )
        multifn_handler = MULTIFN_MUL_ALU_OPS.get(category)
        if multifn_handler is not None:
            return multifn_handler(category, operands)
        if (category >> 4) == 0b11:
            return multifn_dual_mul_add_subtract(category, operands)
        raise ValueError(
            "unsupported multifunction category=%#04x rm=%d ra=%d" % (category, rm, ra)
        )
    cu, opcode = (field >> 20) & 3, (field >> 12) & 0xFF
    rn, rx, ry = (field >> 8) & 0xF, (field >> 4) & 0xF, field & 0xF
    left, right = _ureg(values, rx), _ureg(values, ry)
    # PRM Table 18-10 (p.433) / Table 18-13 (p.434): Dual Add/Subtract is a
    # single-function ALU op (mf=0, not multifunction) whose opcode top
    # nibble (bits 19:16, i.e. this OPCODE's top nibble) is 0111 (fixed) or
    # 1111 (float); the low nibble (bits 15:12) is not part of the opcode at
    # all -- it is RS, a second 4-bit result register alongside
    # RA=RN/FN at bits 11:8 (already read above as RN).
    if cu == 0 and (opcode >> 4) in (0x7, 0xF):
        float_form = (opcode >> 4) == 0xF
        rs = opcode & 0xF
        return dual_add_subtract(rn, rs, rx, ry, left, right, float_form)
    if cu == 1:
        spec = MULT_FIXED_OPS.get(opcode)
        if spec is not None:
            return mult_fixed(spec, rn, rx, ry, left, right, special)
    table = _SINGLE_FUNCTION_TABLES.get(cu)
    single_handler = table.get(opcode) if table else None
    if single_handler is None:
        raise ValueError("unsupported full compute cu=%#x opcode=%#x" % (cu, opcode))
    return single_handler(rn, rx, ry, left, right, values, special, approx_recips)


def _apply_compute(
    state: State,
    insn: Instruction,
    result: Result,
) -> None:
    rn, value, operation, astatx_update = result
    astatx_code = UREG_CODES["ASTATX"]
    new_astatx = _apply_flag_update(_ureg_raw(state.uregs, astatx_code), astatx_update)
    state.uregs[astatx_code] = new_astatx
    _apply_mult_sticky(state, "STKYX", new_astatx, operation)
    if isinstance(rn, str):
        # A string RN never pairs with a tuple VALUE (see the tuple-RN
        # branch below for the only case that returns more than one
        # value); state this real correlation explicitly since ComputeDest
        # and ComputeValue vary independently in the type system.
        assert not isinstance(value, tuple), "string RN with tuple value"
        _event(
            state,
            insn,
            "compute",
            operation=operation,
            result_register=rn,
            value=value,
        )
        # Every string destination (MRF/MRB, "BFFWRP", ...) is its own
        # special-dict slot, keyed by the name the caller returned.
        state.special[rn] = value
        return
    if isinstance(rn, tuple):
        # Dual/triple-result compute (dual add/subtract, MUL/ALU
        # multifunction, MUL dual add/subtract, BITEXT's paired RN+BFFWRP
        # write): destinations sharing one ASTATX update, already combined
        # by the caller (PRM p.3-21/3-22). A string destination is a
        # special-dict slot (as in the single-string case above); an int is
        # an ordinary register.
        assert isinstance(value, tuple), "tuple RN without tuple value"
        _event(
            state,
            insn,
            "compute",
            operation=operation,
            result_register=[
                reg if isinstance(reg, str) else "R%d" % reg for reg in rn
            ],
            value=[_json_value(v) for v in value],
        )
        for reg, val in zip(rn, value, strict=True):
            if isinstance(reg, str):
                state.special[reg] = val
            else:
                state.uregs[reg] = val
        return
    assert not isinstance(value, tuple), "int RN with tuple value"
    # Only a string (MRF/MRB/...) or tuple destination ever carries an
    # MR value (see the two branches above); an int RN is always an
    # ordinary 32-bit register.
    assert not isinstance(value, MR), "int RN with MR value"
    if operation in ("compare", "bit-test", "float-compare"):
        _event(state, insn, "compute", operation=operation, status_only=True)
    else:
        _event(
            state,
            insn,
            "compute",
            operation=operation,
            result_register="R%d" % rn,
            value=value,
        )
        state.uregs[rn] = value
    if operation in ("float-recips-seed-approx", "float-rsqrts-seed-approx"):
        # --approx-recips produced a numeric value with no ROM table behind
        # it (see _approx_recips); mark this path and this instant so any
        # report can find and discount it, matching how "predicate-assumption"
        # tags a forked branch guess.
        state.approx_recips_used = True
        _event(state, insn, "approximate-recips", value=value)


def _compute_pey_values(values: Mapping[int, Value]) -> dict[int, Value]:
    """A PEy view of the register file for _compute: R/F codes 0-15 read
    the paired S/SF register instead (SHARC+ PRM p.3-39, "Compute
    Instructions in SIMD Mode": "S0 = S1 + S2; /* implicit ALU instruction
    */" -- the PEy compute is decoded from the *same* instruction bits as
    PEx, just re-targeted at the S file, so re-running _compute unchanged
    against a shifted register map is exactly this rule)."""
    return _pey_view(values)


def _compute_pey(
    f: Mapping[str, int],
    short: bool,
    values: Mapping[int, Value],
    special: Mapping[str, Operand | MR] | None = None,
    *,
    approx_recips: bool = False,
) -> Result | None:
    """PEy's half of a SIMD compute (SHARC+ PRM p.101, "SIMD Mode":
    "Executes the same instruction simultaneously in both processing
    elements"), decoded against the S/SF register file and the PEy
    multiplier accumulator (PRM p.101, "Multiplier Result Register Swap":
    "swapping also occurs with the PEY unit based registers (REGF_MS0F,
    REGF_MS2F, and REGF_MS0B, REGF_MS2B)"). _compute always names its MR
    data-move/multiply-accumulate destination "MRF" regardless of PE (see
    its own docstring); disguise state.special["MSF"] as "MRF" on the way
    in so that accumulator read is correct, and _apply_compute_pey below
    undoes the disguise on the way out.
    """
    pey_special = _pey_special(special)
    return _compute(
        f,
        short,
        _compute_pey_values(values),
        pey_special,
        approx_recips=approx_recips,
    )


def _apply_compute_pey(
    state: State,
    insn: Instruction,
    result: Result,
) -> None:
    """PEy's half of _apply_compute: the identical shape, redirected to the
    S/SF register file, REGF_ASTATY, and the PEy multiplier accumulator
    (SHARC+ PRM p.66, Table 3-1: ASTATx/ASTATy and STKYx/STKYy are the
    per-PE computation-status register pairs)."""
    rn, value, operation, astatx_update = result
    astaty_code = UREG_CODES["ASTATY"]
    new_astaty = _apply_flag_update(_ureg_raw(state.uregs, astaty_code), astatx_update)
    state.uregs[astaty_code] = new_astaty
    _apply_mult_sticky(state, "STKYY", new_astaty, operation)
    if isinstance(rn, str):
        # A string RN never pairs with a tuple VALUE, matching
        # _apply_compute's own invariant (see its comment there).
        assert not isinstance(value, tuple), "string RN with tuple value"
        _event(
            state,
            insn,
            "compute-pey",
            operation=operation,
            result_register="MSF",
            value=value,
        )
        state.special["MSF"] = value
        return
    if isinstance(rn, tuple):
        assert isinstance(value, tuple), "tuple RN without tuple value"
        # Unlike _apply_compute, no PEy result names a special-dict
        # (string) destination in a tuple result: only the multifunction
        # dual/triple ALU rows produce a tuple RN here, and their
        # destinations are always ordinary S-register numbers.
        names = []
        for reg in rn:
            assert isinstance(reg, int), "PEy tuple RN must be int-only"
            names.append("S%d" % reg)
        _event(
            state,
            insn,
            "compute-pey",
            operation=operation,
            result_register=names,
            value=[_json_value(v) for v in value],
        )
        for reg, val in zip(rn, value, strict=True):
            assert isinstance(reg, int), "PEy tuple RN must be int-only"
            state.uregs[80 + reg] = val
        return
    assert not isinstance(value, tuple), "int RN with tuple value"
    # Only a string (MRF/MRB/...) or tuple destination ever carries an
    # MR value (see the two branches above); an int RN is always an
    # ordinary 32-bit register.
    assert not isinstance(value, MR), "int RN with MR value"
    if operation in ("compare", "bit-test", "float-compare"):
        _event(state, insn, "compute-pey", operation=operation, status_only=True)
    else:
        _event(
            state,
            insn,
            "compute-pey",
            operation=operation,
            result_register="S%d" % rn,
            value=value,
        )
        state.uregs[80 + rn] = value
    if operation in ("float-recips-seed-approx", "float-rsqrts-seed-approx"):
        state.approx_recips_used = True
        _event(state, insn, "approximate-recips-pey", value=value)


def _compute_simd(
    state: State,
    f: Mapping[str, int],
    short: bool,
    values: Mapping[int, Value],
    special: Mapping[str, Operand | MR] | None = None,
    *,
    approx_recips: bool = False,
) -> tuple[
    Result | None,
    Result | None,
]:
    """Decode a compute for PEx, and for PEy too when MODE1.PEYEN is
    concretely set (SHARC+ PRM p.101, "SIMD Mode": "Dispatches a single
    instruction to both processing element's computational units").  SISD
    mode, or an unresolved MODE1, returns a PEy result of None -- exactly
    like SISD, the tracer does not invent a PEy effect it cannot confirm.
    """
    result_x = _compute(f, short, values, special, approx_recips=approx_recips)
    if _simd_active(state) is not True:
        return result_x, None
    result_y = _compute_pey(f, short, values, special, approx_recips=approx_recips)
    return result_x, result_y


def _apply_compute_simd(
    state: State,
    insn: Instruction,
    result_x: Result | None,
    result_y: Result | None,
) -> None:
    if result_x is not None:
        _apply_compute(state, insn, result_x)
    if result_y is not None:
        _apply_compute_pey(state, insn, result_y)
