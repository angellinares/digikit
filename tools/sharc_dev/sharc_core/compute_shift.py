"""Shifter compute (cu=2) and the ShiftImm form: handler bodies dispatched
by tools/sharc_dev/sharc_core/compute.py's SHIFT_OPS table, plus _shift_immediate
(the ShiftImm 6a_mem/6b_shiftimm forms call this directly, not through
_compute -- see its own docstring).

Handler bodies moved verbatim from the cu==2 branches of
tools/sharc_dev/sharc_trace.py's old _compute if-chain and from _shift_immediate;
each still carries that branch's own PRM/PGR citation.
"""

from __future__ import annotations

from collections.abc import Callable, Mapping

from .encoding import (
    ALU_FLAGS_MASK,
    AV_BIT,
    SF_BIT,
    SHIFT_FLAGS_MASK,
    SS_BIT,
    SV_BIT,
    SZ_BIT,
    UREG_CODES,
    _field,
)
from .flags import (
    _astatx_bit_field,
    _astatx_btst,
    _astatx_fext,
    _astatx_fext_reg,
    _astatx_lefto,
    _astatx_leftz,
    _astatx_shift,
)
from .state import (
    MR,
    NO_SPECIAL,
    _ureg,
    _ureg_raw,
)
from .values import (
    ComputeResult,
    Const,
    FlagUpdate,
    Operand,
    Unknown,
    Value,
    _astatx_known_bit,
    _bitwise,
    _flags_forget,
    _flags_from_pairs,
    _flags_put,
    _op_andnot,
    _op_or,
    _op_xor,
    _signed,
    _signed32,
)

Handler = Callable[
    [
        int,
        int,
        int,
        Operand,
        Operand,
        Mapping[int, Value],
        Mapping[str, Operand | MR] | None,
    ],
    ComputeResult,
]


def _field_deposit_or(
    dest: Operand,
    source: Operand,
    position: int,
    length: int,
    sign_extend: bool,
    label: str,
) -> Operand:
    """RN = RN or fdep RX by BIT6:LEN6[(SE)] (PGR p.11-70/11-74, cross-
    checked against compute_table.json's shiftop_shiftimm rows 011001/
    011011): deposits the low LENGTH bits of SOURCE at bit POSITION of DEST
    -- sign-extending the deposited field's own top bit upward through bit
    31 first when SIGN_EXTEND, mirroring the sign-extend convention the
    already-implemented FEXT-SE (opcode 0x12) uses -- then ORs the result
    into DEST (bits outside [POSITION, min(POSITION+LENGTH,32)) are left as
    DEST already had them, unlike the plain, non-OR FDEP forms this tracer
    does not implement)."""
    if length == 0:
        return dest
    if not isinstance(source, Const) or not isinstance(dest, Const):
        return Unknown(label)
    field = source.value & ((1 << length) - 1)
    if sign_extend and (field >> (length - 1)) & 1:
        field |= (0xFFFFFFFF << length) & 0xFFFFFFFF
    deposited = (field << position) & 0xFFFFFFFF
    return Const((dest.value & 0xFFFFFFFF) | deposited)


_BFF_WORD_MASK = 0xFFFFFFFF
_BFF_MASK64 = (1 << 64) - 1


def _as_operand(value: Operand | MR, label: str) -> Operand:
    """Narrow a special-dict value (``Mapping[str, Operand | MR]``, shared
    with MRF/MRB) down to Operand for mypy. BFF_HI/BFF_LO/BFFWRP are never
    modelled as MR -- only the multiplier accumulator is -- so this is
    always a no-op in practice; it reports Unknown instead of raising if
    that invariant is ever broken, rather than letting an MR silently reach
    arithmetic that assumes a 32-bit Operand."""
    return value if not isinstance(value, MR) else Unknown(label)


def _bff_words(
    special: Mapping[str, Operand | MR] | None,
) -> tuple[Operand, Operand]:
    """The bit FIFO's current 64-bit content as (hi, lo) 32-bit halves.

    PRM p.3-18 (out/refs/sharc-plus-prm/all.txt:3507-3511) / PGR p.11-86
    (pgr.txt:23287): "The bit FIFO consists of a 64-bit register internal
    to the shifter and an associated write pointer register... The bit
    FIFO register and write pointer can be accessed only through the
    BITDEP and BITEXT instructions." New bits pack in from the write
    pointer's boundary (MSB-first); BITEXT always reads from the top.
    Tracked in STATE.special (see this module's own docstring) as
    "BFF_HI" (bits [63:32]) and "BFF_LO" (bits [31:0]), the same dict
    BFFWRP already uses. Absent (until a BITDEP, ShiftImm 0x1d, fills
    it) reads as Unknown, matching BFFWRP's own uninitialized default
    below."""
    absent = Unknown("uninitialized bit FIFO")
    hi = (special if special is not None else NO_SPECIAL).get("BFF_HI", absent)
    lo = (special if special is not None else NO_SPECIAL).get("BFF_LO", absent)
    return _as_operand(hi, "BFF_HI is an MR"), _as_operand(lo, "BFF_LO is an MR")


def _bff_extract(
    hi: Operand, lo: Operand, bitlen: int
) -> tuple[Operand, Operand, Operand]:
    """BITEXT's documented pseudocode (PGR p.11-91, pgr.txt:23446-23452):

        1. Rn = FEXT BFF[63:32] BY <(32-bitlen)>:<bitlen>
        2. BFF = BFF << bitlen

    (step 3, BFFWRP -= bitlen, is the caller's job -- it runs independently
    of whether the FIFO content itself is known). Folding BFF's two 32-bit
    halves into one 64-bit int and shifting that reproduces step 1 exactly
    for any BITLEN in [0, 32]: with HI/LO known, ``combined >> (64 -
    bitlen)`` keeps only HI's top BITLEN bits (LO's 32 bits fall off the
    bottom of a right shift of 32 or more), the same bits step 1's FEXT on
    BFF[63:32] alone would read; the same combined value left-shifted by
    BITLEN and re-split is step 2. Returns (extracted, new_hi, new_lo).

    A BITLEN outside 0..32 is documented "prohibited" (PGR p.11-91) for
    step 1 -- FEXT on a single 32-bit word cannot return more than 32 bits
    -- but step 2's 64-bit shift has no such limit (the BITLEN12 values
    up to 535 once seen here were OR-FDEP fields read as BITEXT(NU) through
    a misprinted opcode, see _shift_immediate), so this function stays crash-safe
    (no negative Python shift) for any non-negative BITLEN and still
    returns the shifted FIFO; the ShiftImm opcode 0x14/0x16 handler below
    is the one that discards EXTRACTED and substitutes Unknown when BITLEN
    is out of range, per this file's own module docstring on staying
    Unknown only where the manual actually says so."""
    if bitlen == 0:
        return Const(0), hi, lo
    if not isinstance(hi, Const) or not isinstance(lo, Const):
        unknown = Unknown("bitext: bit FIFO content unknown")
        return unknown, unknown, unknown
    combined = (hi.value << 32) | lo.value
    shift_out = 64 - bitlen
    extracted = combined >> shift_out if shift_out >= 0 else combined << -shift_out
    extracted &= (1 << bitlen) - 1
    shifted = (combined << bitlen) & _BFF_MASK64
    return (
        Const(extracted),
        Const((shifted >> 32) & _BFF_WORD_MASK),
        Const(shifted & _BFF_WORD_MASK),
    )


