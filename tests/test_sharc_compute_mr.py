"""Tests for the multiplier's 80-bit MR accumulator (tools/sharc_core/
state.py's ``MR``/``_mr_from_signed``/``_mr_read_word``/``_mr_write_word``,
and tools/sharc_core/compute_mult.py's ops built on it): manual-derived
numeric cases per operation (PRM Table 17-7/3-5/3-7, out/refs/
sharc-plus-prm/all.txt) -- overflow, saturation edges, rounding ties, MR2's
sign extension, fractional vs integer format, and SIMD PEy (MSF/MSB).

Kept separate from tests/test_sharc_trace_mult.py (which covers the opcode
dispatch table itself) so this lane's edits do not collide with other
agents' concurrent edits to that file.
"""

import os
import sys
import unittest
from importlib import import_module

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools"))
T = import_module("sharc_trace")
compute_mult = import_module("sharc_core.compute_mult")
Instruction = import_module("sharc_disasm").Instruction


def insn(name, fields, length=4, kind="confident"):
    return Instruction(0, length, name, fields, kind=kind)


def full_compute(cu, opcode, rn, rx, ry):
    """PRM Figure 18-1 (p.423): mf=0, cu[21:20], opcode[19:12], rn[11:8],
    rx[7:4], ry[3:0]."""
    field = (cu << 20) | (opcode << 12) | (rn << 8) | (rx << 4) | ry
    return {"compute[22:16]": field >> 16, "compute[15:0]": field & 0xFFFF}


def compute(fields, values, special=None):
    return T._compute(fields, False, values, special)


def compute_pey(fields, values, special=None):
    return T._compute_pey(fields, False, values, special)


# ---------------------------------------------------------------------------
# MR representation itself (state.py).
# ---------------------------------------------------------------------------


class MRRepresentationTest(unittest.TestCase):
    def test_from_signed_round_trips_positive_and_negative(self):
        self.assertEqual(T._mr_from_signed(0x1234).signed(), 0x1234)
        self.assertEqual(T._mr_from_signed(-30).signed(), -30)
        self.assertEqual(T._mr_from_signed(-(1 << 79)).signed(), -(1 << 79))

    def test_mr2_sign_extends_on_read_positive_and_negative(self):
        # PRM p.3-11: "When data is read from the REGF_MR2F register (guard
        # bits), it is sign-extended to 32 bits."
        positive = T._mr_from_signed(0x1234 << 64)  # MR2 = 0x1234
        self.assertEqual(T._mr_read_word(positive, 2), T.Const(0x1234))
        negative = T._mr_from_signed(-(1 << 79))  # MR2's top bit set
        self.assertEqual(T._mr_read_word(negative, 2), T.Const(0xFFFF8000))

    def test_mr0_write_is_not_sign_extended(self):
        # PRM p.3-11: "Data written to the REGF_MR0F register is not
        # sign-extended" -- MR1/MR2 stay exactly as they were.
        mr = T._mr_from_signed(0x1_0000_0000_0000_0000)  # MR1=0, MR2=1
        mr = T._mr_write_word(mr, 0, T.Const(0x80000000))
        self.assertEqual(T._mr_read_word(mr, 0), T.Const(0x80000000))
        self.assertEqual(T._mr_read_word(mr, 1), T.Const(0))
        self.assertEqual(T._mr_read_word(mr, 2), T.Const(1))

    def test_mr1_write_sign_extends_into_mr2(self):
        # PRM p.3-11: "Data written to the REGF_MR1F register is
        # sign-extended to REGF_MR2F, repeating the MSB of REGF_MR1F in the
        # 16 bits of the REGF_MR2F register."
        mr = T._mr_write_word(T.Unknown("start"), 1, T.Const(0x80000000))
        self.assertEqual(T._mr_read_word(mr, 1), T.Const(0x80000000))
        self.assertEqual(T._mr_read_word(mr, 2), T.Const(0xFFFFFFFF))
        mr = T._mr_write_word(T.Unknown("start"), 1, T.Const(0x40000000))
        self.assertEqual(T._mr_read_word(mr, 2), T.Const(0))

    def test_partial_mr_is_not_known_and_signed_is_none(self):
        mr = T._mr_write_word(T.Unknown("start"), 0, T.Const(1))
        self.assertFalse(mr.known)
        self.assertIsNone(mr.signed())
        self.assertEqual(T._mr_read_word(mr, 0), T.Const(1))
        self.assertIsInstance(T._mr_read_word(mr, 1), T.Unknown)

    def test_unknown_write_forgets_only_the_touched_word(self):
        mr = T._mr_from_signed(0x1_00000000_00000000)  # fully known
        cleared = T._mr_write_word(mr, 0, T.Unknown("uninitialized R3"))
        self.assertFalse(cleared.known)
        self.assertIsInstance(T._mr_read_word(cleared, 0), T.Unknown)
        self.assertEqual(T._mr_read_word(cleared, 1), T.Const(0))


