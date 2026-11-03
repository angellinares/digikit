"""Float32 model of tom/tb303 (modules/machines/tb303/src/dsp/tb303.s), operation for operation, and `Open303`, a float64
port of rosic::Open303's voice (oscillator, pre-filter high-pass, TeeBeeFilter TB_303, filter envelope, accent, amp gain,
output filters) at any oversampling, to measure how far the machine is from it."""

from __future__ import annotations

import importlib.util
import math
from pathlib import Path

import numpy as np

f32 = np.float32

# `_dv_exp2` of core/dsp-voice (the SDK's exp2): x + 64, truncate, degree-5 polynomial, exact scale (x > -64)
EXP2_C = [
    f32(c)
    for c in (
        0.9999999403953552,
        0.6931530833244324,
        0.24015361070632935,
        0.0558263324201107,
        0.008989322930574417,
        0.001877583796158433,
    )
]


def exp2(x: np.float32) -> np.float32:
    x = f32(x + f32(64.0))
    i = int(x)  # TRUNC
    fr = f32(x - f32(i))
    p = EXP2_C[5]
    for c in (EXP2_C[4], EXP2_C[3], EXP2_C[2], EXP2_C[1], EXP2_C[0]):
        p = f32(f32(p * fr) + c)
    return f32(np.ldexp(p, i - 64))


_GEN = Path(__file__).resolve().parents[1] / "src/dsp/gen_tables.py"
_spec = importlib.util.spec_from_file_location("tb303_gen_tables", _GEN)
assert _spec and _spec.loader
gen = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gen)

SAW = gen.flat(gen.compact(gen.saw303()))
SQ = gen.flat(gen.compact(gen.square303()))
OFF = gen.offsets()
LEN = [f32(x) for x in gen.TB_LEN]
K = {name: f32(v) for name, v in gen.consts()}


def word01(w: int) -> np.float32:
    return min(f32(f32(w) * K["W01"]), K["ONE"])


def _one_pole(
    st: list, b0: np.float32, b1: np.float32, a1: np.float32, x: np.float32
) -> np.float32:
    y = f32(f32(f32(x * b0) + f32(st[0] * b1)) + f32(st[1] * a1))
    st[0], st[1] = x, y
    return y


def _biquad(st: list, pre: str, x: np.float32) -> np.float32:
    y = f32(x * K[pre + "B0"])
    y = f32(y + f32(st[0] * K[pre + "B1"]))
    y = f32(y + f32(st[1] * K[pre + "B2"]))
    y = f32(y + f32(st[2] * K[pre + "A1"]))
    y = f32(y + f32(st[3] * K[pre + "A2"]))
    st[1], st[0], st[3], st[2] = st[0], x, st[2], y
    return y


