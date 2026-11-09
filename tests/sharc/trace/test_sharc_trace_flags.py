"""Symbolic tracer: ASTATx flags and predicates."""

import unittest
from importlib import import_module

T = import_module("tools.sharc_dev.sharc_trace")
Instruction = import_module("tools.sharc_dev.sharc_disasm").Instruction
L = import_module("tools.sharc_dev.sharcldr")

_floats = import_module(
    "tools.sharc_dev.sharc_core.floats"
)  # _double_to_fixed isn't re-exported by T

from support.sharc_trace_build import (  # noqa: E402
    full_compute,
    shiftimm_fields,
    short_compute,
)


class AstatxFlagsTest(unittest.TestCase):
    """Focused tests for each op's ASTATX flags and the LT/GE/LE/GT/etc.
    condition predicates, from the verified per-instruction PRM table
    (docs/findings/-style citations inline)."""

    def astatx_after(self, fields, short, values, old_astatx, special=None):
        rn, value, operation, update = T._compute(fields, short, values, special)
        return rn, value, operation, update(old_astatx)

    # -- add/subtract/increment/decrement (PRM pp.439-440, 446-447) --------

    def test_add_flags_plain_no_carry_no_overflow(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x01, 0, 1, 2),
            False,
            {1: T.Const(5), 2: T.Const(7)},
            T.Unknown("start"),
        )
        self.assertEqual(op, "add")
        self.assertEqual(value, T.Const(12))
        self.assertEqual(astatx, T.PartialConst(T.ALU_FLAGS_MASK, 0))

    def test_add_flags_carry_and_zero(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x01, 0, 1, 2),
            False,
            {1: T.Const(0xFFFFFFFF), 2: T.Const(1)},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0))
        self.assertEqual(
            astatx, T.PartialConst(T.ALU_FLAGS_MASK, (1 << T.AZ_BIT) | (1 << T.AC_BIT))
        )

    def test_add_flags_signed_overflow(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x01, 0, 1, 2),
            False,
            {1: T.Const(0x7FFFFFFF), 2: T.Const(1), T.UREG_CODES["MODE1"]: T.Const(0)},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0x80000000))
        self.assertEqual(
            astatx, T.PartialConst(T.ALU_FLAGS_MASK, (1 << T.AV_BIT) | (1 << T.AN_BIT))
        )

    def test_subtract_flags_negative_result_no_carry(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x02, 0, 1, 2),
            False,
            {1: T.Const(5), 2: T.Const(7)},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0xFFFFFFFE))
        self.assertEqual(astatx, T.PartialConst(T.ALU_FLAGS_MASK, 1 << T.AN_BIT))

    def test_subtract_flags_equal_sets_az_and_ac(self):
        _, _, _, astatx = self.astatx_after(
            full_compute(0, 0x02, 0, 1, 2),
            False,
            {1: T.Const(7), 2: T.Const(7)},
            T.Unknown("start"),
        )
        self.assertEqual(
            astatx, T.PartialConst(T.ALU_FLAGS_MASK, (1 << T.AZ_BIT) | (1 << T.AC_BIT))
        )

    def test_subtract_same_register_folds_to_zero_even_when_unknown(self):
        # PRM Table 17-5 (p.425) ALUOP 00000010 = RN = RX - RY: RX - RY is
        # architecturally exactly 0 whenever RX and RY are the same register
        # read at the same instant (e.g. the SHARC+ "Rn = Rn - Rn" self-clear
        # idiom), no matter what value that register held -- even one this
        # tracer cannot otherwise pin down. Before the fix, subtracting an
        # Unknown from itself stayed Unknown, which let an uninitialized
        # register poison every later comparison against it.
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x02, 0, 15, 15),
            False,
            {15: T.Unknown("uninitialized R15")},
            T.Unknown("start"),
        )
        self.assertEqual(op, "subtract")
        self.assertEqual(value, T.Const(0))
        self.assertEqual(
            astatx, T.PartialConst(T.ALU_FLAGS_MASK, (1 << T.AZ_BIT) | (1 << T.AC_BIT))
        )

    def test_short_subtract_same_register_folds_to_zero_even_when_unknown(self):
        # Same self-clear idiom through the short-compute table (opcode 1 =
        # subtract, RN and RX both encoding register 3 here).
        rn, value, op, _ = T._compute(
            short_compute(1, 3, 3), True, {3: T.Unknown("uninitialized R3")}
        )
        self.assertEqual(rn, 3)
        self.assertEqual(op, "subtract")
        self.assertEqual(value, T.Const(0))

    def test_increment_matches_add_by_one_flags(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x29, 0, 1, 0),
            False,
            {1: T.Const(0x7FFFFFFF), T.UREG_CODES["MODE1"]: T.Const(0)},
            T.Unknown("start"),
        )
        self.assertEqual(op, "increment")
        self.assertEqual(value, T.Const(0x80000000))
        self.assertEqual(
            astatx, T.PartialConst(T.ALU_FLAGS_MASK, (1 << T.AV_BIT) | (1 << T.AN_BIT))
        )

    def test_decrement_is_rx_minus_1_flags(self):
        # RX=0: 0 - 1 = -1, AN set, no carry (borrow), no AZ.
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x2A, 0, 1, 0), False, {1: T.Const(0)}, T.Unknown("start")
        )
        self.assertEqual(op, "decrement")
        self.assertEqual(value, T.Const(0xFFFFFFFF))
        self.assertEqual(astatx, T.PartialConst(T.ALU_FLAGS_MASK, 1 << T.AN_BIT))
        # RX=1: 1 - 1 = 0, AZ and AC (no borrow) set.
        _, _, _, astatx = self.astatx_after(
            full_compute(0, 0x2A, 0, 1, 0), False, {1: T.Const(1)}, T.Unknown("start")
        )
        self.assertEqual(
            astatx, T.PartialConst(T.ALU_FLAGS_MASK, (1 << T.AZ_BIT) | (1 << T.AC_BIT))
        )

    def test_arith_flags_unknown_when_operand_not_const(self):
        old = T.Const(0xFFFFFFFF)
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x01, 0, 1, 2),
            False,
            {1: T.symbol("x"), 2: T.Const(1)},
            old,
        )
        self.assertIsInstance(value, T.Affine)
        # Only the ALU-flags bits are forgotten; everything else in OLD
        # (here, all 1s) stays known at its old value.
        kept = 0xFFFFFFFF & ~T.ALU_FLAGS_MASK
        self.assertEqual(astatx, T.PartialConst(kept, kept))

    # -- pass/not/and/or/xor (PRM pp.449-452) --------------------------------

    def test_pass_not_and_or_xor_flags_from_result(self):
        cases = (
            (0x21, 1, 2, {1: T.Const(0)}, T.Const(0), 1 << T.AZ_BIT),  # pass
            (0x40, 1, 2, {1: T.Const(5), 2: T.Const(3)}, T.Const(1), 0),  # and
            (
                0x41,
                1,
                2,
                {1: T.Const(0x80000000), 2: T.Const(0)},
                T.Const(0x80000000),
                1 << T.AN_BIT,
            ),  # or
            (
                0x42,
                1,
                2,
                {1: T.Const(5), 2: T.Const(5)},
                T.Const(0),
                1 << T.AZ_BIT,
            ),  # xor
        )
        for opcode, rx, ry, values, expected_value, expected_bits in cases:
            with self.subTest(opcode=hex(opcode)):
                _, value, _, astatx = self.astatx_after(
                    full_compute(0, opcode, 0, rx, ry),
                    False,
                    values,
                    T.Const(0xFFFFFFFF),
                )
                self.assertEqual(value, expected_value)
                self.assertEqual(
                    astatx,
                    T.Const((0xFFFFFFFF & ~T.ALU_FLAGS_MASK) | expected_bits),
                )

    def test_not_flags_unknown_operand_forgets_only_alu_bits(self):
        old = T.Const(0)  # every bit known, all clear
        _, value, op, astatx = self.astatx_after(
            short_compute(4, 0, 1), True, {1: T.Unknown("uninit")}, old
        )
        self.assertEqual(op, "not")
        self.assertIsInstance(value, T.Unknown)
        self.assertEqual(astatx, T.PartialConst(0xFFFFFFFF & ~T.ALU_FLAGS_MASK, 0))
        self.assertIsNone(T._astatx_known_bit(astatx, T.AZ_BIT))
        self.assertEqual(T._astatx_known_bit(astatx, T.MN_BIT), False)

    # -- comp/compu (PRM pp.18-5,18-6; CACC shift) ---------------------------

    def test_compare_signed_sets_an_when_less_and_clears_af(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x0A, 0, 1, 2),
            False,
            {1: T.Const(5), 2: T.Const(7)},
            T.Const(0xFFFFFFFF),
        )
        self.assertEqual(op, "compare")
        preserve = 0x00FFFFC0 & ~(1 << T.AF_BIT)
        cacc = (0xFFFFFFFF >> 1) & 0x7F000000  # CACC (bits 31:24) shifts in from old
        expected = (0xFFFFFFFF & preserve) | cacc | (1 << T.AN_BIT)
        self.assertEqual(astatx, T.Const(expected))
        self.assertFalse(bool(expected & (1 << T.AF_BIT)))

    def test_compare_cacc_shifts_and_new_msb_enters_bit31(self):
        # Old CACC (bits 31:24) = 0b10101010; compu(7,5) -> x>y -> new MSB=1.
        old = T.Const(0b10101010 << 24)
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x0B, 0, 1, 2), False, {1: T.Const(7), 2: T.Const(5)}, old
        )
        expected_cacc = (0b10101010 >> 1) | 0b10000000
        self.assertEqual((astatx.value >> 24) & 0xFF, expected_cacc)

    def test_compare_partial_when_old_astatx_not_fully_known(self):
        # Old ASTATX unknown entirely: AZ/AN/AC/AV/AS/AI/AF become known (the
        # compare's own bits), but CACC cannot be shifted without the old
        # CACC bits, so it stays unknown.
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x0A, 0, 1, 2),
            False,
            {1: T.Const(5), 2: T.Const(7)},
            T.Unknown("start"),
        )
        self.assertEqual(astatx, T.PartialConst(T.ALU_FLAGS_MASK, 1 << T.AN_BIT))
        self.assertIsNone(T._astatx_known_bit(astatx, 31))

    # -- multiplier ops (PRM p.493 for mr-data-move) -------------------------

    def test_multiply_family_computes_multiplier_flags_when_concrete(self):
        # 3 * 4 = 12: with both operands concretely known, MN/MV/MU/MI are
        # now computed from the real 80-bit result (PRM p.28-5/28-6), not
        # forgotten -- 12 is positive (MN=0), fits a 32-bit signed integer
        # (MV=0), integer results never underflow (MU=0), and MI is always
        # 0 for a fixed-point row.
        old = T.Const(0xFFFFFFFF)
        for opcode, cu in ((0x70, 1),):
            _, _, op, astatx = self.astatx_after(
                full_compute(cu, opcode, 0, 1, 2),
                False,
                {1: T.Const(3), 2: T.Const(4)},
                old,
            )
            self.assertEqual(op, "multiply")
            for bit in (T.MN_BIT, T.MV_BIT, T.MU_BIT, T.MI_BIT):
                self.assertEqual(T._astatx_known_bit(astatx, bit), False)

    def test_multiply_family_forgets_multiplier_flags_when_symbolic(self):
        _, _, op, astatx = self.astatx_after(
            full_compute(1, 0x70, 0, 1, 2),
            False,
            {1: T.symbol("track_index"), 2: T.Const(4)},
            T.Const(0xFFFFFFFF),
        )
        self.assertEqual(op, "multiply")
        # MN/MV/MU are data-dependent and forgotten here (no concrete
        # result); MI stays known=0 even so -- PRM Table 3-7 fixes MI at 0
        # for every fixed-point row regardless of the operands, unlike
        # MN/MV/MU.
        expected_mask = 0xFFFFFFFF & ~(
            (1 << T.MN_BIT) | (1 << T.MV_BIT) | (1 << T.MU_BIT)
        )
        self.assertEqual(
            astatx, T.PartialConst(expected_mask, expected_mask & ~(1 << T.MI_BIT))
        )

    def test_mr_data_move_clears_multiplier_flags(self):
        field = (0b100000 << 17) | (1 << 16) | (0 << 12) | (3 << 8)
        fields = {"compute[22:16]": field >> 16, "compute[15:0]": field & 0xFFFF}
        _, _, op, astatx = self.astatx_after(
            fields, False, {3: T.Const(5)}, T.Const(0xFFFFFFFF)
        )
        self.assertEqual(op, "mr-data-move-mr0f")
        self.assertEqual(astatx, T.Const(0xFFFFFFFF & ~T.MULT_FLAGS_MASK))

    # -- leftz (PRM p.521) ----------------------------------------------------

    def test_leftz_flags(self):
        cases = (
            (0, 32, False, True),
            (1, 31, False, False),
            (0x80000000, 0, True, False),
        )
        for rx_value, expected_result, expected_sz, expected_sv in cases:
            with self.subTest(rx=hex(rx_value)):
                _, value, op, astatx = self.astatx_after(
                    full_compute(2, 0x88, 0, 1, 0),
                    False,
                    {1: T.Const(rx_value)},
                    T.Unknown("start"),
                )
                self.assertEqual(op, "leftz")
                self.assertEqual(value, T.Const(expected_result))
                self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)
                self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), expected_sz)
                self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), expected_sv)

    # -- btst reg (PRM p.513) -------------------------------------------------

    def test_btst_flags_tested_bit_and_out_of_range(self):
        # Bit 0 of 0b1 is 1 -> SZ cleared.
        _, _, op, astatx = self.astatx_after(
            full_compute(2, 0xCC, 0, 1, 2),
            False,
            {1: T.Const(0b1), 2: T.Const(0)},
            T.Unknown("start"),
        )
        self.assertEqual(op, "bit-test")
        self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), False)

        # Bit 1 of 0b1 is 0 -> SZ set.
        _, _, _, astatx = self.astatx_after(
            full_compute(2, 0xCC, 0, 1, 2),
            False,
            {1: T.Const(0b1), 2: T.Const(1)},
            T.Unknown("start"),
        )
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), True)

        # Position 32 is out of range -> SZ and SV both set.
        _, _, _, astatx = self.astatx_after(
            full_compute(2, 0xCC, 0, 1, 2),
            False,
            {1: T.Const(0), 2: T.Const(32)},
            T.Unknown("start"),
        )
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), True)

    def test_btst_sv_known_even_when_source_unknown(self):
        # SV only needs the position, so it is known even when the tested
        # register is not; SZ needs the source too, so it stays unknown.
        _, _, _, astatx = self.astatx_after(
            full_compute(2, 0xCC, 0, 1, 2),
            False,
            {1: T.Unknown("uninit"), 2: T.Const(3)},
            T.Unknown("start"),
        )
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), False)
        self.assertIsNone(T._astatx_known_bit(astatx, T.SZ_BIT))

    # -- bset/bclr/btgl reg and immediate (PRM pp.511-513) -------------------

    def test_bit_field_reg_flags(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(2, 0xC0, 0, 1, 2),
            False,
            {1: T.Const(0), 2: T.Const(0)},
            T.Unknown("start"),
        )
        self.assertEqual(op, "bit-set")
        self.assertEqual(value, T.Const(1))
        self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), False)

        # Position > 31: value passes through unchanged, SV set, SZ from the
        # (unchanged) value.
        _, value, _, astatx = self.astatx_after(
            full_compute(2, 0xC4, 0, 1, 2),
            False,
            {1: T.Const(0), 2: T.Const(99)},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0))
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), True)

    def test_bit_set_clear_toggle_immediate_flags(self):
        # bset RX by 0 -> output nonzero, SZ cleared, SV cleared (0 <= 31).
        rn, value, op, astatx4 = T._shift_immediate(
            shiftimm_fields(0x30, 0, 0, 1), {1: T.Const(0)}
        )
        update = astatx4
        astatx = update(T.Unknown("start"))
        self.assertEqual(op, "bit-set-immediate")
        self.assertEqual(value, T.Const(1))
        self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), False)

        # bclr position 40 (> 31): passthrough, SV set.
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x31, 40, 0, 1), {1: T.Const(0xFF)}
        )
        astatx = update(T.Unknown("start"))
        self.assertEqual(value, T.Const(0xFF))
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), True)

    # -- fext immediate (PRM pp.518-519) -------------------------------------

    def test_fext_immediate_sv_set_when_span_exceeds_32(self):
        # position=30, length=8 (encoded via dataex/data8) -> span=38 > 32.
        position, length = 30, 8
        data8 = ((length & 0x3) << 6) | position
        dataex = (length >> 2) & 0xF
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x10, data8, 0, 1, dataex), {1: T.Const(0xFFFFFFFF)}
        )
        astatx = update(T.Unknown("start"))
        self.assertEqual(op, "field-extract-immediate")
        self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), False)

    def test_fext_immediate_sv_clear_when_span_within_32(self):
        position, length = 0, 8
        data8 = ((length & 0x3) << 6) | position
        dataex = (length >> 2) & 0xF
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x10, data8, 0, 1, dataex), {1: T.Const(0)}
        )
        astatx = update(T.Unknown("start"))
        self.assertEqual(value, T.Const(0))
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), True)

    # -- lshift/ashift reg and immediate, OR-forms (PRM pp.508-510) ----------

    def test_logical_shift_reg_flags(self):
        # RY encodes amount=4 in its low byte; RX=1 -> shifted=16, SV set
        # (left shift), SZ from the shifted value (nonzero).
        rn, value, op, update = T._compute(
            full_compute(2, 0x00, 0, 1, 2), False, {1: T.Const(1), 2: T.Const(4)}
        )
        astatx = update(T.Unknown("start"))
        self.assertEqual(op, "logical-shift")
        self.assertEqual(value, T.Const(16))
        self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SV_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), False)

    def test_shift_immediate_or_forms_sz_uses_pre_or_value(self):
        # RN already has bit0 set; OR-lshift RX=0b10 by 1 -> shifted=0b100
        # (nonzero) even though it is OR'd into a nonzero RN. SZ must reflect
        # the shifted value alone, per the PRM text.
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x08, 1, 0, 1), {0: T.Const(1), 1: T.Const(0b10)}
        )
        astatx = update(T.Unknown("start"))
        self.assertEqual(op, "logical-shift-or-immediate")
        self.assertEqual(value, T.Const(0b101))  # 1 | (0b10 << 1)
        self.assertEqual(T._astatx_known_bit(astatx, T.SZ_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)

    def test_or_ashift_immediate_leaves_ss_unknown(self):
        # PRM p.510: OR-ashift's ASTATx/y block omits the SS line (its
        # OR-lshift sibling repeats "SS Cleared"); model that gap as SS
        # becoming unknown rather than guessing it is cleared.
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x09, 1, 0, 1), {0: T.Const(0), 1: T.Const(1)}
        )
        astatx = update(T.Const(0xFFFFFFFF))  # SS previously known (=1)
        self.assertEqual(op, "arithmetic-shift-or-immediate")
        self.assertIsNone(T._astatx_known_bit(astatx, T.SS_BIT))
        # SZ/SV are still defined for OR-ashift.
        self.assertIsNotNone(T._astatx_known_bit(astatx, T.SZ_BIT))
        self.assertIsNotNone(T._astatx_known_bit(astatx, T.SV_BIT))

    def test_ashift_immediate_plain_still_clears_ss(self):
        rn, value, op, update = T._shift_immediate(
            shiftimm_fields(0x01, 1, 0, 1), {1: T.Const(1)}
        )
        astatx = update(T.Const(0xFFFFFFFF))
        self.assertEqual(op, "arithmetic-shift-immediate")
        self.assertEqual(T._astatx_known_bit(astatx, T.SS_BIT), False)


