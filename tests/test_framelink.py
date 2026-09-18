# pyright: reportMissingImports=false
"""tools/framelink.py profiles."""

import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'tools'))

import framelink  # noqa: E402

KEYS = {'name', 'vector', 'handler', 'driver', 'counter', 'gate', 'countdown', 'mode',
        'stop', 'stop_writer', 'tables'}


class FramelinkTest(unittest.TestCase):
    def test_profiles_are_complete(self):
        for sha, prof in framelink.PROFILES.items():
            self.assertEqual(len(sha), 64)
            self.assertEqual(set(prof), KEYS)
            for name in framelink.VARIABLES:
                self.assertIsInstance(prof[name], int)

    def test_track_9a_table_uses_sram_row_base(self):
        self.assertIn((0x80003cd0, 0x9a, 16, 'track_9a'), framelink.TABLES)
        self.assertNotIn((0x80003340, 0x9a, 16, 'track_9a'), framelink.TABLES)

    def test_unknown_image_stops(self):
        with tempfile.NamedTemporaryFile(delete=False) as f:
            f.write(b'not an image')
        try:
            with self.assertRaises(SystemExit):
                framelink.profile_for(f.name)
        finally:
            os.unlink(f.name)


if __name__ == '__main__':
    unittest.main()
