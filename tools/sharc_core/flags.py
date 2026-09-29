"""ASTATX flag updates for compute results.

Every compute handler describes its effect on ASTATX/ASTATY as a
``FlagUpdate`` (values.py): masks of bits it defines and forgets, computed
here from the operands, and applied later by ``_apply_flag_update``. The
updates are data, not closures, so a translator can carry them as three
integers.

Moved verbatim from tools/sharc_trace.py, then converted from updater
closures to FlagUpdate records.
"""

from __future__ import annotations

from .encoding import (
    AC_BIT,
    AF_BIT,
    AI_BIT,
    ALU_FLAGS_MASK,
    AN_BIT,
    AS_BIT,
    AV_BIT,
    AZ_BIT,
    MI_BIT,
    MN_BIT,
    MU_BIT,
    MULT_FLAGS_MASK,
    MV_BIT,
    SS_BIT,
    SV_BIT,
    SZ_BIT,
)
from .floats import (
    _float32,
    _isnan,
)
from .values import (
    CACC_MASK,
    FLAGS_NONE,
    Const,
    FlagUpdate,
    Unknown,
    Value,
    _apply_flag_update,
    _astatx_define,
    _astatx_forget,
    _astatx_known_bit,
    _flags_define,
    _flags_forget,
    _flags_or,
    _flags_put,
    _flags_then,
    _signed32,
)

__all__ = [
    "FLAGS_NONE",
    "MULT_FLAGS_CLEAR",
    "MULT_FLAGS_FIXED",
    "MULT_FLAGS_FORGET",
    "MULT_FLAGS_SAT",
    "FlagUpdate",
    "_alu_arith_updates",
    "_alu_result_bits",
    "_apply_flag_update",
    "_arith_flag_bits",
    "_arith_flag_bits_ci",
    "_astatx_abs",
    "_astatx_alu_arith",
    "_astatx_alu_arith_ci",
    "_astatx_alu_logical",
    "_astatx_bit_field",
    "_astatx_bit_test",
    "_astatx_btst",
    "_astatx_compare",
    "_astatx_compare_float",
    "_astatx_define",
    "_astatx_fext",
    "_astatx_forget",
    "_astatx_lefto",
    "_astatx_leftz",
    "_astatx_shift",
    "_bits_to_updates",
    "_compare_flags",
    "_compare_flags_float",
    "_double_alu_updates",
    "_flags_define",
    "_flags_forget",
    "_flags_or",
    "_flags_put",
    "_flags_then",
    "_float_alu_updates",
    "_mult_flags",
    "_or_updates",
]


def _compare_flags(left: Value, right: Value, signed: bool, label: str) -> Value:
    """Return AZ (bit 0), AN (bit 2) and the new CACC MSB (bit 31) of a compare.

    PRM comp/compu (pp. 18-5, 18-6): AZ when RX equals RY, AN when RX is
    smaller, and the CACC MSB when RX is greater.
    """
    if not isinstance(left, Const) or not isinstance(right, Const):
        return Unknown(label)
    x, y = left.value & 0xFFFFFFFF, right.value & 0xFFFFFFFF
    if signed:
        x, y = _signed32(x), _signed32(y)
    return Const(
        (0x1 if x == y else 0) | (0x4 if x < y else 0) | (0x80000000 if x > y else 0)
    )


def _compare_flags_float(
    left: Value, right: Value, label: str
) -> tuple[Value, bool | None]:
    """comp(FX, FY) (PRM Table 18-5 opcode 0x8A, p.426; PGR p.11-29).

    Same bit-0 (AZ)/bit-2 (AN)/bit-31 (new CACC MSB) value encoding
    ``_compare_flags`` uses for the fixed-point comp/compu, consumed by
    ``_astatx_compare``'s CACC shift-register logic. An unordered (NAN)
    compare sets none of those bits (PGR doesn't document AZ/AN/CACC firing
    on an unordered compare) and instead reports the invalid flag, which
    the caller applies on top via ``_astatx_compare``'s AI override.
    """
    a, b = _float32(left), _float32(right)
    if a is None or b is None:
        return Unknown(label), None
    if _isnan(a) or _isnan(b):
        return Const(0), True
    return (
        Const(
            (0x1 if a == b else 0)
            | (0x4 if a < b else 0)
            | (0x80000000 if a > b else 0)
        ),
        False,
    )


