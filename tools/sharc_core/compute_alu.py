"""Fixed and float ALU compute (cu=0): handler bodies dispatched by
tools/sharc_core/compute.py's ALU_OPS table, plus the dual add/subtract
range match (opcode top nibble 0111/1111) that compute.py calls directly.

Handler bodies moved verbatim from the cu==0 branches of
tools/sharc_trace.py's old _compute if-chain; each still carries that
branch's own PRM/PGR citation.
"""

from __future__ import annotations

from collections.abc import Callable, Mapping

from .encoding import (
    AC_BIT,
    AF_BIT,
    AI_BIT,
    AN_BIT,
    AS_BIT,
    AV_BIT,
    AZ_BIT,
    UREG_CODES,
)
from .flags import (
    _alu_arith_updates,
    _astatx_abs,
    _astatx_alu_arith,
    _astatx_alu_arith_ci,
    _astatx_alu_logical,
    _astatx_compare,
    _astatx_compare_float,
    _compare_flags,
    _compare_flags_float,
    _double_alu_updates,
    _float_alu_updates,
    _or_updates,
)
from .floats import (
    _approx_recips,
    _double_binary,
    _double_compare,
    _double_scalb,
    _double_to_fixed,
    _double_to_float32,
    _double_unary,
    _f32_from_bits,
    _f_add,
    _f_avg,
    _f_neg,
    _f_pass,
    _f_sub,
    _fixed_to_double,
    _fixed_to_double_scaled,
    _fixed_to_float,
    _fixed_to_float_scaled,
    _float32_bits,
    _float32_to_double,
    _float_binary,
    _float_clip,
    _float_copysign,
    _float_logb,
    _float_mantissa,
    _float_max,
    _float_min,
    _float_round32,
    _float_scalb,
    _float_to_fixed,
    _float_to_fixed_trunc,
    _float_unary,
    _scale_double_input,
    _scale_fixed_input,
    _sqrt,
)
from .state import _ureg, _ureg_raw
from .values import (
    Const,
    Operand,
    Unknown,
    Value,
    _add,
    _astatx_known_bit,
    _bitwise,
    _flags_from_pairs,
    _flags_put,
    _not,
    _op_and,
    _op_or,
    _op_xor,
    _signed32,
    _subtract,
)

Handler = Callable[
    [
        int,
        int,
        int,
        Value,
        Value,
        Mapping[int, Value],
        Mapping[str, Value] | None,
        bool,
    ],
    tuple,
]


def dual_add_subtract(
    rn: int,
    rs: int,
    rx: int,
    ry: int,
    left: Operand,
    right: Operand,
    float_form: bool,
) -> tuple:
    """PRM Table 18-10 (p.433) / Table 18-13 (p.434): Dual Add/Subtract is a
    single-function ALU op (mf=0, not multifunction) whose opcode top
    nibble (bits 19:16, i.e. this OPCODE's top nibble) is 0111 (fixed) or
    1111 (float); the low nibble (bits 15:12) is not part of the opcode at
    all -- it is RS, a second 4-bit result register alongside
    RA=RN/FN at bits 11:8 (already read above as RN)."""
    if float_form:
        add_value, add_overflow, add_invalid = _float_binary(
            left, right, "F%d + F%d" % (rx, ry), _f_add
        )
        sub_value, sub_overflow, sub_invalid = _float_binary(
            left, right, "F%d - F%d" % (rx, ry), _f_sub
        )
        updates = _or_updates(
            _float_alu_updates(add_value, av=add_overflow, ai=add_invalid),
            _float_alu_updates(sub_value, av=sub_overflow, ai=sub_invalid),
        )
        operation = "float-dual-add-subtract"
    else:
        add_value = _add(left, right, "R%d + R%d" % (rx, ry))
        sub_value = _subtract(left, right, "R%d - R%d" % (rx, ry), same_source=rx == ry)
        updates = _or_updates(
            _alu_arith_updates(left, right, False),
            _alu_arith_updates(left, right, True, same_source=rx == ry),
        )
        operation = "dual-add-subtract"
    return (
        (rn, rs),
        (add_value, sub_value),
        operation,
        updates,
    )


