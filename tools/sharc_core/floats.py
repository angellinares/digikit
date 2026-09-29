"""32-bit IEEE float and fixed/float conversion helpers.

The float primitives at the top of this module are the only code in the
core that uses struct, math or try (SUBSET.md, "Floats").

Moved verbatim from tools/sharc_trace.py.
"""

from __future__ import annotations

import math
import struct

from .encoding import (
    AC_BIT,
    AI_BIT,
    ALUSAT_BIT,
    AN_BIT,
    AS_BIT,
    AV_BIT,
    AZ_BIT,
    TRUNCATE_BIT,
)
from .values import (
    Const,
    FlagUpdate,
    Unknown,
    Value,
    _astatx_known_bit,
    _flags_from_pairs,
    _flags_put,
    _flags_then,
    _signed,
    _signed32,
)

# ---------------------------------------------------------------------------
# Floating-point compute support.
#
# The register file (R0-R15/F0-F15) is a flat 32-bit store either way; a
# float ALU/multiplier op just reinterprets the same bits as IEEE-754 single
# precision (PRM p.3-4: "floating-point instructions operate on 32-bit ...
# operands"). The SHARC+ ALU/multiplier additionally support an optional
# 40-bit extended-precision float format for *intermediate* results (PRM
# p.3-37, active when MODE1.RND32=0, the reset default: "eight additional
# LSBs of mantissa"), but that only matters to values forwarded between
# back-to-back compute ops without ever reaching the register file; this
# tracer has no pipeline/forwarding model and every UREG it tracks is a
# plain 32-bit value, so every float result here is computed and stored at
# IEEE-754 single precision -- the same width a real RN/FN write-back uses
# regardless of the extended-precision mode. Denormal-flush-to-zero, which
# several PGR entries below document for their inputs/outputs, is also not
# modeled (struct's round-trip preserves denormals exactly); this only
# matters for subnormal magnitudes, which real audio sample/parameter data
# essentially never produces.
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Float primitives. They are the only code in the core that uses struct,
# math or try (tools/sharc_core/SUBSET.md, "Floats"); everything else calls
# them. Python floats are IEEE-754 binary64, so each has an exact Rust
# equivalent on f64:
#
#   _f32_from_bits(b)      f32::from_bits(b) as f64
#   _float32_bits(x)       (x as f32).to_bits(), overflowed = the rounded
#                          value is infinite while x is finite
#   _f64_from_words(h, l)  f64::from_bits((h << 32) | l)
#   _double_pair_bits(x)   x.to_bits() split into (hi, lo)
#   _ldexp(x, n)           libm scalbn(x, n) (infinity on overflow)
#   _trunc_int(x)          x.trunc() as i64 (finite x; callers only range-check)
#   _round_even_int(x)     x.round_ties_even() as i64 (same)
#   _isnan/_isinf/_isfinite/_copysign/_sqrt   f64::is_nan/is_infinite/
#                          is_finite/copysign/sqrt
# ---------------------------------------------------------------------------

_isnan = math.isnan
_isinf = math.isinf
_isfinite = math.isfinite
_copysign = math.copysign
_sqrt = math.sqrt
_INF = math.inf
# The smallest float32 normal magnitude; a result below it is a denormal.
_F32_MIN_NORMAL = 2.0**-126

_PACK_U32 = struct.Struct("<I").pack
_UNPACK_F32 = struct.Struct("<f").unpack
_PACK_F32 = struct.Struct("<f").pack
_UNPACK_U32 = struct.Struct("<I").unpack
_PACK_U64 = struct.Struct(">Q").pack
_UNPACK_F64 = struct.Struct(">d").unpack
_PACK_F64 = struct.Struct(">d").pack
_UNPACK_U64 = struct.Struct(">Q").unpack


def _f32_from_bits(bits: int) -> float:
    """The float32 whose bit pattern is BITS (32 bits), as a Python float."""
    return _UNPACK_F32(_PACK_U32(bits))[0]


def _f64_from_words(hi: int, lo: int) -> float:
    """The binary64 whose bit pattern is HI:LO (two 32-bit words)."""
    return _UNPACK_F64(_PACK_U64(((hi & 0xFFFFFFFF) << 32) | (lo & 0xFFFFFFFF)))[0]


def _ldexp(value: float, exponent: int) -> float:
    """VALUE * 2**EXPONENT, rounded once; +-infinity on overflow (math.ldexp
    raises instead), a denormal or zero on underflow."""
    try:
        return math.ldexp(value, exponent)
    except OverflowError:
        return math.copysign(math.inf, value)


def _trunc_int(value: float) -> int:
    """VALUE (finite) rounded toward zero, as an integer."""
    return math.trunc(value)


def _round_even_int(value: float) -> int:
    """VALUE (finite) rounded to the nearest integer, ties to even."""
    return int(round(value))


def _float32(value: Value) -> float | None:
    """Reinterpret VALUE's 32-bit pattern as IEEE-754 single precision.

    Returns None when VALUE isn't a fully known Const: an Affine (symbolic
    address arithmetic) or Unknown source is not float data and must not be
    silently coerced into one.
    """
    if not isinstance(value, Const):
        return None
    return _f32_from_bits(value.value)


