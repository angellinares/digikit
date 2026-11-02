"""Bounded FX send analysis commands without device or emulator inputs."""

import json
import struct
from pathlib import Path

import numpy as np
import pytest

from tools.patchkit import pk_digikit_fxsend as fxsend


def write_wav(path: Path, samples: np.ndarray) -> None:
    body = samples.astype("<f4").tobytes()
    fmt = struct.pack("<HHIIHH", 3, 2, 48000, 48000 * 8, 8, 32)
    chunks = b"fmt " + struct.pack("<I", len(fmt)) + fmt
    chunks += b"data" + struct.pack("<I", len(body)) + body
    path.write_bytes(b"RIFF" + struct.pack("<I", 4 + len(chunks)) + b"WAVE" + chunks)


@pytest.mark.parametrize("shift", [-80, 0, 120])
def test_added_factor_recovers_scaled_shifted_waveform(tmp_path, capsys, shift):
    # A deterministic, nonperiodic stereo signal avoids ambiguous periodic fits.
    dry = np.random.default_rng(17).uniform(-0.2, 0.2, 5000).astype(np.float32)
    out = np.zeros_like(dry)
    if shift >= 0:
        out[shift:] = 1.5 * dry[: len(dry) - shift]
    else:
        out[:shift] = 1.5 * dry[-shift:]
    base_path, out_path = tmp_path / "base.wav", tmp_path / "out.wav"
    write_wav(base_path, dry)
    write_wav(out_path, out)
    assert fxsend.main(["added-factor", str(base_path), str(out_path)]) == 0
    output = capsys.readouterr()
    assert not output.err
    assert json.loads(output.out)["added_factor"] == pytest.approx(0.5, abs=1e-7)


@pytest.mark.parametrize("silent", [True, False])
def test_pcm_summary_includes_silence(tmp_path, capsys, silent):
    samples = np.zeros(40, dtype=np.float32)
    if not silent:
        samples[7] = -0.375
    path = tmp_path / "capture.wav"
    write_wav(path, samples)
    assert fxsend.main(["pcm", str(path)]) == 0
    assert json.loads(capsys.readouterr().out) == {
        "samples": 40,
        "finite": True,
        "peak": 0.0 if silent else 0.375,
    }


@pytest.mark.parametrize("command", ["pcm", "added-factor"])
@pytest.mark.parametrize("bad", ["empty", "nan", "inf", "malformed"])
def test_invalid_pcm_fails_without_json(tmp_path, capsys, command, bad):
    samples = np.ones(2000, dtype=np.float32)
    if bad == "empty":
        samples = samples[:0]
    elif bad == "nan":
        samples[42] = np.nan
    elif bad == "inf":
        samples[42] = np.inf
    path = tmp_path / "bad.wav"
    write_wav(path, samples)
    if bad == "malformed":
        path.write_bytes(b"not a WAV")
    args = [command, str(path)]
    if command == "added-factor":
        args.append(str(path))
    assert fxsend.main(args) == 1
    output = capsys.readouterr()
    assert not output.out
    assert output.err


@pytest.mark.parametrize("size", [2, 1200])
def test_added_factor_rejects_short_capture(tmp_path, capsys, size):
    path = tmp_path / "short.wav"
    write_wav(path, np.ones(size, dtype=np.float32))
    assert fxsend.main(["added-factor", str(path), str(path)]) == 1
    output = capsys.readouterr()
    assert not output.out
    assert "more than 1200 samples" in output.err


def test_added_factor_rejects_silent_reference_fit_window(tmp_path, capsys):
    samples = np.zeros(2000, dtype=np.float32)
    samples[0] = 0.25  # Audible outside the fit window still cannot define its gain.
    path = tmp_path / "silent-fit.wav"
    write_wav(path, samples)
    assert fxsend.main(["added-factor", str(path), str(path)]) == 1
    output = capsys.readouterr()
    assert not output.out
    assert "non-silent reference fit window" in output.err