def _bits_to_updates(mask: int, bits: int | None) -> FlagUpdate:
    """Define MASK's bits as the matching bits of BITS; BITS=None forgets
    every bit in MASK."""
    if bits is None:
        return _flags_forget(mask)
    return _flags_define(mask, bits)


def _or_updates(a: FlagUpdate, b: FlagUpdate) -> FlagUpdate:
    """Kleene-OR two ASTATX updates bit by bit (PRM p.3-21/3-22:
    "Multifunction Computations ... in the dual add/subtract computation,
    the ALU flags from the two operations are ORed together"). True beats
    anything; a bit present in only one update keeps that update's own
    value."""
    return _flags_or(a, b)


def _alu_arith_updates(
    a: Value, b: Value, subtract: bool, *, same_source: bool = False
) -> FlagUpdate:
    """``_astatx_alu_arith`` under the name the fixed-point dual
    add/subtract uses before it ORs two such results together (PRM
    pp.439-440, 446-447).

    SAME_SOURCE mirrors ``_subtract``'s: with SUBTRACT=True it means A and B
    are the same operand read twice, so the flags are those of 0-0 (AZ/AC
    set, AN/AV clear) regardless of what value that operand held."""
    return _astatx_alu_arith(a, b, subtract, same_source=same_source)


def _float_alu_updates(
    result: Value,
    *,
    av: bool | None = False,
    an_zero: bool = False,
    as_source: Value | None = None,
    ai: bool | None = None,
) -> FlagUpdate:
    """ASTATX update shared by the float ALU ops (PRM Table 3-3,
    pp.3-8/3-9; per-op PGR pages cited at each call site).

    AC is always 0 and AF is always 1 for a float ALU result. AZ/AN come
    from RESULT's bit pattern (both +0.0 and -0.0 count as AZ) unless the
    op's AN column is architecturally fixed to 0 (the abs-family:
    AN_ZERO=True). AS is 0 unless AS_SOURCE is given (FN=abs FX carries the
    *input*'s sign, PGR p.11-31). AV/AI are per-op data: pass the
    (overflowed, invalid) pair ``_float_binary``/``_float_unary`` computed,
    or an explicit fixed value for an op the table/PGR documents as always
    0 (e.g. FN=float RX's AV and AI).
    """
    define = (1 << AC_BIT) | (1 << AF_BIT)
    bits = 1 << AF_BIT
    forget = 0
    if av is None:
        forget |= 1 << AV_BIT
    else:
        define |= 1 << AV_BIT
        if av:
            bits |= 1 << AV_BIT
    if ai is None:
        forget |= 1 << AI_BIT
    else:
        define |= 1 << AI_BIT
        if ai:
            bits |= 1 << AI_BIT
    sign = False if as_source is None else _astatx_known_bit(as_source, 31)
    if sign is None:
        forget |= 1 << AS_BIT
    else:
        define |= 1 << AS_BIT
        if sign:
            bits |= 1 << AS_BIT
    if isinstance(result, Const):
        define |= (1 << AZ_BIT) | (1 << AN_BIT)
        if (result.value & 0x7FFFFFFF) == 0:
            bits |= 1 << AZ_BIT
        if not an_zero and result.value & 0x80000000:
            bits |= 1 << AN_BIT
    else:
        forget |= 1 << AZ_BIT
        if an_zero:
            define |= 1 << AN_BIT
        else:
            forget |= 1 << AN_BIT
    return FlagUpdate(define, bits, forget)


