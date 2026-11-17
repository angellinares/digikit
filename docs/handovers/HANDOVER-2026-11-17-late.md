# Handover 2026-11-17 late: R12 batch merge-proof gate running, R12-8 slice built, RS-C built

Start from docs/plans/sdk-runtime-roadmap.md, then this file. It follows HANDOVER-2026-11-17-evening.md (same day; its merged-PR list and decisions still hold). Plans: docs/plans/runtime-slimming.md (RS-S, RS-C), docs/plans/digikit-hot-reload.md (B1), docs/plans/test-architecture.md (T2).

## Branches and lanes (state about 17:15; nothing pushed except PR 569)

| Branch | Lane | State |
| --- | --- | --- |
| `rt/r12-batch` | sdk-design | Head a95e257f9. Fixes from the device re-run: fx-bipolar waits for the DSP row word; fx-pages uses a relative trigger; fx-kit-persist guards fx reads on the stock boot and has no init-hook counter check (finding 61: boot kit init runs before fx.dks binds; Load Kit does not call the initialiser); fx evidence requires audio only when a suite says so (one resolver, suites explicit); service-check errors (duplicate slot, missing adapters folder); hook_ab compare bound relative to the reference peak (2**-20); machine-rows: expectNoFault, PNG on failure, turns repeat bursts (max 4) past the firmware encoder lead-in filter (finding 03: the emulator matches hardware, our bursts were too small). Merge-proof gate running since 14:41: host stages green, device subset in progress, no failure seen yet (slow under the old 12-emulator budget); device subset `mise run test:subset -- S:machines A:core/fx A:core/dynfx A:core/fxpages C:loading C:storage S:samples --run` (about 100 tasks). Expected failure: fx-bipolar bias-up compare (the TS helper still asserts ulps <= 2), fixed on `fix/batch-review-2` |
| `fix/batch-review-2` | sdk-fixtures | One PCM compare rule (hook_ab `ok` verdict, `--rel-bound`; per-test ulps checks removed; pk_digikit_fxpage deleted); evidence writes the overlay before the fault check; shared evidence collector; captureRemoteOled returns its path; svccheck duplicate slot only for overlapping device lists (Rust tests). Merges into the batch after the gate, then the fx subset and machine-rows run again (about 40 tasks) |
| `fix/dyn-dsp-hang-gallery` | sdk-fixtures | Rig conformance gallery check skips machines with param_rows = 0; the check reads one shared module, `packages/sharc-rig-kit/src/gallery-gaps.mjs`. Merges into the batch |
| `test/pyramid-moves` | | fx-bipolar gain law on the rig (10 cases plus a MIX-during-run case; new check `scales-input`); 5 device PCM compares removed. Reviewed |
| `test/machine-rows-host` | | DN2 clamp bursts reduced to one Gamma clamp; table read-backs stay. Reviewed |
| `test/unit-gaps` | t1-tests | Rebased; about 49 host tests green. Two runtime bugs fixed: `report_dsp` changed an active module's record; module files skipped `role_ok` (a content module with PROVIDES/HOOKS loaded). Reviewed |
| `test/fingerprint-pins` | docs-handover | Builds, host tests green. Rig pins v2 (analog, pd, pluck bit-identical; bd DT2 new; approved from numbers). The freqshift pin writer is fixed (effects are keyed `fx`). Open: write the freqshift pins and the dynreg device pins `dynreg/tests/pins/*.pcm.json` from the gate's kept captures with the pin CLI (numeric report) in the same PR |
| `nx/dks-tests` | t1-tests | rust-tests targets for standalone service crates (found by `[workspace]`); `test:changed` runs objects-check when patchkit is affected. Reviewed, doc nits fixed |
| `b1/bind-resolver` | sdk-gen | B1 plus review fixes (zero-address guard `call_if_bound`, docs) |
| `rs/storage-tests` | docs-plans | RS-S plus review fixes; the assembly test reads sources (sdk.toml, device file); object hash refresh |
| `rs/core-leftovers` | sdk-gen | Head 2394da3f7. RS-C C2(b)-(e), C3 (partial), C4 on top of the fixed B1, plus review fixes (kind-free rule scans whole files and abi; safe machines.types migration; DECLARE pass before voice rows; shared builder module; golden from pre-refactor code). Built and reviewed. 5 runtime host tests are red from RS-C: a loader test expecting 2 commit passes (now 3 with the DECLARE pass), collect, svc_imports, two flaky spans tests; fix in its host verification. Deferred: `fx_entry` to `dkrt_export(name_hash)` (`Service::fx_entry` stays) |
| `r12-8/slice` | t1-tests | R12-8 steps 1-4 built (plan: docs/plans/r12-8-sharc-stubs.md, PR 569, with the late-teardown rule). DT2 dsp-voice split into stubs plus dispatch (dispatch in the `machines.dks` DSP section); dsp-link slot block (16 words, window 0, area 64 bytes shorter); patchkit DSP slots and fills, `.dks` DSP section, service-check; loader: provider DSP load, `Installer` trait with ColdFire and DSP backends, one shared unload lifecycle for modules and services, late teardown (a failed DSP part takes the service and its dependents down; E_MISSING). Imports 29 of 32. Measured: about +10 instructions per stubbed voice (about 350 per block), to judge at the slice gate |
| `test/t1i-rig-read-hook` | sdk-fixtures | T1i: read hook in sharc-rt; shadow-replay read checks restored as rig LoadRules. Tests pending; merges with a full device run (the daemon key changes) |
| `test/t1c-contracts-docs` | | T1c doc fix. Descriptor contracts and the Storage contract suite are on main |
| `nx/emulator-budget` | | EMULATOR_CORES 3, so 4 emulators at budget 12; `DIGI_BUDGET` overrides. Needs a `test:plan` check |
| `tools/devreload-services` | | `patchkit services` is the one source for which services a module needs: the pack target writes `<leaf>.services.json`, TS reads it, Python `dk:dev` and modcheck call patchkit. Needs a `test:plan` check and tests |

## Next, in order

1. Gate result (read failures one file at a time).
2. Merge `fix/batch-review-2` and `fix/dyn-dsp-hang-gallery` into the batch; targeted re-run (fx subset and machine-rows) with the emulator cap; rig conformance; device pins.
3. Batch PRs in order: prep, R12-5, R12-6, T2 lanes, assembly layer, fixtures, consolidation, plus today's fix commits; roadmap rows in the same PRs.
4. R12-8: host verification of the slice, then step 5 DT2 slice gate (needs Em's go), then steps 6-8.
5. RS-C host fixes (5 red runtime tests); B1 and RS-S wait with RS-C for one shared device gate.
6. Device pins for freqshift and dynreg in the `test/fingerprint-pins` PR; T1i full device run.
7. `test:plan` checks and tests for `nx/emulator-budget` and `tools/devreload-services`.

Follow-ups: `fxp_init` deletion check (needs a breakpoint check first), a Service trait contract suite (needs a design), PGO retrain, the complete test run once per batch.

## Decisions taken

- The encoder lead-in is firmware behaviour; the test helper is fixed, not the emulator.
- The fx init hook is not observable in tests; deleting it needs a breakpoint check first.
- `fx_entry` export deferred.
- PCM compares use one relative-peak rule.
- R12-8: all-or-nothing service DSP; pure move (`DK_SPAN` comes with P8d); the capacity cost is accepted; modules and services with a DSP part require coupled execution (zero mode unchanged; `requireCoupledForServices` in `tests/regression/runtime/service-files.ts`).
