"""Synthetic coverage for exact Ghidra-seeded loader decoding."""
import argparse
import os
import sqlite3
import struct
import subprocess
import sys
import tempfile
import unittest
from importlib import import_module

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools"))
S = import_module("sharc_seeddecode")
L = import_module("sharcldr")


def block(address, payload, code=0):
    header = bytearray(
        struct.pack("<IIII", code | 0xAD000000, address, len(payload), 0)
    )
    header[2] = 0
    checksum = 0
    for byte in header:
        checksum ^= byte
    header[2] = checksum
    return bytes(header) + payload


class SeedDecodeTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.blob = os.path.join(self.tmp.name, "loader.bin")
        self.dbpath = os.path.join(self.tmp.name, "seeds.sqlite")
        db = sqlite3.connect(self.dbpath)
        db.executescript("""
            CREATE TABLE insn (sw INTEGER PRIMARY KEY, length INTEGER, mnemonic TEXT,
                flow TEXT, function_sw INTEGER, in_main INTEGER);
            CREATE TABLE functions (sw INTEGER PRIMARY KEY, name TEXT, in_main INTEGER);
        """)
        db.executemany("INSERT INTO functions VALUES (?,?,?)", [(1, "nonmain", 0), (2, "main", 1)])
        db.executemany("INSERT INTO insn VALUES (?,?,?,?,?,?)", [
            (0x10, 2, "rframe", "FALL_THROUGH", 1, 0),
            (0x20, 4, "wrong_length", "FALL_THROUGH", 1, 0),
            (0x30, 6, "uncertain", "FALL_THROUGH", 1, 0),
            (0x40, 2, "unmapped", "FALL_THROUGH", 1, 0),
            (0x48, 6, "unknown", "FALL_THROUGH", 1, 0),
            (0x50, 2, "main", "FALL_THROUGH", 2, 1),
        ])
        db.commit()
        db.close()
        with open(self.blob, "wb") as fh:
            fh.write(b"".join([
                block(L.sw_to_byte(0x10), bytes.fromhex("0119")),
                block(L.sw_to_byte(0x20), bytes.fromhex("0119")),
                block(L.sw_to_byte(0x30), bytes.fromhex("004030000000")),
                block(L.sw_to_byte(0x48), bytes.fromhex("000f0000")),
                block(L.sw_to_byte(0x50), bytes.fromhex("0119")),
                block(0, b"", 1 << L.BFLAGS["FINAL"]),
            ]))

    def tearDown(self):
        self.tmp.cleanup()

    def args(self, **changes):
        values = dict(loader_blob=self.blob, sqlite_db=self.dbpath, scope="non-main",
                      function=None, start=None, end=None, limit=None, only_problems=False)
        values.update(changes)
        return argparse.Namespace(**values)

    def test_statuses_and_no_raw_data(self):
        result = S.report(self.args())
        rows = {row["seed"]["pc_sw"]: row for row in result["rows"]}
        self.assertEqual(rows[0x10]["status"], "confirmed")
        self.assertEqual(rows[0x20]["status"], "length-mismatch")
        self.assertEqual(rows[0x30]["status"], "uncertain")
        self.assertEqual(rows[0x40]["status"], "unmapped")
        self.assertEqual(rows[0x48]["status"], "unknown")
        def keys(value):
            if isinstance(value, dict):
                return set(value) | set().union(*(keys(item) for item in value.values()))
            if isinstance(value, list):
                return set().union(*(keys(item) for item in value))
            return set()

        self.assertTrue({"raw", "bytes", "payload"}.isdisjoint(keys(result)))
        self.assertIn("no linear sweep", result["metadata"]["seed_method"])

    def test_scope_function_range_limit_and_only_problems(self):
        self.assertEqual([x["seed"]["pc_sw"] for x in S.report(self.args(scope="main"))["rows"]], [0x50])
        self.assertEqual(S.report(self.args(function=2))["rows"], [])
        self.assertEqual([x["seed"]["pc_sw"] for x in S.report(self.args(scope="all", function=2))["rows"]], [0x50])
        filtered = S.report(self.args(start=0x20, end=0x40, limit=1, only_problems=True))
        self.assertEqual(filtered["metadata"]["counts"], {"selected_seeds": 1, "emitted_rows": 1})
        self.assertEqual(filtered["rows"][0]["seed"]["pc_sw"], 0x20)

    def test_overlap_last_write_is_observed(self):
        with open(self.blob, "wb") as fh:
            address = L.sw_to_byte(0x10)
            fh.write(block(address, bytes.fromhex("0119")) +
                     block(address, bytes.fromhex("800a")) +
                     block(0, b"", 1 << L.BFLAGS["FINAL"]))
        row = S.report(self.args(limit=1))["rows"][0]
        self.assertEqual(row["decoder"]["form"], "11c")
        self.assertEqual(row["loader_source_block_index"], 1)

    def test_cli_errors_and_positive_limit(self):
        command = [sys.executable, "tools/sharc_seeddecode.py", self.blob, self.dbpath]
        invalid = subprocess.run(command + ["--limit", "0"], capture_output=True, text=True)
        self.assertNotEqual(invalid.returncode, 0)
        self.assertIn("must be positive", invalid.stderr)
        bad_db = subprocess.run([sys.executable, "tools/sharc_seeddecode.py", self.blob,
                                 os.path.join(self.tmp.name, "missing.sqlite")], capture_output=True, text=True)
        self.assertNotEqual(bad_db.returncode, 0)
        self.assertNotIn("Traceback", bad_db.stderr)
        malformed = os.path.join(self.tmp.name, "bad.bin")
        with open(malformed, "wb") as fh:
            fh.write(b"not a loader")
        bad_blob = subprocess.run([sys.executable, "tools/sharc_seeddecode.py", malformed, self.dbpath],
                                  capture_output=True, text=True)
        self.assertNotEqual(bad_blob.returncode, 0)
        self.assertNotIn("Traceback", bad_blob.stderr)
        incomplete = os.path.join(self.tmp.name, "incomplete.bin")
        with open(incomplete, "wb") as fh:
            fh.write(block(L.sw_to_byte(0x10), bytes.fromhex("0119")))
        partial_blob = subprocess.run(
            [sys.executable, "tools/sharc_seeddecode.py", incomplete, self.dbpath],
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(partial_blob.returncode, 0)
        self.assertIn("final marker", partial_blob.stderr)


if __name__ == "__main__":
    unittest.main()
