# Handover 2026-11-10: test speed and clean-up

Read AGENTS.md, then [the test speed plan](../plans/test-speed.md) (steps and evidence). This file holds state and next steps only.

Goal: the gate and module checks finish sooner through less work, the MacBook stays usable, and test code gets simpler. Em's standing rules for this work:

- Simplify and apply the deletion test. A deletion of a test, assertion or wait gets an independent adversarial review first: list each removed check and find what still proves it at the same level. A host test with a fake does not replace the only device proof of real firmware wiring. A different image or adapter means not a duplicate.
- A targeted test run replaces the device part of the gate, not the static part. Before any merge run `pnpm nx:pilot run-many -t lint,fmt-check,rust-fmt,typecheck,python-checks` on the final tree, and grep for other users of anything deleted.
- Test-helper changes do not need a full gate: run the files the change can break, and say what was skipped.
- Tick plan items in the commit that finishes them. Ask before push, PR and merge. Em reviews PRs from this work before merge unless told otherwise.
- Give a measured estimate and a log path before a long run.

## Merged

| PR | What |
| --- | --- |
| #275, #276 | boourns: overdrive curves; TB-303 machine, machine contract 1.6 (gate, voice states zeroed at first note-on), runtime 0.5.6 |
| #361 | Zero DSP by default (Em) |
| #362, #363 | Two old gate failures; `DEVICE_SYX` moved so a `pk_dkrt.py` edit stops selecting the `tests/digikit` scenarios |
| #364 | The test speed plan |
| #367 | Shared boot snapshots, one per daemon build and device ([finding 118](../findings/118-device-test-reload-cost.md)); `settleUi` deleted; emulator ceiling one core each (12 on 12P+4E) |
| #369 | Gate prints a line per finished task; `mise run test:history`; the merge tier selects device tasks with `nx affected` |
| #370 | Vitest 5.1.0 |
| #371, #372 | Equivalence scenarios in TypeScript, each machine's rows in its own `tests/`, shared runner `tests/regression/runtime/equiv.ts` |

## Open

- **#373** `card.rs` decisions as pure functions with 14 host tests. No device test changed. Em to review. The list of device cases it could make redundant recommends deleting none.
- **#374** `fx-track-route` and `shot-dn2` split into per-file targets (5 and 3 files), assertions moved. Em to review. The first runs of `shot-dn2-audio` and `shot-dn2-supersede` each failed once and passed on rerun; cause not found.
- **`test-speed/device-audit`**, PR open for Em: `shot-picker` rewritten to one glue test with the hook steps restored (still needs the DSP); `docs/plans/ledgers/device-test-audit.md` (all 50 device files classified); bootscreen REC held test kept; targeted waits in `sample-playback`, `stock-pool-borrow`, `stock-sampler-lifetime`. `installModules` is unchanged from main. Checks: `shot-dn2` 3 of 3, `shot-picker`, `shot-dt2`, `tom/tb303`, the three proof files once (the `proofs` Vitest project has no Nx target), repo-wide static checks.

## Observer state (branch `test-speed/observer`)

Built: `INPUT_TAKEN` (kind 41, `digikit/remote`), the `Events` helper (`events.until`), `setSmp` through the remote on DN2, and `PARAM_CHANGE` (kind 42, `digikit/debug`: the selected track's Sound words through anchors `cf.project.fn.settings` and `cf.project.fn.track_sound`, import `dkrt_selected_params`, [finding 119](../findings/119-param-change-observer.md)). `PARAM_CHANGE` is proven by a knob-turn device test in `modules/tools/debug` on both devices. Not adopted in `setSmp`: a turn of SMP changes no word of the Sound array on DN2, with or without TRIG 1 held; where it is stored is open. Module parameters such as SMP are stock parameter rows whose turn changes no word of the Sound array, so PARAM_CHANGE does not see them. The baseline event (index 0xffffffff) tells a host that the watch has its snapshot. No `paramChange` helper predicate was added: nothing uses it. Not built: the "ready for input" wait after an install.

## The open `shot-dn2` problem

The SMP step can get stuck. The DSP word reaches `0x100` and does not move while the knob keeps turning (43 more detents with a limit of 64). Runs on 2026-11-09 and 2026-11-10: no post-install wait 0 failures of 12; with a post-install wait 9 of 19, at mixed load. The mechanism is unknown.

Hypothesis (not confirmed): `setSmp` reads `_dv_params`, which is rewritten only while a voice renders, and holds TRIG 1 for one note-on, so a voice that ends before the knob reaches the target freezes the word. Check on 2026-11-10 (temporary log of the word and the output level at each detent, removed again): 6 runs of `shot-dn2-supersede` all passed, no failure to observe. In every run the output stayed audible (window RMS 0.016 and above) until the end of each turn. Turning 0 to 2 takes 1.8 s (22 detents) of the 5 s T sample, and 2 to 0 takes 2.1 to 2.3 s (25 to 27 detents) of the 4 s L sample. One pool entry is 16 detents (the word moves in 0x10 steps), and the word stays at its old value for the first 15 detents of a turn. So the voice does not end inside the passing runs, and the margin of the second turn is about 1.7 s. A failure would need that turn to take almost twice as long in emulated time (dropped detents, pacing). Not applied: a longer Y sample or a retrigger, because no passing run shows the voice ending. If the failure comes back, log the same two values (`pcm.tap` report `window_rms` and the word) at each detent and look at the level when the word stops.

The review of #367 found that `settleUi` was the only wait after a module install: `installModules` stops at the restart log entry and the module list. The firmware-state replacement passed `main_ui_reached` in about 20 ms and the test still failed, so that state does not mean "ready for input". The wait is out of `installModules` again for now.

## Next steps, in order

1. Em reviews #373, #374 and the `device-audit` PR.
2. Observer, rest of plan step 3 stage 2 (INPUT_TAKEN, `events.until` and PARAM_CHANGE exist): a "ready for input" event for the post-install wait, and an event for module parameters such as SMP (they are outside the Sound array; the store of a latched run-time parameter is open). The frozen-word failure has not reproduced in 6 runs; keep the per-detent level log ready for the next failure.
3. Measure one real gate that touches core against the 111-minute baseline of 2026-11-09 (`mise run test:history`). No full gate was run for the changes above. The 12-emulator ceiling has not been seen at full load.
4. Plan step 4: key device tasks on built bytes. It needs cacheable `pack` first. A core change still selects all 41 device tasks.
5. Audit rows for Em to decide, each deletion through the independent review first: `settings` copies and safe-mode copy pairs; `freqshift` and `fx-pages` trims; other rows in the ledger. Remaining items: wall-time test timeouts to emulated time, the ui-demo picker test.

## Decisions pending from Em

Core-change policy (plan step 5). Observer module design. A GitHub issue for boourns's notes on overdrive output level and curve choice has not been filed.

## Known limits

- `nx affected` sees git changes only: after a change to untracked inputs (firmware, ready snapshots, override binaries) run the named targets or the full tier (AGENTS.md).
- `tools/patchkit` docstrings still name the old `fx-track-route` file: those files are Nx `sources`, and editing them invalidates caches.
- `equiv-dyn-pluck.toml` is not a test. It stays as the input of `tools/devtools/sharc_matrix.py`.
- Measurements from the 2026-11-09 gate are in `out/runs/lanes/pr-275-gate-outputs/` (not committed).

## Lanes

`ts-settle` (device-audit), `ts-slots` (#373), `ts-reload` (#374), `main-fixes` and `test-speed` (free). Em removes worktrees; give the command when asked.
