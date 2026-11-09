"""Open303's oscillator tables (rosic::MipMappedWaveTable, SAW303 and SQUARE303) for the TB303 machine.

Open303 keeps 12 mip levels of 2048 samples per waveform; level t keeps the bins below 1024 >> t (level 0 is the
prototype itself). To fit the runtime's 64 KB module stage: level 0, which Open303 reads only below about 23 Hz, reads
level 1 (band-limited) here, and levels 3 and up are stored shorter (TB_LEN): at least 8 samples per cycle of their top
harmonic, still finer than Open303 reads levels 1 and 2 (4).

    uv run python modules/machines/tb303/src/dsp/gen_tables.py   # writes tables.inc next to this file
"""

from __future__ import annotations

import math
import struct
import sys
from pathlib import Path

import numpy as np

N = 2048
LEVELS = 12
TB_LEN = [N, N, N, 1024, 512, 256, 128, 64, 64, 64, 64, 64]  # level 0 is level 1's data


def _round(x: float) -> int:
    return int(math.floor(x + 0.5))


def saw303() -> np.ndarray:
    n1 = min(max(_round(0.5 * (N - 1)), 1), N - 1)
    n2 = N - n1
    p = np.empty(N)
    p[:n1] = np.arange(n1) / (n1 - 1)
    p[n1:] = -1.0 + np.arange(n2) / n2
    return p


def square303(
    drive_db: float = 36.9, offset: float = 4.37, shift_deg: float = 180.0
) -> np.ndarray:
    p = -np.tanh(10 ** (drive_db / 20) * saw303() + offset)
    return np.roll(p, _round(N * shift_deg / 360.0))