def _bff_deposit(
    hi: Operand, lo: Operand, wrp: int, source: Operand, bitlen: int
) -> tuple[Operand, Operand]:
    """BITDEP's documented pseudocode (PGR p.11-87, pgr.txt:23313-23319):

        BFF = BFF OR FDEP Rx BY <64-(BFFWRP+bitlen)>:<bitlen>

    (step 2, BFFWRP += bitlen, is the caller's job). BITDEP is shifter
    opcode 0111 0100 (PGR Table 12-11, pp.12-10/12-11), ShiftImm 0x1d;
    _shift_immediate wires it (dt2-1.16 sw 0xb8863c/0xb8863f). Shares
    _bff_extract's 64-bit-int model; tests/sharc/semantics/test_sharc_compute_shift.py
    checks it against the PRM/PGR's BITDEP+BITEXT pairing
    (out/refs/sharc-plus-prm/all.txt:3495-3545)."""
    if bitlen == 0:
        return hi, lo
    if (
        not isinstance(hi, Const)
        or not isinstance(lo, Const)
        or not isinstance(source, Const)
    ):
        unknown = Unknown("bitdep: operand unknown")
        return unknown, unknown
    position = 64 - (wrp + bitlen)
    if position < 0:
        # "Attempts to append more bits than the bit FIFO has room for
        # results in an undefined bit FIFO and write pointer. SV is set in
        # that case" (PGR p.11-87).
        unknown = Unknown("bitdep: fifo overflow (wrp=%d, bitlen=%d)" % (wrp, bitlen))
        return unknown, unknown
    combined = (hi.value << 32) | lo.value
    field = source.value & ((1 << bitlen) - 1)
    combined |= (field << position) & _BFF_MASK64
    return Const((combined >> 32) & _BFF_WORD_MASK), Const(combined & _BFF_WORD_MASK)


def _bitdep(
    source: Operand, bitlen12: int, special: Mapping[str, Operand | MR] | None
) -> ComputeResult:
    """BITDEP (immediate and register forms): deposit BITLEN12 bits of SOURCE into the bit FIFO."""
    # PGR Table 12-11: BITDEP RX by BITLEN12 is 0111 0100, ShiftImm
    # 011101 (see the 0x19/0x1B note above); PGR p.11-86/11-87:
    # BFF = BFF OR FDEP RX BY <64-(BFFWRP+bitlen)>:<bitlen>, BFFWRP +=
    # bitlen. SF = updated BFFWRP >= 32; SV = bitlen > 32 or overflow
    # (which also undefines the FIFO and pointer); SZ, SS cleared.
    hi, lo = _bff_words(special)
    old_wrp = (special if special is not None else NO_SPECIAL).get("BFFWRP")
    if isinstance(old_wrp, Const):
        overflow: bool | None = old_wrp.value + bitlen12 > 64
        dep_hi, dep_lo = _bff_deposit(hi, lo, old_wrp.value, source, bitlen12)
        dep_wrp: Operand = (
            Unknown("undefined BFFWRP") if overflow else Const(old_wrp.value + bitlen12)
        )
    else:
        overflow = None
        dep_hi = dep_lo = Unknown("bitdep: BFFWRP unknown")
        dep_wrp = Unknown("uninitialized BFFWRP")
    dep_sv: bool | None = (
        True if bitlen12 > 32 or overflow else (None if overflow is None else False)
    )
    dep_updates: FlagUpdate = _flags_from_pairs(
        (
            (SS_BIT, False),
            (SZ_BIT, False),
            (SV_BIT, dep_sv),
            (SF_BIT, dep_wrp.value >= 32 if isinstance(dep_wrp, Const) else None),
        )
    )
    return (
        ("BFFWRP", "BFF_HI", "BFF_LO"),
        (dep_wrp, dep_hi, dep_lo),
        "bit-deposit",
        dep_updates,
    )


