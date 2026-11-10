# Handover 2026-11-10 (3): test speed

Read AGENTS.md, then [the test speed plan](../plans/test-speed.md) (steps and evidence). This file holds state and next steps only. It follows [the second handover of this day](HANDOVER-2026-11-10-test-speed-2.md).

Test-speed work is paused here: work moves to modules, runtime and SDK. Em's standing rules for this work:

- Simplify and apply the deletion test. A deletion of a test, assertion or wait gets an independent adversarial review first: list each removed check and find what still proves it at the same level. A host test with a fake does not replace the only device proof of real firmware wiring. A different image or adapter means not a duplicate.
- A targeted test run replaces the device part of the gate, not the static part. Before any merge run `pnpm nx:pilot run-many -t lint,fmt-check,rust-fmt,typecheck,python-checks` on the final tree, and grep for other users of anything deleted.
- Merges use the affected gate plus the static checks. The full tier runs once per batch of merges, in a background lane. A red batch is bisected.
- Test-helper changes do not need a full gate: run the files the change can break, and say what was skipped.
- Tick plan items in the commit that finishes them. Em reviews PRs from this work before merge unless told otherwise.
- Rank test-speed work by measured wall-time saving per gate and do the biggest first.
- Read the gate trace (`out/runs/gate/<stamp>/`, `gate-history.mjs --trace <dir>`) and the Nx task history (`mise run test:history`) before guessing where gate time goes.
- Give a measured estimate and a log path before a long run.

## Merged this session

| PR | What |
| --- | --- |
| #395 | `NX_PARALLEL` is twice the emulator ceiling. Gate tier runs write a trace (`nx-profile.json`, `slots-trace.json`, merged `trace.json`) and print wall, task-min, slot wait against run time, slowest by run. AGENTS.md, TESTING.md and the agent guard state the batch full-tier rule. |
| #396 | The gate emulator targets are cached (smoke, tier-sweep, dt2-tier-sweep, pins, dt2-tone, card, fast-roots). Replay proven (rerun 3.3 s). |
| #397 | Device tasks hash built bytes, not sources (plan step 4 main part). Profile-edit selectivity and the daemon key are not done. |
| #398 | The no-progress watchdog covers loads. Fixed wall limits became emulated waits and about 35 per-test timeouts are gone (plan step 6). Independent review found 4 gaps; all fixed. |
| #399 | Batch fixes from the first full tier on daa6062b (stopped at 53 min, load 49): gate suites that start emulators take emulator slots; the emulated clock of waits follows the ColdFire (`status.elapsed`), so a deliberate DSP stop no longer looks stuck; boot progress counts during a restart; conformance boot waits count emulated time. |

## Measurements

Details are in the plan's [Measurements](../plans/test-speed.md#measurements) section.

- Full tier of 2026-11-10 17:08: device tasks 984 task-min, 303 inside Vitest, about 595 waiting for an emulator slot. Only 3 to 5 emulators were admitted (admission needs about 6 idle cores) while other branches' proof runs shared the slots.
- First full tier on daa6062b: stopped at 53 min. Twelve gate-suite emulators ran at once (load 49) and four device files failed on guard and wall-limit bugs of #395 and #398; fixed in #399.
- Full tier on ea8132e6 (#395 to #399): green, wall 47.8 min (was 93 min on 2026-11-10 17:08), 359 tasks, 1071 task-min; emulator slots: wait 767 min, run 279 min (about 5.8 emulators on average). Every task ran cold, so the gain is scheduling only; caching (#396) and byte keying (#397) pay from the next change on. Trace: `out/runs/gate/2026-11-10T12-18-01-373Z/` in lane ts-slots; log `out/runs/lanes/full-2026-11-11-batch.log`.
- A lane without a coupled build pays a cold `sharc_gen` build (about 13 min, 6 GB) before any gate suite runs; daemons come from the shared store, coupled binaries do not.

## Open branches

None open from this session. `test-speed/progress-deadline` is superseded by #398 and can be deleted, and so can the backup branch `test-speed/watchdog-deadlines-wip`.

## Next steps, in order

1. Measure the first ordinary PR after this batch (a module or runtime change) with the gate trace: how many device tasks replay from the cache.
2. Measure the next real core-change gate (merge tier) with the trace, and compare with the 126.8 min of the earlier core gate. Run gate-suite proofs only in a lane that already has the coupled build.
3. Decide on CPU-measured emulator admission. Run the 4/6/8/10/12 concurrency throughput run first.
4. Size the stand-in DSP peer statically (`sharc_peer.rs`, `dsp-link.c`).
5. Then the deferred list in the plan's "Later" section.

## Lanes

Used this session: `ts-slots`, `nx-nojob`, `ts-py`, `test-speed` (under `/Users/em/src/digi/lanes/`). Em removes worktrees; nothing was removed here. Keep one warm lane for the next session.

Removal commands for Em:

```sh
git branch -D test-speed/gate-trace test-speed/cache-gate-targets test-speed/device-bytes \
  test-speed/watchdog-deadlines test-speed/watchdog-deadlines-wip test-speed/progress-deadline fix/clock-stuck-during-boot
git push origin --delete test-speed/gate-trace test-speed/cache-gate-targets test-speed/device-bytes \
  test-speed/watchdog-deadlines test-speed/progress-deadline fix/clock-stuck-during-boot
```
