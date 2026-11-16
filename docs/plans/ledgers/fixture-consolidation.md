# Ledger: fixture consolidation (one canonical fixture per contract feature)

Status: written in phase 1 without any build or test run; the tests named below have not run. Phase 2 runs the host checks and lists the device tests that must run.

Why: AGENTS.md "fixtures are DRY": one canonical fixture per contract feature, shared parts in one source, a variant only where a test needs a distinct module identity. The fixture set had two parallel effect paths (patch-time `fx-gain` and `fx-nan`, run-time `dyn-fx-*`), five copies of one gain DSP, four fault injectors that differ in one line, and fixtures that no test reads.

## Step 1: the patch-time effect path is deleted

The registry `fx` of `core/fx` refuses an `[[entry]]` row of every module except `core/dynfx` (`refuse_entries`, `refuse_entries_except`; the pattern of `machines`/`core/dynreg`). Run-time effects fill the rows that `core/dynfx` reserves.

| Deleted | Old assertion | New location |
| --- | --- | --- |
| Fixture `fx-gain` (patch-time effect, 0.5 gain) | Master block halved through the adapter (`test_fx.py` `test_the_fixture_test_fx_gain_halves_the_master_through_the_adapter`) | The adapter's block, clobber and context rules stay in `test_fx.py` (the tests on `FxRig` rows with an inline source, lines 90 to 140 and 269); the halving of a real effect is `fx-contract.device.test.ts` (wet compare 0.5) and `packages/sharc-rig-kit/conformance/fx.test.ts` on `test/dyn-fx-gain`. |
| same | `_fg_gain` word is 0.5 | DROPPED: not a contract feature. The runtime knob is checked by `expectGainRow` in `fx-contract.device.test.ts`. |
| same | Cost of one block below 860 instructions | `test_fx.py` (the 281 budget test) and the conformance budget check. |
| same | `test_the_fixture_puts_one_row_in_the_chain_on_both_devices`: `fx.count == 1`, `fx.row_size == 96`, symbols `_fg_process` and `_fg_state` in the plan | `test_core_dynfx_reserves_three_free_rows_in_the_chain_on_both_devices` (`core/fx,core/dynfx`: `fx.count == 3`, `row_size == 96`). The symbol asserts are DROPPED (there is no patch-time symbol any more). |
| same | `test_fx_specs_find_the_fixtures` (`fx_spec("test/fx-gain")` returns `_fg_*`; `core/fx` and `test/sine` are no effect specs; fx-gain is no machine) | The `core/fx`, `test/sine` and `is_machine_module` asserts moved to `test_run_time_effects_are_effect_specs_too` on `test/dyn-fx-gain`. The patch-time branch of `fx_spec` and `fxSpec` is deleted, so the `_fg_*` assert has no subject. |
| same | Smoke cell `dt2-1.17:core/fx,test/fx-gain` and its 16 goldens | The goldens of the cell `core/fx` are the same 16 hashes (checked on the old `goldens.json`); the cell `core/fx` stays. Audible output with an effect: `fx-contract` wet compare and `fx-live` `requireAudible`. |
| same | wasm test `wasm adds the requirements of the selection like the CLI` used `test/fx-gain` | Uses `core/dynfx` on both devices; the regex `added by requires: .*core/fx` is unchanged. |
| same | `tools/gate/smoke/project.json` input line | Removed. |
| Fixture `fx-nan` (patch-time effect writing NaN) | Conformance negative: the guard rejects it (`spec.ts` `FX_NEGATIVE`) | `test/dyn-fx-nan` stays in `FX_NEGATIVE` (same expected text `guard stepped in`). NaN behaviour: `test_fx.py` (216 to 252), `fault-trace.device.test.ts`, the conformance negatives. |
| `tools/patchkit/pk_proof_fx.py`, `fx_rings.py` and their two entries in `pk_proofs.py` (DT2 and DN2) | Empty chain: PCM, master and rings A and C equal stock | `fx-contract` (plain vs dry compare), the `core/fx` smoke cell, `test_fx.py` (empty chain, lines 35 and 47). |
| same | Gain 0.5: master and USB/DAC rings halved | `fx-contract` (wet compare 0.5). |
| same | Gain word 1.0 equals stock | `fx-pages.device.test.ts` (knob ratio, line 51). |
| same | `enabled` cleared in mid-run: master equals stock after the fade | `test_fx.py` (150) and the `fx-contract` deselect. |
| same | Send part of the proof | `fx-send` rig and device tests (DT2 and DN2), `fx-routes-persist`. |
| same | Rings A (DAC) and C (USB) against the master | DROPPED. Only `fx_rings.py` and `pk_proof_fx.py` read rings A and C. Gap: nothing checks that the hook output reaches the DAC and USB rings; the hook changes only the master literal by design. One line in finding 55. |
| `[[entry]] registry = "fx"` text in `modules/README.md`, `core/fx/README.md`, `core/fx/module.toml`, `pk_conform.py`, `spec.ts`, `typed.rs` comment | Describe the patch-time path | Rewritten for run-time effects only. |

