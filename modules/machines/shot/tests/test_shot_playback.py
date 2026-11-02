"""digikit/shot on the concrete SHARC+ core through core/dsp-voice (the real adapter), against a float32 model of the stereo PCM16 reader.

The pool table (`_shot_rec` records and `_shot_ctl` control sets, one of each for every pool index) is written the way the runtime does: PCM
planes in free L2, the record published with `ready` last. The SMP parameter (word 0, index << 8) picks the record of a note. Checks: SMP
picks the record (two values, two records; an empty or out-of-range entry is silent); start, end, reverse and pitch; L and R differ for a
stereo sample; a mono sample plays centred; the voice ends after the sample; a stale token or a closed entry is silent with no PCM read;
every block leaves the count at 0.
Skips when Selache is not built."""

import numpy as np
import pytest
from support.machine_rig import MachineRig, nw

from tools.patchkit import pk_toolchains

pytestmark = pytest.mark.skipif(
    pk_toolchains.selache_dir() is None,
    reason="selache not built (tools/patchkit/pk_toolchains.py selache)",
)

MODULE = "digikit/shot"
PLANE_L = nw(0x200F0000)  # normal-word addresses of the test PCM planes (free L2)
PLANE_R = nw(0x200F8000)
FRAMES = 200
RATE_HZ = 48000
TOKEN = 7
DK_STEREO = 2


STOCK_GAIN = np.uint32(0x3ECF1AA0).view(np.float32)  # DK_SAMPLE_STOCK_GAIN, 0.4045
STOCK_STEP = np.uint32(0x3C55C8E7).view(np.float32)  # G / 31, the end ramp step


def pcm(seed: int, n: int = FRAMES) -> np.ndarray:
    rng = np.random.default_rng(seed)
    return rng.integers(-32768, 32768, n).astype(np.int16)


LEFT, RIGHT = pcm(1), pcm(2)


def pack(plane: np.ndarray) -> list[int]:
    p = np.concatenate([plane, np.zeros(len(plane) % 2, dtype=np.int16)])
    pairs = p.reshape(-1, 2).astype(np.int64) & 0xFFFF
    return [int(lo | hi << 16) for lo, hi in pairs]


class Shot:
    def __init__(self, device="dt2", index=0, channels=2, frames=FRAMES, rate=RATE_HZ):
        self.rig = MachineRig(MODULE, device)
        self.index = index
        self.channels = channels
        self.frames = frames
        self.src_rate = rate
        self.left = LEFT[:frames]
        self.right = self.left if channels == 1 else RIGHT[:frames]
        d = self.rig.built.data
        self.rec = nw(d["_shot_rec"]) + 8 * index
        self.ctl = nw(d["_shot_ctl"]) + 4 * index
        self.state = nw(self.rig.built.state)
        poke = self.rig.sess.poke
        for k, w in enumerate(pack(self.left)):
            poke(PLANE_L + k, w)
        for k, w in enumerate(pack(self.right)):
            poke(PLANE_R + k, w)
        self.publish()

    def publish(self, token=TOKEN, rec=None, plane_l=PLANE_L, plane_r=PLANE_R):
        """Publish a record: this entry's by default, or `rec` (a record address) with its own planes."""
        poke = self.rig.sess.poke
        rec = self.rec if rec is None else rec
        base_r = plane_l if self.channels == 1 else plane_r
        for k, w in enumerate(
            [0, token, self.frames, self.src_rate, self.channels, 1, plane_l, base_r]
        ):
            poke(rec + k, w)
        poke(rec, 1)  # ready last

    def count(self):
        return self.rig.sess.peek(self.ctl + 1) or 0

    def cursor(self):
        return [self.rig.sess.peek(self.state + k) or 0 for k in range(6)]

    def play(self, note=60, blocks=8, params=None, vel=100):
        r = self.rig
        r.set_params(
            {
                "shot_smp": self.index << 8,
                "shot_start": 0,
                "shot_end": 0x7F00,
                "shot_reverse": 0,
                "shot_pitch": 0x4000,
                **(params or {}),
            }
        )
        r.set_note(note, vel)
        r.trigger()
        left, right = [], []
        if (
            r.device == "dn2"
        ):  # the trig edge block is silent; note_on runs in the next one
            r.block("friendly")
        for _ in range(blocks):
            b = r.block("friendly")
            assert b.returned and b.faults == 0 and b.context_bad == []
            assert self.count() == 0
            left += b.samples
            if r.device == "dt2":
                right += r.odd_block()
        return np.array(left), np.array(right)


