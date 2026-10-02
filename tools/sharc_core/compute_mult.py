"""Multiplier compute (cu=1) and MR data moves: handler bodies dispatched
by tools/sharc_core/compute.py's MULT_OPS table (plus MR_DATAMOVE_REGISTERS/
_mr_data_move for the separate MRDATAMOVE encoding, and the two
multiply-accumulate helpers compute.py calls directly ahead of its
mf/cu dispatch -- see compute.py's docstring for why).

The multiplier's 80-bit result accumulators (MRF/MRB, or MSF/MSB for PEy --
SHARC+ PRM p.3-10) are modelled as ``state.MR`` values (see state.py),
carried through ``special["MRF"]``/``special["MRB"]`` the same way this
module always has. Every fixed-point row of PRM Table 17-7 ("MULOP Encode
Table", out/refs/sharc-plus-prm/all.txt:22612-22660) is generated
programmatically below (``_build_mult_ops``) from the same MOD1/MOD2/MOD3
bit layout the manual documents (all.txt:22660-22734), rather than
hand-written per opcode: each opcode's fixed/variable bits are derived once
in one of the five handler-builder functions
(``_mult_handler``/``_mac_handler``/``_sat_handler``/``_rnd_handler``/
``_clear_handler``), and ``tests/test_sharc_compute_table.py`` cross-checks
every generated key against ``tools/sharcspec/compute_table.json``.
"""

from __future__ import annotations

from collections.abc import Callable, Mapping
from typing import NamedTuple

from .flags import MULT_FLAGS_CLEAR, MULT_FLAGS_FORGET, FlagUpdate, _mult_flags
from .floats import (
    _DOUBLE_ALL_ONES_HI,
    _DOUBLE_ALL_ONES_LO,
    _double,
    _double_binary,
    _double_pair_bits,
    _f_mul,
    _float32,
    _float_binary,
    _isfinite,
    _isinf,
    _isnan,
)
from .state import (
    MR,
    MR_ZERO,
    NO_SPECIAL,
    _mr_from_signed,
    _mr_read_word,
    _mr_write_word,
    _ureg,
)
from .values import (
    ComputeDest,
    ComputeValue,
    Const,
    Operand,
    Unknown,
    Value,
    _multiply,
    _signed32,
)

# A multiplier handler's result may carry a full 80-bit MR (state.py, a
# layer above values.py's Operand/ComputeValue by design -- values.py
# cannot import MR without an import cycle), so this module uses this
# locally widened result type instead of values.py's ComputeResult.
MultResult = tuple[ComputeDest, "ComputeValue | MR", str, FlagUpdate]

Handler = Callable[
    [
        int,
        int,
        int,
        Operand,
        Operand,
        Mapping[int, Value],
        Mapping[str, Operand | MR] | None,
        bool,
    ],
    MultResult,
]

# 80-bit two's-complement field mask (state.py's MR uses the same one, but
# does not export it; redefined here from the same PRM p.3-10 fact -- MR2F
# is bits 79:64 -- rather than reaching into state.py's private name).
_MR_MASK = (1 << 80) - 1


# PRM Table 18-29 MRDATAMOVE (p.438), cross-checked against
# tools/sharcspec/compute_table.json's mrdatamove table (PGR Table 12-10,
# pgr.txt:22827-22841): opcode[15:12] selects which of the six banked
# multiplier-result registers a data move addresses.
MR_DATAMOVE_REGISTERS = {
    0x0: "MR0F",
    0x1: "MR1F",
    0x2: "MR2F",
    0x4: "MR0B",
    0x5: "MR1B",
    0x6: "MR2B",
}
# mr_name -> (accumulator key in state.special, word index 0/1/2 within it).
_MR_DATAMOVE_WORD = {
    "MR0F": ("MRF", 0),
    "MR1F": ("MRF", 1),
    "MR2F": ("MRF", 2),
    "MR0B": ("MRB", 0),
    "MR1B": ("MRB", 1),
    "MR2B": ("MRB", 2),
}


def _mr_data_move(
    mr_name: str,
    rn: int,
    direction: int,
    values: Mapping[int, Value],
    special: Mapping[str, Operand | MR] | None,
) -> MultResult:
    """PRM Table 18-29 MRDATAMOVE (p.438): moves a 32-bit value between the
    register file and one 32/32/16-bit word (MR0/MR1/MR2) of one of the two
    80-bit accumulators (PRM p.3-10/4-77: "Each multiplier has a primary or
    foreground register ... and alternate or background" -- MRF/MRB).
    ``state._mr_read_word``/``_mr_write_word`` hold the actual word-slicing
    and MR1->MR2 sign-extension-on-write rule; this just picks which
    accumulator and word. Flags: PRM p.493, MU/MN/MI/MV all cleared for
    every direction and register."""
    reg_key, word = _MR_DATAMOVE_WORD[mr_name]
    op_name = "mr-data-move-%s" % mr_name.lower()
    specials = special if special is not None else NO_SPECIAL
    current = specials.get(reg_key, Unknown("uninitialized %s" % reg_key))
    if direction:  # register file -> MR word
        new_mr = _mr_write_word(current, word, _ureg(values, rn))
        return reg_key, new_mr, op_name, MULT_FLAGS_CLEAR
    value = _mr_read_word(current, word)
    return rn, value, op_name, MULT_FLAGS_CLEAR


