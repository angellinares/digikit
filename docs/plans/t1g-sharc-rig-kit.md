# T1g SHARC-only rig: the Vitest kit

Status: slice done (`packages/sharc-rig`, [TESTING.md](../TESTING.md) "SHARC-only rig"); phases 1 to 3 done (`packages/sharc-rig-kit`,
[TESTING.md](../TESTING.md) "The Vitest kit"): the machine and effect conformance runs on the rig with the same case lists, pass sets
and recorded costs as the Python suites. This plan is the rest of T1g in [test-architecture.md](test-architecture.md).

## What the slice proved

- The harness runs the loaded code on the generated native core and gives the same output words (bit for bit), the same
  instruction counts, the same store bytes and the same foreign-store texts as the Python rigs. Cases: machines `test/sine`,
  `fadedlimes/analog` (DT2, DN2), `tom/tb303`, `digikit/pluck`, the FX `digikit/freqshift`, and the negative fixture
  `test/dyn-guard-nan#overrun`, all with hostile contexts.
- On 300 to 500 block cases it is 45 to 110 times faster than `CallSession` (process start and the 64k setup pokes included).
- The request is JSON: code segments, DM pokes, calls with registers, reads and `watch` pc ranges. It knows nothing of
  machines, FX or devices. Everything with module meaning stays in the caller, so the kit is a port of the Python rig logic.

## Payoff and confidence

Payoff: the Python conformance suites (a worker per module and device, PyPy when available) and the module tests that
run the rig become Vitest files on a native core. A 500 block case drops from about 20 s to under 1 s. Python test
infrastructure outside the RE tooling goes away (Python rule). Confidence: high for the machine and FX rigs (the port is
mechanical and the slice parity method checks it). Medium for the sound checks (new format; each module's pins must be read
and re-expressed). Recommend going on with phases 1 to 4; phase 5 (adapter tests) only after they are read for what they pin.

## Layout

New project `packages/sharc-rig-kit` (`@digi/sharc-rig-kit`, TypeScript, Vitest `test-host`):

```text
src/rig.ts          run(request) -> response: spawns the sharc-rig binary (path from dist/bin/sharc-rig); types of the JSON
src/toolchain.ts    patchkit sharc inc|cond|asm|link from TypeScript (port of tools/patchkit/pk_sharc.py), object cache in out/cache/rig
src/place.ts        sections to free L2 and code to 0xB90000 (port of machine_rig._build and fx_rig._build); returns symbols and inits
src/context.ts      dsp_voice_rig constants, FRIENDLY and the hostile context draw
src/machine.ts      MachineRig: frame words, note, trigger, release, block (DT2 and DN2), foreign stores, odd voice
src/fx.ts           FxRig: rows, master and return buses, send hooks (DT2 and DN2 slot buses, map, fade flags)
src/spec.ts         port of pk_conform spec(): module.toml and devices/*.toml to sources, entry, parameters, frame
src/cases.ts        port of pk_conform cases() and fx_cases()
src/checks.ts       the rules of the conformance workers, and the sound-check library (below)
src/report.ts       writes out/runs/conformance/<id>@<dev>.json in today's shape
src/samplepool.ts   port of sample_pool.py: planes, pool record, ready last
tests/              kit's own host tests (fake rig response)
```

A module's own sound checks live with the module: `modules/<kind>/<name>/tests/rig/*.toml` plus one generic Vitest entry
`tests/rig.test.ts` per module (a one-line call into the kit, so `tools/nx/modules.mjs` infers the target from the files).

## Assembling, linking and placing from TypeScript

The Python code already does it through the patchkit binary (`patchkit sharc inc | cond | asm | link`); TypeScript calls the same
binary with `execFile` (the existing `tests/regression/runtime/patchkit-exec.ts` shape; `DIGI_PATCHKIT_LIVE` rules apply).
`toolchain.ts` keeps `expand_includes`, `assemble` and `link` with the same arguments and parses the same JSON. Placement is
plain arithmetic (data sections in 4-byte runs from 0x200A0000, code objects one after the other from 0xB90000 rounded to 0x100,
externs of the adapter) and is ported function by function. The object cache keys on the source hash and include
closure (the build already writes the same hash), so a repeated run skips `asm`. Check: for each case of the slice parity
set, the kit's request equals the one recorded from the Python rig (code hex, pokes), run once as a throwaway proof.

## Machine and FX cases, contexts

