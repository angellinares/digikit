"""Symbolic tracer: floating-point computes and opcode regressions."""

import struct
import unittest
from importlib import import_module

T = import_module("tools.sharc_dev.sharc_trace")
Instruction = import_module("tools.sharc_dev.sharc_disasm").Instruction
L = import_module("tools.sharc_dev.sharcldr")
from support.sharc_encode import encode  # noqa: E402
from support.sharc_trace_build import insn  # noqa: E402

_floats = import_module(
    "tools.sharc_dev.sharc_core.floats"
)  # _double_to_fixed isn't re-exported by T

from support.sharc_trace_build import (  # noqa: E402
    TraceRunMixin,
    dual_add_subtract_fields,
    f32,
    full_compute,
    mulalu_fields,
    short_compute,
)


class FloatComputeTest(unittest.TestCase):
    """Float ALU/multiplier/multifunction compute forms (PRM Table 18-5
    p.425-427, Table 18-7 p.428-429, Table 18-10 p.433, and the
    multi_function_selector p.423), cross-checked against the classic PGR's
    per-instruction pages for exact flag formulas the SHARC+ PRM only marks
    '*'/data-dependent."""

    def astatx_after(self, fields, values, old_astatx):
        rn, value, operation, update = T._compute(fields, False, values)
        return rn, value, operation, update(old_astatx)

    # -- Fn = Fx + Fy / Fx - Fy (PGR p.11-24/11-25) --------------------------

    def test_float_add(self):
        rn, value, op, astatx = self.astatx_after(
            full_compute(0, 0x81, 0, 1, 2),
            {1: T.Const(f32(1.5)), 2: T.Const(f32(2.5))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-add")
        self.assertEqual(value, T.Const(f32(4.0)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AC_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AF_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), False)

    def test_float_subtract_negative_result(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x82, 0, 1, 2),
            {1: T.Const(f32(1.0)), 2: T.Const(f32(2.5))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-subtract")
        self.assertEqual(value, T.Const(f32(-1.5)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), False)

    def test_float_add_overflow_rounds_to_infinity_and_sets_av(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x81, 0, 1, 2),
            {1: T.Const(f32(3.4e38)), 2: T.Const(f32(3.4e38))},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0x7F800000))  # +infinity
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), True)

    def test_float_add_nan_input_returns_all_ones_and_sets_ai(self):
        # PGR p.11-24: "A NAN input returns an all 1s result."
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x81, 0, 1, 2),
            {1: T.Const(f32(float("nan"))), 2: T.Const(f32(1.0))},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0xFFFFFFFF))
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), True)

    # -- Fn = (Fx + Fy) / 2 (PRM p.19-4, opcode 1000 1001) -------------------

    def test_float_average(self):
        rn, value, op, astatx = self.astatx_after(
            full_compute(0, 0x89, 0, 1, 2),
            {1: T.Const(f32(1.0)), 2: T.Const(f32(2.0))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-average")
        self.assertEqual(value, T.Const(f32(1.5)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)

    def test_float_average_near_max_stays_finite_and_av_clear(self):
        # PRM p.19-4/19-5: favg's own ASTATx/y Flags table marks AV
        # "Cleared" unconditionally -- unlike plain float-add's AV, which
        # tracks post-rounded overflow. Averaging two already-in-range
        # float32 values can never itself overflow (the result is always
        # within the larger operand's magnitude), unlike a plain add, so
        # this also exercises the near-FLT_MAX case that would overflow
        # float-add.
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x89, 0, 1, 2),
            {1: T.Const(f32(3.4e38)), 2: T.Const(f32(3.4e38))},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(f32(3.4e38)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)

    def test_float_average_nan_input_returns_all_ones_and_sets_ai(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0x89, 0, 1, 2),
            {1: T.Const(f32(float("nan"))), 2: T.Const(f32(1.0))},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0xFFFFFFFF))
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), True)

    # -- Fn = -Fx / Fn = abs Fx (PGR p.11-30/11-31) --------------------------

    def test_float_negate(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0xA2, 0, 1, 0),
            {1: T.Const(f32(2.5))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-negate")
        self.assertEqual(value, T.Const(f32(-2.5)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), True)

    def test_float_abs_an_fixed_zero_as_from_input_sign(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0xB0, 0, 1, 0),
            {1: T.Const(f32(-3.5))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-abs")
        self.assertEqual(value, T.Const(f32(3.5)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AS_BIT), True)
        _, _, _, astatx_pos = self.astatx_after(
            full_compute(0, 0xB0, 0, 1, 0),
            {1: T.Const(f32(3.5))},
            T.Unknown("start"),
        )
        self.assertEqual(T._astatx_known_bit(astatx_pos, T.AS_BIT), False)

    # -- Fn = rnd Fx (PRM Table 18-5 opcode 0xA5, p.20-8; PGR Table 12-4 --
    # opcode 0xA5, pp.12-3/12-4, and p.11-33) --------------------------------

    def test_rnd_passes_through_an_already_32bit_finite_value(self):
        # Every UREG this tracer tracks is already stored at IEEE-754
        # single precision (see the module's "Floating-point compute
        # support" header comment), so re-rounding a finite, normal,
        # already-32-bit value to the same boundary is a no-op.
        rn, value, op, astatx = self.astatx_after(
            full_compute(0, 0xA5, 0, 1, 0),
            {1: T.Const(f32(2.5))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-round32")
        self.assertEqual(value, T.Const(f32(2.5)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AC_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AS_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AF_BIT), True)

    def test_rnd_negative_result_sets_an(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0xA5, 0, 1, 0),
            {1: T.Const(f32(-2.5))},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(f32(-2.5)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), True)

    def test_rnd_denormal_input_flushes_to_signed_zero(self):
        for source, expected in ((1e-40, 0), (-1e-40, 0x80000000)):
            with self.subTest(source=source):
                _, value, _, astatx = self.astatx_after(
                    full_compute(0, 0xA5, 0, 1, 0),
                    {1: T.Const(f32(source))},
                    T.Unknown("start"),
                )
                self.assertEqual(value, T.Const(expected))
                self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), True)

    def test_rnd_nan_input_returns_all_ones_and_sets_ai(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0xA5, 0, 1, 0),
            {1: T.Const(f32(float("nan")))},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0xFFFFFFFF))
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)

    def test_rnd_infinity_input_passes_through_without_setting_av(self):
        # The "post-rounded overflow" corner this op documents only arises
        # from rounding away extended-precision mantissa bits this tracer
        # never carries between ops; passing an already-infinite value
        # through must not fabricate an overflow.
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0xA5, 0, 1, 0),
            {1: T.Const(f32(float("inf")))},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0x7F800000))
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)

    # -- Rn = mant Fx (PRM Table 18-5 p.427, opcode 0xAD; PGR p.11-34/11-35) -

    def test_mant_extracts_hidden_bit_and_fraction_left_justified(self):
        rn, value, op, astatx = self.astatx_after(
            full_compute(0, 0xAD, 0, 1, 0),
            {1: T.Const(f32(3.5))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "mant")
        # 3.5 = 1.75 * 2^1; hidden bit + 23-bit fraction of 1.75 (0x600000)
        # left-justified into a 32-bit 1.31 word.
        self.assertEqual(value, T.Const(0xE0000000))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AS_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), False)

    def test_mant_negative_input_sets_as_not_an(self):
        # PGR p.11-34: "unsigned-magnitude result" -- AS carries the
        # input's sign, AN stays fixed 0 even though the input is negative.
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0xAD, 0, 1, 0),
            {1: T.Const(f32(-3.5))},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0xE0000000))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AS_BIT), True)

    def test_mant_zero_and_denormal_inputs_flush_to_zero_mantissa(self):
        for source, label in ((0.0, "zero"), (1e-40, "denormal")):
            with self.subTest(label=label):
                _, value, _, astatx = self.astatx_after(
                    full_compute(0, 0xAD, 0, 1, 0),
                    {1: T.Const(f32(source))},
                    T.Unknown("start"),
                )
                self.assertEqual(value, T.Const(0))
                self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), True)

    def test_mant_nan_and_infinity_both_return_all_ones(self):
        # PGR p.11-34: "A NAN or an infinity input returns an all 1s
        # result" -- unlike the arithmetic float ALU ops, MANT overrides
        # infinity too, not just NAN.
        _, nan_value, _, nan_astatx = self.astatx_after(
            full_compute(0, 0xAD, 0, 1, 0),
            {1: T.Const(f32(float("nan")))},
            T.Unknown("start"),
        )
        self.assertEqual(nan_value, T.Const(0xFFFFFFFF))
        self.assertEqual(T._astatx_known_bit(nan_astatx, T.AI_BIT), True)
        self.assertEqual(T._astatx_known_bit(nan_astatx, T.AV_BIT), False)
        _, inf_value, _, inf_astatx = self.astatx_after(
            full_compute(0, 0xAD, 0, 1, 0),
            {1: T.Const(f32(float("inf")))},
            T.Unknown("start"),
        )
        self.assertEqual(inf_value, T.Const(0xFFFFFFFF))
        self.assertEqual(T._astatx_known_bit(inf_astatx, T.AV_BIT), True)
        self.assertEqual(T._astatx_known_bit(inf_astatx, T.AI_BIT), False)

    # -- Fn = scalb Fx by Ry (PRM Table 18-5 p.427, opcode 0xBD; PGR p.11-33) -

    def test_scalb_scales_the_exponent_by_the_fixed_point_integer(self):
        rn, value, op, astatx = self.astatx_after(
            full_compute(0, 0xBD, 0, 1, 2),
            {1: T.Const(f32(1.5)), 2: T.Const(3)},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-scalb")
        self.assertEqual(value, T.Const(f32(12.0)))  # 1.5 * 2**3
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)

    def test_scalb_negative_shift_scales_down(self):
        _, value, _, _ = self.astatx_after(
            full_compute(0, 0xBD, 0, 1, 2),
            {1: T.Const(f32(12.0)), 2: T.Const(0xFFFFFFFD)},  # Ry = -3
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(f32(1.5)))

    def test_scalb_overflow_rounds_to_infinity_and_sets_av(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0xBD, 0, 1, 2),
            {1: T.Const(f32(3.0e38)), 2: T.Const(10)},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0x7F800000))  # +infinity
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), True)

    def test_scalb_underflow_flushes_to_zero_not_subnormal(self):
        # PGR p.11-33: "Denormal returns +-zero" -- an explicit override of
        # the ordinary IEEE subnormal result struct would otherwise round to.
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0xBD, 0, 1, 2),
            {1: T.Const(f32(-1.0)), 2: T.Const(0xFFFFFF78)},  # Ry = -136
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0x80000000))  # -0.0
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), True)

    def test_scalb_nan_input_returns_all_ones_and_sets_ai(self):
        _, value, _, astatx = self.astatx_after(
            full_compute(0, 0xBD, 0, 1, 2),
            {1: T.Const(f32(float("nan"))), 2: T.Const(3)},
            T.Unknown("start"),
        )
        self.assertEqual(value, T.Const(0xFFFFFFFF))
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), True)

    # -- Fn = pass Fx (PGR p.11-32) ------------------------------------------

    def test_float_pass_roundtrips_value(self):
        _, value, op, _ = self.astatx_after(
            full_compute(0, 0xA1, 0, 1, 0),
            {1: T.Const(f32(-7.25))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-pass")
        self.assertEqual(value, T.Const(f32(-7.25)))

    # -- Fn = min/max(Fx, Fy) (PGR p.11-46/11-47) ----------------------------

    def test_float_min_max(self):
        values = {1: T.Const(f32(3.0)), 2: T.Const(f32(-1.0))}
        _, min_value, min_op, _ = self.astatx_after(
            full_compute(0, 0xE1, 0, 1, 2), values, T.Unknown("start")
        )
        _, max_value, max_op, _ = self.astatx_after(
            full_compute(0, 0xE2, 0, 1, 2), values, T.Unknown("start")
        )
        self.assertEqual((min_op, min_value), ("float-min", T.Const(f32(-1.0))))
        self.assertEqual((max_op, max_value), ("float-max", T.Const(f32(3.0))))

    # -- Rn = min/max(Rx, Ry) (PRM Table 18-5 p.425, opcode 0x61/0x62; -------
    # -- PGR p.11-20/11-21, fixed-point, distinct from the float form above) -

    def test_fixed_min_max_pick_the_signed_extreme(self):
        values = {1: T.Const(3), 2: T.Const(0xFFFFFFFF)}  # Rx=3, Ry=-1
        _, min_value, min_op, min_astatx = self.astatx_after(
            full_compute(0, 0x61, 0, 1, 2), values, T.Unknown("start")
        )
        _, max_value, max_op, _ = self.astatx_after(
            full_compute(0, 0x62, 0, 1, 2), values, T.Unknown("start")
        )
        self.assertEqual((min_op, min_value), ("min", T.Const(0xFFFFFFFF)))
        self.assertEqual((max_op, max_value), ("max", T.Const(3)))
        # PRM Table 3-2 ("AF Flag = 0"): AV/AC/AS/AI fixed 0; AZ/AN from
        # the chosen (negative) result.
        self.assertEqual(T._astatx_known_bit(min_astatx, T.AN_BIT), True)
        self.assertEqual(T._astatx_known_bit(min_astatx, T.AZ_BIT), False)
        self.assertEqual(T._astatx_known_bit(min_astatx, T.AV_BIT), False)
        self.assertEqual(T._astatx_known_bit(min_astatx, T.AI_BIT), False)

    def test_fixed_min_max_unknown_operand_forgets_the_result(self):
        rn, value, op, astatx = self.astatx_after(
            full_compute(0, 0x61, 0, 1, 2),
            {1: T.Unknown("uninitialized R1"), 2: T.Const(5)},
            T.Const(0xFFFFFFFF),
        )
        self.assertEqual(op, "min")
        self.assertIsInstance(value, T.Unknown)
        self.assertIsNone(T._astatx_known_bit(astatx, T.AZ_BIT))
        self.assertIsNone(T._astatx_known_bit(astatx, T.AN_BIT))

    # -- Rn = clip Rx by Ry (PRM Table 18-5, opcode 0110 0011 = 0x63) -------

    def test_fixed_clip(self):
        for rx, ry, expected, label in (
            (100, 32767, 100, "within range passes through"),
            (40000, 32767, 32767, "positive out-of-range clips to +|Ry|"),
            (-40000, 32767, -32767, "negative out-of-range clips to -|Ry|"),
            (-40000, -32767, -32767, "Ry's sign does not matter"),
            (32767, 32767, 32767, "|Rx| == |Ry| gives Rx"),
        ):
            with self.subTest(label):
                values = {1: T.Const(rx & 0xFFFFFFFF), 2: T.Const(ry & 0xFFFFFFFF)}
                _, value, op, astatx = self.astatx_after(
                    full_compute(0, 0x63, 0, 1, 2), values, T.Unknown("start")
                )
                self.assertEqual((op, value), ("clip", T.Const(expected & 0xFFFFFFFF)))
                self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), expected < 0)
                self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)
        _, value, _, _ = self.astatx_after(
            full_compute(0, 0x63, 0, 1, 2),
            {1: T.Unknown("uninitialized R1"), 2: T.Const(5)},
            T.Unknown("start"),
        )
        self.assertIsInstance(value, T.Unknown)

    # -- Fn = clip Fx by Fy (PGR p.11-48) ------------------------------------

    def test_float_clip(self):
        for fx, fy, expected, label in (
            (1.0, 5.0, 1.0, "within range passes through"),
            (7.0, 5.0, 5.0, "positive out-of-range clips to +|Fy|"),
            (-7.0, 5.0, -5.0, "negative out-of-range clips to -|Fy|"),
        ):
            with self.subTest(label=label):
                _, value, op, _ = self.astatx_after(
                    full_compute(0, 0xE3, 0, 1, 2),
                    {1: T.Const(f32(fx)), 2: T.Const(f32(fy))},
                    T.Unknown("start"),
                )
                self.assertEqual(op, "float-clip")
                self.assertEqual(value, T.Const(f32(expected)))

    # -- comp(Fx, Fy) (PGR p.11-29) -------------------------------------------

    def test_float_compare_equal_sets_az(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x8A, 0, 1, 2),
            {1: T.Const(f32(2.0)), 2: T.Const(f32(2.0))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-compare")
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.AF_BIT), True)

    def test_float_compare_nan_is_unordered_and_sets_ai(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0x8A, 0, 1, 2),
            {1: T.Const(f32(float("nan"))), 2: T.Const(f32(2.0))},
            T.Unknown("start"),
        )
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), True)

    # -- Fn = float Rx / Rn = trunc Fx (PRM p.427, PGR p.11-37/11-39) -------

    def test_float_convert_from_signed_int(self):
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0xCA, 0, 1, 0),
            {1: T.Const(0xFFFFFFFD)},  # -3 two's complement
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-convert")
        self.assertEqual(value, T.Const(f32(-3.0)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), False)

    def test_trunc_toward_zero_both_signs(self):
        for source, expected in ((3.7, 3), (-3.7, -3)):
            with self.subTest(source=source):
                _, value, op, _ = self.astatx_after(
                    full_compute(0, 0xCD, 0, 1, 0),
                    {1: T.Const(f32(source))},
                    T.Unknown("start"),
                )
                self.assertEqual(op, "trunc")
                self.assertEqual(value, T.Const(expected & 0xFFFFFFFF))

    def test_trunc_overflow_saturates_when_alusat_set(self):
        mode1 = T.Const(1 << T.ALUSAT_BIT)
        result = T._float_to_fixed_trunc(T.Const(f32(1e10)), mode1, "trunc F1")
        self.assertEqual(result, (T.Const(0x7FFFFFFF), True, False))

    def test_trunc_overflow_all_ones_when_alusat_clear(self):
        # SC58x/2158x PRM pp.20-11..20-14 ("RN = fix/trunc Fx[ by Ry]"):
        # "If saturation mode is not set, ... a result that overflows
        # returns a floating-point result of all 1s"; the AI bullet's
        # "when saturation mode is not set ... the result overflows"
        # clause sets AI too.
        mode1 = T.Const(0)
        result = T._float_to_fixed_trunc(T.Const(f32(1e10)), mode1, "trunc F1")
        self.assertEqual(result, (T._FLOAT_ALL_ONES, True, True))

    def test_trunc_overflow_unknown_when_alusat_unknown(self):
        # AV is unconditional on ALUSAT (the AV bullet has no saturation
        # gate), but the result and AI depend on which saturation rule
        # would apply, so they stay Unknown when ALUSAT itself is unknown.
        mode1 = T.PartialConst(mask=1 << T.TRUNCATE_BIT, bits=0)
        result = T._float_to_fixed_trunc(T.Const(f32(1e10)), mode1, "trunc F1")
        value, overflow, invalid = result
        self.assertIsInstance(value, T.Unknown)
        self.assertEqual(overflow, True)
        self.assertIsNone(invalid)

    def test_fix_nan_input_is_all_ones_regardless_of_alusat(self):
        # Both FIX/TRUNC pages: "A NAN input returns a floating-point all
        # 1s result" is its own sentence, not scoped by "If saturation
        # mode is not set" -- unlike infinity/overflow below, a NAN's
        # result and AI do not depend on ALUSAT. AV stays clear (the AV
        # bullet only lists "the input is +-infinity", not NAN -- matching
        # this module's existing NAN convention, e.g. float add/sub/mult).
        nan = T.Const(f32(float("nan")))
        for label, mode1 in (
            ("alusat_set", T.Const(1 << T.ALUSAT_BIT)),
            ("alusat_clear", T.Const(0)),
        ):
            with self.subTest(label):
                result = T._float_to_fixed(nan, mode1, False, "fix F1")
                self.assertEqual(result, (T._FLOAT_ALL_ONES, False, True))

    def test_fix_infinity_saturates_when_alusat_set(self):
        # "In saturation mode ... +infinity return[s] the maximum positive
        # number (0x7FFF FFFF), and ... -infinity return[s] the minimum
        # negative number (0x8000 0000)." AI is cleared (its saturation-
        # gated clause does not fire); AV is set (its own bullet: "the
        # input is +-infinity", unconditional on ALUSAT).
        mode1 = T.Const(1 << T.ALUSAT_BIT)
        pinf = T.Const(f32(float("inf")))
        ninf = T.Const(f32(float("-inf")))
        self.assertEqual(
            T._float_to_fixed(pinf, mode1, False, "fix F1"),
            (T.Const(0x7FFFFFFF), True, False),
        )
        self.assertEqual(
            T._float_to_fixed(ninf, mode1, False, "fix F1"),
            (T.Const(0x80000000), True, False),
        )

    def test_fix_infinity_all_ones_when_alusat_clear(self):
        # "If saturation mode is not set, an infinity input ... returns a
        # floating-point result of all 1s"; AI's "when saturation mode is
        # not set ... either input is an infinity" clause now fires too.
        mode1 = T.Const(0)
        pinf = T.Const(f32(float("inf")))
        self.assertEqual(
            T._float_to_fixed(pinf, mode1, False, "fix F1"),
            (T._FLOAT_ALL_ONES, True, True),
        )

    def test_fix_infinity_unknown_when_alusat_unknown(self):
        mode1 = T.PartialConst(mask=1 << T.TRUNCATE_BIT, bits=0)
        pinf = T.Const(f32(float("inf")))
        value, overflow, invalid = T._float_to_fixed(pinf, mode1, False, "fix F1")
        self.assertIsInstance(value, T.Unknown)
        self.assertEqual(overflow, True)
        self.assertIsNone(invalid)

    def test_double_fix_nan_and_infinity_match_the_float_rule(self):
        # SC58x/2158x PRM pp.20-28..20-32 ("RN = fix/trunc Fx:y[ by Ry]")
        # carry the identical NAN/infinity/overflow wording as the 32-bit
        # pages above.
        def dbl(v):
            hi, lo = struct.unpack(">II", struct.pack(">d", v))
            return T.Const(hi), T.Const(lo)

        sat = T.Const(1 << T.ALUSAT_BIT)
        unsat = T.Const(0)
        nan_hi, nan_lo = dbl(float("nan"))
        self.assertEqual(
            _floats._double_to_fixed(nan_hi, nan_lo, sat, False, "fix F1:0"),
            (T._FLOAT_ALL_ONES, False, True),
        )
        self.assertEqual(
            _floats._double_to_fixed(nan_hi, nan_lo, unsat, False, "fix F1:0"),
            (T._FLOAT_ALL_ONES, False, True),
        )
        pinf_hi, pinf_lo = dbl(float("inf"))
        self.assertEqual(
            _floats._double_to_fixed(pinf_hi, pinf_lo, sat, False, "fix F1:0"),
            (T.Const(0x7FFFFFFF), True, False),
        )
        self.assertEqual(
            _floats._double_to_fixed(pinf_hi, pinf_lo, unsat, False, "fix F1:0"),
            (T._FLOAT_ALL_ONES, True, True),
        )

    # -- Fn = recips/rsqrts Fx seeds (PRM p.427, PGR p.11-44/11-45) ---------

    # The seed tables and the flags are checked against the device capture in
    # test_sharc_recips_device.py; the tests below pin single documented cases.

    def seed_after(self, opcode, rx_value, old_astatx=None):
        rn, value, operation, update = T._compute(
            full_compute(0, opcode, 0, 1, 0), False, {1: rx_value}
        )
        return rn, value, operation, update(old_astatx or T.Unknown("start"))

    def test_recips_ordinary_positive_value(self):
        # 4.0 = 1.0 * 2^2: seed exponent -2 - 1 = -3, mantissa table entry 0.
        _, value, op, astatx = self.seed_after(0xC4, T.Const(f32(4.0)))
        self.assertEqual(op, "float-recips-seed")
        self.assertEqual(value, T.Const(0x3E7F8000))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), False)
        # a float ALU operation sets AF (measured on the device)
        self.assertEqual(T._astatx_known_bit(astatx, T.AF_BIT), True)

    def test_recips_negative_value_sets_an_and_keeps_sign(self):
        _, value, op, astatx = self.seed_after(0xC4, T.Const(f32(-4.0)))
        self.assertEqual(value, T.Const(0xBE7F8000))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), True)

    def test_recips_nan_input_returns_all_ones_and_sets_ai_not_an(self):
        # Measured: AN stays clear for a negative NAN, too.
        for bits in (0x7FC00000, 0xFFC00000):
            _, value, op, astatx = self.seed_after(0xC4, T.Const(bits))
            self.assertEqual(value, T.Const(0xFFFFFFFF))
            self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), True)
            self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)

    def test_recips_zero_input_returns_signed_infinity_and_sets_av(self):
        for zero_bits, expected in ((0x00000000, 0x7F800000), (0x80000000, 0xFF800000)):
            with self.subTest(zero_bits=hex(zero_bits)):
                _, value, _, astatx = self.seed_after(0xC4, T.Const(zero_bits))
                self.assertEqual(value, T.Const(expected))
                self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), True)
                self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), False)

    def test_recips_denormal_input_is_flushed_to_zero_first(self):
        # PRM p.417-418 (IEEE-754 compatibility bullet, general to every
        # computational unit): "Denormal operands ... flush to zero when
        # input to a computational unit." A flushed +denormal therefore
        # takes recips's own +-zero -> +-infinity path.
        _, value, _, astatx = self.seed_after(0xC4, T.Const(0x00000001))
        self.assertEqual(value, T.Const(0x7F800000))
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), True)

    def test_recips_large_exponent_underflows_to_signed_zero(self):
        # PRM p.19-17: "If the unbiased exponent of Fx is greater than
        # +125, the result is +-zero." biased 253 -> unbiased 126 > 125.
        _, value, op, astatx = self.seed_after(0xC4, T.Const((253 << 23) | (1 << 31)))
        self.assertEqual(value, T.Const(1 << 31))  # -zero
        self.assertEqual(op, "float-recips-seed-underflow")
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)

    def test_recips_symbolic_input_stays_unknown(self):
        _, value, op, _ = self.seed_after(0xC4, T.Unknown("uninitialized R1"))
        self.assertEqual(op, "float-recips-seed")
        self.assertIsInstance(value, T.Unknown)

    def test_rsqrts_ordinary_positive_value(self):
        # 4.0: e = 2, seed exponent -1 - (2 >> 1) = -2; index (exponent LSB 1, mantissa 0).
        _, value, op, astatx = self.seed_after(0xC5, T.Const(f32(4.0)))
        self.assertEqual(op, "float-rsqrts-seed")
        self.assertEqual(value, T.Const(0x3EFF8000))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), False)

    def test_rsqrts_nan_or_negative_nonzero_input_returns_all_ones(self):
        # PRM p.19-18: "A NAN input or a negative nonzero input returns a
        # result of all 1s"; AI set for either case, AN only for -zero.
        for bits in (0x7FC00000, f32(-4.0) & 0xFFFFFFFF):
            with self.subTest(bits=hex(bits)):
                _, value, op, astatx = self.astatx_after(
                    full_compute(0, 0xC5, 0, 1, 0),
                    {1: T.Const(bits)},
                    T.Unknown("start"),
                )
                self.assertEqual(op, "float-rsqrts-seed")
                self.assertEqual(value, T.Const(0xFFFFFFFF))
                self.assertEqual(T._astatx_known_bit(astatx, T.AI_BIT), True)
                self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), False)

    def test_rsqrts_zero_input_returns_signed_infinity_and_sets_av(self):
        # PRM p.19-18: "The input +-zero returns +-infinity and sets the
        # overflow flag"; AN set only for -zero.
        for zero_bits, expected, an in (
            (0x00000000, 0x7F800000, False),
            (0x80000000, 0xFF800000, True),
        ):
            with self.subTest(zero_bits=hex(zero_bits)):
                _, value, op, astatx = self.astatx_after(
                    full_compute(0, 0xC5, 0, 1, 0),
                    {1: T.Const(zero_bits)},
                    T.Unknown("start"),
                )
                self.assertEqual(op, "float-rsqrts-seed")
                self.assertEqual(value, T.Const(expected))
                self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), True)
                self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), an)
                self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), False)

    def test_rsqrts_denormal_input_is_flushed_to_zero_first(self):
        # Same general denormal-input rule as recips (PRM p.417-418):
        # a flushed +denormal takes rsqrts's own +-zero -> +-infinity path,
        # not the "negative nonzero" all-1s path.
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0xC5, 0, 1, 0),
            {1: T.Const(0x00000001)},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-rsqrts-seed")
        self.assertEqual(value, T.Const(0x7F800000))
        self.assertEqual(T._astatx_known_bit(astatx, T.AV_BIT), True)

    def test_rsqrts_positive_infinity_input_returns_zero(self):
        # PRM p.19-18: "The input +infinity returns +zero."
        _, value, op, astatx = self.astatx_after(
            full_compute(0, 0xC5, 0, 1, 0),
            {1: T.Const(0x7F800000)},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-rsqrts-seed")
        self.assertEqual(value, T.Const(0))
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), True)

    def test_rsqrts_symbolic_input_stays_unknown(self):
        _, value, op, _ = self.seed_after(0xC5, T.Unknown("uninitialized R1"))
        self.assertEqual(op, "float-rsqrts-seed")
        self.assertIsInstance(value, T.Unknown)

    # -- Fn = Fx * Fy (PRM Table 18-7 p.428-429, PGR p.11-57) ----------------

    def test_float_multiply(self):
        rn, value, op, astatx = self.astatx_after(
            full_compute(1, 0x30, 0, 1, 2),
            {1: T.Const(f32(1.5)), 2: T.Const(f32(2.0))},
            T.Const(0xFFFFFFFF),
        )
        self.assertEqual(op, "float-multiply")
        self.assertEqual(value, T.Const(f32(3.0)))
        # PRM Table 3-9: MN/MV/MU/MI are all data-dependent for a float
        # multiply. 1.5*2.0=3.0 is positive, not overflowed, not a
        # denormal underflow and not NAN-invalid.
        for bit in (T.MN_BIT, T.MV_BIT, T.MU_BIT, T.MI_BIT):
            self.assertEqual(T._astatx_known_bit(astatx, bit), False)

    # -- Dual Add/Subtract: Ra = Rx+Ry, Rs = Rx-Ry (PRM Table 18-10 p.433) --

    def test_dual_add_subtract_fixed_writes_both_registers(self):
        fields = dual_add_subtract_fields(False, rs=3, ra=0, rx=1, ry=2)
        rn, value, op, astatx = self.astatx_after(
            fields, {1: T.Const(10), 2: T.Const(3)}, T.Unknown("start")
        )
        self.assertEqual(op, "dual-add-subtract")
        self.assertEqual(rn, (0, 3))
        self.assertEqual(value, (T.Const(13), T.Const(7)))

    def test_dual_add_subtract_ors_an_from_the_subtract_half(self):
        # Rx=3, Ry=10: Ra=Rx+Ry=13 (AN clear), Rs=Rx-Ry=-7 (AN set). A naive
        # "flags follow Ra only" implementation would miss AN; PRM
        # p.3-21/3-22 says both halves' ALU flags are ORed together.
        fields = dual_add_subtract_fields(False, rs=3, ra=0, rx=1, ry=2)
        _, value, _, astatx = self.astatx_after(
            fields, {1: T.Const(3), 2: T.Const(10)}, T.Unknown("start")
        )
        self.assertEqual(value, (T.Const(13), T.Const(0xFFFFFFF9)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), True)

    def test_dual_add_subtract_ors_az_from_the_add_half(self):
        # Rx=5, Ry=-5: Ra=Rx+Ry=0 (AZ set), Rs=Rx-Ry=10 (AZ clear).
        fields = dual_add_subtract_fields(False, rs=3, ra=0, rx=1, ry=2)
        _, value, _, astatx = self.astatx_after(
            fields, {1: T.Const(5), 2: T.Const(0xFFFFFFFB)}, T.Unknown("start")
        )
        self.assertEqual(value, (T.Const(0), T.Const(10)))
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), True)

    def test_dual_add_subtract_float_ors_an_from_the_subtract_half(self):
        fields = dual_add_subtract_fields(True, rs=3, ra=0, rx=1, ry=2)
        rn, value, op, astatx = self.astatx_after(
            fields,
            {1: T.Const(f32(1.0)), 2: T.Const(f32(3.0))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-dual-add-subtract")
        self.assertEqual(rn, (0, 3))
        self.assertEqual(value, (T.Const(f32(4.0)), T.Const(f32(-2.0))))
        self.assertEqual(T._astatx_known_bit(astatx, T.AN_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.AF_BIT), True)

    # -- MUL/ALU multifunction: Fm=Fxm*Fym, Fa=Fxa+-Fya (PGR Table 12-12) ---

    def test_mulalu_add_variant_writes_multiply_and_add_results(self):
        fields = mulalu_fields(0x18, rm=1, ra=2, rxm=0, rym=0, rxa=0, rya=0)
        values = {
            0: T.Const(f32(2.0)),  # Rxm (R0-3 quad, code 0)
            4: T.Const(f32(3.0)),  # Rym (R4-7 quad, code 0)
            8: T.Const(f32(5.0)),  # Rxa (R8-11 quad, code 0)
            12: T.Const(f32(1.0)),  # Rya (R12-15 quad, code 0)
        }
        rn, value, op, astatx = T._compute(fields, False, values)
        astatx = astatx(T.Unknown("start"))
        self.assertEqual(op, "float-mulalu-add")
        self.assertEqual(rn, (1, 2))
        self.assertEqual(value, (T.Const(f32(6.0)), T.Const(f32(6.0))))
        self.assertEqual(T._astatx_known_bit(astatx, T.AF_BIT), True)
        # 2.0 * 3.0 = 6.0: PRM Table 3-9 flags are known, all clear.
        for bit in (T.MN_BIT, T.MV_BIT, T.MU_BIT, T.MI_BIT):
            self.assertEqual(T._astatx_known_bit(astatx, bit), False)

    def test_mulalu_subtract_variant(self):
        fields = mulalu_fields(0x19, rm=1, ra=2, rxm=0, rym=0, rxa=0, rya=0)
        values = {
            0: T.Const(f32(2.0)),
            4: T.Const(f32(3.0)),
            8: T.Const(f32(5.0)),
            12: T.Const(f32(1.0)),
        }
        rn, value, op, _ = T._compute(fields, False, values)
        self.assertEqual(op, "float-mulalu-subtract")
        self.assertEqual(value, (T.Const(f32(6.0)), T.Const(f32(4.0))))

    def test_unsupported_multifunction_category_raises(self):
        # category=0x04 (SSFR fixed-point RM=R3-0*R7-4, RA=RXA+RYA, PGR
        # Table 12-12) is a real MUL/ALU multifunction row this tracer does
        # not implement (only the float rows 0x18/0x19 are); it must still
        # raise rather than being silently misparsed as something else.
        fields = mulalu_fields(0x04, rm=1, ra=2, rxm=0, rym=0, rxa=0, rya=0)
        with self.assertRaises(ValueError) as raised:
            T._compute(fields, False, {})
        self.assertIn("multifunction category", str(raised.exception))

    # -- ShortCompute float rows (PRM Table 18-2 p.423-425, opcodes 8/9/A/B/F) -

    def short_astatx_after(self, fields, values, old_astatx):
        rn, value, operation, update = T._compute(fields, True, values)
        return rn, value, operation, update(old_astatx)

    def test_short_float_add_and_subtract(self):
        # RN doubles as the Y input and result (Table 18-22): FN = FN +- FX.
        values = {0: T.Const(f32(1.5)), 1: T.Const(f32(2.5))}
        _, add_value, add_op, _ = self.short_astatx_after(
            short_compute(0x8, 0, 1), values, T.Unknown("start")
        )
        _, sub_value, sub_op, _ = self.short_astatx_after(
            short_compute(0x9, 0, 1), values, T.Unknown("start")
        )
        self.assertEqual((add_op, add_value), ("float-add", T.Const(f32(4.0))))
        self.assertEqual((sub_op, sub_value), ("float-subtract", T.Const(f32(-1.0))))

    def test_short_float_convert_reads_only_rx(self):
        # FN = float RX: RN is the destination only, not a second input.
        rn, value, op, _ = self.short_astatx_after(
            short_compute(0xA, 0, 1),
            {0: T.Const(0xDEAD), 1: T.Const(0xFFFFFFFD)},  # RX=-3
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-convert")
        self.assertEqual(value, T.Const(f32(-3.0)))

    def test_short_float_compare_is_status_only(self):
        rn, value, op, astatx = self.short_astatx_after(
            short_compute(0xB, 0, 1),
            {0: T.Const(f32(2.0)), 1: T.Const(f32(2.0))},
            T.Unknown("start"),
        )
        self.assertEqual(op, "float-compare")
        self.assertEqual(T._astatx_known_bit(astatx, T.AZ_BIT), True)

    def test_short_float_multiply(self):
        rn, value, op, astatx = self.short_astatx_after(
            short_compute(0xF, 0, 1),
            {0: T.Const(f32(1.5)), 1: T.Const(f32(2.0))},
            T.Const(0xFFFFFFFF),
        )
        self.assertEqual(op, "float-multiply")
        self.assertEqual(value, T.Const(f32(3.0)))
        for bit in (T.MN_BIT, T.MV_BIT, T.MU_BIT, T.MI_BIT):
            self.assertEqual(T._astatx_known_bit(astatx, bit), False)

    def test_short_float_multiply_flags_negative_and_unknown(self):
        _, _, _, astatx = self.short_astatx_after(
            short_compute(0xF, 0, 1),
            {0: T.Const(f32(-1.5)), 1: T.Const(f32(2.0))},
            T.Const(0),
        )
        self.assertEqual(T._astatx_known_bit(astatx, T.MN_BIT), True)
        _, _, _, astatx = self.short_astatx_after(
            short_compute(0xF, 0, 1),
            {0: T.Unknown("x"), 1: T.Const(f32(2.0))},
            T.Const(0),
        )
        self.assertIsNone(T._astatx_known_bit(astatx, T.MN_BIT))

    def test_short_fixed_multiply_flags(self):
        # PRM Table 18-22 0111 = "RN = RN * RX (ssi)"; Table 3-7 flags.
        def run(a, b):
            return self.short_astatx_after(
                short_compute(0x7, 0, 1),
                {0: T.Const(a), 1: T.Const(b)},
                T.Const(0),
            )

        rn, value, op, astatx = run(3, 0xFFFFFFFE)  # 3 * -2
        self.assertEqual(op, "multiply")
        self.assertEqual(value, T.Const(0xFFFFFFFA))
        self.assertEqual(T._astatx_known_bit(astatx, T.MN_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.MV_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.MU_BIT), False)
        self.assertEqual(T._astatx_known_bit(astatx, T.MI_BIT), False)
        _, value, _, astatx = run(0x7FFFFFFF, 0x7FFFFFFF)  # needs > 32 bits
        self.assertEqual(value, T.Const(1))
        self.assertEqual(T._astatx_known_bit(astatx, T.MV_BIT), True)
        self.assertEqual(T._astatx_known_bit(astatx, T.MN_BIT), False)

    def test_short_compute_opcode_space_is_fully_enumerated(self):
        # All 16 ShortCompute opcodes (PRM Table 18-2) are now implemented;
        # none should reach the "unsupported short compute" fallback.
        for opcode in range(16):
            with self.subTest(opcode=hex(opcode)):
                try:
                    T._compute(
                        short_compute(opcode, 0, 1),
                        True,
                        {0: T.Const(1), 1: T.Const(2)},
                    )
                except ValueError as exc:
                    self.fail("opcode %#x unexpectedly unsupported: %s" % (opcode, exc))

    # -- End-to-end wiring through _execute/_apply_compute -------------------

    def test_dual_result_reaches_both_uregs_via_execute(self):
        fields = dual_add_subtract_fields(False, rs=3, ra=0, rx=1, ry=2)
        state = T.State(1, {1: T.Const(10), 2: T.Const(3)})
        record = insn(
            "2a",
            {
                "cond[4:0]": 0x1F,
                "compute[22:16]": fields["compute[22:16]"],
                "compute[15:0]": fields["compute[15:0]"],
            },
            6,
        )
        executed = T._execute(state, record)[0]
        self.assertEqual(executed.uregs[0], T.Const(13))
        self.assertEqual(executed.uregs[3], T.Const(7))
        self.assertEqual(executed.trace[-1]["result_register"], ["R0", "R3"])
        self.assertEqual(executed.trace[-1]["value"], [13, 7])


class PhaseAOpcodeRegressionTest(TraceRunMixin, unittest.TestCase):
    """Literal Phase A vectors from the public PRM tables in SOURCES.md."""

    def test_type10a_relative_decodes_its_split_fields(self):
        # Type10a_rel's VISA prefix and split reladdr fields are from the PRM
        # Figure 15-4-derived decode table, not an implementation round-trip.
        compute = (0x01 << 12) | (2 << 8) | (3 << 4) | 4
        extra = (
            (1 << 44)
            | (3 << 41)
            | (5 << 38)
            | (0x1F << 33)
            | (1 << 32)
            | (0x15 << 27)
            | (9 << 23)
            | compute
        )
        record = T.decode_at(encode("10a_rel", extra), 0, 0)
        self.assertEqual(
            (record.type_name, record.length_bytes, record.kind),
            ("10a_rel", 6, "confident"),
        )
        self.assertEqual(
            record.fields,
            {
                "d": 1,
                "dmi[2:0]": 3,
                "dmm[2:0]": 5,
                "cond[4:0]": 0x1F,
                "dreg[3:0]": 9,
                "compute[22:16]": 0,
                "compute[15:0]": compute,
                "reladdr[5:5]": 1,
                "reladdr[4:0]": 0x15,
            },
        )

    def test_prm_type2b_prefix_is_a_16_bit_type2c(self):
        # The PRM Figure 14-4 Type2b prefix 110000000 is a first parcel in
        # 0xc000..0xc07f, and the silicon runs those as the 16-bit Type2c (DN2 1.11
        # sw 0x1c0e13 and 0x1c4f4a, DT2 1.16 sw 0x1c32ab, in code that plays every
        # block). build_table.py's NOT_VISA keeps Type2b out of the VISA table.
        from tools.sharc_dev import sharc_visa_tables

        self.assertIsNone(sharc_visa_tables.get_type("2b"))
        record = T.decode_at(encode("2c", 0) + b"\x00" * 4, 0, 0)
        self.assertEqual((record.type_name, record.length_bytes), ("2c", 2))

    def test_alu_copysign_uses_magnitude_and_sign_operands(self):
        # ALUOP 0xe0: FN = FX copysign FY (PRM Table 18-5).
        rn, value, operation, _ = T._compute(
            full_compute(0, 0xE0, 0, 1, 2),
            False,
            {1: T.Const(f32(3.5)), 2: T.Const(f32(-2.0))},
        )
        self.assertEqual(
            (rn, operation, value), (0, "float-copysign", T.Const(f32(-3.5)))
        )

    def test_multifunction_convert_max_and_min_literal_vectors(self):
        # PRM Table 18-16 rows 011010, 011110, and 011111 respectively.
        common = {
            0: T.Const(f32(2.0)),
            4: T.Const(f32(3.0)),
            8: T.Const(3),
            12: T.Const(2),
        }
        for category, expected, operation in (
            (0x1A, T.Const(f32(12.0)), "float-mulalu-convert"),
            (0x1E, T.Const(f32(3.0)), "float-mulmax"),
            (0x1F, T.Const(f32(2.0)), "float-mulmin"),
        ):
            with self.subTest(category=hex(category)):
                # The multiplier half is 2.0 * 3.0; the ALU half selects
                # R8/R12, which are fixed inputs for 1a and float inputs for 1e/f.
                values = (
                    common
                    if category == 0x1A
                    else {**common, 8: T.Const(f32(3.0)), 12: T.Const(f32(2.0))}
                )
                rn, value, actual, _ = T._compute(
                    mulalu_fields(category, 1, 2, 0, 0, 0, 0), False, values
                )
                self.assertEqual((rn, actual), ((1, 2), operation))
                self.assertEqual(value, (T.Const(f32(6.0)), expected))

    def test_scaled_fixed_float_convert_forms(self):
        # The Table 18-5 forms ending "by RY" apply RY as a base-two scale.
        mode1 = T.UREG_CODES["MODE1"]
        cases = (
            (
                0xD9,
                {1: T.Const(f32(3.5)), 2: T.Const(1), mode1: T.Const(0)},
                "fix-scaled",
                T.Const(7),
            ),
            (
                0xDA,
                {1: T.Const(3), 2: T.Const(2)},
                "float-convert-scaled",
                T.Const(f32(12.0)),
            ),
            (
                0xDD,
                {1: T.Const(f32(-3.75)), 2: T.Const(1), mode1: T.Const(0)},
                "trunc-scaled",
                T.Const(0xFFFFFFF9),
            ),
        )
        for opcode, values, operation, expected in cases:
            with self.subTest(opcode=hex(opcode)):
                _, value, actual, _ = T._compute(
                    full_compute(0, opcode, 0, 1, 2), False, values
                )
                self.assertEqual((actual, value), (operation, expected))

    def test_alu_carry_and_borrow_consume_astatx_ac(self):
        # Table 18-5 ALUOP 05: RX+RY+ci; 06: RX-RY+ci-1.  AC supplies ci.
        astatx = T.UREG_CODES["ASTATX"]
        cases = (
            (0x05, 0xFFFFFFFF, 0, 1, "add-with-carry", 0, True),
            (0x06, 0, 0, 0, "subtract-with-borrow", 0xFFFFFFFF, False),
            (0x06, 0, 0, 1, "subtract-with-borrow", 0, True),
        )
        for opcode, rx, ry, carry, operation, expected, ac in cases:
            with self.subTest(opcode=hex(opcode), carry=carry):
                _, value, actual, update = T._compute(
                    full_compute(0, opcode, 0, 1, 2),
                    False,
                    {
                        1: T.Const(rx),
                        2: T.Const(ry),
                        astatx: T.Const(carry << T.AC_BIT),
                    },
                )
                self.assertEqual((actual, value), (operation, T.Const(expected)))
                self.assertEqual(
                    T._astatx_known_bit(update(T.Unknown("start")), T.AC_BIT), ac
                )


if __name__ == "__main__":
    unittest.main()