def _bitext(
    rn: int, bitlen12: int, no_update: bool, special: Mapping[str, Operand | MR] | None
) -> ComputeResult:
    """BITEXT / BITEXT (NU) (immediate and register forms): extract BITLEN12 bits from the bit FIFO."""
    # ShiftImm 010100/010110 (PGR Table 12-11's 0101 0000/0101 1000;
    # see the 0x19 note above for the PRM's misprinted 6-bit column) /
    # PGR p.11-90/11-91 (pgr.txt:23402-23481; PRM p.3-18, all.txt:
    # 3507-3511): RN = BITEXT RX|BITLEN12(,NU). Extracts the top
    # BITLEN12 bits of the shifter's internal 64-bit bit FIFO into RN,
    # left-shifts the FIFO by that amount, and decrements BFFWRP by the
    # same amount; 0x16's NU modifier skips the FIFO/pointer update
    # (PGR: "does not modify the bit FIFO or Write pointer") and leaves
    # SF reflecting the un-updated pointer instead. The FIFO is tracked
    # via _bff_words/_bff_extract above (special["BFF_HI"/"BFF_LO"]);
    # it starts Unknown/absent until a BITDEP (0x1d above) fills it, so
    # RN, and the FIFO/BFFWRP once a
    # caller does track them, come out exactly as before this change
    # whenever the FIFO was never tracked to begin with (BFF_HI/BFF_LO
    # are only added to the returned destinations when SPECIAL already
    # carries them, so a caller that never seeds the FIFO keeps the
    # pre-existing 2-destination (RN, "BFFWRP") shape). What changes
    # unconditionally: SV now also follows PGR's "attempts to get more
    # bits than those in the bit FIFO" case (bitlen12 > the current
    # BFFWRP -- previously ignored entirely), and SZ follows the
    # manual's actual formula instead of being hardcoded Unknown.
    over_32 = bitlen12 > 32
    hi, lo = _bff_words(special)
    old_wrp = (special if special is not None else NO_SPECIAL).get("BFFWRP")
    # PGR p.11-91 documents two distinct, independent error conditions
    # with two distinct consequences:
    #   "A value of more than 32 ... is prohibited and use of such a
    #   value sets SV" (OVER_32) -- invalidates RN (the FEXT-based
    #   pseudocode only extracts from a single 32-bit word), but the
    #   pointer/FIFO bookkeeping (step 2/3) is not itself declared
    #   undefined, so it still runs mechanically.
    #   "Attempts to get more bits than those in the bit FIFO results
    #   in undefined pointer and bit FIFO. SV is set in that case"
    #   (UNDERFLOW) -- explicitly undefines the pointer *and* FIFO too
    #   (None when BFFWRP is not known, so this tracer cannot tell).
    underflow = None if not isinstance(old_wrp, Const) else bitlen12 > old_wrp.value
    pointer_and_fifo_undefined = underflow is True
    # _bff_extract's 64-bit shift (its pseudocode step 2) is well
    # defined for any BITLEN12, even one PGR calls "prohibited" for
    # step 1's FEXT -- Python's shift does not care, and mechanical
    # pointer/FIFO bookkeeping is exactly what OVER_32 alone leaves
    # intact. Only actually use its "extracted" element when BITLEN12
    # is in the documented 0..32 range.
    extracted, new_hi, new_lo = _bff_extract(hi, lo, bitlen12)
    value = (
        Unknown("bitext: undefined (bitlen %d > 32)" % bitlen12)
        if over_32
        else extracted
    )
    fifo_after: tuple[Operand, Operand] = (
        (Unknown("undefined bit FIFO"), Unknown("undefined bit FIFO"))
        if pointer_and_fifo_undefined
        else (new_hi, new_lo)
    )
    if over_32:
        sv: bool | None = True
    elif underflow is None:
        sv = None
    else:
        sv = underflow
    updates = _flags_from_pairs(
        (
            (SS_BIT, False),
            (SV_BIT, sv),
            (SZ_BIT, (value.value == 0) if isinstance(value, Const) else None),
        )
    )
    # Only thread BFF_HI/BFF_LO through the result when SPECIAL already
    # tracks the FIFO (see this branch's opening comment); otherwise
    # keep the pre-existing 2-destination (RN, "BFFWRP") shape.
    track_fifo = "BFF_HI" in (
        special if special is not None else NO_SPECIAL
    ) or "BFF_LO" in (special if special is not None else NO_SPECIAL)
    if no_update:
        # PGR p.11-91: NU "returns the requested number of bits as
        # usual" (VALUE above already reflects that) "but does not
        # modify the bit FIFO or Write pointer", and "the SF flag is
        # not updated" -- it keeps reflecting the pointer from before
        # this instruction, not the hypothetical post-decrement value.
        updates = _flags_put(
            updates,
            SF_BIT,
            old_wrp.value >= 32 if isinstance(old_wrp, Const) else None,
        )
        return rn, value, "bit-extract-nu", updates
    wrp_after: Operand = (
        Const(old_wrp.value - bitlen12)
        if isinstance(old_wrp, Const) and not pointer_and_fifo_undefined
        else Unknown(
            "undefined BFFWRP" if isinstance(old_wrp, Const) else "uninitialized BFFWRP"
        )
    )
    updates = _flags_put(
        updates,
        SF_BIT,
        wrp_after.value >= 32 if isinstance(wrp_after, Const) else None,
    )
    if track_fifo:
        return (
            (rn, "BFFWRP", "BFF_HI", "BFF_LO"),
            (value, wrp_after, fifo_after[0], fifo_after[1]),
            "bit-extract",
            updates,
        )
    return (
        (rn, "BFFWRP"),
        (value, wrp_after),
        "bit-extract",
        updates,
    )


