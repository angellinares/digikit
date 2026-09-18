"""Synthetic metadata-only tests for sharc_candidates."""

import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from importlib import import_module

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools"))
C = import_module("sharc_candidates")


class CandidatesTest(unittest.TestCase):
    def setUp(self):
        self.file = tempfile.NamedTemporaryFile(suffix=".sqlite", delete=False)
        self.file.close()
        self.db = sqlite3.connect(self.file.name)
        self.db.executescript("""
            CREATE TABLE decoder (sw INTEGER, form TEXT, kind TEXT, fields TEXT, aligned INTEGER);
            CREATE TABLE insn (
                sw INTEGER, function_sw INTEGER, in_main INTEGER,
                flow TEXT DEFAULT 'FALL_THROUGH'
            );
            CREATE TABLE functions (sw INTEGER, name TEXT, in_main INTEGER);
        """)
        self.db.execute("INSERT INTO functions VALUES (100, 'main_fn', 1)")

    def tearDown(self):
        self.db.close()
        os.unlink(self.file.name)

    def row(self, sw, form, fields, kind="confident", function=100, flow="FALL_THROUGH"):
        self.db.execute(
            "INSERT INTO decoder VALUES (?, ?, ?, ?, 1)",
            (sw, form, kind, json.dumps(fields)),
        )
        self.db.execute("INSERT INTO insn VALUES (?, ?, 1, ?)", (sw, function, flow))

    def candidate(self, sw, source=1, destination=2, delta=0x94, g=0):
        self.row(
            sw,
            "19a",
            {
                "is": source,
                "idis": destination,
                "g": g,
                "data[31:16]": (delta >> 16) & 0xFFFF,
                "data[15:0]": delta & 0xFFFF,
            },
        )

    def found(self, **kwargs):
        self.db.commit()
        return C.candidates(self.file.name, **kwargs)

    def test_exact_byte_word_and_near_miss(self):
        self.candidate(101, delta=0x94)
        self.candidate(102, delta=0x4A)
        self.candidate(103, delta=0x95)
        self.assertEqual(
            [(x["address"], x["unit"]) for x in self.found()],
            [(101, "byte"), (102, "word")],
        )

    def test_fates_and_source_writer(self):
        # writer followed by a 15b use
        self.row(100, "17a", {"ureg": 17})
        self.candidate(101)
        self.row(102, "15b", {"i": 2, "g": 0, "d": 1, "ureg": 1})
        # overwritten, uncertain, control, and no use
        self.candidate(110)
        self.row(111, "17b", {"ureg": 18})
        self.candidate(120)
        self.row(121, "mystery", {}, "provisional")
        self.candidate(130)
        self.row(131, "25a_direct", {}, flow="UNCONDITIONAL_CALL")
        self.candidate(140)
        got = {x["address"]: x for x in self.found(window=3)}
        self.assertEqual(got[101]["source_writer"]["kind"], "writer")
        self.assertEqual(got[101]["source_writer"]["pc"], 100)
        self.assertEqual(got[101]["forward_fate"]["kind"], "consumed")
        self.assertEqual(got[110]["forward_fate"]["kind"], "overwritten")
        self.assertEqual(got[120]["forward_fate"]["kind"], "uncertain-barrier")
        self.assertEqual(got[130]["forward_fate"]["kind"], "control-barrier")
        self.assertEqual(got[140]["forward_fate"]["kind"], "no-use-in-window")

    def test_g_dag_3c_and_stack_ranking(self):
        self.candidate(100, source=1, destination=2)
        self.row(101, "3c", {"dmi[2:0]": 2})
        self.candidate(110, source=1, destination=2, g=1)  # I9 -> I10
        self.row(111, "4b", {"i[2:0]": 2, "g": 1})
        self.candidate(120, source=6, destination=3)
        self.row(121, "16b", {"i[2:0]": 3, "g": 0})
        got = self.found()
        by_pc = {x["address"]: x for x in got}
        self.assertEqual(
            by_pc[100]["forward_fate"]["kind"], "consumed"
        )  # 3c is DAG1 I2
        self.assertEqual((by_pc[110]["source_i"], by_pc[110]["destination_i"]), (9, 10))
        self.assertEqual(by_pc[110]["forward_fate"]["kind"], "consumed")
        self.assertTrue(by_pc[120]["stack_risk"])
        self.assertEqual(got[0]["address"], 100)  # stable tie by address; stack is last

    def test_json_has_no_raw_or_bytes_and_cli_validation(self):
        self.candidate(101)
        self.db.commit()
        output = subprocess.run(
            [sys.executable, "tools/sharc_candidates.py", self.file.name, "--json"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
        self.assertNotIn("raw", output.lower())
        decoded = json.loads(output)
        keys = set()

        def collect(value):
            if isinstance(value, dict):
                keys.update(value)
                for child in value.values():
                    collect(child)
            elif isinstance(value, list):
                for child in value:
                    collect(child)

        collect(decoded)
        self.assertFalse({"raw", "bytes"} & keys)
        self.assertEqual(decoded[0]["address"], 101)
        for option in ("--offset", "--word-bytes", "--window"):
            invalid = subprocess.run(
                [
                    sys.executable,
                    "tools/sharc_candidates.py",
                    self.file.name,
                    option,
                    "0",
                ],
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(invalid.returncode, 0)
            self.assertIn("must be positive", invalid.stderr)

    def test_bad_json_and_missing_schema_are_clear(self):
        self.row(
            101,
            "19a",
            {"is": 1, "idis": 2, "g": 0, "data[31:16]": 0, "data[15:0]": 0x94},
        )
        self.db.execute("UPDATE decoder SET fields = '{bad' WHERE sw = 101")
        self.db.commit()
        with self.assertRaisesRegex(ValueError, "bad decoder fields JSON"):
            C.candidates(self.file.name)
        missing = tempfile.NamedTemporaryFile(delete=False)
        missing.close()
        try:
            with self.assertRaisesRegex(ValueError, "missing or incompatible"):
                C.candidates(missing.name)
        finally:
            os.unlink(missing.name)


if __name__ == "__main__":
    unittest.main()