def mipmap(proto: np.ndarray) -> list[np.ndarray]:
    """Open303's levels at full length (generateMipMap)."""
    spec = np.fft.rfft(proto)
    spec[0] = 0.0
    spec[-1] = 0.0
    levels = [proto.copy()]
    for t in range(1, LEVELS):
        spec[(N >> t) // 2 :] = 0.0
        levels.append(np.fft.irfft(spec, N))
    return levels


def compact(proto: np.ndarray) -> list[np.ndarray]:
    """Levels 1 to 11 at TB_LEN samples: the same spectrum, resampled exactly."""
    full = mipmap(proto)
    out = []
    for t, length in enumerate(TB_LEN):
        if t == 0:
            continue
        if length == N:
            out.append(full[t])
            continue
        spec = np.fft.rfft(full[t])[: length // 2 + 1] * (length / N)
        out.append(np.fft.irfft(spec, length))
    return out


def offsets() -> list[int]:
    """Word offset of each level (level 0: level 1's); every level carries one guard sample (its first) for the interpolator."""
    off, at = [0], 0
    for length in TB_LEN[1:]:
        off.append(at)
        at += length + 1
    return off


def flat(levels: list[np.ndarray]) -> np.ndarray:
    return np.concatenate([np.append(lv, lv[0]) for lv in levels]).astype(np.float32)


def level_for(inc_tbl: float) -> int:
    """Open303's table choice at 48 kHz without oversampling: exponent of the table increment + 1."""
    e = math.frexp(inc_tbl)[1] - 1 if inc_tbl > 0 else -1
    return min(max(e + 1, 0), LEVELS - 1)


FS = 48000.0


def _onepole_hp(fc: float) -> list[float]:
    x = math.exp(-2 * math.pi * fc / FS)
    return [0.5 * (1 + x), -0.5 * (1 + x), x]


def _onepole_ap(fc: float) -> list[float]:
    t = math.tan(math.pi * fc / FS)
    x = (t - 1) / (t + 1)
    return [x, 1.0, -x]


def _biquad(kind: str, f: float, q_or_bw: float) -> list[float]:
    """RBJ cookbook as Open303's BiquadFilter: b0, b1, b2, a1, a2 with y = b.x + a1 y1 + a2 y2."""
    w = 2 * math.pi * f / FS
    s, c = math.sin(w), math.cos(w)
    if kind == "lp12":
        al = s / (2 * q_or_bw)
        sc = 1 / (1 + al)
        b1 = (1 - c) * sc
        return [0.5 * b1, b1, 0.5 * b1, 2 * c * sc, (al - 1) * sc]
    al = s * math.sinh(0.5 * math.log(2.0) * q_or_bw * w / s)
    sc = 1 / (1 + al)
    return [sc, -2 * c * sc, sc, 2 * c * sc, (al - 1) * sc]


def consts() -> list[tuple[str, float]]:
    """The routine's constants (`_tb_k`, read through I1 at K_<name>), from Open303's values at 48 kHz without oversampling."""
    c0, c1 = (
        3.138152786059267e002,
        2.394411986817546e003,
    )  # Open303's measured cutoff range of the env-mod mapping
    off = math.log2(FS * math.sqrt(2))  # fx = fc / (fs sqrt 2) = 2^(log2 fc - off)
    k_env = 1 / (0.001 * FS * math.log(2))  # DecayEnvelope: c = 2^(-k_env / tau_ms)
    out = [
        ("ONE", 1.0),
        ("TWO", 2.0),
        ("HALF", 0.5),
        ("INCT", 2048 / FS),
        ("INVFS", 1 / FS),
        ("W01", 1 / 32512),
        (
            "LC0",
            math.log2(314.0) - off,
        ),  # CUT: Open303's knob, 314 .. 2394 Hz exponential
        ("LC1", math.log2(2394.0 / 314.0)),
        ("C0", math.log(314.0 / c0) / math.log(c1 / c0)),
        ("C1", math.log(2394.0 / 314.0) / math.log(c1 / c0)),
        ("SLOF", 3.773996325111173),
        ("SLOC", 0.736965594166206),
        ("SHIF", 4.194548788411135),
        ("SHIC", 0.864344900642434),
        ("OF", 0.048292930943553),
        ("OC", 0.294391201442418),
        ("RX", -3 / math.log(2)),  # RES: (1 - e^-3r) / (1 - e^-3)
        ("RN", 1 / (1 - math.exp(-3))),
        ("DX", -math.log2(10)),  # DEC: 200 .. 2000 ms exponential
        ("KDEC", k_env / 200.0),
        ("ULO", math.log2(200.0) - off),  # TeeBeeFilter's cutoff floor and ceiling
        ("UHI", math.log2(20000.0) - off),
        ("B1", 12.358354),  # b0 = (A0 + A1 fx) / (1 + B1 fx + B2 fx^2)
        ("B2", 4.4156345),
        ("A0", 0.00045522346),
        ("A1", 6.1922189),
        (
            "RQ0",
            1.32893302,
        ),  # 1 / d on [1, 5.03]: this quadratic, then three Newton steps (2e-7)
        ("RQ1", -0.49643794),
        ("RQ2", 0.05493414),
        (
            "P0",
            7198.6997,
        ),  # k = fx (fx (fx (fx (fx (fx + P0) + P1) + P2) + P3) + P4) + P5
        ("P1", -5837.7917),
        ("P2", -476.47308),
        ("P3", 614.95611),
        ("P4", 213.87126),
        ("P5", 16.998792),
        ("R17", 1 / 17),
        (
            "RC32",
            math.exp(-32 / (FS * 0.001 * 15.0)),
        ),  # rc2, the accent integrator (15 ms), over a block
        ("AMP0", 0.45),
        ("AMPA", 4.0),
        ("GAIN", 0.5),  # Open303 plug-in's default volume, -6 dB
        (
            "SM32",
            1 - math.exp(-32 / (FS * 0.002)),
        ),  # the knob-derived values glide with a 2 ms time constant, a block per step
        (
            "SLIDE",
            math.exp(-1 / (FS * 0.012)),
        ),  # Open303's pitchSlewLimiter: 0.2 x its 60 ms slide time
        ("ADEC", 1 - math.exp(-1 / (FS * 1.230))),  # Open303's ampEnv: decay 1230 ms to sustain 0,
        ("AREL", 1 - math.exp(-1 / (FS * 0.001))),  # release 1 ms,
        ("ARELA", 1 - math.exp(-1 / (FS * 0.050))),  # 50 ms after an accent
        ("B32", 1 / 32),  # the coefficient ramps' step per sample
    ]
    for name, coef in [
        ("H1", _onepole_hp(44.486)),
        ("FB", _onepole_hp(150.0)),
        ("AP", _onepole_ap(14.008)),
        ("H2", _onepole_hp(24.167)),
    ]:
        out += [(f"{name}B0", coef[0]), (f"{name}B1", coef[1]), (f"{name}A1", coef[2])]
    for name, coef in [
        ("DC", _biquad("lp12", 200.0, math.sqrt(0.5))),
        ("N", _biquad("notch", 7.5164, 4.7)),
    ]:
        out += [
            (f"{name}{k}", v)
            for k, v in zip(("B0", "B1", "B2", "A1", "A2"), coef, strict=True)
        ]
    return out


def _hex(words: np.ndarray) -> str:
    return words.astype("<f4").tobytes().hex()


def emit() -> str:
    saw, sq = flat(compact(saw303())), flat(compact(square303()))
    off = struct.pack(f"<{LEVELS}i", *offsets()).hex()
    length = struct.pack(f"<{LEVELS}f", *[float(x) for x in TB_LEN]).hex()
    return "\n".join(
        [
            "// machines/tb303: Open303's SAW303 and SQUARE303 levels. Generated by gen_tables.py; do not edit.",
            f"// {LEVELS} levels of {', '.join(map(str, TB_LEN))} samples (level 0 is level 1), each followed by its first sample.",
            f"#define TB_WORDS {len(saw)}",
            f".PKHEX _tb_saw, {_hex(saw)};",
            f".PKHEX _tb_sq, {_hex(sq)};",
            f".PKHEX _tb_off, {off};",
            f".PKHEX _tb_len, {length};",
            *[f"#define K_{name} {i}" for i, (name, _) in enumerate(consts())],
            f".PKHEX _tb_k, {_hex(np.array([v for _, v in consts()], dtype=np.float32))};",
            "",
        ]
    )


if __name__ == "__main__":
    out = (
        Path(sys.argv[1])
        if len(sys.argv) > 1
        else Path(__file__).with_name("tables.inc")
    )
    out.write_text(emit())
    print(f"wrote {out}")
