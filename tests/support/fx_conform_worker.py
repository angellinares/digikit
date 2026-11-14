"""Runs the conformance cases of one effect module and prints the result as JSON.

    PYTHONPATH=<repo>:<repo>/tests python -m support.fx_conform_worker MODULE [--group G ...] [--seed N]

tests/modules/test_fx_conformance.py starts one worker per module (PyPy when uv can run it). The cases come from tools/patchkit/pk_conform.py
(`fx_cases`); each runs on tests/support/fx_rig.py, the effect behind core/fx on the concrete SHARC+ core with a hostile context, and every
block is checked:

  returned   the hook came back to the stock function's second instruction within the rig's step limit
  context    the context registers (M5 ... MODE1) and the registers the stock function expects (R/I/M, B3) are as they went in
  guard      core/fx did not refuse the block (the row's fault count stays 0): the output was finite and below 8.0. A refused block plays dry,
             so the master alone would pass a broken effect
  writes     the module's code stored only to the output block, its own data and bss (its state included) and its stack
             (the P4a write-set check; tests/support/fx_rig.py `foreign_stores`)
  bounded    the master after the hook is finite and within the level bound
  budget     instructions of the call <= the effect's budget (pk_conform.fx_budget) + the adapter's own (FX_ADAPTER_MAX)

A case stops at its first failure; the message names module, case, block, parameter words and the first bad sample."""

from __future__ import annotations

import argparse
import json
import math
import random
import sys
import time

from support.fx_rig import MASTER, P0, FxRig, Row, f2w, w2f  # noqa: F401
from tools.patchkit import pk_conform

NFAULT_WORD = 17


def f32(x: float) -> float:
    return w2f(f2w(x))


