"""Tests for the ALU (cu=0x0) and shifter (cu=0x2) full-compute opcodes,
the MR data move generalization, two MUL/ALU multifunction categories, the
MUL dual add/subtract multifunction form, and the bit-FIFO ShiftImm
opcodes added to tools/sharc_trace.py -- everything this worktree's slice
of the coverage gap covers except multiplier (cu=0x1) opcodes.

Each case is worked from the public-manual definition cited in the
implementation (out/refs/adsp-2136x_2137x_214xx_pgr_rev2.4 and
out/refs/sharc-plus-prm), independently of tools/sharc_trace.py's own
arithmetic, so a broken implementation cannot pass by construction.
"""

import os
import struct
import sys
import unittest
from importlib import import_module

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools"))
T = import_module("sharc_trace")
Instruction = import_module("sharc_disasm").Instruction


def f32(value):
    """The IEEE-754 single-precision bit pattern for a Python float."""
    return struct.unpack("<I", struct.pack("<f", value))[0]


def full_compute(cu, opcode, rn, rx, ry):
    """A full-compute field dict for _compute(f, short=False, ...)."""
    field = (cu << 20) | (opcode << 12) | (rn << 8) | (rx << 4) | ry
    return {"compute[22:16]": field >> 16, "compute[15:0]": field & 0xFFFF}


def mulalu_fields(category, rm, ra, rxm, rym, rxa, rya):
    """PRM Figure 18-1 mf bit (p.423) + Table 18-16/18-17 (p.434): mf=1,
    opcode[21:16]=CATEGORY, then Rm[15:12]/Ra[11:8]/Rxm[7:6]/Rym[5:4]/
    Rxa[3:2]/Rya[1:0]."""
    field = (
        (1 << 22)
        | (category << 16)
        | (rm << 12)
        | (ra << 8)
        | (rxm << 6)
        | (rym << 4)
        | (rxa << 2)
        | rya
    )
    return {"compute[22:16]": field >> 16, "compute[15:0]": field & 0xFFFF}


def mrdatamove_fields(direction, opcode, rn):
    """PRM Table 18-29 MRDATAMOVE (p.438): fixed bits 22:17=100000, then
    D-bit[16], opcode[15:12], RN[11:8]."""
    field = (0b100000 << 17) | (direction << 16) | (opcode << 12) | (rn << 8)
    return {"compute[22:16]": field >> 16, "compute[15:0]": field & 0xFFFF}


def shiftimm_fields(opcode, data8, rn, rx, dataex=0):
    """A ShiftImm field dict for _shift_immediate."""
    field = (opcode << 16) | (data8 << 8) | (rn << 4) | rx
    return {
        "shiftimm[22:16]": field >> 16,
        "shiftimm[15:0]": field & 0xFFFF,
        "dataex[3:0]": dataex,
    }


def insn(name, fields, length=4, kind="confident"):
    return Instruction(0, length, name, fields, kind=kind)


class ComputeHelperMixin:
    def astatx_after(self, fields, short, values, old_astatx, special=None):
        rn, value, operation, update = T._compute(fields, short, values, special)
        return rn, value, operation, update(old_astatx)

    def shiftimm_astatx_after(self, fields, values, old_astatx, special=None):
        rn, value, operation, update = T._shift_immediate(fields, values, special)
        return rn, value, operation, update(old_astatx)


# --- ALU (cu=0x0) full-compute opcodes --------------------------------------