# PRM Table 17-5: ALUOP 00000001/00000010 are add/subtract.
def alu_add(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    value = _add(left, right, "R%d + R%d" % (rx, ry))
    return rn, value, "add", _astatx_alu_arith(left, right, False)


def alu_subtract(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    same_source = rx == ry
    value = _subtract(left, right, "R%d - R%d" % (rx, ry), same_source=same_source)
    return (
        rn,
        value,
        "subtract",
        _astatx_alu_arith(left, right, True, same_source=same_source),
    )


# PRM p.438-439 / PGR Table 12-3 (p.573): ALUOP 00000101 is
# RN = RX + RY + ci (add with carry) and 00000110 is RN = RX - RY + ci -
# 1 (subtract with borrow), both using ASTATX's AC bit as an explicit
# carry-in read before this instruction's own flags are written. The
# identity RX - RY + ci - 1 = RX + ~RY + ci (see ``_arith_flag_bits_ci``)
# lets both share one value formula: add RY (or its one's complement for
# subtract) plus a constant offset of ci (add) or ci-1 (subtract).
def _alu_carry_impl(rn, rx, ry, left, right, values, subtract: bool) -> tuple:
    astatx = _ureg_raw(values, UREG_CODES["ASTATX"])
    carry_in = _astatx_known_bit(astatx, AC_BIT)
    label = "R%d %s R%d + ci%s" % (
        rx,
        "-" if subtract else "+",
        ry,
        " - 1" if subtract else "",
    )
    value: Operand
    if carry_in is None:
        value = Unknown(label)
    else:
        base = (
            _subtract(left, right, label, same_source=rx == ry)
            if subtract
            else _add(left, right, label)
        )
        offset = (1 if carry_in else 0) - (1 if subtract else 0)
        value = _add(base, Const(offset), label)
    operation = "subtract-with-borrow" if subtract else "add-with-carry"
    return (
        rn,
        value,
        operation,
        _astatx_alu_arith_ci(left, right, subtract, carry_in),
    )


def alu_add_with_carry(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    return _alu_carry_impl(rn, rx, ry, left, right, values, False)


def alu_subtract_with_borrow(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    return _alu_carry_impl(rn, rx, ry, left, right, values, True)


# PGR p.11-9/11-10 (pgr.txt:20570/20606), opcodes 0010 0101/0010 0110:
# RN = RX + ci / RN = RX + ci - 1 -- the single-operand twins of
# 0x05/0x06 above (no RY; PGR's flag table is identical to the RY form),
# so they reuse the same value/flags formulas with RY forced to 0.
def _alu_carry_noy_impl(rn, rx, left, values, subtract: bool) -> tuple:
    astatx = _ureg_raw(values, UREG_CODES["ASTATX"])
    carry_in = _astatx_known_bit(astatx, AC_BIT)
    label = "R%d + ci%s" % (rx, " - 1" if subtract else "")
    value: Operand
    if carry_in is None:
        value = Unknown(label)
    else:
        offset = (1 if carry_in else 0) - (1 if subtract else 0)
        value = _add(left, Const(offset), label)
    operation = "subtract-with-borrow" if subtract else "add-with-carry"
    return (
        rn,
        value,
        operation,
        _astatx_alu_arith_ci(left, Const(0), subtract, carry_in),
    )


def alu_add_with_carry_noy(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    return _alu_carry_noy_impl(rn, rx, left, values, False)


def alu_subtract_with_borrow_noy(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    return _alu_carry_noy_impl(rn, rx, left, values, True)


# PRM Table 18-5: ALUOP 00001010 is signed comp(RX, RY) and 00001011 is
# unsigned compu(RX, RY). Both update status only, so the tracer records
# the comparison without writing RN; the value carries the new flags.
def _alu_compare_impl(rn, rx, ry, left, right, signed: bool) -> tuple:
    label = "%s R%d, R%d" % ("comp" if signed else "compu", rx, ry)
    value = _compare_flags(left, right, signed, label)
    return rn, value, "compare", _astatx_compare(value)


def alu_compare_signed(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    return _alu_compare_impl(rn, rx, ry, left, right, True)


def alu_compare_unsigned(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    return _alu_compare_impl(rn, rx, ry, left, right, False)


def alu_pass(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    return rn, left, "pass", _astatx_alu_logical(left)


# PRM Table 18-5 and p. 19-10: ALUOP 00100010 is RN = -RX, the two's
# complement, with the same flags as 0 - RX.
def alu_negate(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    value = _subtract(Const(0), left, "-R%d" % rx)
    return rn, value, "negate", _astatx_alu_arith(Const(0), left, True)


def alu_increment(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    value = _add(left, Const(1), "R%d + 1" % rx)
    return rn, value, "increment", _astatx_alu_arith(left, Const(1), False)


# PRM Table 18-5 and p. 19-9: ALUOP 00101010 is RN = RX - 1.
def alu_decrement(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    value = _add(left, Const(-1), "R%d - 1" % rx)
    return rn, value, "decrement", _astatx_alu_arith(left, Const(1), True)


# PGR p.11-13/11-14 (pgr.txt:20750), opcode 0011 0000: RN = ABS RX. Value
# and AC/AV/AN/AZ come from the same 0-RX adder as negate (0x22 above,
# "The ABS of the minimum negative number ... causes an overflow" only
# makes sense if ABS always runs the 0-RX path, even for a positive RX);
# unlike negate, AS is data-dependent here (RX's own sign) rather than
# architecturally cleared.
def alu_abs(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    if isinstance(left, Const):
        signed = _signed32(left.value)
        value = left if signed >= 0 else _subtract(Const(0), left, "abs R%d" % rx)
    else:
        value = Unknown("abs R%d" % rx)
    return rn, value, "abs", _astatx_abs(left)


# PRM Table 18-5: ALUOP 01000000..01000010 are the integer logical
# operations AND, OR, and XOR.
_LOGICAL_OPS = {
    0x40: ("and", _op_and),
    0x41: ("or", _op_or),
    0x42: ("xor", _op_xor),
}


def _alu_logical_binary(opcode, rn, rx, ry, left, right) -> tuple:
    name, operation = _LOGICAL_OPS[opcode]
    value = _bitwise(left, right, "R%d %s R%d" % (rx, name, ry), operation)
    return rn, value, name, _astatx_alu_logical(value)


def alu_and(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    return _alu_logical_binary(0x40, rn, rx, ry, left, right)


def alu_or(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    return _alu_logical_binary(0x41, rn, rx, ry, left, right)


def alu_xor(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    return _alu_logical_binary(0x42, rn, rx, ry, left, right)


# PGR p.11-19 (pgr.txt:20918), opcode 0100 0011: RN = NOT RX. Same
# AZ/AN-from-result, AC/AV/AS/AI-cleared rule as pass/and/or/xor.
def alu_not(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    value = _not(left, "not R%d" % rx)
    return rn, value, "not", _astatx_alu_logical(value)


# PRM Table 18-5 (p.425-427), float rows; per-op flags cited at each
# branch (PRM Table 3-3, pp.3-8/3-9, cross-checked against the classic
# PGR's per-instruction pages, which spell out AZ/AN/AV/AI exactly where
# the SHARC+ PRM only marks a column "*"/data-dependent).
def alu_float_add(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    value, overflow, invalid = _float_binary(
        left, right, "F%d + F%d" % (rx, ry), _f_add
    )
    return (
        rn,
        value,
        "float-add",
        _float_alu_updates(value, av=overflow, ai=invalid),
    )


def alu_float_subtract(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    value, overflow, invalid = _float_binary(
        left, right, "F%d - F%d" % (rx, ry), _f_sub
    )
    return (
        rn,
        value,
        "float-subtract",
        _float_alu_updates(value, av=overflow, ai=invalid),
    )


# PRM p.19-4 ("FN = (FX + FY) / 2;", opcode 1000 1001): "Adds the
# floating-point operands in registers Fx and Fy and divides the result by
# 2, by decrementing the exponent of the sum before rounding." Its own
# ASTATx/y Flags table (p.19-4/19-5) differs from plain float-add's: AI is
# the same invalid condition (NAN input, or opposite-signed infinities --
# _float_binary already computes this for the identical add-shaped
# operation), AN and AZ follow the result the same way every other float
# ALU op's do, but AV is documented "Cleared" unconditionally rather than
# tracking overflow the way float-add's AV does.
def alu_float_average(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    value, _overflow, invalid = _float_binary(
        left, right, "(F%d + F%d) / 2" % (rx, ry), _f_avg
    )
    return (
        rn,
        value,
        "float-average",
        _float_alu_updates(value, av=False, ai=invalid),
    )


# PGR p.11-27 (pgr.txt:21185), opcode 1001 0010: Fn = abs(Fx - Fy).
# Magnitude (and so AV/AZ) is identical to plain float-subtract above;
# only the sign bit changes (cleared) and AN is architecturally fixed 0
# rather than following the result's sign.
def alu_float_abs_subtract(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    diff, overflow, invalid = _float_binary(left, right, "F%d - F%d" % (rx, ry), _f_sub)
    value = Const(diff.value & 0x7FFFFFFF) if isinstance(diff, Const) else diff
    return (
        rn,
        value,
        "float-abs-subtract",
        _float_alu_updates(value, av=overflow, an_zero=True, ai=invalid),
    )


# PGR p.11-29: comp(Fx, Fy).
def alu_float_compare(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    label = "comp F%d, F%d" % (rx, ry)
    value, invalid = _compare_flags_float(left, right, label)
    return rn, value, "float-compare", _astatx_compare_float(value, invalid)


# PGR p.11-32: Fn = pass Fx.
def alu_float_pass(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    value, overflow, invalid = _float_unary(left, "pass F%d" % rx, _f_pass)
    return (
        rn,
        value,
        "float-pass",
        _float_alu_updates(value, av=False, ai=invalid),
    )


# PGR p.11-30: Fn = -Fx.
def alu_float_negate(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    value, overflow, invalid = _float_unary(left, "-F%d" % rx, _f_neg)
    return (
        rn,
        value,
        "float-negate",
        _float_alu_updates(value, av=False, ai=invalid),
    )


# PRM Table 18-5 opcode 1010 0101 (p.20-8) / PGR Table 12-4 opcode
# 1010 0101, p.11-33: Fn = rnd Fx.
def alu_float_round32(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    value, invalid = _float_round32(left, "rnd F%d" % rx)
    return (
        rn,
        value,
        "float-round32",
        _float_alu_updates(value, av=False, ai=invalid),
    )


# PGR p.11-34/11-35: Rn = mant Fx. Bespoke flag dict, not
# ``_float_alu_updates``: the result is an unsigned-magnitude fixed
# word (no sign bit of its own to derive AZ/AN from), AN is
# architecturally fixed 0 (PRM Table 3-3), and AS/AV/AI come from the
# *input*'s sign/infinity/NAN rather than the result.
def alu_float_mant(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    value, overflow, negative, invalid = _float_mantissa(left, "mant F%d" % rx)
    updates = _flags_from_pairs(
        (
            (AC_BIT, False),
            (AF_BIT, True),
            (AN_BIT, False),
            (AV_BIT, overflow),
            (AS_BIT, negative),
            (AI_BIT, invalid),
            (AZ_BIT, (value.value == 0) if isinstance(value, Const) else None),
        )
    )
    return rn, value, "mant", updates


# PGR p.11-36 (pgr.txt:21522), opcode 1100 0001: RN = LOGB FX. Bespoke
# flag dict like ``mant`` above: RESULT is a plain fixed-point integer
# (not float), so AN/AZ come from its own bits rather than through
# ``_float_alu_updates``, and AF is set (float-unit op with a
# fixed-point result, same convention as ``mant``/``fix``/``trunc``).
def alu_float_logb(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    mode1 = _ureg_raw(values, UREG_CODES["MODE1"])
    value, overflow, invalid = _float_logb(left, mode1, "logb F%d" % rx)
    updates = _flags_from_pairs(
        (
            (AC_BIT, False),
            (AF_BIT, True),
            (AS_BIT, False),
            (AV_BIT, overflow),
            (AI_BIT, invalid),
            (AZ_BIT, (value.value == 0) if isinstance(value, Const) else None),
            (
                AN_BIT,
                bool(value.value & 0x80000000) if isinstance(value, Const) else None,
            ),
        )
    )
    return rn, value, "logb", updates


# PGR p.11-31: Fn = abs Fx. AN fixed 0; AS carries the *input*'s sign.
def alu_float_abs(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    value, overflow, invalid = _float_unary(left, "abs F%d" % rx, abs)
    return (
        rn,
        value,
        "float-abs",
        _float_alu_updates(value, av=False, an_zero=True, as_source=left, ai=invalid),
    )


# PGR p.11-33: Fn = scalb Fx by Ry. Unlike abs/pass/etc., AN here
# follows the *result*'s sign (PRM Table 3-3 marks AN '*', not 0), so
# this reuses ``_float_alu_updates``'s default (as_source=None,
# an_zero=False) rather than the abs-style override.
def alu_float_scalb(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    value, overflow, invalid = _float_scalb(left, right, "scalb F%d by R%d" % (rx, ry))
    return (
        rn,
        value,
        "float-scalb",
        _float_alu_updates(value, av=overflow, ai=invalid),
    )


# PGR p.11-20/11-21: Rn = min/max(Rx, Ry) -- fixed-point, not the
# float min/max at 0xE1/0xE2 below. AV/AC/AS/AI/AF are all fixed 0
# (PRM Table 3-2, "AF Flag = 0"); only AZ/AN follow the chosen
# operand, the same rule ``_astatx_alu_logical`` already implements
# for pass/not/and/or/xor.
def _alu_minmax_fixed_impl(rn, rx, ry, left, right, minimum: bool) -> tuple:
    name = "min" if minimum else "max"
    value: Operand
    if isinstance(left, Const) and isinstance(right, Const):
        a, b = _signed32(left.value), _signed32(right.value)
        pick_left = (a <= b) if minimum else (a >= b)
        value = left if pick_left else right
    else:
        value = Unknown("%s(R%d, R%d)" % (name, rx, ry))
    return rn, value, name, _astatx_alu_logical(value)


def alu_min_fixed(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    return _alu_minmax_fixed_impl(rn, rx, ry, left, right, True)


def alu_max_fixed(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    return _alu_minmax_fixed_impl(rn, rx, ry, left, right, False)


# PRM Table 18-5 (opcode 0110 0011) and "RN = clip RX by RY" p.19-16: Rx
# if |Rx| < |Ry|, otherwise |Ry| with Rx's sign (the PRM's text gives
# "|Ry| if Rx is negative", which its float twin at 0xE3 and the classic
# manuals give as -|Ry|). Flags as min/max: AZ/AN from the result, the
# rest cleared.
def alu_clip_fixed(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    value: Operand
    if isinstance(left, Const) and isinstance(right, Const):
        a, b = _signed32(left.value), abs(_signed32(right.value))
        value = Const((a if abs(a) < b else (b if a >= 0 else -b)) & 0xFFFFFFFF)
    else:
        value = Unknown("clip(R%d, R%d)" % (rx, ry))
    return rn, value, "clip", _astatx_alu_logical(value)


# PRM p.19-19, opcode 1110 0000 / PGR Table 12-4 p.574, same opcode: Fn =
# Fx copysign Fy.
def alu_float_copysign(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    value, invalid = _float_copysign(left, right, "F%d copysign F%d" % (rx, ry))
    return (
        rn,
        value,
        "float-copysign",
        _float_alu_updates(value, av=False, ai=invalid),
    )


# PGR p.11-46/11-47: Fn = min/max(Fx, Fy).
def _alu_minmax_float_impl(rn, rx, ry, left, right, minimum: bool) -> tuple:
    name = "min" if minimum else "max"
    combine = _float_min if minimum else _float_max
    value, overflow, invalid = _float_binary(
        left, right, "%s(F%d, F%d)" % (name, rx, ry), combine
    )
    return (
        rn,
        value,
        "float-" + name,
        _float_alu_updates(value, av=False, ai=invalid),
    )


def alu_min_float(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    return _alu_minmax_float_impl(rn, rx, ry, left, right, True)


def alu_max_float(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    return _alu_minmax_float_impl(rn, rx, ry, left, right, False)


# PGR p.11-48 / PRM p.3-6: Fn = clip Fx by Fy.
def alu_float_clip(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    value, overflow, invalid = _float_binary(
        left, right, "clip F%d by F%d" % (rx, ry), _float_clip
    )
    return (
        rn,
        value,
        "float-clip",
        _float_alu_updates(value, av=False, ai=invalid),
    )


# PRM p.427/PGR p.11-39 "without scaling factor": Fn = float Rx.
def alu_float_convert(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    value, invalid = _fixed_to_float(left, "float R%d" % rx)
    return (
        rn,
        value,
        "float-convert",
        _float_alu_updates(value, av=False, ai=invalid),
    )


# PGR p.11-39 "with scaling factor" / PRM p.19-.. : Fn = float Rx by Ry.
# AV is data-dependent here (unlike the unscaled form above, where an
# int32 input can never overflow float32 range), per PGR Table 3-3.
def alu_float_convert_scaled(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    value, overflow = _fixed_to_float_scaled(left, right, "float R%d by R%d" % (rx, ry))
    return (
        rn,
        value,
        "float-convert-scaled",
        _float_alu_updates(value, av=overflow, ai=False),
    )


# PRM p.24-.. / PGR p.11-36..11-38, opcode 1100 1001: Rn = fix Fx
# (rounds to nearest or truncates per MODE1.TRUNCATE; see
# ``_float_to_fixed``).
def alu_fix(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    mode1 = _ureg_raw(values, UREG_CODES["MODE1"])
    value, overflow, invalid = _float_to_fixed(left, mode1, False, "fix F%d" % rx)
    return (
        rn,
        value,
        "fix",
        _float_alu_updates(value, av=overflow, ai=invalid),
    )


# PRM p.427/PGR p.11-37: Rn = trunc Fx.
def alu_trunc(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    mode1 = _ureg_raw(values, UREG_CODES["MODE1"])
    value, overflow, invalid = _float_to_fixed_trunc(left, mode1, "trunc F%d" % rx)
    return (
        rn,
        value,
        "trunc",
        _float_alu_updates(value, av=overflow, ai=invalid),
    )


# PGR p.11-36..11-38, opcode 1101 1001: Rn = fix Fx by Ry -- Ry's
# exponent-add (``_scale_fixed_input``) applied before the same fix
# conversion as 0xC9; PGR Table 3-3 gives it the identical flag columns
# as the unscaled form.
def alu_fix_scaled(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    mode1 = _ureg_raw(values, UREG_CODES["MODE1"])
    scaled = _scale_fixed_input(left, right, "F%d * 2**R%d" % (rx, ry))
    value, overflow, invalid = _float_to_fixed(
        scaled, mode1, False, "fix F%d by R%d" % (rx, ry)
    )
    return (
        rn,
        value,
        "fix-scaled",
        _float_alu_updates(value, av=overflow, ai=invalid),
    )


# PGR p.11-36..11-38, opcode 1101 1101: Rn = trunc Fx by Ry -- Ry's
# exponent-add applied before the same truncation as 0xCD; PGR Table 3-3
# again gives it the unscaled form's flag columns.
def alu_trunc_scaled(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    mode1 = _ureg_raw(values, UREG_CODES["MODE1"])
    scaled = _scale_fixed_input(left, right, "F%d * 2**R%d" % (rx, ry))
    value, overflow, invalid = _float_to_fixed(
        scaled, mode1, True, "trunc F%d by R%d" % (rx, ry)
    )
    return (
        rn,
        value,
        "trunc-scaled",
        _float_alu_updates(value, av=overflow, ai=invalid),
    )


# PRM p.427 / PGR p.11-44/11-45: Fn = recips/rsqrts Fx -- iterative
# reciprocal/reciprocal-sqrt seed instructions. The seed *mantissa* comes
# from an unpublished ROM lookup table, so no numeric value can be claimed
# for the ordinary (non-special-case) input. But recips's and rsqrts's own
# ASTATx/y Flags tables (SHARC+ PRM pp.19-16/19-17 for recips,
# pp.19-17/19-18 for rsqrts -- both quoted in full below) define every flag
# -- and, for three documented special-case inputs, the exact *result* too
# -- purely from classifying FX's IEEE-754 bit pattern (sign/exponent/
# mantissa: NAN, +-zero, +infinity, "negative and nonzero", or "unbiased
# exponent > +125"). None of that needs the ROM table, so it is claimed
# unconditionally (not gated behind --approx-recips, which only covers
# recips's *ordinary-case* seed value -- see floats._approx_recips).
def _alu_recip_seed_impl(rn, rx, left, name: str, approx_recips: bool) -> tuple:
    label = "%s F%d (ROM seed mantissa not numerically modeled)" % (name, rx)
    op = "float-" + name + "-seed"
    if not isinstance(left, Const):
        if name == "recips" and approx_recips:
            # --approx-recips is usage-gated, not input-gated (see the
            # docstring above _alu_recip_seed_impl's caller wiring, and
            # compute.py's approx_recips_used calibration flag): a
            # symbolic input still takes the approximated path, it just
            # cannot resolve to a concrete value.
            value, updates = _approx_recips(left)
            return rn, value, "float-recips-seed-approx", updates
        updates = _float_alu_updates(Unknown(label), av=None, ai=None)
        return rn, Unknown(label), op, updates
    bits = left.value & 0xFFFFFFFF
    sign = (bits >> 31) & 1
    biased_exp = (bits >> 23) & 0xFF
    mantissa = bits & 0x7FFFFF
    is_nan = biased_exp == 0xFF and mantissa != 0
    # PRM p.417-418 (IEEE-754-compatibility bullet, general to every
    # computational unit; also cited in floats._approx_recips's docstring):
    # "Denormal operands ... flush to zero when input to a computational
    # unit." A denormal (biased_exp == 0, mantissa != 0) is therefore
    # classified with an exact +-zero input, not with "negative nonzero".
    is_zero = biased_exp == 0
    is_pos_inf = biased_exp == 0xFF and mantissa == 0 and sign == 0
    is_neg_nonzero = sign == 1 and not is_zero and not is_nan
    updates = _flags_from_pairs(
        (
            (AC_BIT, False),
            (AS_BIT, False),
        )
    )

    if name == "rsqrts":
        # PRM p.19-18 "FN = rsqrts FX" ASTATx/y Flags:
        #   AI  Set if the input operand is negative and nonzero, or a
        #       NAN, otherwise cleared
        #   AN  Set if the input operand is -zero, otherwise cleared
        #   AV  Set if the input operand is +-zero, otherwise cleared
        #   AZ  Set if the floating-point result is +zero (Fx = +infinity),
        #       otherwise cleared
        # Same page, Function: "The input +-zero returns +-infinity and
        # sets the overflow flag. The input +infinity returns +zero. A NAN
        # input or a negative nonzero input returns a result of all 1s."
        updates = _flags_put(updates, AI_BIT, is_nan or is_neg_nonzero)
        updates = _flags_put(updates, AN_BIT, bool(is_zero and sign))
        updates = _flags_put(updates, AV_BIT, is_zero)
        if is_nan or is_neg_nonzero:
            updates = _flags_put(updates, AZ_BIT, False)
            return rn, Const(0xFFFFFFFF), op, updates
        if is_zero:
            updates = _flags_put(updates, AZ_BIT, False)
            result = Const((sign << 31) | (0xFF << 23))
            return rn, result, op, updates
        if is_pos_inf:
            updates = _flags_put(updates, AZ_BIT, True)
            return rn, Const(0), op, updates
        # Ordinary positive finite input: the seed mantissa comes from an
        # unpublished ROM table, and the manual's own exponent formula
        # ("the unbiased exponent of Fn = INT[e/2] <?> 1", p.19-18) has a
        # missing operator glyph between "INT[e/2]" and "1" in the PDF
        # itself (confirmed against the rendered page image, not just
        # extracted text -- see out/refs/sharc-plus-prm/png/p0470.png),
        # so no bit-for-bit reproduction of the documented rule is
        # possible.
        # --approx-recips instead approximates the seed the same way
        # floats._approx_recips does for recips: the true mathematical
        # reciprocal-square-root's IEEE-754 bit pattern, keeping only the
        # documented number of accurate mantissa bits (rsqrts is "a 4-bit
        # accurate seed", half of recips's 8, so the top 4 mantissa bits
        # are kept and the low 19 zeroed, vs recips's top-8/low-15 split).
        updates = _flags_put(updates, AZ_BIT, False)
        if approx_recips:
            x = _f32_from_bits(bits)
            seed_bits, _overflowed = _float32_bits(1.0 / _sqrt(x))
            seed_bits &= 0xFFF80000
            return (
                rn,
                Const(seed_bits),
                "float-rsqrts-seed-approx",
                updates,
            )
        return rn, Unknown(label), op, updates

    # name == "recips": PRM pp.19-16/19-17 "FN = recips FX" ASTATx/y Flags
    # (also quoted in full in floats._approx_recips's docstring):
    #   AI  Set if the input operand is a NAN, otherwise cleared
    #   AN  Set if the input operand is negative, otherwise cleared
    #   AV  Set if the input operand is +-zero, otherwise cleared
    #   AZ  Set if the floating-point result is +-zero (unbiased exponent
    #       of Fx is greater than +125), otherwise cleared
    updates = _flags_put(updates, AI_BIT, is_nan)
    updates = _flags_put(updates, AN_BIT, bool(sign))
    if is_nan:
        updates = _flags_put(updates, AV_BIT, False)
        updates = _flags_put(updates, AZ_BIT, False)
        return rn, Const(0xFFFFFFFF), op, updates
    if is_zero:
        updates = _flags_put(updates, AV_BIT, True)
        updates = _flags_put(updates, AZ_BIT, False)
        result = Const((sign << 31) | (0xFF << 23))
        return rn, result, op, updates
    updates = _flags_put(updates, AV_BIT, False)
    unbiased_exp = biased_exp - 127
    if unbiased_exp > 125:
        updates = _flags_put(updates, AZ_BIT, True)
        return rn, Const(sign << 31), op, updates
    updates = _flags_put(updates, AZ_BIT, False)
    if approx_recips:
        # --approx-recips's own classification duplicates the above (kept
        # separate in floats.py, verified independently); only its
        # ordinary-case numeric mantissa approximation is used here.
        value, _ = _approx_recips(left)
        return rn, value, "float-recips-seed-approx", updates
    return rn, Unknown(label), op, updates


def alu_recips_seed(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    return _alu_recip_seed_impl(rn, rx, left, "recips", approx_recips)


def alu_rsqrts_seed(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    return _alu_recip_seed_impl(rn, rx, left, "rsqrts", approx_recips)


# ---------------------------------------------------------------------------
# 64-bit (IEEE double) ALU ops (ADSP-SC58x/2158x PRM p.20-6/Table 18-6,
# opcodes 0x11-0x1F -- cu=0, the same ALU unit as every op above, just a
# gap in the classic PRM/PGR's opcode table that SHARC+ fills). RN/RX/RY
# are read from the *same* field positions as a 32-bit op (Table 18-23);
# only the register-pair operands (Fm:n/Fx:y/Fz:w) additionally read their
# neighbour register via ``_ureg`` -- LEFT/RIGHT (already resolved by
# ``_compute`` against the plain rx/ry codes) are unused here except where
# an operand really is a single 32-bit register (fix/trunc/float's Rn or
# Ry scale factor), named explicitly at each call site instead of reusing
# LEFT/RIGHT's generic names, since which operand is a pair and which is
# plain differs op to op (Table 18-28's "Rn/Rx/Ry/Rz" vs "Fm:n/Fx:y/Fz:w"
# columns). None of these opcodes appears in dt2-1.16, dt2-1.15C, dn2-1.11
# or dn2-1.10E's aligned, in-function code (tools/sharc.py census, lane F1)
# -- implemented from the manual ahead of any observed use, like the
# multiplier/shifter's own already-decoded-but-unobserved corners.
def alu_double_add(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    hi, lo, overflow, invalid = _double_binary(
        _ureg(values, rx + 1),
        _ureg(values, rx),
        _ureg(values, ry + 1),
        _ureg(values, ry),
        "F%d:%d + F%d:%d" % (rx + 1, rx, ry + 1, ry),
        _f_add,
    )
    return (
        (rn + 1, rn),
        (hi, lo),
        "double-add",
        _double_alu_updates(hi, lo, av=overflow, ai=invalid),
    )


def alu_double_subtract(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    hi, lo, overflow, invalid = _double_binary(
        _ureg(values, rx + 1),
        _ureg(values, rx),
        _ureg(values, ry + 1),
        _ureg(values, ry),
        "F%d:%d - F%d:%d" % (rx + 1, rx, ry + 1, ry),
        _f_sub,
    )
    return (
        (rn + 1, rn),
        (hi, lo),
        "double-subtract",
        _double_alu_updates(hi, lo, av=overflow, ai=invalid),
    )


def alu_double_compare(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    label = "comp F%d:%d, F%d:%d" % (rx + 1, rx, ry + 1, ry)
    value, invalid = _double_compare(
        _ureg(values, rx + 1),
        _ureg(values, rx),
        _ureg(values, ry + 1),
        _ureg(values, ry),
        label,
    )
    return rn, value, "double-compare", _astatx_compare_float(value, invalid)


def alu_double_negate(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    hi, lo, _overflow, invalid = _double_unary(
        _ureg(values, rx + 1),
        _ureg(values, rx),
        "-F%d:%d" % (rx + 1, rx),
        _f_neg,
    )
    return (
        (rn + 1, rn),
        (hi, lo),
        "double-negate",
        _double_alu_updates(hi, lo, av=False, ai=invalid),
    )


def alu_double_abs(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    hi, lo, _overflow, invalid = _double_unary(
        _ureg(values, rx + 1),
        _ureg(values, rx),
        "abs F%d:%d" % (rx + 1, rx),
        abs,
    )
    return (
        (rn + 1, rn),
        (hi, lo),
        "double-abs",
        _double_alu_updates(hi, lo, av=False, an_zero=True, ai=invalid),
    )


def alu_double_pass(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    hi, lo, _overflow, invalid = _double_unary(
        _ureg(values, rx + 1),
        _ureg(values, rx),
        "pass F%d:%d" % (rx + 1, rx),
        _f_pass,
    )
    return (
        (rn + 1, rn),
        (hi, lo),
        "double-pass",
        _double_alu_updates(hi, lo, av=False, ai=invalid),
    )


def alu_double_fix(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    mode1 = _ureg_raw(values, UREG_CODES["MODE1"])
    value, overflow, invalid = _double_to_fixed(
        _ureg(values, rx + 1),
        _ureg(values, rx),
        mode1,
        False,
        "fix F%d:%d" % (rx + 1, rx),
    )
    return (
        rn,
        value,
        "double-fix",
        _float_alu_updates(value, av=overflow, ai=invalid),
    )


def alu_double_fix_scaled(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    mode1 = _ureg_raw(values, UREG_CODES["MODE1"])
    hi, lo = _scale_double_input(
        _ureg(values, rx + 1),
        _ureg(values, rx),
        right,
        "F%d:%d * 2**R%d" % (rx + 1, rx, ry),
    )
    value, overflow, invalid = _double_to_fixed(
        hi, lo, mode1, False, "fix F%d:%d by R%d" % (rx + 1, rx, ry)
    )
    return (
        rn,
        value,
        "double-fix-scaled",
        _float_alu_updates(value, av=overflow, ai=invalid),
    )


def alu_double_trunc(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    mode1 = _ureg_raw(values, UREG_CODES["MODE1"])
    value, overflow, invalid = _double_to_fixed(
        _ureg(values, rx + 1),
        _ureg(values, rx),
        mode1,
        True,
        "trunc F%d:%d" % (rx + 1, rx),
    )
    return (
        rn,
        value,
        "double-trunc",
        _float_alu_updates(value, av=overflow, ai=invalid),
    )


def alu_double_trunc_scaled(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    mode1 = _ureg_raw(values, UREG_CODES["MODE1"])
    hi, lo = _scale_double_input(
        _ureg(values, rx + 1),
        _ureg(values, rx),
        right,
        "F%d:%d * 2**R%d" % (rx + 1, rx, ry),
    )
    value, overflow, invalid = _double_to_fixed(
        hi, lo, mode1, True, "trunc F%d:%d by R%d" % (rx + 1, rx, ry)
    )
    return (
        rn,
        value,
        "double-trunc-scaled",
        _float_alu_updates(value, av=overflow, ai=invalid),
    )


def alu_double_float(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    hi, lo, invalid = _fixed_to_double(left, "float R%d" % rx)
    return (
        (rn + 1, rn),
        (hi, lo),
        "double-float",
        _double_alu_updates(hi, lo, av=False, ai=invalid),
    )


def alu_double_float_scaled(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    hi, lo, overflow = _fixed_to_double_scaled(
        left, right, "float R%d by R%d" % (rx, ry)
    )
    return (
        (rn + 1, rn),
        (hi, lo),
        "double-float-scaled",
        _double_alu_updates(hi, lo, av=overflow, ai=False),
    )


def alu_double_scalb(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    hi, lo, overflow, invalid = _double_scalb(
        _ureg(values, rx + 1),
        _ureg(values, rx),
        right,
        "scalb F%d:%d by R%d" % (rx + 1, rx, ry),
    )
    return (
        (rn + 1, rn),
        (hi, lo),
        "double-scalb",
        _double_alu_updates(hi, lo, av=overflow, ai=invalid),
    )


def alu_double_to_float32(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    value, overflow, invalid = _double_to_float32(
        _ureg(values, rx + 1), _ureg(values, rx), "cvt F%d:%d" % (rx + 1, rx)
    )
    return (
        rn,
        value,
        "double-to-float32",
        _float_alu_updates(value, av=overflow, ai=invalid),
    )


def alu_float32_to_double(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    hi, lo, invalid = _float32_to_double(left, "cvt F%d" % rx)
    return (
        (rn + 1, rn),
        (hi, lo),
        "float32-to-double",
        _double_alu_updates(hi, lo, av=False, ai=invalid),
    )


ALU_OPS: dict[int, Handler] = {
    0x01: alu_add,
    0x02: alu_subtract,
    0x05: alu_add_with_carry,
    0x06: alu_subtract_with_borrow,
    0x25: alu_add_with_carry_noy,
    0x26: alu_subtract_with_borrow_noy,
    0x0A: alu_compare_signed,
    0x0B: alu_compare_unsigned,
    0x21: alu_pass,
    0x22: alu_negate,
    0x29: alu_increment,
    0x2A: alu_decrement,
    0x30: alu_abs,
    0x40: alu_and,
    0x41: alu_or,
    0x42: alu_xor,
    0x43: alu_not,
    0x81: alu_float_add,
    0x82: alu_float_subtract,
    0x89: alu_float_average,
    0x92: alu_float_abs_subtract,
    0x8A: alu_float_compare,
    0xA1: alu_float_pass,
    0xA2: alu_float_negate,
    0xA5: alu_float_round32,
    0xAD: alu_float_mant,
    0xC1: alu_float_logb,
    0xB0: alu_float_abs,
    0xBD: alu_float_scalb,
    0x61: alu_min_fixed,
    0x62: alu_max_fixed,
    0x63: alu_clip_fixed,
    0xE0: alu_float_copysign,
    0xE1: alu_min_float,
    0xE2: alu_max_float,
    0xE3: alu_float_clip,
    0xCA: alu_float_convert,
    0xDA: alu_float_convert_scaled,
    0xC9: alu_fix,
    0xCD: alu_trunc,
    0xD9: alu_fix_scaled,
    0xDD: alu_trunc_scaled,
    0xC4: alu_recips_seed,
    0xC5: alu_rsqrts_seed,
    0x11: alu_double_add,
    0x12: alu_double_subtract,
    0x13: alu_double_compare,
    0x14: alu_double_negate,
    0x15: alu_double_abs,
    0x16: alu_double_pass,
    0x17: alu_double_fix,
    0x18: alu_double_fix_scaled,
    0x19: alu_double_trunc,
    0x1A: alu_double_trunc_scaled,
    0x1B: alu_double_float,
    0x1C: alu_double_float_scaled,
    0x1D: alu_float32_to_double,
    0x1E: alu_double_to_float32,
    0x1F: alu_double_scalb,
}
