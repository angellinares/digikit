"""Pure functions of tools/device/devprobe.py: step syntax, key labels, float WAV writing and hit analysis. No device, no sounddevice."""

from __future__ import annotations

import numpy as np
import pytest

from tools.device import devprobe as D
from tools.emulator import audiocheck

RATE = 48000


def test_default_steps():
    assert D.parse_steps("trig1*10@500") == [D.Press(None, "trig1", 10, 500)]


def test_step_forms():
    steps = D.parse_steps("turn:B:-3,wait:250,shot:menu,key:25*2@100,play")
    assert steps == [
        D.Turn(2, -3),
        D.Wait(250),
        D.Shot("menu"),
        D.Press(25, "key:25", 2, 100),
        D.Press(None, "play", 1, 500),
    ]
    assert D.parse_steps("turn:LEVEL:1") == [D.Turn(9, 1)]
    assert D.parse_steps("shot") == [D.Shot(None)]


@pytest.mark.parametrize(
    "bad", ["", "trig1,,play", "turn:B", "turn:Z:1", "wait", "foo:1", "key:x", "a*0"]
)
def test_bad_steps(bad):
    with pytest.raises(ValueError):
        D.parse_steps(bad)


def test_key_labels_from_the_device_file():
    keys = D.load_key_map("dt2")
    assert keys["trig1"] == 25
    assert keys["play"] == 20


def test_write_wav_f32_round_trip(tmp_path):
    t = np.arange(RATE // 10) / RATE
    left = 0.5 * np.sin(2 * np.pi * 440 * t)
    right = 0.25 * np.sin(2 * np.pi * 880 * t)
    x = np.stack([left, right], axis=1).astype(np.float32)
    path = tmp_path / "a.wav"
    D.write_wav_f32(path, x, RATE)
    rate, y = audiocheck.load(path)
    assert rate == RATE
    assert y.shape == x.shape
    assert np.max(np.abs(y - x)) < 1e-6


def hit(freq: float) -> np.ndarray:
    t = np.arange(int(0.15 * RATE)) / RATE
    return 0.5 * np.sin(2 * np.pi * freq * t) * np.exp(-t / 0.03)


def signal(freqs: list[float]) -> np.ndarray:
    x = np.zeros((int(RATE * (0.5 * len(freqs) + 0.5)), 2))
    for i, f in enumerate(freqs):
        s = int((0.2 + 0.5 * i) * RATE)
        h = hit(f)
        x[s : s + len(h), :] = h[:, None]
    return x


def test_analyse_consistent_hits():
    events = [{"t": 0.2 + 0.5 * i, "kind": "key"} for i in range(3)]
    r = D.analyse(signal([440, 440, 440]), RATE, events)
    assert r["summary"]["hits"] == 3
    assert r["summary"]["key_presses"] == 3
    assert r["summary"]["consistent"] is True
    for h in r["hits"]:
        assert h["f0_hz"] == pytest.approx(440, rel=0.02)
        assert abs(h["latency_ms"]) < 10


def test_analyse_different_hits():
    r = D.analyse(signal([440, 880, 220]), RATE)
    assert r["summary"]["consistent"] is False
    assert r["summary"]["f0_spread"] == pytest.approx(4, rel=0.05)
    assert r["hits"][0]["latency_ms"] is None
