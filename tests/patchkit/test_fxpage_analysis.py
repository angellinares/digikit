"""FXPAGE's JSON adapter leaves per-leg PCM tolerances to its caller."""

import json
from pathlib import Path

from tools.patchkit import hook_ab
from tools.patchkit import pk_digikit_fxpage as fxpage


def test_compare_reports_above_two_ulp_without_applying_generic_gate(
    monkeypatch, capsys
):
    """The B/E/C legs need the oracle's metrics even above the generic 2-ULP gate."""
    inputs = {Path("dry.wav"): object(), Path("wet.wav"): object()}
    monkeypatch.setattr(hook_ab, "read_wav", inputs.__getitem__)
    calls = []
    report = {"max_err_ulps": 3.5, "rms_db": -1.2, "shift": 64, "identical": False}

    def compare(base, sample, gain):
        calls.append((base, sample, gain))
        return report

    monkeypatch.setattr(fxpage, "scaled", compare)
    assert fxpage.main(["compare", "dry.wav", "wet.wav", "--gain", "0.75"]) == 0
    assert calls == [(inputs[Path("dry.wav")], inputs[Path("wet.wav")], 0.75)]
    assert json.loads(capsys.readouterr().out) == report