def model(plane, start, end, rate, reverse, blocks):
    """The reader's output for one plane: split cursor, linear interpolation in float32, a zero tail after the end.
    Every block is multiplied by the stock gain G; the block in which the cursor passes the end by the running float32 gain G - k * G / 31."""
    out = np.zeros(32 * blocks, dtype=np.float32)
    pos = (end - 1 if reverse else start) * 65536
    done = start >= end
    done_at = 0 if done else None
    for n in range(len(out)):
        index, frac = divmod(pos, 65536)
        if done or not start <= index < end:
            done = True
            if done_at is None:
                done_at = n
            break
        a = np.float32(plane[index] / 32768)
        b = np.float32(plane[min(index + 1, end - 1)] / 32768)
        out[n] = np.float32(
            a + np.float32(np.float32(frac / 65536) * np.float32(b - a))
        )
        pos += rate
        if not start * 65536 <= pos < end * 65536:
            done = True
            done_at = n
    ending = None if done_at is None else done_at // 32
    for b in range(blocks):
        gain, step = STOCK_GAIN, STOCK_STEP if b == ending else np.float32(0)
        for k in range(32):
            out[b * 32 + k] = np.float32(out[b * 32 + k] * gain)
            gain = np.float32(gain - step)
    return out


def frames_of(word, frames):
    f = np.float32(np.float32(word) * np.float32(frames)) * np.float32(1 / 32512)
    return min(int(np.rint(f)), frames)


def expected_rate(note, pitch_word, src_rate=RATE_HZ, reverse=False):
    x = ((note - 60) + (pitch_word - 0x4000) * 3 / 2048) / 12
    r = round(src_rate / 48000 * 2**x * 65536)
    r = min(max(r, 1), 1 << 22)
    return -r if reverse else r


def check(s, note=60, blocks=8, **params):
    p = {
        "shot_start": 0,
        "shot_end": 0x7F00,
        "shot_reverse": 0,
        "shot_pitch": 0x4000,
        **params,
    }
    got_l, got_r = s.play(note, blocks, params)
    reverse = p["shot_reverse"] >= 0x4000
    start = frames_of(p["shot_start"], s.frames)
    end = frames_of(p["shot_end"], s.frames)
    cur = s.cursor()
    rate = cur[4] - (1 << 32) if cur[4] >= 1 << 31 else cur[4]
    want = expected_rate(note, p["shot_pitch"], s.src_rate, reverse)
    assert abs(rate - want) <= max(2, abs(want) >> 17), (
        rate,
        want,
    )  # the device's exp2 is a polynomial
    mag = abs(rate)
    wl = model(s.left, start, end, -mag if reverse else mag, reverse, blocks)
    wr = model(s.right, start, end, -mag if reverse else mag, reverse, blocks)
    return got_l, got_r, wl, wr, (start, end, rate)


DEVICES = ["dt2", "dn2"]


@pytest.mark.parametrize("device", DEVICES)
def test_default_params_play_the_whole_sample_with_distinct_channels(device):
    s = Shot(device)
    gl, gr, wl, wr, (start, end, rate) = check(s)
    assert (start, end) == (0, FRAMES) and rate == 65536
    assert np.array_equal(gl, wl)
    assert np.max(np.abs(wl)) > 0.04 and not np.array_equal(wl, wr)
    if device == "dt2":
        assert np.array_equal(gr, wr)  # R reaches the odd voice's buffer
    # DN2: the adapter drops R for now (TODO: the shadow right-channel stages), only L is checked


