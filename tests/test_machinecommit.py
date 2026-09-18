"""Unit tests for the bounded direct-refresh A2 experiment helpers."""

import importlib.util
import os
import sys
import unittest
from types import SimpleNamespace

_TOOLS = os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools")
sys.path.insert(0, _TOOLS)
_TOOL = os.path.join(_TOOLS, "machinecommit.py")
_SPEC = importlib.util.spec_from_file_location("machinecommit", _TOOL)
assert _SPEC is not None and _SPEC.loader is not None
machinecommit = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(machinecommit)


class BoundedRecordsTest(unittest.TestCase):
    def test_counts_and_truncation(self):
        sink = machinecommit.BoundedRecords(2)
        for value in range(3):
            sink.add({"value": value})
        self.assertEqual(
            sink.result(),
            {
                "total": 3,
                "emitted": 2,
                "truncated": True,
                "records": [{"value": 0}, {"value": 1}],
            },
        )
        with self.assertRaises(ValueError):
            machinecommit.BoundedRecords(129)


class FrameHelpersTest(unittest.TestCase):
    def test_type_and_coalesced_diff(self):
        left = b"abcde"
        right = b"aXYdeZ"
        self.assertIsNone(machinecommit.frame_type(b"", 0))
        typed = bytearray(machinecommit.FRAME_TYPE_OFF + 2)
        typed[machinecommit.FRAME_TYPE_OFF : machinecommit.FRAME_TYPE_OFF + 2] = (
            b"\x00\x05"
        )
        self.assertEqual(machinecommit.frame_type(bytes(typed), 0), 5)
        self.assertEqual(machinecommit.frame_diffs([left], [right])[0]["pass"], 0)
        self.assertEqual(
            machinecommit.frame_diff(left, right),
            {
                "bytes": [
                    {"offset": 1, "before": "62", "after": "58"},
                    {"offset": 2, "before": "63", "after": "59"},
                    {"offset": 5, "before": "", "after": "5a"},
                ],
                "ranges": [
                    {"start": 1, "end": 3, "before": "6263", "after": "5859"},
                    {"start": 5, "end": 6, "before": "", "after": "5a"},
                ],
            },
        )


ORIGINAL, NEW, SOURCE = 1, 5, 0x44000034


def frames(words):
    return [
        {"length": machinecommit.FRAME_LENGTH, "sha256": str(i), "type_word": word}
        for i, word in enumerate(words)
    ]


def result(name, variant, words, obj_type, row_type, cache, refresh=0, writes=0):
    records = [{"args": [SOURCE, 0]}] if refresh else []
    final = {"obj_type": obj_type, "row_type": row_type, "cache": cache}
    return {
        "condition": name,
        "variant": variant,
        "track": 0,
        "source": SOURCE,
        "row": machinecommit.ROW_BASE,
        "cache_slot": machinecommit.CACHE_BASE,
        "before": {"obj_type": ORIGINAL, "row_type": ORIGINAL, "cache": 0},
        "poked": final.copy(),
        "post_refresh": final.copy(),
        "after": final,
        "direct_call": {
            "invoked": name.endswith("refresh"),
            "d0": 0 if name.endswith("refresh") else None,
        },
        "row_sha256": {
            "before": "a",
            "after": "b" if name.endswith("refresh") else "a",
        },
        "frames": frames(words),
        "stops": ["returned"] * 3,
        "refresh_records": {
            "total": refresh,
            "emitted": refresh,
            "truncated": False,
            "records": records,
        },
        "row_writes": {
            "total": writes,
            "emitted": writes,
            "truncated": False,
            "records": [],
        },
    }


def accepted_matrix():
    specs = [
        ("baseline", [ORIGINAL] * 3, ORIGINAL, ORIGINAL, 0),
        ("source_only", [ORIGINAL] * 3, NEW, ORIGINAL, 0),
        ("unchanged_refresh", [ORIGINAL] * 3, ORIGINAL, ORIGINAL, SOURCE),
        ("changed_refresh", [ORIGINAL, NEW, NEW], NEW, NEW, SOURCE),
    ]
    output = []
    for variant in ("clean", "instrumented"):
        for name, words, obj, row, cache in specs:
            output.append(
                result(
                    name,
                    variant,
                    words,
                    obj,
                    row,
                    cache,
                    refresh=int(variant == "instrumented" and name.endswith("refresh")),
                    writes=int(variant == "instrumented" and name.endswith("refresh")),
                )
            )
    return output