def _mr_flags(
    raw80: int, fractional: bool, signed_result: bool
) -> tuple[bool, bool, bool]:
    """MN/MV/MU (SHARC+ PRM p.28-5/28-6, REGF_ASTATX.MN/MV/MU field
    descriptions) for a concrete 80-bit multiplier result RAW80 (the
    unsigned bit pattern), evaluated against the FORMAT (fractional or
    integer, signed or unsigned) the instruction itself declares via its
    MOD1/MOD2/MOD3 bits -- which need not be the format that originally
    produced RAW80 (e.g. ``sat mrf (UI)`` reads back whatever mrf holds as
    an unsigned integer, regardless of how it got there).

    MN is the sign of the full 80-bit field (bit 79), for every format.
    MV/MU each check how many of the upper bits are pure sign/zero padding:
    MV when the value does not fit the format's usable width ("Two's-
    complement fractional with the upper 17 bits of MR not all zeros or all
    ones", etc.), MU (fractional only -- the PRM's MU bullets list only the
    two fractional cases, never an integer one, so integer results never
    underflow) when the value is nonzero but entirely within the sub-32-bit
    region MR0 alone provides.
    """
    mn = bool((raw80 >> 79) & 1)
    if fractional:
        upper48 = (raw80 >> 32) & 0xFFFFFFFFFFFF
        lower32 = raw80 & 0xFFFFFFFF
        if signed_result:
            upper17 = (raw80 >> 63) & 0x1FFFF
            mv = upper17 not in (0, 0x1FFFF)
            mu_upper_ok = upper48 in (0, 0xFFFFFFFFFFFF)
        else:
            upper16 = (raw80 >> 64) & 0xFFFF
            mv = upper16 != 0
            mu_upper_ok = upper48 == 0
        mu = mu_upper_ok and lower32 != 0
    else:
        upper49 = (raw80 >> 31) & 0x1FFFFFFFFFFFF
        if signed_result:
            mv = upper49 not in (0, (1 << 49) - 1)
        else:
            upper48 = (raw80 >> 32) & 0xFFFFFFFFFFFF
            mv = upper48 != 0
        mu = False
    return mn, mv, mu


def _astatx_mult_from(
    raw80: int | None, fractional: bool, signed_result: bool, *, sat: bool = False
) -> FlagUpdate:
    """ASTATX/ASTATY update for a fixed-point multiplier result: MN/MV/MU
    from RAW80 via ``_mr_flags``, MI fixed 0 (PRM Table 3-7: every
    fixed-point row lists MI as a flat "0" -- MI only ever applies to the
    floating-point multiply row, see ``_astatx_mult_float``). SAT=True
    additionally fixes MV to 0 (PRM Table 3-7's sat row alone: "MU * MN *
    MV 0 MI 0" -- unlike every other fixed-point row, whose MV column is
    data-dependent). RAW80=None (an unknown operand) forgets MN/MV/MU
    instead of guessing.
    """
    if raw80 is None:
        mn = mu = None
        mv = False if sat else None
    else:
        mn, mv, mu = _mr_flags(raw80, fractional, signed_result)
        if sat:
            mv = False
    return _mult_flags(mn, mv, mu, False)


def _astatx_mult_float(
    result: Value, overflowed: bool | None, invalid: bool | None
) -> FlagUpdate:
    """PRM Table 3-9 "FN = FX * FY": MU/MN/MV/MI all data-dependent (unlike
    every fixed-point row). MN is RESULT's own sign bit; MV is OVERFLOWED
    (post-rounded exponent > 127, exactly what ``_float_binary`` already
    reports via struct's OverflowError path); MU is a denormal RESULT
    (biased exponent 0, nonzero mantissa -- "the floating-point result
    underflows (unbiased exponent < -126)", and a denormal's unbiased
    exponent is exactly -127); MI is INVALID as ``_float_binary`` already
    computes it (NaN input)."""
    mn = mv = mu = None
    if isinstance(result, Const):
        bits = result.value
        mn = bool(bits & 0x80000000)
        mv = bool(overflowed)
        mu = (bits & 0x7F800000) == 0 and (bits & 0x007FFFFF) != 0
    return _mult_flags(mn, mv, mu, invalid)


