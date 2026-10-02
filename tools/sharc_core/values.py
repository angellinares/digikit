"""Concrete and symbolic values and the integer arithmetic over them.

This module is the value lattice: the boundary between concrete and
symbolic execution (tools/sharc_core/SUBSET.md, sections 1-2). Semantic
code elsewhere tests values only with isinstance(v, Const) and goes
through the functions here for everything else, including the
ASTATX/ASTATY FlagUpdate records and their application.

Moved verbatim from tools/sharc_trace.py.
"""

from __future__ import annotations

import re
from collections.abc import Callable
from dataclasses import dataclass
from typing import NamedTuple

from .addressing import byte_to_normal_word, normal_word_to_architectural_byte
from .encoding import AF_BIT


@dataclass(frozen=True)
class Const:
    value: int

    def __post_init__(self):
        object.__setattr__(self, "value", self.value & 0xFFFFFFFF)


@dataclass(frozen=True)
class Affine:
    """A canonical 32-bit affine expression, constant plus named terms."""

    constant: int
    terms: tuple[tuple[str, int], ...]

    def __post_init__(self):
        coefficients: dict[str, int] = {}
        for name, coefficient in self.terms:
            if not _SYMBOL_RE.fullmatch(name):
                raise ValueError("invalid symbol name: " + repr(name))
            coefficients[name] = (coefficients.get(name, 0) + coefficient) & 0xFFFFFFFF
        object.__setattr__(self, "constant", self.constant & 0xFFFFFFFF)
        object.__setattr__(
            self,
            "terms",
            tuple(
                sorted(
                    (name, coefficient)
                    for name, coefficient in coefficients.items()
                    if coefficient
                )
            ),
        )


@dataclass(frozen=True)
class Unknown:
    reason: str


@dataclass(frozen=True)
class PartialConst:
    """A 32-bit value known only at some bit positions.

    Used for ASTATX/ASTATY: different instruction classes each define a
    disjoint group of bits (ALU flags, shifter flags, multiplier flags, BTF,
    CACC), so full 32-bit knowledge is rare in practice, but bit-level
    knowledge is common and is all the condition predicates ever need (each
    reads at most a handful of specific bits). ``mask`` has a 1 at every
    known bit position; ``bits`` holds the known value at those positions and
    is canonicalized to 0 elsewhere so two PartialConst values with the same
    knowledge compare and hash equal regardless of what an unknown position
    happened to hold before.
    """

    mask: int
    bits: int

    def __post_init__(self):
        object.__setattr__(self, "mask", self.mask & 0xFFFFFFFF)
        object.__setattr__(self, "bits", self.bits & self.mask)


Value = Const | Affine | Unknown | PartialConst
# The generic arithmetic below (_add/_negate/_subtract/_multiply/_bitwise/
# _not/_terms) must never see a PartialConst: per state.py's _ureg/_ureg_raw
# docstrings, PartialConst is only ever stored at ASTATX/ASTATY and _ureg
# (the function every arithmetic/addressing call site uses to read a UREG)
# already converts it to Const or Unknown before it can reach here. _terms
# would raise AttributeError on a PartialConst (it has no .value/.constant),
# so Operand states that invariant in the type system instead of leaving it
# only as a comment.
Operand = Const | Affine | Unknown
# A ``_compute`` handler's result shape (compute.py's ``_apply_compute`` and
# friends): most handlers name one destination and one value, but a handful
# -- dual add/subtract, the MUL/ALU multifunction rows, ShiftImm's paired
# RN+BFFWRP bit-extract -- name two or three of each at once, sharing one
# ASTATX/ASTATY update between them. Both shapes flow through the same
# tuple position, so the position's type is the union of the two.
ComputeDest = int | str | tuple[int | str, ...]
ComputeValue = Operand | tuple[Operand, ...]