class AluOpcodesTest(ComputeHelperMixin, unittest.TestCase):
    """PGR pp.11-9/11-19/11-27 (pgr.txt:20570-21235), PRM Table 18-5."""

    # -- RN = RX + ci / RX + ci - 1 (PGR p.11-9/11-10, opcodes 0x25/0x26) ----

    def test_add_with_carry_no_y_plain(self):
        # RX=5, carry-in=1 -> 6, no flags set.
        values = {1: T.Const(5), T.UREG_CODES["ASTATX"]: T.Const(1 << T.AC_BIT)}
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x25, 0, 1, 0), False, values, T.Unknown("start")
        )
        self.assertEqual(op, "add-with-carry")
        self.assertEqual(value, T.Const(6))
        self.assertEqual(astatx, T.PartialConst(T.ALU_FLAGS_MASK, 0))

    def test_add_with_carry_no_y_carry_out_and_zero(self):
        # RX=0xFFFFFFFF, carry-in=1 -> wraps to 0: AZ and AC set.
        values = {
            1: T.Const(0xFFFFFFFF),
            T.UREG_CODES["ASTATX"]: T.Const(1 << T.AC_BIT),
        }
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x25, 0, 1, 0), False, values, T.Unknown("start")
        )
        self.assertEqual(value, T.Const(0))
        self.assertEqual(
            astatx, T.PartialConst(T.ALU_FLAGS_MASK, (1 << T.AZ_BIT) | (1 << T.AC_BIT))
        )

    def test_subtract_with_borrow_no_y(self):
        # RX=5, borrow (carry-in=0) -> RX + 0 - 1 = 4, AC set (no further
        # borrow), matching the RY-form's documented convention.
        values = {1: T.Const(5), T.UREG_CODES["ASTATX"]: T.Const(0)}
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x26, 0, 1, 0), False, values, T.Unknown("start")
        )
        self.assertEqual(op, "subtract-with-borrow")
        self.assertEqual(value, T.Const(4))
        self.assertEqual(astatx, T.PartialConst(T.ALU_FLAGS_MASK, 1 << T.AC_BIT))

    def test_add_with_carry_no_y_unknown_carry_forgets_flags(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x25, 0, 1, 0),
            False,
            {1: T.Const(5)},  # ASTATX not supplied -> carry-in unknown
            T.Const(0xFFFFFFFF),
        )
        self.assertIsInstance(value, T.Unknown)
        kept = 0xFFFFFFFF & ~T.ALU_FLAGS_MASK
        self.assertEqual(astatx, T.PartialConst(kept, kept))

    # -- RN = ABS RX (PGR p.11-13/11-14, opcode 0x30) ------------------------

    def test_abs_positive_passthrough(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x30, 0, 1, 0), False, {1: T.Const(5)}, T.Unknown("start")
        )
        self.assertEqual(op, "abs")
        self.assertEqual(value, T.Const(5))
        self.assertEqual(astatx, T.PartialConst(T.ALU_FLAGS_MASK, 0))

    def test_abs_negative_negates_and_sets_as(self):
        # RX = -5 (0xFFFFFFFB) -> 5; AS set (input was negative).
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x30, 0, 1, 0),
            False,
            {1: T.Const(0xFFFFFFFB)},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(5))
        self.assertEqual(astatx, T.PartialConst(T.ALU_FLAGS_MASK, 1 << T.AS_BIT))

    def test_abs_int_min_overflows_and_wraps(self):
        # PGR: "The ABS of the minimum negative number causes an overflow."
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x30, 0, 1, 0),
            False,
            {1: T.Const(0x80000000)},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0x80000000))
        expected_bits = (1 << T.AV_BIT) | (1 << T.AN_BIT) | (1 << T.AS_BIT)
        self.assertEqual(astatx, T.PartialConst(T.ALU_FLAGS_MASK, expected_bits))

    # -- RN = NOT RX (PGR p.11-19, opcode 0x43) ------------------------------

    def test_not_flags_from_result(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x43, 0, 1, 0),
            False,
            {1: T.Const(0x0F0F0F0F)},
            T.Unknown("start"),
        )
        self.assertEqual(op, "not")
        self.assertEqual(value, T.Const(0xF0F0F0F0))
        self.assertEqual(astatx, T.PartialConst(T.ALU_FLAGS_MASK, 1 << T.AN_BIT))

    def test_not_all_ones_is_zero(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x43, 0, 1, 0),
            False,
            {1: T.Const(0xFFFFFFFF)},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0))
        self.assertEqual(astatx, T.PartialConst(T.ALU_FLAGS_MASK, 1 << T.AZ_BIT))

    # -- RN = LOGB FX (PGR p.11-36, opcode 0xC1) -----------------------------

    def test_logb_normal_exponent(self):
        mode1 = T.UREG_CODES["MODE1"]
        values = {1: T.Const(f32(2.0)), mode1: T.Const(0)}
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0xC1, 0, 1, 0), False, values, T.Unknown("start")
        )
        self.assertEqual(op, "logb")
        self.assertEqual(value, T.Const(1))
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AF_BIT), True)

    def test_logb_nan_returns_all_ones_and_sets_ai(self):
        mode1 = T.UREG_CODES["MODE1"]
        values = {1: T.Const(0x7FC00000), mode1: T.Const(0)}  # quiet NaN
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0xC1, 0, 1, 0), False, values, T.Unknown("start")
        )
        self.assertEqual(value, T.Const(0xFFFFFFFF))
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)

    def test_logb_zero_unsaturated_returns_float_negative_infinity(self):
        mode1 = T.UREG_CODES["MODE1"]
        values = {1: T.Const(0), mode1: T.Const(0)}  # ALUSAT clear
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0xC1, 0, 1, 0), False, values, T.Unknown("start")
        )
        self.assertEqual(value, T.Const(0xFF800000))
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), True)

    def test_logb_zero_saturated_returns_min_fixed(self):
        mode1 = T.UREG_CODES["MODE1"]
        values = {1: T.Const(0), mode1: T.Const(1 << T.ALUSAT_BIT)}
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0xC1, 0, 1, 0), False, values, T.Unknown("start")
        )
        self.assertEqual(value, T.Const(0x80000000))
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), True)

    # -- FN = ABS(FX - FY) (PGR p.11-27, opcode 0x92) ------------------------

    def test_float_abs_subtract_negative_diff_is_abs(self):
        values = {1: T.Const(f32(3.0)), 2: T.Const(f32(5.0))}
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x92, 0, 1, 2), False, values, T.Unknown("start")
        )
        self.assertEqual(op, "float-abs-subtract")
        self.assertEqual(value, T.Const(f32(2.0)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)

    def test_float_abs_subtract_nan_sets_ai(self):
        nan = 0x7FC00000
        values = {1: T.Const(nan), 2: T.Const(f32(1.0))}
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x92, 0, 1, 2), False, values, T.Unknown("start")
        )
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        # abs() clears whatever sign bit float-subtract's NaN quirk set.
        self.assertEqual(value.value & 0x80000000, 0)


