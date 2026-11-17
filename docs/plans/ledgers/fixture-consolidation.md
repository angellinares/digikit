# Ledger: fixture consolidation (one canonical fixture per contract feature)

Status: host checks ran (patchkit tests, pytest of the touched files, web test-wasm, regression and kit host tests, typecheck, layers, formats). The device tests named in the pull request have not run. Packed bytes against the base `0ea0387ef` (`sdk_hashes.mjs`): `sine`, `dyn-dn2` (dn2), `dyn-fx-gain`, its wrappers, `dyn-fx-gain-bipolar`, all core objects and both patched images are unchanged; changed: `dyn-crash` (new code), `lfo-shape-dup` (the shared source), `dyn-dn2` on dt2 (new); removed: the deleted fixtures.

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

`tools/devtools/sdk_hashes.mjs` now packs service modules as `.dks` (needed to record the baseline).

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

## Step 4: dyn-params is merged into dyn-dn2

`dyn-params` (DT2 only) and `dyn-dn2` (DN2 only) had the same shape: four parameters of different ranges on the DSP of `sine`. `dyn-dn2` now builds for both devices (`dt2-1.17` in `ports` and `targets`, `[uses.dt2]` of one machine, four parameter rows, no page, `record_bytes` 444 copied from `dyn-params`, and a hand-written `dt2` part in `build/gen/gallery.json`). Its `module.c`, DSP and names are unchanged, so the DN2 bytes (two PCM pins: `equiv-dn2-set` and the sine pin) must be byte-identical: checked in phase 2. The values in `uses.dt2` and the DT2 gallery are derived from `dyn-params` and the DN2 gallery; `mise run module:check test/dyn-dn2` rewrites them if they are wrong (phase 2 does not run it; open).

| Deleted | Old assertion | New location |
| --- | --- | --- |
| Fixture `dyn-params` | `roundtrip.device.test.ts` DT2 sets and types (`dynp`): project round trip with a run-time machine on track 1 | The same scenarios with `test/dyn-dn2:dyn_pluck` (hint 7 and the panel script unchanged: the position in the SRC list and the knob words must be rechecked on a DT2 run, open). |
| same | `modules-capacity.device.test.ts`: MODULES reply for two DSP modules (dyn-params, sine) | The same with `test/dyn-dn2` and `test/sine`. |
| same | `inferred-needs.device.test.ts`, DT2 only | `test/dyn-dn2` runs on both devices (the `DT2_ONLY` filter is deleted). |
| same | `dkneeds.wasm.test.ts`: wasm reader and CLI give the same needs | The same with `test/dyn-dn2` (DT2 pack). |
| same | `dynreg`, `core/runtime-tests`, `debug` project module lists | `test/dyn-dn2` where `test/dyn-params` was. |
| same | DT2 parameter ranges 1..126, 2..125, 4..123 of its four params | DROPPED: no test asserted them. |
| same | `test_patchkit_registry.py` `test_pack_refuses_a_module_for_the_wrong_device` used `dyn-params` as the DT2-only module and `dyn-dn2` as the DN2-only one | Uses `dyn-dsp-hang` (DT2 only) refused for the Digitone; no DN2-only fixture is left (`dyn-dn2-pages` is parked), so the other direction is not tested. |
| `inferred-needs.device.test.ts` entries `digikit/freqshift`, `pluck`, `fadedlimes/analog`, `bd`, `pd` (5 boots per device, full tier) | The resources taken equal the needs inferred from the packed module, for a real module of each kind | Replaced by decision: the five real modules are replaced by the fixture `commit-demo`, which covers the multi-entry and effect-with-page shapes. The other fixtures cover the rest: `sine` and `dyn-dn2` (machine), `dyn-fx-gain` (effect), `c-demo` (raw ColdFire module); the debug modules stay. Gap: no service-type module (`svc-test`) and no module with `synth-decl` shape in that test; real DSP code sizes are no longer exercised. |

## Step 5: the fault fixtures are merged into dyn-crash

One fault injector `test/dyn-crash` (id kept). Its store file `cfg/crash-after.bin`: bytes 0 to 3 the delay in blocks (big endian), byte 4 the mode, optional byte 5 the place. Modes: 0 illegal instruction, 1 division by zero (both unchanged, with the `crash-pc` log), 2 reset of the machine (SOFTRST) in `tick` (old `dyn-fault-late` mode 0), 3 hang of the UI task (old `dyn-fault-late` mode 1), 4 reset in `init`, never return (old `dyn-fault`). Place 0 `tick`, place 2 the frame callback of the boot animation (`init` takes it with `dk_intro_take`; modes 0 and 2). Without the file the module never faults, so every test that uses a mode asserts the fault happened. `intro-ticks` stays (boot animation, not a fault).

