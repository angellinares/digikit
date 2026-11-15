"""digikit/shot on the concrete SHARC+ core through core/dsp-voice (the real adapter, the Python rig), against a float32 model of the stereo PCM16 reader:
the generator of the reference tables of `tests/rig/shot.toml` (`uv run python -m tools.devtools.rig_refs modules/machines/shot`, see rig_refs_gen.py). It asserts that
the routine equals the model before it writes a table. The checks themselves run on the SHARC-only rig."""

import numpy as np
from support.machine_rig import MachineRig, nw
from support.sample_pool import PLANE_L, PLANE_R, pack, pcm, publish

MODULE = "digikit/shot"


FRAMES = 200


RATE_HZ = 48000


TOKEN = 7


REV, REV_L, FWD_L, FWD = (mode << 8 for mode in range(4))  # the PLAY word: mode * 256


STOCK_GAIN = np.uint32(0x3ECF1AA0).view(np.float32)  # DK_SAMPLE_STOCK_GAIN, 0.4045


STOCK_STEP = np.uint32(0x3C55C8E7).view(np.float32)  # G / 31, the end ramp step


LEFT, RIGHT = pcm(1), pcm(2)


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
        self.rec = nw(d["_shot_sample_rec"]) + 8 * index
        self.ctl = nw(d["_shot_sample_ctl"]) + 4 * index
        self.state = nw(self.rig.built.state)
        poke = self.rig.sess.poke
        for k, w in enumerate(pack(self.left)):
            poke(PLANE_L + k, w)
        for k, w in enumerate(pack(self.right)):
            poke(PLANE_R + k, w)
        self.publish()

    def publish(self, token=TOKEN, rec=None, plane_l=PLANE_L, plane_r=PLANE_R):
        """Publish a record: this entry's by default, or `rec` (a record address) with its own planes."""
        publish(
            self.rig.sess.poke,
            self.rec if rec is None else rec,
            0,
            self.frames,
            self.src_rate,
            self.channels,
            token,
            plane_l,
            plane_r,
        )

    def count(self):
        return self.rig.sess.peek(self.ctl + 1) or 0

    def cursor(self):
        return [self.rig.sess.peek(self.state + k) or 0 for k in range(6)]

    def start(self, note=60, params=None, vel=100):
        """Set the parameters and trigger a note; `step` renders the blocks."""
        r = self.rig
        r.set_params(
            {
                "sample": self.index << 8,
                "start": 0,
                "end": 0x7F00,
                "play": FWD,
                "pitch": 0x4000,
                **(params or {}),
            }
        )
        r.set_note(note, vel)
        r.trigger()
        if (
            r.device == "dn2"
        ):  # the trig edge block is silent; note_on runs in the next one
            r.block("friendly")

    def step(self, blocks, params=None):
        """Change parameters (if any), then render `blocks` blocks of the running note."""
        r = self.rig
        if params:
            r.set_params(params)
        left, right = [], []
        for _ in range(blocks):
            b = r.block("friendly")
            assert b.returned and b.faults == 0 and b.context_bad == []
            assert self.count() == 0
            left += b.samples
            if r.device == "dt2":
                right += r.odd_block()
        return np.array(left), np.array(right)

    def play(self, note=60, blocks=8, params=None, vel=100):
        self.start(note, params, vel)
        return self.step(blocks)


def model(plane, start, end, rate, reverse, blocks, schedule=None, loop=False):
    """The reader's output for one plane: split cursor, linear interpolation in float32, a zero tail after the end.
    Every block is multiplied by the stock gain G; the block in which the cursor passes the end by the running float32 gain G - k * G / 31.
    `schedule` maps a block index to the (reverse, start, end) or (reverse, start, end, loop) in frames that hold from that block on; the
    cursor continues. The range is [0, end) forward and [start, end) in reverse and when looping. A cursor outside it renders the block over
    the whole sample and ends the voice; when looping it moves into the region (START forward, END - 1 in reverse) instead.
    A looping cursor wraps by the region length (once; still outside: the voice ends); a forward cursor at END - 1 interpolates toward START."""
    out = np.zeros(32 * blocks, dtype=np.float32)
    mag = abs(rate)
    pos = (end - 1 if reverse else start) * 65536
    done = start >= end
    ending = 0 if done else None
    for blk in range(blocks):
        if schedule and blk in schedule:
            reverse, start, end, *rest = schedule[blk]
            loop = bool(rest and rest[0])
        if done:
            continue
        step = -mag if reverse else mag
        lo, hi = (start, end) if reverse or loop else (0, end)
        forced = not lo <= pos // 65536 < hi
        if forced and loop and lo < hi:
            pos = (hi - 1 if reverse else lo) * 65536 + pos % 65536
            forced = False
        elif forced:
            lo, hi = 0, len(plane)
        for n in range(blk * 32, blk * 32 + 32):
            index, frac = divmod(pos, 65536)
            if not lo <= index < hi:
                done = True
                break
            a = np.float32(plane[index] / 32768)
            wraps = loop and not forced and not reverse
            nxt = lo if wraps and index + 1 >= hi else min(index + 1, hi - 1)
            b = np.float32(plane[nxt] / 32768)
            out[n] = np.float32(
                a + np.float32(np.float32(frac / 65536) * np.float32(b - a))
            )
            pos += step
            if not lo * 65536 <= pos < hi * 65536:
                if loop and hi - lo >= 2 and not forced:
                    pos += (
                        -(hi - lo) * 65536 if pos >= hi * 65536 else (hi - lo) * 65536
                    )
                if not lo * 65536 <= pos < hi * 65536:
                    done = True
        if forced:
            done = True
        if done:
            ending = blk
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
        "start": 0,
        "end": 0x7F00,
        "play": FWD,
        "pitch": 0x4000,
        **params,
    }
    got_l, got_r = s.play(note, blocks, params)
    reverse = p["play"] >> 8 <= 1
    loop = p["play"] >> 8 in (1, 2)
    start = frames_of(p["start"], s.frames)
    end = frames_of(p["end"], s.frames)
    cur = s.cursor()
    rate = cur[4] - (1 << 32) if cur[4] >= 1 << 31 else cur[4]
    want = expected_rate(note, p["pitch"], s.src_rate, reverse)
    assert abs(rate - want) <= max(2, abs(want) >> 17), (
        rate,
        want,
    )  # the device's exp2 is a polynomial
    mag = abs(rate)
    step = -mag if reverse else mag
    wl = model(s.left, start, end, step, reverse, blocks, loop=loop)
    wr = model(s.right, start, end, step, reverse, blocks, loop=loop)
    return got_l, got_r, wl, wr, (start, end, rate)


def word_of(frame):
    """A parameter word that maps to `frame` of the 200-frame sample."""
    w = int(np.ceil(frame * 32512 / FRAMES))
    assert frames_of(w, FRAMES) == frame
    return w


def sine_plane(frames, period):
    return np.rint(16000 * np.sin(2 * np.pi * np.arange(frames) / period)).astype(
        np.int16
    )
