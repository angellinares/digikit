"""Offline coverage of the retained frequency-shifter PCM oracles' JSON adapter."""

import copy
import json
import struct
from pathlib import Path

import numpy as np
import pytest

from tools.patchkit import pk_digikit_fxshift as shifter


def write_wav(path: Path, mono: np.ndarray) -> None:
    body = np.repeat(mono.astype("<f4"), 2).tobytes()
    fmt = struct.pack("<HHIIHH", 3, 2, 48000, 48000 * 8, 8, 32)
    chunks = b"fmt " + struct.pack("<I", len(fmt)) + fmt
    chunks += b"data" + struct.pack("<I", len(body)) + body
    path.write_bytes(b"RIFF" + struct.pack("<I", 4 + len(chunks)) + b"WAVE" + chunks)


@pytest.fixture(scope="module")
def captures(tmp_path_factory):
    directory = tmp_path_factory.mktemp("fxshift")
    model = shifter.load_model("digikit/freqshift")
    n = np.arange(48000)
    f0 = 440.0
    dry = (0.2 * np.sin(2 * np.pi * f0 * n / shifter.FS)).astype(np.float32)
    values = {"up": 40.0, "down": -40.0, "e": -40.0, "send": 40.0}
    pcm = {"dry": dry, "a": dry, "b": dry, "other": dry}
    for tag, key in (("c", "up"), ("d", "down"), ("e", "e")):
        hz = model.target_inc(values[key]) * shifter.FS / 2**32
        pcm[tag] = 0.2 * np.sin(2 * np.pi * (f0 + hz) * n / shifter.FS)
    gain = (40 / 127) ** 2
    added = shifter.model_out(gain * dry, values["send"], model)
    pcm["send"] = dry + added
    pcm["half"] = dry + 0.5 * added
    paths = {}
    for tag, signal in pcm.items():
        path = directory / f"{tag}.wav"
        write_wav(path, signal)
        paths[tag] = str(path)
    return (
        {
            "module": "digikit/freqshift",
            "pcm": paths,
            "shift": values,
            "mix": {"send": 1.0, "half": 0.5},
        },
        pcm,
        n,
    )


def test_json_adapter_accepts_independent_master_tones_and_float_model_send(
    captures, tmp_path, capsys
):
    request, _, _ = captures
    path = tmp_path / "request.json"
    path.write_text(json.dumps(request))
    assert shifter.main([str(path)]) == 0
    result = json.loads(capsys.readouterr().out)
    assert result["pass"], [item for item in result["checks"] if not item["pass"]]


@pytest.mark.parametrize(
    "fault", ["image", "residual", "level", "send_gain", "other", "mix_zero"]
)
def test_pcm_oracles_reject_spectral_route_and_endpoint_regressions(
    captures, tmp_path, fault
):
    request, pcm, n = captures
    request = copy.deepcopy(request)
    tag, label = "c", "shift C:"
    changed = pcm[tag].copy()
    if fault == "image":
        model = shifter.load_model(request["module"])
        hz = model.target_inc(request["shift"]["up"]) * shifter.FS / 2**32
        changed += 0.01 * np.sin(2 * np.pi * abs(440 - hz) * n / shifter.FS)
    elif fault == "residual":
        changed += 0.01 * pcm["dry"]
    elif fault == "level":
        changed *= 1.01
    elif fault == "send_gain":
        tag, label = "send", "send send:"
        changed = pcm["dry"] + 2 * (pcm["send"] - pcm["dry"])
    elif fault == "other":
        tag, label = "other", "other:"
        changed = pcm["dry"] * 1.01
    elif fault == "mix_zero":
        tag, label = "b", "shift B:"
        changed = pcm["dry"] * 1.01
    path = tmp_path / f"{fault}.wav"
    write_wav(path, changed)
    request["pcm"][tag] = str(path)
    result = shifter.pcm_report(request)
    assert not result["pass"]
    assert any(
        item["label"].startswith(label) and not item["pass"]
        for item in result["checks"]
    )


@pytest.mark.parametrize("bad", ["short", "nan", "silent"])
def test_json_adapter_rejects_invalid_controls_without_json(
    captures, tmp_path, capsys, bad
):
    request, _, _ = captures
    request = copy.deepcopy(request)
    samples = np.zeros(48000, dtype=np.float32)
    if bad == "short":
        samples = samples[:100]
    elif bad == "nan":
        samples[42] = np.nan
    wav = tmp_path / "bad.wav"
    write_wav(wav, samples)
    request["pcm"]["dry"] = str(wav)
    path = tmp_path / "request.json"
    path.write_text(json.dumps(request))
    assert shifter.main([str(path)]) == 1
    output = capsys.readouterr()
    assert not output.out
    assert output.err
