"""Synthetic tests for the bounded loader-string mapper."""

import json
import os
import re
import sqlite3
import struct
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools"))
from importlib import import_module

sharcldr = import_module("sharcldr")
sharcstrings = import_module("sharcstrings")


def block(code, address, payload=b""):
    header = bytearray(
        struct.pack("<IIII", code | 0xAD000000, address, len(payload), 0)
    )
    header[2] = 0
    checksum = 0
    for value in header:
        checksum ^= value
    header[2] = checksum
    return bytes(header) + payload


def record(index, payload_offset, payload_length, address, fill=False):
    return {
        "index": index,
        "payload_offset": payload_offset,
        "payload_len": payload_length,
        "target_address": address,
        "fill": fill,
        "core": 0,
    }


class SharcStringsTest(unittest.TestCase):
    def test_mapping_boundary_rejection_and_short_words(self):
        data = b"\0HELLO\0"
        blocks = [record(0, 1, 5, sharcldr.SW_ALIAS_BASE)]
        rows = sharcstrings.map_strings(data, blocks, min_length=5)
        self.assertEqual(rows[0]["loaded_byte_address"], sharcldr.SW_ALIAS_BASE)
        self.assertEqual(rows[0]["short_word_address"], 0)
        self.assertEqual(
            (rows[0]["short_word_start"], rows[0]["short_word_end"]), (0, 2)
        )
        crossing_data = b"\0ABCDEF\0"
        crossing = [
            record(0, 1, 3, sharcldr.SW_ALIAS_BASE),
            record(1, 4, 3, sharcldr.SW_ALIAS_BASE + 3),
        ]
        self.assertEqual(
            sharcstrings.map_strings(crossing_data, crossing, min_length=5), []
        )

    def test_last_write_shadow_and_regex_limits(self):
        data = b"\0ALPHA\0BRAVO\0"
        base = sharcldr.SW_ALIAS_BASE
        blocks = [record(0, 1, 5, base), record(1, 7, 5, base + 2)]
        self.assertEqual(
            [row["text"] for row in sharcstrings.map_strings(data, blocks)], ["BRAVO"]
        )
        rows = sharcstrings.map_strings(
            data, blocks, patterns=[re.compile("alpha", re.I)], include_shadowed=True
        )
        self.assertEqual(rows[0]["text"], "ALPHA")
        self.assertTrue(rows[0]["shadowed"])
        self.assertEqual(
            list(sharcstrings._printable_runs(b"\0ABCDE\0", 5, 5)), [(1, "ABCDE")]
        )
        self.assertEqual(list(sharcstrings._printable_runs(b"\0ABCDEF\0", 5, 5)), [])

    def test_sqlite_correlation_and_json_safe_keys(self):
        data = b"\0HELLO\0"
        rows = sharcstrings.map_strings(data, [record(0, 1, 5, sharcldr.SW_ALIAS_BASE)])
        with tempfile.NamedTemporaryFile(suffix=".sqlite", delete=False) as source:
            path = source.name
        try:
            db = sqlite3.connect(path)
            db.executescript("""
                CREATE TABLE refs (from_sw INTEGER, to_sw INTEGER, type TEXT);
                CREATE TABLE insn (sw INTEGER, function_sw INTEGER);
                CREATE TABLE functions (sw INTEGER, name TEXT);
            """)
            db.execute("INSERT INTO refs VALUES (9, 1, 'DATA')")
            db.execute("INSERT INTO refs VALUES (10, 9, 'OTHER')")
            db.execute("INSERT INTO insn VALUES (9, 4)")
            db.execute("INSERT INTO functions VALUES (4, 'generic_fn')")
            db.commit()
            sharcstrings.correlate_refs(rows, path)
            self.assertEqual(
                rows[0]["refs"],
                [
                    {
                        "from_pc": 9,
                        "type": "DATA",
                        "function_address": 4,
                        "function_name": "generic_fn",
                    }
                ],
            )
            encoded = json.dumps(rows, sort_keys=True)
            self.assertNotIn('"raw"', encoded)
            self.assertNotIn('"bytes"', encoded)
        finally:
            os.unlink(path)

    def test_cli_validation_and_no_match(self):
        base = sharcldr.SW_ALIAS_BASE
        stream = block(0, base, b"\0HELLO\0")
        with tempfile.NamedTemporaryFile(delete=False) as source:
            source.write(stream)
            path = source.name
        try:
            command = [sys.executable, "tools/sharcstrings.py", path]
            no_match = subprocess.run(
                command + ["--grep", "missing", "--json"],
                text=True,
                capture_output=True,
            )
            self.assertEqual(no_match.returncode, 0)
            self.assertEqual(json.loads(no_match.stdout), [])
            invalid = subprocess.run(
                command + ["--min-length", "0"], text=True, capture_output=True
            )
            self.assertNotEqual(invalid.returncode, 0)
            self.assertIn("must be positive", invalid.stderr)
            bad_regex = subprocess.run(
                command + ["--grep", "["], text=True, capture_output=True
            )
            self.assertNotEqual(bad_regex.returncode, 0)
            self.assertIn("invalid --grep regex", bad_regex.stderr)
        finally:
            os.unlink(path)


if __name__ == "__main__":
    unittest.main()