class Tb303:
    """One voice: `note_on(velocity, params)` at a note-on, then `block(pitch, params, released)` per 32 samples; params are the
    six knob words (WAVE, CUT, RES, ENV, DEC, ACC), `released` a block whose gate is down (the first one does what matters:
    the later ones repeat it)."""

    def __init__(self) -> None:
        z = f32(0.0)
        self.phase = self.freq = self.e = self.ec = self.ec32 = self.acc = self.rc = z
        # b0, k, 2g at the previous block's end (b0 = 0: none yet)
        self.coef = [z, z, z]
        self.glide = [z, z, z, z]  # u0, es, eo, rs
        self.rel, self.slide, self.accon = 0, z, z
        self.aenv, self.aon, self.arc, self.at, self.aks = z, z, z, z, z
        self.h1, self.fb, self.ap, self.h2 = [z, z], [z, z], [z, z], [z, z]
        self.y = [z, z, z, z]
        self.dc, self.n = [z, z, z, z], [z, z, z, z]

    def note_on(self, velocity: int, params: list[int]) -> None:
        self.slide = K["SLIDE"] if not self.rel and self.freq != 0 else f32(0.0)
        if not self.slide:
            self.at = self.aks = K["ONE"]
            # a new note's first block starts on its own coefficients, not a ramp from the last note's
            self.coef[0] = f32(0.0)
        self.aon = K["ONE"]
        self.rel = 0
        accent = velocity > 100 << 8
        self.acc = word01(params[5]) if accent else f32(0.0)
        self.accon = K["ONE"] if accent else f32(0.0)
        self.arc = K["ARELA"] if accent else K["AREL"]
        m = K["KDEC"]
        if not accent:
            m = f32(exp2(f32(word01(params[4]) * K["DX"])) * m)
        self.ec = exp2(-m)
        self.ec32 = self.ec
        for _ in range(5):
            self.ec32 = f32(self.ec32 * self.ec32)
        if not self.slide:
            self.e = exp2(m)

    def block(
        self, pitch: int, params: list[int], released: bool = False, n: int = 32
    ) -> np.ndarray:
        x = f32(f32(pitch) * f32(1 / 3072))
        x = max(min(x, f32(11.2)), f32(0.0))
        target = f32(exp2(x) * f32(440 * 2 ** (-69 / 12)))
        b = K["ONE"] if params[0] >= 0x80 else f32(0.0)
        xc = word01(params[1])
        u0 = f32(f32(xc * K["LC1"]) + K["LC0"])
        c = f32(f32(xc * K["C1"]) + K["C0"])
        xe = word01(params[3])
        slo = f32(f32(xe * K["SLOF"]) + K["SLOC"])
        shi = f32(f32(xe * K["SHIF"]) + K["SHIC"])
        es = f32(f32(f32(K["ONE"] - c) * slo) + f32(c * shi))
        eo = f32(f32(c * K["OF"]) + K["OC"])
        rs = f32(f32(K["ONE"] - exp2(f32(word01(params[2]) * K["RX"]))) * K["RN"])
        cs = self.slide
        self.rel |= int(released)
        if released:
            self.aon = f32(0.0)
        ak = K["ADEC"] if self.aon else self.arc
        ampk = f32(f32(self.acc * K["AMPA"]) + K["AMP0"]) if self.aon else f32(0.0)
        goal = [u0, es, eo, rs]
        if self.freq == 0:
            self.glide = list(goal)
        f = target if self.freq == 0 else self.freq
        # the filter's coefficients at the block's last sample: the glides one block on, the envelope at e ec^32, the accent
        # integrator one step of rc2^32; the samples ramp b0, k and 2g to them from the previous block's
        self.glide = [
            f32(c + f32(f32(t_ - c) * K["SM32"]))
            for c, t_ in zip(self.glide, goal, strict=True)
        ]
        u0, es, eo, rs = self.glide
        e = f32(self.e * self.ec32)
        a = f32(e * self.accon)
        self.rc = f32(f32(f32(self.rc - a) * K["RC32"]) + a)
        t = f32(f32(f32(e - eo) * es) + f32(self.rc * self.acc))
        u = max(f32(t + u0), K["ULO"])
        fx = exp2(min(u, K["UHI"]))
        d = f32(f32(f32(f32(fx * K["B2"]) + K["B1"]) * fx) + K["ONE"])
        r = f32(f32(f32(f32(d * K["RQ2"]) + K["RQ1"]) * d) + K["RQ0"])
        for _ in range(3):
            r = f32(r * f32(K["TWO"] - f32(d * r)))
        b0 = f32(f32(f32(fx * K["A1"]) + K["A0"]) * r)
        k = f32(fx + K["P0"])
        for p in ("P1", "P2", "P3", "P4", "P5"):
            k = f32(f32(k * fx) + K[p])
        g = f32(f32(f32(f32(k * K["R17"]) - K["ONE"]) * rs) + K["ONE"])
        g = f32(g * f32(K["ONE"] + rs))
        k = f32(k * rs)
        end = [b0, k, f32(g + g)]
        if self.coef[0] == 0:
            self.coef = list(end)
        step = [
            f32(f32(x - p_) * K["B32"]) for x, p_ in zip(end, self.coef, strict=True)
        ]
        b0, k, g2 = self.coef
        self.coef = end
        ph = self.phase
        out = np.empty(n, dtype=np.float32)
        for i in range(n):
            f = f32(f32(f32(f - target) * cs) + target)
            b0, k, g2 = (f32(b0 + step[0]), f32(k + step[1]), f32(g2 + step[2]))
            self.e = f32(self.e * self.ec)
            e = self.e
            self.aenv = f32(self.aenv + f32(f32(self.at - self.aenv) * self.aks))
            self.aks, self.at = ak, f32(0.0)
            amp = _biquad(self.dc, "DC", f32(f32(e * ampk) + self.aenv))
            # oscillator
            lev = max(int(np.frexp(f32(f * K["INCT"]))[1]) - 1 + 1, 0)
            pos = f32(ph * LEN[lev])
            j = int(pos)
            fr = f32(pos - f32(j))
            j += OFF[lev]
            saw = f32(SAW[j] + f32(f32(SAW[j + 1] - SAW[j]) * fr))
            sq = f32(SQ[j] + f32(f32(SQ[j + 1] - SQ[j]) * fr))
            xo = -f32(saw + f32(f32(f32(sq * K["HALF"]) - saw) * b))
            ph = f32(ph + f32(f * K["INVFS"]))
            if f32(ph - K["ONE"]) >= 0:
                ph = f32(ph - K["ONE"])
            # filters
            h = _one_pole(self.h1, K["H1B0"], K["H1B1"], K["H1A1"], xo)
            q = _one_pole(self.fb, K["FBB0"], K["FBB1"], K["FBA1"], f32(k * self.y[3]))
            y0 = f32(h - q)
            y1, y2, y3, y4 = self.y
            y1 = f32(y1 + f32(f32(b0 + b0) * f32(f32(y0 - y1) + y2)))
            y2 = f32(y2 + f32(b0 * f32(f32(y1 - f32(y2 + y2)) + y3)))
            y3 = f32(y3 + f32(b0 * f32(f32(y2 - f32(y3 + y3)) + y4)))
            y4 = f32(y4 + f32(b0 * f32(y3 - f32(y4 + y4))))
            self.y = [y1, y2, y3, y4]
            o = f32(g2 * y4)
            o = _one_pole(self.ap, K["APB0"], K["APB1"], K["APA1"], o)
            o = _one_pole(self.h2, K["H2B0"], K["H2B1"], K["H2A1"], o)
            o = _biquad(self.n, "N", o)
            out[i] = f32(f32(o * amp) * K["GAIN"])
        self.phase, self.freq = ph, f
        return out


