"""Tests for tools/sharc_widthaudit.py.

The static lint and the per-form WIDTH_RULES functions need no firmware
image, so they run unconditionally. The concrete audit (audit_root, needing
a real run_init() plus a call_render()/call_frame()) is a firmware
integration test like tests/test_sharc_harness.py's own FrameRenderFromInit
Test -- marked slow and skipped when the DT2 1.16 bytes are absent.
"""

import os
import pathlib
import sys
import unittest

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools"))
import sharc_widthaudit as wa  # noqa: E402

DT2_116_BLOB = pathlib.Path("out/sections/dt2-1.16/section_7_BLOB.bin")


class FormsWithWidthFieldsTest(unittest.TestCase):
    def test_finds_the_known_width_bearing_forms(self):
        forms = wa._forms_with_width_fields()
        # tools/sharcspec/decode_table.json's own set, as audited by hand
        # when WIDTH_RULES was written (this module's own docstring/git
        # history) -- a form disappearing here (or a new one appearing)
        # means decode_table.json changed shape and WIDTH_RULES needs a
        # fresh look, so this is an exact-set assertion, not a subset one.
        self.assertEqual(
            forms,
            {
                "3a": {"l"},
                "3b": {"l", "w", "x"},
                "3d": {"l", "w", "x", "ex"},
                "4b": {"l", "w", "x"},
                "4d": {"l", "w", "x"},
                "7a": {"l", "w"},
                "11a": {"x"},
                "11c": {"x"},
                "14a": {"l"},
                "14d": {"l", "w", "x", "ex"},
                "15a": {"l"},
                "15b": {"l"},
                "19a_scaled": {"w"},
            },
        )


class StaticAuditTest(unittest.TestCase):
    def test_every_width_bearing_handler_reads_its_own_fields(self):
        # This is the repo-wide invariant this tool's --static mode
        # checks: a regression here means a form's handler stopped
        # mentioning one of its own decoded width fields (the "field
        # decoded, never consulted" half of the bug class this module's
        # docstring describes -- e.g. Type3d before this lane's own fix
        # would have failed this exact assertion, had this tool existed
        # then).
        self.assertEqual(wa.static_audit(), [])