def _float32_bits(value: float) -> tuple[int, bool]:
    """Round VALUE to IEEE-754 single precision; return (bits, overflowed).

    struct raises OverflowError for a finite double outside the float32
    range. SHARC+ float overflow rounds to signed infinity in the default
    round-to-nearest mode (PRM Table 3-3 / PGR p.11-24 AV description:
    "post-rounded result overflows ... returns +-infinity"); reproduce that
    by hand when struct refuses, since struct has no float32-infinity
    fallback of its own.
    """
    try:
        return _UNPACK_U32(_PACK_F32(value))[0], False
    except OverflowError:
        sign = 0x80000000 if _copysign(1.0, value) < 0 else 0
        return (0x7F800000 | sign), True


_FLOAT_ALL_ONES = Const(0xFFFFFFFF)


def _float_binary(
    left: Value, right: Value, expression: str, operation
) -> tuple[Value, bool | None, bool | None]:
    """Evaluate a float ALU/multiplier binary OPERATION; return (result,
    overflowed, invalid).

    Several PGR float-ALU entries (e.g. p.11-24 Fx+Fy, p.11-46 MIN, p.11-48
    CLIP) document "A NAN input returns an all 1s result" -- a fixed
    sentinel pattern, not whatever IEEE NaN OPERATION would naturally
    produce -- so an explicit NaN *input* is special-cased before OPERATION
    ever runs. A NaN produced by OPERATION itself from two non-NaN inputs
    (e.g. +infinity + -infinity, PGR p.11-24's "opposite-signed infinities"
    AI case) is not overridden: it keeps the ordinary computed NaN pattern.
    Either input not being a known Const makes the whole result unknown.
    """
    a, b = _float32(left), _float32(right)
    if a is None or b is None:
        return Unknown(expression), None, None
    if _isnan(a) or _isnan(b):
        return _FLOAT_ALL_ONES, False, True
    raw = operation(a, b)
    bits, overflowed = _float32_bits(raw)
    return Const(bits), overflowed, _isnan(raw)


def _float_unary(
    value: Value, expression: str, operation
) -> tuple[Value, bool | None, bool | None]:
    """Unary counterpart of ``_float_binary`` (see its docstring)."""
    a = _float32(value)
    if a is None:
        return Unknown(expression), None, None
    if _isnan(a):
        return _FLOAT_ALL_ONES, False, True
    raw = operation(a)
    bits, overflowed = _float32_bits(raw)
    return Const(bits), overflowed, _isnan(raw)


def _f_add(a: float, b: float) -> float:
    return a + b


def _f_sub(a: float, b: float) -> float:
    return a - b


def _f_mul(a: float, b: float) -> float:
    return a * b


def _f_avg(a: float, b: float) -> float:
    """(A + B) / 2 in double precision, before the single rounding."""
    return (a + b) / 2


def _f_neg(a: float) -> float:
    return -a


def _f_pass(a: float) -> float:
    return a


def _float_min(a: float, b: float) -> float:
    """PGR p.11-46: smaller operand; min(+0, -0) is documented as -0."""
    if a == 0.0 and b == 0.0:
        return -0.0
    return a if a < b else b


def _float_max(a: float, b: float) -> float:
    """PGR p.11-47: larger operand; max(+0, -0) is documented as +0."""
    if a == 0.0 and b == 0.0:
        return 0.0
    return a if a > b else b


def _float_clip(a: float, b: float) -> float:
    """PGR p.11-48 / PRM p.3-6 CLIP: FX if |FX| < |FY|, else +-|FY| with
    FX's sign (copysign handles the FX=+-0 boundary the same as the PGR
    text's "if Fx is positive")."""
    return a if abs(a) < abs(b) else _copysign(abs(b), a)


def _float_mantissa(
    value: Value, expression: str
) -> tuple[Value, bool | None, bool | None, bool | None]:
    """RN = mant FX (PRM Table 18-5 opcode 0xAD, p.427; PGR p.11-34/11-35).

    Extracts the hidden bit plus the 23-bit fraction, left-justified as an
    unsigned-magnitude 1.31 fixed-point word (bit31 the hidden bit, bits
    30-8 the fraction, bits 7-0 zero-filled); the 24 significant bits
    always fit exactly, so no rounding is performed (PGR: "no rounding is
    performed because all results are inherently exact"). Denormal and
    zero inputs flush to a zero mantissa (PGR: "Denormal inputs are
    flushed to +-zero"). A NAN *or an infinity* input returns the fixed
    all-1s sentinel (PGR: "A NAN or an infinity input returns an all 1s
    result") -- unlike the arithmetic float ALU ops above, which only
    override NAN, MANT also overrides infinity, since it is not an
    ordinary IEEE operation. Returns (result, overflow=is-infinity,
    negative=input-sign, invalid=is-NAN); AN is always cleared for this
    op (PRM Table 3-3) and is not returned here.
    """
    if not isinstance(value, Const):
        return Unknown(expression), None, None, None
    bits = value.value
    sign = bool(bits & 0x80000000)
    exponent = (bits >> 23) & 0xFF
    fraction = bits & 0x7FFFFF
    if exponent == 0xFF:
        is_nan = fraction != 0
        return _FLOAT_ALL_ONES, not is_nan, sign, is_nan
    if exponent == 0:
        return Const(0), False, sign, False
    return Const((0x800000 | fraction) << 8), False, sign, False


