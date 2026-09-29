"""Opt-in regression: emu.checkpoint.make's default (no-coverage) ladder path
produces guest state byte-identical to the old coverage=True path.

`_make_fast` (see emu/checkpoint.py's own docstring) skips installing
emu.dspboot's global per-instruction `cover` hook and drives execution with
exact `uc.emu_start(pc, 0, count=delta)` calls instead. `cover` and its
`extra_hook` call are purely diagnostic -- neither ever writes a register or
memory -- so removing it should never change what a rung's snapshot holds,
only how fast it was built and whether `seen`/`stall_pcs`/`curve` got
populated. This is what finds out if that claim ever breaks (e.g. a future
edit to `_make_fast` that drifts from `_make_with_coverage`'s hook set, or an
off-by-one in where it stops relative to the old hook's timing -- see
docs/findings/07-emulator.md, "cold-boot cover-hook cost").
"""

import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


@unittest.skipUnless(
    os.environ.get("DT2_SYX"), "set DT2_SYX=Digitakt_II_OS1.16.syx to run this"
)
class CheckpointMakeFastPathTest(unittest.TestCase):
    def test_fast_and_coverage_ladders_are_byte_identical(self):
        import emu.checkpoint as ck
        from tools.snapeq import compare

        # Small enough to run in a couple of seconds in either mode; the
        # equivalence claim does not depend on how far the ladder runs.
        points = [1_000_000, 2_000_000]
        with (
            tempfile.TemporaryDirectory() as cov_dir,
            tempfile.TemporaryDirectory() as fast_dir,
        ):
            cov_saved = ck.make(
                points, prefix=os.path.join(cov_dir, "boot"), coverage=True
            )
            fast_saved = ck.make(
                points, prefix=os.path.join(fast_dir, "boot"), coverage=False
            )
            self.assertEqual(len(cov_saved), len(points))
            self.assertEqual(len(fast_saved), len(points))

            for cov_entry, fast_entry in zip(cov_saved, fast_saved, strict=True):
                cov_at, cov_path = cov_entry[0], cov_entry[1]
                fast_at, fast_path = fast_entry[0], fast_entry[1]
                self.assertEqual(cov_at, fast_at)
                diff = compare(cov_path, fast_path)
                self.assertIsNone(diff, diff)
                # task_create_hits must still be populated (by the
                # scoped hook, unaffected by `coverage`) even though
                # `seen` -- the removed global hook's own output -- is
                # empty in the fast path.
                cov_ntasks, fast_ntasks = cov_entry[4], fast_entry[4]
                self.assertEqual(cov_ntasks, fast_ntasks)
                fast_nseen = fast_entry[3]
                self.assertEqual(fast_nseen, 0)

            with open(os.path.join(cov_dir, ".ladder.json")) as fh:
                cov_cfg = json.load(fh)
            with open(os.path.join(fast_dir, ".ladder.json")) as fh:
                fast_cfg = json.load(fh)
            self.assertTrue(cov_cfg["coverage"])
            self.assertFalse(fast_cfg["coverage"])


if __name__ == "__main__":
    unittest.main()
