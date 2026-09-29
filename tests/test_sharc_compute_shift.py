"""Tests for the shifter's bit-FIFO model (tools/sharc_core/compute_shift.py's
``_bff_words``/``_bff_extract``/``_bff_deposit`` and the ShiftImm opcode
0x14/0x16 BITEXT handler built on them).

Numeric cases are hand-derived from the PRM/PGR's documented pseudocode
(PRM p.3-18, out/refs/sharc-plus-prm/all.txt:3507-3511; PGR p.11-86/11-87/
11-90/11-91, pgr.txt:23287-23481) rather than lifted from the manuals'
own worked examples, which label bits with letters ("qwertyui...") instead
of giving actual numbers. Where a test's setup mirrors a manual listing's
*procedure* (out/refs/sharc-plus-prm/all.txt:3495-3521's Example of Header
Extraction: BFFWRP=0; BITDEP R10 by 32; R6 = BITEXT(6)) this is noted
inline, with concrete numbers standing in for the manual's letters and the
expected outputs verified independently of this file's own code (by hand,
in the docstring/comments below -- not by calling _bff_extract/_bff_deposit
to generate the "expected" value, which would test nothing).

Kept separate from tests/test_sharc_trace.py and tests/test_sharc_trace_forms.py
(which cover the ShiftImm opcode dispatch table itself) so this lane's
edits do not collide with other agents' concurrent edits to those files.
"""

import os
import sys
import unittest
from importlib import import_module

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools"))
T = import_module("sharc_trace")
compute_shift = import_module("sharc_core.compute_shift")


def shiftimm_fields(opcode, data8, rn, rx, dataex=0):
    """A ShiftImm field dict for T._shift_immediate (matches the helper of
    the same name in tests/test_sharc_trace.py)."""
    field = (opcode << 16) | (data8 << 8) | (rn << 4) | rx
    return {
        "shiftimm[22:16]": field >> 16,
        "shiftimm[15:0]": field & 0xFFFF,
        "dataex[3:0]": dataex,
    }


def bitext_fields(opcode, bitlen12, rn, rx=0):
    """A ShiftImm field dict encoding BITLEN12 the way opcode 0x14/0x16
    split it: dataex[3:0] holds bits [11:8], data8 holds bits [7:0]."""
    return shiftimm_fields(opcode, bitlen12 & 0xFF, rn, rx, (bitlen12 >> 8) & 0xF)


# ---------------------------------------------------------------------------
# _bff_extract: the 64-bit-int model shared by BITEXT and (unreached)
# BITDEP.
# ---------------------------------------------------------------------------