# ---------------------------------------------------------------------------
# Plain multiply, no accumulate (PRM Table 17-7 "01yx f00r"/"F10r"/"F11r").
# ---------------------------------------------------------------------------


class PlainMultiplyTest(unittest.TestCase):
    def test_integer_overflow_sets_mv(self):
        # INT_MIN * -1 = 2**31, one past INT32_MAX: overflows the 32-bit
        # signed integer format (PRM p.28-6: "upper 49 bits of MR not all
        # zeros or all ones").
        rn, value, op, astatx_update = compute(
            full_compute(1, 0x70, 5, 1, 2),  # SSI
            {1: T.Const(0x80000000), 2: T.Const(0xFFFFFFFF)},
        )
        self.assertEqual(value, T.Const(0x80000000))  # low 32 bits, unaffected
        astatx = astatx_update(T.Const(0))
        self.assertEqual(T._astatx_known_bit(astatx, T.MV_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.MN_BIT), False)  # 2**31 > 0

    def test_unsigned_integer_no_overflow_within_range(self):
        rn, value, op, astatx_update = compute(
            full_compute(1, 0x40, 5, 1, 2),  # UUI
            {1: T.Const(100), 2: T.Const(200)},
        )
        self.assertEqual(value, T.Const(20000))
        astatx = astatx_update(T.Const(0))
        self.assertEqual(T._astatx_known_bit(astatx, T.MV_BIT), False)

    def test_signed_fractional_redundant_shift_overflow(self):
        # -1.0 * -1.0 in 1.31 signed fractional: the documented redundant-
        # sign left shift (PRM p.3-9) doubles the raw product to exactly
        # 2**63, one past the signed-fractional max (2**63-1) -- a classic
        # DSP fixed-point corner case, not a modelling bug.
        rn, value, op, astatx_update = compute(
            full_compute(1, 0x7C, 0, 1, 2),  # mrf = RX*RY (SSF)
            {1: T.Const(0x80000000), 2: T.Const(0x80000000)},
        )
        self.assertEqual(rn, "MRF")
        self.assertEqual(value.signed(), 1 << 63)
        astatx = astatx_update(T.Const(0))
        self.assertEqual(T._astatx_known_bit(astatx, T.MV_BIT), True)

    def test_unsigned_fractional_half_times_half(self):
        rn, value, op, _ = compute(
            full_compute(1, 0x48, 5, 1, 2),  # UUF, RN dest
            {1: T.Const(0x80000000), 2: T.Const(0x80000000)},
        )
        self.assertEqual(value, T.Const(0x40000000))  # 0.25 (PRM p.27-4)

    def test_symbolic_operand_is_unknown_not_a_crash(self):
        rn, value, op, astatx_update = compute(
            full_compute(1, 0x74, 0, 1, 2),  # mrf = RX*RY (SSI)
            {1: T.symbol("track_index"), 2: T.Const(4)},
        )
        self.assertEqual(rn, "MRF")
        self.assertIsInstance(value, T.Unknown)
        astatx = astatx_update(T.Const(0xFFFFFFFF))
        self.assertIsNone(T._astatx_known_bit(astatx, T.MV_BIT))
        # MI is architecturally fixed 0 even with a symbolic operand.
        self.assertEqual(T._astatx_known_bit(astatx, T.MI_BIT), False)


