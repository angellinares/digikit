# T1g phase 6: retirement of the Python rigs and tests

One row per retired file, with its replacement, the proof and where the evidence is
([plan](../t1g-sharc-rig-kit.md) "Retirement list and proofs"). The deletions need an independent adversarial review before merge.
Base: main at 7e1f99e9e (PR 510). Evidence runs of 2026-11-16 are in the scratch directory of the run (named below); each command works from a clean checkout.

## Evidence

**Conformance comparison (Python suite against the kit).**

- Python: `uv run python -m pytest tests/modules/test_machine_conformance.py tests/modules/test_fx_conformance.py -rf -q`: 209 passed (302 s).
  Reports in `out/runs/conformance/`.
- Kit: `DIGI_CONFORM_REPORTS=DIR DIGI_JOBS=4 pnpm exec vitest run --config vitest.host.config.ts --project sharc-rig-conformance`: 210 passed (51 s), 30 reports in DIR.
  The extra test is the kit's own (the negative-fixture group split).
- Comparison of the 30 reports (`<id>@<device|fx>.json`): `label`, `budget`, `params` and, for every case, `group`, `name`, `blocks`, `steady`, `noteon`, `failure`
  (only `python`, `runner` and `build_seconds` are left out): **30 of 30 identical, 3293 cases**.
- Observation [R]: in the first full Python run the 22 `sample` cases of `digikit/shot` reported a lower `steady` than the kit (DT2 stereo 2331 against 3209,
  DN2 2658 against 3534). Cause: another lane's report of a shot with the S6a sample kernel (2449 and 2658) was in the shared `out/runs/conformance/`, and
  this run read it. PR 514 fixed it: reports are per worktree and the cost writer checks the worktree. A run of `digikit/shot` alone gave 3325 and 3534, identical to the kit.
- Reproduction. Run the two suites at `7e1f99e9e` (`git worktree add <dir> 7e1f99e9e`) with the first command above, copy `out/runs/conformance/*.json` to `PY_DIR`,
  run the kit command with `DIGI_CONFORM_REPORTS=KIT_DIR`, then compare field by field (`python`, `runner` and `build_seconds` are left out):

  ```sh
  python3 - PY_DIR KIT_DIR <<'PY'
  import json, sys, pathlib
  py, kit = (pathlib.Path(p) for p in sys.argv[1:3])
  skip = {"python", "runner", "build_seconds"}
  bad = 0
  for f in sorted(kit.glob("*.json")):
      a = {k: v for k, v in json.loads((py / f.name).read_text()).items() if k not in skip}
      b = {k: v for k, v in json.loads(f.read_text()).items() if k not in skip}
      if a != b:
          bad += 1
          print("differs:", f.name)
  print(len(list(kit.glob("*.json"))) - bad, "identical,", bad, "different")
  PY
  ```

  The reports of the 2026-11-16 run were in the scratch directory of that session and are not kept; the numbers above come from them.

**Negative fixtures.** Both suites expect every failure of a negative fixture to contain the same text (`NEGATIVE` and `FX_NEGATIVE` in `pk_conform.py` and in
`packages/sharc-rig-kit/src/spec.ts`, equal): `test/dyn-dsp-hang` "did not return", `test/dyn-guard-nan` "guard stepped in",
`test/dyn-guard-nan#overrun` "wrote memory it does not own", FX `test/fx-nan` and `test/dyn-fx-nan` "guard stepped in". Both runs above passed these tests
(`packages/sharc-rig-kit/conformance/machine.test.ts` and `fx.test.ts`).

**Costs.** The writer (`pk_modcheck.write_costs_rust`, that is `patchkit uses set`) ran on the 30 fresh kit reports: no `module.toml` of `digikit/*`, `fadedlimes/*` or
`tom/*` changed (`git status` clean). The fixtures under `tests/fixtures/modules` carry no committed cost; the writer added rows there, which were restored.

**Mutation proofs** of the module tests: [t1g-module-tests.md](t1g-module-tests.md) (the Python test and the kit check both fail on one changed DSP constant).

## Rows

