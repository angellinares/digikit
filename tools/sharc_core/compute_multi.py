"""Multifunction compute (mf=1) and short compute (the compact 16-bit
Type 2c encoding): handler bodies dispatched by
tools/sharc_core/compute.py's SHORT_OPS and MULTIFN_MUL_ALU_OPS tables
(plus multifn_dual_mul_add_subtract, which compute.py calls directly for
the category range match its own docstring explains).

Handler bodies moved verbatim from the ``if short:`` and ``if mf:``
branches of tools/sharc_trace.py's old _compute if-chain; each still
carries that branch's own PRM/PGR citation.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import NamedTuple

from .flags import (
    MULT_FLAGS_FORGET,
    FlagUpdate,
    _astatx_alu_arith,
    _astatx_alu_logical,
    _astatx_compare,
    _astatx_compare_float,
    _compare_flags,
    _compare_flags_float,
    _flags_then,
    _float_alu_updates,
    _or_updates,
)
from .floats import (
    _f_add,
    _f_avg,
    _f_sub,
    _fixed_to_float,
    _fixed_to_float_scaled,
    _float32,
    _float32_bits,
    _float_binary,
    _float_max,
    _float_min,
    _float_unary,
)
from .values import (
    Const,
    Unknown,
    Value,
    _add,
    _bitwise,
    _multiply,
    _not,
    _op_and,
    _op_or,
    _op_xor,
    _subtract,
)

ShortHandler = Callable[[int, int, Value, Value], tuple]


# ---------------------------------------------------------------------------
# Short compute (Type 2c): opcode, rn, rx, RN's value ("left"), RX's value
# ("right") -- note RN doubles as both an input and the destination here
# (PRM Table 18-22: "RN = RN op RX"), unlike full compute's RX/RY inputs.
# ---------------------------------------------------------------------------


def short_add(rn, rx, left, right) -> tuple:
    value = _add(left, right, "R%d + R%d" % (rn, rx))
    return rn, value, "add", _astatx_alu_arith(left, right, False)


def short_subtract(rn, rx, left, right) -> tuple:
    same_source = rn == rx
    value = _subtract(left, right, "R%d - R%d" % (rn, rx), same_source=same_source)
    return (
        rn,
        value,
        "subtract",
        _astatx_alu_arith(left, right, True, same_source=same_source),
    )


def short_pass(rn, rx, left, right) -> tuple:
    return rn, right, "pass", _astatx_alu_logical(right)


def short_not(rn, rx, left, right) -> tuple:
    value = _not(right, "not R%d" % rx)
    return rn, value, "not", _astatx_alu_logical(value)


def short_increment(rn, rx, left, right) -> tuple:
    value = _add(right, Const(1), "R%d + 1" % rx)
    return rn, value, "increment", _astatx_alu_arith(right, Const(1), False)


def short_decrement(rn, rx, left, right) -> tuple:
    value = _add(right, Const(-1), "R%d - 1" % rx)
    return rn, value, "decrement", _astatx_alu_arith(right, Const(1), True)


def short_multiply(rn, rx, left, right) -> tuple:
    value = _multiply(left, right, "R%d * R%d" % (rn, rx))
    return rn, value, "multiply", MULT_FLAGS_FORGET


def _short_logical_impl(rn, rx, left, right, name: str, operation) -> tuple:
    value = _bitwise(left, right, "R%d %s R%d" % (rn, name, rx), operation)
    return rn, value, name, _astatx_alu_logical(value)


def short_and(rn, rx, left, right) -> tuple:
    return _short_logical_impl(rn, rx, left, right, "and", _op_and)


def short_or(rn, rx, left, right) -> tuple:
    return _short_logical_impl(rn, rx, left, right, "or", _op_or)


def short_xor(rn, rx, left, right) -> tuple:
    return _short_logical_impl(rn, rx, left, right, "xor", _op_xor)


# PRM ShortCompute table (p. 17-3): 0011 is the signed comp(RN, RX).
def short_compare(rn, rx, left, right) -> tuple:
    value = _compare_flags(left, right, True, "comp R%d, R%d" % (rn, rx))
    return rn, value, "compare", _astatx_compare(value)


# PRM Table 18-2 (p.423-425)/PGR "Short Compute Opcodes"
# (pgr.txt:23108-23120): 1000-1011 and 1111 are the float
# ShortCompute rows -- the same ops as the full-compute float table
# above, just the compact 16-bit Type 2c encoding where RN doubles
# as both the Y input and the result (Table 18-22: "RN = RN op RX").
def short_float_add(rn, rx, left, right) -> tuple:
    value, overflow, invalid = _float_binary(
        left, right, "F%d + F%d" % (rn, rx), _f_add
    )
    return (
        rn,
        value,
        "float-add",
        _float_alu_updates(value, av=overflow, ai=invalid),
    )


def short_float_subtract(rn, rx, left, right) -> tuple:
    value, overflow, invalid = _float_binary(
        left, right, "F%d - F%d" % (rn, rx), _f_sub
    )
    return (
        rn,
        value,
        "float-subtract",
        _float_alu_updates(value, av=overflow, ai=invalid),
    )


# FN = float RX: unlike the other short float rows, RN is not
# read as an input here (only RX is converted); RN is purely the
# destination.
def short_float_convert(rn, rx, left, right) -> tuple:
    value, invalid = _fixed_to_float(right, "float R%d" % rx)
    return (
        rn,
        value,
        "float-convert",
        _float_alu_updates(value, av=False, ai=invalid),
    )


def short_float_compare(rn, rx, left, right) -> tuple:
    label = "comp F%d, F%d" % (rn, rx)
    value, invalid = _compare_flags_float(left, right, label)
    return rn, value, "float-compare", _astatx_compare_float(value, invalid)


def short_float_multiply(rn, rx, left, right) -> tuple:
    a, b = _float32(left), _float32(right)
    if a is None or b is None:
        value: Value = Unknown("F%d * F%d" % (rn, rx))
    else:
        bits, _overflowed = _float32_bits(a * b)
        value = Const(bits)
    return rn, value, "float-multiply", MULT_FLAGS_FORGET


SHORT_OPS: dict[int, ShortHandler] = {
    0x0: short_add,
    0x1: short_subtract,
    0x2: short_pass,
    0x3: short_compare,
    0x4: short_not,
    0x5: short_increment,
    0x6: short_decrement,
    0x7: short_multiply,
    0x8: short_float_add,
    0x9: short_float_subtract,
    0xA: short_float_convert,
    0xB: short_float_compare,
    0xC: short_and,
    0xD: short_or,
    0xE: short_xor,
    0xF: short_float_multiply,
}


# ---------------------------------------------------------------------------
# Multifunction (mf=1): MUL/ALU (PGR Table 12-12, pgr.txt:23129-23198) and
# the dual add/subtract twin (PRM ch.24 p.528-529).
# ---------------------------------------------------------------------------


class MultifnOperands(NamedTuple):
    """The four multifunction INPUT operands (PRM Table 18-16/18-17,
    p.434: Fxm in F0-3, Fym in F4-7, Fxa in F8-11, Fya in F12-15) and the
    two OUTPUT register numbers (rm, ra), decoded once by compute.py
    regardless of which multifunction category follows."""

    rm: int
    ra: int
    rxm_reg: int
    rym_reg: int
    rxa_reg: int
    rya_reg: int
    fxm: Value
    fym: Value
    fxa: Value
    fya: Value


MultifnHandler = Callable[[int, MultifnOperands], tuple]


def _multifn_fm_value(op: MultifnOperands) -> Value:
    """The multiplier half shared by every MUL/ALU multifunction category:
    ordinary IEEE NaN propagation (not the ALU's NaN-input all-1s quirk),
    computed directly rather than through ``_float_binary`` -- same as the
    plain float multiply."""
    fm_a, fm_b = _float32(op.fxm), _float32(op.fym)
    if fm_a is None or fm_b is None:
        return Unknown("F%d * F%d" % (op.rxm_reg, op.rym_reg))
    fm_bits, _fm_overflowed = _float32_bits(fm_a * fm_b)
    return Const(fm_bits)


def _multifn_result(
    op: MultifnOperands, fa_value: Value, fa_updates: FlagUpdate, operation: str
) -> tuple:
    fm_value = _multifn_fm_value(op)
    update = _flags_then(fa_updates, MULT_FLAGS_FORGET)
    return (op.rm, op.ra), (fm_value, fa_value), operation, update


# PGR Table 12-12 (pgr.txt:23129-23198), opcode[21:16] 011000/011001:
# FM = FXM*FYM, FA = FXA+-FYA -- the only MUL/ALU multifunction rows
# this firmware's audio code uses. Flags follow the single-function
# rule for each half (PRM p.3-21/3-22: multifunction "handle[s]
# flags in the same way as the single function computations" except
# for dual add/subtract), so the ALU half reuses
# ``_float_alu_updates`` and the multiplier half stays forgotten via
# ``MULT_FLAGS_FORGET`` exactly as the plain float multiply below.
def multifn_add_subtract(category: int, op: MultifnOperands) -> tuple:
    subtract = category == 0x19
    fa_op = _f_sub if subtract else _f_add
    fa_value, fa_overflow, fa_invalid = _float_binary(
        op.fxa,
        op.fya,
        "F%d %s F%d" % (op.rxa_reg, "-" if subtract else "+", op.rya_reg),
        fa_op,
    )
    return _multifn_result(
        op,
        fa_value,
        _float_alu_updates(fa_value, av=fa_overflow, ai=fa_invalid),
        "float-mulalu-subtract" if subtract else "float-mulalu-add",
    )


# PGR Table 12-12 opcode 011010 (p.587): FM = FXM*FYM,
# FA = FLOAT RXA by RYA -- the ALU half is the scaled
# fixed->float convert (0xDA in compute_alu.py), reading RXA/RYA as fixed
# rather than float registers (same physical register file
# slots, ``fxa``/``fya`` above are just the raw bit patterns).
def multifn_convert(category: int, op: MultifnOperands) -> tuple:
    fa_value, fa_overflow = _fixed_to_float_scaled(
        op.fxa, op.fya, "float R%d by R%d" % (op.rxa_reg, op.rya_reg)
    )
    return _multifn_result(
        op,
        fa_value,
        _float_alu_updates(fa_value, av=fa_overflow, ai=False),
        "float-mulalu-convert",
    )


# PGR Table 12-12 opcode 011100 (p.588): FM = FXM*FYM,
# FA = (FXA + FYA)/2 -- the ALU half is the single-function
# float average (PGR p.11-28: AV architecturally fixed 0, never
# data-dependent, unlike plain float add).
def multifn_average(category: int, op: MultifnOperands) -> tuple:
    fa_value, _fa_overflow, fa_invalid = _float_binary(
        op.fxa,
        op.fya,
        "(F%d + F%d)/2" % (op.rxa_reg, op.rya_reg),
        _f_avg,
    )
    return _multifn_result(
        op,
        fa_value,
        _float_alu_updates(fa_value, av=False, ai=fa_invalid),
        "float-mulalu-average",
    )


# PGR Table 12-12 opcode 011101 (p.588): FM = FXM*FYM,
# FA = ABS FXA -- the ALU half is the same single-function float abs as
# 0xB0 in compute_alu.py (AN fixed 0, AS carries FXA's own sign).
def multifn_abs(category: int, op: MultifnOperands) -> tuple:
    fa_value, _fa_overflow, fa_invalid = _float_unary(
        op.fxa, "abs F%d" % op.rxa_reg, abs
    )
    return _multifn_result(
        op,
        fa_value,
        _float_alu_updates(
            fa_value, av=False, an_zero=True, as_source=op.fxa, ai=fa_invalid
        ),
        "float-mulalu-abs",
    )


# PGR Table 12-12 opcodes 011110/011111 (p.588): FM = FXM*FYM,
# FA = MAX/MIN(FXA, FYA) -- the ALU half is the same
# single-function float min/max as 0xE1/0xE2 in compute_alu.py (AV
# fixed 0 there too).
def multifn_minmax(category: int, op: MultifnOperands) -> tuple:
    minimum = category == 0x1F
    name = "min" if minimum else "max"
    combine = _float_min if minimum else _float_max
    fa_value, fa_overflow, fa_invalid = _float_binary(
        op.fxa, op.fya, "%s(F%d, F%d)" % (name, op.rxa_reg, op.rya_reg), combine
    )
    return _multifn_result(
        op,
        fa_value,
        _float_alu_updates(fa_value, av=False, ai=fa_invalid),
        "float-mul" + name,
    )


MULTIFN_MUL_ALU_OPS: dict[int, MultifnHandler] = {
    0x18: multifn_add_subtract,
    0x19: multifn_add_subtract,
    0x1A: multifn_convert,
    0x1C: multifn_average,
    0x1D: multifn_abs,
    0x1E: multifn_minmax,
    0x1F: multifn_minmax,
}


# PRM ch.24 p.528-529 "Floating-Point Multiplier and ALU (dual Add
# and Subtract)": Fm=F3-0*F7-4, Fa=F11-8+F15-12, Fs=F11-8-F15-12 --
# the multifunction twin of the single-function dual add/subtract in
# compute_alu.py, for FFT butterflies. Neither PRM nor PGR prints this
# row's opcode bits (compute_table.json's multifn_mul_dual_addsub note),
# but tools/sharcdb.py's independently-built register-def/use table
# (_compute_regdef_reguse's is_dual_addsub branch) already extracts
# RS from bits 19:16 -- i.e. category's own low 4 bits, with only
# category's top 2 bits (here, "11") acting as the fixed selector --
# confirmed against this decoder's own database. FS shares FXA/FYA
# with FA (same operand pair, opposite sign), matching the
# single-function dual add/subtract's OR'd-flags convention.
#
# compute.py calls this directly for the category range match
# (category >> 4) == 0b11, after a MULTIFN_MUL_ALU_OPS lookup misses:
# it is not one fixed key but 16 (category's low nibble is RS, a third
# result register, not part of the opcode), so it cannot live as a
# single dict entry.
def multifn_dual_mul_add_subtract(category: int, op: MultifnOperands) -> tuple:
    rs = category & 0xF
    fm_value = _multifn_fm_value(op)
    add_value, add_overflow, add_invalid = _float_binary(
        op.fxa, op.fya, "F%d + F%d" % (op.rxa_reg, op.rya_reg), _f_add
    )
    sub_value, sub_overflow, sub_invalid = _float_binary(
        op.fxa, op.fya, "F%d - F%d" % (op.rxa_reg, op.rya_reg), _f_sub
    )
    updates = _or_updates(
        _float_alu_updates(add_value, av=add_overflow, ai=add_invalid),
        _float_alu_updates(sub_value, av=sub_overflow, ai=sub_invalid),
    )
    return (
        (op.rm, op.ra, rs),
        (fm_value, add_value, sub_value),
        "float-mul-dual-add-subtract",
        _flags_then(updates, MULT_FLAGS_FORGET),
    )
