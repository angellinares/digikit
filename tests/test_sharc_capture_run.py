"""Tests for tools/sharc_capture_run.py's pure helpers: argument parsing and
the constants run_natural_track_refresh() depends on. No firmware or
snapshot needed -- see tests/test_sharc_replay.py's own docstring for the
project convention this follows (pure helpers always run; anything that
needs a real Machine/snapshot is exercised by hand, per the module's own
docstring).
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools"))
import sharc_capture_run as scr  # noqa: E402


class ParseTrackTypePokeTest(unittest.TestCase):
    def test_decimal(self):
        self.assertEqual(scr.parse_track_type_poke("3:2"), (3, 2))

    def test_hex(self):
        self.assertEqual(scr.parse_track_type_poke("0xf:0x5"), (15, 5))

    def test_missing_colon_rejected(self):
        with self.assertRaises(ValueError):
            scr.parse_track_type_poke("32")

    def test_track_out_of_range_rejected(self):
        with self.assertRaises(ValueError):
            scr.parse_track_type_poke("16:2")
        with self.assertRaises(ValueError):
            scr.parse_track_type_poke("-1:2")


class ParseSlotPokeTest(unittest.TestCase):
    def test_decimal_and_hex(self):
        self.assertEqual(scr.parse_slot_poke("0:7"), (0, 7))
        self.assertEqual(scr.parse_slot_poke("0xf:0x400"), (15, 0x400))

    def test_rejects_bad_shape_and_range(self):
        for spec in ("07", "16:7", "0:0x10000"):
            with self.assertRaises(ValueError):
                scr.parse_slot_poke(spec)


class ParsePressTest(unittest.TestCase):
    def test_name_with_space(self):
        self.assertEqual(scr.parse_press("TRIG 1@0x10"), ("TRIG 1", 16))

    def test_rejects_missing_at_or_name(self):
        for spec in ("NO", "@100"):
            with self.assertRaises(ValueError):
                scr.parse_press(spec)


class CalibrationDefaultsTest(unittest.TestCase):
    """--settle-smoother, --poke-slot, --press and --trig-hold are opt-in:
    the defaults keep the old behaviour."""

    def test_defaults_off(self):
        args = scr.parse_args(["snap", "--out", "x.dt2cap", "--kind", "note"])
        self.assertFalse(args.settle_smoother)
        self.assertEqual(args.poke_slot, [])
        self.assertEqual(args.press, [])
        self.assertEqual(args.trig_hold, 0)

    def test_slot_halfword_is_tx_0xe0(self):
        # Row halfword 0x2d; the frame's per-track block at TX 0xda starts at
        # row halfword 0x2a, so the slot lands at TX 0xda + 6 = 0xe0.
        self.assertEqual(0xDA + 2 * (scr.SLOT_HALFWORD - 0x2A), 0xE0)


class ParseMemRangeTest(unittest.TestCase):
    def test_hex_range(self):
        self.assertEqual(
            scr.parse_mem_range("0x8c000000:0x8c000010"), (0x8C000000, 0x8C000010)
        )

    def test_missing_colon_rejected(self):
        with self.assertRaises(ValueError):
            scr.parse_mem_range("0x8c000000")

    def test_hi_must_exceed_lo_rejected(self):
        with self.assertRaises(ValueError):
            scr.parse_mem_range("0x10:0x10")
        with self.assertRaises(ValueError):
            scr.parse_mem_range("0x10:0x5")


class ParseArgsPreInstrsTest(unittest.TestCase):
    """Lane A3: --pre-instrs lets the real per-track kit-load-and-refresh
    (run_natural_track_refresh()'s own docstring) fire before the timed
    capture phase, instead of every capture seeing the SRAM row's
    zero-initialized reset value. Default 0 must keep every existing
    capture's exact behaviour (an opt-in flag, per this lane's own scope)."""

    def _parse(self, extra=()):
        return scr.parse_args(
            ["snap.snap", "--out", "out.dt2cap", "--kind", "idle", *extra]
        )

    def test_default_is_zero(self):
        self.assertEqual(self._parse().pre_instrs, 0)

    def test_decimal_value(self):
        self.assertEqual(
            self._parse(["--pre-instrs", "15000000"]).pre_instrs, 15_000_000
        )

    def test_hex_value(self):
        self.assertEqual(self._parse(["--pre-instrs", "0xE00000"]).pre_instrs, 0xE00000)

    def test_combines_with_poke_track_type(self):
        args = self._parse(["--pre-instrs", "15000000", "--poke-track-type", "3:2"])
        self.assertEqual(args.pre_instrs, 15_000_000)
        self.assertEqual(args.poke_track_type, ["3:2"])


class NaturalRefreshConstantsTest(unittest.TestCase):
    """KIT_LOAD_FN/ROW_REFRESH_FN are docs/findings/04-coldfire-dsp-link.md
    addresses (FUN_4002d9c4, FUN_4002d438), pinned so a future edit that
    moves either constant is caught here rather than silently missing the
    hook at runtime."""

    def test_kit_load_fn_address(self):
        self.assertEqual(scr.KIT_LOAD_FN, 0x4002D9C4)

    def test_row_refresh_fn_address(self):
        self.assertEqual(scr.ROW_REFRESH_FN, 0x4002D438)

    def test_run_natural_track_refresh_is_callable(self):
        self.assertTrue(callable(scr.run_natural_track_refresh))


class _FakeUc:
    """Just enough of Unicorn's `uc` surface for ready_to_force(): one
    register, SR, whose value is fixed at construction."""

    def __init__(self, sr):
        self._sr = sr

    def reg_read(self, _reg):
        return self._sr


class _FakeMachine:
    def __init__(self, sr):
        self.uc = _FakeUc(sr)


class ReadyToForceTest(unittest.TestCase):
    """ready_to_force() guards tools/sharc_capture_run.py's periodic
    vector-191 forcing against re-entering vector_191_handler on top of an
    unfinished previous call (Lane H1, docs/findings/04-coldfire-dsp-link.md
    "Lane H1: the DSPI2 forcing loop re-entered its own handler"): measured
    by execution, one call costs 45,000-60,000 ColdFire instructions to
    return, comparable to or larger than --force-period's own 50,000
    default, and Machine.raise_vector() does not itself check the current
    interrupt mask before jumping to the handler."""

    def _sr(self, ipl):
        return (ipl & 0x07) << 8

    def test_level_none_always_ready(self):
        # An unresolved profile keeps the old unconditional behaviour --
        # silently refusing every forced frame would be worse.
        self.assertTrue(scr.ready_to_force(_FakeMachine(self._sr(7)), None))

    def test_ready_when_ipl_below_level(self):
        m = _FakeMachine(self._sr(3))
        self.assertTrue(scr.ready_to_force(m, 5))

    def test_not_ready_when_ipl_at_level(self):
        # Already inside a same-priority handler (most likely a previous
        # forced call that has not returned yet) -- do not re-enter it.
        m = _FakeMachine(self._sr(5))
        self.assertFalse(scr.ready_to_force(m, 5))

    def test_not_ready_when_ipl_above_level(self):
        m = _FakeMachine(self._sr(6))
        self.assertFalse(scr.ready_to_force(m, 5))

    def test_ready_at_ipl_zero(self):
        m = _FakeMachine(self._sr(0))
        self.assertTrue(scr.ready_to_force(m, 5))


class TrigChannelBitTest(unittest.TestCase):
    """Lane J1: trig_channel_bit() generalizes TRIG_1_CHANNEL/TRIG_1_BIT to
    any 0-indexed track, via emu/panelin.py's code_for() inverse."""

    def test_track_0_matches_trig_1_constants(self):
        self.assertEqual(scr.trig_channel_bit(0), (scr.TRIG_1_CHANNEL, scr.TRIG_1_BIT))

    def test_track_2(self):
        # TRIG 3 (track 2): code 27 -> channel 3, bit 2.
        self.assertEqual(scr.trig_channel_bit(2), (3, 2))

    def test_track_15_last_valid(self):
        # TRIG 16 (track 15): code 40 -> channel 4, bit 7.
        self.assertEqual(scr.trig_channel_bit(15), (4, 7))

    def test_track_out_of_range_rejected(self):
        with self.assertRaises(ValueError):
            scr.trig_channel_bit(16)
        with self.assertRaises(ValueError):
            scr.trig_channel_bit(-1)


if __name__ == "__main__":
    unittest.main()
