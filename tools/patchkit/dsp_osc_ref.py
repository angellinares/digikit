"""Integer noise and float32 folded-phase sine references for sdk/dsp.

The sine model rounds every multiply/add separately, matching BD/PD's original
Horner order. Phase folding, noise seeds and signed float conversion are caller-owned.
"""

from __future__ import annotations

import numpy as np


def xorshift32(state: int) -> int:
    """One unsigned 32-bit xorshift (13, -17, 5) step; zero is absorbing."""
    state &= 0xFFFFFFFF
    state ^= (state << 13) & 0xFFFFFFFF
    state ^= state >> 17
    return (state ^ (state << 5)) & 0xFFFFFFFF


def sine_poly(x) -> np.ndarray:
    """DK_SINE_POLY_STEP on an already folded phase x in [-1/4, 1/4]."""
    x = np.asarray(x, dtype=np.float32)
    square = x * x
    # Exact float32 coefficient values from the existing module polynomial.
    a9, a7, a5, a3, a1 = np.array(
        [0x42283C1A, 0xC2996966, 0x42A335E3, 0xC2255DE7, 0x40C90FDB],
        dtype=np.uint32,
    ).view(np.float32)
    y = a9 * square
    y = y + a7
    y = y * square
    y = y + a5
    y = y * square
    y = y + a3
    y = y * square
    y = y + a1
    y = y * x
    return -y
