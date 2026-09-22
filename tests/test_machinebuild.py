"""`tools/machinebuild.py`'s `audit_literals` static check.

`build_rank_shim` once tail-jumped to 1.15C's `RANK_INSERT` on every image
(fixed to take the profile's own address); `audit_literals` is the guard that
would have caught that in milliseconds instead of a hung boot.
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'tools'))

import machinebuild as mb
import machinepatch as mp
import machineprofile as prof

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DT2_116_IMAGE = os.path.join(REPO, 'out', 'sections', 'dt2-1.16',
                             'section_3_MAIN_OS.bin')
DT2_116_AVAILABLE = (os.path.exists(DT2_116_IMAGE)
                     and prof.sha256_file(DT2_116_IMAGE)
                     == '57bb4dfa8df07d846adc72fdb4fb0d3cd3c5680c524bf498338460207e008e7d')


class AuditLiteralsTest(unittest.TestCase):
    def test_a_1_15c_rank_insert_is_flagged_against_1_16(self):
        addr = prof.DT2_116['cave_b']
        old = bytes(8)
        new = b'\x4e\xf9' + bytes.fromhex('40198948') + bytes(2)
        with self.assertRaises(SystemExit) as caught:
            mb.audit_literals([(addr, old, new)], prof.DT2_116)
        self.assertIn('40198948', str(caught.exception))
        self.assertIn('1.15C address', str(caught.exception))


@unittest.skipUnless(DT2_116_AVAILABLE, 'no dt2-1.16 MAIN OS image on disk')
class FullPlanAuditTest(unittest.TestCase):
    def test_the_nine_part_1_16_plan_passes_the_audit(self):
        profile = prof.DT2_116
        read = prof.image_reader(DT2_116_IMAGE)
        flash_cave_addr, _flash_cave_size = profile['flash_cave']
        writes = mp.plan_b(read, flash_cave_addr, parts=mp.PARTS,
                           profile=profile)
        mb.audit_literals(writes, profile)


if __name__ == '__main__':
    unittest.main()