class BffExtractTest(unittest.TestCase):
    def test_extract_within_top_word(self):
        # hi = 0xF0000000 (top nibble set), lo = 0: the documented pseudo-
        # code (PGR p.11-91) is "Rn = FEXT BFF[63:32] BY <32-bitlen>:
        # <bitlen>" -- by hand, extracting the top 4 bits of 0xF0000000
        # gives the top nibble itself, 0xF, right-justified.
        extracted, new_hi, new_lo = compute_shift._bff_extract(
            T.Const(0xF0000000), T.Const(0), 4
        )
        self.assertEqual(extracted, T.Const(0xF))
        # Step 2 (BFF <<= bitlen) shifts the whole 64-bit register left by
        # 4: the extracted nibble falls off the top, and no lower bits
        # exist to replace it, so both halves become 0.
        self.assertEqual(new_hi, T.Const(0))
        self.assertEqual(new_lo, T.Const(0))

    def test_extract_zero_bits_is_a_no_op(self):
        # BITLEN=0 is always well-defined (PGR's own pseudocode reduces to
        # a no-op FEXT/shift), even when the FIFO content itself is not --
        # this must not manufacture an Unknown out of a trivially-known 0.
        extracted, new_hi, new_lo = compute_shift._bff_extract(
            T.Unknown("x"), T.Unknown("y"), 0
        )
        self.assertEqual(extracted, T.Const(0))
        self.assertEqual(new_hi, T.Unknown("x"))
        self.assertEqual(new_lo, T.Unknown("y"))

    def test_extract_unknown_fifo_is_unknown(self):
        extracted, new_hi, new_lo = compute_shift._bff_extract(
            T.Unknown("uninitialized bit FIFO"), T.Const(0), 6
        )
        self.assertIsInstance(extracted, T.Unknown)
        self.assertIsInstance(new_hi, T.Unknown)
        self.assertIsInstance(new_lo, T.Unknown)

    def test_extract_carries_low_word_bits_into_high_word(self):
        # hi = 0x00000001 (a single valid bit at the very bottom of the
        # top word -- i.e. the word is otherwise full), lo = 0x80000000
        # (the low word's own top bit, next in line once the top word
        # empties). This is PRM/PGR's own "BFF = BFF << bitlen" step
        # crossing the hi/lo boundary (all.txt:3507's "64-bit register",
        # not two independent 32-bit ones).
        #
        # By hand: combined = hi:lo = 0x0000000180000000 (64 bits). The
        # top 1 bit (bit 63) is 0, so BITEXT(1) extracts 0. Shifting the
        # 64-bit register left by 1 moves lo's bit 31 (the only set bit in
        # lo) up into hi's bit 0, and hi's own bit 0 (already 1) up into
        # bit 1 -- new hi = 0b11 = 3, new lo = 0.
        extracted, new_hi, new_lo = compute_shift._bff_extract(
            T.Const(1), T.Const(0x80000000), 1
        )
        self.assertEqual(extracted, T.Const(0))
        self.assertEqual(new_hi, T.Const(3))
        self.assertEqual(new_lo, T.Const(0))

    def test_extract_matches_header_extraction_listing(self):
        # out/refs/sharc-plus-prm/all.txt:3495-3521, "Example of Header
        # Extraction": BFFWRP = 0x0; BITDEP R10 by 32; R6 = BITEXT(6). The
        # manual labels R10's bits with letters; substitute a concrete
        # value, R10 = 0x12345678, and hand-verify BITEXT(6)'s result two
        # independent ways.
        #
        # After "BITDEP R10 by 32" into an empty FIFO, hi = R10 = 0x12345678
        # exactly (see BffDepositTest.test_deposit_into_empty_fifo_matches_
        # listing below) and lo = 0.
        #
        # Hand check 1 (bit string): 0x12345678 = 0001 0010 0011 0100 0101
        # 0110 0111 1000. Its top 6 bits are 000100 = 4, so BITEXT(6) must
        # extract 4.
        #
        # Hand check 2 (arithmetic, independent of check 1): the shifted
        # high word is (hi << 6) & 0xFFFFFFFF = (0x12345678 * 64) mod
        # 2**32 = 19546873344 mod 4294967296 = 2367004160 = 0x8D159E00.
        extracted, new_hi, new_lo = compute_shift._bff_extract(
            T.Const(0x12345678), T.Const(0), 6
        )
        self.assertEqual(extracted, T.Const(4))
        self.assertEqual(new_hi, T.Const(0x8D159E00))
        self.assertEqual(new_lo, T.Const(0))


# ---------------------------------------------------------------------------
# _bff_deposit: BITDEP's own pseudocode, unreachable through decode in this
# ISA (see the function's docstring) but exercised directly here.
# ---------------------------------------------------------------------------


class BffDepositTest(unittest.TestCase):
    def test_deposit_into_empty_fifo_matches_listing(self):
        # out/refs/sharc-plus-prm/all.txt:3495-3521: "BFFWRP = 0x0;
        # ... BITDEP R10 by 32". Depositing a full 32-bit word into an
        # empty FIFO (wrp=0) must land it exactly in the high word: PGR's
        # pseudocode position is 64-(wrp+bitlen) = 64-(0+32) = 32, i.e. the
        # deposited field starts exactly at the hi/lo boundary.
        new_hi, new_lo = compute_shift._bff_deposit(
            T.Const(0), T.Const(0), 0, T.Const(0x12345678), 32
        )
        self.assertEqual(new_hi, T.Const(0x12345678))
        self.assertEqual(new_lo, T.Const(0))

    def test_deposit_packs_below_existing_content(self):
        # A non-empty FIFO (wrp=4, top nibble of hi already holds 0b1010)
        # depositing 4 more bits (0b0110) packs them immediately below the
        # existing content: position = 64-(4+4) = 56, i.e. bits [59:56] of
        # the 64-bit register, which is bits [27:24] of hi.
        hi = 0b1010 << 28  # existing 4 valid bits, MSB-justified in hi
        new_hi, new_lo = compute_shift._bff_deposit(
            T.Const(hi), T.Const(0), 4, T.Const(0b0110), 4
        )
        self.assertEqual(new_hi, T.Const((0b1010 << 28) | (0b0110 << 24)))
        self.assertEqual(new_lo, T.Const(0))

    def test_deposit_overflow_is_undefined(self):
        # PGR p.11-87: "Attempts to append more bits than the bit FIFO has
        # room for results in an undefined bit FIFO and write pointer."
        new_hi, new_lo = compute_shift._bff_deposit(
            T.Const(0), T.Const(0), 60, T.Const(0xFF), 8
        )
        self.assertIsInstance(new_hi, T.Unknown)
        self.assertIsInstance(new_lo, T.Unknown)

    def test_deposit_zero_bits_is_a_no_op(self):
        new_hi, new_lo = compute_shift._bff_deposit(
            T.Unknown("x"), T.Unknown("y"), 5, T.Const(0xFF), 0
        )
        self.assertEqual(new_hi, T.Unknown("x"))
        self.assertEqual(new_lo, T.Unknown("y"))


