"""The float32 model of digikit/pluck as a block generator: the source of the reference tables of `tests/rig/pluck.toml`
(`uv run python -m tools.devtools.rig_refs modules/machines/pluck`, see rig_refs_gen.py)."""

import numpy as np
import pluck_ref


def model_blocks(
    n: int, note=60.0, tune=64.0, vel=100, decay=0x4000, pick=0, bright=0x4000, color=0
) -> np.ndarray:
    q = pluck_ref.Params(
        int(note * 256), int(tune * 256), decay, pick, bright, color, vel << 8
    )
    s = pluck_ref.Pluck()
    s.note_on(q)
    return np.concatenate([s.block(q) for _ in range(n)])
