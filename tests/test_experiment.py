# pyright: reportMissingImports=false
"""Phase 1 experiment recipe and report helpers use no firmware."""

import os
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools"))
import experiment

run_experiment = experiment.execute

RECIPE = {
    "name": "button",
    "snapshot": "snapshots/postintro.snap",
    "endpoint": 2_400_000,
    "repeat": 2,
    "exact": True,
    "panel_dwell": 16,
    "manipulation": {"code": 14, "press": 400_000, "release": 800_000},
    "ranges": [{"lo": 0x80000000, "hi": 0x80000004, "name": "test"}],
}


class ExperimentTest(unittest.TestCase):
    def test_schema_rejects_non_exact_multiple_actions_and_bad_timing(self):
        for change in (
            {"exact": False},
            {"manipulations": []},
            {"manipulation": {"code": 14, "press": 8, "release": 8}},
            {"manipulation": {"code": 14, "press": 8, "release": 2_400_000}},
        ):
            recipe = dict(RECIPE)
            recipe.update(change)
            with self.assertRaises(experiment.RecipeError):
                experiment.validate_recipe(recipe)

    def test_a1_raw_feeds_are_strict_and_exactly_parsed(self):
        recipe = {
            **RECIPE,
            "manipulation": {
                "feeds": [
                    {"at": 400000, "hex": "2201"},
                    {"at": 8400000, "hex": "2002"},
                    {"at": 20400000, "hex": "2000"},
                    {"at": 28400000, "hex": "2200"},
                ]
            },
            "endpoint": 40000000,
            "a1": {
                "observation_at": 14400000,
                "panel_raw": True,
                "ui_trace": True,
                "block_coverage": True,
            },
        }
        normalized = experiment.validate_recipe(recipe)
        self.assertEqual(normalized["manipulation"]["feeds"][0]["hex"], "2201")
        self.assertEqual(
            experiment.feed_tuples(
                "[guirun] input --feed 400016:2201 (asked 400000)\n"
            ),
            [(400000, 400016, "2201")],
        )
        recipe["manipulation"] = {"feeds": [{"at": 1, "hex": "00"}]}
        with self.assertRaises(experiment.RecipeError):
            experiment.validate_recipe(recipe)

    def test_execute_a1_uses_quarantined_lanes_and_exact_artifacts(self):
        raw = {
            **RECIPE,
            "endpoint": 40000000,
            "manipulation": {
                "feeds": [{"at": at, "hex": text} for at, text in experiment.A1_FEEDS]
            },
            "a1": {
                "observation_at": 14400000,
                "panel_raw": True,
                "ui_trace": True,
                "block_coverage": True,
            },
        }
        recipe = experiment.validate_recipe(raw)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot = root / "in.snap"
            snapshot.write_bytes(b"in")
            recipe["snapshot"] = str(snapshot)
            main = root / "main"
            firmware = root / "firmware"
            sections = root / "sections"
            main.write_bytes(b"main")
            firmware.write_bytes(b"fw")
            sections.mkdir()

            def runner(cmd, **kwargs):
                saves = [
                    cmd[i + 1].split(":", 1)
                    for i, x in enumerate(cmd)
                    if x == "--save-at"
                ]
                panel = cmd[cmd.index("--panel-raw-at") + 1].split(":", 1)[1]
                profile = "--trace-ui-json" in cmd
                manipulated = "--feed" in cmd
                for _at, path in saves:
                    Path(path).write_bytes(b"M" if manipulated else b"B")
                Path(panel).write_bytes((b"m" if manipulated else b"b") * 1024)
                if profile:
                    ui = (
                        [
                            "[ui] 1 q+ depth=1 SRC(2) 0x03 press|chord",
                            "[ui] 2 activate MachineSelectionView@0x1",
                            "[ui] 3 q+ depth=1 FUNC(17) 0x00 -",
                        ]
                        if manipulated
                        else ["[ui] idle"]
                    )
                    Path(cmd[cmd.index("--trace-ui-json") + 1]).write_text(
                        __import__("json").dumps(
                            {
                                "kind": "scoped dynamic call/view evidence",
                                "events": ui,
                            }
                        )
                    )
                    Path(cmd[cmd.index("--block-profile") + 1]).write_text(
                        __import__("json").dumps(
                            {
                                "perturbing": True,
                                "kind": "basic-block entries",
                                "entries": [
                                    {"address": 2 if manipulated else 1, "hits": 1}
                                ]
                            }
                        )
                    )
                lines = [
                    "saved snapshot at %d instrs -> %s" % (int(at) + 16, path)
                    for at, path in saves
                ]
                if manipulated:
                    lines += [
                        "[guirun] input --feed %d:%s (asked %d)" % (at + 16, text, at)
                        for at, text in experiment.A1_FEEDS
                    ]
                lines += [
                    "[guirun] panel raw asked 14400000 latched 14400000 -> %s" % panel,
                    "[guirun] end: instrs=40M terminal=False tasks=1 dtim3=1 mainloop=1 jobs=0 pc=0x0",
                    "[guirun] faults: 0 distinct pages touched",
                ]
                return SimpleNamespace(stdout="\n".join(lines), stderr="", returncode=0)

            with (
                mock.patch.object(
                    experiment,
                    "resolve_inputs",
                    return_value=(main, firmware, sections),
                ),
                mock.patch.object(
                    experiment, "diff_snapshots", return_value={"regions": []}
                ),
            ):
                report = experiment.execute(recipe, root / "run", runner)
            self.assertTrue(report["success"])
            self.assertEqual(set(report["lanes"]), {"state", "profile"})

    def test_a1_profile_validation_rejects_summary_or_unlabelled_blocks(self):
        valid_ui = {
            "kind": "scoped dynamic call/view evidence",
            "events": ["[ui] event"],
        }
        valid_blocks = {
            "perturbing": True,
            "kind": "basic-block entries",
            "entries": [{"address": 16, "hits": 1}],
        }
        self.assertTrue(
            experiment._a1_profile_outputs_valid(valid_ui, valid_blocks)
        )
        self.assertFalse(
            experiment._a1_profile_outputs_valid(
                {**valid_ui, "summary": "window"}, valid_blocks
            )
        )
        self.assertFalse(
            experiment._a1_profile_outputs_valid(
                {
                    "kind": valid_ui["kind"],
                    "events": ["[ui] hook error at 0x1: bad"],
                },
                valid_blocks,
            )
        )
        self.assertFalse(
            experiment._a1_profile_outputs_valid(
                valid_ui, {"kind": "basic-block entries", "entries": []}
            )
        )

    def test_a1_ui_trajectory_requires_release_suppression_after_activation(self):
        ordered = [
            "[ui] 1 q+ depth=1 SRC(2) 0x03 press|chord",
            "[ui] 2 activate MachineSelectionView@0x1",
            "[ui] 3 q+ depth=1 FUNC(17) 0x00 -",
        ]
        result = experiment._a1_ui_trajectory(ordered)
        self.assertTrue(result["valid"])
        self.assertFalse(result["src_release_record"])
        self.assertFalse(experiment._a1_ui_trajectory(list(reversed(ordered)))["valid"])
        for unexpected in (
            "[ui] 2.1 q+ depth=1 SRC(2) 0x12 release|chord",
            "[ui] 2.1 q+ depth=1 SRC(2) 0x0b press|chord|repeat",
            "[ui] 2.1 close-call MachineSelectionView@0x1",
        ):
            with self.subTest(unexpected=unexpected):
                self.assertFalse(
                    experiment._a1_ui_trajectory(
                        ordered[:2] + [unexpected] + ordered[2:]
                    )["valid"]
                )
        self.assertFalse(
            experiment._a1_ui_trajectory(
                [
                    "[ui] 1 q+ depth=1 OTHER(2) 0x03",
                    *ordered[1:],
                ]
            )["valid"]
        )
        self.assertFalse(
            experiment._a1_ui_trajectory(
                [*ordered[:2], "[ui] 3 q+ depth=1 OTHER(17) 0x00"]
            )["valid"]
        )

    def test_duplicate_manipulation_key_is_rejected(self):
        with tempfile.NamedTemporaryFile(mode="w") as recipe:
            recipe.write('{"manipulation": {}, "manipulation": {}}')
            recipe.flush()
            with self.assertRaises(experiment.RecipeError):
                experiment.load_recipe(recipe.name)

    def test_one_logical_action_builds_only_press_and_release(self):
        recipe = experiment.validate_recipe(RECIPE)
        base = experiment.command(recipe, "in.snap", "out.snap", "baseline")
        changed = experiment.command(recipe, "in.snap", "out.snap", "manipulated")
        self.assertIn("--exact", base)
        self.assertIn("--limit", base)
        self.assertNotIn("--input", base)
        self.assertEqual(changed.count("--input"), 2)
        self.assertIn("400000:press:14", changed)
        self.assertIn("800000:release:14", changed)
        self.assertIn("--save-at", changed)
        self.assertIn(
            "--syx",
            experiment.command(
                recipe, "in.snap", "out.snap", "baseline", "firmware.syx"
            ),
        )

    def test_determinism_requires_successful_matching_endpoints_and_boundaries(self):
        same = [
            {"returncode": 0, "endpoint_sha256": "a", "actual_saved_instructions": 12},
            {"returncode": 0, "endpoint_sha256": "a", "actual_saved_instructions": 12},
        ]
        for record in same:
            record["fault_pages"] = 0
        self.assertTrue(experiment.deterministic(same, 10))
        self.assertFalse(experiment.deterministic(same, 13))
        self.assertFalse(
            experiment.deterministic(
                [
                    same[0],
                    {
                        "returncode": 0,
                        "endpoint_sha256": "a",
                        "actual_saved_instructions": 13,
                        "fault_pages": 0,
                    },
                ]
            )
        )
        self.assertFalse(experiment.deterministic(same[:-1]))
        self.assertFalse(
            experiment.deterministic(same + [{"returncode": 0, "endpoint_sha256": "b"}])
        )
        self.assertFalse(
            experiment.deterministic(
                [
                    {
                        "returncode": 1,
                        "endpoint_sha256": "a",
                        "actual_saved_instructions": 12,
                        "fault_pages": 0,
                    },
                    same[1],
                ]
            )
        )

    def test_relative_snapshot_path_is_resolved_from_root(self):
        with (
            tempfile.TemporaryDirectory() as tmp,
            mock.patch.object(experiment, "ROOT", Path(tmp)),
        ):
            self.assertEqual(
                experiment.resolve_snapshot_path("snapshots/postintro.snap"),
                Path(tmp, "snapshots/postintro.snap").resolve(),
            )
            absolute = Path(tmp, "other.snap").resolve()
            self.assertEqual(experiment.resolve_snapshot_path(absolute), absolute)

    def test_resolve_inputs_preflights_sections_hash(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            main, firmware, sections = (
                root / "main.bin",
                root / "firmware.syx",
                root / "sections",
            )
            main.write_bytes(b"main")
            firmware.write_bytes(b"firmware")
            sections.mkdir()
            (sections / ".source-sha256").write_text(
                experiment.sha256(firmware) + " source\n"
            )
            with (
                mock.patch("emu.config.main_image", return_value=main),
                mock.patch("emu.config.firmware", return_value=firmware),
                mock.patch("emu.config.sections_dir", return_value=sections),
            ):
                self.assertEqual(
                    experiment.resolve_inputs(),
                    (main.resolve(), firmware.resolve(), sections.resolve()),
                )
            (sections / ".source-sha256").write_text("0" * 64 + "\n")
            with (
                mock.patch("emu.config.main_image", return_value=main),
                mock.patch("emu.config.firmware", return_value=firmware),
                mock.patch("emu.config.sections_dir", return_value=sections),
                self.assertRaisesRegex(experiment.RecipeError, "SHA-256"),
            ):
                experiment.resolve_inputs()

    def _execute(
        self,
        actuals=None,
        returncodes=None,
        save_lines=True,
        create_endpoint=True,
        input_batches=None,
        fault_pages: int | None = 0,
    ):
        """Run execute with a child that writes only the --save-at endpoint."""
        actuals = actuals or {"baseline": 2_400_016, "manipulated": 2_400_016}
        returncodes = returncodes or {}
        input_batches = input_batches or {"baseline": 0, "manipulated": 2}
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        root = Path(tmp.name)
        snapshot = root / "input.snap"
        snapshot.write_bytes(b"snapshot")
        main, firmware, sections = (
            root / "main.bin",
            root / "firmware.syx",
            root / "sections",
        )
        main.write_bytes(b"main")
        firmware.write_bytes(b"firmware")
        sections.mkdir()
        recipe = experiment.validate_recipe({**RECIPE, "snapshot": str(snapshot)})
        endpoints = []

        def runner(cmd, **kwargs):
            save_at = cmd[cmd.index("--save-at") + 1]
            _requested, endpoint = save_at.split(":", 1)
            endpoint = Path(endpoint)
            case = "manipulated" if "--input" in cmd else "baseline"
            repetition = len([item for item in endpoints if item[0] == case]) + 1
            endpoints.append((case, endpoint))
            if create_endpoint:
                endpoint.write_bytes(case.encode())
            case_actuals = actuals[case]
            if isinstance(case_actuals, list):
                actual = case_actuals[repetition - 1]
            else:
                actual = case_actuals
            stdout = (
                f"saved snapshot at {actual} instrs -> {endpoint}\n"
                if save_lines
                else ""
            )
            stdout += "[guirun] input ~0.4M: 2120\n" * input_batches[case]
            if fault_pages is not None:
                stdout += f"[guirun] faults: {fault_pages} distinct pages touched\n"
            return SimpleNamespace(
                stdout=stdout,
                stderr="",
                returncode=returncodes.get((case, repetition), 0),
            )

        with mock.patch.object(
            experiment, "resolve_inputs", return_value=(main, firmware, sections)
        ):
            report = run_experiment(recipe, root / "run", runner)
        self.assertEqual(len(endpoints), 4)
        self.assertEqual(len({path for _, path in endpoints}), 4)
        return report

    def test_execute_repeats_cases_at_same_actual_boundary_and_gates_diff(self):
        with mock.patch.object(
            experiment, "diff_snapshots", return_value={"regions": []}
        ) as diff:
            report = self._execute()
        self.assertTrue(report["success"])
        self.assertIsNone(report["comparison_reason"])
        self.assertEqual(
            report["cases"]["baseline"]["requested_save_instructions"], 2_400_000
        )
        self.assertEqual(
            report["cases"]["baseline"]["actual_saved_instructions"], 2_400_016
        )
        diff.assert_called_once()

    def test_execute_nonzero_or_missing_endpoint_gates_diff(self):
        with mock.patch.object(experiment, "diff_snapshots") as diff:
            report = self._execute(returncodes={("baseline", 1): 1})
        self.assertFalse(report["success"])
        self.assertEqual(
            report["cases"]["baseline"]["determinism_reason"],
            "one or more repetitions returned nonzero",
        )
        diff.assert_not_called()
        with mock.patch.object(experiment, "diff_snapshots") as diff:
            report = self._execute(create_endpoint=False)
        self.assertFalse(report["success"])
        self.assertEqual(
            report["cases"]["baseline"]["determinism_reason"],
            "one or more repetitions did not create the requested endpoint",
        )
        diff.assert_not_called()

    def test_execute_missing_save_line_or_mismatched_actual_boundary_gates_diff(self):
        with mock.patch.object(experiment, "diff_snapshots") as diff:
            report = self._execute(save_lines=False)
        self.assertFalse(report["success"])
        self.assertEqual(
            report["cases"]["baseline"]["determinism_reason"],
            "one or more successful repetitions did not report a saved boundary",
        )
        diff.assert_not_called()
        with mock.patch.object(experiment, "diff_snapshots") as diff:
            report = self._execute(
                actuals={"baseline": [2_400_016, 2_400_032], "manipulated": 2_400_016}
            )
        self.assertFalse(report["success"])
        self.assertEqual(
            report["cases"]["baseline"]["determinism_reason"],
            "successful repetitions saved at different actual instruction boundaries",
        )
        diff.assert_not_called()

    def test_execute_cross_case_actual_boundary_mismatch_gates_diff(self):
        with mock.patch.object(experiment, "diff_snapshots") as diff:
            report = self._execute(
                actuals={"baseline": 2_400_016, "manipulated": 2_400_032}
            )
        self.assertFalse(report["success"])
        self.assertEqual(
            report["comparison_reason"],
            "baseline and manipulated cases saved at different actual "
            "instruction boundaries",
        )
        self.assertIsNone(report["diff"])
        diff.assert_not_called()

    def test_execute_incomplete_gesture_gates_diff(self):
        with mock.patch.object(experiment, "diff_snapshots") as diff:
            report = self._execute(input_batches={"baseline": 0, "manipulated": 1})
        self.assertFalse(report["success"])
        self.assertEqual(
            report["cases"]["manipulated"]["determinism_reason"],
            "one or more repetitions delivered the wrong number of panel input batches",
        )
        diff.assert_not_called()

    def test_execute_fault_or_missing_fault_summary_gates_diff(self):
        for fault_pages, reason in (
            (1, "one or more repetitions touched fault pages"),
            (None, "one or more repetitions did not report a final fault count"),
        ):
            with mock.patch.object(experiment, "diff_snapshots") as diff:
                report = self._execute(fault_pages=fault_pages)
            self.assertFalse(report["success"])
            self.assertEqual(report["cases"]["baseline"]["determinism_reason"], reason)
            diff.assert_not_called()

    def test_output_guard_and_existing_directory(self):
        with (
            tempfile.TemporaryDirectory() as tmp,
            mock.patch.object(experiment, "ROOT", Path(tmp)),
        ):
            with self.assertRaises(experiment.RecipeError):
                experiment.guarded_run_dir(Path("/outside"), "button", "one")
            run = Path(tmp) / "out" / "experiments" / "button" / "test-existing"
            run.mkdir(parents=True)
            with self.assertRaises(experiment.RecipeError):
                experiment.guarded_run_dir(Path(tmp) / "out", "button", "test-existing")

    def test_output_guard_rejects_symlinked_ancestor(self):
        with (
            tempfile.TemporaryDirectory() as tmp,
            tempfile.TemporaryDirectory() as outside,
            mock.patch.object(experiment, "ROOT", Path(tmp)),
        ):
            out = Path(tmp) / "out"
            out.mkdir()
            (out / "experiments").symlink_to(outside, target_is_directory=True)
            with self.assertRaisesRegex(experiment.RecipeError, "resolves outside"):
                experiment.guarded_run_dir(out, "button", "one")

    def test_range_comparison_uses_snapdiff_helpers(self):
        left, right = {"left": 1}, {"right": 1}
        with (
            mock.patch.object(
                experiment.snapdiff, "_load_blob", side_effect=[left, right]
            ),
            mock.patch.object(
                experiment.snapdiff, "read", side_effect=[b"\0\0\0\0", b"\0\x01\0\2"]
            ),
            mock.patch.object(
                experiment.snapdiff, "runs", return_value=[(0x80000001, 0x80000004)]
            ),
            mock.patch.object(experiment.snapdiff, "label", return_value="synthetic"),
        ):
            report = experiment.diff_snapshots("a.snap", "b.snap", RECIPE["ranges"])
        self.assertEqual(report["regions"][0]["changed_bytes"], 2)
        self.assertEqual(report["regions"][0]["runs"][0]["a"], "000000")
        self.assertEqual(report["regions"][0]["runs"][0]["b"], "010002")


if __name__ == "__main__":
    unittest.main()
