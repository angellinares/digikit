"""A long run of the frequency shifter on the concrete SHARC+ core (plain Python, so that it also runs under PyPy), printed as JSON.

    PYTHONPATH=<repo>:<repo>/tests python <this file> [--blocks N] [--v SHIFT] [--hz F]

A sine of F Hz (amplitude 0.5) goes through the effect behind core/fx for N blocks. The shift of the oscillator is exactly known from the increment
the routine derives (the integer `inc`), so the output is a tone at F + inc 48000 / 2^32. For eight windows spread over the run the worker
reports the amplitude and the phase of that tone (a Hann-windowed projection at the exact frequency: a drift of the oscillator's frequency or level shows as a
change of phase or amplitude from window to window) and, at the end, the oscillator's integer phase against the closed form
`528 d + (N - 1) 32 inc` (d = inc >> 5: the first block ramps from a zero increment)."""

from __future__ import annotations

import argparse
import cmath
import json
import math
from pathlib import Path

from support.fx_rig import FxRig, Row, f2w, w2f

SRC = Path(__file__).resolve().parents[1] / "src/dsp/freqshift.s"
FS = 48000.0
WINDOW = 400  # blocks per window: 12,800 samples
STATE_PHASE = 0  # state word 0 of the effect (freqshift.s)
STATE_INC = 1


def f32(x: float) -> float:
    return w2f(f2w(x))


def project(ys: list[float], f: float, first: int) -> tuple[float, float]:
    """The amplitude and the phase of the component at f Hz of ys, whose first sample is the sample number FIRST (a Hann window)."""
    n = len(ys)
    num = 0j
    wsum = 0.0
    for i, y in enumerate(ys):
        w = 0.5 - 0.5 * math.cos(2 * math.pi * i / (n - 1))
        num += w * y * cmath.exp(-2j * math.pi * f * (first + i) / FS)
        wsum += w
    z = 2 * num / wsum
    return abs(z), cmath.phase(z)


def run(blocks: int, v: float, f_in: float) -> dict:
    rig = FxRig(
        [Row("_fs_process", "", desc="_fs_desc", params=(f2w(v),))], sources=(SRC,)
    )
    windows = [round(i * (blocks - WINDOW) / 7) for i in range(8)]
    out: list[list[float]] = [[] for _ in windows]
    peak = 0.0
    for k in range(blocks):
        x = [
            f32(0.5 * math.sin(2 * math.pi * f_in * (k * 32 + i) / FS))
            for i in range(32)
        ]
        b = rig.block(x + x, ctx="friendly")
        assert b.returned and not b.ctx_bad and not b.regs_bad, (k, b.halt)
        y = rig.scratch()[:32]
        peak = max(peak, max(abs(s) for s in y))
        for j, w0 in enumerate(windows):
            if w0 <= k < w0 + WINDOW:
                out[j].extend(y)
    inc = rig.state_word(0, STATE_INC)
    inc = inc - (1 << 32) if inc >= 1 << 31 else inc
    f_out = f_in + inc * FS / 2**32
    # the sample numbers are exact integers, so the phase of a window is comparable with every other
    rows = []
    for j, w0 in enumerate(windows):
        a, ph = project(out[j], f_out, w0 * 32)
        rows.append({"first_block": w0, "amplitude": a, "phase": ph})
    d = inc >> 5
    want = (528 * d + (blocks - 1) * 32 * inc) & 0xFFFFFFFF
    return {
        "blocks": blocks,
        "inc": inc,
        "f_out": f_out,
        "windows": rows,
        "phase_word": rig.state_word(0, STATE_PHASE),
        "phase_want": want,
        "peak": peak,
    }


def main() -> None:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument("--blocks", type=int, default=1500)
    ap.add_argument("--v", type=float, default=33.3)
    ap.add_argument("--hz", type=float, default=1000.0)
    a = ap.parse_args()
    print(json.dumps(run(a.blocks, a.v, a.hz)))


if __name__ == "__main__":
    main()
