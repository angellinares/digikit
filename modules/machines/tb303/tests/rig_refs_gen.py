"""Reference tables of the tb303 sound checks: the output of its float32 Python model, the data that the old Python tests compared the routine with.

    uv run python -m tools.devtools.rig_refs modules/machines/tb303 [--check]

Run through tools/devtools/rig_refs.py. The tables are the pin once the model is retired."""

from __future__ import annotations

import sys
from pathlib import Path

import numpy as np

from tools.devtools.rig_refs import write

HERE = Path(__file__).resolve().parent


def generate(check: bool) -> None:
    mod = HERE
    sys.path.insert(0, str(mod))
    import tb303_cases as t  # type: ignore[import-not-found]
    import tb303_ref  # type: ignore[import-not-found]

    out = mod / "rig/ref"
    for name, (note, vel, params) in t.CASES.items():
        tune = 70.0 if name == "open-tuned" else 64.0
        want = t.model_run(tb303_ref.Tb303(), note, vel, params, 60, tune)
        write(out / f"model-{name}.f32", want, check)
    a = [0, t.word(0.4), t.word(0.6), t.word(0.8), t.word(0.3), 0x7F00]
    b = [0x100, t.word(0.4), t.word(0.6), t.word(0.8), t.word(0.3), 0x7F00]
    for released in (False, True):
        m = tb303_ref.Tb303()
        want = [t.model_run(m, 41, 90, a, 20)]
        if released:
            want.append(
                np.concatenate(
                    [
                        m.block(t.pitch(41), a, True),
                        m.block(t.pitch(41), a),
                        m.block(t.pitch(41), a),
                    ]
                )
            )
        want.append(t.model_run(m, 53, 110, b, 30))
        write(
            out / f"slide-{'released' if released else 'held'}.f32",
            np.concatenate(want),
            check,
        )
