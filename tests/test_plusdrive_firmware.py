"""Opt-in, slow: a built +Drive image passes DT2 1.16's own code.

Builds an image from a short synthetic stereo 44.1 kHz WAV (so the
resampler runs) with the built-in project from the extracted 1.16 MAIN OS,
then runs tools/plusdrive_check.py's bounded calls on a restored 1.16
snapshot: mount, reference resolution (FUN_4015b178/FUN_4015ab5c), the
firmware's own content hash (FUN_4015af0c), the project record's COKi check
and decode (FUN_400c0c2c), the slot list "Load all samples" walks
(FUN_4004e598), and the sample loader itself (FUN_40154540) up to the
FlexBus pages and slot header it sends. About two minutes.

    DT2_SYX=Digitakt_II_OS1.16.syx uv run python -m pytest --slow \\
        tests/test_plusdrive_firmware.py
"""

import math
import os
import tempfile
import unittest

import pytest

import tools.plusdrive as pd
import tools.plusdrive_check as pc
from tests.test_plusdrive import _write_wav

_MAIN_OS = os.environ.get("DT2_MAIN_IMG", pd.DEFAULT_MAIN_OS)


@pytest.mark.slow
@unittest.skipUnless(
    os.environ.get("DT2_SYX"), "set DT2_SYX=Digitakt_II_OS1.16.syx to run this"
)
@unittest.skipUnless(os.path.exists(_MAIN_OS), "needs the DT2 1.16 MAIN OS image")
@unittest.skipUnless(os.path.exists(pc.DEFAULT_SNAPSHOT), "needs the 1.16 snapshot")
class PlusDriveFirmwareTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        samples = os.path.join(cls.tmp.name, "samples")
        os.makedirs(samples)
        frames = [
            v
            for i in range(9000)
            for v in (
                round(12000 * math.sin(i / 5)),
                round(-9000 * math.sin(i / 11)),
            )
        ]
        _write_wav(os.path.join(samples, "tone.wav"), 44100, 2, frames)
        with open(_MAIN_OS, "rb") as f:
            main_os = f.read()
        cls.image = os.path.join(cls.tmp.name, "dt2.img")
        cls.entry = pd.build(samples, cls.image, main_os=main_os)[0]
        cls.r = pc.run_checks(cls.image, cls.entry["id"], main_os=_MAIN_OS)

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def test_mount_indexes_the_file(self):
        m = self.r["mount"]
        self.assertEqual(m["mount_d0"], 0)
        self.assertEqual(m["mounted"], 1)
        self.assertEqual(m["id_valid"], 1)
        self.assertTrue(m["hash_index_has_file"])
        self.assertEqual(m["hash_find_id"], self.entry["id"])

    def test_reference_resolves_exactly(self):
        r = self.r["resolve"]
        self.assertEqual(r["resolve_d0"], 1)  # id, seq, hash and size all match
        self.assertEqual(r["resolve_id"], self.entry["id"])
        self.assertEqual(r["key_d0"], 0)
        self.assertEqual(r["key"], self.entry["ref"])

    def test_firmware_hash_matches(self):
        r = self.r["rehash"]
        self.assertEqual(r["rehash_d0"], 0)
        self.assertEqual(r["firmware_hash"], r["image_hash"])
        self.assertEqual(r["image_hash"], self.entry["hash"])

    def test_project_decodes_and_points_at_the_file(self):
        p = self.r["project"]
        self.assertEqual(p["coki_ok"], 1)
        self.assertEqual(p["header_word_0x14"] & 3, 0)
        self.assertEqual(p["reinit"], 0)  # decoded, not replaced by a new project
        self.assertEqual(p["container_version"], 5)
        self.assertEqual(p["slots"][:3], [7, 1, 3])
        # Track 1 of the active kit: machine 0 (ONESHOT), slot 7.
        self.assertEqual(p["tracks"][0], (0, 7))
        for ref in p["refs"].values():
            self.assertEqual(ref, self.entry["ref"])

    def test_loader_streams_the_file(self):
        load = self.r["load"]
        self.assertEqual(load["load_d0"], 0)
        size = self.entry["size"]
        alloc = (size + 0x200F) & ~0x1FFF
        header = load["slot_headers"][-1]
        data_len = size - 0x50
        self.assertEqual(
            header, (load["slot"], 0x20, alloc // 2 + 0x20, 48000, data_len // 2)
        )
        self.assertEqual(load["stereo"], 1)
        self.assertEqual(load["len"], data_len)
        self.assertEqual(load["rate"], 48000)
        self.assertEqual(load["pcm_matches"], [True, True])
        self.assertIn((0x1F, load["slot"]), load["events"])

    def test_directory_walks(self):
        d = self.r["directory"]
        names = [n for n, _ in d["listing"]]
        # The mount links the RAM-resident "factory" directory into the
        # root through FUN_40156334, i.e. into the pages this tool wrote.
        self.assertEqual(names, [b".", b"..", b"factory", b"tone"])
        self.assertEqual(d["lookup"], 1)
        self.assertEqual(d["lookup_id"], self.entry["id"])
        for firmware, ours in d["hashes"].values():
            self.assertEqual(firmware, ours)
        self.assertEqual(d["path"], b"/tone")
        self.assertEqual(self.r["load"]["name"], b"tone")

    def test_second_slot_aliases(self):
        alias = self.r["alias"]
        self.assertEqual(alias["load_d0"], 0)
        self.assertEqual(alias["data_pages"], 0)
        self.assertEqual(
            alias["slot_headers"][-1][1:], self.r["load"]["slot_headers"][-1][1:]
        )


if __name__ == "__main__":
    unittest.main()