def _float_logb(
    value: Value, mode1: Value, expression: str
) -> tuple[Value, bool | None, bool]:
    """RN = logb FX (PGR p.11-36, pgr.txt:21522): the unbiased two's-
    complement exponent of FX as a fixed-point integer (biased_exp - 127).
    A NAN input returns the all-1s sentinel. A +-infinity or +-zero input
    (denormals flush to zero first, same general rule ``_float_mantissa``
    already cites) returns a value gated on MODE1.ALUSAT exactly like
    ``_float_to_fixed``'s saturation branches: unsaturated, PGR says these
    return the *floating-point* +infinity/-infinity bit pattern verbatim in
    the (nominally fixed-point) result register; saturated, the maximum
    positive/negative fixed-point integer. Returns (result,
    overflow=input-is-inf-or-zero, invalid=is-NAN); AV/AI feed AV/AI
    directly, AN/AZ come from RESULT's own bits at the call site (RESULT is
    a plain integer here, not a float, so ``_float_alu_updates`` does not
    apply)."""
    if not isinstance(value, Const):
        return Unknown(expression), None, False
    bits = value.value & 0xFFFFFFFF
    biased_exp = (bits >> 23) & 0xFF
    mantissa = bits & 0x7FFFFF
    if biased_exp == 0xFF and mantissa != 0:
        return _FLOAT_ALL_ONES, False, True
    if biased_exp == 0xFF or biased_exp == 0:
        saturating = _astatx_known_bit(mode1, ALUSAT_BIT)
        if saturating is None:
            return Unknown(expression), True, False
        if biased_exp == 0xFF:
            result = Const(0x7FFFFFFF) if saturating else Const(0x7F800000)
        else:
            result = Const(0x80000000) if saturating else Const(0xFF800000)
        return result, True, False
    return Const(_signed(biased_exp - 127, 32) & 0xFFFFFFFF), False, False


def _float_scalb(
    value: Value, scale: Value, expression: str
) -> tuple[Value, bool | None, bool | None]:
    """FN = scalb FX by RY (PRM Table 18-5 opcode 0xBD, p.427; PGR p.11-33).

    Adds the two's-complement fixed-point integer RY to FX's exponent
    (i.e. FX * 2**RY). Overflow rounds to +-infinity (round-to-nearest,
    the only rounding mode this tracer models, matching every other float
    op here); a result whose magnitude underflows below the smallest
    float32 normal (2**-126) flushes to +-zero rather than becoming a
    subnormal (PGR: "Denormal returns +-zero" -- an explicit override of
    struct's ordinary IEEE denormal rounding, the same kind of override
    ``_float_to_fixed_trunc`` already applies for its own corner cases). A
    NAN input returns the same all-1s sentinel ``_float_binary`` uses;
    zero and infinity inputs pass through unchanged (``math.ldexp``
    preserves both, matching the PRM, which documents no special case for
    them). Returns (result, overflow, invalid).
    """
    a = _float32(value)
    if a is None or not isinstance(scale, Const):
        return Unknown(expression), None, None
    if _isnan(a):
        return _FLOAT_ALL_ONES, False, True
    shift = _signed32(scale.value)
    scaled = _ldexp(a, shift)
    if scaled != 0.0 and not _isinf(scaled) and abs(scaled) < _F32_MIN_NORMAL:
        return Const(0x80000000 if scaled < 0 else 0), False, False
    bits, overflowed = _float32_bits(scaled)
    return Const(bits), overflowed, False


def _fixed_to_float(value: Value, expression: str) -> tuple[Value, bool | None]:
    """FN = float RX (PRM Table 18-5 opcode 0xCA, p.427; PGR p.11-39 "without
    scaling factor"): numeric int32->float32 conversion, not a bit
    reinterpretation. PGR documents AV and AI both fixed 0 for the
    no-scaling form actually used here (RN=FLOAT RX BY RY, which also takes
    a scale factor, is not implemented). Returns (result, invalid) where
    invalid is always False when computable, matching that fixed AI=0.
    """
    if not isinstance(value, Const):
        return Unknown(expression), None
    bits, _ = _float32_bits(float(_signed32(value.value)))
    return Const(bits), False


