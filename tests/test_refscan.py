"""refscan.extract_refs: which operand tokens count as absolute addresses."""

import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "tools"))

from refscan import extract_refs  # noqa: E402

LOW = 0x10000


class ExtractRefs(unittest.TestCase):
    def test_absolute_long(self):
        self.assertEqual(extract_refs("$400dd6a6.l", LOW), [0x400DD6A6])

    def test_immediate(self):
        self.assertEqual(extract_refs("#$800068e4, d0", LOW), [0x800068E4])

    def test_register_displacement_is_not_an_address(self):
        self.assertEqual(extract_refs("$14(a7), a0", 0), [])
        self.assertEqual(extract_refs("-$14(a7), a0", 0), [])
        self.assertEqual(extract_refs("$400dd6a6(a2), a0", LOW), [])

    def test_a_rejected_token_does_not_shrink_into_a_hit(self):
        # The lookahead used to fail on "$400dd6a6(" and the regex then backed
        # off to "$400dd6a", reporting 0x400dd6a -- above the default floor.
        self.assertEqual(extract_refs("$400dd6a6(a2), a0", LOW), [])

    def test_pc_relative_target_is_an_address(self):
        # disasm() resolves PC-relative operands to the absolute target, so
        # this names 0x400dd6a6 -- the DN2 kit SAVE reaches the per-sound SAVE
        # exactly this way, `lea %pc@(0x400dd6a6),%a4` then `jsr %a4@`.
        self.assertEqual(extract_refs("$400dd6a6(pc), a4", LOW), [0x400DD6A6])

    def test_pc_relative_indexed_target_is_an_address(self):
        self.assertEqual(extract_refs("$40012340(pc,d0.w), a0", LOW), [0x40012340])


if __name__ == "__main__":
    unittest.main()