def _astatx_mult_double(
    hi_result: Value, lo_result: Value, overflowed: bool | None, invalid: bool | None
) -> FlagUpdate:
    """SC58x/2158x PRM ch.23 "64-bit Floating-Point Operations" (p.23-2..
    23-4), each op's identical "ASTATx/y Flags" table: MN/MV/MU/MI, the
    same shape as ``_astatx_mult_float`` above but reading the register
    pair's bits (HI bit31=sign, HI bits30-20=the 11-bit biased exponent,
    HI bits19-0 + LO = the 52-bit mantissa) instead of a 32-bit float's.
    MU ("unbiased exponent < -1022") is a true denormal HI:LO result
    (biased exponent field 0, mantissa nonzero -- excluding exact zero,
    matching ``_astatx_mult_float``'s own ``!= 0`` mantissa check); this
    project's 64-bit ops do not flush denormal results to zero
    (``floats.py``'s 64-bit header comment), so a genuine denormal double
    is reported here exactly as computed. The manual's own NAN-sign
    refinement ("if only one operand is a NAN, the sign of the result is
    the sign of that NAN operand") is not modeled: like every other NAN
    case in this project, ``_double_binary`` returns a fixed all-1s (thus
    always-negative) sentinel regardless of which operand was NAN.
    """
    mn = mv = mu = None
    if isinstance(hi_result, Const) and isinstance(lo_result, Const):
        hi_bits, lo_bits = hi_result.value, lo_result.value
        mn = bool(hi_bits & 0x80000000)
        mv = bool(overflowed)
        biased_exp = (hi_bits >> 20) & 0x7FF
        mantissa_nonzero = (hi_bits & 0xFFFFF) != 0 or lo_bits != 0
        mu = biased_exp == 0 and mantissa_nonzero
    return _mult_flags(mn, mv, mu, invalid)


def _mr_product_raw(
    x: int, y: int, signed_x: bool, signed_y: bool, fractional: bool
) -> int:
    """The 80-bit two's-complement bit pattern (mod 2**80) a fixed-point
    multiply places in the accumulator (PRM p.3-9/3-10, Figure 3-2): the
    exact product of the two operands (each interpreted per SIGNED_X/
    SIGNED_Y), doubled once when both are signed and the format is
    fractional ("If both inputs are fractional and signed, the multiplier
    automatically shifts the result left one bit to remove the redundant
    sign bit"). That single left shift is *all* that distinguishes the
    fractional and integer placements: an integer product's meaningful
    32-bit truncation is bits 31:0 of the result (ordinary 32x32->32
    wraparound multiply), a fractional product's is bits 63:32 (Figure
    3-2's "FRACTIONAL RESULT" region) -- both simply fall out of masking
    the same raw product into the 80-bit field, no extra positional shift
    needed (cross-checked against ``_multiply_fractional``'s existing
    >>31/>>32 register-direct extraction, which ``_mr_extract32`` below
    reduces to when there is no accumulate step)."""
    xv = _signed32(x) if signed_x else (x & 0xFFFFFFFF)
    yv = _signed32(y) if signed_y else (y & 0xFFFFFFFF)
    product = xv * yv
    if fractional and signed_x and signed_y:
        product <<= 1
    return product & _MR_MASK


def _mr_extract32(raw80: int, fractional: bool) -> int:
    """The 32-bit value sent to a register-file location (PRM p.3-10:
    "using bits 6332 for a fractional result or bits 310 for an integer
    result")."""
    return (raw80 >> 32) & 0xFFFFFFFF if fractional else raw80 & 0xFFFFFFFF


def _mr_round(raw80: int) -> int:
    """Round-to-nearest of the 80-bit value at the MR1F/MR0F (bit 32)
    boundary (PRM p.3-11 "Round MRx Instruction": "performs a round to
    nearest of the 80-bit MRF value at bit 32"): add a half-ULP at bit 31
    in the full 80-bit two's-complement field (so it correctly carries into
    MR2F on overflow), then zero the low 32 bits (discarding MR0F -- "the
    rounded result in MR1F can be sent to the register file", matching how
    the same section describes *not* rounding as "discarding bits 310 [...]
    truncates a fractional result")."""
    rounded = (raw80 + (1 << 31)) & _MR_MASK
    return rounded & (_MR_MASK ^ 0xFFFFFFFF)


# (fractional, signed) -> (max, min) representable value (PRM Table 3-5,
# p.3-11 "Fixed-Point Format Maximum Values (Saturation)"), as plain Python
# ints -- masking either bound into 80 bits (``& _MR_MASK``) reproduces the
# table's hex rows exactly, since two's-complement sign-extension of a
# negative Python int under ``&`` already fills MR2F correctly.
_SAT_BOUNDS: dict[tuple[bool, bool], tuple[int, int]] = {
    (True, True): ((1 << 63) - 1, -(1 << 63)),  # fractional, signed
    (True, False): ((1 << 64) - 1, 0),  # fractional, unsigned
    (False, True): ((1 << 31) - 1, -(1 << 31)),  # integer, signed
    (False, False): ((1 << 32) - 1, 0),  # integer, unsigned
}


