"""Bounded native SHARC startup from loader memory and fresh reference reset.

No captured running state, Python instruction fallback or provisional forms.
Use --compare to check each instruction against the Python reference. A
completed budget is an execution probe, not proof of completed DSP boot.
The flattened loader image does not reproduce the ROM's earlier INIT callback
or its application handoff context.

    uv run python tools/sharc_reset_check.py dt2-1.16 --lib LIB --report OUT
    uv run python tools/sharc_reset_check.py dt2-1.16 --lib LIB --compare
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import time

import sharc_diff as sd
import sharc_run as sr
import sharc_transpile_run as nr
import sharcldr


def bounded_steps(text: str) -> int:
    value = int(text, 0)
    if not 1 <= value <= 1_000_000:
        raise argparse.ArgumentTypeError("steps must be between 1 and 1000000")
    return value


def probe(image: str, library: str, steps: int, compare: bool = False) -> dict:
    data = sr._load_image_memory(image)
    entries = sharcldr.entry_points(data.blocks)
    if not entries:
        raise ValueError("loader has no FIRST entry")
    entry = entries[-1]
    core = nr.NativeCore(nr.pack_image(data), library)
    build = core.info()
    image_hash = hashlib.sha256(data.data).hexdigest()
    if build["image_sha256"] != image_hash:
        raise ValueError("native library's instruction image does not match the loader")
    runner = sr.Runner(data, entry)
    nr.to_native(core, runner.state)
    started = time.perf_counter()
    differences: list[str] = []
    if compare:
        reference = sd.PythonEngine(data)
        reference.load_runner(runner)
        result = sd.run_lockstep(reference, core, max_steps=steps, compare_every=1)
        differences = result.diff
        compared = result.steps_agreed
    else:
        core.run(steps)
        compared = None
    stats = core.stats()
    return {
        "image": image,
        "loader_sha256": image_hash,
        "build": build,
        "seed": "fresh sharc_run.make_state concrete reference reset defaults",
        "captured_state": False,
        "python_instruction_fallback": False,
        "provisional_forms": [],
        "initial_pc": entry,
        "final_pc": core.pc(),
        "instruction_budget": steps,
        "instructions_completed": stats["instructions"],
        "instructions_compared": compared,
        "differences": differences,
        "native_halt": core.halt_reason,
        "outcome": "state_divergence"
        if differences
        else "native_halt"
        if core.halted
        else "instruction_budget",
        "execution_seconds": time.perf_counter() - started,
        "stats": stats,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image")
    parser.add_argument("--lib", required=True)
    parser.add_argument("--steps", type=bounded_steps, default=10_000)
    parser.add_argument("--compare", action="store_true")
    parser.add_argument("--report", type=pathlib.Path)
    args = parser.parse_args()
    if args.compare and args.steps > 10_000:
        parser.error("instruction-by-instruction comparison is bounded to 10000 steps")
    try:
        result = probe(args.image, args.lib, args.steps, args.compare)
    except (OSError, ValueError, RuntimeError) as error:
        parser.exit(2, f"sharc-reset-check: {error}\n")
    text = json.dumps(result, indent=2)
    if args.report:
        args.report.write_text(text + "\n")
    print(text)
    return int(result["outcome"] != "instruction_budget")


if __name__ == "__main__":
    raise SystemExit(main())
