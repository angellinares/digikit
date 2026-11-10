# Handover 2026-11-10 (2): test speed

Read AGENTS.md, then [the test speed plan](../plans/test-speed.md) (steps and evidence). This file holds state and next steps only. It follows [the first handover of this day](HANDOVER-2026-11-10-test-speed.md).

Goal: the gate and module checks finish sooner through less work, the MacBook stays usable, and test code gets simpler. Em's standing rules for this work:

- Simplify and apply the deletion test. A deletion of a test, assertion or wait gets an independent adversarial review first: list each removed check and find what still proves it at the same level. A host test with a fake does not replace the only device proof of real firmware wiring. A different image or adapter means not a duplicate.
- A targeted test run replaces the device part of the gate, not the static part. Before any merge run `pnpm nx:pilot run-many -t lint,fmt-check,rust-fmt,typecheck,python-checks` on the final tree, and grep for other users of anything deleted.
- Test-helper changes do not need a full gate: run the files the change can break, and say what was skipped.
- Tick plan items in the commit that finishes them. Em reviews PRs from this work before merge unless told otherwise.
- Commits, PRs and merges are allowed at the coordinator's judgement after the checks. The full tier runs before each push or PR to main.
- Rank test-speed work by measured wall-time saving per gate and do the biggest first.
- Read the Nx task-history timeline (`mise run test:history`, the Nx task-history database) before guessing where gate time goes.
- Give a measured estimate and a log path before a long run.

## Merged this session

| PR | What |
| --- | --- |
| #382 | Per-module device constants (`uses` on `[[gen]]`). Generated includes are 1704 lines smaller. Architecture change, no selection gain. |
| #383 | Module-subject Python tests moved from `tests/patchkit` into their modules. `py_select` selects by the nearest `module.toml` (core adapter tests were never selected before). |
| #384 | Core-change policy B: narrow merge cache keys, the subset in `project.json` `metadata.mergeTests`, `test-device-all` twins, full tier before each push or PR. |
| #385 | Two Python gate failures fixed: stale render-input review counts; a machine-state pcode test deleted after review. |
| #386 | Each device test file runs as one task per device (`DIGI_DEVICE`, `--dt2` and `--dn2` halves). Safe-mode now lists `test/c-demo`. |
| #387 | `listen({ fromBoot: true })` fixes the DT2 `PARAM_CHANGE` race: the cursor is taken after the module's self-logged baseline. The baseline is now INFO. |
| #388 | No gate task runs alone (the only measured cost was exclusive tasks). |
| #389 | nx-tools host tests 93 s to 28 s: re-proofs of declared inputs removed, six checks kept after review. |
| #390 | `boot-restart.device.test.ts`: the dkrt boot proof's restart part in TypeScript (both devices, full tier passes); the Python proof stays until three gaps close (ledger `debug-card-safe.md`) |
| #391 | `dkui-host` remote host fake defines `dkrt_log_ev` (the full tier found the link error) |
| #392 | bootscreen, introblank and settings split by independent group (bodies identical to main by independent check; 18 halves pass) |
| #393 | Two dkrt proof gaps closed in TypeScript: voice row types in `boot-restart`, a DT2 `test/sine` note in dynreg `equiv` |

## Measurements

Details are in the plan's [Measurements](../plans/test-speed.md#measurements) section.

