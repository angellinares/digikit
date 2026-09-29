"""Ruff lint and format checks for the files that have been cleaned.

The rules live in pyproject.toml. Add a path here once it passes both
`ruff check` and `ruff format --check`; the list should only grow.
"""

import os
import subprocess
import sys

import pytest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

CLEAN = [
    "emu/dspi2.py",
    "emu/dspiframe.py",
    "emu/livesharc.py",
    "emu/sharc_capture.py",
    "emu/sharc_peer.py",
    "emu/ssi.py",
    "tests/test_cf.py",
    "tests/test_cf_names.py",
    "tests/test_cfdb.py",
    "tests/test_checkpoint_make_fast.py",
    "tests/test_dspboot_esdhc_wiring.py",
    "tests/test_dspi2.py",
    "tests/test_dt2_reach_running.py",
    "tests/test_dt2gui.py",
    "tests/test_esdhc_identity.py",
    "tests/test_factory_table.py",
    "tests/test_lint.py",
    "tests/test_live_audio.py",
    "tests/test_live_gui.py",
    "tests/test_live_sharc.py",
    "tests/test_livesharc.py",
    "tests/test_plusdrive.py",
    "tests/test_plusdrive_firmware.py",
    "tests/test_pypy.py",
    "tests/test_sharc_armpath.py",
    "tests/test_sharc_calltrace.py",
    "tests/test_sharc_capture.py",
    "tests/test_sharc_capture_run.py",
    "tests/test_sharc_compute_mr.py",
    "tests/test_sharc_compute_shift.py",
    "tests/test_sharc_compute_table.py",
    "tests/test_sharc_contract.py",
    "tests/test_sharc_coverage.py",
    "tests/test_sharc_dac.py",
    "tests/test_sharc_diff.py",
    "tests/test_sharc_disasm.py",
    "tests/test_sharc_dmamap.py",
    "tests/test_sharc_framemap.py",
    "tests/test_sharc_golden.py",
    "tests/test_sharc_graph.py",
    "tests/test_sharc_harness.py",
    "tests/test_sharc_inputs.py",
    "tests/test_sharc_lp0.py",
    "tests/test_sharc_memdiff.py",
    "tests/test_sharc_peer.py",
    "tests/test_sharc_proc.py",
    "tests/test_sharc_replay.py",
    "tests/test_sharc_run.py",
    "tests/test_sharc_subset_lint.py",
    "tests/test_sharc_survey.py",
    "tests/test_sharc_symbols.py",
    "tests/test_sharc_trace_alu.py",
    "tests/test_sharc_trace_double.py",
    "tests/test_sharc_trace_forms.py",
    "tests/test_sharc_trace_mult.py",
    "tests/test_sharc_trace_simd.py",
    "tests/test_sharc_transpile.py",
    "tests/test_sharc_widthaudit.py",
    "tests/test_sharcldr.py",
    "tests/test_snapread.py",
    "tests/test_ssi_coalesce.py",
    "tests/test_timebase.py",
    "tests/test_timer_hold.py",
    "tests/test_types.py",
    "tools/cf.py",
    "tools/cf_names.py",
    "tools/cfdb.py",
    "tools/cfrealtime.py",
    "tools/dt2_reach_running.py",
    "tools/dt2gui.py",
    "tools/gen_test_samples.py",
    "tools/live_audio.py",
    "tools/live_gui_check.py",
    "tools/plusdrive.py",
    "tools/plusdrive_check.py",
    "tools/sharc.py",
    "tools/sharc_armpath.py",
    "tools/sharc_calltrace.py",
    "tools/sharc_capture_run.py",
    "tools/sharc_contract.py",
    "tools/sharc_core",
    "tools/sharc_coverage.py",
    "tools/sharc_dac.py",
    "tools/sharc_diff.py",
    "tools/sharc_disasm.py",
    "tools/sharc_dmamap.py",
    "tools/sharc_framemap.py",
    "tools/sharc_harness.py",
    "tools/sharc_inputs.py",
    "tools/sharc_lp0.py",
    "tools/sharc_memdiff.py",
    "tools/sharc_proc.py",
    "tools/sharc_replay.py",
    "tools/sharc_rsgen.py",
    "tools/sharc_rsvec.py",
    "tools/sharc_run.py",
    "tools/sharc_subset_lint.py",
    "tools/sharc_survey.py",
    "tools/sharc_symbols.py",
    "tools/sharc_transpile.py",
    "tools/sharc_transpile_infer.py",
    "tools/sharc_transpile_run.py",
    "tools/sharc_widthaudit.py",
    "tools/sharcldr.py",
    "tools/snapeq.py",
    "tools/snapread.py",
]


def ruff(*args):
    return subprocess.run(
        [sys.executable, "-m", "ruff", *args, *CLEAN],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )


@pytest.mark.parametrize(
    "args",
    [("check", "--no-cache"), ("format", "--check", "--no-cache")],
    ids=["check", "format"],
)
def test_ruff(args):
    result = ruff(*args)
    assert result.returncode == 0, result.stdout + result.stderr