@pytest.mark.parametrize(
    "params",
    [
        {"shot_start": 40 * 256, "shot_end": 100 * 256},
        {"shot_start": 0, "shot_end": 50 * 256},
        {"shot_start": 0, "shot_end": 90 * 256, "shot_reverse": 0x7F00},
        {"shot_start": 20 * 256, "shot_end": 127 * 256, "shot_reverse": 0x4000},
    ],
    ids=["window", "end-only", "reverse", "reverse-start"],
)
def test_start_end_and_reverse_select_the_frames(params):
    s = Shot("dt2")
    gl, gr, wl, wr, (start, end, rate) = check(s, **params)
    assert 0 <= start < end <= FRAMES
    assert np.array_equal(gl, wl) and np.array_equal(gr, wr)
    played = int(np.count_nonzero(wl)) or 1
    assert 0 < played <= end - start + 1


@pytest.mark.parametrize(
    "note,pitch",
    [(72, 0x4000), (48, 0x4000), (60, 96 * 256), (60, 32 * 256), (67, 70 * 256)],
    ids=["note+12", "note-12", "pitch+12", "pitch-12", "note+7,pitch+2.25"],
)
def test_pitch_is_the_note_plus_the_pitch_parameter(note, pitch):
    s = Shot("dt2")
    gl, gr, wl, wr, (_, _, rate) = check(s, note=note, shot_pitch=pitch)
    assert np.array_equal(gl, wl) and np.array_equal(gr, wr)
    semis = (note - 60) + (pitch - 0x4000) * 3 / 2048
    assert abs(rate / 65536 / 2 ** (semis / 12) - 1) < 3e-5


def test_a_source_rate_other_than_48k_scales_the_cursor():
    s = Shot("dt2", rate=24000)
    gl, gr, wl, wr, (_, _, rate) = check(s, blocks=12)
    assert abs(rate - 32768) <= 2
    assert np.array_equal(gl, wl) and np.array_equal(gr, wr)


def test_a_mono_sample_plays_centred():
    s = Shot("dt2", channels=1)
    gl, gr, wl, wr, _ = check(s)
    assert np.array_equal(gl, wl) and np.array_equal(gl, gr)


def test_another_pool_entry_reads_its_own_record():
    s = Shot("dt2", index=3)
    gl, gr, wl, wr, _ = check(s)
    assert np.array_equal(gl, wl) and np.array_equal(gr, wr)


@pytest.mark.parametrize("device", DEVICES)
def test_two_smp_values_pick_two_records(device):
    """Two entries hold different PCM; the SMP word of the note, not the track, chooses. Entry 5 is published after entry 2 and both stay ready."""
    s = Shot(device, index=2)
    other_l, other_r = nw(0x200F4000), nw(0x200FC000)
    poke = s.rig.sess.poke
    third = pcm(3)
    for k, w in enumerate(pack(third)):
        poke(other_l + k, w)
    for k, w in enumerate(pack(pcm(4))):
        poke(other_r + k, w)
    d = s.rig.built.data
    rec5 = nw(d["_shot_rec"]) + 8 * 5
    s.publish(token=TOKEN + 1, rec=rec5, plane_l=other_l, plane_r=other_r)
    first, _ = s.play(blocks=8)  # SMP 2
    assert np.array_equal(first, model(s.left, 0, FRAMES, 65536, False, 8))
    s.index = 5
    second, _ = s.play(blocks=8)  # SMP 5
    assert np.array_equal(second, model(third, 0, FRAMES, 65536, False, 8))
    assert not np.array_equal(first, second)


@pytest.mark.parametrize(
    "smp",
    [7, 15, 16, 127],
    ids=["empty-7", "empty-15", "above-pool-16", "above-pool-127"],
)
def test_an_empty_or_out_of_range_entry_is_silent(smp):
    """The entry has no record (never published, or past the pool): note_on does not latch, render ends the voice with no PCM read."""
    s = Shot("dt2", index=0)
    s.index = smp
    left, right = s.play(blocks=2)
    assert not np.any(left) and not np.any(right)
    assert s.count() == 0


