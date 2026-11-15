"""The float32 model of the frequency shifter (freqshift_ref.py): the SHIFT mapping, the Hilbert pair, the oscillator and the shifter on tones.

No assembler needed: this is the specification that the rig check tests/rig/freqshift.toml holds the DSP routine to."""

import freqshift_ref as R
import numpy as np
import pytest

FS = R.FS


def amplitude(y: np.ndarray, f: float, skip: int) -> float:
    """The amplitude of the component of y at exactly f Hz (a Hann-windowed single-bin projection; a neighbour 100 Hz away is 90 dB down)."""
    y = y[skip:]
    n = np.arange(len(y))
    w = np.hanning(len(y))
    return float(2 * abs(np.sum(w * y * np.exp(-2j * np.pi * f * n / FS))) / np.sum(w))


def tone_through(
    f_in: float, v: float, blocks: int, amp: float = 0.5
) -> tuple[np.ndarray, int]:
    sh = R.Shifter()
    n = np.arange(blocks * R.BLOCK)
    x = (amp * np.sin(2 * np.pi * f_in * n / FS)).astype(np.float32)
    out = [
        sh.block(np.concatenate([x[k * 32 : k * 32 + 32]] * 2), v)[:32]
        for k in range(blocks)
    ]
    return np.concatenate(out).astype(np.float64), R.target_inc(v)


def test_the_mapping_is_zero_at_zero_symmetric_and_monotonic():
    assert R.target_inc(0.0) == 0
    vs = np.linspace(0, 64, 513)
    incs = [R.target_inc(float(v)) for v in vs]
    assert all(b > a for a, b in zip(incs, incs[1:], strict=False))
    for v in (0.25, 1.0, 7.5, 33.0, 63.0, 64.0):
        assert R.target_inc(-v) == -R.target_inc(v)
    # past the ends the word is clamped to +-64
    assert R.target_inc(1000.0) == R.target_inc(64.0)


def test_the_mapping_reaches_slow_sweeps_and_about_two_kilohertz():
    assert 0.05 < R.hz(1.0) <= 0.1  # one step is a slow sweep
    assert 1900 < R.hz(64.0) < 2100 and 1700 < R.hz(63.0) < 2000
    # the routine's float32 arithmetic gives the float64 mapping to within 2 mHz (0.7 ppm of the end)
    err = max(
        abs(R.target_inc(float(v)) / R.INC_PER_HZ - R.hz(float(v)))
        for v in np.linspace(-64, 63.5, 600)
    )
    assert err < 0.002, err
    for f in (0.1, 1.0, 100.0, 1000.0):
        assert abs(R.hz(R.v_for_hz(f)) - f) < 1e-9 * max(1.0, f)


def test_the_hilbert_pair_has_image_rejection_over_the_band():
    f = np.geomspace(30, 18000, 3000)
    r = R.image_rejection(FS, f, R.COEF_P, R.COEF_Q)
    assert r.min() > 45.0, (r.min(), f[r.argmin()])
    # the whole range up to 23.5 kHz is as good; below 30 Hz it falls off towards DC
    f = np.geomspace(30, 23500, 3000)
    assert R.image_rejection(FS, f, R.COEF_P, R.COEF_Q).min() > 45.0
    assert R.image_rejection(FS, np.array([5.0]), R.COEF_P, R.COEF_Q)[0] > 3.0


def test_the_sine_polynomial_and_the_fold_are_accurate_over_the_turn():
    p = np.linspace(-(2**31), 2**31 - 1, 20001).astype(np.int64)
    got = np.array([float(R.sinq(int(x))) for x in p])
    want = np.sin(2 * np.pi * p / 2**32)
    assert np.abs(got - want).max() < 1.5e-6


@pytest.mark.parametrize("shift", [100.0, -100.0, 2000.0, 50.0])
def test_a_tone_moves_by_the_shift_and_the_image_is_40_db_down(shift):
    v = R.v_for_hz(abs(shift)) * (1 if shift > 0 else -1)
    f_in = 1000.0
    y, inc = tone_through(f_in, v, 260)
    exact = inc * FS / 2**32
    skip = 60 * R.BLOCK
    wanted = amplitude(y, f_in + exact, skip)
    image = amplitude(y, abs(f_in - exact), skip)
    assert abs(wanted - 0.5) < 0.005, wanted
    assert wanted / image > 100.0, (20 * np.log10(wanted / image), wanted, image)
    # nothing at the input frequency but the leakage of the image (the shifter does not pass the input through)
    if abs(shift) > 20:
        assert wanted / amplitude(y, f_in, skip) > 100.0


@pytest.mark.parametrize("f_in", [30.0, 100.0, 1000.0, 5000.0, 12000.0, 18000.0])
def test_shift_zero_is_flat(f_in):
    y, _ = tone_through(f_in, 0.0, 450)
    a = amplitude(y, f_in, 330 * R.BLOCK)
    assert abs(a / 0.5 - 1.0) < 2e-3, a


def test_the_phase_accumulator_never_drifts():
    """The oscillator is an integer: after N blocks at one increment the phase is the closed form, however long the run."""
    sh = R.Shifter()
    inc = R.target_inc(33.3)
    x = np.zeros(64, dtype=np.float32)
    for _ in range(3):
        sh.block(x, 33.3)
    for n in (3, 4):
        # block 0 ramps from 0: phase = 528 d + 32 inc (n - 1)
        d = inc >> 5
        want = R.wrap32(528 * d + (n - 1) * 32 * inc)
        assert sh.phase == want, (n, sh.phase, want)
        sh.block(x, 33.3)
    # a million blocks in closed form never leave the 32-bit turn
    assert R.wrap32(528 * (inc >> 5) + (10**6 - 1) * 32 * inc) in range(-(2**31), 2**31)


def test_the_output_is_clamped_below_the_level_bound():
    sh = R.Shifter()
    rng = np.random.default_rng(1)
    worst = 0.0
    for _ in range(80):
        x = rng.uniform(-8, 8, 64).astype(np.float32)
        worst = max(worst, float(np.abs(sh.block(x, 25.0)).max()))
    assert worst <= 7.9 + 1e-6