def _mac_accumulator(
    src_key: str, special: Mapping[str, Operand | MR] | None
) -> tuple[Operand | MR, int | None]:
    specials = special if special is not None else NO_SPECIAL
    accumulator = specials.get(src_key, Unknown("uninitialized %s" % src_key))
    acc_signed = accumulator.signed() if isinstance(accumulator, MR) else None
    return accumulator, acc_signed


# Fixed-point multiplier operation kinds (PRM Table 17-7 row families).
_MULT, _MAC, _SAT, _RND, _CLEAR = 0, 1, 2, 3, 4
# MOD1/MOD2/MOD3 destination field -> accumulator name (RN has none).
_DEST_KEYS: dict[str, str] = {"mrf": "MRF", "mrb": "MRB"}


class MultSpec(NamedTuple):
    """One fixed-point multiplier opcode of PRM Table 17-7, as data: the
    row family (KIND) and the MOD1/MOD2/MOD3 options its bits select.
    ``mult_fixed`` executes it."""

    kind: int
    signed_x: bool
    signed_y: bool
    fractional: bool
    round_: bool
    # Signedness of the result: signed_x or signed_y for a multiply or
    # accumulate, the MOD2/MOD3 sign for a saturate or round.
    signed: bool
    subtract: bool
    dest_key: str | None  # "MRF"/"MRB", or None for an RN destination
    src_key: str  # accumulator read by accumulate/saturate/round
    op_name: str
    sat_max: int
    sat_min: int


def _mult_handler(
    signed_x: bool, signed_y: bool, fractional: bool, round_: bool, dest: str
) -> MultSpec:
    """PRM Table 17-7 "01yx f00r"/"F10r"/"F11r" rows (p.17-7): (RN|mrf|mrb)
    = RX * RY MOD1 -- a plain multiply, no accumulate. DEST is "rn", "mrf"
    or "mrb"."""
    dest_key = _DEST_KEYS.get(dest)
    op_name = (
        "multiply"
        if dest_key is None
        else "multiply-mrf"
        if dest_key == "MRF"
        else "multiply-mrb"
    )
    return MultSpec(
        _MULT,
        signed_x,
        signed_y,
        fractional,
        round_,
        signed_x or signed_y,
        False,
        dest_key,
        "",
        op_name,
        0,
        0,
    )


def _mult_body(
    spec: MultSpec, rn: int, rx: int, ry: int, left: Operand, right: Operand
) -> MultResult:
    if isinstance(left, Const) and isinstance(right, Const):
        raw = _mr_product_raw(
            left.value, right.value, spec.signed_x, spec.signed_y, spec.fractional
        )
        if spec.round_:
            raw = _mr_round(raw)
        astatx = _astatx_mult_from(raw, spec.fractional, spec.signed)
        if spec.dest_key is None:
            return rn, Const(_mr_extract32(raw, spec.fractional)), spec.op_name, astatx
        return spec.dest_key, _mr_from_signed(raw), spec.op_name, astatx
    astatx = _astatx_mult_from(None, spec.fractional, spec.signed)
    if spec.dest_key is not None:
        return (
            spec.dest_key,
            Unknown("R%d * R%d MOD1" % (rx, ry)),
            spec.op_name,
            astatx,
        )
    # RN destination, no accumulate: preserve the tracer's existing
    # symbolic-affine multiply-by-constant capability for the plain
    # *integer*, unrounded case (PRM p.3-9: the low 32 bits of an
    # integer product do not depend on rounding or the redundant-sign
    # shift, so this is exactly ``_multiply``'s domain -- the same
    # helper the pre-80-bit-MR version of this handler called
    # directly). A fractional or rounded RN result still has no
    # symbolic model, matching ``_multiply_fractional``'s own Unknown
    # fallback for anything but two Consts.
    value = (
        Unknown("R%d * R%d MOD1" % (rx, ry))
        if spec.fractional or spec.round_
        else _multiply(left, right, "R%d * R%d" % (rx, ry))
    )
    return rn, value, spec.op_name, astatx


def _mac_handler(
    signed_x: bool,
    signed_y: bool,
    fractional: bool,
    round_: bool,
    dest: str,
    subtract: bool,
    src: str,
) -> MultSpec:
    """PRM Table 17-7 "10yx.../11yx..." rows (p.17-7): (RN|mrf|mrb) =
    (mrf|mrb) [+-] RX * RY MOD1. DEST is "rn", "mrf" or "mrb"; SRC ("mrf" or
    "mrb") is the accumulator read (and, unless DEST=="rn", also written)."""
    return MultSpec(
        _MAC,
        signed_x,
        signed_y,
        fractional,
        round_,
        signed_x or signed_y,
        subtract,
        _DEST_KEYS.get(dest),
        "MRF" if src == "mrf" else "MRB",
        "multiply-subtract" if subtract else "multiply-accumulate",
        0,
        0,
    )


