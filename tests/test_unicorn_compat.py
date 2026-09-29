# pyright: reportMissingImports=false
import inspect
import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import ANY, patch

import emu.unicorn_compat as unicorn_compat
from emu.dtim import Dtims, Timers
from emu.longrun import spin
from emu.pit import Pits


class UnicornCompatibilityTest(unittest.TestCase):
    def setUp(self):
        unicorn_compat._evaluate_runtime.cache_clear()

    def tearDown(self):
        unicorn_compat._evaluate_runtime.cache_clear()

    def test_evaluator_reports_both_semantic_paths(self):
        with (
            patch.object(
                unicorn_compat,
                "_run_case",
                side_effect=[{"pass": True, "sr": 4}, {"pass": False, "sr": 4}],
            ) as run,
            patch.object(
                unicorn_compat, "_run_count_boundary_case", return_value={"pass": True}
            ) as count_run,
            patch.object(
                unicorn_compat, "_run_btst_flush_case", return_value={"pass": True}
            ) as btst_run,
            patch.object(
                unicorn_compat, "_run_mac_load_case", return_value={"pass": True}
            ) as mac_run,
            patch.object(
                unicorn_compat,
                "_run_emac_fractional_case",
                return_value={"pass": True},
            ) as frac_run,
        ):
            result = unicorn_compat.evaluate(factory=object())
        self.assertFalse(result["compatible"])
        self.assertEqual(
            list(result["cases"]),
            [
                "zero_z_taken",
                "nonzero_z_clear",
                "count_boundary_cmp_z",
                "btst_flush_z",
                "emac_mac_with_load",
                "emac_fractional",
            ],
        )
        self.assertEqual(run.call_count, 2)
        count_run.assert_called_once_with(ANY)
        btst_run.assert_called_once_with(ANY)
        mac_run.assert_called_once_with(ANY)
        frac_run.assert_called_once_with(ANY)

    def test_btst_keeps_z_after_code_hook_ccr_sync(self):
        # Runs the loaded Unicorn: fails on a build with the hook CCR sync
        # patch but without unicorn-2.1.4-m68k-flush-flags-sync.patch.
        from unicorn import UC_ARCH_M68K, UC_MODE_BIG_ENDIAN, Uc
        from unicorn.m68k_const import UC_CPU_M68K_CFV4E

        def factory():
            uc = Uc(UC_ARCH_M68K, UC_MODE_BIG_ENDIAN)
            uc.ctl_set_cpu_model(UC_CPU_M68K_CFV4E)
            return uc

        result = unicorn_compat._run_btst_flush_case(factory)
        self.assertEqual(
            result,
            {
                "tb_boundary_branch_value": 2,
                "count_stop_pc": 0x100C,
                "count_stop_sr": 0x0C,
                "count_stop_branch_value": 2,
                "pass": True,
            },
        )

    def test_failure_is_actionable_without_patched_runtime(self):
        with patch.object(
            unicorn_compat,
            "evaluate",
            return_value={
                "compatible": False,
                "cases": {
                    "zero_z_taken": {"pass": False},
                    "nonzero_z_clear": {"pass": True},
                },
            },
        ):
            with self.assertRaisesRegex(RuntimeError, "install-patched-unicorn.sh"):
                unicorn_compat.require_compatible_unicorn()

    def test_runtime_evaluation_is_cached_but_injected_evaluation_is_not(self):
        compatible = {"compatible": True, "cases": {}}
        with patch.object(
            unicorn_compat, "evaluate", return_value=compatible
        ) as evaluate:
            self.assertEqual(unicorn_compat._evaluate_runtime(), compatible)
            self.assertEqual(unicorn_compat._evaluate_runtime(), compatible)
        self.assertEqual(evaluate.call_count, 1)

    def test_installer_dry_run_selects_the_bound_dynamic_payload(self):
        script = Path(__file__).parents[1] / "tools" / "install-patched-unicorn.sh"
        output = subprocess.run(
            ["bash", str(script), "--dry-run"],
            check=True,
            text=True,
            capture_output=True,
        ).stdout
        expected = (
            "libunicorn.2.dylib" if sys.platform == "darwin" else "libunicorn.so.2"
        )
        self.assertIn("target=", output)
        self.assertTrue(
            output.split("target=", 1)[1].splitlines()[0].endswith(expected)
        )
        self.assertIn("expected-build-payload=build/" + expected, output)

    def test_timer_execution_has_no_arbitrary_cap(self):
        for callable_ in (spin, Pits.step, Dtims.step, Timers.step):
            self.assertNotIn("cap", inspect.signature(callable_).parameters)

    def test_legacy_boot_cannot_bypass_guard(self):
        import emu.boot as legacy_boot

        with (
            patch.object(
                legacy_boot,
                "require_compatible_unicorn",
                side_effect=RuntimeError("incompatible runtime"),
            ),
            patch.object(legacy_boot, "Uc") as constructor,
        ):
            with self.assertRaisesRegex(RuntimeError, "incompatible runtime"):
                legacy_boot.boot(limit=0)
        constructor.assert_not_called()


if __name__ == "__main__":
    unittest.main()