| Retired | Replaced by | Proof |
| --- | --- | --- |
| `tests/modules/test_machine_conformance.py`, `tests/support/conform_worker.py` | `sharc-rig-kit` machine conformance (`spec.ts`, `cases.ts`, `checks.ts`, `conform.ts`; Nx `test-conformance-rig`) | Comparison above; negative fixtures above; costs above |
| `tests/modules/test_fx_conformance.py`, `tests/support/fx_conform_worker.py` | same, effect side (`fx.ts`) | same |
| `tools/patchkit/pk_conform.py`: `Case`, `cases`, `FxCase`, `fx_cases`, `fx_word`, `NEGATIVE*`, `FX_NEGATIVE`, `*_SKIP`, `machine_specs`, `fx_specs`, `fx_budget`, case constants | `cases.ts`, `spec.ts`, `patchkit budget` | the two rows above. The rest of the file stays: `spec`, `fx_spec`, `budget`, `Param`, `Frame`, `MachineSpec`, `DEVICES`, `LEVEL_BOUND`, `ConformError`; importers: `tests/support/machine_rig.py`, `fx_rig.py`, `tools/patchkit/pk_modcheck.py`, `pk_adapter_cost.py`, `tests/patchkit/test_pk_conform.py`, `modules/core/adapters/dsp-voice/tests/test_dsp_voice_stereo*.py`, `test_dsp_voice_input.py`, `test_dsp_guard_contract.py` |
| `pk_modcheck.py`: `--python-conformance`, `plan(..., python_conformance)`, `write_costs`, `CONFORMANCE*`, `CONFORMANCE_REPORTS` | `rig_conformance` and `write_costs_rust` (already the default path) | `mise run module:check` on one machine and one effect after the change (see the PR description); `write_uses` stays: `tests/patchkit/test_modcheck_uses.py` tests it, `patchkit uses set` is its Rust twin |
| `tools/devtools/py_select.py` group `machine-conformance` | none needed: no test file is left under `tests/modules` | `mise run test:plan` after the change |
| `tests/patchkit/test_sine.py` (trimmed to the fall-through test below) | `tests/fixtures/modules/sine/tests/rig/sine.toml` | mapping and mutation in t1g-module-tests.md; the one unported test (an unregistered type falls back to the stock code, under 70 instructions) is kept (below): the DT2-only adapter test `test_dsp_voice.py::test_an_unregistered_type_falls_through_to_the_stock_renderer` does not cover the DN2 wrapper |
| `modules/machines/tb303/tests/test_tb303.py` (trimmed to the kept tests below) | `tests/rig/tb303.toml`. The helper part (cases, `model_run`) stays as `tb303_cases.py` for `rig_refs_gen.py`; `tb303_ref.py` stays (model of the tables) | mapping and mutation in t1g-module-tests.md; `rig_refs --check` same |
| `modules/machines/pluck/tests/test_pluck.py`, `test_pluck_tune.py`, and the rig test of `test_pluck_range.py` (the file stays with the model tests) | `tests/rig/pluck.toml`. `pluck_rig.py` stays whole (`model_blocks` for `rig_refs_gen.py`, `PluckRig` for the kept state test); `pluck_ref.py` stays | same |
| `modules/fx/freqshift/tests/test_freqshift.py` (trimmed to the kept tests below; `freqshift_long.py`, `freqshift_tone.py` stay for its slow run) | `tests/rig/freqshift.toml`. `mixed_input` stays as `freqshift_inputs.py` for `rig_refs_gen.py`; `freqshift_ref.py`, `test_freqshift_ref.py`, `test_fxshift_analysis.py` and `freqshift.device.test.ts` stay | same |
| `modules/machines/shot/tests/test_shot_playback.py` (trimmed to the latch test below) | `tests/rig/shot.toml`. The model and the rig-driving helper stay as `shot_model.py` for `rig_refs_gen.py`, which asserts routine equals model before it writes a table | same; the tables were not changed, so no PCM pin is re-pinned |

## Kept on purpose