def _double_alu_updates(
    hi_value: Value,
    lo_value: Value,
    *,
    av: bool | None = False,
    an_zero: bool = False,
    ai: bool | None = None,
) -> FlagUpdate:
    """ASTATX update for the 64-bit float ALU ops (SC58x/2158x PRM
    ch.20 "64-bit Floating-Point Computations"; per-op flags cited at each
    call site). Same shape as ``_float_alu_updates``: AN is HI_VALUE's own
    sign bit; AZ needs *both* halves zero (unlike AN, a zero HI half alone
    does not imply LO is also zero -- a subnormal double can have a
    zero-looking HI word with a nonzero LO mantissa tail). AS (sign of a
    separate operand, e.g. abs's input sign) never appears in any 64-bit
    ALU flags table this project has read, unlike the 32-bit abs op, so it
    is not modeled here -- every 64-bit op's AS column is "Cleared" per the
    manual.
    """
    update = _float_alu_updates(Const(0), av=av, an_zero=an_zero, ai=ai)
    if isinstance(hi_value, Const) and isinstance(lo_value, Const):
        bits = hi_value.value
        update = _flags_put(
            update, AZ_BIT, (bits & 0x7FFFFFFF) == 0 and lo_value.value == 0
        )
        return _flags_put(update, AN_BIT, False if an_zero else bool(bits & 0x80000000))
    update = _flags_put(update, AZ_BIT, None)
    return _flags_put(update, AN_BIT, False if an_zero else None)


def _astatx_bit_test(source: Value, mask: int, xor: bool) -> bool | None:
    """BIT TST / BIT XOR (SHARC+ Core Programming Reference, "Type 18a
    ISA/VISA (register bit manipulation)", p.401/16-2-16-3): the test
    operation sets BTF if every bit set in the data value (MASK) is also
    set in the system register; the XOR operation sets BTF if the system
    register equals the data value exactly.

    Both predicates only ever need specific bits of SOURCE -- TST only the
    ones MASK sets, XOR all 32 -- so a PartialConst (ASTATX/ASTATY are the
    only registers ever stored that way, per ``PartialConst``'s docstring)
    can still decide the result when just those bits are known, and a
    single known bit that already disagrees decides it even with the rest
    unknown.
    """
    unknown = False
    for bit in range(32):
        want = bool(mask & (1 << bit))
        if not xor and not want:
            continue
        known = _astatx_known_bit(source, bit)
        if known is None:
            unknown = True
        elif known != want:
            return False
    return None if unknown else True


def _astatx_compare_float(value: Value, invalid: bool | None) -> FlagUpdate:
    """Float comp (PRM Table 3-3 AI='*'; PGR p.11-29 spells it out: "Set if
    either of the input operands is a NAN"). Identical to
    ``_astatx_compare``'s AC/AV/AS-clear, AZ/AN/CACC-from-VALUE and
    CACC-shift behaviour (which needs the *old* ASTATX, so it is reused
    rather than duplicated); only AI and AF differ from the fixed-point
    comp/compu version, which the PRM documents as always 0/0 rather than
    float compare's AI=data-dependent, AF=1.
    """
    after = _flags_put(_flags_define(1 << AF_BIT, 1 << AF_BIT), AI_BIT, invalid)
    return _flags_then(_astatx_compare(value), after)


def _alu_result_bits(value: Const) -> int:
    """AN/AZ for a pass/not/and/or/xor result (PRM pp.449-452); AC/AV/AS/AI
    are always 0 for these."""
    bits = 0
    if value.value & 0x80000000:
        bits |= 1 << AN_BIT
    if value.value == 0:
        bits |= 1 << AZ_BIT
    return bits


def _arith_flag_bits(a: Const, b: Const, subtract: bool) -> int:
    """AC/AV/AN/AZ for add/subtract/increment/decrement (PRM pp.439-440,
    446-447); AS/AI are always 0.

    AC is the carry out of the MSB adder stage; AV is the XOR of the carries
    into and out of the MSB adder stage (the standard two's-complement
    signed-overflow test). Subtraction is modelled the way the ALU does it:
    add the one's complement of B with a forced carry-in of 1 (so decrement,
    RX - 1, is add(RX, 1, subtract=True), matching the PRM wording exactly).
    """
    A = a.value & 0xFFFFFFFF
    if subtract:
        b_eff, carry_in = (~b.value) & 0xFFFFFFFF, 1
    else:
        b_eff, carry_in = b.value & 0xFFFFFFFF, 0
    low31 = (A & 0x7FFFFFFF) + (b_eff & 0x7FFFFFFF) + carry_in
    carry_into_msb = (low31 >> 31) & 1
    full = A + b_eff + carry_in
    carry_out = (full >> 32) & 1
    result = full & 0xFFFFFFFF
    bits = 0
    if carry_out:
        bits |= 1 << AC_BIT
    if carry_into_msb ^ carry_out:
        bits |= 1 << AV_BIT
    if result & 0x80000000:
        bits |= 1 << AN_BIT
    if result == 0:
        bits |= 1 << AZ_BIT
    return bits


