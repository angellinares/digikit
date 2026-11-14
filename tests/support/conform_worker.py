"""Runs the conformance cases of one machine module on one device and prints the result as JSON.

    PYTHONPATH=<repo>:<repo>/tests python -m support.conform_worker MODULE DEVICE [--group G ...] [--seed N]

tests/modules/test_machine_conformance.py starts one worker per (module, device), under PyPy when uv can run it (the concrete
core is plain Python: PyPy is 3 to 4 times faster, which keeps a machine under a minute) and under the test's own Python
otherwise. The cases come from tools/patchkit/pk_conform.py; this file runs them on tests/support/machine_rig.py and checks
the invariants of every block:

  returned   the call came back to the caller within the rig's step limit
  context    the context registers (M5 ... MODE1) are as they went in
  finite     every output sample is finite (the output block is NaN before the call: a sample the routine did not write fails)
  bound      |sample| <= LEVEL_BOUND
  guard      the runtime guard of core/dsp-voice did not step in (DT2: its fault count stays 0; a guarded block is zeroed, so
             finite and bound alone would pass a broken routine)
  writes     the module's code stored only to the output block, its own voice state, its own data and its stack
             (tests/support/machine_rig.py `_allowed`); the "writes" group also runs two voices at once
  budget     instructions of the block <= the voice-block budget; the block of a trigger (DN2: and the one after the edge)
             <= the note-on budget (pk_conform.budget)

A case stops at its first failure; the message names module, device, case, block, parameter words, note, velocity and the
first bad sample, so the failure can be reproduced from it alone."""

from __future__ import annotations

import argparse
import json
import math
import sys
import time

from support import sample_pool
from support.machine_rig import MachineRig, nw
from tools.patchkit import pk_conform


def words_at(case: pk_conform.Case, k: int):
    return case.words[k] if len(case.words) > 1 else case.words[0]


def describe(case: pk_conform.Case, k: int, words: dict[str, int], vel: int) -> str:
    ws = " ".join(f"{pid}={w:#x}" for pid, w in words.items()) or "(no parameters)"
    return f"case {case.group}/{case.name} block {k}: note {case.note} velocity {vel} words [{ws}]"


def publish_sample(
    rig: MachineRig, sp: pk_conform.MachineSpec, case: pk_conform.Case
) -> None:
    """The "sample-pool" setup: the PCM planes of the case's sample (a fixed seed) and its record at pool index 0. The case's words select index 0."""
    assert case.sample is not None and sp.sample_records
    frames, rate, channels = case.sample
    left = sample_pool.noise(1, frames)
    right = left if channels == 1 else sample_pool.noise(2, frames)
    sample_pool.write_planes(rig.sess.poke, left, right)
    sample_pool.publish(
        rig.sess.poke,
        nw(rig.built.data[sp.sample_records]),
        0,
        frames,
        rate,
        channels,
    )


def run_case(
    sp: pk_conform.MachineSpec, case: pk_conform.Case, bud: pk_conform.Budget, seed: int
) -> dict:
    t0 = time.time()
    rig = MachineRig(sp.module, sp.device, seed=seed, record_stores=True)
    if case.setup == "sample-pool":
        publish_sample(rig, sp, case)
    rig.set_note(case.note, case.vel)
    words: dict[str, int] = {}
    vel = case.vel
    worst_steady = worst_noteon = 0
    trig = set(case.trig)
    noteon_blocks = set(trig) | ({k + 1 for k in trig} if sp.device == "dn2" else set())
    failure = None
    k = 0
    for k in range(case.blocks):
        words = dict(words_at(case, k))
        rig.set_params(words)
        if k in trig:
            rig.trigger()
        b = rig.block()
        where = describe(case, k, words, vel)
        if not b.returned:
            h = b.halt
            failure = f"{where}: the call did not return to the caller within {rig.step_limit} steps (halt {h.get('reason')} at {b.out.get('final_pc_sw', 0):#x}, {b.instructions} instructions)"
        elif b.faults:
            failure = f"{where}: the runtime guard stepped in ({b.faults} fault(s) counted: the routine's output was not finite or above {pk_conform.LEVEL_BOUND}, and the guard zeroed the block)"
        elif b.foreign:
            failure = f"{where}: the module wrote memory it does not own ({'; '.join(b.foreign)})"
        elif b.context_bad:
            failure = f"{where}: the context registers {','.join(b.context_bad)} did not come back as they went in (in {', '.join(f'{n}={rig.last_ctx[n]:#x}' for n in b.context_bad)})"
        else:
            bad = next(
                (i for i, x in enumerate(b.samples) if not math.isfinite(x)), None
            )
            if bad is not None:
                failure = f"{where}: output sample {bad} of 32 is {b.samples[bad]!r} (not finite; the output block is NaN before the call, so this sample may be one the routine did not write)"
            else:
                big = next(
                    (
                        i
                        for i, x in enumerate(b.samples)
                        if abs(x) > pk_conform.LEVEL_BOUND
                    ),
                    None,
                )
                if big is not None:
                    failure = f"{where}: output sample {big} is {b.samples[big]!r}, above the level bound {pk_conform.LEVEL_BOUND}"
        if failure is None:
            if k in noteon_blocks:
                worst_noteon = max(worst_noteon, b.instructions)
                if b.instructions > bud.note_on_block:
                    failure = f"{where}: {b.instructions} instructions in a note-on block, budget {bud.note_on_block}"
            else:
                worst_steady = max(worst_steady, b.instructions)
                if b.instructions > bud.voice_block:
                    failure = f"{where}: {b.instructions} instructions in a block, budget {bud.voice_block} per voice"
        if failure:
            break
    return {
        "group": case.group,
        "name": case.name,
        "blocks": k + 1,
        "steady": worst_steady,
        "noteon": worst_noteon,
        "failure": f"{sp.label} {failure}" if failure else None,
        "seconds": round(time.time() - t0, 2),
    }


