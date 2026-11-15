"""Reference tables of the pluck sound checks: the output of its float32 Python model, the data that the old Python tests compared the routine with.

    uv run python -m tools.devtools.rig_refs modules/machines/pluck [--check]

Run through tools/devtools/rig_refs.py. The tables are the pin once the model is retired."""

from __future__ import annotations

import sys
from pathlib import Path

import numpy as np

from tools.devtools.rig_refs import write

HERE = Path(__file__).resolve().parent


def generate(check: bool) -> None:
    """`pluck_rig.model_blocks` for the cases of tests/rig/pluck.toml; the toml holds the tolerance 2e-5 * max(1, peak)."""
    mod = HERE
    sys.path.insert(0, str(mod))
    from pluck_rig import model_blocks  # type: ignore[import-not-found]

    out = mod / "rig/ref"
    for name, (n, kw) in PLUCK_CASES.items():
        want = model_blocks(n, **kw)
        write(out / f"model-{name}.f32", want, check)
        print(
            f"  atol {2e-5 * max(1.0, float(np.max(np.abs(want)))):.9g}, model peak {float(np.max(np.abs(want))):.6g}"
        )


PLUCK_CASES = {
    "default": (40, {}),
    "low": (
        40,
        {
            "note": 48,
            "decay": 0x7000,
            "pick": 0x2000,
            "bright": 0x2000,
            "color": 0x3000,
        },
    ),
    "high-tuned": (
        40,
        {"note": 84, "tune": 66.0, "vel": 127, "bright": 0x7F00, "color": 0x7F00},
    ),
    "pick-max": (40, {"note": 33, "pick": 0x7F00, "decay": 0x1000, "bright": 0}),
    "note0": (8, {"note": 0, "tune": 0 / 256.0}),
    "low-tune": (8, {"note": 60, "tune": 0x0400 / 256.0}),
    "note100-high-tune": (8, {"note": 100, "tune": 0x7C00 / 256.0}),
    "note127": (8, {"note": 127, "tune": 0x7F00 / 256.0}),
    "note127-max-word": (8, {"note": 127, "tune": 0xFFFF / 256.0}),
}