| Deleted | Old assertion | New location |
| --- | --- | --- |
| Fixture `dyn-fault` (reset in `init`, no cfg file) | `safe-mode.device.test.ts` test 1: boots 1 and 2 never reach the UI loop, boot 3 is safe mode (counter 2, no module, E_SAFE_MODE, intro frame, record) | Same test on `test/dyn-crash` with `cfg [be32(0), 4]`. The fault is asserted by the counter 2 and `safe: 1` of the third boot (a missing cfg file gives no safe mode and fails). |
| same | `isa_record.rs` `pack_size_summary_parts_sum_to_the_file` and `pack_refuses_a_tampered_object` used its object | Use the object of `dyn-crash`. |
| same | `test_patchkit_dkrt_boot.py`: committed objects current, packs for both devices | Same on `test/dyn-crash`. |
| `pk_dkrt_boot.py` scenario `safe` (Python): three boots of the reset-in-init card are safe mode with the counter, no module loaded, the voice rows free, the UI loop ran with counter 0 and the selection kept; the safe-once flag boots safe mode (flag), clears the flag, keeps the selection | Duplicate of `safe-mode.device.test.ts` tests 1 (counter, no module, record) and 2 (flag, flag cleared, selection kept). DROPPED, with its `E_SAFE_MODE` constant, the `fault.dkm` pack and the `--only safe` option. The check that the voice rows are free in safe mode (`voice_rows`) is added to `safe-mode.device.test.ts` test 1 (`voiceRows` of `project-kit.ts`, symbols `voices.rows` and `dyndsp_info` in the rig). |
| Fixture `dyn-fault-late` (`cfg/fault-after.bin`, modes 0 reset, 1 hang) | `safe-mode.device.test.ts` early and late tests: a hang before the stable period raises the counter, boot 3 is safe mode; a hang after it clears the counter; every boot asserts `hung` | Same tests on `test/dyn-crash` with `crash-after.bin` and mode 3. `hung` is asserted on each boot, so a missing file fails. Mode 2 (reset in `tick`) has no test of its own (the old mode 0 had none either). |
| Fixture `intro-fault` (illegal instruction in the frame callback) | `bootscreen-fault.device.test.ts`: the boot halts in the stock handler (`guest fault during boot: Halted`) | Same test, `dyn-crash` with cfg `[0,0,0,0, 0, 2]`. A missing cfg file gives a normal boot and the assert on the error fails. |
| Fixture `intro-reset` (SOFTRST in the frame callback) | `bootscreen-safe.device.test.ts`: two resets from the intro callback, boot 3 safe mode by counter, seq 3, stock intro, selection kept | Same test, cfg `[0,0,0,0, 2, 2]`. The counter assert fails without the file. |
| `modules/tools/bootscreen/project.json`, `modules/core/runtime/tests/project.json` module lists | Name the old fixtures | Name `test/dyn-crash` (and `test/intro-ticks`). |

Unchanged: `crash-capture.device.test.ts` (cfg modes 0 and 1: the crash record is asserted, so the fault is asserted) and `fault-trace.device.test.ts` (writes the cfg with mode 0; asserts the dk/fault record).

## Step 6: lfo-shape-dup becomes a wrapper, native-pages is deleted

| Deleted | Old assertion | New location |
| --- | --- | --- |
| `lfo-shape-dup/src/module.c` (a copy of `lfo-shape` with the ramp from 500 instead of -1000) | A second module with the shape id 7 loads as `E_PLAN_KEY` (`lfo.device.test.ts`, `refusal.device.test.ts`: the tests assert the error code and the names only) | `lfo-shape-dup` keeps its own `module.toml` (module name) and builds `../lfo-shape/src/module.c`. The ramp of the second module (500) is DROPPED: no test reads it. Not done: the alias (`pack-aliases`) the design called for; test-only projects reject aliases (`tools/nx/device-tests.mjs`, "needs explicit pack-aliases support"), so the toml-only wrapper is the fallback. |
| Fixture `native-pages` (replacement page zero and page one, action mapping, independent level access) | No test used it ("none yet" in the fixture table) | DROPPED. Gap: nothing proves replacement native pages with a bound click on a fixture; `native-page` covers the callback page, and a screenshot of a real module would cover the rest. `docs/design/native-machine-pages.md` says so. |

## Open gaps and points

- `tb303.device.test.ts` stays the only device test of the machine gate delivery (`DK_MACHINE_GATE`): no fixture sets it. A fixture with a gate would let a fixture test replace the real-module test.
- Rings A and C of the fx hook are not read by any test any more (step 1).
- `dyn-dn2` on the Digitakt II: `[uses.dt2]` and the DT2 part of `build/gen/gallery.json` are derived by hand; `module:check` is the writer. The hint 7 and the knob script of `roundtrip.device.test.ts` for DT2 must be confirmed on a device run.
- `lfo-shape-dup` is a wrapper, not an alias.
- Not done (optional by decision): `dyn-fx-gain-bipolar` without routes, and `commit-demo` sharing a DSP.
