#!/usr/bin/env python3
"""A held note's audio: its pitch, what lies between its harmonics, clicks, and render against render.

audiocheck answers "what is in this run". This module answers the questions an oscillator change raises:

    fundamental        pitch by autocorrelation, 20 Hz..4 kHz, on the loudest stretch
    between_db         power between the harmonics of f0 against all power above 20 Hz: aliasing and
                       noise. This is the measure behind the Waverider sub's polyBLEP decision (dn2_firmware,
                       2026-10-26): tests/dsp/test_audiotone.py rebuilds that table from it
    above_db           power above a frequency (20 kHz by default) against all power
    steps              sample-to-sample jumps that stand out: periodic (a saw's reset) or isolated (a click)
    compare            two renders sample for sample: identical, or by how much and from where
    note_report        all of the above for one file, with plain findings

    uv run python -m tools.emulator.audiotone RENDER.wav [RENDER2.wav ...] [--f0 HZ]
    uv run python -m tools.emulator.audiotone --compare BEFORE.wav AFTER.wav

Prints JSON. Arrays are float64 of shape (n, channels), as audiocheck.load returns them.
"""

from __future__ import annotations

import argparse
import json
import math
import sys

import numpy as np

from tools.emulator.audiocheck import F, load

SILENT = 1e-3
FLOOR_DB = -120.0


def _mono(x: F) -> F:
    return x.mean(axis=1) if x.ndim == 2 else x


def _power_db(p: float) -> float:
    return max(FLOOR_DB, 10 * math.log10(p)) if p > 0 else FLOOR_DB


def loudest(mono: F, rate: int, seconds: float = 0.5) -> tuple[int, int]:
    """(start, length) of the loudest stretch of `seconds`: where the pitch is read."""
    n = min(len(mono), int(seconds * rate))
    if n <= 0:
        return 0, 0
    total = np.concatenate(([0.0], np.cumsum(mono**2)))
    return int(np.argmax(total[n:] - total[:-n])), n


def fundamental(seg: F, rate: int) -> float | None:
    """The pitch of a mono segment by autocorrelation, 20 Hz..4 kHz; None when it has none."""
    n = len(seg)
    if n < 256:
        return None
    seg = seg - seg.mean()
    if np.abs(seg).max() < SILENT:
        return None
    size = 1 << int(np.ceil(np.log2(2 * n)))
    ac = np.fft.irfft(np.abs(np.fft.rfft(seg, size)) ** 2)[:n]
    if ac[0] <= 0:
        return None
    ac = ac / ac[0]
    below = np.flatnonzero(
        ac < 0
    )  # past lag 0's own lobe, or every lag looks like a period
    lo = max(int(rate / 4000), int(below[0]) if len(below) else n)
    hi = min(n - 2, int(rate / 20))
    if hi <= lo + 2:
        return None
    best = float(ac[lo:hi].max())
    if best < 0.5:
        return None
    # the first peak as good as the best: no octave drop
    k = next(
        (
            j
            for j in range(lo + 1, hi)
            if ac[j] >= 0.9 * best and ac[j] >= ac[j - 1] and ac[j] >= ac[j + 1]
        ),
        lo + int(np.argmax(ac[lo:hi])),
    )
    a, b, c = ac[k - 1], ac[k], ac[k + 1]
    curve = a - 2 * b + c
    shift = float(np.clip(0.5 * (a - c) / curve, -0.5, 0.5)) if curve < 0 else 0.0
    return float(rate / (k + shift))


def between_db(seg: F, rate: int, f0: float, near: float = 0.12) -> float:
    """Power more than `near` x f0 away from every harmonic of f0, against all power above 20 Hz, in dB
    (Blackman window over the whole segment). Lower is cleaner: a naive square at 110 Hz reads about
    -27 dB, the same square with a polyBLEP about -39 dB."""
    x = _mono(seg)
    p = np.abs(np.fft.rfft(x * np.blackman(len(x)))) ** 2
    f = np.fft.rfftfreq(len(x), 1 / rate)
    harmonic = np.abs(f / f0 - np.round(f / f0)) < near
    audible = f > 20
    return _power_db(
        float(p[~harmonic & audible].sum()) / (float(p[audible].sum()) or 1e-24)
    )


def above_db(seg: F, rate: int, hz: float = 20000.0) -> float:
    """Power above `hz` against all power above 20 Hz, in dB (Hann window)."""
    x = _mono(seg)
    p = np.abs(np.fft.rfft(x * np.hanning(len(x)))) ** 2
    f = np.fft.rfftfreq(len(x), 1 / rate)
    return _power_db(float(p[f > hz].sum()) / (float(p[f > 20].sum()) or 1e-24))