def _shift_immediate(
    f: Mapping[str, int],
    values: Mapping[int, Value],
    special: Mapping[str, Operand | MR] | None = None,
) -> ComputeResult:
    """Execute the documented ShiftImm subset seen on qualifying paths.

    SPECIAL is the same special-register mapping ``_compute`` reads MRF
    from: "BFFWRP" (the bit-FIFO write pointer; opcodes 0x14/0x16/0x1f
    below read and update it) and "BFF_HI"/"BFF_LO" (the bit FIFO's own
    64-bit content, tracked as two 32-bit halves; opcodes 0x14/0x16 read
    and update these too -- see _bff_words/_bff_extract below). Defaults to
    None for every caller that does not need those opcodes."""
    field = (_field(f, "shiftimm[22:16]") << 16) | _field(f, "shiftimm[15:0]")
    opcode = (field >> 16) & 0x3F
    data8 = (field >> 8) & 0xFF
    rn, rx = (field >> 4) & 0xF, field & 0xF
    source = _ureg(values, rx)
    if opcode in (0x00, 0x01, 0x08, 0x09):
        amount = _signed(data8, 8)
        base = opcode & 0x01
        name = "lshift" if base == 0x00 else "ashift"
        if amount == 0:
            shifted = source
        elif not isinstance(source, Const):
            shifted = Unknown("%s R%d by %d" % (name, rx, amount))
        elif amount >= 32:
            shifted = Const(0)
        elif amount <= -32:
            shifted = (
                Const(0xFFFFFFFF)
                if base == 0x01 and source.value & 0x80000000
                else Const(0)
            )
        elif amount > 0:
            shifted = Const(source.value << amount)
        elif base == 0x01:
            shifted = Const(_signed32(source.value) >> -amount)
        else:
            shifted = Const(source.value >> -amount)
        if opcode in (0x08, 0x09):
            # PRM Table 17-9, shiftimm 001000/001001 (p. 17-10): RN = RN or
            # (l/a)shift RX by DATA8.
            value = _bitwise(
                _ureg(values, rn),
                shifted,
                "R%d or %s R%d by %d" % (rn, name, rx, amount),
                _op_or,
            )
            operation = (
                "logical-shift-or-immediate"
                if base == 0x00
                else "arithmetic-shift-or-immediate"
            )
        else:
            value = shifted
            operation = (
                "logical-shift-immediate"
                if base == 0x00
                else "arithmetic-shift-immediate"
            )
        # SZ is defined from the shifted value before any OR (PRM pp.509-510:
        # "Set if the shifted result is zero"); SS is cleared for every one
        # of these forms except OR-ashift (opcode 0x09), whose entry omits
        # the SS line.
        ss_mode = "forget" if opcode == 0x09 else "clear"
        return rn, value, operation, _astatx_shift(amount, shifted, ss_mode)
    if opcode == 0x10:
        position = data8 & 0x3F
        length = (_field(f, "dataex[3:0]") << 2) | (data8 >> 6)
        if length == 0:
            value = Const(0)
        elif not isinstance(source, Const):
            value = Unknown("fext R%d by %d:%d" % (rx, position, length))
        else:
            value = Const((source.value >> position) & ((1 << min(length, 32)) - 1))
        return (
            rn,
            value,
            "field-extract-immediate",
            _astatx_fext(position + length, value),
        )
    if opcode == 0x12:
        # PRM Table 17-9 p.17-10/17-11 (out/refs/sharc-plus-prm/all.txt
        # lines 22781-22785): shiftimm 010010 is
        # "RREG = fext RREG by BIT6:LEN6 (se)" -- the sign-extending twin
        # of the already-implemented opcode 0x10. Field layout (position,
        # length) is identical to 0x10; NOTE at PRM p.3-17 (all.txt
        # line 3486) says the (SE) option "sign extends the left bits" of
        # the extracted field, i.e. bits above the extracted field take the
        # value of the field's own sign (MSB) instead of being cleared. PGR
        # p.11-78 (out/refs/adsp-2136x_2137x_214xx_pgr_rev2.4/all.txt:23040)
        # spells this out for "Rn = FEXT Rx BY <bit6>:<len6> (SE)": "The
        # MSBs of Rn are sign-extended by the MSB of the extracted field" --
        # i.e. the field itself must first be isolated to its own LENGTH
        # bits (as 0x10 above does) before its sign is read and extended;
        # bits of SOURCE above the field must not leak into the result.
        position = data8 & 0x3F
        length = (_field(f, "dataex[3:0]") << 2) | (data8 >> 6)
        if length == 0:
            value = Const(0)
        elif not isinstance(source, Const):
            value = Unknown("fext R%d by %d:%d (se)" % (rx, position, length))
        else:
            field_len = min(length, 32)
            field = (source.value >> position) & ((1 << field_len) - 1)
            value = Const(_signed(field, field_len) & 0xFFFFFFFF)
        return (
            rn,
            value,
            "field-extract-immediate-se",
            _astatx_fext(position + length, value),
        )
    if opcode in (0x30, 0x31):
        position = data8
        if position > 31:
            value = source
        else:
            calculate = _op_or if opcode == 0x30 else _op_andnot
            name = "bset" if opcode == 0x30 else "bclr"
            value = _bitwise(
                source,
                Const(1 << position),
                "%s R%d by %d" % (name, rx, position),
                calculate,
            )
        operation = "bit-set-immediate" if opcode == 0x30 else "bit-clear-immediate"
        return rn, value, operation, _astatx_bit_field(Const(position), value)
    if opcode == 0x32:
        position = data8
        if position > 31:
            value = source
        else:
            value = _bitwise(
                source,
                Const(1 << position),
                "btgl R%d by %d" % (rx, position),
                _op_xor,
            )
        return (
            rn,
            value,
            "bit-toggle-immediate",
            _astatx_bit_field(Const(position), value),
        )
    if opcode == 0x33:
        # PRM Table 17-9: ShiftImm 110011 is btst RX by DATA8, the immediate
        # form of the 11001100 register operation. It updates status only, so
        # RN keeps its value.
        return rn, source, "bit-test", _astatx_btst(source, Const(data8))
    if opcode in (0x19, 0x1B):
        # RN = RN or fdep RX by BIT6:LEN6 (0x19) and its (SE) form (0x1B),
        # PGR p.11-70/11-74. PGR Table 12-11 (pp.12-10/12-11) gives the
        # 8-bit shifter opcodes 0110 0100 / 0110 1100 and says a ShiftImm
        # opcode is their "upper 6 MSBs": 011001 / 011011. The SHARC+ PRM's
        # Table 17-9 (pp.17-10/17-11) has the same 8-bit values, but its
        # 6-bit column is off by one row for bitext (nu), or fdep and or
        # fdep (se) (011001 / 011011 / 011101 instead of 010110 / 011001 /
        # 011011). The image agrees with PGR: every 0x19 in dt2-1.16 has a
        # BIT6:LEN6 field that BITEXT would reject (bitlen12 95..535) and an
        # OR-FDEP reading that fits, e.g. the integer-to-float helpers'
        # sw 0xb88fe6 "R0 = R0 or fdep R2 by 23:8" (exponent) and sw
        # 0xb88fa4 "R0 = R0 or fdep R12 by 31:1" (sign).
        sign_extend = opcode == 0x1B
        position = data8 & 0x3F
        length = (_field(f, "dataex[3:0]") << 2) | (data8 >> 6)
        dest = _ureg(values, rn)
        label = "R%d or fdep R%d by %d:%d%s" % (
            rn,
            rx,
            position,
            length,
            " (se)" if sign_extend else "",
        )
        value = _field_deposit_or(dest, source, position, length, sign_extend, label)
        operation = "field-deposit-or-se" if sign_extend else "field-deposit-or"
        return rn, value, operation, _astatx_fext(position + length, value)
    if opcode == 0x1D:
        # PGR p.11-86/11-87 and Table 12-11 (0111 0100): see _bitdep.
        bitlen12 = (_field(f, "dataex[3:0]") << 8) | data8
        return _bitdep(source, bitlen12, special)
    if opcode == 0x1F:
        # compute_table.json shiftop_shiftimm row 011111 / PGR p.11-89
        # (pgr.txt:23379): BFFWRP = DATA7 -- the immediate form of cu=2
        # opcode 0x7c above (writes the bit-FIFO write-pointer special
        # register, not an RN).
        new_wrp = Const(data8 & 0x7F)
        updates: FlagUpdate = _flags_from_pairs(
            (
                (SS_BIT, False),
                (SZ_BIT, False),
                (SV_BIT, new_wrp.value > 64),
                (SF_BIT, new_wrp.value >= 32),
            )
        )
        return "BFFWRP", new_wrp, "bffwrp-write", updates
    if opcode in (0x14, 0x16):
        # PGR p.11-90/11-91 and Table 12-11 (0101 0000 / 0101 1000): see _bitext.
        bitlen12 = (_field(f, "dataex[3:0]") << 8) | data8
        return _bitext(rn, bitlen12, opcode == 0x16, special)
    raise ValueError("unsupported ShiftImm opcode %#x" % opcode)


