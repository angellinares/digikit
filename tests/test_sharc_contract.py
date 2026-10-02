"""Sanity checks for tools/sharc_contract.py against the real dt2-1.16
program database (out/sharcdb/dt2-1.16.sqlite, built from out/sections --
see the worktree's `out`/`sections` symlinks). Skipped, not marked slow: it
only opens the already-built database (sharc.load() rebuilds only when the
blob's sha256 or DB_VERSION has moved on), the same thing tools/sharc.py
itself does by default.

Each assertion here pins a fact already recorded in
docs/findings/06-sharc-engine-and-startup.md or the 2026-09-24 handover, so
a regression in the reach walk, the known-structure grouping, or the writer
classification shows up as a test failure instead of a silent contract
change.
"""

import os
import pathlib
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools"))
import sharc  # noqa: E402
import sharc_contract as C  # noqa: E402

DT2_116_DB = pathlib.Path("out/sharcdb/dt2-1.16.sqlite")


@unittest.skipUnless(
    DT2_116_DB.exists(), "out/sharcdb/dt2-1.16.sqlite is not available"
)
class ContractTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.img = sharc.load("dt2-1.16")

    # --- reach --------------------------------------------------------------

    def test_reach_from_render_root_matches_known_count(self):
        # docs/findings/06 / the 2026-09-24 handover: "Reach from 0x1c2b24
        # over call+jump edges: 154 functions, 11,575 instructions."
        reach = C.reach_functions(self.img, 0x1C2B24)
        self.assertEqual(len(reach), 154)
        # 11575 -> 11577: 2b/2c width-resolver fix re-syncs DT2 1.16 0x1c32ab.
        self.assertEqual(C.instruction_count(self.img, reach), 11577)

    def test_render_reach_includes_the_known_call_chain(self):
        # FUN_1c2b24 -> 0x1c24e9 (per-track unpack) -> FUN_1c642a (per-slot
        # dispatch) -> master stage 0x1c207b.
        reach = C.reach_functions(self.img, 0x1C2B24)
        for fn in (0x1C24E9, 0x1C642A, 0x1C207B):
            self.assertIn(fn, reach, "0x%x missing from FUN_1c2b24's reach" % fn)

    def test_audio_task_command3_root_reaches_the_render_root(self):
        reach = C.reach_functions(self.img, 0x1C7671)
        self.assertIn(0x1C2B24, reach)

    # --- known-structure grouping --------------------------------------------

    def test_render_reads_the_voice_records(self):
        contract = C.build(self.img, 0x1C2B24)
        labels = {o["label"]: o for o in contract["objects"]}
        self.assertIn("per-track record", labels)
        obj = labels["per-track record"]
        self.assertEqual(obj["range"], "0x2506ec-0x25226c")
        self.assertGreater(obj["n_read_addrs"], 0)

    def test_boot_fills_the_mix_table_base(self):
        # docs/findings/06, "Boot fills the pointer table the mix reads":
        # FUN_1c15e3's one resolved writer at 0x1c16cc.
        contract = C.build(self.img, 0x1C2B24)
        labels = {o["label"]: o for o in contract["objects"]}
        self.assertIn("mix table base", labels)
        obj = labels["mix table base"]
        self.assertTrue(obj["externally_provided"])
        writer_sites = {s for w in obj["writers"] for s in w["sites"]}
        self.assertIn("0x1c16cc", writer_sites)
        boot_writer = next(w for w in obj["writers"] if "0x1c16cc" in w["sites"])
        self.assertEqual(boot_writer["class"], "boot/init")

    # --- the reset vector is boot, not an ISR --------------------------------

    def test_reset_vector_is_not_classified_as_an_isr(self):
        # roots.kind='interrupt_vector', 'slot 1 RSTI' targets the same sw
        # as the loader_entry root (0x1c1338): it is the boot vector.
        rows = self.img.sql(
            "SELECT sw FROM roots WHERE image=? AND kind='interrupt_vector' AND note='slot 1 RSTI'",
            self.img.name,
        )
        self.assertEqual(rows, [(C.RESET_VECTOR_SW,)])
        root_index = C._RootIndex(self.img)
        # FUN_1c15e3 is boot-only (its sole caller is inside the boot chain,
        # 0x1c8092 -> FUN_1c7ff9 -> ... -> loader_entry) -- confirm it is
        # classified as boot/init, not misread as an ISR via the reset
        # vector's own interrupt_vector root row.
        self.assertEqual(root_index.classify_writer_function(0x1C15E3), "boot/init")

    # --- 0x255970 has no literal writer --------------------------------------

    def test_machine_type_cache_has_no_resolved_writer(self):
        # docs/findings/06: "The cache at 0x255970 sits in a fill block and
        # has no literal store anywhere; how it is refreshed from the
        # ColdFire frame is [O]."
        rows = C._global_ptr_rows(self.img, "store", 0x255970, 0x255972)
        self.assertEqual(rows, [])

    # --- peripheral windows ---------------------------------------------------

    def test_peripheral_windows(self):
        self.assertTrue(C.is_peripheral(0x31000004))
        self.assertTrue(C.is_peripheral(0x300C0))
        self.assertFalse(C.is_peripheral(0x2412C8))
        self.assertFalse(C.is_peripheral(0x40000000))


if __name__ == "__main__":
    unittest.main()