| Kept | Why (importers) |
| --- | --- |
| `tests/support/machine_rig.py` | adapter tests `test_dsp_voice_stereo.py`, `test_dsp_voice_stereo_dn2.py`, `test_dsp_voice_input.py`, `test_dsp_guard_contract.py` (phase 5); `pk_adapter_cost.py`; `shot_model.py` |
| `tests/support/fx_rig.py` | adapter tests `test_fx.py` (phase 5); `freqshift_demo.py` (a listening render, no kit replacement) |
| `tests/support/dsp_voice_rig.py` | adapter tests `test_dsp_guard_contract.py`, `test_fx.py`; `machine_rig.py`, `fx_rig.py` |
| `tests/support/sample_pool.py` | `modules/machines/shot/tests/shot_model.py` only (the generator of the shot tables); `samplepool.ts` is its port |
| `pk_sharc.CallSession`, `run_call` | kernel tests and RE tools (plan) |

## Kept until phase 5

Decision: no coverage is lost before phase 5. Each assertion that no rig check replaces yet stays in a Python test, in its original file name trimmed to these
tests; each carries the comment "kept until T1g phase 5 ports it". They run as named pytest files and `py_select.py` selects them (the glob `modules/**/tests/test_*.py`).
The rig-driven tests that the kit replaces stay deleted.

| Kept | File | Why it stays |
| --- | --- | --- |
| `test_tb303_tracks_unpack_a_level_of_100` | `modules/machines/tb303/tests/test_tb303.py` | the adapter's `_dv_unpack` writes 100 << 8 on a TB303 track only; it belongs to the dsp-voice adapter tests (phase 5), which have no such test yet |
| `test_the_model_sounds_like_open303` | same | model against Open303 (log-spectral distance); nothing else runs `tb303_ref.Open303` |
| tb303 model-only: tie, release, accent above 100, bounded knobs | same | they never ran the routine, so no kit case replaces them |
| `test_smp_is_latched_but_other_parameter_words_remain_live` | `modules/machines/shot/tests/test_shot_playback.py` | the adapter's latch words (phase 5); no adapter test yet |
| pluck model-only: loop period for every note and TUNE word, 200 blocks bounded | `modules/machines/pluck/tests/test_pluck_range.py` | model only; the kit has the routine's corners, not the exhaustive grid |
| freqshift: module declares one effect, the committed SHIFT knob is bipolar | `modules/fx/freqshift/tests/test_freqshift.py` | they read module.toml and gallery.json, no rig |
| freqshift: the 90,000-block (60 s) oscillator run, `slow` | same (with `freqshift_long.py`, `freqshift_tone.py`) | the kit case runs 1,500 blocks; the long run is not ported |
| `test_a_slot_with_an_unregistered_type_goes_back_to_the_stock_code_without_the_table_walk` (dt2, dn2) | `tests/patchkit/test_sine.py` | covers the DN2 wrapper `dsp-voice-dn2.s` fall-through (under 70 instructions) and the hostile-context register restore; the DT2-only adapter test does not |
| pluck: the final string state words lp' and ap' are finite after 8 blocks of the five corner cases | `modules/machines/pluck/tests/test_pluck_range.py` (with `pluck_rig.py`) | the kit corner cases compare the output with the model, not the state words |
| freqshift model | `modules/fx/freqshift/tests/test_freqshift_ref.py` | model only (was never part of the deletion) |

The old files are in git history at `7e1f99e9e`.

## Phase 5 ports (adapter tests on the rig)

The Python tests below were reviewed independently; the verdict is in the last column of each row. Deleted or trimmed in this PR. "Values" says whether the pinned values of the
Python test (output words, registers, counters, words in DM) were reproduced by the Vitest test without a change to any pin. Rig tests run with
`pnpm nx:pilot run <module-id>:test-rig`. Mutation proof: one changed constant in the DSP source (or in the reference) fails the Vitest test.

