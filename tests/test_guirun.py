# pyright: reportMissingImports=false
"""Unit coverage for guirun's stateful timer checkpoint setup."""

import os
import sys
import tempfile
import unittest
from collections import Counter
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest import mock

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(__file__)), "tools"))
import guirun


class GuirunTimerCheckpointTest(unittest.TestCase):
    def test_intro_timer_mode_defaults_to_historical_hold(self):
        self.assertEqual(guirun.parse_args([]).intro_timers, "held")

    def test_intro_timer_mode_accepts_real_pit3(self):
        self.assertEqual(
            guirun.parse_args(["checkpoint.snap", "--intro-timers", "pit3"])
            .intro_timers,
            "pit3",
        )

    def test_main_requests_deferred_timer_restore(self):
        class StopAfterBuild(Exception):
            pass

        def fake_build(*args, **kwargs):
            self.assertEqual(kwargs["deferred_components"], ("timers",))
            self.assertTrue(kwargs["unblock"])
            self.assertIsNone(kwargs["ssi0_request_hz"])
            self.assertFalse(kwargs["ssi0_legacy_upgrade"])
            raise StopAfterBuild

        with (
            mock.patch.object(sys, "argv", ["guirun.py", "checkpoint.snap"]),
            mock.patch.object(guirun, "build", side_effect=fake_build),
            self.assertRaises(StopAfterBuild),
        ):
            guirun.main()

    def test_main_accepts_no_unblock(self):
        class StopAfterBuild(Exception):
            pass

        def fake_build(*args, **kwargs):
            self.assertFalse(kwargs["unblock"])
            raise StopAfterBuild

        with (
            mock.patch.object(
                sys,
                "argv",
                ["guirun.py", "checkpoint.snap", "--no-unblock"],
            ),
            mock.patch.object(guirun, "build", side_effect=fake_build),
            self.assertRaises(StopAfterBuild),
        ):
            guirun.main()

    def test_main_forwards_explicit_ssi0_upgrade(self):
        class StopAfterBuild(Exception):
            pass

        def fake_build(*args, **kwargs):
            self.assertEqual(kwargs["ssi0_request_hz"], 96000)
            self.assertTrue(kwargs["ssi0_legacy_upgrade"])
            raise StopAfterBuild

        with (
            mock.patch.object(
                sys,
                "argv",
                [
                    "guirun.py",
                    "checkpoint.snap",
                    "--ssi0-request-hz",
                    "96000",
                    "--ssi0-upgrade-legacy",
                ],
            ),
            mock.patch.object(guirun, "build", side_effect=fake_build),
            self.assertRaises(StopAfterBuild),
        ):
            guirun.main()

    def test_ssi0_upgrade_requires_explicit_rate(self):
        with (
            mock.patch.object(
                sys,
                "argv",
                ["guirun.py", "checkpoint.snap", "--ssi0-upgrade-legacy"],
            ),
            self.assertRaisesRegex(SystemExit, "requires --ssi0-request-hz"),
        ):
            guirun.main()

    def test_restored_timers_are_preserved_without_construction(self):
        timers = SimpleNamespace(sources=(SimpleNamespace(ips=18_720_000),))

        def restore():
            events["checkpoint_components"]["timers"] = timers
            return timers

        events: dict[str, Any] = {
            "restore_checkpoint_timers": restore,
            "checkpoint_components": {},
        }
        result, restored = guirun.restore_or_construct_timers(
            events,
            lambda: self.fail("must not construct over restored timer state"),
        )

        self.assertIs(result, timers)
        self.assertTrue(restored)
        self.assertIs(events["checkpoint_components"]["timers"], timers)

    def test_legacy_timers_are_constructed_and_registered(self):
        timers = SimpleNamespace(sources=(SimpleNamespace(ips=4_680_000),))
        events: dict[str, Any] = {
            "restore_checkpoint_timers": lambda: None,
            "checkpoint_components": {},
        }

        result, restored = guirun.restore_or_construct_timers(events, lambda: timers)

        self.assertIs(result, timers)
        self.assertFalse(restored)
        self.assertIs(events["checkpoint_components"]["timers"], timers)

    def test_explicit_ips_rejects_different_saved_rate(self):
        timers = SimpleNamespace(sources=(SimpleNamespace(ips=18_720_000),))
        events: dict[str, Any] = {
            "restore_checkpoint_timers": lambda: timers,
            "checkpoint_components": {},
        }

        with self.assertRaisesRegex(RuntimeError, "conflicts with checkpoint"):
            guirun.restore_or_construct_timers(events, lambda: None, 4_680_000)

    def test_run_timer_clock_removes_restored_checkpoint_origin(self):
        timers = SimpleNamespace(now=74_936_800)
        self.assertEqual(guirun.run_timer_clock(timers, 60_272_373), 14_664_427)

    def test_intro_handover_restores_channels_from_checkpoint_topology(self):
        pit = SimpleNamespace(channels=(3,))
        timers = SimpleNamespace(sources=(pit,), release=mock.Mock())

        guirun.release_intro_timers(timers)

        self.assertEqual(pit.channels, (3, 2, 0))
        timers.release.assert_called_once_with()

    def test_intro_handover_preserves_complete_channels(self):
        pit = SimpleNamespace(channels=(3, 2, 0))
        timers = SimpleNamespace(sources=(pit,), release=mock.Mock())

        guirun.release_intro_timers(timers)

        self.assertEqual(pit.channels, (3, 2, 0))
        timers.release.assert_called_once_with()

    def test_pit3_intro_constructs_only_pit3_and_holds_dtims(self):
        args = SimpleNamespace(intro_timers="pit3", ips=4_680_000)
        with (
            mock.patch.object(guirun, "Pits", autospec=True) as pits,
            mock.patch.object(guirun, "Dtims", autospec=True) as dtims,
            mock.patch.object(guirun, "Timers", autospec=True) as timers,
        ):
            guirun.construct_timers("machine", args, intro=True)

        pits.assert_called_once_with(
            "machine", channels=(3,), hold=False, instr_per_sec=4_680_000
        )
        dtims.assert_called_once_with(
            "machine", channels=(3,), hold=True, instr_per_sec=4_680_000
        )
        timers.assert_called_once_with(pits.return_value, dtims.return_value)

    def test_block_profile_is_sorted_and_labelled_perturbing(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "blocks.json"
            guirun.write_block_profile(path, Counter({0x20: 2, 0x10: 3}))

            self.assertEqual(
                path.read_text(),
                '{"entries": [{"address": 16, "hits": 3}, '
                '{"address": 32, "hits": 2}], "kind": "basic-block entries", '
                '"perturbing": true}',
            )


if __name__ == "__main__":
    unittest.main()