# --- Shifter (cu=0x2) full-compute opcodes ----------------------------------


class ShifterOpcodesTest(ComputeHelperMixin, unittest.TestCase):
    """PRM Table 17-9 / PGR Table 12-11 (pgr.txt:22540-22965)."""

    def test_or_lshift_register_ors_shifted_into_rn(self):
        # RN already 1, RX=0b10 shifted left by 2 (RY low byte=2) -> 0b1000;
        # OR'd with 1 -> 0b1001.
        values = {0: T.Const(1), 1: T.Const(0b10), 2: T.Const(2)}
        _, value, op, astatx = self.astatx_after(
            full_compute(2, 0x20, 0, 1, 2), False, values, T.Unknown("start")
        )
        self.assertEqual(op, "logical-shift-or")
        self.assertEqual(value, T.Const(0b1001))
        self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), True)  # left shift
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), False)  # shifted != 0

    def test_or_lshift_register_sz_is_pre_or_shifted_value(self):
        # Shift amount 0 -> shifted value equals RX itself (0) even though
        # RN is nonzero before the OR; SZ must reflect the shifted value.
        values = {0: T.Const(5), 1: T.Const(0), 2: T.Const(0)}
        _, value, _, astatx = self.astatx_after(
            full_compute(2, 0x20, 0, 1, 2), False, values, T.Unknown("start")
        )
        self.assertEqual(value, T.Const(5))
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), True)

    def test_lefto_counts_leading_ones(self):
        # (RX, count, SZ): SZ is set when the MSB of RX is 0; SV when count == 32.
        cases = (
            (0xFFFFFFFF, 32, False),
            (0x7FFFFFFF, 0, True),
            (0xF0000000, 4, False),
        )
        for rx_value, expected_result, expected_sz in cases:
            with self.subTest(rx=hex(rx_value)):
                _, value, op, astatx = self.astatx_after(
                    full_compute(2, 0x8C, 0, 1, 0),
                    False,
                    {1: T.Const(rx_value)},
                    T.Unknown("start"),
                )
                self.assertEqual(op, "lefto")
                self.assertEqual(value, T.Const(expected_result))
                self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)
                self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), expected_sz)
                self.assertEqual(
                    T._astatx_known_bit(astatx, T.SV_BIT), expected_result == 32
                )

    def test_lefto_sz_set_when_msb_clear(self):
        _, _, _, astatx = self.astatx_after(
            full_compute(2, 0x8C, 0, 1, 0),
            False,
            {1: T.Const(0x7FFFFFFF)},
            T.Unknown("start"),
        )
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), True)

    def test_lefto_sz_clear_when_msb_set(self):
        _, _, _, astatx = self.astatx_after(
            full_compute(2, 0x8C, 0, 1, 0),
            False,
            {1: T.Const(0xFFFFFFFF)},
            T.Unknown("start"),
        )
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), False)

    def test_bffwrp_read_reflects_special_dict(self):
        rn, value, op, astatx = self.astatx_after(
            full_compute(2, 0x70, 5, 1, 0),
            False,
            {},
            T.Const(0xFFFFFFFF),
            special={"BFFWRP": T.Const(40)},
        )
        self.assertEqual(op, "bffwrp-read")
        self.assertEqual(rn, 5)
        self.assertEqual(value, T.Const(40))
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), False)

    def test_bffwrp_read_uninitialized_is_unknown(self):
        _, value, _, _ = self.astatx_after(
            full_compute(2, 0x70, 5, 1, 0), False, {}, T.Unknown("start")
        )
        self.assertIsInstance(value, T.Unknown)

    def test_bffwrp_write_register_form(self):
        # RN-positioned field is the SOURCE here (unusual for this opcode),
        # value=0x85 -> masked to 7 bits = 5 -> SV clear (<=64), SF clear (<32).
        rn, value, op, astatx = self.astatx_after(
            full_compute(2, 0x7C, 0, 1, 0),
            False,
            {0: T.Const(0x85)},
            T.Unknown("start"),
        )
        self.assertEqual(op, "bffwrp-write")
        self.assertEqual(rn, "BFFWRP")
        self.assertEqual(value, T.Const(5))
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SF_BIT), False)

    def test_bffwrp_write_register_form_overflow_and_half_full(self):
        # 0xFF & 0x7F = 127: > 64 (SV set), >= 32 (SF set).
        _, value, _, astatx = self.astatx_after(
            full_compute(2, 0x7C, 0, 1, 0),
            False,
            {0: T.Const(0xFF)},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(127))
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.SF_BIT), True)

    def test_undocumented_opcode_0x14_decodes_without_raising(self):
        # No public source documents this opcode; it must still decode (so
        # the walk does not desync) with an Unknown result and forgotten
        # shifter flags, matching the file's existing 0xb0 precedent.
        rn, value, op, astatx = self.astatx_after(
            full_compute(2, 0x14, 3, 7, 14), False, {}, T.Const(0xFFFFFFFF)
        )
        self.assertEqual(rn, 3)
        self.assertIsInstance(value, T.Unknown)
        self.assertIsNone(T._astatx_known_bit(astatx, T.SV_BIT))

    def test_reserved_cu3_decodes_without_raising(self):
        # PRM: cu=11 ("cu=3") is reserved, not used by SINGLEFN -- decoded
        # rather than guessed at, same rationale as the 0x14 case above.
        rn, value, op, astatx = self.astatx_after(
            full_compute(3, 0xD6, 9, 15, 15), False, {}, T.Const(0xFFFFFFFF)
        )
        self.assertEqual(rn, 9)
        self.assertIsInstance(value, T.Unknown)
        self.assertEqual(op, "compute-reserved-cu3")


