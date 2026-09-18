# pyright: reportMissingImports=false
"""tools/snapdiff.py: page reads, run merging and table labels."""

import os
import sys
import unittest
import zlib

sys.path.insert(0, os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'tools'))

import snapdiff  # noqa: E402

SRAM = 0x80000000


def blob(writes):
    page = bytearray(snapdiff.PAGE)
    for addr, data in writes:
        page[addr - SRAM:addr - SRAM + len(data)] = data
    return {'pages': {SRAM: zlib.compress(bytes(page))}}


class SnapdiffTest(unittest.TestCase):
    def test_missing_page_reads_as_zero(self):
        self.assertEqual(snapdiff.read({'pages': {}}, SRAM, SRAM + 4), bytes(4))

    def test_runs_merge_within_gap(self):
        a = blob([])
        b = blob([(0x80003340, b'\x01'), (0x80003343, b'\x02'), (0x80003350, b'\x03')])
        lo, hi = 0x80003300, 0x80003400
        got = list(snapdiff.runs(snapdiff.read(a, lo, hi), snapdiff.read(b, lo, hi), lo, 4))
        self.assertEqual(got, [(0x80003340, 0x80003344), (0x80003350, 0x80003351)])

    def test_labels(self):
        self.assertEqual(snapdiff.label(0x80003cd0), 'track_9a[0]+0x0')
        self.assertEqual(snapdiff.label(0x80003cd0 + 2 * 0x9a + 5), 'track_9a[2]+0x5')
        self.assertEqual(snapdiff.label(0x80003340), '')
        self.assertEqual(snapdiff.label(0x80005348 + 0x10), 'tx_frame+0x10')
        self.assertEqual(snapdiff.label(SRAM), '')


if __name__ == '__main__':
    unittest.main()
