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
| upload | 3 pass | 11 of 16 pass before fixes; upload-folders fixed (10 s stall allowance), not rerun; upload-cut and upload-clear-cut skipped (open item 1) |
| card-safety, card-small, debug-module, safe-mode, crash-capture, debug-guard | 14 pass | 24 pass, 13 fail; the failing groups are skipped (open item 6) |

## Open items

1. upload-cut and upload-clear-cut: after a power cut, the exported overlay of the first session holds a
   freshly formatted empty store header (66 records near sector 48 173 056 on DT2), while the guest's
   next boot lists the old file. Hypothesis: a `dsp: 'zero'` session restores a ColdFire snapshot taken
   with another card, so the runtime's RAM picture of the store does not match the card. First
   experiment: run the case with `dsp: 'sharc'` or `'cold'`. Python keeps these two scenarios until fixed.
2. The daemon's `card.overlay` exports its whole overlay, including the loaded `overlay.bin`; card-rig
   drops records that repeat the card's own overlay (`withoutOwn`), so a guest write of identical bytes
   is not seen. Fix in the daemon: export only this session's writes.
3. Not ported yet: kind `debug` parts capacity, fits, inferred-needs, isolation, memaudit, sticky-types,
   menu-open; kind `menu` (pluck module scenarios); module equiv TOMLs; upload-rust (Rust host parity).
4. `mise run test:runtime` builds plusdrive and fails offline when cargo needs downloads; use verify.
6. Debug-family full tier, skipped per device until fixed (Python still covers them): DT2 crash-capture
   early and late ("registers: expected [] to have a length of 16"); DT2 safe-mode, 4 cases ("expected
   [ 1, +0 ] to deeply equal [ +0, +0 ]" and similar boot-counter checks); DN2 cut-selection, 5 cases
   ("loaded 0 seq 2 copy 0" where loaded 2 was expected; also "card writes in the store range" in the
   close check); DN2 debug-guard; DT2 periodic debug events ("a full set (24 words, six events)":
   got 32). The failure logs are in `out/runs/verify/20261102-032104` of the integration lane.
5. Daemons from other sessions and lanes stay alive for days (`mise run clean:runs` lists orphans).

## Next steps

Done (PRs #254, #255, #257 and the tests-into-modules PR): Python settings, roundtrip, equiv-dn2-set and card
scenarios deleted; Python changes route by each test's `sources`; emulated-time waits and shared helpers;
device tests in their module projects with inferred Nx `test-device` and `test-conformance` targets; one
conformance suite over all 8 shipped modules; one Nx cache for all worktrees, guarded by `nx_input_audit`.

1. Run the runtime step's files in parallel (one daemon per file, about 4 at once). Replace the remaining
   wall-clock test timeouts with no-progress limits first. Measure with one paired timing.
2. The gate writes the Vitest log to the run directory as it runs (today it appears only at the end).
3. Split `tests/regression/runtime/live.ts` so a helper change reruns only its importers.
4. Build caching: per-module `pack` targets, then the daemon binary, each passing `nx_input_audit`;
   device targets depend on them. Then cache device results, starting with conformance.
5. Upload full tier once, then delete the matching Python upload scenarios. Fix open item 1, then delete
   upload-cut and upload-clear-cut. Port the remaining debug parts (item 3), then measure the Python tool
   graph again (finding 104).
6. Stage D: `nx affected` selects device tests; delete verify_route's hand tables.

## Process notes

Sub-agents edit code from concrete failure output; the coordinator runs the proofs and reports each
file's result. Never `git stash` in a lane (one stack for all worktrees). Node's
`copyFileSync(..., COPYFILE_FICLONE)` makes a dense copy on macOS: use `cloneFile` or hard links.
