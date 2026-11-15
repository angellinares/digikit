"""Tone analysis for the the frequency shifter tests (plain numpy)."""

import numpy as np

FS = 48000.0


def amplitude(y: np.ndarray, f: float, skip: int = 0) -> float:
    """The amplitude of the component of y at exactly f Hz: a Hann-windowed single-bin projection. A neighbour 100 Hz away, in a window of 5,000 samples
    or more, is more than 90 dB down."""
    y = np.asarray(y, dtype=np.float64)[skip:]
    n = np.arange(len(y))
    w = np.hanning(len(y))
    return float(2 * abs(np.sum(w * y * np.exp(-2j * np.pi * f * n / FS))) / np.sum(w))


def sine_blocks(f: float, blocks: int, amp: float = 0.5) -> np.ndarray:
    """A sine as float32 samples, blocks * 32 of them."""
    n = np.arange(blocks * 32)
    return (amp * np.sin(2 * np.pi * f * n / FS)).astype(np.float32)
