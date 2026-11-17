# Handover 2026-11-17 late: R12 batch merge-proof gate running, review fixes applied, RS-C built

Start from docs/plans/sdk-runtime-roadmap.md, then this file. It follows HANDOVER-2026-11-17-evening.md (same day; its merged-PR list and decisions still hold). Plans: docs/plans/runtime-slimming.md (RS-S, RS-C), docs/plans/digikit-hot-reload.md (B1), docs/plans/test-architecture.md (T2).

## Branches and lanes (state about 15:15; nothing pushed)

| Branch | Lane | State |
| --- | --- | --- |
| `rt/r12-batch` | sdk-design | Head a95e257f9. Fixes from the device re-run: fx-bipolar waits for the DSP row word; fx-pages uses a relative trigger; fx-kit-persist guards fx reads on the stock boot and has no init-hook counter check (finding 61: boot kit init runs before fx.dks binds; Load Kit does not call the initialiser); fx evidence requires audio only when a suite says so (one resolver, suites explicit); service-check errors (duplicate slot, missing adapters folder); hook_ab compare bound relative to the reference peak (2**-20); machine-rows: expectNoFault, PNG on failure, turns repeat bursts (max 4) past the firmware encoder lead-in filter (finding 03: the emulator matches hardware, our bursts were too small). Merge-proof gate running since 14:41: host stages green; device subset `mise run test:subset -- S:machines A:core/fx A:core/dynfx A:core/fxpages C:loading C:storage S:samples --run` (about 100 tasks). Expected failure: fx-bipolar bias-up compare (the TS helper still asserts ulps <= 2), fixed on `fix/batch-review-2` |
| `fix/batch-review-2` | sdk-fixtures | One PCM compare rule (hook_ab `ok` verdict, `--rel-bound`; per-test ulps checks removed; pk_digikit_fxpage deleted); evidence writes the overlay before the fault check; shared evidence collector; captureRemoteOled returns its path; svccheck duplicate slot only for overlapping device lists (Rust tests). Merges into the batch after the gate, then the fx subset and machine-rows run again (about 40 tasks) |
| `fix/dyn-dsp-hang-gallery` | sdk-fixtures | Rig conformance gallery check skips machines with param_rows = 0; single-source `galleryGaps` in progress |
| `test/pyramid-moves` | | fx-bipolar gain law on the rig (10 cases plus a MIX-during-run case; new check `scales-input`); 5 device PCM compares removed. Reviewed |
| `test/machine-rows-host` | | DN2 clamp bursts reduced to one Gamma clamp; table read-backs stay. Reviewed |
| `test/unit-gaps` | t1-tests | Rebased; about 49 host tests green. Two runtime bugs fixed: `report_dsp` changed an active module's record; module files skipped `role_ok` (a content module with PROVIDES/HOOKS loaded). Reviewed |
| `test/fingerprint-pins` | docs-handover | Builds, host tests green. Rig pins v2 (analog, pd, pluck bit-identical; bd DT2 new; approved from numbers). Blocker: dynreg equiv needs device pins `dynreg/tests/pins/*.pcm.json`; write them from the gate's kept captures with the pin CLI (numeric report) in the same PR. freqshift fingerprint pins not written (the same-as cases name no device) |
| `nx/dks-tests` | t1-tests | rust-tests targets for standalone service crates (found by `[workspace]`); `test:changed` runs objects-check when patchkit is affected. Reviewed, doc nits fixed |
| `b1/bind-resolver` | sdk-gen | B1 plus review fixes (zero-address guard `call_if_bound`, docs) |
| `rs/storage-tests` | docs-plans | RS-S plus review fixes; the assembly test reads sources (sdk.toml, device file); object hash refresh |
| `rs/core-leftovers` | sdk-gen | Head 2394da3f7. RS-C C2(b)-(e), C3 (partial), C4 on top of the fixed B1, plus review fixes (kind-free rule scans whole files and abi; safe machines.types migration; DECLARE pass before voice rows; shared builder module; golden from pre-refactor code). Compile-only so far. Deferred: `fx_entry` to `dkrt_export(name_hash)` (`Service::fx_entry` stays) |

## In progress

- R12-8 design (architect).
- `dk:dev` service files in `--store`.
- Emulator budget cap for gates (load reached 79 with 12 emulators).
- `galleryGaps` single source.

## Next, in order

1. Gate result (read failures one file at a time).
2. Merge `fix/batch-review-2` and the gallery fix into the batch; run the fx subset, machine-rows (4 emulators) and rig conformance again.
3. Device pins for dynreg equiv from the kept captures (pin CLI, numeric report), in the `test/fingerprint-pins` PR.
4. Batch PRs in order: prep, R12-5, R12-6, T2 lanes, assembly layer, fixtures, consolidation, plus today's fix commits; roadmap rows in the same PRs.
5. Host verification of RS-C and `nx/dks-tests`.
6. Rebase B1, RS-S and RS-C onto main; one shared device gate.
7. R12-8 after its design is agreed.

Follow-ups: `dk:dev` services, emulator cap, `galleryGaps` dedupe, fx init hook deletion check (needs a breakpoint check first), PGO retrain, the complete test run once.

## Decisions taken

- The encoder lead-in is firmware behaviour; the test helper is fixed, not the emulator.
- The fx init hook is not observable in tests; deleting it needs a breakpoint check first.
- `fx_entry` export deferred.
- PCM compares use one relative-peak rule.
