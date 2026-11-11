"""Float32 reference models of the SDK DSP step macros (sdk/dsp/**/*.inc) over one block of 32 samples.

Each model does the macro's operations in its order, with one float32 rounding per SHARC+ operation and no fused
multiply-add, so the kernel on the concrete core equals the model bit for bit: gain, mix, ramp, onepole, sat, biquad, delay,
halfband_down.

recip, div and rsqrt start from the core's seed (RECIPS 8 bits, RSQRTS 4 bits; the seed table is not published and is not
modelled). They return the exact value in float64; the kernel is compared within a relative bound:

  REL_RECIP = 2^-21   2 Newton steps from an 8-bit seed leave about 2^-32; the rest is rounding. Each step rounds x y,
                      2 - x y and y (2 - x y) once, about 3 x 2^-24 = 1.8e-7 in the last step. 2^-21 = 4.8e-7 is 4 ulp.
  REL_DIV   = 2^-20   the reciprocal bound plus the rounding of the final multiply (2^-24), rounded up to 2^-20.
  REL_RSQRT = 2^-21   3 Newton steps from a 4-bit seed leave about 4e-9 (e' = 1.5 e^2: 6e-3, 5e-5, 4e-9); the rest is
                      the rounding of x y, (x y) y, 1.5 - .., y (..), about 4 x 2^-24 = 2.4e-7.
"""

from __future__ import annotations

import numpy as np

f32 = np.float32

BLOCK = 32
REL_RECIP = 2.0**-21
REL_DIV = 2.0**-20
REL_RSQRT = 2.0**-21


def _v(x) -> np.ndarray:
    return np.asarray(x, dtype=f32)


def gain(x, g) -> np.ndarray:
    """DK_GAIN_STEP: y = x g."""
    return _v(x) * f32(g)


def mix(a, b, m) -> np.ndarray:
    """DK_MIX_STEP: t = b - a; t = t m; y = a + t."""
    a = _v(a)
    t = _v(b) - a
    t = t * f32(m)
    return a + t


def ramp(cur, target, n: int = BLOCK, inv_n: float = 1.0 / BLOCK) -> np.ndarray:
    """DK_RAMP_SETUP then n x DK_RAMP_STEP: the values after each step (the first is cur + step)."""
    step = f32(target) - f32(cur)
    step = step * f32(inv_n)
    c = f32(cur)
    out = np.empty(n, dtype=f32)
    for i in range(n):
        c = c + step
        out[i] = c
    return out


def onepole(x, a, y0) -> np.ndarray:
    """DK_ONEPOLE_STEP per sample: t = x - y; t = a t; y = y + t; returns y after each sample."""
    x = _v(x)
    a = f32(a)
    y = f32(y0)
    out = np.empty(len(x), dtype=f32)
    for i, xi in enumerate(x):
        t = xi - y
        t = a * t
        y = y + t
        out[i] = y
    return out


def biquad(x, b0, b1, b2, a1, a2, s1, s2):
    """_dk_biquad (sdk/dsp/filter/biquad.inc), per sample: p0 = b0 x; p1 = b1 x; p2 = b2 x; y = p0 + s1; u = s2 + p1; q = a1 y;
    s1 = u - q; q = a2 y; s2 = p2 - q. Returns (y, s1, s2) with the state after the last sample."""
    x = _v(x)
    b0, b1, b2, a1, a2 = (f32(c) for c in (b0, b1, b2, a1, a2))
    s1, s2 = f32(s1), f32(s2)
    out = np.empty(len(x), dtype=f32)
    for i, xi in enumerate(x):
        p0 = b0 * xi
        p1 = b1 * xi
        p2 = b2 * xi
        y = p0 + s1
        u = s2 + p1
        q = a1 * y
        s1 = u - q
        q = a2 * y
        s2 = p2 - q
        out[i] = y
    return out, s1, s2


def delay(x, ring, w, d):
    """_dk_delay (sdk/dsp/delay/delay.inc), per sample: k = trunc(d); f = d - k; g = 1 - f; ring[w] = x; a = ring[w - k];
    b = ring[w - k - 1]; p = g a; q = f b; y = p + q; w = w + 1 (indices mod len(ring)). Returns (y, ring, w) with the ring and
    the write offset after the block; `ring` is not changed."""
    x = _v(x)
    ring = _v(ring).copy()
    n = len(ring)
    w = int(w)
    d = f32(d)
    k = int(np.trunc(d))
    f = d - f32(k)
    g = f32(1.0) - f
    out = np.empty(len(x), dtype=f32)
    for i, xi in enumerate(x):
        ring[w] = xi
        a = ring[(w - k) % n]
        b = ring[(w - k - 1) % n]
        p = g * a
        q = f * b
        out[i] = p + q
        w = (w + 1) % n
    return out, ring, w


