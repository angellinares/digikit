"""tools/emulator/palette.py on synthetic hits: a knob that moves the sound, one that does not, two presets
that are the same sound, and the report files from a manifest."""

from __future__ import annotations

import json
import pathlib
import wave

import numpy as np
import pytest

from tools.emulator import palette as pl

R = 48000


def hit(f: float, decay_s: float, amp: float = 0.5, secs: float = 1.5) -> np.ndarray:
    t = np.arange(int(R * secs)) / R
    x = amp * np.sin(2 * np.pi * f * t) * np.exp(-t / decay_s)
    return x[:, None].repeat(2, axis=1)


def write(path: pathlib.Path, x: np.ndarray) -> None:
    with wave.open(str(path), "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(R)
        w.writeframes((np.clip(x, -1, 1) * 32767).astype("<i2").tobytes())


def test_describe_a_decaying_tone() -> None:
    d = pl.describe(hit(1000, 0.1), R)
    assert not d["silent"] and d["width"] == 0.0
    assert d["centroid_hz"] == pytest.approx(1000, rel=0.15)
    assert d["t60_ms"] == pytest.approx(
        0.1 * 6.9 * 1000, rel=0.1
    )  # -60 dB after ln(1000) time constants
    assert pl.fingerprint(d) == "warm tonal medium"


def test_describe_silence_and_width() -> None:
    assert pl.describe(np.zeros((1000, 2)), R)["silent"]
    x = hit(1000, 0.2)
    x[:, 1] *= -1  # all side, no mid
    assert pl.describe(x, R)["silent"]  # the mono mix is silent
    y = hit(1000, 0.2)
    y[:, 1] *= 0.5
    assert pl.describe(y, R)["width"] > 0.1


def test_a_dead_knob_and_a_live_one() -> None:
    same = [pl.describe(hit(1000, 0.2), R) for _ in range(5)]
    moves = [pl.describe(hit(f, 0.2), R) for f in (200, 400, 800, 1600, 3200)]
    assert pl.is_dead(pl.travel(same))
    assert not pl.is_dead(pl.travel(moves))
    assert pl.travel(moves)["centroid"] == pytest.approx(4, abs=0.6)


def test_report_from_a_manifest(tmp_path: pathlib.Path) -> None:
    rows = []
    for i, f in enumerate((200, 800, 3200)):
        write(tmp_path / f"tune-{i}.wav", hit(f, 0.2))
        rows.append(
            {
                "section": "sweep",
                "param": "tune",
                "label": f"tune={i}",
                "wav": f"tune-{i}.wav",
            }
        )
    for i in range(3):
        write(tmp_path / f"dead-{i}.wav", hit(1000, 0.2))
        rows.append(
            {
                "section": "sweep",
                "param": "dead",
                "label": f"dead={i}",
                "wav": f"dead-{i}.wav",
            }
        )
    for name, f in (("A", 500), ("B", 500), ("C", 4000)):
        write(tmp_path / f"p{name}.wav", hit(f, 0.3))
        rows.append(
            {
                "section": "preset",
                "preset": name,
                "label": f"{name} voice 0",
                "wav": f"p{name}.wav",
            }
        )
    write(tmp_path / "n60.wav", hit(262, 0.5))
    rows.append({"section": "note", "label": "note 60", "wav": "n60.wav"})
    (tmp_path / "m.json").write_text(json.dumps({"title": "test", "rows": rows}))
    out = tmp_path / "out"
    assert pl.main([str(tmp_path / "m.json"), "--out", str(out)]) == 0
    md = (out / "palette.md").read_text()
    assert "1 of 2 controls move nothing audible:** `dead`" in md
    assert "A = B" in md and "A = C" not in md
    page = (out / "palette.html").read_text()
    assert (
        page.count("<audio") == len(rows)
        and 'src="../tune-0.wav"' in page
        and page.count("<svg") == len(rows)
    )