# --- MR data move (PRM Table 18-29) -----------------------------------------


class MrDataMoveTest(ComputeHelperMixin, unittest.TestCase):
    def test_write_mr1f_returns_mrf_key(self):
        # MR1F/MR0F/MR2F are all words of the same 80-bit MRF accumulator
        # (PRM p.3-10), so a data move to any of them returns "MRF" -- the
        # specific word touched is in the operation name instead.
        rn, value, op, _ = T._compute(
            mrdatamove_fields(1, 1, 3), False, {3: T.Const(0x1234)}, None
        )
        self.assertEqual(rn, "MRF")
        self.assertIsInstance(value, T.MR)
        self.assertEqual(T._mr_read_word(value, 1), T.Const(0x1234))
        # PRM p.3-11: a write to MR1F also sign-extends into MR2F; 0x1234's
        # bit 31 is 0, so MR2F becomes 0. MR0F was never written.
        self.assertEqual(T._mr_read_word(value, 2), T.Const(0))
        self.assertIsInstance(T._mr_read_word(value, 0), T.Unknown)
        self.assertEqual(op, "mr-data-move-mr1f")

    def test_read_mr2b_from_special_dict(self):
        mrb = T._mr_write_word(T.Unknown("uninitialized MRB"), 2, T.Const(0x77))
        rn, value, op, _ = T._compute(
            mrdatamove_fields(0, 6, 5), False, {}, {"MRB": mrb}
        )
        self.assertEqual(rn, 5)
        self.assertEqual(value, T.Const(0x77))
        self.assertEqual(op, "mr-data-move-mr2b")

    def test_read_uninitialized_register_is_unknown(self):
        rn, value, _, _ = T._compute(mrdatamove_fields(0, 4, 5), False, {}, None)
        self.assertEqual(rn, 5)
        self.assertIsInstance(value, T.Unknown)

    def test_mult_flags_cleared_for_every_register(self):
        _, _, _, astatx = self.astatx_after(
            mrdatamove_fields(1, 2, 3),  # MR2F
            False,
            {3: T.Const(5)},
            T.Const(0xFFFFFFFF),
        )
        self.assertEqual(astatx, T.Const(0xFFFFFFFF & ~T.MULT_FLAGS_MASK))

    def test_mr0f_writes_the_shared_mrf_accumulator_key(self):
        # PRM p.3-10: REGF_MRF "is comprised of" MR2F/MR1F/MR0F -- MR0F is
        # the low 32 bits of the same 80-bit accumulator the
        # multiply-accumulate rows call "MRF", so a data move to/from MR0F
        # must land in state.special["MRF"] (as a partially known MR, since
        # this alone says nothing about MR1F/MR2F), not a separate "MR0F"
        # slot.
        state = T.State(0x10, {2: T.Const(0x55)})
        result = T._compute(
            mrdatamove_fields(1, 0, 2), False, dict(state.uregs), state.special
        )
        T._apply_compute(state, insn("2a", {}), result)
        mrf = state.special.get("MRF")
        self.assertIsInstance(mrf, T.MR)
        self.assertFalse(mrf.known)
        self.assertEqual(T._mr_read_word(mrf, 0), T.Const(0x55))
        self.assertNotIn("MR0F", state.special)

    def test_mr1f_also_writes_the_shared_mrf_accumulator_key(self):
        state = T.State(0x10, {2: T.Const(0x66)})
        result = T._compute(
            mrdatamove_fields(1, 1, 2),
            False,
            dict(state.uregs),
            state.special,  # MR1F
        )
        T._apply_compute(state, insn("2a", {}), result)
        mrf = state.special.get("MRF")
        self.assertIsInstance(mrf, T.MR)
        self.assertEqual(T._mr_read_word(mrf, 1), T.Const(0x66))
        self.assertNotIn("MR1F", state.special)


