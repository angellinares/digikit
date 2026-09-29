"""mypy check for the SHARC core and its concrete/symbolic entry points.

The SHARC core (tools/sharc_core) also runs under PyPy 3.11
(tests/test_pypy.py), so it is checked against Python 3.11 here too (see
[tool.mypy] in pyproject.toml). Add a path here once it passes cleanly; the
list should only grow.
"""

import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

TYPED = [
    "emu/livesharc.py",
    "emu/mmiotrace.py",
    "emu/sharc_peer.py",
    "tests/test_mmio_record.py",
    "tests/test_mmiotrace.py",
    "tools/cf.py",
    "tools/cf_lockstep.py",
    "tools/cf_names.py",
    "tools/cfdb.py",
    "tools/cfisa/gen.py",
    "tools/cfisa/oracle.py",
    "tools/live_audio.py",
    "tools/live_gui_check.py",
    "tools/mmio_record.py",
    "tools/sharc.py",
    "tools/sharc_core",
    "tools/sharc_dac.py",
    "tools/sharc_diff.py",
    "tools/sharc_disasm.py",
    "tools/sharc_framemap.py",
    "tools/sharc_harness.py",
    "tools/sharc_inputs.py",
    "tools/sharc_proc.py",
    "tools/sharc_rsgen.py",
    "tools/sharc_run.py",
    "tools/sharc_subset_lint.py",
    "tools/sharc_survey.py",
    "tools/sharc_symbols.py",
    "tools/sharc_trace.py",
    "tools/sharc_transpile.py",
    "tools/sharc_transpile_infer.py",
    "tools/sharc_transpile_run.py",
    "tools/sharc_widthaudit.py",
    "tools/sharcldr.py",
    "tools/snapread.py",
]


def test_mypy():
    result = subprocess.run(
        [sys.executable, "-m", "mypy", *TYPED],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stdout + result.stderr