# PRM Table 17-9: SHIFTOP 00000000 is RN = LSHIFT RX by RY. The signed
# low byte of RY selects a left (positive) or logical right (negative)
# shift; magnitudes of 32 or more produce zero.
def shift_logical(rn, rx, ry, left, right, values, special) -> tuple:
    amount: int | None = None
    value: Operand
    if not isinstance(right, Const):
        value = Unknown("lshift R%d by R%d" % (rx, ry))
    else:
        amount = _signed(right.value & 0xFF, 8)
        if amount == 0:
            value = left
        elif not isinstance(left, Const):
            value = Unknown("lshift R%d by %d" % (rx, amount))
        elif amount >= 32 or amount <= -32:
            value = Const(0)
        elif amount > 0:
            value = Const(left.value << amount)
        else:
            value = Const(left.value >> -amount)
    return rn, value, "logical-shift", _astatx_shift(amount, value, "clear")


# PRM Table 17-9 p.17-10 (out/refs/sharc-plus-prm/all.txt line 22765):
# SHIFTOP 00000100 is RN = ASHIFT RX by RY -- the register-operand twin
# of the already-implemented ShiftImm opcode 0x01 (arithmetic shift by
# an 8-bit immediate). Same signed-low-byte amount, same 32-magnitude
# saturation, differing only in that a right shift (negative amount) is
# sign-extending, mirroring _shift_immediate's opcode==0x01 branch
# exactly.
# CALIBRATION NOTE: added in a scratch copy of sharc_trace.py for this
# task only; not part of the tracked tool.
def shift_arithmetic(rn, rx, ry, left, right, values, special) -> tuple:
    amount = None
    value: Operand
    if not isinstance(right, Const):
        value = Unknown("ashift R%d by R%d" % (rx, ry))
    else:
        amount = _signed(right.value & 0xFF, 8)
        if amount == 0:
            value = left
        elif not isinstance(left, Const):
            value = Unknown("ashift R%d by %d" % (rx, amount))
        elif amount >= 32:
            value = Const(0)
        elif amount <= -32:
            value = Const(0xFFFFFFFF) if left.value & 0x80000000 else Const(0)
        elif amount > 0:
            value = Const(left.value << amount)
        else:
            value = Const(_signed32(left.value) >> -amount)
    return rn, value, "arithmetic-shift", _astatx_shift(amount, value, "clear")


# PRM Table 17-9 (compute_table.json shiftop_shiftimm row 00100000):
# SHIFTOP 00100000 is RN = RN OR LSHIFT RX by RY -- the register-operand
# twin of the already-implemented ShiftImm opcode 0x08 (OR-lshift by an
# 8-bit immediate). Same signed-low-byte amount/32-magnitude rule as
# plain lshift (0x00 above), computed independently here rather than
# shared, then ORed into RN instead of replacing it.
def shift_logical_or(rn, rx, ry, left, right, values, special) -> tuple:
    or_amount: int | None = None
    shifted: Operand
    if not isinstance(right, Const):
        shifted = Unknown("lshift R%d by R%d" % (rx, ry))
    else:
        or_amount = _signed(right.value & 0xFF, 8)
        if or_amount == 0:
            shifted = left
        elif not isinstance(left, Const):
            shifted = Unknown("lshift R%d by %d" % (rx, or_amount))
        elif or_amount >= 32 or or_amount <= -32:
            shifted = Const(0)
        elif or_amount > 0:
            shifted = Const(left.value << or_amount)
        else:
            shifted = Const(left.value >> -or_amount)
    value = _bitwise(
        _ureg(values, rn),
        shifted,
        "R%d or lshift R%d by R%d" % (rn, rx, ry),
        _op_or,
    )
    return rn, value, "logical-shift-or", _astatx_shift(or_amount, shifted, "clear")


# PRM Table 17-9: SHIFTOP 10001000 is RN = leftz RX.
def shift_leftz(rn, rx, ry, left, right, values, special) -> tuple:
    value = (
        Const(32 if left.value == 0 else 32 - left.value.bit_length())
        if isinstance(left, Const)
        else Unknown("leftz R%d" % rx)
    )
    return rn, value, "leftz", _astatx_leftz(left, value)


# PGR p.11-83 (pgr.txt:23171): SHIFTOP 10001100 is RN = LEFTO RX --
# leading 1s, the complement of leftz above (leading 0s of ~RX).
def shift_lefto(rn, rx, ry, left, right, values, special) -> tuple:
    value: Operand
    if isinstance(left, Const):
        inverted = (~left.value) & 0xFFFFFFFF
        value = Const(32 if inverted == 0 else 32 - inverted.bit_length())
    else:
        value = Unknown("lefto R%d" % rx)
    return rn, value, "lefto", _astatx_lefto(left, value)


# PRM Table 18-9: SHIFTOP 11000000/11000100 are variable bit set/clear.
def _shift_bitset_impl(rn, rx, ry, left, right, opcode: int) -> tuple:
    name = "bset" if opcode == 0xC0 else "bclr"
    value: Operand
    if not isinstance(right, Const):
        value = Unknown("%s R%d by R%d" % (name, rx, ry))
    elif right.value > 31:
        value = left
    else:
        calculate = _op_or if opcode == 0xC0 else _op_andnot
        value = _bitwise(
            left, Const(1 << right.value), "%s R%d by R%d" % (name, rx, ry), calculate
        )
    return (
        rn,
        value,
        "bit-set" if opcode == 0xC0 else "bit-clear",
        _astatx_bit_field(right, value),
    )


