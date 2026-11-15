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
- Observation [O]: in the first full Python run the 22 `sample` cases of `digikit/shot` (11 per device) reported a lower `steady` (DT2 stereo 2331 against 3209,
  worst block 2449 against 3325; DN2 2658 against 3534) than the kit and than `dsp_cost_voice` in module.toml. A second Python run of `digikit/shot` alone
  (`DIGI_CONFORM_MODULES=digikit/shot DIGI_CONFORM_DEVICE=dt2|dn2`) reported 3325 and 3534, identical to the kit. The kit's numbers repeat; the cause of the
  Python difference in the full run (parallel workers, host load 60) is not found. The Python suite is deleted, so this closes with the deletion; a reviewer who
  wants the cause can run the full Python suite at `7e1f99e9e`.

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
| `tests/patchkit/test_sine.py` | `tests/fixtures/modules/sine/tests/rig/sine.toml` | mapping and mutation in t1g-module-tests.md; the one unported test (an unregistered type falls back to the stock code, under 70 instructions) is pinned by `modules/core/adapters/dsp-voice/tests/test_dsp_voice.py::test_an_unregistered_type_falls_through_to_the_stock_renderer` (under 60 instructions) |
| `modules/machines/tb303/tests/test_tb303.py` | `tests/rig/tb303.toml`. The helper part (cases, `model_run`) stays as `tb303_cases.py` for `rig_refs_gen.py`; `tb303_ref.py` stays (model of the tables) | mapping and mutation in t1g-module-tests.md; `rig_refs --check` same |
| `modules/machines/pluck/tests/test_pluck.py`, `test_pluck_range.py`, `test_pluck_tune.py` | `tests/rig/pluck.toml`. `pluck_rig.py` is cut to `model_blocks` (used by `rig_refs_gen.py`); `pluck_ref.py` stays | same |
| `modules/fx/freqshift/tests/test_freqshift.py`, `freqshift_long.py`, `freqshift_tone.py` | `tests/rig/freqshift.toml`. `mixed_input` stays as `freqshift_inputs.py` for `rig_refs_gen.py`; `freqshift_ref.py`, `test_freqshift_ref.py`, `test_fxshift_analysis.py` and `freqshift.device.test.ts` stay | same |
| `modules/machines/shot/tests/test_shot_playback.py` | `tests/rig/shot.toml`. The model and the rig-driving helper stay as `shot_model.py` for `rig_refs_gen.py`, which asserts routine equals model before it writes a table | same; the tables were not changed, so no PCM pin is re-pinned |

## Kept on purpose

| Kept | Why (importers) |
| --- | --- |
| `tests/support/machine_rig.py` | adapter tests `test_dsp_voice_stereo.py`, `test_dsp_voice_stereo_dn2.py`, `test_dsp_voice_input.py`, `test_dsp_guard_contract.py` (phase 5); `pk_adapter_cost.py`; `shot_model.py` |
| `tests/support/fx_rig.py` | adapter tests `test_fx.py`, `test_fx_send.py`, `test_fx_send_dn2.py` (phase 5); `freqshift_demo.py` (a listening render, no kit replacement) |
| `tests/support/dsp_voice_rig.py` | adapter tests `test_dsp_voice.py`, `test_dsp_guard.py`, `test_dsp_guard_contract.py`, `test_fx.py`; `machine_rig.py`, `fx_rig.py` |
| `tests/support/sample_pool.py` | `modules/machines/shot/tests/shot_model.py` only (the generator of the shot tables); `samplepool.ts` is its port |
| `pk_sharc.CallSession`, `run_call` | kernel tests and RE tools (plan) |

## Assertions that no rig check replaces (review items)

- `tb303`: `test_tb303_tracks_unpack_a_level_of_100` (the adapter's `_dv_unpack` writes 100 << 8 on a TB303 track only) and `test_the_model_sounds_like_open303`
  (model against Open303, log-spectral distance; `tb303_ref.Open303` stays but nothing runs it). The first belongs to the dsp-voice adapter tests (phase 5) and has no
  adapter test yet.
- `shot`: `test_smp_is_latched_but_other_parameter_words_remain_live` (the adapter's latch words, phase 5), no adapter test yet.
- Model-only tests that never ran the routine: tb303 (tie, release, accent, bounded knobs), pluck (loop period for every note, 200 blocks bounded), freqshift
  (`test_freqshift_ref.py` stays; the 90,000-block `slow` run is not ported).

Each is in the git history of the deleted file at `7e1f99e9e`.
