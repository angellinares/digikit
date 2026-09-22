"""emu/semscan.py against the pinned per-version extracts.

Same skip-if-image-absent pattern as tests/test_symbols_real.py: each image
is optional, so each test method is individually skipped when its own
extract is not present rather than skipping the whole module.
"""

import unittest
from pathlib import Path

from emu import semscan, symbols

# The pinned per-version extracts, not the shared sections/ directory, which
# holds whichever firmware was extracted last (see tests/test_symbols_real.py).
DT15 = Path("out/sections/dt2-1.15C/section_3_MAIN_OS.bin")
DT16 = Path("out/sections/dt2-1.16/section_3_MAIN_OS.bin")
DN11 = Path("out/sections/dn2-1.11/section_3_MAIN_OS.bin")

# emu/pit.py: PIT3 is the display frame timer, claimed by the display
# module partway through boot and modelled from then on -- see
# emu/symbols.py:display_sem and emu/pit.py's own module docstring.
PIT3_VECTOR = 208


class SemscanTest(unittest.TestCase):
    def _check_bq_pair_and_display_sem(self, path):
        profile = symbols.resolve(path.read_bytes())
        never_fake = semscan.never_fake_semaphores(path.read_bytes())
        # bq_free_sem/bq_ready_sem are plain task-code posters (neither give
        # is inside an ISR at all) -- see emu/symbols.py's bq_free_giver
        # comment -- so the scan must find both with no modeled_vectors at
        # all.
        self.assertEqual(never_fake, {profile.bq_free_sem, profile.bq_ready_sem})

        # display_sem's only poster is the display module's own PIT3 ISR.
        # With PIT3 unmodelled it is correctly left out (an ISR of a source
        # nothing here emulates never actually posts); once PIT3 is passed
        # in as modelled, its poster is reachable from a modelled vector and
        # must join never_fake.
        with_pit3 = semscan.never_fake_semaphores(
            path.read_bytes(), modeled_vectors=frozenset({PIT3_VECTOR}))
        self.assertIn(profile.display_sem, with_pit3)
        self.assertTrue({profile.bq_free_sem, profile.bq_ready_sem} <= with_pit3)

    @unittest.skipUnless(DT16.exists(), "DT2 1.16 image absent")
    def test_dt2_1_16(self):
        self._check_bq_pair_and_display_sem(DT16)

    @unittest.skipUnless(DT15.exists(), "DT2 1.15C image absent")
    def test_dt2_1_15C(self):
        self._check_bq_pair_and_display_sem(DT15)

    @unittest.skipUnless(DN11.exists(), "DN2 1.11 image absent")
    def test_dn2_1_11(self):
        self._check_bq_pair_and_display_sem(DN11)

    @unittest.skipUnless(DT16.exists(), "DT2 1.16 image absent")
    def test_vector_scan_finds_all_known_vectors_on_dt2_1_16(self):
        # Positive control for the vector-table scan itself (step 1 of the
        # module docstring's algorithm), independent of which semaphores it
        # feeds into: reaches into the private _find_vectors because this
        # is whitebox-checking the scan's completeness, not the public
        # contract. Counts confirmed by cross-checking every one of the 31
        # sites against a full, unrestricted linear disassembly of the
        # image (which desyncs and only finds 22 of them -- see the module
        # docstring's note on why _find_vectors does not just disassemble
        # the whole image) and against scratch/semscan.py's Ghidra-based
        # xrefs for the 7 that carry a give/give_b poster.
        data = DT16.read_bytes()
        vectors = semscan._find_vectors(data, symbols.LOAD_ADDR)
        self.assertEqual(len(vectors), 31)
        self.assertEqual(len({v['handler'] for v in vectors}), 26)


if __name__ == '__main__':
    unittest.main()