- `cases.ts` ports `cases()` and `fx_cases()` unchanged (limits, corners, modulation walk, long, retrigger; signals, params,
  modulation, switching, long). The random walks use `PyRandom` (`prng.ts`), the Mersenne Twister of Python's `random.Random` with its
  seeding, `random`, `uniform`, `choice` and `randrange`: the case values are the old ones, so the worst steady block of a module is
  the recorded `dsp_cost_voice` and the Python and Rust runs are comparable block by block.
- `context.ts` ports the context tuples; the draw uses `PyRandom` and one seed per rig as today, so the contexts are the old ones too.
- `machine.ts` reads like `machine_rig.py`: `setParams`, `setNote`, `trigger`, `release`, `block(ctx)`, `use`, `select`, `oddBlock`.
  Each method appends pokes and one call to a request; `block` sends the pending ops and returns the 32 samples (floats from the
  words), instructions, halt, context check, fault word and foreign stores (`watch` = module code ranges; the allowed regions
  logic of `_allowed` and `_names` is ported). NaN preloading of output and `.PKBSS` is kept.
- The rules of the workers (returned, context, finite, bound, guard, writes, budget) become `checks.ts`; budgets come from
  `patchkit budget` (Rust, JSON).
- The conformance run is one Vitest file in the kit project parameterised over the module list (as `DIGI_CONFORM_MODULES`
  today) and one worker pool; reports have today's JSON shape so `module:check` and the cost writer read them unchanged.

## Sound checks (`tests/rig/*.toml`)

There are none today; the module pins are Python (`test_tb303.py` with `tb303_ref.py`, `test_pluck*.py`, `test_freqshift*.py`,
`test_shot_playback.py`, `tests/patchkit/test_sine.py`). Format:

```toml
[[case]]
name = "c4 sounds"
device = ["dt2", "dn2"]
note = 60            # velocity, params (by manifest id), blocks, trigger and release blocks
params = { decay = 0x4000 }
blocks = 200
[[case.check]]
kind = "rms"         # also: peak, pitch-hz, dc, silent-after, decay-time, same-as (reference words hash), no-clip
min = 0.05
max = 0.5
```

`checks.ts` holds the analysis (RMS, peak, DC, autocorrelation pitch, envelope decay, silence) in plain TypeScript, tested by
host tests on synthetic signals. A pinned reference (`tb303_ref.py` style) becomes a words hash or a short float table in a
file next to the toml, written by a `--update` run of the kit and reviewed in the diff (PCM-pin rule: Em approves changes
to pins in `tests/scenarios`; module pins follow the same review).

## dsp_cost_voice writer

`pk_modcheck.measured_costs` reads the report JSON and `write_uses` edits `[uses.<device>]` in `module.toml`.
The reports keep their shape, so only the writer moves: add `patchkit uses set --id ID --device D --dsp-cost-voice N`
(Rust, with the TOML editing `write_uses` does, comments kept) and let `module:check` call the kit, then the writer.
`pk_modcheck` stays for size, fit and the module's own tests until those move (not in T1g).

## Sample pool and DN2 stereo and send FX

- Sample pool (`shot`): `samplepool.ts` is a port of `sample_pool.py` (about 60 lines of pokes; done in phase 3, as the conformance of
  `shot` needs it). The conformance setup `sample-pool` calls it before the first block. `test_shot_playback.py` becomes a rig test with sound checks.
- DN2 machine stereo (`stereo.ts` and `stereo-stages.ts`; the Python rig files are retired) and the send FX hooks (`fxsend.s`, `fxsend-dn2.s`: track
  buses, slot map, fade flags, return bus): the request format needs no change. They are extra methods of `machine.ts` and
  `fx.ts`. They serve the adapter tests, not the module conformance, so they come in phase 5.

## Nx targets and inputs

- `sharc-rig:build` (cache false, outputs `dist/bin/sharc-rig` and `dist/bytes/sharc-rig.sha256`): `rust-env.mjs --gen`, so the
  binary exists beside the other rust-env builds; its hash covers the generated core.
- `@digi/sharc-rig-kit:test-host` (cache true): kit source, its tests, the binary hash, patchkit binary
  (`patchkit:cli`), `sdk/**` includes through the module inputs. No daemon, no firmware, no emulator.
- Per module `<id>:test-rig` (inferred by `tools/nx/modules.mjs` when `tests/rig/*.toml` exists): inputs are the module's
  production files, its `tests/rig/**`, the kit source, the two binary hashes, `modules/core/adapters/dsp-voice` and `fx`
  sources (the wrapper is part of the run). It has no `dependsOn` on `digi-control:daemons`, so a daemon key change does
  not select it, and a rig edit does not select device tests (checked: `show target inputs digi-control:daemons --check`).