def _float_to_fixed(
    value: Value, mode1: Value, always_truncate: bool, expression: str
) -> tuple[Value, bool | None, bool | None]:
    """RN = FIX FX / RN = TRUNC FX (PRM Table 18-5 opcodes 0xC9/0xCD, p.427;
    PGR p.11-36..11-38), and their scaled BY RY siblings 0xD9/0xDD (the
    caller pre-scales VALUE via ``_scale_fixed_input`` -- PGR Table 3-3,
    p.3-11 marks the BY RY forms with the identical AZ/AV/AN/AC/AS/AI
    columns as the unscaled ones, so no separate flag rule is needed here).

    TRUNC always truncates toward zero, ignoring MODE1 (ALWAYS_TRUNCATE=True
    -- PGR p.11-37: "The trunc operation always truncates toward 0. The
    TRUNCATE bit does not influence operation of the trunc instruction.").
    FIX instead rounds to nearest, ties to even (Python's ``round()`` on a
    float already implements this) when MODE1.TRUNCATE=0, or truncates
    toward zero when MODE1.TRUNCATE=1 (PGR p.11-37 / PRM p.24-..); the whole
    result is Unknown when TRUNCATE itself is unknown, rather than guessing
    which rounding applied.

    A result within int32 range needs no saturation. Out-of-range magnitudes
    and NAN/+-infinity inputs follow the SHARC+ Core PGR's own FIX/TRUNC
    pages verbatim (out/refs/sc58x-2158x-prm/all.txt pp.20-11..20-14, "RN =
    fix/trunc Fx[ by Ry]" -- identical wording on all four pages):

    - A NAN input is unconditional on ALUSAT: "A NAN input returns a
      floating-point all 1s result" is its own sentence, not part of the
      "If saturation mode is not set" clause that follows it, and the AI
      flag bullet lists "the input operand is a NAN" separately from the
      saturation-gated "either input is an infinity or the result
      overflows". AV's own bullet never mentions NAN (only "+-infinity"),
      matching this module's existing NAN convention elsewhere (see
      ``_float_binary``'s docstring): overflow=False, invalid=True.
    - "In saturation mode (ALUSAT=1) positive overflows and +infinity
      return 0x7FFFFFFF, and negative overflows and -infinity return
      0x8000 0000" -- AV is set (the AV bullet: "if the input is
      +-infinity" / mantissa-shift condition, with no ALUSAT gate), AI is
      cleared (the AI bullet's infinity/overflow clause only fires "when
      saturation mode is not set").
    - "If saturation mode is not set, an infinity input or a result that
      overflows returns a floating-point result of all 1s" -- AV is set
      (same unconditional bullet as above) and now AI is also set (the
      "when saturation mode is not set" clause applies).
    - When ALUSAT itself is unknown, an infinity input or numeric overflow
      still sets AV (that bullet never depends on ALUSAT), but the result
      and AI are Unknown since which saturation behaviour applies cannot be
      told apart.

    Returns (result, overflow, invalid).
    """
    a = _float32(value)
    if a is None:
        return Unknown(expression), None, None
    saturating = _astatx_known_bit(mode1, ALUSAT_BIT)
    if _isnan(a):
        return _FLOAT_ALL_ONES, False, True
    if _isinf(a):
        if saturating:
            return Const(0x7FFFFFFF if a > 0 else 0x80000000), True, False
        if saturating is False:
            return _FLOAT_ALL_ONES, True, True
        return Unknown(expression), True, None
    if always_truncate:
        rounded = _trunc_int(a)
    else:
        truncate_mode = _astatx_known_bit(mode1, TRUNCATE_BIT)
        if truncate_mode is None:
            return Unknown(expression), None, False
        rounded = _trunc_int(a) if truncate_mode else _round_even_int(a)
    if -(1 << 31) <= rounded <= (1 << 31) - 1:
        return Const(rounded & 0xFFFFFFFF), False, False
    if saturating:
        return Const(0x7FFFFFFF if rounded > 0 else 0x80000000), True, False
    if saturating is False:
        return _FLOAT_ALL_ONES, True, True
    return Unknown(expression), True, None


def _float_to_fixed_trunc(
    value: Value, mode1: Value, expression: str
) -> tuple[Value, bool | None, bool | None]:
    """RN = TRUNC FX: ``_float_to_fixed`` with ALWAYS_TRUNCATE=True. Kept as
    a named wrapper since opcode 0xCD's call site predates the shared
    FIX/TRUNC helper and reads more clearly with its own name."""
    return _float_to_fixed(value, mode1, True, expression)


def _scale_fixed_input(value: Value, scale: Value, expression: str) -> Value:
    """Fx * 2**Ry (exponent add), the shared first step of RN = FIX/TRUNC FX
    BY RY (PGR p.11-37: "the fixed-point two's-complement integer in Ry is
    added to the exponent of the floating-point operand in Fx before the
    conversion"). Reuses ``_float_scalb``'s ldexp/overflow/denormal-flush
    rule -- the same exponent-add primitive FN=SCALB uses -- and discards
    its own (overflow, invalid) pair, since the caller's FIX/TRUNC applies
    its own overflow/NAN handling to the scaled value afterward.
    """
    scaled, _overflow, _invalid = _float_scalb(value, scale, expression)
    return scaled


