# Gate speed (plan, 2026-10-30)

Goal: `mise run verify` runs only what a change can affect, and a rerun repeats only what failed.
The router is `tools/devtools/verify.py` and `verify_route.py`; the budget is `verify_budget.py`.
Commands and tiers: [testing](../TESTING.md).

## Done

- [x] Scheduling: steps and one job per python file share one CPU budget, longest first.
- [x] Untracked root files do not select steps.
- [x] Fresh mtimes in patchkit staging (no stale-object rebuilds).
- [x] Patchkit groups (`patchkit_fine`): a patchkit file selects its own tests and smoke only when it can change an image.
- [x] Runner kinds (`runner_fine`): a scenario runner selects the digikit tests of the kinds it serves.
- [x] `Cargo.lock` routing: only the packages of the changed members run.
- [x] Metadata-only module objects are dropped from the changed set.
- [x] Reruns: `result.json` has `failed_units`; `--rerun-failed` repeats failed units plus what changed since.
- [x] `--py-tests` and `--tests` run named files.
- [x] Daemon check: a plan that needs daemons without building them stops early.
- [x] DN2 send-FX core and the profiling switch.
- [x] Exactness pins for DN2 1.12.
- [x] Test-only changes (tests/devtools, patchkit and digikit Python tests, device acceptance tests, tools/devtools) select no smoke.
- [x] A rerun after a failed check (`python-tests:<check>`) repeats that check and the failed files only.

## Open

- [ ] Smoke per-cell results: pk_harness writes a result per cell, so a rerun repeats the failed cells only.
- [ ] Skip audio scenarios whose inputs did not change: hash the image, the module objects, the daemon and the test code per scenario.
- [ ] Move ColdFire-only checks to the CF-only rig: a block-counter tick in `--dsp zero`; split fxroute, fxroutes, fxpage and fxpersist; settings.
- [ ] Runtime-resident DSP code at 0xbec4xx is still interpreted on DN2: needs a JIT or a runtime-aware gen.

Not here: the own SHARC assembler is in [firmware-decls-and-sdk.md](firmware-decls-and-sdk.md).