def steps(mono: F, rate: int) -> dict[str, object]:
    """Jumps above 4x the 99th percentile of the jumps in the same 2048 samples, and at least 2 % of full
    scale. Jumps at even gaps (a saw's reset) are `periodic`; the others are `isolated`: a note's edge or a
    click. A click smaller than four of a bright wave's own jumps is not seen here."""
    empty: dict[str, object] = {
        "periodic": 0,
        "isolated": 0,
        "largest_isolated": 0.0,
        "times_s": [],
    }
    d = np.abs(np.diff(mono))
    if len(d) < 16 or d.max() < SILENT:
        return empty
    block = 2048
    pad = np.pad(d, (0, -len(d) % block), constant_values=np.nan).reshape(-1, block)
    local = np.maximum(4 * np.nanpercentile(pad, 99, axis=1), 0.02)
    hits = np.flatnonzero(d > np.repeat(local, block)[: len(d)])
    if len(hits) == 0:
        return empty
    groups = [int(hits[0])]
    for h in hits[1:]:
        if h - groups[-1] > rate // 1000:
            groups.append(int(h))
    gaps = np.diff(groups)

    def even(i: int) -> bool:  # gaps i and i + 1 the same, within 5 %
        return 0 <= i < len(gaps) - 1 and abs(
            int(gaps[i]) - int(gaps[i + 1])
        ) <= 0.05 * max(gaps[i], gaps[i + 1])

    alone = [
        g for i, g in enumerate(groups) if not (even(i - 2) or even(i - 1) or even(i))
    ]
    return {
        "periodic": len(groups) - len(alone),
        "isolated": len(alone),
        "largest_isolated": round(float(max((d[g] for g in alone), default=0.0)), 5),
        "times_s": [round(g / rate, 4) for g in alone[:8]],
    }


def compare(a: F, b: F, rate: int) -> dict[str, object]:
    """B against A, sample for sample: the same render, or by how much and from where."""
    n = min(len(a), len(b))
    out: dict[str, object] = {"frames_a": len(a), "frames_b": len(b), "compared": n}
    if not n or a.shape[1] != b.shape[1]:
        return out | {"identical": False, "same_channels": a.shape[1] == b.shape[1]}
    diff = b[:n] - a[:n]
    worst = float(np.abs(diff).max())
    where = np.flatnonzero(np.abs(diff).max(axis=1) > 0)
    rms_a, rms_b = (
        float(np.sqrt(np.mean(a[:n] ** 2))),
        float(np.sqrt(np.mean(b[:n] ** 2))),
    )
    return out | {
        "identical": worst == 0.0 and len(a) == len(b),
        "max_difference": worst,
        "max_difference_lsb16": round(worst * 32768, 2),
        "difference_rms_db": round(_power_db(float(np.mean(diff**2))), 2),
        "level_change_db": round(_power_db(rms_b**2) - _power_db(rms_a**2), 3),
        "first_difference_s": round(int(where[0]) / rate, 5) if len(where) else None,
        "differing_frames": int(len(where)),
    }


def note_report(x: F, rate: int, f0: float | None = None) -> dict[str, object]:
    """Pitch, between_db, above_db and steps for one render, read on its loudest half second, with findings."""
    mono = _mono(x)
    start, n = loudest(mono, rate)
    seg = mono[start : start + n]
    pitch = f0 or fundamental(seg, rate)
    between = round(between_db(seg, rate, pitch), 1) if pitch else None
    above = round(above_db(seg, rate), 1)
    s = steps(mono, rate)
    notes = []
    if between is not None and between > -30:
        notes.append(
            f"{between} dB lies between the harmonics of {pitch:.2f} Hz: "
            "aliasing, noise or modulation"
        )
    if above > -50:
        notes.append(
            f"{above} dB of the power is above 20 kHz: aliasing or a bright source"
        )
    if s["isolated"]:
        notes.append(
            f"{s['isolated']} isolated step(s) at {s['times_s']} s: a note's edge or a click"
        )
    return {
        "fundamental_hz": round(pitch, 2) if pitch else None,
        "read_at_s": [round(start / rate, 3), round((start + n) / rate, 3)],
        "between_db": between,
        "above_20k_db": above,
        "steps": s,
        "findings": notes,
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("wav", nargs="*")
    ap.add_argument(
        "--f0",
        type=float,
        help="the pitch to measure against (default: read from the audio)",
    )
    ap.add_argument("--compare", nargs=2, metavar=("BEFORE", "AFTER"))
    a = ap.parse_args(argv)
    if a.compare:
        (ra, xa), (rb, xb) = load(a.compare[0]), load(a.compare[1])
        if ra != rb:
            print(json.dumps({"identical": False, "rates": [ra, rb]}))
            return 1
        r = compare(xa, xb, ra)
        print(json.dumps(r))
        return 0 if r["identical"] else 1
    if not a.wav:
        ap.error("give a WAV, or --compare BEFORE AFTER")
    for path in a.wav:
        rate, x = load(path)
        print(json.dumps({"file": path} | note_report(x, rate, a.f0)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