# --- MUL/ALU multifunction categories 0x1c (average) / 0x1d (abs) ----------


class MultifnMulAluTest(ComputeHelperMixin, unittest.TestCase):
    def test_average_category(self):
        # FM = 2.0*3.0 = 6.0; FA = (5.0+3.0)/2 = 4.0.
        values = {
            0: T.Const(f32(2.0)),
            4: T.Const(f32(3.0)),
            8: T.Const(f32(5.0)),
            12: T.Const(f32(3.0)),
        }
        rn, value, op, astatx = self.astatx_after(
            mulalu_fields(0x1C, 1, 2, 0, 0, 0, 0), False, values, T.Unknown("start")
        )
        self.assertEqual(op, "float-mulalu-average")
        self.assertEqual(rn, (1, 2))
        self.assertEqual(value, (T.Const(f32(6.0)), T.Const(f32(4.0))))
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)

    def test_abs_category_carries_input_sign_and_clears_an(self):
        values = {
            0: T.Const(f32(2.0)),
            4: T.Const(f32(3.0)),
            8: T.Const(f32(-5.0)),
            12: T.Const(f32(0.0)),
        }
        rn, value, op, astatx = self.astatx_after(
            mulalu_fields(0x1D, 1, 2, 0, 0, 0, 0), False, values, T.Unknown("start")
        )
        self.assertEqual(op, "float-mulalu-abs")
        self.assertEqual(value, (T.Const(f32(6.0)), T.Const(f32(5.0))))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AS_BIT), True)