def shift_bset(rn, rx, ry, left, right, values, special) -> tuple:
    return _shift_bitset_impl(rn, rx, ry, left, right, 0xC0)


def shift_bclr(rn, rx, ry, left, right, values, special) -> tuple:
    return _shift_bitset_impl(rn, rx, ry, left, right, 0xC4)


# PRM Table 17-9 and p. 23-5: SHIFTOP 11001000 is
# RN = btgl RX by RY.  Positions outside the 32-bit field leave RX
# unchanged.
def shift_btgl(rn, rx, ry, left, right, values, special) -> tuple:
    value: Operand
    if not isinstance(right, Const):
        value = Unknown("btgl R%d by R%d" % (rx, ry))
    elif right.value > 31:
        value = left
    else:
        value = _bitwise(
            left,
            Const(1 << right.value),
            "btgl R%d by R%d" % (rx, ry),
            _op_xor,
        )
    return rn, value, "bit-toggle", _astatx_bit_field(right, value)


# PRM Table 18-9 and pp. 24-5--24-6: SHIFTOP 11001100 is
# btst RX by RY. It changes status flags only and has no RN result.
def shift_btst(rn, rx, ry, left, right, values, special) -> tuple:
    return rn, left, "bit-test", _astatx_btst(left, right)


# PGR p.11-88 (pgr.txt:23353), opcode 0111 0000: RN = BFFWRP -- reads
# the bit-FIFO write pointer this tracer tracks via ShiftImm opcode
# 0x1f/cu=2 opcode 0x7c below (special-dict key "BFFWRP"; Unknown until
# one of those has run). SF is documented "Not affected" so it is left
# out of UPDATES entirely (stays whatever it already was).
def shift_bffwrp_read(rn, rx, ry, left, right, values, special) -> tuple:
    value = (special if special is not None else NO_SPECIAL).get(
        "BFFWRP", Unknown("uninitialized BFFWRP")
    )
    updates: FlagUpdate = _flags_from_pairs(
        (
            (SS_BIT, False),
            (SZ_BIT, False),
            (SV_BIT, False),
        )
    )
    return rn, value, "bffwrp-read", updates


# PGR p.11-89 (pgr.txt:23379), opcode 0111 1100: BFFWRP = RN|<data7> --
# the register-operand twin of ShiftImm opcode 0x1f below. The register
# form reads the RN-positioned field as its SOURCE, not a destination,
# per PGR's own text: "Updates write pointer from Rn ... Only 7 least
# significant bits of Rn are written."
def shift_bffwrp_write(rn, rx, ry, left, right, values, special) -> tuple:
    source = _ureg(values, rn)
    new_wrp = (
        Const(source.value & 0x7F)
        if isinstance(source, Const)
        else Unknown("BFFWRP = R%d" % rn)
    )
    updates = _flags_from_pairs(
        (
            (SS_BIT, False),
            (SZ_BIT, False),
            (SV_BIT, (new_wrp.value > 64) if isinstance(new_wrp, Const) else None),
            (SF_BIT, (new_wrp.value >= 32) if isinstance(new_wrp, Const) else None),
        )
    )
    return "BFFWRP", new_wrp, "bffwrp-write", updates


# Shifter opcode 1011 0000: absent from both public sources' shifter
# tables (PRM Table 17-9, p.17-10/17-11, and PGR Table 12-11, p.580-581,
# transcribed in full -- neither lists any 0xA0-0xBF row). Seen at
# `sw 0x1cd002`. Rather than guess an operation from an undocumented
# opcode, decode it (so the walk does not desync) and leave both the
# result and its flags Unknown, per this file's existing rule for gaps
# the manuals do not cover.
def shift_undocumented_b0(rn, rx, ry, left, right, values, special) -> tuple:
    label = "shift opcode 0xb0 R%d, R%d (undocumented; no public source)" % (rx, ry)
    return (
        rn,
        Unknown(label),
        "shift-undocumented-b0",
        _flags_forget(ALU_FLAGS_MASK),
    )


# Shifter opcode 0001 0100: absent from PRM Table 17-9, PGR Table 12-11
# and tools/sharc_dev/sharcspec/compute_table.json's shiftop_shiftimm table alike
# (all three transcribed in full for this project; none has a 00010100
# row). Seen at `sw 0x1cadac`, between two otherwise-ordinary
# instructions, so not obviously misdecoded data -- decoded like the
# 0xb0 case above (so the walk does not desync) rather than guessed.
def shift_undocumented_14(rn, rx, ry, left, right, values, special) -> tuple:
    label = "shift opcode 0x14 R%d, R%d (undocumented; no public source)" % (rx, ry)
    return (
        rn,
        Unknown(label),
        "shift-undocumented-14",
        _flags_forget(SHIFT_FLAGS_MASK),
    )


# ---------------------------------------------------------------------------
# Register-form shifter opcodes (PRM Table 18-9, pp.431-433; Table 3-11,
# p.3-20/3-21). The Y register is the second source: for the field
# operations it holds two positive integers, bit6 = Ry[5:0] and len6 =
# Ry[11:6] (PRM Figure 3-5 and "Bit Field Manipulation Instructions", p.3-17);
# the register-file figure there puts len6 at bits 19..14 of the 40-bit word,
# i.e. Ry[11:6] of the 32-bit value. The immediate twins are in
# _shift_immediate above (0x10/0x12/0x19/0x1b/0x14/0x16/0x1d).
# ---------------------------------------------------------------------------


# PRM pp.23-11 to 23-12 (RN = fext RX by RY, with and without (SE)): extracts
# LEN6 bits of RX starting at BIT6 and right-justifies them in RN; the (SE)
# form sign-extends the field's own top bit through bit 31 unless that bit
# lies off-scale left. Flags: SS cleared, SZ = result is zero, SV = len6 +
# bit6 > 32. Same value rule as the immediate forms 0x10/0x12; a LEN6 over 32
# still reads only the 32 bits of RX.
def _fext_impl(rn, rx, ry, left, right, sign_extend: bool) -> tuple:
    label = "fext R%d by R%d%s" % (rx, ry, " (se)" if sign_extend else "")
    value: Operand
    if not isinstance(right, Const):
        value = Unknown(label)
    else:
        position = right.value & 0x3F
        length = (right.value >> 6) & 0x3F
        if length == 0:
            value = Const(0)
        elif not isinstance(left, Const):
            value = Unknown(label)
        else:
            field_len = min(length, 32)
            field = (left.value >> position) & ((1 << field_len) - 1)
            if sign_extend:
                value = Const(_signed(field, field_len) & 0xFFFFFFFF)
            else:
                value = Const(field)
    operation = "field-extract-se" if sign_extend else "field-extract"
    return rn, value, operation, _astatx_fext_reg(right, value)