def halfband_design() -> np.ndarray:
    """The odd taps c1, c3, c5, c7, c9, c11 (float64) of the 23-tap half-band in sdk/dsp/filter/halfband.inc: the ideal half-band
    h[n] = sin(pi n / 2) / (pi n) times a Kaiser window of length 23 (np.kaiser(23, HALFBAND_BETA), n = -11 .. 11), scaled so that
    the DC gain 0.5 + 2 (c1 + c3 + ... + c11) is exactly 1.0 in float64. The centre tap is exactly 0.5 and the even taps are 0.
    beta 3.4 puts the edges of the transition band (Kaiser's estimate, 22 x 2.285 x d(omega) = A - 8) at about 0.2 and 0.3 fs for
    A of about 40 dB: a larger beta gives more attenuation far from the edge but a wider transition, so less at 0.3 fs."""
    j = np.array([1.0, 3.0, 5.0, 7.0, 9.0, 11.0])
    ideal = np.sin(np.pi * j / 2) / (np.pi * j)
    window = np.kaiser(2 * HALFBAND_HALF + 1, HALFBAND_BETA)[
        HALFBAND_HALF + j.astype(int)
    ]
    c = ideal * window
    return c * (0.25 / np.sum(c))


HALFBAND_BETA = 3.4
HALFBAND_HALF = 11  # the centre tap index: 23 taps
# The kernel's table: d_j = 2 c_j as float32 (exact doubling), j = 1, 3, 5, 7, 9, 11. The routine sums d_j s_j and averages
# with the centre, (x[2m-11] + t) / 2, so that it needs no 0.5 register.
HALFBAND_C = tuple(f32(2.0 * c) for c in halfband_design())
HALFBAND_RING = 32


def halfband_down(x, hist, off):
    """_dk_halfband_down (sdk/dsp/filter/halfband.inc) over 2 n inputs. Per output m: write x[2m], x[2m + 1] at ring cells o, o + 1
    (o = the offset, then o += 2); with the cell of x[2m - q] at o - q (o before the writes): s1 = x[2m-10] + x[2m-12];
    s3 = x[2m-8] + x[2m-14]; s5 = x[2m-6] + x[2m-16]; s7 = x[2m-4] + x[2m-18]; s9 = x[2m-2] + x[2m-20]; s11 = x[2m] + x[2m-22];
    p_j = d_j s_j (d_j = HALFBAND_C = 2 c_j); t = ((((p1 + p3) + p5) + p7) + p11) + p9; y = (x[2m-11] + t) / 2 (one rounding
    of the sum, then the exact halving). Returns (y, ring, off) after the block; `hist` is not changed."""
    x = _v(x)
    ring = _v(hist).copy()
    off = int(off)
    d1, d3, d5, d7, d9, d11 = HALFBAND_C
    half = f32(0.5)
    out = np.empty(len(x) // 2, dtype=f32)
    for m in range(len(x) // 2):
        o = off

        def at(q, o=o):
            return ring[(o - q) % HALFBAND_RING]

        xe, xo = x[2 * m], x[2 * m + 1]
        s1 = at(10) + at(12)
        s3 = at(8) + at(14)
        s5 = at(6) + at(16)
        s7 = at(4) + at(18)
        s9 = at(2) + at(20)
        s11 = xe + at(22)
        t = d1 * s1 + d3 * s3
        t = t + d5 * s5
        t = t + d7 * s7
        t = t + d11 * s11
        t = t + d9 * s9
        out[m] = (at(11) + t) * half
        ring[o % HALFBAND_RING] = xe
        ring[(o + 1) % HALFBAND_RING] = xo
        off = (o + 2) % HALFBAND_RING
    return out, ring, off


def sat(x) -> np.ndarray:
    """DK_SAT_STEP: c = clip(x, +-1); t = c c; t = t 0.5; t = 1.5 - t; y = c t."""
    c = np.clip(_v(x), f32(-1.0), f32(1.0))
    t = c * c
    t = t * f32(0.5)
    t = f32(1.5) - t
    return c * t


def recip(x) -> np.ndarray:
    """DK_RECIP: the exact 1 / x in float64 (compare within REL_RECIP)."""
    return 1.0 / _v(x).astype(np.float64)


def div(n, d) -> np.ndarray:
    """DK_DIV: the exact n / d in float64 (compare within REL_DIV)."""
    return _v(n).astype(np.float64) / _v(d).astype(np.float64)


def rsqrt(x) -> np.ndarray:
    """DK_RSQRT: the exact 1 / sqrt(x) in float64 (compare within REL_RSQRT), x > 0."""
    return 1.0 / np.sqrt(_v(x).astype(np.float64))


def max_rel_error(got, want) -> float:
    """Largest |got - want| / |want| over the block."""
    got = np.asarray(got, dtype=np.float64)
    want = np.asarray(want, dtype=np.float64)
    return float(np.max(np.abs(got - want) / np.abs(want)))