def test_the_voice_ends_after_the_sample():
    s = Shot("dt2")
    r = s.rig
    s.play(blocks=1)
    for _ in range(10):  # 200 frames = 7 blocks
        r.block("friendly")
    assert r.read_rec_word(r.voice, 0x1B8) & 0xFF == 0, "DK_DONE ends the voice"
    b = r.block("friendly")
    assert all(x == 0.0 for x in b.samples)


@pytest.mark.parametrize("device", DEVICES)
def test_a_stale_token_is_silent_and_ends_the_voice(device):
    s = Shot(device)
    r = s.rig
    left, _ = s.play(blocks=2)
    assert np.any(left)  # playing
    s.rig.sess.poke(
        s.rec + 1, TOKEN + 1
    )  # a select: the old token is no longer current
    b = r.block("friendly")
    assert all(x == 0.0 for x in b.samples)
    if device == "dt2":
        assert r.read_rec_word(r.voice, 0x1B8) & 0xFF == 0
        assert all(x == 0.0 for x in r.odd_block())
    assert s.count() == 0


@pytest.mark.parametrize("what", ["not-ready", "closed", "bad-fmt", "empty"])
def test_an_entry_that_is_not_playable_is_silent_with_no_pcm_read(what):
    s = Shot("dt2")
    r = s.rig
    poke = r.sess.poke
    if what == "not-ready":
        poke(s.rec, 0)
    elif what == "closed":
        poke(s.ctl, 1)
    elif what == "bad-fmt":
        poke(s.rec + 5, 2)
    else:
        poke(s.rec + 2, 0)
    poke(s.rec + 6, 0xFFFFFFF0)  # a bad plane address: reading it would fault
    poke(s.rec + 7, 0xFFFFFFF0)
    left, right = s.play(blocks=2)
    assert not np.any(left) and not np.any(right)
    assert s.count() == 0


@pytest.mark.parametrize("device", DEVICES)
def test_instruction_count_is_inside_the_declared_ceiling(device):
    s = Shot(device)
    s.rig.set_note(60, 100)
    s.rig.trigger()
    first = s.rig.block("friendly")
    steady = s.rig.block("friendly")
    desc = s.rig.built.desc
    assert first.instructions < desc[7] + 1500, (
        first.instructions
    )  # note_on + render + the adapter
    assert steady.instructions < desc[6] + 1500, steady.instructions


@pytest.mark.parametrize("device", DEVICES)
def test_the_module_stores_only_inside_its_own_memory(device):
    s = Shot(device)
    s.rig.record_stores = True
    s.rig.sess.record_stores()
    s.rig.set_note(60, 100)
    s.rig.trigger()
    for _ in range(3):
        b = s.rig.block("friendly")
        assert b.returned and b.foreign == [], b.foreign


def sine_plane(frames, period):
    return np.rint(16000 * np.sin(2 * np.pi * np.arange(frames) / period)).astype(
        np.int16
    )


@pytest.mark.parametrize(
    "note", [60, 72, 48], ids=["unison", "octave-up", "octave-down"]
)
def test_a_pure_tone_keeps_its_level_at_any_pitch(note):
    """The pitch changes only the speed: a sine of 16000/32768 keeps its amplitude, scaled by the stock gain 0.4045, one octave up or down."""
    frames = 1600
    s = Shot("dt2", frames=frames)
    plane = sine_plane(
        frames, 160
    )  # 300 Hz at 48 kHz, well below the Nyquist limit of the 2x read
    poke = s.rig.sess.poke
    for k, w in enumerate(pack(plane)):
        poke(PLANE_L + k, w)
        poke(PLANE_R + k, w)
    got_l, _ = s.play(note=note, blocks=20)
    n = np.arange(64, 576)  # inside the sample at every pitch
    w = 2 * np.pi * 2 ** ((note - 60) / 12) / 160
    basis = np.stack([np.sin(w * n), np.cos(w * n)], 1)
    c = np.linalg.lstsq(basis, got_l[64:576], rcond=None)[0]
    amp = np.hypot(*c)
    assert abs(amp / (16000 / 32768 * float(STOCK_GAIN)) - 1) < 0.02, amp