def _fixed_to_float_scaled(
    value: Value, scale: Value, expression: str
) -> tuple[Value, bool | None]:
    """FN = FLOAT RX BY RY (PRM p.19-.. ; PGR p.11-39 "with scaling factor"):
    numeric int32->float32 conversion as ``_fixed_to_float``, then the
    fixed-point two's-complement integer in RY is added to the result's
    exponent (PGR: "the fixed-point two's-complement integer in Ry is added
    to the exponent of the floating-point result"). Overflow (unbiased
    exponent > 127) returns +-infinity; underflow (unbiased exponent <
    -126) flushes to +-zero (PGR: "Overflow generates a return of
    +-infinity ...; underflow generates a return of +-zero"). Unlike the
    unscaled form -- where an int32 input can never land in the subnormal
    range, so AV is architecturally fixed 0 (PGR Table 3-3) -- AV here is
    genuinely data-dependent, so this returns (result, overflow) rather
    than the unscaled helper's implicit always-False.
    """
    if not isinstance(value, Const) or not isinstance(scale, Const):
        return Unknown(expression), None
    unscaled = float(_signed32(value.value))
    shift = _signed32(scale.value)
    scaled = _ldexp(unscaled, shift)
    if scaled != 0.0 and not _isinf(scaled) and abs(scaled) < _F32_MIN_NORMAL:
        return Const(0x80000000 if scaled < 0 else 0), False
    bits, overflowed = _float32_bits(scaled)
    return Const(bits), overflowed


def _float_copysign(
    left: Value, right: Value, expression: str
) -> tuple[Value, bool | None]:
    """FN = FX copysign FY (PRM p.19-19, opcode 1110 0000; PGR p.11-45,
    Table 12-4 opcode 1110 0000): copies FY's sign bit onto FX's exponent
    and mantissa unchanged. A denormal FX input flushes to zero before the
    sign copy (PRM/PGR: "A denormal input is flushed to +-zero"). A NAN
    input -- either operand -- returns the fixed all-1s sentinel
    (``_float_binary``'s convention). Returns (result, invalid); AC/AS/AV
    are architecturally fixed for this op (PRM Table 3-3 / PGR Table 3-3)
    and are not returned here.
    """
    a, b = _float32(left), _float32(right)
    if a is None or b is None:
        return Unknown(expression), None
    if _isnan(a) or _isnan(b):
        return _FLOAT_ALL_ONES, True
    magnitude = abs(a)
    if 0.0 < magnitude < _F32_MIN_NORMAL:
        magnitude = 0.0
    negative = _copysign(1.0, b) < 0
    result = -magnitude if negative else magnitude
    bits, _overflowed = _float32_bits(result)
    return Const(bits), False


def _float_round32(value: Value, expression: str) -> tuple[Value, bool | None]:
    """FN = rnd FX (PRM Table 18-5 opcode 1010 0101, p.20-8 "32-bit and
    40-bit Operations"; PGR Table 12-4 opcode 1010 0101, pp.12-3/12-4, and
    p.11-33): rounds FX to a 32-bit floating-point boundary.

    The PRM's own wording for the rounding-mode choice ("as defined by the
    REGF_MODE1.RND32 bit") is inconsistent with what RND32 documents
    elsewhere in the same manual (register-map chapter, p.29-55: RND32
    selects whether the computational units round floating-point data to
    32 bits or 40 bits -- an output-width choice, not a nearest-vs-truncate
    one) and with the classic PGR's wording for the identical op ("the
    rounding mode bit in MODE1"); this follows the PGR and every other
    rounding-mode citation in this file (MODE1.TRUNCATE -- see
    ``_float_to_fixed``'s docstring).

    That rounding-mode choice is not observable here regardless: per this
    module's "Floating-point compute support" header comment, every UREG
    this tracer tracks is already stored at IEEE-754 single precision, so
    FX is already rounded to the 32-bit boundary this op targets, and
    re-rounding an already-32-bit-precision, finite, normal input is a
    no-op under either rounding rule. The "post-rounded overflow" corner
    the manual documents only arises from rounding away mantissa bits
    beyond 32-bit precision, which this tracer never carries between ops;
    the float-pass op (opcode 0xA1, via ``_float_unary``) already fixes
    AV=False on the same reasoning, so this does too. A denormal input
    still flushes to +-zero (this op documents that override explicitly,
    the same as ``_float_copysign`` above); a NAN input returns the fixed
    all-1s sentinel. Returns (result, invalid).
    """
    a = _float32(value)
    if a is None:
        return Unknown(expression), None
    if _isnan(a):
        return _FLOAT_ALL_ONES, True
    magnitude = abs(a)
    if 0.0 < magnitude < _F32_MIN_NORMAL:
        return Const(0x80000000 if a < 0 else 0), False
    bits, _overflowed = _float32_bits(a)
    return Const(bits), False