class FlagUpdate(NamedTuple):
    """An ASTATX/ASTATY update as data: what a compute does to the flags.

    Applied by ``_apply_flag_update``: DEFINE_MASK's bits become known with
    the values in DEFINE_BITS, then FORGET_MASK's bits become unknown; every
    other bit keeps whatever the old value knew. The masks are disjoint and
    DEFINE_BITS lies inside DEFINE_MASK.

    CACC is -1 except for a compare with a known result, where it is the
    new CACC bit (the compare result's bit 31). The CACC field (bits 31:24)
    is an 8-bit shift register (PRM comp/compu), so its new value needs the
    old one: when the old ASTATX is fully known the shift is exact,
    otherwise FORGET_MASK (which then holds the CACC bits) forgets it.

    Calling a FlagUpdate applies it, so ``update(astatx)`` still works for
    callers written against the earlier closure form.
    """

    define_mask: int
    define_bits: int
    forget_mask: int
    cacc: int = -1

    def __call__(self, astatx: Value) -> Value:
        return _apply_flag_update(astatx, self)


# A handler's 4th element is its ASTATX/ASTATY update.
ComputeResult = tuple[ComputeDest, ComputeValue, str, FlagUpdate]
_SYMBOL_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


def _affine(constant: int, terms: tuple[tuple[str, int], ...]) -> Const | Affine:
    """Build a canonical affine value, collapsing a constant expression."""
    value = Affine(constant, terms)
    return Const(value.constant) if not value.terms else value


def symbol(name: str) -> Affine:
    """Return the named symbolic value NAME."""
    if not _SYMBOL_RE.fullmatch(name):
        raise ValueError("invalid symbol name: " + repr(name))
    return Affine(0, ((name, 1),))


def _signed(value: int, bits: int) -> int:
    return value - (1 << bits) if value & (1 << (bits - 1)) else value


def _signed32(value: int) -> int:
    return _signed(value & 0xFFFFFFFF, 32)


def _terms(value: Const | Affine) -> tuple[int, tuple[tuple[str, int], ...]]:
    return (
        (value.value, ()) if isinstance(value, Const) else (value.constant, value.terms)
    )


# A symbol name this module recognises as denoting a value some caller has
# already bounded to a known numeric range: either an entry-time seed in
# the convention tools/sharcwriters.py's seed_sets()/ENTRY_SEED_NAMES uses
# ("I6e", "B7e", ...: one or more uppercase letters, one or more digits,
# then "e"), or one of this module's own CIRC_SYMBOL_PREFIX-tagged symbols
# (below). This module does not itself know the numeric bound -- that is
# the caller's fact to state (tools/sharcwriters.py's STACK_SYMBOLS /
# CIRC_WRAP_SLACK) -- it only recognises the *shape* of a name a caller is
# likely to have bounded, so it knows when re-deriving a fresh symbol
# through a circular MODIFY is meaningful rather than fabricating a bound
# for an arbitrary, unrelated value that merely happens to be a bare named
# term (e.g. a loop-count symbol).
_BOUNDED_SYMBOL_RE = re.compile(r"^[A-Z]+\d+e$")
CIRC_SYMBOL_PREFIX = "circ_"


def _stack_bounded_symbol(value: Value) -> tuple[str, int] | None:
    """-> (name, signed constant offset), if `value` is exactly one named
    symbol with coefficient 1 (any constant offset) whose name matches
    _BOUNDED_SYMBOL_RE or starts with CIRC_SYMBOL_PREFIX -- otherwise None.
    A second term, or a coefficient other than 1, means the value's range
    is no longer provably tied to the symbol's own bound (e.g. a scaled or
    summed expression), so the caller falls back to Unknown rather than
    guess."""
    if isinstance(value, Affine) and len(value.terms) == 1:
        name, coefficient = value.terms[0]
        if coefficient == 1 and (
            _BOUNDED_SYMBOL_RE.match(name) or name.startswith(CIRC_SYMBOL_PREFIX)
        ):
            return name, _signed(value.constant, 32)
    return None


def _add(left: Operand, right: Operand, expression: str) -> Operand:
    if isinstance(left, Unknown) or isinstance(right, Unknown):
        return Unknown(expression)
    constant, terms = _terms(left)
    other_constant, other_terms = _terms(right)
    return _affine(constant + other_constant, terms + other_terms)


