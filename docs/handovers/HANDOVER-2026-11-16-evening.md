# Handover 2026-11-16 evening: structure first, R11 and R12-1 built, R12-0 probe

Start from docs/plans/sdk-runtime-roadmap.md, then this file and the morning file HANDOVER-2026-11-16.md. Results are in the plans, findings, ledgers and PRs; this file holds state and next steps.

## Merged 2026-11-16 (PRs 512-538)

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

## Order now (agreed)

1. R11 boot plan and module states (proven alone).
2. R12-1 service crates and template.
3. R12-2 to R12-8 `.dks` steps.
4. P8c dispatcher spans at the interface-B boundary.
5. P8d DSP stats window.
6. P9 device calibration.
7. R14 dsp-voice adapter cost.

T1e step 3 owner keys were measured (fx 38 -> 34 files, storage 38 -> 31, shared 38 -> 77) and are held until after R12. Reference branch: `test/t1e-step3-keys`.

## Branches built, not merged

| Branch | Lane | State | Next |
| --- | --- | --- | --- |
| `rt/r11-boot-states` | sdk-ui | R11 boot plan and module states. Host green after fixing a double lock in `cleanup_tests` and adding refusal-test tags. | Device proof: refusal tests on both devices, capacity, safe-mode, needs, boot-restart, gate-smoke, gate-pins. Then merge. |
| `rt/r12-1-service-crates` | docs-design | R12-1 service crates and template: `samples` and `lfo` crates, template, structure check `patchkit service-check` (run through `module:check -- service/NAME`), FakeCore tests. R11 merged in. Host green. It is slightly more than a move (Em accepted): samples inlining, `peakwin::Window` keeps its core pointer (+8 bytes bss), text +0.15%. | After R11 merges: device batch of samples, lfo, fx, machines tagged tests and gate-smoke. Then merge. |
| `tools/r12-0-pc-probe` | sdk-gen | R12-0 PC probe tool and check results (below). | Write the finding, land the probe. |

## R12-0 PC probe results

- Check 1 (task switches from interrupts): no task switch happens from an interrupt handler at IPL >= 2. Switches entered from an interrupt are vector 205 from IPL 0 (the design's vector 191 was wrong). Trap #0 switches from tasks with interrupts masked exist (about 8,000 per run).
- Decision (Em): interrupt-only service code uses load-test-jump stubs and can be unloaded after masking and clearing the slot. Task-reachable code needs the entry/exit counter for hot reload.
- Check 2 (first fire of vtable-only hooks): DT2 dispatch, page_desc, value_class and DN2 source_key, source_code, source_draw first fire after main-OS entry, in one task at IPL 0, so direct install works.
- param_code also runs before main-OS entry, in the boot project decode loop (DT2 FUN_400e64dc, DN2 FUN_400e7eb0). Finding 131 says the boot decode passes -1 (stock path). A run with a saved project using pluck is in progress to check that no digikit type reaches stock code.
- DT2 clone.4, DN2 dispatch, source_turn and tab_label_test were not reached yet. The scenario rerun is in progress.
- A finding is to be written with the final results.

## Other findings today

- crash-capture had been failing since 2026-11-10. The full tier skipped `full: true` tests, which hid it. Fixed in PR 529.
- PR 518 found 81 cached SHARC objects assembled by the old Selache.
- The rig profiler shows the dsp-voice adapter's parameter copy is 25-30% of a machine voice's block (R14 dsp-voice adapter cost).
- S6b SIMD is parked (ceiling 4-5% of shot's block).
- dsp-facts fixture fixed (LIRPTL was register item 108).

## Next

1. R11 device proof, then merge.
2. R12-1 device batch, then merge.
3. R12-0 finding, and land the probe.
4. Full tier on a quiet machine (now includes the 9 formerly skipped tests).
5. R12-2 format v2 and provider slots.

Design waiting for Em: R12-7 Transfer installs services mockup (in progress).

Still open from the morning file: the 8 failed full-tier tasks, the storage device test timeout under load, the PGO retrain, dsp-facts hardware run (needs Em's go).

## Working rules learned

- Merge main before any device run.
- Static-check edited test data against its generator.
- Run `patchkit:objects-check` after any change to module source files, even test-only code.
- Fast-forward the main worktree after merges.
- Name items with id and description.

## Lanes

sdk-ui (R11), docs-design (R12-1), sdk-gen (R12-0 probe), t1-tests (pgo message fix), docs-handover (this file). Free: sdk-fixtures, sdk-design, docs-plans. Em removes merged branches and lanes.
