# Handover 2026-11-02: Python device scenarios to TypeScript, test tooling

## Start here

Branch `test/scenario-migration` (integration lane `/Users/em/src/digi/lanes/nx-integration`). It adds
TypeScript ports of the Python device scenarios, the helpers they share, and test tooling. No Python
scenario is deleted yet. Plans: [triage and migration](../plans/python-test-triage.md),
[port map](../plans/scenario-port-map.md), ledgers in [docs/plans/ledgers/](../plans/ledgers/),
[Nx plan](../plans/nx-pilot.md), [audio QA draft](../plans/audio-qa.md).

## How to run device proofs

1. Daemons: `uv run python -m tools.devtools.verify_build daemons --env-out FILE` writes
   `DIGI_EMUD_DT2` and `DIGI_EMUD_DN2` (incremental in a warm lane, about 1 minute).
2. Ready inputs: `DIGI_PK_READY_DT2_117` and `DIGI_PK_READY_DN2_112` (the lane's `out/ready-*`).
3. Run one file: `mise run verify -- --tests runtime/settings.test.ts`. verify packs the modules the
   file's `runtimeAcceptance` record names, builds plusdrive and patchkit from the checkout and passes
   `DIGI_PLUSDRIVE` and `DIGI_PATCHKIT`.
4. Run the three families in parallel through Nx:
   `pnpm nx:pilot run-many -p regression -t test-device-upload test-device-settings test-device-debug --parallel=3`.
   Add `DIGIKIT_TIER=full` for the full tier.

Device runs need no exclusive benchlock: results use emulated time, sessions have their own run
directories and the shared caches are written atomically. Keep about three runs at once.
Do not write ad-hoc proof scripts with hard-coded binaries: a main-tree patchkit from another branch
broke every run once (`unknown module core/sample-pool`).

## Proof state (2026-11-02, this branch)

| Family | Quick tier | Full tier |
| --- | --- | --- |
| settings, roundtrip, equiv | 18 pass | 20 of 20 pass; equiv keeps the approved PCM pin |
| upload | 3 pass | 11 of 16 pass before fixes; upload-folders fixed (10 s stall allowance), not rerun; upload-cut and upload-clear-cut fixed on `test/card-upload-ports` (open item 1) |
| card-safety, card-small, debug-module, safe-mode, crash-capture, debug-guard | 14 pass | 24 pass, 13 fail; the failing groups are skipped (open item 6) |

## Open items

1. Resolved on branch `test/card-upload-ports`: the daemon cached the ColdFire-only patched boot state
   per firmware, not per card (`card_snapshot_key` in packages/emulator/control/src/controller.rs), so
   every later zero-DSP boot in one daemon restored the first card's RAM picture of the store and wrote
   it back over the new card. upload-cut and upload-clear-cut now run on both devices.
2. The daemon's `card.overlay` exports its whole overlay, including the loaded `overlay.bin`; card-rig
   drops records that repeat the card's own overlay (`withoutOwn`), so a guest write of identical bytes
   is not seen. Fix in the daemon: export only this session's writes.
3. Not ported yet: kind `debug` parts capacity, fits, inferred-needs, isolation, memaudit, sticky-types,
   menu-open; kind `menu` (pluck module scenarios); module equiv TOMLs; upload-rust (Rust host parity).
4. `mise run test:runtime` builds plusdrive and fails offline when cargo needs downloads; use verify.
6. DN2 cut-selection is resolved on branch `test/card-upload-ports`: the test read boot info while the
   DSP part of the modules was still LOADING. The helper `bootInfoLoaded` (card-kit.ts) now waits for it.
   The four debug-family skips are resolved on branch `test/full-tier-skips`:
   - DT2 crash-capture ("registers: expected [] to have a length of 16"): the runtime logs FAULT_FOUND,
     SETTINGS_WRITE and BOOT_RECORD between CRASH_MOD and the replayed detail (registers, stack, tail), and
     `crashRecord` and `faultDetail` stopped at the first other event. They now skip events that are not parts
     of the record. The test also allows the store writes of the fault record (`dk/fault`).
   - DT2 safe-mode (4 cases): the card is soft-reset before the DT2 filesystem check ends, so the coupled
     ready state never comes; the group needs `relaxFs` (Python waits for the main UI only).
   - DN2 debug-guard ("[ 1, 0 ] to equal [ 0, 0 ]"): the test read boot info while the DSP part of a module was
     LOADING; it pauses and uses `bootInfoLoaded`.
   - DT2 periodic events: `watch::DIAG_WORDS` is 32 since the log sink change, so a set is eight events, not six.
5. Daemons from other sessions and lanes stay alive for days (`mise run clean:runs` lists orphans).

## Next steps

Done (PRs #254, #255, #257 and the tests-into-modules PR): Python settings, roundtrip, equiv-dn2-set and card
scenarios deleted; Python changes route by each test's `sources`; emulated-time waits and shared helpers;
device tests in their module projects with inferred Nx `test-device` and `test-conformance` targets; one
conformance suite over all 8 shipped modules; one Nx cache for all worktrees, guarded by `nx_input_audit`.

1. Done: runtime and conformance files run in parallel (`REGRESSION_JOBS`, `DIGI_RUNTIME_JOBS`).
   Open: the no-progress watchdog (`DIGI_NO_PROGRESS_MS`) is off by default. Its `status` polls wake the daemon's
   controller and move where a free-running coupled emulator stops (fx-reuse DN2: onset shift 192 > 128 with it,
   pass without) [D]. Fix in the daemon: status requests must not change emulation, or a passive heartbeat file.
2. Done: every gate step streams its log to `out/runs/verify/<id>/<step>.log`; the START line prints the path.
3. Done: `live.ts` is split into devices, ready, waits, install, artifacts and protocol helpers (tests/regression/runtime).
4. Done: daemons come from `<git common dir>/emud-cache/<dev>/<key>` (tools/devtools/emud_key.py, audited
   against cargo's `digi-emud.d`); a fresh lane restores both in under a second. The Nx cache is per worktree
   (Nx 22's database cache does not restore entries of another worktree). Next: device-result caching with a
   self-hosted Nx remote cache server (`NX_SELF_HOSTED_REMOTE_CACHE_SERVER`, OpenAPI spec), starting with conformance.
5. Port the remaining debug parts (item 3), then measure the Python tool graph again (finding 104).
6. Stage D: `nx affected` selects device tests; delete verify_route's hand tables.

## Process notes

Sub-agents edit code from concrete failure output; the coordinator runs the proofs and reports each
file's result. Never `git stash` in a lane (one stack for all worktrees). Node's
`copyFileSync(..., COPYFILE_FICLONE)` makes a dense copy on macOS: use `cloneFile` or hard links.