# ---------------------------------------------------------------------------
# The wired-up ShiftImm opcode 0x14 (update) / 0x16 (NU) BITEXT handler.
# ---------------------------------------------------------------------------


class BitextOpcodeTest(unittest.TestCase):
    def test_bitext_update_extracts_and_advances_pointer(self):
        special = {
            "BFFWRP": T.Const(32),
            "BFF_HI": T.Const(0x12345678),
            "BFF_LO": T.Const(0),
        }
        rn, value, op, update = T._shift_immediate(
            bitext_fields(0x14, 6, rn=6), {}, special
        )
        self.assertEqual(op, "bit-extract")
        self.assertEqual(rn, (6, "BFFWRP", "BFF_HI", "BFF_LO"))
        self.assertEqual(
            value, (T.Const(4), T.Const(26), T.Const(0x8D159E00), T.Const(0))
        )
        astatx = update(T.Unknown("start"))
        self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), False)
        # Updated BFFWRP (26) < 32 -> SF clears.
        self.assertEqual(T._astatx_known_bit(astatx, T.SF_BIT), False)

    def test_bitext_nu_leaves_fifo_and_pointer_untouched(self):
        special = {
            "BFFWRP": T.Const(32),
            "BFF_HI": T.Const(0x12345678),
            "BFF_LO": T.Const(0),
        }
        rn, value, op, update = T._shift_immediate(
            bitext_fields(0x16, 6, rn=6), {}, special
        )
        self.assertEqual(op, "bit-extract-nu")
        self.assertEqual(rn, 6)
        self.assertEqual(value, T.Const(4))
        astatx = update(T.Unknown("start"))
        # SF reflects the *un-updated* pointer (32 >= 32 -> set), per PGR
        # p.11-91's "If NU modifier is used SF reflects the un-updated
        # Write pointer status" -- even though a real update would clear it
        # (26 < 32, as the update-variant test above shows).
        self.assertEqual(T._astatx_known_bit(astatx, T.SF_BIT), True)

    def test_bitext_over_32_undefines_rn_but_not_the_pointer(self):
        # PGR p.11-91's two error sentences have different scope: "A value
        # of more than 32 ... is prohibited and use of such a value sets
        # SV" says nothing about the pointer or FIFO (unlike the separate
        # "results in undefined pointer and bit FIFO" sentence for the
        # underflow case below) -- and step 1's FEXT genuinely cannot
        # return more than 32 bits from a single word, while step 2/3's
        # pointer/FIFO bookkeeping has no such limit. So RN is Unknown, but
        # BFFWRP still decrements mechanically: 64 - 40 = 24.
        special = {"BFFWRP": T.Const(64)}
        rn, value, op, update = T._shift_immediate(
            bitext_fields(0x14, 40, rn=6), {}, special
        )
        self.assertEqual(value[0], T.Unknown("bitext: undefined (bitlen 40 > 32)"))
        self.assertEqual(value[1], T.Const(24))
        astatx = update(T.Unknown("start"))
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.SF_BIT), False)  # 24 < 32

    def test_bitext_over_32_forces_unknown_even_when_fifo_is_known(self):
        # Correction (2026-09-28): the "sightings" below were ShiftImm 0x19,
        # which is Rn = Rn OR FDEP, not BITEXT (NU) (PGR Table 12-11; see
        # compute_shift._shift_immediate). The over-32 rule tested here
        # still holds for a real BITEXT.
        # Lane H2 (2026-09-26): docs/findings/06's real-firmware BITEXT
        # sightings (BITLEN12 in {95, 384, 192, 256, 535}, at 0xb88fa4,
        # 0xb89a60, 0xb8c8ec, 0xb8c92a and one more) all hit this same
        # over_32 branch, whose "prohibited" wording (PGR p.11-91 and the
        # byte-identical SC58x/2158x PRM p.24-18) never documents a numeric
        # result. This lane checked, live, whether a smaller-than-BITLEN12
        # interpretation (mod 64, low 6 bits, clamp to 32, or a full 64-bit
        # extract-then-truncate) could recover one anyway; it cannot,
        # because this branch (the caller's own OVER_32 check) discards
        # EXTRACTED unconditionally BEFORE any length-dependent value would
        # ever reach RN -- proven here with a FULLY KNOWN FIFO (unlike this
        # file's own dt2-1.16 trace, where BFF_HI/BFF_LO are never known at
        # all: no BITDEP instruction executes anywhere in that image, so
        # _bff_words() always returns Unknown regardless of length there
        # too). RN stays Unknown even though the FIFO itself is fully
        # concrete and BITLEN12=40's own FEXT-based extraction would
        # otherwise be well defined (bits 63:56 of 0xFFFFFFFF00000000 are
        # all 1s -- see the sibling underflow tests above for the same
        # FIFO's arithmetic in the *legal* bitlen range). A future fix
        # needs a citable PRM/PGR value for this case, not a length
        # reinterpretation: this test pins today's behaviour so such a
        # change is a deliberate, visible edit.
        special = {
            "BFFWRP": T.Const(64),
            "BFF_HI": T.Const(0xFFFFFFFF),
            "BFF_LO": T.Const(0),
        }
        rn, value, op, update = T._shift_immediate(
            bitext_fields(0x14, 40, rn=6), {}, special
        )
        self.assertEqual(value[0], T.Unknown("bitext: undefined (bitlen 40 > 32)"))
        astatx = update(T.Unknown("start"))
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), True)

    def test_bitext_underflow_sets_sv_when_pointer_known(self):
        # PGR p.11-91: "Attempts to get more bits than those in the bit
        # FIFO results in undefined pointer and bit FIFO. SV is set in
        # that case" -- distinct from (and here, in isolation from) the
        # BITLEN12>32 case: bitlen=6 is legal in general, but exceeds a
        # BFFWRP of 4. Per the manual's own wording, only the *pointer and
        # FIFO* are declared undefined here -- RN is not mentioned, and the
        # FEXT-based extraction (step 1) is mechanically well defined
        # regardless of BFFWRP, so RN still comes out concrete: the top 6
        # bits of 0xF0000000 are 111100 = 0x3C = 60.
        special = {
            "BFFWRP": T.Const(4),
            "BFF_HI": T.Const(0xF0000000),
            "BFF_LO": T.Const(0),
        }
        rn, value, op, update = T._shift_immediate(
            bitext_fields(0x14, 6, rn=6), {}, special
        )
        self.assertEqual(value[0], T.Const(0x3C))
        self.assertIsInstance(value[1], T.Unknown)  # BFFWRP undefined
        self.assertIsInstance(value[2], T.Unknown)  # and the FIFO itself
        self.assertIsInstance(value[3], T.Unknown)
        astatx = update(T.Unknown("start"))
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), True)

    def test_bitext_underflow_unknown_when_pointer_unknown(self):
        # No BFFWRP tracked at all (never written in this trace): SV must
        # stay unknown, not silently False -- this is the bug the old
        # "SV_BIT: bitlen12 > 32" formula had (it ignored this case
        # entirely). The extracted value is still computed mechanically,
        # since BITEXT's FEXT step never reads BFFWRP.
        special = {"BFF_HI": T.Const(0xF0000000), "BFF_LO": T.Const(0)}
        rn, value, op, update = T._shift_immediate(
            bitext_fields(0x14, 4, rn=6), {}, special
        )
        self.assertEqual(value[0], T.Const(0xF))
        astatx = update(T.Unknown("start"))
        self.assertIsNone(T._astatx_known_bit(astatx, T.SV_BIT))
        # BFFWRP was never known, so it stays Unknown ("uninitialized"),
        # not merely "undefined" -- these are different reasons but both
        # collapse to Unknown.
        self.assertIsInstance(value[1], T.Unknown)

    def test_bitext_absent_special_value_unknown_sv_now_honest(self):
        # The common case in dt2-1.16: no BITDEP ever runs and BFFWRP is
        # never written, so special is empty for this opcode. RN (and, on
        # the update variant, BFFWRP/the FIFO) stay Unknown, matching the
        # pre-existing default -- but SV changes from this file's old,
        # always-False formula (which only ever checked bitlen12 > 32) to
        # None here, since with no BFFWRP tracked this tracer genuinely
        # cannot know whether PGR p.11-91's "more bits than those in the
        # bit FIFO" condition holds. That correction is the point of this
        # test, not an implementation detail.
        rn, value, op, update = T._shift_immediate(
            bitext_fields(0x14, 6, rn=6), {}, None
        )
        self.assertIsInstance(value[0], T.Unknown)
        astatx = update(T.Unknown("start"))
        self.assertIsNone(T._astatx_known_bit(astatx, T.SV_BIT))
        self.assertIsNone(T._astatx_known_bit(astatx, T.SF_BIT))