class WidthRulesTest(unittest.TestCase):
    """One assertion per WIDTH_RULES entry against ACCESS_WIDTHS/the PRM
    citation in that rule's own docstring -- independent of whatever a
    sharc_core handler currently does, since the whole point of this table
    is to catch a handler that disagrees with it."""

    def test_access3_rules_match_access_widths_table(self):
        for rule in (wa._w_access3,):
            self.assertEqual(rule({"l": 0, "x": 0, "w": 0}), "byte")
            self.assertEqual(rule({"l": 0, "x": 1, "w": 0}), "byte-sign-extended")
            self.assertEqual(rule({"l": 1, "x": 0, "w": 0}), "short-word")
            self.assertEqual(rule({"l": 1, "x": 1, "w": 0}), "short-word-sign-extended")
            self.assertEqual(rule({"l": 0, "x": 1, "w": 1}), "normal-word")
            self.assertEqual(rule({"l": 1, "x": 1, "w": 1}), "long-word")
            self.assertIsNone(rule({"l": 0, "x": 0, "w": 1}))

    def test_4b_has_no_long_word_row(self):
        # PRM p.13-32, BH/BHSE (Type 4b): (1, 1, 1) has no suffix.
        self.assertEqual(wa._w_4b({"l": 1, "x": 1, "w": 1}), "normal-word")
        self.assertEqual(wa._w_4b({"l": 1, "x": 0, "w": 0}), "short-word")
        self.assertEqual(wa._w_4b({"l": 0, "x": 1, "w": 0}), "byte-sign-extended")
        self.assertIsNone(wa._w_4b({"l": 0, "x": 1, "w": 1}))

    def test_3d_stops_on_w1_waccess_and_matches_access3_on_w0(self):
        self.assertIsNone(wa._w_3d({"l": 0, "x": 0, "w": 1}))
        self.assertIsNone(wa._w_3d({"l": 1, "x": 1, "w": 1}))
        self.assertEqual(wa._w_3d({"l": 0, "x": 0, "w": 0}), "byte")
        self.assertEqual(wa._w_3d({"l": 1, "x": 0, "w": 0}), "short-word")
        self.assertEqual(wa._w_3d({"l": 1, "x": 1, "w": 0}), "short-word-sign-extended")

    def test_14d_stops_on_w1_ex1_and_x1_store(self):
        self.assertIsNone(wa._w_14d({"w": 1, "ex": 0, "l": 0, "x": 0, "d": 0}))
        self.assertIsNone(wa._w_14d({"w": 0, "ex": 1, "l": 0, "x": 0, "d": 0}))
        self.assertIsNone(wa._w_14d({"w": 0, "ex": 0, "l": 0, "x": 1, "d": 1}))
        self.assertEqual(wa._w_14d({"w": 0, "ex": 0, "l": 0, "x": 0, "d": 0}), "byte")
        self.assertEqual(
            wa._w_14d({"w": 0, "ex": 0, "l": 0, "x": 1, "d": 0}),
            "byte-sign-extended",
        )

    def test_3a_selects_long_word_on_l(self):
        # 2026-09-25: Type3a's (LW) option is now implemented (see
        # forms_move.py's _type_3a docstring and tools/sharc_harness.py's
        # FRAME_MILESTONE), the same register-pair access _w_pair already
        # covers for Type14a/15a/15b -- _w_3a mirrors it exactly.
        self.assertEqual(wa._w_3a({"l": 0}), "normal-word")
        self.assertEqual(wa._w_3a({"l": 1}), "long-word")

    def test_pair_forms_select_long_word_on_l(self):
        for rule in (wa._w_pair, wa._w_3a):
            self.assertEqual(rule({"l": 0}), "normal-word")
            self.assertEqual(rule({"l": 1}), "long-word")

    def test_7a_selects_short_word_on_l_only(self):
        self.assertEqual(wa._w_7a({"l": 0, "w": 0}), "normal-word")
        self.assertEqual(wa._w_7a({"l": 0, "w": 1}), "normal-word")
        self.assertEqual(wa._w_7a({"l": 1, "w": 0}), "short-word")
        self.assertEqual(wa._w_7a({"l": 1, "w": 1}), "short-word")

    def test_19a_scaled_selects_on_w(self):
        self.assertEqual(wa._w_19a_scaled({"w": 0}), "short-word")
        self.assertEqual(wa._w_19a_scaled({"w": 1}), "normal-word")


@pytest.mark.slow
@unittest.skipUnless(DT2_116_BLOB.exists(), "DT2 1.16 firmware bytes are not available")
class ConcreteAuditTest(unittest.TestCase):
    """audit_root() against the real firmware -- run_init() is ~1.24M
    instructions (see tools/sharc_harness.py's own docstring), so this is
    a firmware integration test like tests/test_sharc_harness.py's
    FrameRenderFromInitTest, not a unit test."""

    def test_voice_root_has_zero_mismatches(self):
        report = wa.audit_root("voice", image="dt2-1.16")
        self.assertEqual(report.mismatches, [])
        self.assertGreater(report.events_checked, 0)

    def test_frame_root_has_zero_mismatches(self):
        report = wa.audit_root("frame", image="dt2-1.16", max_steps=500_000)
        self.assertEqual(report.mismatches, [])
        self.assertGreater(report.events_checked, 0)
        # Every width-bearing form this lane fixed (3d, 4b) is actually
        # reached by the frame render -- a regression that made either
        # one stop early (an "unsupported" halt) would silently shrink
        # this to zero checks instead of failing loudly, so pin both.
        self.assertIn("3d", report.per_form_checked)
        self.assertIn("4b", report.per_form_checked)


if __name__ == "__main__":
    unittest.main()