def _astatx_alu_logical(value: Value) -> FlagUpdate:
    """pass/not/and/or/xor: AC/AV/AS/AI/AF cleared; AN/AZ from VALUE."""
    if isinstance(value, Const):
        return _flags_define(ALU_FLAGS_MASK, _alu_result_bits(value))
    return _flags_forget(ALU_FLAGS_MASK)


def _astatx_alu_arith(
    a: Value, b: Value, subtract: bool, *, same_source: bool = False
) -> FlagUpdate:
    """add/subtract/increment/decrement: AC/AV/AN/AZ from A and B; AS/AI/AF
    cleared.

    SAME_SOURCE mirrors ``_subtract``'s: with SUBTRACT=True it means A and B
    are the same operand read twice (the "Rn = Rn - Rn" self-clear idiom),
    so the flags are those of 0-0 (AZ/AC set, AN/AV clear) regardless of
    what value that operand held, even an Unknown one."""
    if same_source and subtract:
        return _flags_define(ALU_FLAGS_MASK, _arith_flag_bits(Const(0), Const(0), True))
    if isinstance(a, Const) and isinstance(b, Const):
        return _flags_define(ALU_FLAGS_MASK, _arith_flag_bits(a, b, subtract))
    return _flags_forget(ALU_FLAGS_MASK)


def _astatx_abs(source: Value) -> FlagUpdate:
    """abs (PGR p.11-13/11-14): AC/AV/AN/AZ come from the same adder the
    value itself is computed with -- 0-RX (subtract) when RX is negative,
    0+RX (add, i.e. an ordinary passthrough) when it is not -- so AN/AZ
    always agree with the actual returned value, and AC/AV are trivially 0
    on the positive branch (adding 0 cannot carry or overflow) but can be
    set on the negative branch (ABS(INT_MIN) overflows, matching the PGR
    text, exactly like negate(INT_MIN)). AS is set from RX's own sign
    (unlike negate, whose AS is always cleared); AI cleared; AF cleared
    (every fixed-point ALU op clears AF, PRM p.439)."""
    if isinstance(source, Const):
        negative = bool(source.value & 0x80000000)
        bits = _arith_flag_bits(Const(0), source, negative)
        adder = (1 << AZ_BIT) | (1 << AV_BIT) | (1 << AN_BIT) | (1 << AC_BIT)
        return _flags_define(
            ALU_FLAGS_MASK, (bits & adder) | ((1 << AS_BIT) if negative else 0)
        )
    return _flags_forget(ALU_FLAGS_MASK)


def _arith_flag_bits_ci(a: Const, b: Const, subtract: bool, carry_in: bool) -> int:
    """AC/AV/AN/AZ for RN = RX+RY+ci / RN = RX-RY+ci-1 (PRM p.438-439; PGR
    Table 12-3 opcodes 0000 0101/0000 0110, p.573); AS/AI are always 0,
    matching plain add/subtract (PRM: "AS Cleared", "AI Cleared" for both).

    Same two's-complement adder model as ``_arith_flag_bits``, with the
    ASTATX AC bit supplied as an explicit carry-in. The identity RX - RY +
    ci - 1 = RX + ~RY + ci (two's complement: ~RY = -RY-1) means the
    subtract-with-borrow row needs no forced +1 of its own -- CI itself
    supplies the carry that ordinary subtract hard-codes to 1 -- so this
    reuses the exact same b_eff = ~B one's-complement substitution as
    ``_arith_flag_bits``, just with CI standing in for the fixed carry_in.
    """
    A = a.value & 0xFFFFFFFF
    b_eff = (~b.value if subtract else b.value) & 0xFFFFFFFF
    ci = 1 if carry_in else 0
    low31 = (A & 0x7FFFFFFF) + (b_eff & 0x7FFFFFFF) + ci
    carry_into_msb = (low31 >> 31) & 1
    full = A + b_eff + ci
    carry_out = (full >> 32) & 1
    result = full & 0xFFFFFFFF
    bits = 0
    if carry_out:
        bits |= 1 << AC_BIT
    if carry_into_msb ^ carry_out:
        bits |= 1 << AV_BIT
    if result & 0x80000000:
        bits |= 1 << AN_BIT
    if result == 0:
        bits |= 1 << AZ_BIT
    return bits