# ---------------------------------------------------------------------------
# Multiply-accumulate/subtract (PRM Table 17-7 "10yx.../11yx...").
# ---------------------------------------------------------------------------


class AccumulateTest(unittest.TestCase):
    def test_integer_register_result_needs_only_mr0(self):
        partial = T._mr_write_word(T.Unknown("reset"), 0, T.Const(0x269548))
        for opcode, expected in ((0xB0, 0x269570), (0xF0, 0x269520)):
            with self.subTest(opcode=opcode):
                rn, value, _, flags = compute(
                    full_compute(1, opcode, 7, 1, 2),
                    {1: T.Const(1), 2: T.Const(40)},
                    special={"MRF": partial},
                )
                self.assertEqual(rn, 7)
                self.assertEqual(value, T.Const(expected))
                astat = flags(T.Const(0xFFFFFFFF))
                self.assertIsNone(T._astatx_known_bit(astat, T.MV_BIT))
                self.assertIsNone(T._astatx_known_bit(astat, T.MN_BIT))
                self.assertEqual(T._astatx_known_bit(astat, T.MI_BIT), False)

    def test_partial_mr_integer_result_wraps_without_guessing_upper_words(self):
        partial = T._mr_write_word(T.Unknown("reset"), 0, T.Const(0xFFFFFFFE))
        _, value, _, _ = compute(
            full_compute(1, 0xB0, 7, 1, 2),
            {1: T.Const(2), 2: T.Const(3)},
            special={"MRF": partial},
        )
        self.assertEqual(value, T.Const(4))
        _, fractional, _, _ = compute(
            full_compute(1, 0xB8, 7, 1, 2),
            {1: T.Const(2), 2: T.Const(3)},
            special={"MRF": partial},
        )
        self.assertIsInstance(fractional, T.Unknown)
        dest, accumulator, _, _ = compute(
            full_compute(1, 0xB4, 7, 1, 2),
            {1: T.Const(2), 2: T.Const(3)},
            special={"MRF": partial},
        )
        self.assertEqual(dest, "MRF")
        self.assertEqual(T._mr_read_word(accumulator, 0), T.Const(4))
        self.assertIsInstance(T._mr_read_word(accumulator, 1), T.Unknown)

    def test_partial_mr_accumulate_then_integer_register_read(self):
        partial = T._mr_write_word(T.Unknown("reset"), 0, T.Const(0x269548))
        _, accumulated, _, _ = compute(
            full_compute(1, 0xB4, 0, 10, 2),
            {10: T.Const(4), 2: T.Const(80)},
            special={"MRF": partial},
        )
        _, pointer, _, _ = compute(
            full_compute(1, 0xB0, 7, 11, 6),
            {11: T.Const(0), 6: T.Const(40)},
            special={"MRF": accumulated},
        )
        self.assertEqual(pointer, T.Const(0x269688))
        self.assertFalse(accumulated.known)

    def test_accumulate_stays_exact_past_32_bits(self):
        # Sum the same 0.25 (1.63 fmt) fractional product eight times: a
        # 32-bit-truncating accumulator would lose the fractional
        # remainder after the first couple of additions, but the real
        # 80-bit MRF keeps the exact sum throughout (this is the whole
        # point of modelling the full accumulator).
        mrf = T._mr_from_signed(0)
        values = {1: T.Const(0x40000000), 2: T.Const(0x40000000)}  # 0.5 * 0.5
        for _i in range(8):
            rn, value, op, _ = compute(
                full_compute(1, 0xBC, 0, 1, 2), values, special={"MRF": mrf}
            )
            self.assertEqual(op, "multiply-accumulate")
            mrf = value
        self.assertEqual(mrf.signed(), 8 * (1 << 61))  # 8 * 0.25, exactly

    def test_subtract_row(self):
        rn, value, op, _ = compute(
            full_compute(1, 0xFC, 0, 1, 2),  # mrf = mrf - RX*RY (SSF)
            {1: T.Const(0x40000000), 2: T.Const(0x40000000)},  # 0.5 * 0.5
            special={"MRF": T._mr_from_signed(1 << 62)},  # 0.5
        )
        self.assertEqual(rn, "MRF")
        self.assertEqual(value.signed(), 1 << 61)  # 0.5 - 0.25 = 0.25
        self.assertEqual(op, "multiply-subtract")

    def test_uninitialized_accumulator_is_unknown(self):
        rn, value, op, _ = compute(
            full_compute(1, 0xBC, 0, 1, 2),
            {1: T.Const(1), 2: T.Const(1)},
            special={},
        )
        self.assertIsInstance(value, T.Unknown)

    def test_mrb_accumulate_is_independent_of_mrf(self):
        # opcode 0xB6: mrb = mrb + RX*RY (SSI) -- used here to cross-check
        # an mrb-destination accumulate (rather than re-deriving 0xB4/
        # 0xBC's own mrf-only rows) leaves any existing MRF value
        # untouched.
        rn, value, op, _ = compute(
            full_compute(1, 0xB6, 0, 1, 2),
            {1: T.Const(3), 2: T.Const(4)},
            special={"MRF": T._mr_from_signed(999), "MRB": T._mr_from_signed(1)},
        )
        self.assertEqual(rn, "MRB")
        self.assertEqual(value.signed(), 1 + 12)
        self.assertEqual(op, "multiply-accumulate")