def _mac_body(
    spec: MultSpec,
    rn: int,
    rx: int,
    ry: int,
    left: Operand,
    right: Operand,
    special: Mapping[str, Operand | MR] | None,
) -> MultResult:
    accumulator, acc_signed = _mac_accumulator(spec.src_key, special)
    # An integer RN destination takes only bits 31:0 (PRM Table 3-7).
    # Unknown guard/upper words cannot affect modular addition in that
    # slice. Keep the full-result flags unknown rather than inventing the
    # unwritten accumulator words (MR0 writes do not sign-extend).
    if (
        acc_signed is None
        and not spec.fractional
        and not spec.round_
        and isinstance(left, Const)
        and isinstance(right, Const)
    ):
        low = _mr_read_word(accumulator, 0)
        if isinstance(low, Const):
            product = _mr_product_raw(
                left.value, right.value, spec.signed_x, spec.signed_y, False
            )
            raw = low.value - product if spec.subtract else low.value + product
            flags = _astatx_mult_from(None, False, spec.signed)
            result = Const(raw & 0xFFFFFFFF)
            if spec.dest_key is None:
                return rn, result, spec.op_name, flags
            # Carry out of MR0 may change the unknown upper words. Keep
            # only this independently known slice for later MR/RN reads.
            partial = _mr_write_word(Unknown("unknown upper MAC result"), 0, result)
            return spec.dest_key, partial, spec.op_name, flags
    if acc_signed is None or not (isinstance(left, Const) and isinstance(right, Const)):
        astatx = _astatx_mult_from(None, spec.fractional, spec.signed)
        label = "%s %s R%d * R%d MOD1" % (
            spec.src_key,
            "-" if spec.subtract else "+",
            rx,
            ry,
        )
        return (spec.dest_key or rn, Unknown(label), spec.op_name, astatx)
    product = _mr_product_raw(
        left.value, right.value, spec.signed_x, spec.signed_y, spec.fractional
    )
    product_signed = product - (1 << 80) if product & (1 << 79) else product
    raw = (
        (acc_signed - product_signed)
        if spec.subtract
        else (acc_signed + product_signed)
    ) & _MR_MASK
    if spec.round_:
        raw = _mr_round(raw)
    astatx = _astatx_mult_from(raw, spec.fractional, spec.signed)
    if spec.dest_key is None:
        return rn, Const(_mr_extract32(raw, spec.fractional)), spec.op_name, astatx
    return spec.dest_key, _mr_from_signed(raw), spec.op_name, astatx


def _sat_handler(fractional: bool, signed_: bool, dest: str, src: str) -> MultSpec:
    """PRM Table 17-7 "0000 F--x" rows (p.17-7): (RN|mrf|mrb) = sat
    (mrf|mrb) MOD2 -- clamp the accumulator to the MOD2-declared format's
    representable range (Table 3-5). MV is architecturally fixed 0 for this
    row (saturation removes overflow by definition); MN/MU still reflect
    the pre-saturation accumulator (a saturated result can never itself
    underflow or be negative-but-clamped-positive, so evaluating them
    post-clamp would just report the same thing the clamp already forces)."""
    src_key = "MRF" if src == "mrf" else "MRB"
    max_value, min_value = _SAT_BOUNDS[(fractional, signed_)]
    return MultSpec(
        _SAT,
        False,
        False,
        fractional,
        False,
        signed_,
        False,
        _DEST_KEYS.get(dest),
        src_key,
        "saturate-mrf" if src_key == "MRF" else "saturate-mrb",
        max_value,
        min_value,
    )


def _sat_body(
    spec: MultSpec, rn: int, special: Mapping[str, Operand | MR] | None
) -> MultResult:
    _accumulator, acc_signed = _mac_accumulator(spec.src_key, special)
    if acc_signed is None:
        astatx = _astatx_mult_from(None, spec.fractional, spec.signed, sat=True)
        return (
            spec.dest_key or rn,
            Unknown("saturated %s (uninitialized)" % spec.src_key),
            spec.op_name,
            astatx,
        )
    clamped = max(spec.sat_min, min(spec.sat_max, acc_signed))
    astatx = _astatx_mult_from(
        acc_signed & _MR_MASK, spec.fractional, spec.signed, sat=True
    )
    raw = clamped & _MR_MASK
    if spec.dest_key is None:
        return rn, Const(_mr_extract32(raw, spec.fractional)), spec.op_name, astatx
    return spec.dest_key, _mr_from_signed(raw), spec.op_name, astatx