- `@digi/sharc-rig-kit:test-conformance-rig` (cache true, in the merge gate) runs all machines and effects on the rig; it replaces the Python
  pytest files in `affected` when they are retired (phase 6). Its inputs are the module manifests, DSP sources (`*.s`, `*.inc`) and
  `build/gen`, the two adapters, the SDK, the device profiles and the hashes of the two binaries (`dependentTasksOutputFiles`).
- The slice's root `Cargo.toml` and `Cargo.lock` edit changed the daemon key once; later rig edits do not.

## Retirement list and proofs

Deletion needs an independent adversarial review (rule); each line names its proof.

| Python | Replaced by | Proof before deletion |
| --- | --- | --- |
| `tests/modules/test_machine_conformance.py`, `tests/support/conform_worker.py` | kit conformance run | Same case list per (module, device); same pass set; the four negative fixtures fail with the same messages (`dyn-dsp-hang`, `dyn-guard-nan`, `#overrun`, FX `fx-nan`); the worst steady block per (module, device) equals `dsp_cost_voice` in `module.toml` (zero diff after a writer run) |
| `tests/modules/test_fx_conformance.py`, `tests/support/fx_conform_worker.py` | same | same, with `dsp_cost_fx` |
| `tools/patchkit/pk_conform.py` (spec, cases, budget glue) | `spec.ts`, `cases.ts`, `patchkit budget` | the two lines above; `tools/devtools/py_select.py` entries removed in the same PR |
| `tests/support/machine_rig.py`, `fx_rig.py`, `dsp_voice_rig.py`, `sample_pool.py` | `machine.ts`, `fx.ts`, `context.ts`, `samplepool.ts` | no importer left (grep); the parity scripts of the slice (Python request recorded vs kit request, outputs compared) pass for every case group once |
| `modules/machines/tb303/tests/test_tb303.py`, `tb303_ref.py` | `tests/rig/*.toml` | each assertion maps to a check; a mutation of the DSP (one constant) fails both before and the toml after |
| `modules/machines/pluck/tests/test_pluck*.py`, `pluck_ref.py`, `pluck_rig.py` | same | same |
| `modules/fx/freqshift/tests/test_freqshift*.py`, `freqshift_ref.py`, `freqshift_demo.py`, `freqshift_long.py` | same (the device test `freqshift.device.test.ts` stays) | same |
| `modules/machines/shot/tests/test_shot_playback.py` | rig test with `samplepool.ts` | same, and the PCM pins it holds are re-pinned with Em's approval |
| `tests/patchkit/test_sine.py` | kit smoke over `test/sine` | parity with the recorded outputs |
| Adapter tests on the rigs (`modules/core/adapters/dsp-voice/tests/test_dsp_*.py`, `fx/tests/test_fx*.py`, `dsp-stereo-proof`) | phase 5 | read each for the property it pins first; port or delete with review |
| `pk_sharc.CallSession`, `run_call` | stays | used by kernel tests in `tests/patchkit/test_dsp_*.py` and the RE tools; not part of T1g |

## Phases and estimate (agent hours, one agent per phase, host-only)

1. Done. Kit skeleton, `toolchain.ts`, `place.ts`, `machine.ts`, `context.ts`, parity with the Python request on the slice cases, `sharc-rig:build` target: 6 h.
   The rig got `--serve` (a core that keeps its state between lines) and the op `pokem` (masked poke), so a rig runs block by block as the Python rigs do.
2. Done. `fx.ts` (master chain, no send) and parity: 3 h.
3. Done. `spec.ts`, `cases.ts`, `checks.ts`, `report.ts`, negative fixtures, conformance Nx targets, the `patchkit uses` writer: 7 h.
4. Done (tb303, pluck, freqshift, shot, sine; mapping and mutation proofs in [ledgers/t1g-module-tests.md](ledgers/t1g-module-tests.md)). Sound-check format, analysis library, port of the module tests (tb303, pluck, freqshift, shot with sample pool, sine): 10 h. Largest risk is reading the pins; the reference files may need re-pinning.
5. Adapter and stereo rigs (`dn2_stereo_*`, send FX, the dsp-voice and fx adapter tests): 12 h. Optional.
6. Retirement PRs with reviews and the batch `test:full`: 4 h. Prepared for review in [ledgers/t1g-retirement.md](ledgers/t1g-retirement.md) (Python suites, rigs that stay and why, module tests); the batch `test:full` is open.

Phases 1 to 4 and 6: about 30 h; with phase 5: about 42 h. Phases 1 to 3 give the conformance replacement (about 16 h) and
can merge before phase 4.
