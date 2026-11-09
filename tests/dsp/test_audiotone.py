"""tools/emulator/audiotone.py on synthetic signals with known answers.

The polyBLEP table: dn2_firmware chose a polyBLEP for the Waverider sub's SQR on these figures, the power
between the sub's harmonics per note, naive against polyBLEP (dn2_firmware site/waverider.html, Technical
tab, 2026-10-29). The sub's naive square is a u32 phase accumulator's top bit; the polyBLEP here is the
two-sample textbook form. Both columns rebuild to 1 dB, so a change to `between_db` that would have read
that decision differently fails here.
"""

from __future__ import annotations

import numpy as np
import pytest

from tools.emulator import audiotone as at

R = 48000
SKIP, N = 4800, 8192  # past the note's start, then the measure's window, as on the unit

# note (the sub plays an octave lower): (naive dB, polyBLEP dB), as the page prints them
TABLE = {
    57: (-27, -39),
    60: (-31, -54),
    63: (-26, -42),
    66: (-26, -41),
    69: (-25, -41),
    72: (-24, -40),
    75: (-23, -39),
    78: (-22, -38),
    81: (-26, -63),
    84: (-21, -37),
    87: (-20, -35),
    90: (-19, -35),
    93: (-19, -35),
    96: (-17, -32),
}


def sub_hz(note: int) -> float:
    return 440.0 * 2 ** ((note - 12 - 69) / 12)


def naive_square(f0: float, n: int) -> np.ndarray:
    step = int(f0 / R * 2**32)
    phase = (np.arange(n, dtype=np.uint64) * step) & 0xFFFFFFFF
    return np.where(phase >> 31, -1.0, 1.0)


def blep_square(f0: float, n: int) -> np.ndarray:
    dt = f0 / R
    t = (np.arange(n) * dt) % 1.0

    def blep(t: np.ndarray) -> np.ndarray:
        out = np.zeros_like(t)
        a = t < dt
        x = t[a] / dt
        out[a] = x + x - x * x - 1
        b = t > 1 - dt
        x = (t[b] - 1) / dt
        out[b] = x * x + x + x + 1
        return out

    return np.where(t < 0.5, 1.0, -1.0) + blep(t) - blep((t + 0.5) % 1.0)


@pytest.mark.parametrize("note", sorted(TABLE))
def test_between_db_rebuilds_the_sub_polyblep_table(note: int) -> None:
    f0 = sub_hz(note)
    naive = at.between_db(naive_square(f0, SKIP + N)[SKIP:], R, f0)
    blep = at.between_db(blep_square(f0, SKIP + N)[SKIP:], R, f0)
    want_naive, want_blep = TABLE[note]
    assert abs(naive - want_naive) <= 1.0
    assert abs(blep - want_blep) <= 1.0


def test_between_db_of_a_sine_is_far_down() -> None:
    t = np.arange(N) / R
    assert at.between_db(np.sin(2 * np.pi * 440 * t), R, 440.0) < -80


def test_fundamental_reads_a_square_and_rejects_noise() -> None:
    assert at.fundamental(naive_square(220.0, 24000), R) == pytest.approx(
        220.0, rel=0.002
    )
    noise = np.random.default_rng(1).standard_normal(24000) * 0.3
    assert at.fundamental(noise, R) is None


def test_above_db_sees_a_tone_above_20k() -> None:
    t = np.arange(N) / R
    low = np.sin(2 * np.pi * 1000 * t)
    assert at.above_db(low, R) < -80
    assert at.above_db(low + 0.1 * np.sin(2 * np.pi * 21000 * t), R) > -25


def test_steps_tells_a_click_from_a_saws_resets() -> None:
    t = np.arange(R) / R
    saw = 0.5 * (2 * ((t * 100) % 1.0) - 1)
    s = at.steps(saw, R)
    assert s["periodic"] > 90 and s["isolated"] == 0
    sine = 0.3 * np.sin(2 * np.pi * 200 * t)
    sine[24000:] += 0.2  # one step
    c = at.steps(sine, R)
    assert c["isolated"] == 1 and c["times_s"] == [0.5]


def test_compare_says_identical_or_where() -> None:
    x = np.zeros((1000, 2))
    assert at.compare(x, x.copy(), R)["identical"]
    y = x.copy()
    y[480, 1] = 1 / 32768
    r = at.compare(x, y, R)
    assert (
        not r["identical"]
        and r["first_difference_s"] == 0.01
        and r["max_difference_lsb16"] == 1.0
    )


def test_note_report_flags_a_naive_square_and_not_a_sine() -> None:
    sq = 0.5 * naive_square(sub_hz(96), R)[:, None].repeat(2, axis=1)
    rep = at.note_report(sq, R)
    assert rep["fundamental_hz"] == pytest.approx(sub_hz(96), rel=0.002)
    assert any("between the harmonics" in f for f in rep["findings"])
    t = np.arange(R) / R
    sine = 0.5 * np.sin(2 * np.pi * 440 * t)[:, None].repeat(2, axis=1)
    assert at.note_report(sine, R)["findings"] == []
