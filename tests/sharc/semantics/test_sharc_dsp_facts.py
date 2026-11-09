"""The value items of the `test/dsp-facts` probe on a Digitakt II (OS 1.17): FIX, float ALU edge cases, fixed-point overflow with and
without ALUSAT, the float multiplier's invalid result, and a multiplier overflow read through MR1F.

The core reproduces the 59 items exactly: result, ASTAT[13:0] and STKY[9:0] (`tests/fixtures/sharc/dsp-facts-values-dt2-1.17.json`,
written by `tools/sharc_dev/dsp_facts_values.py`; finding 103).
"""

from __future__ import annotations

from pathlib import Path

from support.paths import REPO_ROOT

from tools.sharc_dev import dsp_facts_values

FIXTURE = Path(REPO_ROOT) / dsp_facts_values.FIXTURE


def test_core_reproduces_the_device_capture():
    items = dsp_facts_values.load_items(FIXTURE)
    assert len(items) == 59
    assert dsp_facts_values.check(items) == []