# ---------------------------------------------------------------------------
# Saturate (PRM Table 17-7 "0000 F--x", Table 3-5's six max-value rows).
# ---------------------------------------------------------------------------


class SaturateTest(unittest.TestCase):
    def test_signed_fractional_positive_edge_clamps(self):
        rn, value, op, astatx_update = compute(
            full_compute(1, 0x09, 3, 0, 0),  # RN = sat mrf (SF)
            {},
            special={"MRF": T._mr_from_signed(1 << 63)},  # one past max
        )
        self.assertEqual(value, T.Const(0x7FFFFFFF))
        self.assertEqual(op, "saturate-mrf")
        astatx = astatx_update(T.Const(0xFFFFFFFF))
        # PRM Table 3-7's sat row: MV is architecturally fixed 0, even
        # though the source value overflowed the format.
        self.assertEqual(T._astatx_known_bit(astatx, T.MV_BIT), False)

    def test_signed_fractional_negative_edge_clamps(self):
        rn, value, op, _ = compute(
            full_compute(1, 0x09, 3, 0, 0),
            {},
            special={"MRF": T._mr_from_signed(-(1 << 63) - 1)},
        )
        self.assertEqual(value, T.Const(0x80000000))

    def test_unsigned_fractional_negative_clamps_to_zero(self):
        rn, value, op, _ = compute(
            full_compute(1, 0x08, 3, 0, 0),  # RN = sat mrf (UF)
            {},
            special={"MRF": T._mr_from_signed(-1)},
        )
        self.assertEqual(value, T.Const(0))

    def test_signed_integer_edge_clamps(self):
        rn, value, op, _ = compute(
            full_compute(1, 0x01, 3, 0, 0),  # RN = sat mrf (SI)
            {},
            special={"MRF": T._mr_from_signed(1 << 31)},  # one past INT32_MAX
        )
        self.assertEqual(value, T.Const(0x7FFFFFFF))

    def test_unsigned_integer_edge_clamps(self):
        rn, value, op, _ = compute(
            full_compute(1, 0x00, 3, 0, 0),  # RN = sat mrf (UI)
            {},
            special={"MRF": T._mr_from_signed((1 << 32) + 5)},
        )
        self.assertEqual(value, T.Const(0xFFFFFFFF))

    def test_in_range_value_passes_through_unchanged(self):
        rn, value, op, _ = compute(
            full_compute(1, 0x09, 3, 0, 0),
            {},
            special={"MRF": T._mr_from_signed(1 << 61)},  # 0.25, in-range
        )
        self.assertEqual(value, T.Const(0x20000000))

    def test_self_store_destination_writes_back_the_clamped_mr(self):
        # opcode 0x0B: mrf = sat mrf (SF) -- clamps the accumulator itself,
        # not just an RN extraction.
        rn, value, op, _ = compute(
            full_compute(1, 0x0B, 0, 0, 0),
            {},
            special={"MRF": T._mr_from_signed(1 << 63)},
        )
        self.assertEqual(rn, "MRF")
        self.assertEqual(value.signed(), (1 << 63) - 1)


