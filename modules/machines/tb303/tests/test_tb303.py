"""tom/tb303 on the concrete SHARC+ core through core/dsp-voice, against the float32 model (tests/tb303_ref.py Tb303), and the
model against Open303's own voice (tb303_ref.Open303, a float64 port)."""

import numpy as np
import pytest
import tb303_ref
from support.machine_rig import MachineRig

from tools.patchkit import pk_toolchains

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)

IDS = ("tb303_wave", "tb303_cut", "tb303_res", "tb303_env", "tb303_dec", "tb303_acc")


def word(x: float) -> int:
    return round(x * 0x7F00)


class Rig(MachineRig):
    def __init__(self) -> None:
        super().__init__("tom/tb303", "dt2")

    def play(
        self, note: float, vel: int, params: list[int], tune: float = 64.0
    ) -> None:
        self.set_note(note, vel)
        self.set_params(
            {"tune": int(tune * 256), **dict(zip(IDS, params, strict=True))}
        )
        self.trigger()

    def run(self, n: int) -> tuple[np.ndarray, list[int]]:
        ys, counts = [], []
        for _ in range(n):
            b = self.block()
            assert b.returned, b.halt
            ys.append(np.array(b.samples, dtype=np.float32))
            counts.append(b.instructions)
        return np.concatenate(ys), counts


def pitch(note: float, tune: float = 64.0) -> int:
    return int(note * 256) + int(tune * 256) - 0x4000


def model_run(
    m: tb303_ref.Tb303,
    note: float,
    vel: int,
    params: list[int],
    n: int,
    tune: float = 64.0,
) -> np.ndarray:
    m.note_on(vel << 8, params)
    return np.concatenate([m.block(pitch(note, tune), params) for _ in range(n)])


CASES = {
    "saw": (36, 80, [0, word(0.3), word(0.7), word(0.6), word(0.4), 0x7F00]),
    "square-accent": (
        48,
        120,
        [0x100, word(0.5), word(0.9), word(1.0), word(0.0), word(0.5)],
    ),
    "closed-no-env": (45, 64, [0, 0, 0, 0, word(1.0), 0]),
    "open-tuned": (60, 101, [0, word(1.0), word(0.5), word(0.3), word(0.8), word(0.3)]),
    "high": (84, 127, [0x100, word(0.8), word(1.0), word(0.8), word(0.2), 0x7F00]),
}


@pytest.mark.parametrize("name", CASES)
def test_the_routine_matches_the_model(name):
    note, vel, params = CASES[name]
    tune = 70.0 if name == "open-tuned" else 64.0
    rig = Rig()
    rig.play(note, vel, params, tune)
    got, _ = rig.run(60)
    want = model_run(tb303_ref.Tb303(), note, vel, params, 60, tune)
    assert np.max(np.abs(want)) > 0.05
    np.testing.assert_allclose(got, want, atol=1e-5)


@pytest.mark.parametrize("track", [0, 5])
@pytest.mark.parametrize(
    "released", [False, True], ids=["held-slides", "released-jumps"]
)
def test_a_held_note_slides_into_the_next(released, track):
    """The second note starts while the first is held (the gate is up): its pitch glides from the first's;
    after a note-off it starts on its own pitch. The state carries over; only the released case restarts the envelopes."""
    rig, m = Rig(), tb303_ref.Tb303()
    rig.select(track)
    a = [0, word(0.4), word(0.6), word(0.8), word(0.3), 0x7F00]
    b = [0x100, word(0.4), word(0.6), word(0.8), word(0.3), 0x7F00]
    rig.play(41, 90, a)
    x, _ = rig.run(20)
    want = [model_run(m, 41, 90, a, 20)]
    if released:  # the gate goes down in this block and stays down
        rig.release()
        z, _ = rig.run(3)
        want.append(
            np.concatenate(
                [
                    m.block(pitch(41), a, True),
                    m.block(pitch(41), a),
                    m.block(pitch(41), a),
                ]
            )
        )
        x = np.concatenate([x, z])
    rig.play(53, 110, b)
    y, _ = rig.run(30)
    want.append(model_run(m, 53, 110, b, 30))
    np.testing.assert_allclose(np.concatenate([x, y]), np.concatenate(want), atol=1e-5)
    f0 = 440 * 2 ** ((53 - 69) / 12)
    if released:
        assert abs(m.freq - f0) < 0.01
    m2 = tb303_ref.Tb303()
    model_run(m2, 41, 90, a, 20)
    if released:
        m2.block(pitch(41), a, True)
        m2.block(pitch(41), a)
    m2.note_on(110 << 8, b)
    m2.block(pitch(53), b)
    if released:
        assert abs(m2.freq - f0) < 0.01
    else:
        assert m2.freq < 0.8 * f0  # 0.7 ms into a 12 ms glide from 87 Hz to 175 Hz


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


def test_velocity_above_100_is_an_accent_of_amount_acc():
    p = [0, word(0.4), word(0.7), word(0.5), word(0.5), 0x7F00]
    plain = model_run(tb303_ref.Tb303(), 45, 100, p, 60)
    acc = model_run(tb303_ref.Tb303(), 45, 101, p, 60)
    assert np.max(np.abs(acc)) > 1.5 * np.max(np.abs(plain))
    half = model_run(tb303_ref.Tb303(), 45, 101, [*p[:5], word(0.5)], 60)
    assert np.max(np.abs(plain)) < np.max(np.abs(half)) < np.max(np.abs(acc))


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


def test_tb303_tracks_unpack_a_level_of_100():
    """DEC sits on knob 7 (LEV), so TB303's row declares that knob's range and core/dsp-voice's _dv_unpack writes 100 << 8 over knob 7's
    halfword on a TB303 track only, keeps the word's other half and the unpack's arguments, and continues in the unpack."""
    import support.dsp_voice_rig as dv

    rig = Rig()
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


def test_cost_per_block():
    rig = Rig()
    rig.play(60, 120, [0, word(0.5), word(0.5), word(0.5), word(0.5), 0x7F00])
    _, counts = rig.run(4)
    assert max(counts) < 7500 + 900, (
        counts
    )  # the routine's declared budget plus the adapter and guard