def signal_block(name: str, k: int, rng: random.Random) -> list[float]:
    """64 floats (32 left, 32 right) of block k, within +-1.0 like the clipped master."""
    out = []
    for ch in range(2):
        for i in range(32):
            n = k * 32 + i
            if name == "silence":
                x = 0.0
            elif name == "dc":
                x = 0.5 if ch == 0 else -0.25
            elif name == "square":
                x = 1.0 if (n // 5) % 2 == ch else -1.0
            elif name == "noise":
                x = rng.uniform(-1.0, 1.0)
            elif name == "impulse":
                x = 1.0 if (n == 0 and ch == 0) or (n == 16 and ch == 1) else 0.0
            elif name == "sine":
                x = 0.9 * math.sin(2 * math.pi * (220 + 110 * ch) * n / 48000)
            elif name == "ramp":
                x = ((n % 64) / 32.0 - 1.0) * (1 if ch == 0 else -1)
            else:
                raise ValueError(name)
            out.append(f32(x))
    return out


def describe(sp, case, k: int, words) -> str:
    ws = " ".join(f"p{j}={w:#x}" for j, w in enumerate(words)) or "(no parameters)"
    return f"case {case.group}/{case.name} block {k}: signal {case.signal} words [{ws}]"


def run_case(sp, case, bud: int, desc: tuple[int, ...], seed: int) -> dict:
    t0 = time.time()
    rig = FxRig(
        [Row(sp.process, sp.state, desc=sp.desc)],
        sources=sp.sources,
        seed=seed,
        record_stores=True,
    )
    rng = random.Random(seed)
    stack = desc[5]
    worst = 0
    failure = None
    k = 0
    on = True
    for k in range(case.blocks):
        raw = case.words[k] if len(case.words) > 1 else case.words[0]
        words = tuple(pk_conform.fx_word(sp, j, w) for j, w in enumerate(raw))
        for j, w in enumerate(words):
            rig.set_row_word(0, P0 + j, w)
        if case.toggle and k and k % case.toggle == 0:
            on = not on
            rig.set_enabled(0, on)
        master = signal_block(case.signal, k, rng)
        before = rig.row_word(0, NFAULT_WORD)
        b = rig.block(master)
        where = describe(sp, case, k, words)
        if not b.returned:
            failure = f"{where}: the hook did not return to the stock function within {rig.step_limit} steps (halt {b.halt.get('reason')} at {b.raw.get('final_pc_sw', 0):#x}, {b.instructions} instructions)"
        elif rig.row_word(0, NFAULT_WORD) != before:
            out = rig.scratch()
            bad = next(
                (
                    i
                    for i, x in enumerate(out)
                    if not math.isfinite(x) or abs(x) > pk_conform.LEVEL_BOUND
                ),
                None,
            )
            what = (
                f"output sample {bad} of 64 is {out[bad]!r}"
                if bad is not None
                else "the process routine returned a negative number"
            )
            failure = f"{where}: the adapter's guard stepped in ({what}; the block played dry)"
        elif b.ctx_bad or b.regs_bad:
            failure = f"{where}: registers {','.join(b.ctx_bad + b.regs_bad)} did not come back as they went in"
        else:
            foreign = rig.foreign_stores(stack)
            if foreign:
                failure = f"{where}: the module wrote memory it does not own ({'; '.join(foreign)})"
            else:
                bad = next(
                    (
                        i
                        for i, x in enumerate(b.master)
                        if not math.isfinite(x) or abs(x) > pk_conform.LEVEL_BOUND
                    ),
                    None,
                )
                if bad is not None:
                    failure = f"{where}: master sample {bad} is {b.master[bad]!r} after the hook"
        if failure is None:
            worst = max(worst, b.instructions)
            if b.instructions > bud + pk_conform.FX_ADAPTER_MAX:
                failure = f"{where}: {b.instructions} instructions in a block, budget {bud} for the effect and {pk_conform.FX_ADAPTER_MAX} for core/fx"
        if failure:
            break
    return {
        "group": case.group,
        "name": case.name,
        "blocks": k + 1,
        "steady": worst,
        "noteon": 0,
        "failure": f"{sp.label} {failure}" if failure else None,
        "seconds": round(time.time() - t0, 2),
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument("module")
    ap.add_argument("--group", action="append", default=[])
    ap.add_argument("--seed", type=int, default=0xD1617)
    a = ap.parse_args()
    sp = pk_conform.fx_spec(a.module)
    if sp is None:
        print(json.dumps({"error": f"{a.module} is no effect with a DSP object"}))
        return 2
    t0 = time.time()
    probe = FxRig([Row(sp.process, sp.state, desc=sp.desc)], sources=sp.sources)
    built = time.time() - t0
    desc = probe.desc_words(sp.desc)
    problems = []
    if desc[0] != 0x444B4658:
        problems.append("the description has no DK_FX_MAGIC")
    if desc[1] != 1:
        problems.append(f"contract {desc[1]} is not DK_FX_V1")
    if desc[2] > 8:
        problems.append(f"{desc[2]} parameters, at most 8")
    if desc[3] % 4:
        problems.append(f"state_bytes {desc[3]} is not a multiple of 4")
    bud = pk_conform.fx_budget()
    results = []
    if problems:
        results.append(
            {
                "group": "signals",
                "name": "description",
                "blocks": 0,
                "steady": 0,
                "noteon": 0,
                "failure": f"{sp.label} {'; '.join(problems)}",
                "seconds": 0,
            }
        )
    else:
        for case in pk_conform.fx_cases(desc[2]):
            if a.group and case.group not in a.group:
                continue
            results.append(run_case(sp, case, bud, desc, a.seed))
    print(
        json.dumps(
            {
                "label": sp.label,
                "python": sys.version.split()[0]
                + (" pypy" if "PyPy" in sys.version else ""),
                "build_seconds": round(built, 1),
                "budget": {"effect": bud, "adapter": pk_conform.FX_ADAPTER_MAX},
                "n_params": desc[2],
                "cases": results,
            }
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
