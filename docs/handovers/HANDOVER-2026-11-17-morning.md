# Handover 2026-11-17 morning: R12 batch on the emulator, assembly test layer, hot reload plan

Start from docs/plans/sdk-runtime-roadmap.md, then this file. Designs: docs/plans/runtime-and-services.md 13.2 (as on branch `rt/r12-batch`), docs/plans/digikit-hot-reload.md, docs/plans/profiling.md 13.1 (P8d, on branch `prof/p8d-dsp-stats`), docs/plans/test-architecture.md (T2). This file holds state and next steps.

## Merged since the night handover

| PR | What |
| --- | --- |
| 554 | R12 stack A: format v2, Loader, installer, service ABI, `lfo.dks` (gate A green on DT2 and DN2) |
| 556 | roadmap and handover update |
| 559 | R12-7 Transfer installs the services a module needs (store-format green on both devices) |
| 560 | hot reload plan (active again), finding 135 (firmware facts for it), roadmap rows B1, R12-8, H, size; prettier fix on `emulated-clock.ts` |

## Branches and lanes (nothing below is pushed unless noted)

| Branch | Lane | State |
| --- | --- | --- |
| `rt/r12-batch` | sdk-design | integration stack: R12 prep (one `dkrt_info_table` with slot/data kinds, `cap_<kind>` capacities, `Platform::info`, bind-made tails, HOOKS device mask, service template and `service-check`), FW_READY, R12-5 `fx.dks`, R12-6 `machines.dks` (SLICE-clone hooks deleted, clone.5 kept, DN2 frametype stub + slot), T2 lanes (FX contract suite, machine rig references with fingerprint/plot tooling, card/settings duplicates), harness fresh-session-after-failure, review fixes, main merged. Host-green. Device: store-format, providers, settings-copies pass; machine and FX modules refused with -73 E_MISSING (service files not loaded) in the last run (logs `out-batch6-*.log`). Head ae626caf8 |
| `test/assembly-layer` | docs-plans | T2 step 4 host assembly tests, one wip commit 068499cb7, not compiled (written blind: expect compile fixes). `loader/tests/assembly_tests.rs` reads the real `dist/` packs, bound services, `runtime.dko.json` and a new image info file (`patchkit apply` plans gain an `info` list; `tools/nx/device-images.mjs` writes `dist/bytes/images/<dev>/set-<set>.info`), runs search, plan, role checks, install and a DSP-less module load. `core/runtime:test-assembly` (cached, narrow inputs); device, conformance and regression targets dependsOn it. The TS ABI-objects test moved into it. Two patchkit build-rule tests (path-dependency sources are already hashed; if the real-crate test fails, add SVC_ABI_FILES to the hash). Notes: test builds always add a machines row (`svc_rows::test_machines`), the test removes it for its run; the device SVC_ABI comes from the immediate in `runtime.dko.json` (host sizes differ). First run should reproduce the -73 on the host |
| `test/machines-ui-device-tests` | t1-tests | removes the three patch-time machine fixtures (machine-clone, dn2-clone, dn2-pages) and their smoke cells; new run-time cases `machine-rows-dn2` (test/dyn-dn2), `machine-rows-dt2` (test/sine); two-page fixture `test/dyn-dn2-pages` with `machine-pages-dn2.device.test.ts` (no page-index word exists, so the page shows through which Sound word a knob edits; its `[uses.dn2]` is hand-computed: run `mise run module:check test/dyn-dn2-pages` to regenerate); patchkit refuses patch-time machine `[[entry]]` rows. Ledger `docs/plans/ledgers/smoke-machines-cells.md`. Head d2b188b3d; the deletion commit 3c7b92430 is no longer last (reorder it last before merge). Not built; needs `cargo test -p patchkit`, `@digi/web:test-wasm`, the patchkit pytest files, a device run and an independent review |
| `test/coverage-report` | sdk-fixtures | `mise run coverage`: cargo llvm-cov (0.9.1, installed globally) + vitest v8, per-project HTML, `out/coverage/index.html`, `summary.md`. Not run; needs `pnpm install` for `@vitest/coverage-v8`; check the Rust HTML link path and demangled names on the first run |
| `prof/p8d-dsp-stats` | sdk-ui | P8d host-only parts done (window-3 control, stats mirror, `dk_dsp_stats_*` imports, `PROFILE_DSP` 0x1b, dk-session and dkprof readers). SHARC phase waits for R12-8. Rebase onto main later (runtime version conflict expected) |
| `rt/r12-batch-fixes`, `rt/r12-prep-svc-template`, `rt/r12-5-fx-dks`, `rt/r12-6-machines-dks`, `test/t2-*`, `test/harness-fresh-session-after-failure` | various | all merged into `rt/r12-batch`; keep until the batch lands |

## Next (in order)

1. Build and run `test/assembly-layer` on the batch head; fix the -73 cause on the host; merge the layer into the batch.
2. Build the other lanes (coverage, machines fixtures) with at most two Rust builds and none during a device window.
3. One device run on the batch: the machine and FX files (fixture suite, frametype, dynreg, fx adapters, lfo, refusal), the new machine-rows and two-page cases, two-notes experiment (`DIGIKIT_EXPERIMENT=two-notes`), `mise run module:check fadedlimes/bd` then the bd DT2 re-pin. Smoke separately afterwards (the three machine cells are deleted on the fixtures branch). Run with `--nx-bail=false` so one failure does not block the rest.
4. Independent reviews: the smoke-machines-cells ledger. Em approves: rig pins and plots (`out/rig/` in the docs-handover lane), freqshift rig pins, the DT2 test/sine PCM hash (printed by the gate), the two-notes verdict.
5. Merge the batch in order (prep, R12-5, R12-6, T2 lanes, assembly layer, fixtures), one subset proof shared by the stack; then the PGO retrain. Update the roadmap R12/T2 rows in the same PRs.
6. Then: runtime size quick wins (samples CORE, LOADER, files JOBS to bss, about 50 KB data); B1 bind-time firmware addresses + hot reload S0 exit convention; R12-8 (decisions D1-D5 accepted); P8d SHARC phase; P8c; P9 (hardware go); R14; hot reload S1-S5 with `samples.dks`.

## Decisions taken this session (recorded in the plans)

- SLICE-clone support dropped; no manual-slice clones.
- Service design: one mechanism per concern; capacities from `cap_<kind>` info entries; the runtime names no service or adapter table; R12 finishes as built.
- R12-8 before P8d's SHARC phase; D1 rows stay in the image, D2 DK_SPAN in the service dispatch, D3 DSP slots via window 0, D4 DN2 stereo hooks stay, D5 heartbeat in the stub.
- Hot reload: gate plus counter per ColdFire hook, no opt-out; refuse during transport; refuse a swap that does not fit twice; new hook sites need a restart; S0 with B1, S1-S5 after R12-8.
- P8 decisions 1-7 accepted (P8d after R12-8 now).
- Test pyramid: unit (logic), assembly (seams between real artifacts), device (fixtures at real boundaries only). Two-page cycling covered by a run-time fixture. Boundary register skipped.

## Working rules learned

- Wait for a quiet machine with exact process checks (`pgrep -x digi-emud`, `pgrep -f 'nx[.]mjs run'`); a `pgrep -f` pattern matching its own command line never ends. Other sessions run device tests back to back.
- Never edit a worktree whose tests are running; lane agents edit in another lane.
- A changed ABI boundary file needs the fingerprint and every service object regenerated (assembly tests and the build rule now cover it).
- Do not put a test's setup into inputs every device test hashes.
- Sub-agents are forced to Sonnet by a hook; review runtime and ABI work closely.