- Core gate of 2026-11-10 on the device-constants change: 126.8 min, cold snapshot store, about 3 cores busy elsewhere.
- Timeline from the Nx task-history database: exclusive tasks ran one after another until minute 50, and no device test started before. A 5 s exclusive CLI drained the run for about 9 min at minute 61. `python-tests` ran alone at the end. Device-test durations include emulator-slot waiting.
- A one-emulator-per-target trial made smoke and tier-quick 3 to 5 times slower. It was reverted: multi-emulator targets keep their job counts.
- History of core-touching merges (30 days, 128): 12% change no built bytes (step 4's scoped payoff), 33% are emulator-only, 38% change object bytes.
- Full tier on main after #382-#388 (2026-11-10, 17:08-18:41): 93 min, every device file as per-device halves plus the full-only tests and tier sweeps, with proof runs of other branches beside it. 3 failures: `dkui-host:rust-tests` did not link (no `dkrt_log_ev` in the remote host fake; fixed in #391); `core/runtime-inferred-needs-tests:test-device-full--dn2` hit the fixed 120 s runtime lifecycle deadline; `core/runtime-tests:test-device-all--fault-trace--dn2` lost its connection during the safe-mode restart after a DSP stop. Both device failures are DN2 halves under load: more concurrency (halves, no exclusive tasks) exposes the wall-time deadlines of plan step 6. Rerun alone on a quiet machine, both pass (inferred-needs 11/11, fault-trace 4/4): load timeouts, not code faults.
- Group splits proof: all 18 halves (9 files, both devices) pass; merged as #392.

## Open branches

- `test-speed/progress-deadline` (pushed, NOT for merge): the daemon reports a coupled cold boot's icount in `status.boot`, and the session polls it on a second connection around `firmware.load`. Device proofs with a short idle limit failed because card-rig loads (zero DSP and SHARC) showed no boot progress. The load takes a path that does not report it: zero DSP uses a ColdFire-only boot job on the actor, and the SHARC card-rig path is not traced yet. Next step: one probe that polls `status` every second during a cold load and records which counters move (`boot.icount`, `cf.icount`, job). Then wire the moving counter for each path.

## Next steps, in order

1. `progress-deadline`: run the probe and finish (plan step 6). This is now the first item: the 2026-11-10 full tier failed two DN2 device halves on wall-time limits under load (both pass alone), and the faster scheduling (halves, no exclusive tasks) raises the load further. Remaining wall-time polls from the 2026-11-10 timeout audit: `expectRuntimeUi` 120 s, card-rig polls 30 to 90 s, safe-mode, fault-trace, fits and conformance polls. About 30 per-test `timeout:` values of 600 to 1200 s duplicate the Vitest backstop and the watchdog.
2. Measure the next real core-change gate with policy B warm (expect about 15 to 20 min) and compare with 126.8.
3. Plan step 4 main part: device tasks keyed on image and `.dkm` bytes. First check: does the daemon binary rebuild byte-identically?
4. Plan step 10 ports, in this order (scope in the ledger and plan): the last dkrt proof gap (relocated `test/sine` DSP bytes with imports on both devices), then delete only the slow proof function, `Proof` and its CLI; the MEMORY page of a loaded machine and a `dkrun` case (then the menu runners go; `test_menu_dt2` took 528 s); `pages-dyn-pluck` (needs a frame-compare helper); `upload-rust` (run the Rust example from TS).
5. `shot-dn2` SMP stall. It is not the `listen()` race (it waits on the host-caused `INPUT_TAKEN`). On the next failure, read which error text it gives ("not seen within", "emulated clock is stuck", "SMP did not reach"). Rerun with `DIGI_EMU_FAST=0` to rule the AOT fast tier in or out.
6. Tooling fix 3: `devices/ports/dn2-1.11.toml` is hand-maintained since a90692c5 (regeneration churn dropped). Regenerate it and pin it in `test_pk_current.py` with a per-port skip, only together with a change that already pays a full gate (it rewrites `modules/core/fw/reference.json`).
7. DN2 runtime-resident DSP code at 0xbec4xx is always interpreted (plan item 12).

## Lanes

Used this session: `test-speed`, `ts-slots`, `ts-py` (under `/Users/em/src/digi/lanes/`). The older lanes the first handover listed (`ts-settle`, `ts-reload`, `main-fixes`, `pr-275`, `pr-276`) are already gone. Em removes worktrees; nothing was removed here. The open branches (`test-speed/group-splits`, `test-speed/progress-deadline`) are committed, so a lane can go once its branch is pushed or merged; keep one warm lane for the next session. The stash "observer: word count in debug (dropped, 2026-11-10)" is in the shared stash list.

Removal commands for Em:

```sh
git worktree remove /Users/em/src/digi/lanes/ts-slots
git worktree remove /Users/em/src/digi/lanes/test-speed   # after the handover PR is merged
git branch -D test-speed/device-constants test-speed/core-policy test-speed/py-tests-by-module \
  test-speed/py-noise test-speed/device-split fix/param-change-baseline-info \
  test-speed/no-exclusive test-speed/nx-tests-trim docs/device-constants-fanout \
  test-speed/observer
```