# ---------------------------------------------------------------------------
# Round (PRM Table 17-7 "0001 1...", Table 3-6's round-to-nearest-at-bit-32).
# ---------------------------------------------------------------------------


class RoundTest(unittest.TestCase):
    def test_round_half_up_tie(self):
        # Exactly half a ULP (bit 31 set, nothing below): rounds up.
        rn, value, op, _ = compute(
            full_compute(1, 0x19, 3, 0, 0),  # RN = rnd mrf (SF)
            {},
            special={"MRF": T._mr_from_signed(0x80000000)},
        )
        self.assertEqual(value, T.Const(1))  # MR1F after rounding up
        self.assertEqual(op, "round-mrf")

    def test_round_below_half_truncates(self):
        rn, value, op, _ = compute(
            full_compute(1, 0x19, 3, 0, 0),
            {},
            special={"MRF": T._mr_from_signed(0x7FFFFFFF)},
        )
        self.assertEqual(value, T.Const(0))

    def test_round_negative_tie(self):
        # Two's-complement round-half-up (add 2**31, mod 2**80) applies to
        # a negative accumulator exactly the same as a positive one: -2**31
        # + 2**31 (the rounding constant) = 0, so MR1F rounds to 0.
        rn, value, op, _ = compute(
            full_compute(1, 0x19, 3, 0, 0),
            {},
            special={"MRF": T._mr_from_signed(-0x80000000)},
        )
        self.assertEqual(value, T.Const(0))

    def test_round_carries_into_mr2(self):
        # MR1F at its max (0xFFFFFFFF) rounds up and carries into MR2F.
        rn, value, op, _ = compute(
            full_compute(1, 0x1A, 0, 0, 0),  # mrf = rnd mrf (UF), self-store
            {},
            special={"MRF": T._mr_from_signed((0xFFFFFFFF << 32) | 0x80000000)},
        )
        self.assertEqual(rn, "MRF")
        self.assertEqual(T._mr_read_word(value, 2), T.Const(1))
        self.assertEqual(T._mr_read_word(value, 1), T.Const(0))
        self.assertEqual(T._mr_read_word(value, 0), T.Const(0))

    def test_mod1_round_bit_applies_within_a_multiply_row(self):
        # MOD1's own "R" suffix (SSFR/UUFR/...) rounds as part of a plain
        # multiply row too, not just the standalone rnd instruction:
        # opcode 0x49 is UUFR (0x48 UUF | round bit).
        rn, value, op, _ = compute(
            full_compute(1, 0x49, 5, 1, 2),
            {1: T.Const(0x80000000), 2: T.Const(0x80000001)},
        )
        # Unrounded UUF result (0x48) would truncate; the rounded RN result
        # differs by the rounding adjustment.
        _, unrounded, _, _ = compute(
            full_compute(1, 0x48, 5, 1, 2),
            {1: T.Const(0x80000000), 2: T.Const(0x80000001)},
        )
        self.assertNotEqual(value, unrounded)


