"""The cases and the float32 model run of tom/tb303: the source of the reference tables of `tests/rig/tb303.toml`
(`uv run python -m tools.devtools.rig_refs modules/machines/tb303`, see rig_refs_gen.py). The routine itself is checked on the SHARC-only rig."""

import numpy as np
import tb303_ref


def word(x: float) -> int:
    return round(x * 0x7F00)


def pitch(note: float, tune: float = 64.0) -> int:
    return int(note * 256) + int(tune * 256) - 0x4000


def model_run(
    m: tb303_ref.Tb303,
    note: float,
    vel: int,
    params: list[int],
    n: int,
    tune: float = 64.0,
) -> np.ndarray:
    m.note_on(vel << 8, params)
    return np.concatenate([m.block(pitch(note, tune), params) for _ in range(n)])


CASES = {
    "saw": (36, 80, [0, word(0.3), word(0.7), word(0.6), word(0.4), 0x7F00]),
    "square-accent": (
        48,
        120,
        [0x100, word(0.5), word(0.9), word(1.0), word(0.0), word(0.5)],
    ),
    "closed-no-env": (45, 64, [0, 0, 0, 0, word(1.0), 0]),
    "open-tuned": (60, 101, [0, word(1.0), word(0.5), word(0.3), word(0.8), word(0.3)]),
    "high": (84, 127, [0x100, word(0.8), word(1.0), word(0.8), word(0.2), 0x7F00]),
}