class AcceptanceTest(unittest.TestCase):
    def test_acceptance_passes_for_valid_matrix(self):
        accepted, reasons, equivalence = machinecommit.accept(
            accepted_matrix(), ORIGINAL, NEW, 3
        )
        self.assertTrue(accepted)
        self.assertEqual(reasons, [])
        self.assertTrue(all(item["equivalent"] for item in equivalence.values()))

    def test_unchanged_refresh_may_update_other_frame_fields(self):
        data = accepted_matrix()
        for item in data:
            if item["condition"] == "unchanged_refresh":
                item["frames"][1]["sha256"] = "wholesale-refresh"
                item["frames"][2]["sha256"] = "wholesale-refresh-stable"
        accepted, reasons, unused = machinecommit.accept(data, ORIGINAL, NEW, 3)
        self.assertTrue(accepted, reasons)

    def test_missing_refresh_fails(self):
        data = accepted_matrix()
        watched = next(
            r
            for r in data
            if r["condition"] == "changed_refresh" and r["variant"] == "instrumented"
        )
        watched["refresh_records"]["total"] = 0
        watched["refresh_records"]["records"] = []
        accepted, reasons, unused = machinecommit.accept(data, ORIGINAL, NEW, 3)
        self.assertFalse(accepted)
        self.assertIn("direct_refresh_hit", {reason["gate"] for reason in reasons})

    def test_duplicate_condition_fails(self):
        data = accepted_matrix()
        data.append(data[0].copy())
        accepted, reasons, unused = machinecommit.accept(data, ORIGINAL, NEW, 3)
        self.assertFalse(accepted)
        self.assertEqual(reasons[0]["gate"], "condition_matrix")

    def test_changed_source_state_fails(self):
        data = accepted_matrix()
        for item in data:
            if item["condition"] == "changed_refresh":
                item["poked"]["obj_type"] = ORIGINAL
        accepted, reasons, unused = machinecommit.accept(data, ORIGINAL, NEW, 3)
        self.assertFalse(accepted)
        self.assertIn("changed_refresh_source", {reason["gate"] for reason in reasons})

    def test_changed_row_must_move_during_direct_call(self):
        data = accepted_matrix()
        for item in data:
            if item["condition"] == "changed_refresh":
                item["post_refresh"]["row_type"] = ORIGINAL
        accepted, reasons, unused = machinecommit.accept(data, ORIGINAL, NEW, 3)
        self.assertFalse(accepted)
        self.assertIn(
            "changed_refresh_post_row", {reason["gate"] for reason in reasons}
        )

    def test_frame_delay_fails(self):
        data = accepted_matrix()
        for item in data:
            if item["condition"] == "changed_refresh":
                item["frames"] = frames([ORIGINAL] * 3)
        accepted, reasons, unused = machinecommit.accept(data, ORIGINAL, NEW, 3)
        self.assertFalse(accepted)
        self.assertIn(
            "changed_refresh_frame_delay", {reason["gate"] for reason in reasons}
        )

    def test_source_only_full_frame_mismatch_fails(self):
        data = accepted_matrix()
        for item in data:
            if item["condition"] == "source_only":
                item["frames"][1]["sha256"] = "unexpected"
        accepted, reasons, unused = machinecommit.accept(data, ORIGINAL, NEW, 3)
        self.assertFalse(accepted)
        self.assertIn("source_only_frames", {reason["gate"] for reason in reasons})

    def test_clean_watch_mismatch_is_named(self):
        data = accepted_matrix()
        watched = next(
            r
            for r in data
            if r["condition"] == "baseline" and r["variant"] == "instrumented"
        )
        watched["after"]["cache"] = 99
        accepted, reasons, unused = machinecommit.accept(data, ORIGINAL, NEW, 3)
        self.assertFalse(accepted)
        mismatch = next(
            reason
            for reason in reasons
            if reason["gate"] == "clean_instrumented_equivalence"
        )
        self.assertEqual(mismatch["detail"][0]["field"], "after")


class ArgumentValidationTest(unittest.TestCase):
    def test_rejects_unknown_image_and_invalid_ranges(self):
        args = SimpleNamespace(
            track=0,
            new_type=1,
            passes=3,
            limit=5_000_000,
            record_limit=32,
            stack_longs=8,
        )
        with self.assertRaises(SystemExit):
            machinecommit.validate_args(args, "not-the-image")
        args.track = 16
        with self.assertRaises(SystemExit):
            machinecommit.validate_args(args, machinecommit.IMAGE_SHA256)
        args.track = 0
        args.limit = 0
        with self.assertRaises(SystemExit):
            machinecommit.validate_args(args, machinecommit.IMAGE_SHA256)
        args.limit = machinecommit.MAX_INSTRUCTION_LIMIT + 1
        with self.assertRaises(SystemExit):
            machinecommit.validate_args(args, machinecommit.IMAGE_SHA256)


if __name__ == "__main__":
    unittest.main()
