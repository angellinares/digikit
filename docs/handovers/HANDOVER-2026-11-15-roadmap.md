# Handover 2026-11-15: roadmap (S5, R13, P8a, R8, lfo flake)

Start from docs/plans/sdk-runtime-roadmap.md, then this file. Results are in the plans and findings; this file holds state and next steps.

## Merged today

| PR | Item |
| --- | --- |
| 481 | T1a test-selection baseline: over-selection fixes |
| 482 | S5a SDK DSP contract, derived DSP cost, including the shot sample-driven conformance case (5,057 DT2 / 5,266 DN2 instructions) and the runtime FX_DESC_BYTES fix (28 -> 24) |
| 484 | `coupled_ab --image`: A/B of two firmware images |

## Branches built, not merged

| Branch | Lane | State | Next step |
| --- | --- | --- | --- |
| `sdk/s5b-defuse` | `~/src/digi/lanes/sdk-fixtures` (that lane's checkout is now on `fix/wait-for-boot-race`: switch back to merge S5b) | S5b SDK DSP contract is complete: dksharc def/use; finding 130 [V] claims 1-7 (claim 8 [D]); register contract in sdk.toml (clobber, keep, stack, never_write; never_write: B7 L6 L7 M5 M6 M7 M13 IMASK IMASKP IRPTL MMASK TPERIOD TCOUNT FLAGS; stack: I7 PCSTK PCSTKP); rules check warns only, content modules only; 0 warnings on real modules, 13 on the test/dsp-facts fixture (checker gap: bit-level MODE1). Merged with main once; the gate is red only on the lfo flake. | Merge main again (main gained SHARC core PRs 485-487, including "a write of Bn also loads In"), re-run the gate after the flake fix, PR. |
| `rt/r13-storage` | `~/src/digi/lanes/sdk-gen` | R13 storage interface is complete, reviewed, device tests green; merge approved by Em. The gate stopped on the lfo flake (run with --nx-bail). Three tracked build/*.dko.json files in that lane show only source_sha256 drift (uncommitted). | After the flake fix: merge main, gate, PR, merge. |
| `prof/p8a-spans` | `~/src/digi/lanes/sdk-design` | P8a spans is complete, reviewed, A/B passed. It contains an early copy of the A/B tool (merged since as PR 484), so merge main first. Doc fix pending: docs/INSTRUMENTATION.md `wb profile spans` example says 1,000,000 Hz; the clock is 132 MHz. | Merge main (sdk.toml overlaps S5b: regenerate), gate, summary to Em for OK, PR. |
| `rt/r8-spike` | `~/src/digi/lanes/sdk-ui` | R8 whole-path spike is done, finding 131. It stays unmerged; R12 services as `.dks` builds on it. Contains: format and bind (patchkit), provider resolution, import resolution against providers, the installer, the device test core/runtime-provider-tests. | Keep as the base for R12. |
| `fix/wait-for-boot-race` | `~/src/digi/lanes/sdk-fixtures` | The lfo flake fix, in progress (see next section). | See next section. |
| `docs/roadmap-2026-11-15` | `sdk-ui` | Superseded by this handover PR. | Can be deleted. |

## The lfo device test flake (blocks S5b, R13, P8a)

What we know:
- The test core/runtime-lfo-tests ("the lfo service owns lfo_shape ... a second owner is refused") fails on main: on DN2 since R7 generic records and service registry, on DT2 later.
- The runtime behaves correctly: the planner refuses B with E_PLAN_KEY and boot reaches the UI.
- Root cause: the emulator delivers USB MIDI by calling the firmware's rx callback as soon as USB is enabled, before the firmware stored its callbacks in this boot. The registration function at 0x4000434a stores two callbacks into .bss slots (DT2 0x409659d0 and 0x409659d4; DN2 1.12 0x4058f218 and 0x4058f21c). A host request during early boot runs the callback against uninitialised state. The callback never returns where the emulator expects, so `usb.active` stays set and every later delivery is refused (link dead, CPU alive). Real hardware is not affected.
- Fix (uncommitted in lane sdk-fixtures): packages/emulator/boot/src/midi.rs and runtime.rs gate delivery on the slots (addresses read from the store instructions); `usb_due_in` returns None until the callbacks are registered.
- A deciding run is in progress: the original lfo test with only the emulator fix, 5 runs on DN2 and 1 on DT2. The earlier harness changes (guardRestart, lfo test on debug-module events) are saved as a patch in the session scratchpad and set aside.

Next:
- If the runs pass, commit the emulator fix alone. Run a targeted device list (Em prefers a targeted route over the full gate of about 2 h for emulator changes): lfo x5, one heavy-USB test per device, upload, safe mode, one machine load, the debug module. The full set runs in the next batched full-tier run. Then PR and merge, then re-run the S5b and R13 gates.
- If they fail, the waitForBoot no-LOG race is real on its own: the race fix with debug-module events also hit "log: E_PROTO" during the restart.

## Decisions taken today

- S5a shot sample case: shot gets a sample-driven conformance case, because the rig plays no sample and the derived cost is otherwise a floor.
- T1a include-scan fixes (PR 481).
- Finding 130 marks: claims 1-7 [V], claim 8 [D].
- S5b register contract: classes clobber, keep, stack, never_write. Corrections: I7 is class stack, I6 and B6 are keep, I12 is clobber; the check applies to content modules only; review fixes applied; register classes set for IMASK, USTAT, PCSTK and similar.
- R13 storage interface design:
  - Storage trait in the Platform composition; roots are the store and Samples.
  - The sample pool is device-neutral; settings stay on the block path.
  - At most 2 listings from the arena; the caller owns the listing state; steps are Entry, Pending, End.
  - Abandoned listings are released after 10 s with E_LOST.
- P8a spans design:
  - Spans come from the runtime's ColdFire call sites; the debug module owns the sink.
  - Clock is the stock DTIM0 counter, read only (no timer is free).
  - Protocol ids are 0x1e and 0x1f; runtime 0.8.1.
- The A/B tool (PR 484) lands before the P8a merge.
- R8 whole-path spike plan and its decisions (finding 131).
- R9 `fx` linked in through R12 services as `.dks` proceed; R12 is scoped to movable hooks, and boot-thread and interrupt hooks keep image stubs with pointer slots.
- The roadmap adds S6 SDK sample kernel and T1c Host contract tests as own rows.

## Follow-ups (not started)

- The workbench Transfer pane waits for a boot without LOG after an upload restart (dktransfer.ts about line 499). On a real device the same race exists. A per-boot value (boot sequence) in the INFO reply would fix it and the remaining first-install race in tests. This is a protocol addition and needs Em.
- FX_DESC_BYTES and other SDK constants are hand-written in Rust: generate them.
- The dksharc check needs bit-level MODE1 tracking before the check switches to refuse.
- Take short-lived listing blocks from the top of the arena (R13 storage interface fragmentation).
- Measure the UI-task stack after R13 storage interface.
- The emulator ready inputs were missing from the shared out/ (rebuilt for DN2 and DT2).
- PGO profiles are stale after today's merges: retrain once per batch.
- Profile pane mockup (P8 Profile pane).
- The `wb` CLI lacks --help and lists only some profile subcommands.

## Next on the roadmap

- Finish the flake fix and the three merges (S5b, R13 storage interface, P8a spans), with a full-tier batch afterwards.
- The Profile pane (mockup first).
- T1c Host contract tests.
- S6 SDK sample kernel (after S5b).
- T1e service-scoped selection.
- Then R9 `fx` linked in.

## Notes

- Lanes and branches:
  - sdk-fixtures: `fix/wait-for-boot-race` (the `sdk/s5b-defuse` branch also lives here).
  - sdk-gen: `rt/r13-storage`.
  - sdk-design: `prof/p8a-spans`.
  - sdk-ui: `rt/r8-spike`.
  - docs-handover: this branch.
- Em removes merged branches and lanes.