def _negate(value: Operand, expression: str) -> Operand:
    if isinstance(value, Unknown):
        return Unknown(expression)
    constant, terms = _terms(value)
    return _affine(
        -constant, tuple((name, -coefficient) for name, coefficient in terms)
    )


def _subtract(
    left: Operand, right: Operand, expression: str, *, same_source: bool = False
) -> Operand:
    """LEFT - RIGHT, with an explicit fold for the self-subtract idiom.

    SAME_SOURCE=True is the caller's promise that LEFT and RIGHT are two
    reads of the exact same register/operand at this instant (e.g. the
    SHARC+ "Rn = Rn - Rn" self-clear idiom, PRM Table 17-5 / 18-10 ALUOP
    add/subtract with RX=RY): whatever that shared value is -- even an
    Unknown/symbolic one -- X - X is exactly 0 in 32-bit modular
    arithmetic, so fold to Const(0) directly rather than letting an
    Unknown operand swallow the whole expression (Unknown - Unknown would
    otherwise stay Unknown forever, e.g. a subsequent DO-loop compare
    against it never resolving concretely and forking every iteration)."""
    if same_source:
        return Const(0)
    return _add(left, _negate(right, expression), expression)


def _multiply(left: Operand, right: Operand, expression: str) -> Operand:
    if isinstance(left, Unknown) or isinstance(right, Unknown):
        return Unknown(expression)
    if isinstance(left, Const) and isinstance(right, Const):
        return Const(left.value * right.value)
    if isinstance(left, Const):
        constant, terms = _terms(right)
        return _affine(
            left.value * constant,
            tuple((name, left.value * coefficient) for name, coefficient in terms),
        )
    if isinstance(right, Const):
        constant, terms = _terms(left)
        return _affine(
            right.value * constant,
            tuple((name, right.value * coefficient) for name, coefficient in terms),
        )
    return Unknown(expression + " (non-affine multiplication)")


def _multiply_fractional(
    left: Operand, right: Operand, signed_x: bool, signed_y: bool, expression: str
) -> Operand:
    """RX * RY MOD1 in 1.31/0.32 fractional format (PRM "Fixed-Point
    Formats", p.27-3/27-4): a 32-bit fractional operand's value is its raw
    bit pattern scaled by 2**-31 (signed) or 2**-32 (unsigned), so the
    64-bit product is scaled by 2**-62/2**-63/2**-64 depending on operand
    signs. The register-file/MRF result keeps the top 32 bits of that
    product (PRM Figure 3-2, p.3-10: "bits 63-0 for a fractional result").
    When both inputs are signed, PRM p.3-9 documents an extra left shift by
    one to remove the redundant sign bit before that truncation, which
    folds into dividing by 2**31 instead of 2**32 below. Only the doubly
    Const case is evaluated; anything else (Unknown, or a still-symbolic
    Affine, which this shift does not distribute over) stays Unknown.
    """
    if not (isinstance(left, Const) and isinstance(right, Const)):
        return Unknown(expression)
    x = _signed32(left.value) if signed_x else left.value
    y = _signed32(right.value) if signed_y else right.value
    shift = 31 if (signed_x and signed_y) else 32
    return Const((x * y) >> shift)


def _aconv_symbol(
    value: Affine, direction: str, source_code: int, pc_sw: int
) -> Affine:
    """Return an opaque, stable symbolic result for map-dependent ACONV.

    B2W is not affine when the source's low two bits are unknown.  The PRM's
    address-map/ILAD exception also prevents treating a symbolic source as an
    unconditional shift.  Retaining a source-derived opaque symbol lets the
    bounded writer tracer continue without asserting a false linear relation.
    """
    pieces = [direction, str(source_code), "%x" % pc_sw, "%x" % value.constant]
    pieces.extend("%s_%x" % (name, coefficient) for name, coefficient in value.terms)
    return symbol("aconv_" + "_".join(pieces))