# ---------------------------------------------------------------------------
# 64-bit (IEEE double) floating-point compute support (ADSP-SC58x/2158x PRM,
# out/refs/sc58x-2158x-prm; not in the classic PRM/PGR this project otherwise
# cites -- SHARC+ adds these on top of the base ISA).
#
# A "DBLREG" operand Fm:n/Fx:y/Fz:w is a neighbor register pair: p.3-35
# Table 3-20 ("pair F1:0 consists of R1 and R0"), decoded from the *same*
# rn/rx/ry 4-bit field positions as a 32-bit compute (Table 18-23/Figure
# 18-1); only even codes are valid pair selectors (p.18-16/17 Table 18-27/
# 18-28: opcode 0000->F1:0, 0010->F3:2, ... 1110->F15:14, odd codes "-").
# The higher-numbered register holds the more-significant half (all.txt:
# 32507 "REGF_MR0F contains the least significant 32 bits", 32651
# "REGF_MR2F contains the most significant 16 bits" -- the same low-number-
# is-low-half convention applied to a register pair), matching Figure 28-2's
# (p.28-2) standard IEEE-754 binary64 layout (sign:bit63, 11-bit exponent:
# bits62-52, 52-bit mantissa:bits51-0) split across hi=bits63-32/lo=bits31-0.
#
# Python floats already are IEEE-754 binary64, so (unlike the float32 path
# above, which has to hand-round every result through struct) ordinary
# Python arithmetic on the unpacked doubles already produces the correctly-
# rounded IEEE double result, including overflow-to-infinity. Matching this
# module's own documented simplification for the 32-bit path (this file's
# header comment: denormal-flush-to-zero "is also not modeled ... this only
# matters for subnormal magnitudes, which real audio sample/parameter data
# essentially never produces"), the 64-bit ops below do not flush denormal
# inputs/outputs to zero either, even though several PRM pages document that
# as the real hardware's behaviour: AZ/MU still report a denormal result
# correctly (computed from its real bit pattern), only the *flush* itself is
# skipped.
# ---------------------------------------------------------------------------

_DOUBLE_ALL_ONES_HI = Const(0xFFFFFFFF)
_DOUBLE_ALL_ONES_LO = Const(0xFFFFFFFF)


def _double(hi: Value, lo: Value) -> float | None:
    """Combine a register pair (HI = more-significant half, LO =
    less-significant half) into a Python double. None when either half
    isn't a fully known Const."""
    if not isinstance(hi, Const) or not isinstance(lo, Const):
        return None
    return _f64_from_words(hi.value, lo.value)


def _double_pair_bits(value: float) -> tuple[int, int]:
    """(hi, lo) 32-bit halves of VALUE's IEEE-754 binary64 pattern. Unlike
    ``_float32_bits``, this never raises/needs an overflow fallback: VALUE
    is already a Python double (the type every 64-bit op here computes
    with), so it already IS the correctly-rounded binary64 result verbatim
    (including a literal ``inf`` on overflow)."""
    bits = _UNPACK_U64(_PACK_F64(value))[0]
    return (bits >> 32) & 0xFFFFFFFF, bits & 0xFFFFFFFF


def _double_binary(
    hi_a: Value, lo_a: Value, hi_b: Value, lo_b: Value, expression: str, operation
) -> tuple[Value, Value, bool | None, bool | None]:
    """Evaluate a 64-bit float ALU/multiplier binary OPERATION; return
    (hi_result, lo_result, overflowed, invalid). Mirrors ``_float_binary``'s
    NAN-input override (PRM p.20-22 "A NAN input returns an all 1s
    result") and overflow detection (a literal ``inf`` OPERATION result
    with two finite inputs -- the real IEEE-754 double range, unlike
    float32, so no manual struct-overflow fallback is needed)."""
    a, b = _double(hi_a, lo_a), _double(hi_b, lo_b)
    if a is None or b is None:
        return Unknown(expression), Unknown(expression), None, None
    if _isnan(a) or _isnan(b):
        return _DOUBLE_ALL_ONES_HI, _DOUBLE_ALL_ONES_LO, False, True
    raw = operation(a, b)
    hi, lo = _double_pair_bits(raw)
    overflowed = _isinf(raw) and _isfinite(a) and _isfinite(b)
    return Const(hi), Const(lo), overflowed, _isnan(raw)


def _double_unary(
    hi: Value, lo: Value, expression: str, operation
) -> tuple[Value, Value, bool | None, bool | None]:
    """Unary counterpart of ``_double_binary`` (see its docstring)."""
    a = _double(hi, lo)
    if a is None:
        return Unknown(expression), Unknown(expression), None, None
    if _isnan(a):
        return _DOUBLE_ALL_ONES_HI, _DOUBLE_ALL_ONES_LO, False, True
    raw = operation(a)
    hi_bits, lo_bits = _double_pair_bits(raw)
    return Const(hi_bits), Const(lo_bits), False, _isnan(raw)


def _double_compare(
    hi_a: Value, lo_a: Value, hi_b: Value, lo_b: Value, label: str
) -> tuple[Value, bool | None]:
    """comp(FX:Y, FZ:W) (SC58x/2158x PRM p.20-24): same AZ(bit0)/AN(bit2)/
    CACC-MSB(bit31) encoding as ``_compare_flags_float``, consumed the same
    way by ``_astatx_compare``."""
    a, b = _double(hi_a, lo_a), _double(hi_b, lo_b)
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