def shift_fext(rn, rx, ry, left, right, values, special) -> tuple:
    return _fext_impl(rn, rx, ry, left, right, False)


def shift_fext_se(rn, rx, ry, left, right, values, special) -> tuple:
    return _fext_impl(rn, rx, ry, left, right, True)


# PRM pp.23-6 to 23-10 (RN = [RN or] fdep RX by RY [(SE)]): deposits the low
# LEN6 bits of RX at bit BIT6 of RN. The plain forms zero everything outside
# the field; the OR forms OR the (sign-extended, for (SE)) field into the
# old RN. Flags as for fext: SS cleared, SZ = result zero, SV = len6 + bit6
# > 32. The deposit arithmetic is _field_deposit_or, shared with the
# immediate OR forms 0x19/0x1b; a plain deposit is an OR into zero.
def _fdep_impl(
    rn, rx, ry, left, right, values, or_into: bool, sign_extend: bool
) -> tuple:
    label = "%sfdep R%d by R%d%s" % (
        "R%d or " % rn if or_into else "",
        rx,
        ry,
        " (se)" if sign_extend else "",
    )
    dest: Operand = _ureg(values, rn) if or_into else Const(0)
    value: Operand
    if not isinstance(right, Const):
        value = Unknown(label)
    else:
        position = right.value & 0x3F
        length = (right.value >> 6) & 0x3F
        value = _field_deposit_or(dest, left, position, length, sign_extend, label)
    if or_into:
        operation = "field-deposit-or-se" if sign_extend else "field-deposit-or"
    else:
        operation = "field-deposit-se" if sign_extend else "field-deposit"
    return rn, value, operation, _astatx_fext_reg(right, value)


def shift_fdep(rn, rx, ry, left, right, values, special) -> tuple:
    return _fdep_impl(rn, rx, ry, left, right, values, False, False)


def shift_fdep_se(rn, rx, ry, left, right, values, special) -> tuple:
    return _fdep_impl(rn, rx, ry, left, right, values, False, True)


def shift_fdep_or(rn, rx, ry, left, right, values, special) -> tuple:
    return _fdep_impl(rn, rx, ry, left, right, values, True, False)


def shift_fdep_or_se(rn, rx, ry, left, right, values, special) -> tuple:
    return _fdep_impl(rn, rx, ry, left, right, values, True, True)


# PRM p.23-3 (RN = rot RX by RY), Table 18-9 SHIFTOP 00001000: rotates RX
# left by the two's-complement amount in RY (negative rotates right). A
# rotate by k bits to the right is a rotate by (32 - k) bits to the left, so
# the low five bits of RY (two's complement) are the left-rotate count
# whatever the operand width. Flags: SS and SV cleared, SZ = result zero.
def shift_rotate(rn, rx, ry, left, right, values, special) -> tuple:
    value: Operand
    if not isinstance(right, Const) or not isinstance(left, Const):
        value = Unknown("rot R%d by R%d" % (rx, ry))
    else:
        count = right.value & 31
        value = Const((left.value << count) | (left.value >> (32 - count)))
    updates = _flags_from_pairs(
        (
            (SS_BIT, False),
            (SV_BIT, False),
            (SZ_BIT, (value.value == 0) if isinstance(value, Const) else None),
        )
    )
    return rn, value, "rotate", updates


# PRM Table 17-9 (compute_table.json shiftop_shiftimm row 00100100): SHIFTOP
# 00100100 is RN = RN or ashift RX by RY, the register twin of ShiftImm 0x09
# and the arithmetic twin of 0x20 above. Same signed-low-byte amount and
# 32-magnitude saturation as ashift (0x04); the shifted value is ORed into
# RN. SZ/SV come from the shifted value before the OR; SS is left unknown,
# as in the immediate form 0x09 (the OR-ashift entry has no SS line, PRM
# pp.23-1 to 23-3).
def shift_arithmetic_or(rn, rx, ry, left, right, values, special) -> tuple:
    or_amount: int | None = None
    shifted: Operand
    if not isinstance(right, Const):
        shifted = Unknown("ashift R%d by R%d" % (rx, ry))
    else:
        or_amount = _signed(right.value & 0xFF, 8)
        if or_amount == 0:
            shifted = left
        elif not isinstance(left, Const):
            shifted = Unknown("ashift R%d by %d" % (rx, or_amount))
        elif or_amount >= 32:
            shifted = Const(0)
        elif or_amount <= -32:
            shifted = Const(0xFFFFFFFF) if left.value & 0x80000000 else Const(0)
        elif or_amount > 0:
            shifted = Const(left.value << or_amount)
        else:
            shifted = Const(_signed32(left.value) >> -or_amount)
    value = _bitwise(
        _ureg(values, rn),
        shifted,
        "R%d or ashift R%d by R%d" % (rn, rx, ry),
        _op_or,
    )
    return (
        rn,
        value,
        "arithmetic-shift-or",
        _astatx_shift(or_amount, shifted, "forget"),
    )


# PRM p.23-13 (RN = exp RX, SHIFTOP 10000000; RN = exp RX (ex), 10000100):
# the exponent is the two's complement of (leading sign bits of RX - 1),
# i.e. 1 - (the count of leading bits equal to bit 31, bit 31 included), a
# value from -31 (RX = 0 or -1) to 0; the result is written to the 8-bit
# SHF8 field and sign-extended to 32 bits (PGR p.3-26 / PRM Figure 3-4 text:
# "the shifter places these results in the SHF8 field or the bit6 field and
# sign-extends the results to 32 bits"). The (EX) form, for the result of an
# ALU operation, writes +1 instead when AV is set (the lost overflow bit)
# and takes its SS from bit 31 XOR AV. Flags: SZ = exponent is zero, SV
# cleared, SS = bit 31 (EX: bit 31 XOR AV).
def _exp_impl(rn, rx, left, values, extended: bool) -> tuple:
    label = "exp R%d%s" % (rx, " (ex)" if extended else "")
    overflow: bool | None = False
    if extended:
        overflow = _astatx_known_bit(_ureg_raw(values, UREG_CODES["ASTATX"]), AV_BIT)
    value: Operand
    sign: bool | None
    if not isinstance(left, Const):
        value = Unknown(label)
        sign = None
    else:
        negative = bool(left.value & 0x80000000)
        magnitude = (left.value ^ 0xFFFFFFFF) if negative else left.value
        leading = 32 - magnitude.bit_length()
        sign = negative
        if overflow is None:
            value = Unknown(label)
        elif overflow:
            value = Const(1)
            sign = not negative
        else:
            value = Const((1 - leading) & 0xFFFFFFFF)
    if overflow is None:
        sign = None
    updates = _flags_from_pairs(
        (
            (SS_BIT, sign),
            (SV_BIT, False),
            (SZ_BIT, (value.value == 0) if isinstance(value, Const) else None),
        )
    )
    return rn, value, "exp-ex" if extended else "exp", updates


