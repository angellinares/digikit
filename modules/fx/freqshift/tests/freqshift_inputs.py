"""The input block of the frequency shifter reference tables (`tests/rig/freqshift.toml`, `uv run python -m tools.devtools.rig_refs modules/fx/freqshift`):
numpy draws it, which a TypeScript run cannot."""

import numpy as np

FS = 48000.0


def mixed_input(k: int, rng: np.random.Generator) -> np.ndarray:
    """Block k: a sine and noise, a different mix on each channel."""
    n = np.arange(k * 32, k * 32 + 32)
    s = 0.4 * np.sin(2 * np.pi * 440 * n / FS)
    left = s + rng.uniform(-0.3, 0.3, 32)
    right = 0.7 * s + rng.uniform(-0.5, 0.5, 32)
    return np.concatenate([left, right]).astype(np.float32)