def _astatx_alu_arith_ci(
    a: Value, b: Value, subtract: bool, carry_in: bool | None
) -> FlagUpdate:
    """add-with-carry/subtract-with-borrow: AC/AV/AN/AZ from A, B and the
    ASTATX AC carry-in; AS/AI/AF cleared, same as ``_astatx_alu_arith``.
    Forgets the flags (rather than defining them) whenever the carry-in
    itself is unknown, not just when A or B is."""
    if isinstance(a, Const) and isinstance(b, Const) and carry_in is not None:
        return _flags_define(
            ALU_FLAGS_MASK, _arith_flag_bits_ci(a, b, subtract, carry_in)
        )
    return _flags_forget(ALU_FLAGS_MASK)


def _astatx_compare(value: Value) -> FlagUpdate:
    """PRM comp/compu: AC/AV/AS/AI/AF clear; AZ/AN from VALUE (bits 0, 2);
    CACC (bits 31:24) is an 8-bit shift register, newest bit (VALUE bit 31)
    entering at bit 31. The shift needs the old CACC bits, so it is only
    computed exactly when the old ASTATX is fully known; otherwise CACC
    becomes unknown while the other newly defined bits do not (FlagUpdate's
    CACC field).
    """
    if not isinstance(value, Const):
        return _flags_forget(ALU_FLAGS_MASK | CACC_MASK)
    new_low = value.value & ((1 << AZ_BIT) | (1 << AN_BIT))
    return FlagUpdate(ALU_FLAGS_MASK, new_low, CACC_MASK, (value.value >> 31) & 1)


# multiply/multiply-add-mrf/saturate-mrf/multiply-accumulate: the tracer
# does not model the multiplier result format, so MN/MV/MU/MI are always
# unknown.
MULT_FLAGS_FORGET = _flags_forget(MULT_FLAGS_MASK)
# mr-data-move: PRM p.493 documents MU/MN/MI/MV all cleared.
MULT_FLAGS_CLEAR = _flags_define(MULT_FLAGS_MASK, 0)
# Fixed-point multiply/multiply-mrf/multiply-accumulate rows of PRM Table
# 3-7 (p.3-12): MN/MV/MU are data-dependent on the unmodeled multiplier
# result format, so they stay unknown like MULT_FLAGS_FORGET; MI is
# documented 0 on every fixed-point row there (it only ever applies to the
# floating-point row), so it is defined rather than forgotten.
MULT_FLAGS_FIXED = FlagUpdate(
    1 << MI_BIT, 0, (1 << MN_BIT) | (1 << MV_BIT) | (1 << MU_BIT)
)
# sat mrf/mrb MOD2 row of PRM Table 3-7 (p.3-12): MN/MV are data-dependent
# and unmodeled, but that row documents MU and MI as fixed 0 (unlike the
# plain multiply/accumulate rows, where only MI is fixed).
MULT_FLAGS_SAT = FlagUpdate(
    (1 << MU_BIT) | (1 << MI_BIT), 0, (1 << MN_BIT) | (1 << MV_BIT)
)


def _mult_flags(
    mn: bool | None, mv: bool | None, mu: bool | None, mi: bool | None
) -> FlagUpdate:
    """Multiplier flags MN/MV/MU/MI: True/False defines each, None forgets it."""
    update = _flags_put(FLAGS_NONE, MN_BIT, mn)
    update = _flags_put(update, MV_BIT, mv)
    update = _flags_put(update, MU_BIT, mu)
    return _flags_put(update, MI_BIT, mi)