def _rnd_handler(signed_: bool, dest: str, src: str) -> MultSpec:
    """PRM Table 17-7 "0001 1..." rows (p.17-7): (RN|mrf|mrb) = rnd
    (mrf|mrb) MOD3. Always fractional -- MOD3 only has SF/UF options ("The
    RND operation ... applies only to fractional results", PRM p.3-11)."""
    src_key = "MRF" if src == "mrf" else "MRB"
    return MultSpec(
        _RND,
        False,
        False,
        True,
        True,
        signed_,
        False,
        _DEST_KEYS.get(dest),
        src_key,
        "round-mrf" if src_key == "MRF" else "round-mrb",
        0,
        0,
    )


def _rnd_body(
    spec: MultSpec, rn: int, special: Mapping[str, Operand | MR] | None
) -> MultResult:
    _accumulator, acc_signed = _mac_accumulator(spec.src_key, special)
    if acc_signed is None:
        astatx = _astatx_mult_from(None, True, spec.signed)
        return (
            spec.dest_key or rn,
            Unknown("rounded %s (uninitialized)" % spec.src_key),
            spec.op_name,
            astatx,
        )
    raw = _mr_round(acc_signed & _MR_MASK)
    astatx = _astatx_mult_from(raw, True, spec.signed)
    if spec.dest_key is None:
        return rn, Const(_mr_extract32(raw, True)), spec.op_name, astatx
    return spec.dest_key, _mr_from_signed(raw), spec.op_name, astatx


def _clear_handler(dest: str) -> MultSpec:
    """PRM Table 17-7 "0001 01d0" rows (p.17-7): (mrf|mrb) = 0. PRM Table
    3-7: all four flags fixed 0, the same pattern ``MULT_FLAGS_CLEAR``
    already gives the MR-data-move rows."""
    dest_key = "MRF" if dest == "mrf" else "MRB"
    return MultSpec(
        _CLEAR,
        False,
        False,
        False,
        False,
        False,
        False,
        dest_key,
        "",
        "clear-mrf" if dest_key == "MRF" else "clear-mrb",
        0,
        0,
    )


def mult_fixed(
    spec: MultSpec,
    rn: int,
    rx: int,
    ry: int,
    left: Operand,
    right: Operand,
    special: Mapping[str, Operand | MR] | None,
) -> MultResult:
    """Execute one fixed-point multiplier opcode (see MultSpec)."""
    if spec.kind == _MULT:
        return _mult_body(spec, rn, rx, ry, left, right)
    if spec.kind == _MAC:
        return _mac_body(spec, rn, rx, ry, left, right, special)
    if spec.kind == _SAT:
        return _sat_body(spec, rn, special)
    if spec.kind == _RND:
        return _rnd_body(spec, rn, special)
    assert spec.dest_key is not None
    return spec.dest_key, MR_ZERO, spec.op_name, MULT_FLAGS_CLEAR


# PRM Table 18-7 (p.428-429): MULOP 00110000 is Fn = Fx * Fy. Flags are the
# multiplier's MN/MV/MU/MI (PRM Table 3-9), computed by _astatx_mult_float
# above rather than forgotten -- unlike the fixed-point rows, every one of
# these four is genuinely data-dependent for a float multiply.
def mult_float(rn, rx, ry, left, right, values, special, approx_recips) -> tuple:
    result, overflowed, invalid = _float_binary(
        left, right, "F%d * F%d" % (rx, ry), _f_mul
    )
    astatx = _astatx_mult_float(result, overflowed, invalid)
    return rn, result, "float-multiply", astatx