class PredicateTruthTableTest(unittest.TestCase):
    """LT/GE/LE/GT truth tables (PGR Table 4-37 p.4-93 / PRM p.4-53)."""

    def astatx_state(self, af, an, az, av=None):
        bits = 0
        mask = (1 << T.AF_BIT) | (1 << T.AN_BIT) | (1 << T.AZ_BIT)
        if af:
            bits |= 1 << T.AF_BIT
        if an:
            bits |= 1 << T.AN_BIT
        if az:
            bits |= 1 << T.AZ_BIT
        if av is not None:
            mask |= 1 << T.AV_BIT
            if av:
                bits |= 1 << T.AV_BIT
        return T.PartialConst(mask, bits)

    def test_af0_alusat0_truth_table(self):
        astatx_code = T.UREG_CODES["ASTATX"]
        mode1_code = T.UREG_CODES["MODE1"]
        # AF=0, ALUSAT=0: cross = AN xor AV. Enumerate AN, AV, AZ.
        for an in (False, True):
            for av in (False, True):
                for az in (False, True):
                    state = T.State(
                        0,
                        {
                            astatx_code: self.astatx_state(False, an, az, av),
                            mode1_code: T.Const(0),
                        },
                    )
                    cross = an != av
                    expect_lt = cross
                    expect_le = cross or az
                    with self.subTest(an=an, av=av, az=az):
                        self.assertEqual(T._predicate(state, 0x01), expect_lt)
                        self.assertEqual(T._predicate(state, 0x11), not expect_lt)
                        self.assertEqual(T._predicate(state, 0x02), expect_le)
                        self.assertEqual(T._predicate(state, 0x12), not expect_le)

    def test_af0_alusat1_flips_the_av_term(self):
        astatx_code = T.UREG_CODES["ASTATX"]
        mode1_code = T.UREG_CODES["MODE1"]
        state = T.State(
            0,
            {
                astatx_code: self.astatx_state(False, an=False, az=False, av=True),
                mode1_code: T.Const(1 << T.ALUSAT_BIT),
            },
        )
        # AV and not ALUSAT = False (ALUSAT set) -> cross = AN xor False = AN = False.
        self.assertFalse(T._predicate(state, 0x01))  # LT
        self.assertTrue(T._predicate(state, 0x11))  # GE

    def test_af0_av_unknown_returns_none(self):
        # AV itself must be known to answer LT/GE/LE/GT at all.
        astatx_code = T.UREG_CODES["ASTATX"]
        state = T.State(
            0,
            {
                astatx_code: self.astatx_state(False, an=True, az=False, av=None),
                T.UREG_CODES["MODE1"]: T.Unknown("mode1"),
            },
        )
        self.assertIsNone(T._predicate(state, 0x01))

    def test_af0_av_false_does_not_need_alusat(self):
        # AV and not ALUSAT is 0 regardless of ALUSAT when AV=0, so MODE1
        # need not be known to resolve the predicate.
        astatx_code = T.UREG_CODES["ASTATX"]
        state = T.State(
            0,
            {
                astatx_code: self.astatx_state(False, an=True, az=False, av=False),
                T.UREG_CODES["MODE1"]: T.Unknown("mode1"),
            },
        )
        self.assertTrue(T._predicate(state, 0x01))  # LT: cross = AN xor 0 = True
        self.assertFalse(T._predicate(state, 0x11))  # GE

    def test_af1_truth_table(self):
        astatx_code = T.UREG_CODES["ASTATX"]
        for an in (False, True):
            for az in (False, True):
                state = T.State(0, {astatx_code: self.astatx_state(True, an, az)})
                expect_le = an or az
                expect_lt = an and not az
                with self.subTest(an=an, az=az):
                    self.assertEqual(T._predicate(state, 0x02), expect_le)
                    self.assertEqual(T._predicate(state, 0x12), not expect_le)
                    self.assertEqual(T._predicate(state, 0x01), expect_lt)
                    self.assertEqual(T._predicate(state, 0x11), not expect_lt)

    def test_unknown_bits_return_none(self):
        astatx_code = T.UREG_CODES["ASTATX"]
        state = T.State(0, {astatx_code: T.Unknown("uninitialized")})
        for cond in (0x01, 0x02, 0x11, 0x12):
            self.assertIsNone(T._predicate(state, cond))

    def test_simple_single_bit_conditions(self):
        astatx_code = T.UREG_CODES["ASTATX"]
        for cond, bit in T.SIMPLE_COND_BITS.items():
            bit_pos, negate = bit
            with self.subTest(cond=hex(cond)):
                state_true = T.State(
                    0, {astatx_code: T.PartialConst(1 << bit_pos, 1 << bit_pos)}
                )
                state_false = T.State(0, {astatx_code: T.PartialConst(1 << bit_pos, 0)})
                self.assertEqual(T._predicate(state_true, cond), not negate)
                self.assertEqual(T._predicate(state_false, cond), negate)


if __name__ == "__main__":
    unittest.main()