class Open303:
    """rosic::Open303's voice in float64 (a tie jumps to its pitch, no slide): `n` x oversampling with plain decimation and the
    oscillator band-limited below fs / 2 (`n` = 4 with `ellip=True` is Open303 itself, its quarter-band elliptic filter)."""

    ELL_A = [-9.1891604652189471, 40.177553696870497, -110.11636661771178, 210.18506612078195, -293.84744771903240,
             308.16345558359234, -244.06786780384243, 144.81877911392738, -62.770692151724198, 18.867762095902137,
             -3.5327094230551848, 0.31183189275203149]  # fmt: skip
    ELL_B = [0.00013671732099945628, -0.00055538501265606384, 0.0013681887636296387, -0.0022158566490711852,
             0.0028320091007278322, -0.0029776933151090413, 0.0030283628243514991, -0.0029776933151090413,
             0.0028320091007278331, -0.0022158566490711861, 0.0013681887636296393, -0.00055538501265606384,
             0.00013671732099945636]  # fmt: skip
    FULL_SAW = gen.mipmap(gen.saw303())
    FULL_SQ = gen.mipmap(gen.square303())

    def __init__(self, wave: float, cutoff: float, reso: float, envmod: float, decay: float, accent: float = 100.0,
                 n: int = 4, ellip: bool = False, fs: float = 48000.0) -> None:  # fmt: skip
        self.n, self.ellip, self.fs, self.fo = n, ellip, fs, fs * n
        self.dl = 2 if ellip else int(math.log2(n)) + 1
        self.wave, self.cutoff, self.accent, self.decay = (
            wave,
            cutoff,
            accent / 100,
            decay,
        )
        self.rs = (1 - math.exp(-3 * reso / 100)) / (1 - math.exp(-3))
        e = envmod / 100
        c0, c1 = 3.138152786059267e002, 2.394411986817546e003
        c = math.log(cutoff / c0) / math.log(c1 / c0)
        slo, shi = (
            3.773996325111173 * e + 0.736965594166206,
            4.194548788411135 * e + 0.864344900642434,
        )
        self.es, self.eo = (
            (1 - c) * slo + c * shi,
            0.048292930943553 * c + 0.294391201442418,
        )
        self.rc2c = math.exp(-1 / (fs * 0.001 * 15.0))
        self.adec = 1 - math.exp(-1 / (fs * 1.230))
        self.aenv, self.aon, self.atk, self.arel = 0.0, False, False, 0.0

        def hp(fc: float, f: float) -> tuple[float, float, float]:
            x = math.exp(-2 * math.pi * fc / f)
            return 0.5 * (1 + x), -0.5 * (1 + x), x

        t = math.tan(math.pi * 14.008 / fs)
        self.c_h1, self.c_fb, self.c_h2 = (
            hp(44.486, self.fo),
            hp(150.0, self.fo),
            hp(24.167, fs),
        )
        self.c_ap = ((t - 1) / (t + 1), 1.0, -(t - 1) / (t + 1))
        w = 2 * math.pi * 7.5164 / fs
        al = math.sin(w) * math.sinh(0.5 * math.log(2.0) * 4.7 * w / math.sin(w))
        sc = 1 / (1 + al)
        self.c_n = (sc, -2 * math.cos(w) * sc, sc, 2 * math.cos(w) * sc, (al - 1) * sc)
        w = 2 * math.pi * 200.0 / fs
        al = math.sin(w) / (2 * math.sqrt(0.5))
        sc = 1 / (1 + al)
        b1 = (1 - math.cos(w)) * sc
        self.c_dc = (0.5 * b1, b1, 0.5 * b1, 2 * math.cos(w) * sc, (al - 1) * sc)
        self.phase, self.env, self.envc, self.rc, self.acc, self.freq = (
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            440.0,
        )
        self.st = {k: [0.0, 0.0, 0.0, 0.0] for k in ("h1", "fb", "ap", "h2", "n", "dc")}
        self.y = [0.0] * 4
        self.w = [0.0] * 12

    def note(self, note: int, velocity: int) -> None:
        """A note-on; velocity 0 is the note-off. A note-on while one is held is a tie: neither envelope restarts."""
        if not velocity:
            self.aon = False
            return
        self.freq = 440 * 2 ** ((note - 69) / 12)
        acc = velocity >= 100
        self.acc = self.accent if acc else 0.0
        self.envc = math.exp(-1 / (0.001 * (200.0 if acc else self.decay) * self.fs))
        self.arel = 1 - math.exp(-1 / (self.fs * 0.001 * (50.0 if acc else 1.0)))
        if not self.aon:
            self.env = 1 / self.envc
            self.atk = True
        self.aon = True

    def _op(self, k: str, c: tuple, x: float) -> float:
        s = self.st[k]
        y = c[0] * x + c[1] * s[0] + c[2] * s[1]
        s[0], s[1] = x, y
        return y

    def _bq(self, k: str, c: tuple, x: float) -> float:
        s = self.st[k]
        y = c[0] * x + c[1] * s[0] + c[2] * s[1] + c[3] * s[2] + c[4] * s[3]
        s[1], s[0], s[3], s[2] = s[0], x, s[2], y
        return y

    def sample(self) -> float:
        inc = 2048 * self.freq / self.fo
        self.env *= self.envc
        e = self.env
        a = e if self.acc > 0 else 0.0
        self.rc = a + self.rc2c * (self.rc - a)
        fc = min(
            max(
                self.cutoff * 2 ** (self.es * (e - self.eo) + self.acc * self.rc), 200.0
            ),
            20000.0,
        )
        fx = fc / self.fo / math.sqrt(2)
        b0 = (0.00045522346 + 6.1922189 * fx) / (
            1.0 + 12.358354 * fx + 4.4156345 * fx * fx
        )
        k = (
            fx
            * (
                fx
                * (
                    fx * (fx * (fx * (fx + 7198.6997) - 5837.7917) - 476.47308)
                    + 614.95611
                )
                + 213.87126
            )
            + 16.998792
        )
        g = ((k / 17.0 - 1.0) * self.rs + 1.0) * (1.0 + self.rs)
        k *= self.rs
        if self.atk:
            self.aenv, self.atk = 1.0, False
        else:
            self.aenv -= (self.adec if self.aon else self.arel) * self.aenv
        boost = 0.45 * e + self.acc * 4.0 * e if self.aon else 0.0
        amp = self._bq("dc", self.c_dc, self.aenv + boost)
        lev = min(max(math.frexp(inc)[1] - 1 + self.dl, 0), 11)
        y = self.y
        for _ in range(self.n):
            while self.phase >= 2048:
                self.phase -= 2048
            i = int(self.phase)
            fr = self.phase - i
            j = (i + 1) % 2048
            s1, s2 = self.FULL_SAW[lev], self.FULL_SQ[lev]
            o = (1 - self.wave) * ((1 - fr) * s1[i] + fr * s1[j]) + self.wave * (
                (1 - fr) * s2[i] + fr * s2[j]
            ) * 0.5
            self.phase += inc
            x = self._op("h1", self.c_h1, -o)
            y0 = x - self._op("fb", self.c_fb, k * y[3])
            y[0] += 2 * b0 * (y0 - y[0] + y[1])
            y[1] += b0 * (y[0] - 2 * y[1] + y[2])
            y[2] += b0 * (y[1] - 2 * y[2] + y[3])
            y[3] += b0 * (y[2] - 2 * y[3])
            f = 2 * g * y[3]
            if self.ellip:
                tmp = f - sum(
                    a_ * w_ for a_, w_ in zip(self.ELL_A, self.w, strict=True)
                )
                f = self.ELL_B[0] * tmp + sum(
                    b_ * w_ for b_, w_ in zip(self.ELL_B[1:], self.w, strict=True)
                )
                self.w = [tmp, *self.w[:11]]
        t = self._op("ap", self.c_ap, f)
        t = self._op("h2", self.c_h2, t)
        t = self._bq("n", self.c_n, t)
        return t * amp

    def render(self, notes: list[tuple[int, int, int]]) -> np.ndarray:
        """(note, velocity, samples) per note; velocity 0 is a note-off."""
        out = []
        for note, vel, count in notes:
            self.note(note, vel)
            out += [self.sample() for _ in range(count)]
        return np.array(out)