def _double_to_fixed(
    hi: Value, lo: Value, mode1: Value, always_truncate: bool, expression: str
) -> tuple[Value, bool | None, bool | None]:
    """RN = fix FX:Y / RN = trunc FX:Y (SC58x/2158x PRM p.20-28/20-30,
    p.20-29/20-32 for the BY RY forms -- the caller pre-scales HI/LO via
    ``_scale_double_input``): same rounding/saturation rule as
    ``_float_to_fixed`` (see its docstring for the NAN/infinity/overflow
    flag derivation -- the double FIX/TRUNC pages carry the identical
    wording, "A NAN input returns a floating point all 1s result" as its
    own unconditional sentence and the AI/AV bullets split the same way),
    reading a 64-bit input and always producing a plain 32-bit RN (Table
    18-28: the fix/trunc destination column is "Rn/Rx/Ry/Rz", not a
    register pair)."""
    a = _double(hi, lo)
    if a is None:
        return Unknown(expression), None, None
    saturating = _astatx_known_bit(mode1, ALUSAT_BIT)
    if _isnan(a):
        return _FLOAT_ALL_ONES, False, True
    if _isinf(a):
        if saturating:
            return Const(0x7FFFFFFF if a > 0 else 0x80000000), True, False
        if saturating is False:
            return _FLOAT_ALL_ONES, True, True
        return Unknown(expression), True, None
    if always_truncate:
        rounded = _trunc_int(a)
    else:
        truncate_mode = _astatx_known_bit(mode1, TRUNCATE_BIT)
        if truncate_mode is None:
            return Unknown(expression), None, False
        rounded = _trunc_int(a) if truncate_mode else _round_even_int(a)
    if -(1 << 31) <= rounded <= (1 << 31) - 1:
        return Const(rounded & 0xFFFFFFFF), False, False
    if saturating:
        return Const(0x7FFFFFFF if rounded > 0 else 0x80000000), True, False
    if saturating is False:
        return _FLOAT_ALL_ONES, True, True
    return Unknown(expression), True, None


def _scale_double_input(
    hi: Value, lo: Value, scale: Value, expression: str
) -> tuple[Value, Value]:
    """(hi, lo) of Fx:y * 2**Ry (exponent add), the shared first step of
    RN = FIX/TRUNC FX:Y BY RY (SC58x/2158x PRM p.20-29: "the fixed-point
    two's-complement integer in Ry is added to the exponent of the
    floating-point operand in Fx:y before the conversion"). The only two
    callers immediately feed this pair back into ``_double_to_fixed``."""
    a = _double(hi, lo)
    if a is None or not isinstance(scale, Const):
        return Unknown(expression), Unknown(expression)
    shift = _signed32(scale.value)
    # NAN/infinity pass straight through; _double_to_fixed applies its own
    # NAN/infinity special case to the unscaled value.
    scaled = a if _isnan(a) or _isinf(a) else _ldexp(a, shift)
    hi_bits, lo_bits = _double_pair_bits(scaled)
    return Const(hi_bits), Const(lo_bits)


def _fixed_to_double(value: Value, expression: str) -> tuple[Value, Value, bool | None]:
    """FM:N = float RX (SC58x/2158x PRM p.20-33): numeric int32->double
    conversion (always exact -- int32's range is a tiny sliver of double's),
    AI/AV architecturally fixed 0 (p.20-33/20-34 ASTATx/y Flags). Returns
    (hi, lo, invalid=False when computable)."""
    if not isinstance(value, Const):
        return Unknown(expression), Unknown(expression), None
    hi, lo = _double_pair_bits(float(_signed32(value.value)))
    return Const(hi), Const(lo), False


def _fixed_to_double_scaled(
    value: Value, scale: Value, expression: str
) -> tuple[Value, Value, bool | None]:
    """FM:N = float RX by RY (SC58x/2158x PRM p.20-34): ``_fixed_to_double``
    then RY's exponent add (p.20-34: "the fixed-point two's-complement
    integer in Ry is added to the exponent of the floating-point result").
    AV is data-dependent here (p.20-34: "Set if the result overflows
    (unbiased exponent > 1023)"), unlike the unscaled form."""
    if not isinstance(value, Const) or not isinstance(scale, Const):
        return Unknown(expression), Unknown(expression), None
    unscaled = float(_signed32(value.value))
    shift = _signed32(scale.value)
    scaled = _ldexp(unscaled, shift)
    hi, lo = _double_pair_bits(scaled)
    overflowed = _isinf(scaled)
    return Const(hi), Const(lo), overflowed


def _double_scalb(
    hi: Value, lo: Value, scale: Value, expression: str
) -> tuple[Value, Value, bool | None, bool | None]:
    """FM:N = scalb FX:Y by RY (SC58x/2158x PRM p.20-27): adds the
    fixed-point two's-complement integer in Ry to Fx:y's exponent. A NAN
    input returns the all-1s sentinel; zero/infinity pass through
    unchanged (``math.ldexp`` already preserves both)."""
    a = _double(hi, lo)
    if a is None or not isinstance(scale, Const):
        return Unknown(expression), Unknown(expression), None, None
    if _isnan(a):
        return _DOUBLE_ALL_ONES_HI, _DOUBLE_ALL_ONES_LO, False, True
    shift = _signed32(scale.value)
    scaled = _ldexp(a, shift)
    hi_bits, lo_bits = _double_pair_bits(scaled)
    overflowed = _isinf(scaled) and not _isinf(a)
    return Const(hi_bits), Const(lo_bits), overflowed, False


