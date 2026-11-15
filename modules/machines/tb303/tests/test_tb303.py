"""tom/tb303: the assertions that no SHARC-rig check (tests/rig/tb303.toml) replaces yet. Kept until T1g phase 5 ports it
(docs/plans/ledgers/t1g-retirement.md "Kept until phase 5"). The routine against the model, the slide, cost and conformance run on the kit."""

import numpy as np
import pytest
import tb303_ref
from support.machine_rig import MachineRig
from tb303_cases import model_run, pitch, word

from tools.patchkit import pk_toolchains

NO_SELACHE = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)


# kept until T1g phase 5 ports it (model only)
@pytest.mark.parametrize("released", [False, True], ids=["tie", "retrigger"])
def test_a_tie_restarts_neither_envelope(released):
    p = [0, word(0.4), word(0.6), word(0.8), word(0.3), 0x7F00]
    m = tb303_ref.Tb303()
    model_run(m, 41, 90, p, 40)
    if released:
        m.block(pitch(41), p, True)
    e, a = m.e, m.aenv
    m.note_on(90 << 8, p)
    m.block(pitch(48), p)
    if released:
        assert m.aenv == tb303_ref.K["ONE"] or m.aenv > 0.99
        assert m.e > e
    else:
        assert 0.9 * a < m.aenv < a
        assert m.e < e


# kept until T1g phase 5 ports it (model only)
def test_a_release_ends_the_note():
    """1 ms release (the declicker sets the pace), 50 ms after an accent; blocks of 32 samples."""
    p = [0, word(0.6), word(0.5), word(0.5), word(0.5), 0x7F00]
    m = tb303_ref.Tb303()
    model_run(m, 45, 90, p, 40)
    m.block(pitch(45), p, True)
    tail = [np.max(np.abs(m.block(pitch(45), p))) for _ in range(20)]
    assert tail[0] > 1e-2 and max(tail[10:]) < 1e-3
    m = tb303_ref.Tb303()
    model_run(m, 45, 120, p, 40)
    a = m.aenv
    for _ in range(300):
        m.block(pitch(45), p, m.rel == 0)
    assert abs(m.aenv / a - np.exp(-300 * 32 / 48000 / 0.05)) < 1e-3


# kept until T1g phase 5 ports it (model only)
def test_velocity_above_100_is_an_accent_of_amount_acc():
    p = [0, word(0.4), word(0.7), word(0.5), word(0.5), 0x7F00]
    plain = model_run(tb303_ref.Tb303(), 45, 100, p, 60)
    acc = model_run(tb303_ref.Tb303(), 45, 101, p, 60)
    assert np.max(np.abs(acc)) > 1.5 * np.max(np.abs(plain))
    half = model_run(tb303_ref.Tb303(), 45, 101, [*p[:5], word(0.5)], 60)
    assert np.max(np.abs(plain)) < np.max(np.abs(half)) < np.max(np.abs(acc))


# kept until T1g phase 5 ports it (model only)
@pytest.mark.parametrize("vel", [80, 120])
def test_knobs_flipping_every_block_stay_bounded(vel):
    """Cutoff, env mod and resonance between their extremes every block: Open303's filter blows up under this; the glides keep it bounded."""
    lo = [0, 0, 0x7F00, 0, 0x7F00, 0]
    hi = [0x100, 0x7F00, 0, 0x7F00, 0, 0x7F00]
    m = tb303_ref.Tb303()
    m.note_on(vel << 8, hi)
    peak = max(
        float(np.max(np.abs(m.block(pitch(60), hi if k % 2 else lo))))
        for k in range(300)
    )
    assert peak < 3.0


def lsd(a: np.ndarray, b: np.ndarray, n: int = 4096) -> float:
    """Log-spectral distance in dB over 30 Hz .. 16 kHz, weighted by the reference's energy, averaged over frames."""
    win, f = np.hanning(n), np.fft.rfftfreq(n, 1 / 48000)
    m = (f > 30) & (f < 16000)
    out = []
    for s in range(2400, len(a) - n, n // 2):
        pa = np.abs(np.fft.rfft(a[s : s + n] * win))[m] + 1e-9
        pb = np.abs(np.fft.rfft(b[s : s + n] * win))[m] + 1e-9
        w = pa**2 / np.sum(pa**2)
        out.append(np.sqrt(np.sum(w * (20 * np.log10(pa / pb)) ** 2)))
    return float(np.mean(out))


# kept until T1g phase 5 ports it (model against Open303)
@pytest.mark.parametrize(
    "wave,cut,res,env,dec,acc,note,vel",
    [
        (0, 0.3, 0.7, 0.6, 0.4, 1.0, 45, 80),
        (0, 0.3, 0.7, 0.6, 0.4, 1.0, 45, 120),
        (0, 0.3, 0.7, 0.6, 0.4, 0.5, 45, 120),
        (0, 0.0, 1.0, 0.3, 0.9, 1.0, 33, 80),
    ],
    ids=["saw", "accent", "half-accent", "low-full-res"],
)
def test_the_model_sounds_like_open303(wave, cut, res, env, dec, acc, note, vel):
    """Open303 itself (4x oversampled, its elliptic decimator) against the machine's 1x model: the spectra agree within 0.5 dB."""
    ref = tb303_ref.Open303(
        wave,
        314 * (2394 / 314) ** cut,
        100 * res,
        100 * env,
        200 * 10**dec,
        100 * acc,
        n=4,
        ellip=True,
    )
    want = ref.render([(note, vel if vel <= 100 else 100, 9600)])
    m = tb303_ref.Tb303()
    params = [
        0x100 if wave else 0,
        word(cut),
        word(res),
        word(env),
        word(dec),
        word(acc),
    ]
    got = model_run(m, note, vel, params, 300)
    assert lsd(want, got[:9600] / tb303_ref.K["GAIN"]) < 0.5


# kept until T1g phase 5 ports it (the adapter test)
@NO_SELACHE
def test_tb303_tracks_unpack_a_level_of_100():
    """DEC sits on knob 7 (LEV), so TB303's row declares that knob's range and core/dsp-voice's _dv_unpack writes 100 << 8 over knob 7's
    halfword on a TB303 track only, keeps the word's other half and the unpack's arguments, and continues in the unpack."""
    import support.dsp_voice_rig as dv

    rig = MachineRig("tom/tb303", "dt2")
    s = rig.sess
    tb = rig.spec.machine_type
    word = lambda t: (dv.FRAME + 0xEC + 0x60 * t) // 4  # noqa: E731
    s.poke(rig.built.data["_dv_mask"] // 4, 1 << min(tb, 31))
    s.poke(
        (dv.TYPE_CACHE + 4) // 4, tb << 16 | 3
    )  # tracks 2 and 3: SAMPLE-like 3, TB303
    s.poke((dv.TYPE_CACHE + 8) // 4, 3 << 16 | tb)  # tracks 4 and 5: TB303, 3
    for t in (2, 3, 4, 5):
        s.poke(word(t), 0xABCD_1234)
    for t in (2, 3, 4, 5):
        res = s.call(
            "_dv_unpack", {"R8": 2 * t, "R12": t, "L4": 0}, read_regs=("R8", "R12")
        )
        assert res["halt"]["pc_sw"] == dv.UNPACK_BP
        assert res["regs"]["R8"] == 2 * t and res["regs"]["R12"] == t
    assert [s.peek(word(t)) for t in (2, 3, 4, 5)] == [
        0xABCD_1234,
        0xABCD_6400,
        0xABCD_6400,
        0xABCD_1234,
    ]