# ---------------------------------------------------------------------------
# Clear (PRM Table 17-7 "0001 01d0").
# ---------------------------------------------------------------------------


class ClearTest(unittest.TestCase):
    def test_clear_resets_to_zero_with_all_flags_clear(self):
        rn, value, op, astatx_update = compute(
            full_compute(1, 0x14, 0, 0, 0),  # mrf = 0
            {},
            special={"MRF": T._mr_from_signed(-1)},
        )
        self.assertEqual(rn, "MRF")
        self.assertEqual(value.signed(), 0)
        self.assertEqual(op, "clear-mrf")
        astatx = astatx_update(T.Const(0xFFFFFFFF))
        for bit in (T.MN_BIT, T.MV_BIT, T.MU_BIT, T.MI_BIT):
            self.assertEqual(T._astatx_known_bit(astatx, bit), False)


# ---------------------------------------------------------------------------
# Underflow (MU) -- fractional-only (PRM p.28-6).
# ---------------------------------------------------------------------------


class UnderflowTest(unittest.TestCase):
    def test_tiny_nonzero_fractional_value_sets_mu(self):
        # mrf=1 (the smallest nonzero positive 80-bit pattern): all bits
        # except the very bottom are 0, matching the fractional underflow
        # condition (PRM p.28-6: upper 48 bits all zero, lower 32 not).
        mn, mv, mu = compute_mult._mr_flags(1, fractional=True, signed_result=True)
        self.assertEqual((mn, mv, mu), (False, False, True))

    def test_integer_results_never_underflow(self):
        mn, mv, mu = compute_mult._mr_flags(1, fractional=False, signed_result=True)
        self.assertFalse(mu)


# ---------------------------------------------------------------------------
# SIMD PEy (MSF/MSB share the same opcodes, redirected register file).
# ---------------------------------------------------------------------------


class SimdPeyTest(unittest.TestCase):
    def test_pey_accumulate_uses_msf_independent_of_pex_mrf(self):
        # S1/S2 (PEy's register file) live at UREG codes 80+1/80+2.
        values = {81: T.Const(0x40000000), 82: T.Const(0x40000000)}  # 0.5 * 0.5
        rn, value, op, _ = compute_pey(
            full_compute(1, 0xBC, 0, 1, 2),
            values,
            special={"MSF": T._mr_from_signed(1 << 60), "MRF": T._mr_from_signed(999)},
        )
        self.assertEqual(rn, "MRF")  # _compute_pey's internal disguise key
        self.assertEqual(value.signed(), (1 << 60) + (1 << 61))

    def test_apply_compute_pey_writes_msf_not_mrf(self):
        state = T.State(1, {})
        state.uregs[81] = T.Const(0x40000000)
        state.uregs[82] = T.Const(0x40000000)
        state.special["MSF"] = T._mr_from_signed(0)
        result = compute_pey(full_compute(1, 0xBC, 0, 1, 2), state.uregs, state.special)
        T._apply_compute_pey(state, insn("2a", {}), result)
        self.assertIn("MSF", state.special)
        self.assertNotIn("MRF", state.special)
        self.assertEqual(state.special["MSF"].signed(), 1 << 61)

    def test_pey_saturate(self):
        rn, value, op, _ = compute_pey(
            full_compute(1, 0x09, 3, 0, 0),
            {},
            special={"MSF": T._mr_from_signed(1 << 63)},
        )
        self.assertEqual(value, T.Const(0x7FFFFFFF))
        self.assertEqual(op, "saturate-mrf")


if __name__ == "__main__":
    unittest.main()