# --- MUL dual add/subtract multifunction (category top 2 bits = 0b11) -----


class MultifnDualAddSubtractTest(ComputeHelperMixin, unittest.TestCase):
    def test_three_way_result_fm_fa_fs(self):
        # category = 0x30 | rs; rs=3 here (category=0x33).
        values = {
            0: T.Const(f32(2.0)),
            4: T.Const(f32(3.0)),
            8: T.Const(f32(5.0)),
            12: T.Const(f32(2.0)),
        }
        rn, value, op, astatx = self.astatx_after(
            mulalu_fields(0x33, 1, 2, 0, 0, 0, 0), False, values, T.Unknown("start")
        )
        self.assertEqual(op, "float-mul-dual-add-subtract")
        self.assertEqual(rn, (1, 2, 3))
        self.assertEqual(
            value, (T.Const(f32(6.0)), T.Const(f32(7.0)), T.Const(f32(3.0)))
        )
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)

    def test_writes_all_three_registers_through_apply_compute(self):
        values = {
            0: T.Const(f32(2.0)),
            4: T.Const(f32(3.0)),
            8: T.Const(f32(5.0)),
            12: T.Const(f32(2.0)),
        }
        state = T.State(0x10, dict(values))
        result = T._compute(
            mulalu_fields(0x38, 1, 2, 0, 0, 0, 0), False, state.uregs, state.special
        )
        T._apply_compute(state, insn("2a", {}), result)
        self.assertEqual(state.uregs[1], T.Const(f32(6.0)))  # Rm
        self.assertEqual(state.uregs[2], T.Const(f32(7.0)))  # Ra
        # rs = category & 0xF = 0x38 & 0xF = 8, so Rs writes register 8,
        # overwriting the fxa input that was just read.
        self.assertEqual(state.uregs[8], T.Const(f32(3.0)))  # Rs


# --- ShiftImm bit-FIFO opcodes (PGR p.11-70/11-90, pgr.txt:22883-23400) ----


