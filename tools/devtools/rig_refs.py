"""Reference tables of the sound checks (docs/TESTING.md "Sound checks"): the output of a module's float32 Python model as little-endian float32 words,
the data that the old Python tests compared the routine with.

    uv run python -m tools.devtools.rig_refs tb303|pluck [--check]

Writes `modules/<kind>/<name>/tests/rig/ref/*.f32`. With --check it only compares the files with what the model gives now. The tables are the pin
once the model is retired; changing one is a re-pin (review as for a PCM pin of tests/scenarios).
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[2]
sys.path[:0] = [str(ROOT), str(ROOT / "tests")]


def write(path: Path, data: np.ndarray, check: bool) -> None:
    raw = np.asarray(data, dtype="<f4").tobytes()
    if check:
        same = path.is_file() and path.read_bytes() == raw
        print(f"{path.relative_to(ROOT)}: {'same' if same else 'DIFFERENT'}")
        if not same:
            raise SystemExit(1)
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(raw)
    print(f"{path.relative_to(ROOT)}: {len(raw) // 4} words")


def tb303(check: bool) -> None:
    mod = ROOT / "modules/machines/tb303/tests"
    sys.path.insert(0, str(mod))
    import tb303_ref  # type: ignore[import-not-found]
    import test_tb303 as t  # type: ignore[import-not-found]

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


def pluck(check: bool) -> None:
    """`pluck_rig.model_blocks` for the cases of test_pluck.py and test_pluck_range.py; the toml holds the tolerance 2e-5 * max(1, peak)."""
    mod = ROOT / "modules/machines/pluck/tests"
    sys.path.insert(0, str(mod))
    from pluck_rig import model_blocks  # type: ignore[import-not-found]

    out = mod / "rig/ref"
    for name, (n, kw) in PLUCK_CASES.items():
        want = model_blocks(n, **kw)
        write(out / f"model-{name}.f32", want, check)
        print(
            f"  atol {2e-5 * max(1.0, float(np.max(np.abs(want)))):.9g}, model peak {float(np.max(np.abs(want))):.6g}"
        )


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


def freqshift(check: bool) -> None:
    """Inputs of test_freqshift.py (numpy generators, which TypeScript cannot draw) and the output of `freqshift_ref.Shifter` for them."""
    mod = ROOT / "modules/fx/freqshift/tests"
    sys.path.insert(0, str(mod))
    import freqshift_ref as r  # type: ignore[import-not-found]
    import test_freqshift as t  # type: ignore[import-not-found]

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


def main() -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument("module", choices=["tb303", "pluck", "freqshift"])
    ap.add_argument("--check", action="store_true")
    a = ap.parse_args()
    {"tb303": tb303, "pluck": pluck, "freqshift": freqshift}[a.module](a.check)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