def _astatx_bit_field(position: Value, result: Value) -> FlagUpdate:
    """bset/bclr/btgl reg and immediate (PRM pp.511-513): SS cleared; SZ =
    output == 0; SV = bit position > 31. An immediate POSITION is passed as
    a Const."""
    update = _flags_define(1 << SS_BIT, 0)
    if not isinstance(position, Const):
        update = _flags_put(update, SV_BIT, None)
        return _flags_put(update, SZ_BIT, None)
    update = _flags_put(update, SV_BIT, position.value > 31)
    return _flags_put(
        update, SZ_BIT, (result.value == 0) if isinstance(result, Const) else None
    )


def _astatx_fext(span: int, result: Value) -> FlagUpdate:
    """fext immediate (PRM pp.518-519): SS cleared; SZ = output == 0; SV =
    len6 + bit6 > 32. SPAN is len6+bit6, always known from the immediate."""
    update = _flags_define(1 << SS_BIT, 0)
    update = _flags_put(update, SV_BIT, span > 32)
    return _flags_put(
        update, SZ_BIT, (result.value == 0) if isinstance(result, Const) else None
    )


def _astatx_leftz(source: Value, result: Value) -> FlagUpdate:
    """leftz (PRM p.521): SS cleared; SZ = MSB of RX is 1; SV = result == 32."""
    update = _flags_define(1 << SS_BIT, 0)
    update = _flags_put(
        update,
        SZ_BIT,
        bool(source.value & 0x80000000) if isinstance(source, Const) else None,
    )
    return _flags_put(
        update, SV_BIT, (result.value == 32) if isinstance(result, Const) else None
    )


def _astatx_lefto(source: Value, result: Value) -> FlagUpdate:
    """lefto (PGR p.11-83): SS cleared; SZ = MSB of RX is 0; SV = result ==
    32. The mirror image of ``_astatx_leftz``'s SZ polarity (leading 1s are
    zero in count exactly when RX starts with a 0 bit)."""
    update = _flags_define(1 << SS_BIT, 0)
    update = _flags_put(
        update,
        SZ_BIT,
        not bool(source.value & 0x80000000) if isinstance(source, Const) else None,
    )
    return _flags_put(
        update, SV_BIT, (result.value == 32) if isinstance(result, Const) else None
    )


def _astatx_btst(source: Value, position: Value) -> FlagUpdate:
    """btst reg (PRM p.513): SS cleared; SZ set if the tested bit is 0 or the
    position is out of range, cleared if the tested bit is 1; SV = position >
    31. BTF is unaffected."""
    update = _flags_define(1 << SS_BIT, 0)
    if not isinstance(position, Const):
        update = _flags_put(update, SV_BIT, None)
        return _flags_put(update, SZ_BIT, None)
    pos = position.value
    out_of_range = pos > 31
    update = _flags_put(update, SV_BIT, out_of_range)
    if out_of_range:
        return _flags_put(update, SZ_BIT, True)
    if isinstance(source, Const):
        return _flags_put(update, SZ_BIT, not bool(source.value & (1 << pos)))
    return _flags_put(update, SZ_BIT, None)


def _astatx_shift(amount: int | None, shifted: Value, ss_mode: str) -> FlagUpdate:
    """lshift/ashift reg and immediate, OR-lshift/OR-ashift immediate (PRM
    pp.508-510): SZ = the shifted value (before any OR) is zero; SV = the
    shift amount is a left shift (> 0).

    SS is cleared for every one of these forms except OR-ashift, whose PRM
    entry omits an SS line entirely (unlike its OR-lshift sibling, which
    repeats "SS Cleared"); pass ss_mode="forget" there so SS becomes unknown
    instead of guessed, without touching any other already-known bit.
    """
    update = _flags_put(FLAGS_NONE, SS_BIT, False if ss_mode == "clear" else None)
    update = _flags_put(update, SV_BIT, None if amount is None else amount > 0)
    return _flags_put(
        update, SZ_BIT, (shifted.value == 0) if isinstance(shifted, Const) else None
    )