def _aconv(value: Value, w2b: bool, source_code: int, pc_sw: int) -> Value:
    """Apply the PRM-likely ACONV arithmetic without inventing ILAD behavior."""
    if isinstance(value, Const):
        mapped = (
            normal_word_to_architectural_byte(value.value)
            if w2b
            else byte_to_normal_word(value.value)
        )
        if mapped is not None:
            return Const(mapped)
        # PRM Table 6-4: an address already in the destination space is
        # unchanged. In particular W2B must not multiply an L1 byte pointer.
        already_in_space = (
            byte_to_normal_word(value.value)
            if w2b
            else normal_word_to_architectural_byte(value.value)
        )
        if already_in_space is not None:
            return value
        return Const(value.value << 2 if w2b else value.value >> 2)
    if not isinstance(value, Affine):
        return Unknown("ACONV source is not symbolic")
    if w2b:
        return _multiply(value, Const(4), "ACONV W2B")
    if value.constant % 4 == 0 and all(
        coefficient % 4 == 0 for _, coefficient in value.terms
    ):
        return _affine(
            value.constant // 4,
            tuple((name, coefficient // 4) for name, coefficient in value.terms),
        )
    return _aconv_symbol(value, "b2w", source_code, pc_sw)


def _op_and(a: int, b: int) -> int:
    return a & b


def _op_or(a: int, b: int) -> int:
    return a | b


def _op_xor(a: int, b: int) -> int:
    return a ^ b


def _op_andnot(a: int, b: int) -> int:
    """A AND NOT B: bit clear."""
    return a & ~b


def _bitwise(
    left: Operand, right: Operand, expression: str, operation: Callable[[int, int], int]
) -> Operand:
    """OPERATION (one of the named _op_* functions above) on two known
    32-bit values; Unknown otherwise."""
    if isinstance(left, Const) and isinstance(right, Const):
        return Const(operation(left.value, right.value))
    return Unknown(expression)


def _not(value: Operand, expression: str) -> Operand:
    return Const(~value.value) if isinstance(value, Const) else Unknown(expression)


def _is_unknown(value: Value) -> bool:
    """VALUE carries no usable knowledge: an Unknown (or a PartialConst,
    which only ASTATX/ASTATY hold). A concrete specialisation tests the
    known flag; Affine values exist only in the symbolic driver."""
    return isinstance(value, (Unknown, PartialConst))


def _astatx_known_bit(value: Value, bit: int) -> bool | None:
    """Return ASTATX/ASTATY bit BIT if known, else None."""
    if isinstance(value, Const):
        return bool(value.value & (1 << bit))
    if isinstance(value, PartialConst):
        if value.mask & (1 << bit):
            return bool(value.bits & (1 << bit))
        return None
    return None


# ---------------------------------------------------------------------------
# ASTATX/ASTATY knowledge updates. The flag registers are the only UREGs
# stored as PartialConst; these functions are the whole of the lattice
# arithmetic over them.
# ---------------------------------------------------------------------------

CACC_MASK = 0xFF000000
# Bits a known compare keeps from the old ASTATX: 6-23 minus AF.
_COMPARE_PRESERVE = 0x00FFFFC0 & ~(1 << AF_BIT)


def _astatx_define(old: Value, mask: int, bits: int) -> Value:
    """Return OLD with MASK's bits set definitively to BITS (masked to MASK);
    bits outside MASK keep whatever knowledge OLD already carried."""
    mask &= 0xFFFFFFFF
    bits &= mask
    if isinstance(old, Const):
        return Const((old.value & ~mask) | bits)
    if isinstance(old, PartialConst):
        new_mask = old.mask | mask
        new_bits = (old.bits & ~mask) | bits
        return (
            Const(new_bits)
            if new_mask == 0xFFFFFFFF
            else PartialConst(new_mask, new_bits)
        )
    # Unknown (or a stray non-ASTATX Value type): only MASK becomes known.
    if not mask:
        return old
    return Const(bits) if mask == 0xFFFFFFFF else PartialConst(mask, bits)


def _astatx_forget(old: Value, mask: int) -> Value:
    """Return OLD with MASK's bits downgraded to unknown; other bits keep
    whatever knowledge OLD already carried."""
    mask &= 0xFFFFFFFF
    if isinstance(old, Const):
        new_mask = 0xFFFFFFFF & ~mask
        new_bits = old.value & new_mask
    elif isinstance(old, PartialConst):
        new_mask = old.mask & ~mask
        new_bits = old.bits & new_mask
    else:
        return old
    return (
        Unknown("astatx bits forgotten")
        if new_mask == 0
        else PartialConst(new_mask, new_bits)
    )


def _apply_flag_update(old: Value, update: FlagUpdate) -> Value:
    """The new ASTATX/ASTATY after UPDATE (see FlagUpdate)."""
    if update.cacc >= 0 and isinstance(old, Const):
        value = old.value
        shifted = (
            (value & _COMPARE_PRESERVE)
            | ((value >> 1) & 0x7F000000)
            | (update.cacc << 31)
        )
        result: Value = Const((shifted & ~update.define_mask) | update.define_bits)
        forget = update.forget_mask & ~CACC_MASK
        return _astatx_forget(result, forget) if forget else result
    result = old
    if update.define_mask:
        result = _astatx_define(result, update.define_mask, update.define_bits)
    if update.forget_mask:
        result = _astatx_forget(result, update.forget_mask)
    return result


FLAGS_NONE = FlagUpdate(0, 0, 0)


def _flags_define(mask: int, bits: int) -> FlagUpdate:
    """Define MASK's bits as BITS."""
    return FlagUpdate(mask, bits & mask, 0)


def _flags_forget(mask: int) -> FlagUpdate:
    """Forget MASK's bits."""
    return FlagUpdate(0, 0, mask)


def _flags_put(update: FlagUpdate, bit: int, known: bool | None) -> FlagUpdate:
    """UPDATE with BIT set to KNOWN: True/False defines it, None forgets it.
    A later put of the same bit replaces an earlier one."""
    m = 1 << bit
    if known is None:
        return FlagUpdate(
            update.define_mask & ~m,
            update.define_bits & ~m,
            update.forget_mask | m,
            update.cacc,
        )
    return FlagUpdate(
        update.define_mask | m,
        (update.define_bits & ~m) | (m if known else 0),
        update.forget_mask & ~m,
        update.cacc,
    )


def _flags_from_pairs(pairs: tuple[tuple[int, bool | None], ...]) -> FlagUpdate:
    """An update from (bit, known) pairs, applied in order with
    ``_flags_put`` (so a later pair for the same bit wins)."""
    update = FLAGS_NONE
    for bit, known in pairs:
        update = _flags_put(update, bit, known)
    return update


def _flags_then(first: FlagUpdate, second: FlagUpdate) -> FlagUpdate:
    """One update equal to FIRST followed by SECOND: for each bit the later
    update wins. SECOND must not carry a CACC shift of its own.

    Exact for every result that keeps at least one known bit; when both
    leave every bit unknown, the Unknown's reason text may differ."""
    forget = (first.forget_mask & ~second.define_mask) | second.forget_mask
    define = (second.define_mask | (first.define_mask & ~first.forget_mask)) & ~forget
    bits = (second.define_bits | (first.define_bits & ~second.define_mask)) & define
    return FlagUpdate(define, bits, forget, first.cacc)


def _flags_or(a: FlagUpdate, b: FlagUpdate) -> FlagUpdate:
    """Kleene OR of two updates bit by bit (PRM p.3-21/3-22: in the dual
    add/subtract "the ALU flags from the two operations are ORed
    together"). A known 1 wins; otherwise an unknown bit stays unknown; a
    bit only one update mentions keeps that update's value."""
    true = a.define_bits | b.define_bits
    unknown = (a.forget_mask | b.forget_mask) & ~true
    false = (a.define_mask | b.define_mask) & ~true & ~unknown
    return FlagUpdate(true | false, true, unknown)