def shift_exp(rn, rx, ry, left, right, values, special) -> tuple:
    return _exp_impl(rn, rx, left, values, False)


def shift_exp_ex(rn, rx, ry, left, right, values, special) -> tuple:
    return _exp_impl(rn, rx, left, values, True)


# PRM pp.23-15 to 23-16 (FN = funpack RX) / PGR p.11-85, SHIFTOP 10010100:
# converts the 16-bit short float in the low 16 bits of RX (sign bit 15,
# 4-bit exponent bits 14:11, 11-bit fraction bits 10:0) to an IEEE single in
# FN. For exponent 1..15 the result exponent is the exponent's MSB, then four
# copies of the complement of that MSB, then its three LSBs, and the
# fraction is padded with 12 zeros. For exponent 0 the number is a denormal:
# the result exponent is 120 - N, N being the count of leading zeros of the
# 11-bit fraction, and the fraction is shifted left past its leading 1 (the
# "hidden" 1 is stripped) and padded. The sign is copied in every case.
# NOT IN THE MANUAL: an all-zero exponent and fraction is taken as signed
# zero (the N rule has no leading 1 to find). Flags SS, SZ, SV cleared.
def shift_funpack(rn, rx, ry, left, right, values, special) -> tuple:
    value: Operand
    if not isinstance(left, Const):
        value = Unknown("funpack R%d" % rx)
    else:
        packed = left.value & 0xFFFF
        sign = packed >> 15
        exponent = (packed >> 11) & 0xF
        fraction = packed & 0x7FF
        if exponent != 0:
            top = exponent >> 3
            single_exp = (top << 7) | (0 if top else 0x78) | (exponent & 7)
            mantissa = fraction << 12
        elif fraction == 0:
            single_exp = 0
            mantissa = 0
        else:
            zeros = 11 - fraction.bit_length()
            single_exp = 120 - zeros
            mantissa = ((fraction << (zeros + 1)) & 0x7FF) << 12
        value = Const((sign << 31) | (single_exp << 23) | mantissa)
    updates = _flags_from_pairs(((SS_BIT, False), (SZ_BIT, False), (SV_BIT, False)))
    return rn, value, "funpack", updates


# Register-form bit FIFO operations. BITDEP RX by RY (SHIFTOP 01110100, PRM
# pp.23-16/23-17) and RN = BITEXT RX [(NU)] (01010000 / 01011000, PRM
# pp.23-17 to 23-19) take the bit count from "the lower 6 bits of Ry"
# (BITDEP) or "of Rx" (BITEXT), with a count over 32 prohibited (SV); the
# FIFO arithmetic is _bitdep/_bitext, shared with the immediate forms. An
# unknown count undefines the FIFO, the pointer and the result.
def _bit_fifo_unknown(rn: int, label: str, writes_rn: bool) -> ComputeResult:
    unknown = Unknown(label)
    updates = _flags_from_pairs(
        (
            (SS_BIT, False),
            (SV_BIT, None),
            (SZ_BIT, None if writes_rn else False),
            (SF_BIT, None),
        )
    )
    if writes_rn:
        return (
            (rn, "BFFWRP", "BFF_HI", "BFF_LO"),
            (unknown, unknown, unknown, unknown),
            "bit-extract",
            updates,
        )
    return (
        ("BFFWRP", "BFF_HI", "BFF_LO"),
        (unknown, unknown, unknown),
        "bit-deposit",
        updates,
    )


def shift_bitdep(rn, rx, ry, left, right, values, special) -> tuple:
    if not isinstance(right, Const):
        return _bit_fifo_unknown(rn, "bitdep R%d by R%d" % (rx, ry), False)
    return _bitdep(left, right.value & 0x3F, special)


def shift_bitext(rn, rx, ry, left, right, values, special) -> tuple:
    if not isinstance(left, Const):
        return _bit_fifo_unknown(rn, "bitext R%d" % rx, True)
    return _bitext(rn, left.value & 0x3F, False, special)


def shift_bitext_nu(rn, rx, ry, left, right, values, special) -> tuple:
    if not isinstance(left, Const):
        return _bit_fifo_unknown(rn, "bitext R%d (nu)" % rx, True)
    return _bitext(rn, left.value & 0x3F, True, special)


SHIFT_OPS: dict[int, Handler] = {
    0x00: shift_logical,
    0x04: shift_arithmetic,
    0x08: shift_rotate,
    0x20: shift_logical_or,
    0x24: shift_arithmetic_or,
    0x40: shift_fext,
    0x44: shift_fdep,
    0x48: shift_fext_se,
    0x4C: shift_fdep_se,
    0x50: shift_bitext,
    0x58: shift_bitext_nu,
    0x64: shift_fdep_or,
    0x6C: shift_fdep_or_se,
    0x74: shift_bitdep,
    0x80: shift_exp,
    0x84: shift_exp_ex,
    0x94: shift_funpack,
    0x88: shift_leftz,
    0x8C: shift_lefto,
    0xC0: shift_bset,
    0xC4: shift_bclr,
    0xC8: shift_btgl,
    0xCC: shift_btst,
    0x70: shift_bffwrp_read,
    0x7C: shift_bffwrp_write,
    # Undocumented; kept in this same table (not a separate one) so cu=2
    # dispatch stays a single lookup -- see each handler's own docstring.
    0xB0: shift_undocumented_b0,
    0x14: shift_undocumented_14,
}