| Python test | Vitest replacement | Values | Notes | Status |
| --- | --- | --- | --- | --- |
| `modules/core/adapters/overdrive/tests/test_overdrive.py` (5 tests, 12 cases) | `modules/core/adapters/overdrive/tests/overdrive.rig.test.ts` (12 cases) | same, 12 of 12 | The tanh approximant test is dropped: it checked a formula in the test file against `tanh` and never ran `overdrive.s` (tautology). Mutation: `tanh(4*z)` to `tanh(3*z)` in the reference fails `tnh4`. Time: Python 35 s for both files (25 cases), Vitest 0.5 s and 1.9 s | deleted in this PR |
| `modules/core/adapters/sample-pool/tests/test_sample_pool_dsp_dn2.py` (7 tests, 13 cases) | `modules/core/adapters/sample-pool/tests/sample-pool-dsp-dn2.rig.test.ts` (14 cases: the hostile register part of the first test is its own case) | same, 14 of 14 | Same C reference (`pool_wire.h`, compiled with `cc`). The DDR base is pinned (`SP_DDR_BASE_NW32` is 0x1014c400, stock DDR 0x80531000) in the setup test. Mutation: `0x401` to `0x400` in `sample-pool.s` fails the setup test. | deleted in this PR |
| `modules/core/adapters/fx/tests/test_fx_send.py`: the 26 rig tests (24 functions, one with 3 cases) | `modules/core/adapters/fx/tests/fx-send.rig.test.ts` (26 cases; the model and rows are in `fx-send-model.ts`) | same, 26 of 26. The five printed instruction counts are identical: master hook with one send row 29 (free row 21); send hook with 0, 3, 1 rows 16, 31, 24; steady send effect 1318 (no amount), 1468 (one track), 3718 (16 tracks and 16 tracks at mix 0.5) | `test_fx_send.py` keeps its two firmware-image tests (hook ret 0x1C207E, site_len 6; anchors 0x252DF8, 0x254778): `test_the_send_hook_takes_the_first_instruction_of_the_master_function` and `test_the_anchors_of_the_send_route_hold_the_track_bus_and_the_return_bus` (they patch the stock image and read the port file; no rig). The stores test also checks that stores were recorded at all (the Python test passes when nothing is recorded). Mutation: `ONE_127` one bit off fails 6 cases. Python 29.5 s; Vitest 1.9 s | trimmed in this PR to the two firmware-image tests |
| `modules/core/adapters/fx/tests/test_fx_send_dn2.py` (7 tests) | `modules/core/adapters/fx/tests/fx-send-dn2.rig.test.ts` (7 cases) | same, 7 of 7. DN2 send hook costs 1733 (1 slot), 2186 (4), 3998 (16), identical | Mutation: the fade step 1/31 to 1/30 fails the fade test. Python 2.5 s; Vitest about 1 s | deleted in this PR |
| `modules/core/adapters/dsp-voice/tests/test_dsp_voice.py`: 145 rig cases (26 functions; the M6 and M13 grid is 96, the drawn contexts 12, the notes 7, the tracks 5, the c3 variants 4) | `modules/core/adapters/dsp-voice/tests/dsp-voice.rig.test.ts` (145 cases; kit `VoiceRig` and `crumbRig`, `voice.ts`) | same, 145 of 145: output words, register values, breadcrumb words, ring words, instruction bounds (60, 15, 6), pitch within a cent and amplitude 0.35 to 0.40, all as pinned | Includes the two text checks `nosys` and `B3 before I3` (ported with `applyConditionals`). Not ported, stays in Python (the static DAG-store test moves to a surviving file in the retirement PR): `test_no_build_of_the_module_stores_a_same_dag_register_through_an_index_register` (needs `pk_sharc.sharc_object` and its `BuildError`); it fails on main now, the assembler message is `selas: line 3: I0 cannot be stored through its own DAG`, the test expects `parcel 0x0: DM store of I0 (DAG1)`. Mutation: `0x20D1A6` to `0x20D1A7` in `dsp-voice.s` fails 5 cases | trimmed in this PR to the static DAG-store test (fixed by PR 519) |
| `modules/core/adapters/dsp-voice/tests/test_dsp_guard.py` (5 tests) | `modules/core/adapters/dsp-voice/tests/dsp-guard.rig.test.ts` (5 cases) | same, 5 of 5 (fault word 17 after the trickle, kind words, the 8 knob words of the faulting call, clamp mask `0b01110011`) | Python for both dsp-voice files 74 s (151 cases, 150 passed); Vitest 6 s for 150 cases (0.4 s the guard file) | deleted in this PR |

