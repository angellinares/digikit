"""PCM comparison and its machine-readable CLI (no emulator)."""

import json
import math
import struct
from pathlib import Path

import numpy as np
import pytest

from tools.patchkit import hook_ab


def signal(lead: int, gain: float = 1.0) -> np.ndarray:
    # Include an attack near the onset threshold: the threshold must scale with gain.
    wave = np.array([1.5e-6, 0.125, -0.25, 0.375, -0.5, 0.25], dtype=np.float32)
    return np.concatenate((np.zeros(lead, dtype=np.float32), wave * gain))


def write_wav(path: Path, samples: np.ndarray) -> None:
    body = samples.astype("<f4").tobytes()
    fmt = struct.pack("<HHIIHH", 3, 2, 48000, 48000 * 8, 8, 32)
    chunks = b"fmt " + struct.pack("<I", len(fmt)) + fmt
    chunks += b"data" + struct.pack("<I", len(body)) + body
    path.write_bytes(b"RIFF" + struct.pack("<I", 4 + len(chunks)) + b"WAVE" + chunks)


def test_alignment_preserves_comparison_metrics_and_reports_shift():
    ref, run = signal(4), signal(12, 0.5)
    result = hook_ab.compare_aligned(ref, run, 0.5)
    assert result == {**hook_ab.compare(ref[4:], run[12:], 0.5), "shift": 8}
    assert result["max_err_ulps"] <= 2.0
    assert result["rms_db"] == pytest.approx(20 * math.log10(0.5), abs=0.001)
    assert not result["identical"]
    assert hook_ab.compare_aligned(run, ref, 2.0)["shift"] == -8
    assert hook_ab.compare_aligned(ref, signal(12), 1.0)["identical"]


def test_alignment_uses_only_the_common_signal_length():
    ref, run = signal(4), signal(12, 0.5)[:-2]
    result = hook_ab.compare_aligned(ref, run, 0.5)
    assert result["samples"] == 4
    assert result["length_equal"]
    assert result["max_err_ulps"] <= 2.0


def test_wrong_gain_is_detected_after_alignment():
    result = hook_ab.compare_aligned(signal(4), signal(12, 0.5), 0.25)
    assert result["shift"] == 8
    assert result["max_err_ulps"] > 2.0


@pytest.mark.parametrize("silent", ["base", "test", "both"])
def test_missing_onset_reports_failure(silent):
    base = np.zeros(20, dtype=np.float32) if silent != "test" else signal(4)
    test = np.zeros(20, dtype=np.float32) if silent != "base" else signal(12)
    result = hook_ab.compare_aligned(base, test, 0.5)
    assert result["shift"] is None
    assert result["rms_db"] is None
    assert result["max_err_ulps"] > 2.0


@pytest.mark.parametrize(
    "gain, quiet, exit_code", [(0.5, False, 0), (0.25, False, 1), (0.5, True, 1)]
)
def test_aligned_json_cli_is_parseable_and_returns_comparison_status(
    tmp_path, capsys, gain, quiet, exit_code
):
    base_path, test_path = tmp_path / "base.wav", tmp_path / "test.wav"
    base = signal(4)
    test = np.zeros(20, dtype=np.float32) if quiet else signal(12, 0.5)
    write_wav(base_path, base)
    write_wav(test_path, test)
    assert (
        hook_ab.main(
            [
                "compare",
                str(base_path),
                str(test_path),
                "--gain",
                str(gain),
                "--align-onset",
                "--json",
            ]
        )
        == exit_code
    )
    output = capsys.readouterr()
    assert not output.err
    result = json.loads(output.out)
    assert result == hook_ab.compare_aligned(base, test, gain)
    assert "NaN" not in output.out
    assert "Infinity" not in output.out


def test_default_cli_keeps_human_readable_output(tmp_path, capsys):
    base_path, test_path = tmp_path / "base.wav", tmp_path / "test.wav"
    write_wav(base_path, signal(4))
    write_wav(test_path, signal(4, 0.5))
    assert (
        hook_ab.main(["compare", str(base_path), str(test_path), "--gain", "0.5"]) == 0
    )
    output = capsys.readouterr().out
    assert "max_err_ulps" in output
    assert "PASS (gain 0.5: error within 2 ulps)" in output
