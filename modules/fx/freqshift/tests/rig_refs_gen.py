"""Reference tables of the freqshift sound checks: the output of its float32 Python model, the data that the old Python tests compared the routine with.

    uv run python -m tools.devtools.rig_refs modules/fx/freqshift [--check]

Run through tools/devtools/rig_refs.py. The tables are the pin once the model is retired."""

from __future__ import annotations

import sys
from pathlib import Path

import numpy as np

from tools.devtools.rig_refs import write

HERE = Path(__file__).resolve().parent


FREQSHIFT_V = {
    "0": 0.0,
    "1": 1.0,
    "20": 20.0,
    "40": 40.0,
    "neg40": -40.0,
    "63": 63.0,
    "neg64": -64.0,
    "tiny": 2e-6,
}
FREQSHIFT_STEPS = [0.0, 5.0, -20.0, 40.0, 63.0, -64.0, 0.5, -0.5, 33.0, 0.0]


def generate(check: bool) -> None:
    """Inputs of the Python tests (numpy generators, which TypeScript cannot draw) and the output of `freqshift_ref.Shifter` for them."""
    mod = HERE
    sys.path.insert(0, str(mod))
    import freqshift_inputs as t  # type: ignore[import-not-found]
    import freqshift_ref as r  # type: ignore[import-not-found]

    out = mod / "rig/ref"
    for seed, blocks in ((3, 36), (5, 50), (9, 24)):
        rng = np.random.default_rng(seed)
        write(
            out / f"mixed-in-{seed}.f32",
            np.concatenate([t.mixed_input(k, rng) for k in range(blocks)]),
            check,
        )
    rng = np.random.default_rng(3)
    xs = [t.mixed_input(k, rng) for k in range(36)]
    for name, v in FREQSHIFT_V.items():
        model = r.Shifter()
        write(
            out / f"model-v{name}.f32",
            np.concatenate([model.block(x, v) for x in xs]),
            check,
        )
    rng = np.random.default_rng(5)
    model = r.Shifter()
    want = []
    for k in range(len(FREQSHIFT_STEPS) * 5):
        want.append(model.block(t.mixed_input(k, rng), FREQSHIFT_STEPS[k // 5]))
    write(out / "model-moving.f32", np.concatenate(want), check)
    for shift in (100.0, -100.0, 2000.0):
        v = r.v_for_hz(abs(shift)) * (1 if shift > 0 else -1)
        exact = r.target_inc(v) * r.FS / 2**32
        print(f"shift {shift:g}: v = {v!r}, exact Hz = {exact!r}")
    inc = r.target_inc(33.3)
    d = inc >> 5
    print(
        f"v = 33.3: inc {inc}, f_out {1000.0 + inc * r.FS / 2**32!r}, phase word after 1500 blocks {r.wrap32(528 * d + 1499 * 32 * inc) & 0xFFFFFFFF}"
    )