def _double_to_float32(
    hi: Value, lo: Value, expression: str
) -> tuple[Value, bool | None, bool | None]:
    """FN = cvt FX:Y (SC58x/2158x PRM p.20-36): double -> single-precision
    narrowing conversion, round-to-nearest (this tracer does not model
    MODE1.TRUNCATE's alternate rounding for this op, the same limitation
    ``_float_round32`` documents for its own rounding-mode citation).
    A NAN input returns the float32 all-1s sentinel; a finite double
    outside float32 range overflows to +-infinity (matching
    ``_float32_bits``'s own overflow fallback)."""
    a = _double(hi, lo)
    if a is None:
        return Unknown(expression), None, None
    if _isnan(a):
        return _FLOAT_ALL_ONES, False, True
    bits, overflowed = _float32_bits(a)
    return Const(bits), overflowed, False


def _float32_to_double(
    value: Value, expression: str
) -> tuple[Value, Value, bool | None]:
    """FM:N = cvt FX (SC58x/2158x PRM p.20-35): single -> double-precision
    widening conversion (always exact; every float32 value is exactly
    representable as a double). A NAN input returns the all-1s sentinel."""
    a = _float32(value)
    if a is None:
        return Unknown(expression), Unknown(expression), None
    if _isnan(a):
        return _DOUBLE_ALL_ONES_HI, _DOUBLE_ALL_ONES_LO, True
    hi, lo = _double_pair_bits(a)
    return Const(hi), Const(lo), False


def _approx_recips(left: Value) -> tuple[Value, FlagUpdate]:
    """Opt-in ``--approx-recips`` model of ``FN = recips FX``.

    PRM p.19-16/19-17 (out/refs/sharc-plus-prm, quoted in ``_compute``'s
    recips/rsqrts branch below): "Creates an 8-bit accurate seed for
    1/Fx... The mantissa of the seed is determined from a ROM table using
    the 7 MSBs (excluding the hidden bit) of the Fx mantissa as an index."
    That ROM table's contents are not published, so this cannot reproduce
    the real hardware seed bit for bit. It instead:

      - reproduces every documented special case exactly: NaN input ->
        all-1s result (PRM p.417-418 IEEE-754-compatibility bullet: "NAN
        inputs ... return a quiet NAN (all 1s)"); +-zero input -> +-infinity
        with the overflow flag; an Fx unbiased exponent > +125 -> +-zero;
      - flushes a denormal input to +-zero first, per the same PRM section's
        general rule ("Denormal operands ... flush to zero when input to a
        computational unit"), which recips's own page does not restate but
        which applies to every computational unit;
      - for the ordinary case, derives the seed's exponent from the
        documented rule (unbiased exponent of Fn = -e-1, e = Fx's unbiased
        exponent) and approximates its mantissa as the true mathematical
        reciprocal's mantissa, truncated to the documented 8-bit accuracy
        (the low 15 of 23 mantissa bits zeroed) so as not to claim
        precision no public source confirms.

    Every value this returns is an approximation the caller must not treat
    as ground truth; ``_apply_compute`` tags it with an "approximate-recips"
    trace event so a report can always tell it apart from a real seed.
    """
    updates: FlagUpdate = _flags_from_pairs(
        (
            (AC_BIT, False),
            (AS_BIT, False),
        )
    )
    if not isinstance(left, Const):
        updates = _flags_then(
            updates,
            _flags_from_pairs(
                (
                    (AV_BIT, None),
                    (AI_BIT, None),
                    (AN_BIT, None),
                    (AZ_BIT, None),
                )
            ),
        )
        return Unknown("recips seed (symbolic input)"), updates
    bits = left.value & 0xFFFFFFFF
    sign = (bits >> 31) & 1
    biased_exp = (bits >> 23) & 0xFF
    mantissa = bits & 0x7FFFFF
    if biased_exp == 0xFF and mantissa != 0:  # NaN
        updates = _flags_then(
            updates,
            _flags_from_pairs(
                (
                    (AI_BIT, True),
                    (AN_BIT, bool(sign)),
                    (AV_BIT, False),
                    (AZ_BIT, False),
                )
            ),
        )
        return Const(0xFFFFFFFF), updates
    updates = _flags_put(updates, AI_BIT, False)
    updates = _flags_put(updates, AN_BIT, bool(sign))
    if biased_exp == 0:  # +-zero, or a denormal flushed to zero on input
        updates = _flags_put(updates, AV_BIT, True)
        updates = _flags_put(updates, AZ_BIT, False)
        return Const((sign << 31) | (0xFF << 23)), updates  # +-infinity
    updates = _flags_put(updates, AV_BIT, False)
    unbiased_exp = biased_exp - 127
    if unbiased_exp > 125:
        updates = _flags_put(updates, AZ_BIT, True)
        return Const(sign << 31), updates  # +-zero
    updates = _flags_put(updates, AZ_BIT, False)
    x = _f32_from_bits(bits)
    seed_bits, _overflowed = _float32_bits(1.0 / x)
    # "8-bit accurate seed": keep sign, exponent and the top 8 mantissa
    # bits; zero the low 15 mantissa bits this model cannot claim.
    seed_bits &= 0xFFFF8000
    return Const(seed_bits), updates