POLY_BLOCKS = 12


def run_poly(sp: pk_conform.MachineSpec, notes: tuple[int, int], seed: int) -> dict:
    """Two voices on one core, notes `notes` (voice 1 starts four blocks late), all parameters at their defaults, then at their
    maxima from block 6, then at their minima from block 9. A store of one voice into the other's state, or past either, fails."""
    t0 = time.time()
    rig = MachineRig(sp.module, sp.device, seed=seed, record_stores=True)
    rig.select(0)
    rig.select(1)
    failure = None
    k = 0
    for k in range(POLY_BLOCKS):
        level = "hi" if 6 <= k < 9 else "lo" if k >= 9 else None
        for t in (0, 1):
            rig.use(t)
            if level:
                rig.set_params(
                    {p.id: p.hi if level == "hi" else p.lo for p in sp.params}
                )
            if k == 4 * t:
                rig.set_note(notes[t], 100)
                rig.trigger()
            if k < 4 * t:
                continue  # voice 1 has not started
            b = rig.block()
            if not b.returned or b.foreign:
                why = "; ".join(b.foreign) or "the call did not return"
                failure = f"{sp.label} case writes/two-voices/notes{notes[0]},{notes[1]} block {k} voice {t}: the module wrote memory it does not own ({why})"
                break
        if failure:
            break
    return {
        "group": "writes",
        "name": f"two-voices/notes{notes[0]},{notes[1]}",
        "blocks": k + 1,
        "steady": 0,
        "noteon": 0,
        "failure": failure,
        "seconds": round(time.time() - t0, 2),
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument("module")
    ap.add_argument("device")
    ap.add_argument("--group", action="append", default=[])
    ap.add_argument("--seed", type=int, default=0xD1617)
    a = ap.parse_args()
    sp = pk_conform.spec(a.module, a.device)
    if sp is None:
        print(
            json.dumps(
                {"error": f"{a.module} is no machine with a DSP object for {a.device}"}
            )
        )
        return 2
    bud = pk_conform.budget(a.device)
    t0 = time.time()
    MachineRig(a.module, a.device)  # builds (assembles, links) once; the cases reuse it
    built = time.time() - t0
    results = []
    for case in pk_conform.cases(sp):
        if a.group and case.group not in a.group:
            continue
        results.append(run_case(sp, case, bud, a.seed))
    if not a.group or "writes" in a.group:
        for notes in ((24, 60), (60, 100), (100, 127)):
            results.append(run_poly(sp, notes, a.seed))
    out = {
        "label": sp.label,
        "python": sys.version.split()[0] + (" pypy" if "PyPy" in sys.version else ""),
        "build_seconds": round(built, 1),
        "budget": {
            "voice_block": bud.voice_block,
            "note_on_block": bud.note_on_block,
            "headroom": bud.headroom,
        },
        "params": [p.id for p in sp.params],
        "cases": results,
    }
    print(json.dumps(out))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