Kit change for these (no change to the rig crate): `RawRig` (`packages/sharc-rig-kit/src/raw.ts`) places objects at named bases with data at their alignment and
calls routines with registers the test sets; `peek` is a call at the return point with no steps.

The send route in `FxRig` (`packages/sharc-rig-kit/src/fx.ts`): `Row.amounts`, `setTrack`, `setSlot`, `setSlotmap`, `setFade`, `setReturn`, `setAmounts`, `sendBlock`, `FxBlock.ret`,
`peek`, `rowWord`, `stateWord`, `symbolWord`, `bus`, `prevGains` and `recordStores: 'all'` (the stores of all code, not only the module's).

`VoiceRig` (`packages/sharc-rig-kit/src/voice.ts`) ports `Rig` of `tests/support/dsp_voice_rig.py` and the `CrumbRig` of `test_dsp_voice.py`: a call runs on a fresh core with the DM
the test built (as `pk_sharc.run_call` does), defines (`#ifdef` options) and a break at any symbol of the wrapper, and `extra_reads` become `reads`.
`Linked.checked` (instructions re-decoded by the link) and the exports `M6_HOSTILE` and `M13_HOSTILE` were added.

## Phase 5b ports (adapter tests on the rig with the stock DN2 1.12 image)

Rig features for these (PR of this branch): `image`, `stock_reads`, per-call `breaks` and `resume`, and `specials` (MSF, MSB) in `packages/sharc-rig/src/lib.rs`; the stores of all code come from `watch` over every pc; `StereoRig` and `StereoSession` in `packages/sharc-rig-kit/src/stereo.ts` (port of the retired `dn2_stereo_rig.py`);
the stage table and chain harness in `modules/core/adapters/dsp-voice/tests/stereo-stages.ts` (port of the retired `dn2_stereo_stages.py`). The tests need `out/sections/dn2-1.12/section_7_BLOB.bin`
(sha256 pinned in `stock.ts`) and skip with their describe title when it is missing. An independent review cleared the rows; the Python files are deleted in the PR of branch test/t1g-phase5b (`test_dsp_stereo_proof.py` is trimmed to the packaging test, which has no rig port). `tests/support/dn2_stereo_rig.py` and `dn2_stereo_stages.py` had no other user and are deleted too. The `DSP_STEREO_PROOF_EVIDENCE` and `DSP_STEREO_UY_EVIDENCE` files had no reader and went with them.

Known gap, in both versions: the hostile-register twins test is vacuous (the hostile values equal the defaults), so it does not prove callee-saved registers.

| Python test | Vitest replacement | Values | Notes | Status |
| --- | --- | --- | --- | --- |
| `modules/core/adapters/dsp-voice/tests/test_dsp_stereo_gain_slot.py` (9 functions, 24 cases) | `dsp-stereo-gain-slot.rig.test.ts` (24 cases) | same, 24 of 24 (planes, buffers, the one-slot registers I3, I4, I5, I12, I6, I7, R9; the hook costs under 16 x 30 instructions) | Mutation: `LSHIFT R3 BY 5` to `BY 4` in `_gs_slot` fails 10 cases | Deleted (this PR) |
| `test_dsp_stereo_identity.py` (3 functions, 26 cases) | `dsp-stereo-identity.rig.test.ts` (26 cases) | same, 26 of 26 | Mutation: late shaper scale 0x41800000 to 0x41000000 fails `shaper-late` | Deleted (this PR) |
| `test_dsp_stereo_twins.py` (10 functions, 66 cases) | `dsp-stereo-twins.rig.test.ts` (66 cases) | same, 66 of 66 | same mutation fails 2 cases here | Deleted (this PR) |
| `test_dsp_stereo_cost.py` (1 function, 12 cases) | `dsp-stereo-cost.rig.test.ts` (12 cases) | same, 12 of 12: every stage inside its ceiling (the ceilings are the Python values) | | Deleted (this PR) |
| `test_dsp_stereo_lifecycle.py` (6 functions, 111 cases) | `dsp-stereo-lifecycle.rig.test.ts` (111 cases) | same, 111 of 111 | | Deleted (this PR) |
| `modules/core/adapters/dsp-stereo-proof/tests/test_dsp_stereo_proof.py` (16 functions, 39 cases) | `dsp-stereo-proof.rig.test.ts` (39 cases: 38 ported and one fixture check) | same, 38 of 38: identity (mode, slot, in place), reset, history corruption, SIMD rejection (registers, frame, no stores, MSF/MSB), stock zero-count I12, passthroughs, DAG fallbacks, scratch DAGs, sticky status, polarity, mode latching, non-finite refusal, signed zero | Not ported, stays Python (the file is trimmed to it): `test_packaging_retargets_only_the_five_engine_calls` (runs `patchkit apply` on the stock `.syx`). The fixture checks of `decode_loaded_at` (type 25a at the sites, 3c and 16a after them) became one test: the stock bytes at the sites equal the assembled `CJUMP target (DB)` with its two delay slots (stronger than a type name). `MEASURED` and the `DSP_STEREO_PROOF_EVIDENCE` file are not ported (they only wrote measurements, no assertion; the private page size and the instruction bound are asserted). Mutation: the restore of STKYX at `_sp_render_finish` replaced by a NOP fails the 2 sticky cases and the non-finite `output` case | Trimmed to the packaging test (this PR) |
| `test_dsp_stereo_uy_proof.py` (10 functions, 36 cases) | `dsp-stereo-uy-proof.rig.test.ts` (37 cases: 36 ported and one fixture check) | same, 36 of 36 | the fixture check also asserts that the first stage object is a prefix of the linked U/Y build. The 3a site (0x1c96aa) is `DM(I7, M7) = R2, R4 = R5 + R10` in its delay slot (assembled text equals the stock bytes) | Deleted (this PR) |

Time for the five dsp-voice files (239 cases): Python 51.6 s, Vitest 46.5 s (each case opens one to four rig processes; the 817 KB image loads in 0.1 s per process). The two proof files (75 Python cases, 76 Vitest cases): Python 9.9 s, Vitest 15.5 s: the rig is slower here because every case starts one or two processes and the Python cases are short (the rig's gain is in long runs, not in 0.2 s cases).

## Shadow replay (the stereo shadow stages of finding 96)

`tests/patchkit/test_dsp_stereo_shadow_replay.py` (44 cases) is the evidence of finding 96 for the shadow right-channel stages: the right channel runs as a shadow of the left (a private copy of the
state, the same stock stages) and must equal an isolated mono run of the same channel on a separate core. Store watchpoints bound the writes of each call, including the nested stock frames, and
count the writes per state word. Ported to `modules/core/adapters/dsp-stereo-proof/tests/dsp-stereo-shadow-replay.rig.test.ts` (43 cases; rig and helper `shadow-rig.ts` beside it), on the rig with
the stock DN2 1.12 image; it skips with its describe title when `out/sections/dn2-1.12/section_7_BLOB.bin` is missing. The synthetic fixtures `tests/fixtures/dsp/stereo-*-shadow-replay.s` stay where
they are (finding 96 cites them); `tools/nx/rig-tests.mjs` now lists them as inputs of the `test-rig` targets. Kit change: `RawCallOptions.profile` (the rig's per-pc execution count). The rig crate is unchanged.

The Python file is deleted (this PR) after independent review.

Gap: the rig checks load counts, not load addresses, so a load with the right count and a wrong address passes. The ordered-route and helper-reset read bounds and the held period have no substitute; the held period is partly covered by the repeated-word check. The rig core runs with `assume_nw32` on by default (`packages/emulator/sharc-rt`), the setting Python enabled per group. The Python step bound of 12000 is kept, though the two cores may count steps differently (longest rig call 4302). Follow-up: roadmap item T1i (rig read hook).

Python on main (this branch, `uv run pytest tests/patchkit/test_dsp_stereo_shadow_replay.py -rf`): 23 failed, 21 passed, 44.3 s. The failures are not only `max-steps`: the Python core halts on
`unsupported full compute opcode 0xe3` (recurrences), `fork (2 successors)` (prepared, 3 + 2 reset cases), `no form matches` (gain, 4), `unsupported full compute opcode 0xc3` (ring, 2), and `max-steps` at 12000
(source, 5; ordered, 6). Rig: 43 cases pass in 10.8 s (about 985 calls, each case opens 1 to 3 rig processes). The longest call is `_init_*_ring` with 4302 instructions; every call is far inside
the Python limit of 12000, so no bound was raised.

| Group (Python test) | Cases | Rig | Python on main | Mutation (fixture, restored) |
| --- | --- | --- | --- | --- |
| multiblock (`test_multiblock_shadow_matches_isolated_mono`) | 1 | match: shadow equals mono on words, controls and histories, 4 blocks, reset reproduces block 0; stores 4 per audio word, 2 per history word | pass | `_right`: the U/Y stage on the left state (`0x08040400`) fails it and the 3 fault cases |
| recurrences (`test_recurrences_default_core_matches_independent_channels`) | 1 | match | fails (opcode 0xe3) | `_right_recurrences`: U/Y on the left state fails it |
| fault injection (`test_shadow_corruption_is_observable`) | 3 | match: each fault differs from the reference | pass | same as multiblock |
| filter leaves (`test_filter_leaf_shadow_matches_isolated_mono`: snapshots, nine, six, with and without a corrupt history) | 6 | match: independent equal, bad-history differs in audio and state | pass | `_prepare_snapshots` without `COPY_WORD(14)` fails 2 snapshot cases |
| prepared stages (`test_prepared_shadow_matches_isolated_mono`: held, prepared, 3 faults each) | 6 | match | `held` pass, `prepared` 3 fail (fork) | `_prepare_held` without `COPY_WORD(3)` fails 3 |
| gain (`test_gain_shadow_matches_isolated_mono`) | 4 | match: phases `[2, 2, 1|4]`, markers, table loads reached | 4 fail (no form) | `_prepare_gain` with 10 copied words fails 4 |
| ring (`test_ring_shadow_matches_isolated_mono`: private rings, shared pointer) | 2 | match; the shared pointer breaks the store bounds ("ring store bounds") and changes the left backing | 2 fail (opcode 0xc3) | `_prepare_ring` without `COPY_RING(15)` fails 2 |
| source (`test_source_continuation_matches_isolated_mono`: depth 0, 8, 4, 2, 1) | 5 | match; S registers as pinned | 5 fail (max-steps) | right source stage writing the left audio plane fails 5 |
| ordered route (`test_ordered_route_matches_isolated_mono`: none and 5 faults) | 6 | match; phases `[2, 2, 1, 1]` | 6 fail (max-steps) | `_prepare_ordered` without `COPY_ORDERED(114)` fails 6 |
| helper reset (`test_helper_reset_mirroring_matches_isolated_mono`: 4 families, missed reset, blanket snapshots) | 9 | match | `prepared` 2 fail (fork), 7 pass | right held reset on the left record fails 1; right snapshots reset on the left record fails 2 |

Not ported: `test_base_image_overlay_and_decode_cache_isolation` (passes in Python). It tests the Python `CallSession` (overlay order of a hand-made `LoadedMemory`, decode cache per session); the rig
places every object over the stock image and every case runs one core per process. One observation from the mutations: `_prepare` with 13 instead of 14 control words does not fail the base
cases (word 13 is not read before the stage writes it), so the Python test does not pin that copy either.

**Not reproduced: the load checks.** The Python core logs every DM read; the rig records stores only (`watch`). The Python test uses reads in six groups (prepared, gain, ring, source, ordered, helper reset)
to check that the table and audio loads stay in range (address range of each load at its PC) and that the loaded values are finite (held period `1 < p < 32`). The port keeps what the rig can say:
the number of times the load instructions run, which equals the Python count of loads (held 1 load = 4 bytes, prepared 64, ring 128, source 16 double-word sample loads and 2 table loads; the counts
are the Python byte counts divided by the width of the load) and which of the gain table loads ran. The address ranges, the ordered-route and reset read bounds and the held period are **open**:
they need a read record in the rig (a read analogue of `watch`, i.e. a read hook in the SHARC runtime `_dm_read` paths, which is a change to `packages/emulator/sharc-rt` and so to the generated-core key).
Store checks are complete: the rig reports one run per instruction, split here into words (a long-word or SIMD store of two words is one run of 8 bytes).
