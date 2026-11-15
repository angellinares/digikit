"""Reference tables of the sound checks (docs/TESTING.md "Sound checks"): the output of a module's float32 Python model as little-endian float32 words,
the data that the old Python tests compared the routine with.

    uv run python -m tools.devtools.rig_refs MODULE_DIR [--check]

Runs `generate(check)` of `MODULE_DIR/tests/rig_refs_gen.py`, which writes `MODULE_DIR/tests/rig/ref/*.f32` (and, for a module with many cases, the toml).
With --check it only compares the files with what the model gives now. The tables are the pin once the model is retired; changing one is a re-pin
(review as for a PCM pin of tests/scenarios).
"""

from __future__ import annotations

import argparse
import importlib.util
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


def main() -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument("module_dir", type=Path)
    ap.add_argument("--check", action="store_true")
    a = ap.parse_args()
    gen = (
        ROOT / a.module_dir if not a.module_dir.is_absolute() else a.module_dir
    ) / "tests/rig_refs_gen.py"
    spec = importlib.util.spec_from_file_location("rig_refs_gen", gen)
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    mod.generate(a.check)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
