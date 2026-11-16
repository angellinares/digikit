# Handover 2026-11-16 evening: structure first, R11 and R12-1 merged, R12-0 probe done

Start from docs/plans/sdk-runtime-roadmap.md, then this file and the morning file HANDOVER-2026-11-16.md. Results are in the plans, findings, ledgers and PRs; this file holds state and next steps.

## Merged 2026-11-16 (PRs 512-545)

| PR | Item |
| --- | --- |
| 512 | crc32 `unsafe fn` (outside contributor) |
| 517 | wasm size cap removed |
| 518 | SHARC object staleness keyed by Selache revision; SDK kernels save DAG1 registers through R |
| 519 | four stale tests fixed |
| 520 | R12 services as `.dks` and P8 designs |
| 521 | rpc waits in emulated time; `emulatorDk` helper |
| 522 | T1g phase 5 slice 1 (adapter tests on the rig) |
| 523 | handover follow-ups: one test lock, samples payload size, per-device cores, FXF_SEND docs |
| 524 | runtime object source hash |
| 525 | T1g phase 5 Python retirement |
| 526 | roadmap progress |
| 527 | rig per-PC profile feeding `wb profile report` |
| 528 | P9 calibration design, R14 adapter cost item, P8 follow-ups |
| 529 | full tier runs `full: true` tests; crash-capture timing |
| 530 | T1d needs on zero DSP |
| 531 | R10 machines linked in |
| 532 | T1e steps 1-2 (tags and owners) |
| 533 | new order: structure before packaging before instrumentation |
| 534 | T1g phase 5b stereo tests on the rig |
| 535 | S6a+ SWSE sample kernel |
| 536 | shadow replay on the rig; T1i rig read hook item |
| 537 | T1e runtime-owners producer and owners fix |
| 538 | nx-tools test fix |
| 540 | native_pgo lane message |
| 541 | R12-7 Transfer installs services: design and mockup (`docs/design/r12-7-transfer-mockup.html`) |
| 542 | R11 boot plan, module states, refusal fixtures (DONE) |
| 543 | R12-0 PC probe and finding 132 (DONE) |
| 544 | samples-store-live checks the payload size and the stored size (follow-up to 523) |
| 545 | R12-1 service crates and template (DONE) |

## Order now (agreed)

1. R12-2 to R12-8 `.dks` steps (R12-0, R12-1 and R12-7 design are done).
2. P8c dispatcher spans at the interface-B boundary.
3. P8d DSP stats window.
4. P9 device calibration.
5. R14 dsp-voice adapter cost.

T1e step 3 owner keys were measured (fx 38 -> 34 files, storage 38 -> 31, shared 38 -> 77) and are held until after R12.

## Branches

No unmerged branches from this session remain. Reference branch kept: `test/t1e-step3-keys`.

## R12-0 PC probe results (finding 132)

- Interrupt-only service code uses load-test-jump stubs and can be unloaded after masking and clearing the slot.
- Task-reachable code needs the entry/exit counter for hot reload.
- Dispatch (DT2), page_desc, value_class and the DN2 SRC and tab hooks install directly.
- DT2 clone.4 and DN2 dispatch stay patched.
- param_code direct install is confirmed with a saved digikit project.

## Other findings today

- crash-capture had been failing since 2026-11-10. The full tier skipped `full: true` tests, which hid it. Fixed in PR 529.
- PR 518 found 81 cached SHARC objects assembled by the old Selache.
- The rig profiler shows the dsp-voice adapter's parameter copy is 25-30% of a machine voice's block (R14 dsp-voice adapter cost).
- S6b SIMD is parked (ceiling 4-5% of shot's block).
- dsp-facts fixture fixed (LIRPTL was register item 108).

## Next

1. Full tier on a quiet machine. The coordinator runs `DIGI_GUARD=off mise -C <lane at origin/main> run test:full` with nothing else running. It now also runs the 9 `full: true` tests that were skipped before PR 529: debug-module, crash-capture, memaudit, safe-mode, debug-guard, upload, upload-faults, dynreg roundtrip, pluck equiv. Triage any red result before fixing.
2. R12-2 format v2 and provider slots (runtime-and-services 13.2), then R12-3 installer and slots through R12-8 SHARC stubs. P8c dispatcher spans follow after R12.

Still open from the morning file: the 8 failed full-tier tasks, the storage device test timeout under load, the PGO retrain, dsp-facts hardware run (needs Em's go).

## Working rules learned

- Merge main before any device run.
- Static-check edited test data against its generator.
- Run `patchkit:objects-check` after any change to module source files, even test-only code.
- Fast-forward the main worktree after merges.
- Name items with id and description.
- A change to what the device reports needs the device test that reads it before merge (PR 523 changed the samples payload size; PR 544 fixed the test that read it).

## Lanes

docs-handover (this file). Other lanes are free. Em removes merged branches and lanes.