# ---------------------------------------------------------------------------
# 64-bit (IEEE double) multiplier ops (ADSP-SC58x/2158x PRM ch.23, Table
# 18-8, opcodes 0x31-0x33 -- cu=1, right after 0x30's plain 32-bit float
# multiply, the classic PRM/PGR's MULOP table has no entries above 0x30).
# The manual documents a WARNING for all three ("the data in MR register
# at the end of execution ... is not valid") -- this tracer has no pipeline
# model where that would show up (MRF/MRB are only ever touched here via
# the ordinary accumulator special-dict slots the fixed-point MAC rows use,
# and none of these three ops reads or writes one), so it is not modeled.
# None of the three appears in dt2-1.16, dt2-1.15C, dn2-1.11 or dn2-1.10E's
# aligned, in-function code (lane F1 census); implemented ahead of any
# observed use, like ``mult_undocumented_10`` below is decoded ahead of a
# public source.
# ---------------------------------------------------------------------------
def mult_double_multiply(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    """FM:N = FX:Y * FZ:W (p.23-2): both operands are register pairs."""
    hi, lo, overflowed, invalid = _double_binary(
        _ureg(values, rx + 1),
        _ureg(values, rx),
        _ureg(values, ry + 1),
        _ureg(values, ry),
        "F%d:%d * F%d:%d" % (rx + 1, rx, ry + 1, ry),
        _f_mul,
    )
    astatx = _astatx_mult_double(hi, lo, overflowed, invalid)
    return (rn + 1, rn), (hi, lo), "double-multiply", astatx


def mult_double_multiply_single(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    """FM:N = FX:Y * FY (p.23-3): FX:Y a register pair, FY (RIGHT) a plain
    single-precision register."""
    label = "F%d:%d * F%d" % (rx + 1, rx, ry)
    a = _double(_ureg(values, rx + 1), _ureg(values, rx))
    b = _float32(right)
    if a is None or b is None:
        astatx = _astatx_mult_double(Unknown(label), Unknown(label), None, None)
        return (
            (rn + 1, rn),
            (Unknown(label), Unknown(label)),
            "double-multiply-single",
            astatx,
        )
    if _isnan(a) or _isnan(b):
        hi, lo = _DOUBLE_ALL_ONES_HI, _DOUBLE_ALL_ONES_LO
        astatx = _astatx_mult_double(hi, lo, False, True)
        return (rn + 1, rn), (hi, lo), "double-multiply-single", astatx
    raw = a * b
    hi_int, lo_int = _double_pair_bits(raw)
    hi, lo = Const(hi_int), Const(lo_int)
    overflowed = _isinf(raw) and _isfinite(a) and _isfinite(b)
    astatx = _astatx_mult_double(hi, lo, overflowed, False)
    return (rn + 1, rn), (hi, lo), "double-multiply-single", astatx


def mult_float_widening_multiply(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    """FM:N = FX * FY (p.23-4): both operands plain single-precision
    registers (LEFT/RIGHT, already resolved by ``_compute`` against the
    plain rx/ry codes); the double-precision result is exact (a
    float32*float32 product always fits a double's 52-bit mantissa
    exactly), so overflow is architecturally impossible here."""
    label = "F%d * F%d" % (rx, ry)
    a, b = _float32(left), _float32(right)
    if a is None or b is None:
        astatx = _astatx_mult_double(Unknown(label), Unknown(label), None, None)
        return (
            (rn + 1, rn),
            (Unknown(label), Unknown(label)),
            "float-widening-multiply",
            astatx,
        )
    if _isnan(a) or _isnan(b):
        hi, lo = _DOUBLE_ALL_ONES_HI, _DOUBLE_ALL_ONES_LO
        astatx = _astatx_mult_double(hi, lo, False, True)
        return (rn + 1, rn), (hi, lo), "float-widening-multiply", astatx
    hi_int, lo_int = _double_pair_bits(a * b)
    hi, lo = Const(hi_int), Const(lo_int)
    astatx = _astatx_mult_double(hi, lo, False, False)
    return (rn + 1, rn), (hi, lo), "float-widening-multiply", astatx


# Undocumented in both public sources: PRM Table 17-7 and PGR Table 12-5
# both list only mrf/mrb=0 (0001 0100/0110) and rnd MOD3 (0001 100x-111x)
# under the "0001 xxxx" opcode prefix -- neither has a 0001 0000 row.
# Decode it (so the walk does not desync) and leave the result and flags
# Unknown, same as the shifter's undocumented 0xB0 gap in compute_shift.py.
def mult_undocumented_10(
    rn, rx, ry, left, right, values, special, approx_recips
) -> tuple:
    label = "multiply opcode 0x10 R%d, R%d (undocumented; no public source)" % (rx, ry)
    return rn, Unknown(label), "multiply-undocumented-10", MULT_FLAGS_FORGET


def _build_mult_ops() -> tuple[dict[int, Handler], dict[int, MultSpec]]:
    """PRM Table 17-7's full fixed-point MULOP encode space (p.17-7/17-8),
    generated from the same MOD1/MOD2/MOD3 bit layout the manual documents
    (p.17-9: y=bit5 Y-signed, x=bit4 X-signed, f=bit3 fractional, r=bit0
    round) rather than one handler per opcode by hand. 0xB4/0xB0 (the two
    multiply-accumulate-into-MRF rows compute.py's dispatch matches on the
    raw field *before* the mf bit, ahead of ever consulting this table --
    see compute.py's module docstring) are deliberately excluded here, even
    though their bit patterns fall out of the same "10yx F10r"/"F00r" loops
    below; ``multiply_accumulate_mrf``/``multiply_add_mrf`` are built the
    same way, just not inserted into MULT_OPS.
    """
    ops: dict[int, Handler] = {}
    specs: dict[int, MultSpec] = {}

    # "0000 Fbdx": (RN|mrf|mrb) = sat (mrf|mrb) MOD2. bit3=F (MOD2
    # fractional/integer), bit2=b (0=mrf source,1=mrb), bit1=d (0=dest RN,
    # 1=dest is the same register as the source), bit0=s (MOD2 signed).
    for frac_bit, fractional in ((0, False), (1, True)):
        for b_bit, src in ((0, "mrf"), (1, "mrb")):
            for d_bit, dest in ((0, "rn"), (1, src)):
                for s_bit, signed_ in ((0, False), (1, True)):
                    opcode = (frac_bit << 3) | (b_bit << 2) | (d_bit << 1) | s_bit
                    specs[opcode] = _sat_handler(fractional, signed_, dest, src)

    # "0001 01d0": (mrf|mrb) = 0.
    for d_bit, dest in ((0, "mrf"), (1, "mrb")):
        opcode = 0b00010100 | (d_bit << 1)
        specs[opcode] = _clear_handler(dest)

    # "0001 1bdx": (RN|mrf|mrb) = rnd (mrf|mrb) MOD3. bit2=b (source),
    # bit1=d (dest), bit0=MOD3 sign.
    for b_bit, src in ((0, "mrf"), (1, "mrb")):
        for d_bit, dest in ((0, "rn"), (1, src)):
            for s_bit, signed_ in ((0, False), (1, True)):
                opcode = 0b00011000 | (b_bit << 2) | (d_bit << 1) | s_bit
                specs[opcode] = _rnd_handler(signed_, dest, src)

    # "01yx f00r"/"F10r"/"F11r": (RN|mrf|mrb) = RX*RY MOD1 (no accumulate).
    # bits2:1 select the destination (00=RN, 10=mrf, 11=mrb).
    for y_bit, signed_y in ((0, False), (1, True)):
        for x_bit, signed_x in ((0, False), (1, True)):
            for f_bit, fractional in ((0, False), (1, True)):
                for r_bit, round_ in ((0, False), (1, True)):
                    if r_bit and not f_bit:
                        continue  # MOD1 has no integer-round ("...IR") option
                    for dest_bits, dest in ((0b00, "rn"), (0b10, "mrf"), (0b11, "mrb")):
                        opcode = (
                            0b01000000
                            | (y_bit << 5)
                            | (x_bit << 4)
                            | (f_bit << 3)
                            | (dest_bits << 1)
                            | r_bit
                        )
                        specs[opcode] = _mult_handler(
                            signed_x, signed_y, fractional, round_, dest
                        )

    # "10yx.../11yx...": (RN|mrf|mrb) = (mrf|mrb) [+-] RX*RY MOD1. bit7:6
    # selects add (10, 0x80) / subtract (11, 0xC0); bits2:1 select
    # dest/source together, since a RN destination still has to say which
    # accumulator was read (00=RN from mrf, 01=RN from mrb, 10=mrf, 11=mrb).
    for subtract, hi_bits in ((False, 0b10), (True, 0b11)):
        for y_bit, signed_y in ((0, False), (1, True)):
            for x_bit, signed_x in ((0, False), (1, True)):
                for f_bit, fractional in ((0, False), (1, True)):
                    for r_bit, round_ in ((0, False), (1, True)):
                        if r_bit and not f_bit:
                            continue
                        for dest_bits, dest, src in (
                            (0b00, "rn", "mrf"),
                            (0b01, "rn", "mrb"),
                            (0b10, "mrf", "mrf"),
                            (0b11, "mrb", "mrb"),
                        ):
                            opcode = (
                                (hi_bits << 6)
                                | (y_bit << 5)
                                | (x_bit << 4)
                                | (f_bit << 3)
                                | (dest_bits << 1)
                                | r_bit
                            )
                            if opcode in (0xB0, 0xB4):
                                continue  # compute.py intercepts these first
                            specs[opcode] = _mac_handler(
                                signed_x,
                                signed_y,
                                fractional,
                                round_,
                                dest,
                                subtract,
                                src,
                            )

    ops[0x30] = mult_float
    ops[0x10] = mult_undocumented_10
    ops[0x31] = mult_double_multiply
    ops[0x32] = mult_double_multiply_single
    ops[0x33] = mult_float_widening_multiply
    return ops, specs


# MULT_OPS holds the handler functions; MULT_FIXED_OPS the fixed-point
# opcodes as MultSpec data, executed by mult_fixed (compute.py consults it
# first). Together they are the whole cu=1 opcode space.
MULT_OPS, MULT_FIXED_OPS = _build_mult_ops()

# The two multiply-accumulate-into-MRF rows compute.py's dispatch matches
# ahead of the mf/cu table (see its module docstring): opcode 0xB4 ("mrf =
# mrf + RX*RY MOD1 SSI") and 0xB0 ("RN = mrf + RX*RY MOD1 SSI"), built from
# the exact same _mac_handler this module uses for every other MOD1
# combination -- both a real 80-bit accumulate now, not the previous
# 32-bit-truncating placeholder.
MULTIPLY_ACCUMULATE_MRF = _mac_handler(True, True, False, False, "mrf", False, "mrf")
MULTIPLY_ADD_MRF = _mac_handler(True, True, False, False, "rn", False, "mrf")