# ---------------------------------------------------------------------------
# ShiftImm opcode 0x12 (Rn = FEXT Rx BY bit6:len6 (SE)): the fix for the
# "reference bug" (fext ... (se) did not mask the extracted field before
# reading/extending its sign, so bits of the source above the field leaked
# into the result). PGR p.11-78 (out/refs/adsp-2136x_2137x_214xx_pgr_rev2.4/
# all.txt:23036-23052): "Rn = FEXT Rx BY <bit6>:<len6> (SE) ... The MSBs of
# Rn are sign-extended by the MSB of the extracted field"; PRM p.3-17
# (out/refs/sharc-plus-prm/all.txt:3486): the (SE) option "sign extends the
# left bits" of the field FEXT would otherwise clear. Vectors below are the
# ones the bug report recorded from dt2-1.16 sw 0x1c2607's block (function
# 0x1c24e9), whose every fext-se there has pos=0, len=16 (confirmed via
# tools/sharc.py against out/sharcdb/dt2-1.16.sqlite).
# ---------------------------------------------------------------------------


class FextSeOpcodeTest(unittest.TestCase):
    def test_positive_field_low_bit_unaffected(self):
        # R2 = 0x00012345, pos=0, len=16: field = 0x2345, MSB (bit 15) is 0
        # -> no sign extension, matching plain FEXT (opcode 0x10) on the
        # same field. Before the fix this leaked bit 16 (0x10000) of the
        # source, returning 0x12345 instead.
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x12, data8=0, rn=0, rx=2, dataex=4),
            {2: T.Const(0x00012345)},
        )
        self.assertEqual(rn, 0)
        self.assertEqual(value, T.Const(0x2345))
        self.assertEqual(op, "field-extract-immediate-se")

    def test_negative_field_sign_extends_to_32_bits(self):
        # R2 = 0x00018000, pos=0, len=16: field = 0x8000, MSB set -> sign
        # extend to 0xffff8000. Before the fix this returned 0x8000
        # unextended (the field's own bits were right, but nothing above
        # bit 15 was filled in).
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x12, data8=0, rn=0, rx=2, dataex=4),
            {2: T.Const(0x00018000)},
        )
        self.assertEqual(value, T.Const(0xFFFF8000))

    def test_source_bits_above_the_field_never_leak(self):
        # R2 = 0xabcd7fff, pos=0, len=16: field = 0x7fff, MSB clear -> the
        # field itself, with every bit of 0xabcd0000 above it discarded.
        # Before the fix, source.value (0xabcd7fff) was passed to _signed
        # unmasked; its bit 15 happened to be 0 so the sign check still
        # passed, but the result kept all of 0xabcd7fff instead of masking
        # down to the 16-bit field.
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x12, data8=0, rn=0, rx=2, dataex=4),
            {2: T.Const(0xABCD7FFF)},
        )
        self.assertEqual(value, T.Const(0x7FFF))

    def test_field_narrower_than_16_still_masks_and_sign_extends(self):
        # A position/length not seen in dt2-1.16 but legal per the manual:
        # R1 = fext-se(R3, pos=4, len=4). Source = 0x000005F0 -> bits 7:4 =
        # 0xF (1111), MSB of the 4-bit field set -> sign-extends to
        # 0xFFFFFFFF. data8 bit layout: position = data8 & 0x3F = 4,
        # length = (dataex << 2) | (data8 >> 6) = 4 -> dataex=1, data8=4.
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x12, data8=4, rn=1, rx=3, dataex=1),
            {3: T.Const(0x000005F0)},
        )
        self.assertEqual(rn, 1)
        self.assertEqual(value, T.Const(0xFFFFFFFF))

    def test_length_32_uses_the_whole_word(self):
        # pos=0, len=32 (data8=0, dataex=8: length=(8<<2)|0=32): the field
        # is the entire 32-bit source, so there are no bits above it left to
        # sign-extend -- the 32-bit two's-complement value is unchanged
        # regardless of its sign bit.
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x12, data8=0, rn=0, rx=2, dataex=8),
            {2: T.Const(0x80000000)},
        )
        self.assertEqual(value, T.Const(0x80000000))

    def test_length_zero_is_zero(self):
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x12, data8=0, rn=0, rx=2, dataex=0),
            {2: T.Const(0xFFFFFFFF)},
        )
        self.assertEqual(value, T.Const(0))

    def test_unknown_source_stays_unknown(self):
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x12, data8=0, rn=0, rx=2, dataex=4),
            {},
        )
        self.assertIsInstance(value, T.Unknown)


if __name__ == "__main__":
    unittest.main()
