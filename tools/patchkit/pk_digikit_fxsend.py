#!/usr/bin/env python3
"""FX send PCM analysis helpers and a bounded JSON interface.

uv run python -m tools.patchkit.pk_digikit_fxsend added-factor BASE.wav OUT.wav
uv run python -m tools.patchkit.pk_digikit_fxsend pcm FILE.wav
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np

from tools.patchkit import hook_ab

GAIN = 0.5  # test/dyn-fx-gain


def aligned(ref: np.ndarray, run: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """REF and RUN from the note's onset in each, over the common length (the boots reach the note up to a few blocks apart)."""

    def onset(x: np.ndarray) -> int:
        nz = np.flatnonzero(np.abs(x) > 1e-6)
        return int(nz[0]) if len(nz) else 0

    a, b = onset(ref), onset(run)
    n = min(len(ref) - a, len(run) - b)
    return ref[a : a + n], run[b : b + n]


def added_factor(dry: np.ndarray, out: np.ndarray) -> float:
    """The factor of the part that the send adds: OUT is the dry note times (1 + f), possibly started a few frames later than DRY (the note's start in the coupled run depends on how many
    DSP instructions the blocks before it cost, so a heavier hook starts it later). The least-squares gain of OUT over DRY at the best frame shift (even interleaved offsets, up to 256 frames
    either way; the shift that leaves the smallest residue, which is 0 for a note that starts together) is 1 + f. A median of ratios is noisy where the sound is quiet."""
    pad = 600
    n = min(len(dry), len(out)) - 2 * pad
    d = dry[pad : pad + n].astype(np.float64)
    dd = float(np.dot(d, d))
    best = (float("inf"), 0.0)
    for sh in range(-512, 513, 2):
        o = out[pad + sh : pad + sh + n].astype(np.float64)
        a = float(np.dot(o, d) / dd)
        res = float(np.abs(o - a * d).max())
        best = min(best, (res, a - 1.0))
    return best[1]


def _read_pcm(path: Path) -> np.ndarray:
    """Read completed capture samples with the existing shared WAV reader."""
    samples = hook_ab.read_wav(path)
    if not len(samples):
        raise ValueError(f"{path}: empty PCM")
    if not np.isfinite(samples).all():
        raise ValueError(f"{path}: PCM contains nonfinite samples")
    return samples


def main(argv: list[str] | None = None) -> int:
    """Small JSON analysis interface for host-owned FX send scenarios."""
    parser = argparse.ArgumentParser(description=main.__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    factor = commands.add_parser("added-factor")
    factor.add_argument("base", type=Path)
    factor.add_argument("out", type=Path)
    pcm = commands.add_parser("pcm")
    pcm.add_argument("file", type=Path)
    args = parser.parse_args(argv)
    result: dict[str, int | float | bool]
    try:
        if args.command == "pcm":
            samples = _read_pcm(args.file)
            result = {
                "samples": len(samples),
                "finite": True,
                "peak": float(np.abs(samples).max()),
            }
        else:
            base, out = _read_pcm(args.base), _read_pcm(args.out)
            n = min(len(base), len(out)) - 1200
            if n <= 0:
                raise ValueError("added-factor requires more than 1200 samples per WAV")
            dry = base[600 : 600 + n].astype(np.float64)
            if float(np.dot(dry, dry)) == 0:
                raise ValueError(
                    "added-factor requires a non-silent reference fit window"
                )
            value = added_factor(base, out)
            if not np.isfinite(value):
                raise ValueError("added-factor produced a nonfinite result")
            result = {"added_factor": value}
    except (OSError, ValueError, KeyError, SystemExit) as error:
        print(f"{parser.prog}: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, allow_nan=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