Test added: `packages/patchkit/tests/prims.rs` `a_patch_time_effect_row_of_any_module_but_core_dynfx_is_refused` (the real `core/fx` registry, a synthetic module with an `fx` entry, the registry's reason in the error).

## Step 2: trace-demo is merged into dyn-fx-gain

`trace-demo` was `dyn-fx-gain` plus `#include "dk_trace.inc"`, two `DK_STEP` lines in the DSP and one `DK_TRACE_LOG` in `module.c`. The marks now sit in the canonical sources of `dyn-fx-gain`. They expand to nothing in a build without `--trace`, so the packed bytes of `dyn-fx-gain` must not change (checked in phase 2 with `sdk_hashes`).

| Deleted | Old assertion | New location |
| --- | --- | --- |
| Fixture `trace-demo` | `packages/patchkit/tests/trace.rs`: a build without `--trace` equals the build of the sources without the mark lines; no trace bit; with `--trace` the trace bit, `_dk_step` import and a longer file; the traced objects do not replace the plain ones in the cache | The same test on `test/dyn-fx-gain` (stripped copy of `module.toml`, `module_ids.h`, `module.c`, `dyn_fx_gain.s`). No assertion changed. |
| same | `.agents/skills/digikit-module/SKILL.md:71` names the fixture | Names `dyn-fx-gain`. |

## Step 3: one gain DSP for dyn-fx-gain and four wrappers

The DSP sources of `dyn-fx-gain-b`, `-both`, `-send` and `-sendonly` were the DSP of `dyn-fx-gain` with another symbol prefix (`dgb`, `gbt`, `gs`, `gso`; checked by normalising the prefix: the four are identical to it, except the trace lines now in `dyn-fx-gain`). Each is now a wrapper: its own `module.toml` and `module.c` (module identity, tab name, defaults, routes), `sources = ["../dyn-fx-gain/src/dsp/dyn_fx_gain.s"]`, `dsp = "dg"`. Effect ids hash the module name and the effect id, so they do not move. `dyn-fx-gain-bipolar` keeps its own DSP (`F2 = F2 * (1/64) + 1.0`).

| Deleted | Old assertion | New location |
| --- | --- | --- |
| `dyn_fx_gain_b.s`, `_both.s`, `_send.s`, `_sendonly.s` | The four DSP objects behave as `dyn-fx-gain` (out = in * p0, call counter in the state word) | The one source of `dyn-fx-gain`. Every test that loads a wrapper still runs its own module identity, defaults and routes. Packed bytes may change (symbol names, trace no-ops): listed in the phase 2 report. |

Canonical roles after the step: `dyn-fx-gain` route (master, master); `-both` (master or send, default master); `-send` (master or send, default send); `-sendonly` (send); `-b` second effect id with other defaults and the tab GNB (`dk_fx_mix`, the kit persistence test).

`modules/core/adapters/dynfx/tests/test_dyn_fx.py`: `test/dyn-fx-gain-b` leaves the parametrisation of `test_committed_objects_are_current` (it now builds the object of `dyn-fx-gain`, which is checked) and the `other` pack of `test_the_effect_record_carries_the_id_the_kit_stores` is dropped: the `--name other/name` case of the same test already proves that another module name gives another effect id.
