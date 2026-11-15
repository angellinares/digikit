"""Reference tables of the sound checks (docs/TESTING.md "Sound checks"): the output of a module's float32 Python model as little-endian float32 words,
the data that the old Python tests compared the routine with.

    uv run python -m tools.devtools.rig_refs tb303|pluck|freqshift [--check]

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


def main() -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument("module", choices=["tb303"])
    ap.add_argument("--check", action="store_true")
    a = ap.parse_args()
    {"tb303": tb303}[a.module](a.check)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