class BitFifoShiftImmTest(ComputeHelperMixin, unittest.TestCase):
    def test_or_fdep_se_matches_manual_worked_example(self):
        # position=13, length=14 packed via data8/dataex exactly like the
        # already-implemented FEXT/FEXT-SE opcodes.
        position, length = 13, 14
        data8 = ((length & 0x3) << 6) | position
        dataex = (length >> 2) & 0xF
        source = 0b11_1000_1100_1101  # 14 bits, sign bit (bit13) set
        rn, value, op, astatx = self.shiftimm_astatx_after(
            shiftimm_fields(0x1B, data8, 0, 1, dataex),
            {0: T.Const(0), 1: T.Const(source)},
            T.Unknown("start"),
        )
        self.assertEqual(op, "field-deposit-or-se")
        self.assertEqual(value, T.Const(0xFF19A000))
        self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), False)  # span 27 <= 32

    def test_or_fdep_se_ors_into_existing_destination(self):
        position, length = 4, 4
        data8 = ((length & 0x3) << 6) | position
        dataex = (length >> 2) & 0xF
        rn, value, _, _ = self.shiftimm_astatx_after(
            shiftimm_fields(0x1B, data8, 0, 1, dataex),
            {0: T.Const(1), 1: T.Const(0b1010)},  # sign bit (bit3) set
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0xFFFFFFA1))  # 1 | (sign-extended 0xa << 4)

    def test_or_fdep_se_sv_set_when_span_exceeds_32(self):
        position, length = 30, 10
        data8 = ((length & 0x3) << 6) | position
        dataex = (length >> 2) & 0xF
        _, _, _, astatx = self.shiftimm_astatx_after(
            shiftimm_fields(0x1B, data8, 0, 1, dataex),
            {0: T.Const(0), 1: T.Const(0)},
            T.Unknown("start"),
        )
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), True)

    def test_or_fdep_is_shiftimm_0x19(self):
        # PGR Table 12-11 (pp.12-10/12-11): Rn = Rn OR FDEP is shifter
        # opcode 0110 0100, ShiftImm = its upper six bits 011001. The PRM's
        # Table 17-9 6-bit column misprints it as bitext (nu). DT2 1.16 sw
        # 0xb88fe6 "R0 = R0 or fdep R2 by 23:8" puts an exponent at 23:8.
        position, length = 23, 8
        data8 = ((length & 0x3) << 6) | position
        dataex = (length >> 2) & 0xF
        rn, value, op, astatx = self.shiftimm_astatx_after(
            shiftimm_fields(0x19, data8, 0, 2, dataex),
            {0: T.Const(0x00400000), 2: T.Const(0x7F)},
            T.Unknown("start"),
        )
        self.assertEqual((rn, op), (0, "field-deposit-or"))
        self.assertEqual(value, T.Const(0x3FC00000))  # 1.5f
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), False)  # 31 <= 32

    def test_or_fdep_sign_bit_example(self):
        # sw 0xb88fa4 "R0 = R0 or fdep R12 by 31:1": the sign of a float.
        data8 = ((1 & 0x3) << 6) | 31
        rn, value, _, _ = self.shiftimm_astatx_after(
            shiftimm_fields(0x19, data8, 0, 12, 0),
            {0: T.Const(0x3F800000), 12: T.Const(1)},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0xBF800000))

    def test_bitdep_immediate_is_0x1d(self):
        # PGR Table 12-11: 0111 0100 BITDEP -> ShiftImm 011101 (not or fdep
        # (se)). PGR p.11-86/11-87: BFF |= FDEP Rx BY <64-(WRP+len)>:<len>,
        # WRP += len, SF = WRP >= 32. DT2 1.16 sw 0xb8863c/0xb8863f run
        # "BITDEP R2 by 32; BITDEP R1 by 32".
        special = {"BFFWRP": T.Const(0), "BFF_HI": T.Const(0), "BFF_LO": T.Const(0)}
        rn, value, op, astatx = self.shiftimm_astatx_after(
            shiftimm_fields(0x1D, 32, 0, 2, 0),
            {2: T.Const(0xABCD1234)},
            T.Unknown("start"),
            special=special,
        )
        self.assertEqual(op, "bit-deposit")
        self.assertEqual(rn, ("BFFWRP", "BFF_HI", "BFF_LO"))
        self.assertEqual(value, (T.Const(32), T.Const(0xABCD1234), T.Const(0)))
        self.assertEqual(T._astatx_known_bit(astatx, T.SF_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), False)

    def test_field_deposit_or_helper_without_sign_extension(self):
        # Direct check of the shared helper's non-SE branch (not wired to
        # any implemented opcode, but part of its documented contract):
        # sign bit clear -> no extension either way.
        self.assertEqual(
            T._field_deposit_or(T.Const(0), T.Const(0b0110), 4, 4, True, "x"),
            T.Const(0x60),
        )
        self.assertEqual(
            T._field_deposit_or(T.Const(0), T.Const(0b0110), 4, 4, False, "x"),
            T.Const(0x60),
        )

    def test_bffwrp_immediate_write(self):
        rn, value, op, astatx = self.shiftimm_astatx_after(
            shiftimm_fields(0x1F, 20, 0, 0), {0: T.Const(0)}, T.Unknown("start")
        )
        self.assertEqual(op, "bffwrp-write")
        self.assertEqual(rn, "BFFWRP")
        self.assertEqual(value, T.Const(20))
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SF_BIT), False)

    def test_bffwrp_immediate_write_overflow_and_half_full(self):
        # data8=200 -> masked to 7 bits = 72: SV set (>64), SF set (>=32).
        _, value, _, astatx = self.shiftimm_astatx_after(
            shiftimm_fields(0x1F, 200, 0, 0), {0: T.Const(0)}, T.Unknown("start")
        )
        self.assertEqual(value, T.Const(72))
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.SF_BIT), True)

    def test_bitext_updates_pointer_and_sets_sv_when_too_long(self):
        # bitlen12=40 > 32 -> SV set; BFFWRP 50 -> 50-40=10 -> SF clear.
        rn, value, op, astatx = self.shiftimm_astatx_after(
            shiftimm_fields(0x14, 40 & 0xFF, 3, 1, (40 >> 8) & 0xF),
            {1: T.Const(0)},
            T.Unknown("start"),
            special={"BFFWRP": T.Const(50)},
        )
        self.assertEqual(op, "bit-extract")
        self.assertEqual(rn, (3, "BFFWRP"))
        self.assertIsInstance(value[0], T.Unknown)  # FIFO content unmodeled
        self.assertEqual(value[1], T.Const(10))
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.SF_BIT), False)

    def test_bitext_sf_set_when_pointer_stays_half_full(self):
        rn, value, op, astatx = self.shiftimm_astatx_after(
            shiftimm_fields(0x14, 10, 3, 1, 0),
            {1: T.Const(0)},
            T.Unknown("start"),
            special={"BFFWRP": T.Const(40)},
        )
        self.assertEqual(value[1], T.Const(30))  # 40 - 10
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), False)  # 10 <= 32
        self.assertEqual(T._astatx_known_bit(astatx, T.SF_BIT), False)  # 30 < 32

    def test_bitext_pointer_unknown_when_uninitialized(self):
        rn, value, _, astatx = self.shiftimm_astatx_after(
            shiftimm_fields(0x14, 10, 3, 1, 0), {1: T.Const(0)}, T.Unknown("start")
        )
        self.assertIsInstance(value[1], T.Unknown)
        self.assertIsNone(T._astatx_known_bit(astatx, T.SF_BIT))

    def test_bitext_nu_does_not_touch_pointer_or_sf(self):
        # NU: no special-dict write at all (single-destination return), and
        # SF (previously known =1) must stay exactly as it was.
        rn, value, op, astatx = self.shiftimm_astatx_after(
            shiftimm_fields(0x16, 10, 3, 1, 0),
            {1: T.Const(0)},
            T.Const(1 << T.SF_BIT),
            special={"BFFWRP": T.Const(40)},
        )
        self.assertEqual(op, "bit-extract-nu")
        self.assertEqual(rn, 3)
        self.assertIsInstance(value, T.Unknown)
        self.assertEqual(T._astatx_known_bit(astatx, T.SF_BIT), True)  # unchanged

    def test_bitext_writes_pointer_through_apply_compute(self):
        state = T.State(0x10, {1: T.Const(0)}, special={"BFFWRP": T.Const(50)})
        result = T._shift_immediate(
            shiftimm_fields(0x14, 20, 3, 1, 0), dict(state.uregs), state.special
        )
        T._apply_compute(state, insn("6b_shiftimm", {}), result)
        self.assertEqual(state.special["BFFWRP"], T.Const(30))
        self.assertIsInstance(state.uregs[3], T.Unknown)


if __name__ == "__main__":
    unittest.main()
