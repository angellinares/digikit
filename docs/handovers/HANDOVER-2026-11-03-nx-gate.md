# Handover 2026-11-03: Nx gate, device caching, watchdog

## State

Merged 2026-11-02/03:

- #263: pathcheck and pinned tests deleted; pnpm-wrapper mise tasks deleted.
- #264: Nx pack and CLI targets; device targets run vitest directly.
- #265: session run dirs reuse the booted-snapshot cache.
- #266, #267: every gate step is an Nx target. Owner projects are tools, devices, sdk and tests.
  `fx_tier.py` and `py_select.py` moved out of the old gate.
- #268: Nx gate selection fixed against a 15-commit comparison.
- #269: `mise run gate` is the merge gate. The Python gate, `verify.py` and its routing are deleted.
- #270: module `test-device` and `test-conformance` are cached in a shared Nx remote cache.

This PR (branch `test/watchdog-default`):

- The no-progress watchdog is on by default (`DIGI_NO_PROGRESS_MS`, 120 s).
- Runtime and conformance wall timeouts are 30 min backstops.
- `gate-fx` is split into `gate-fx` (vitest FX suites) and `gate-tier` (pins, tier, dt2-tone).

## How the gate works

- `mise run gate`: `nx affected` against `DIGI_GATE_BASE` (default `main`), `--parallel 2`, nice 10,
  shared benchlock, remote cache read and write.
- `gate:full`: run-many plus the tier sweeps.
- `test:changed`: cheap targets only, cache read-only.
- Selection is per Nx project. A project is touched when it owns a changed file, depends on a touched
  project, or has a target input `{workspaceRoot}/<glob>` that matches a changed file.
- Gate projects in `tools/gate/` (smoke, fx, tier, card, stock, rust-modules) carry their triggers as
  such inputs.
- Module projects depend on regression, conformance, digi-control (daemon), patchkit (packs),
  devices and sdk.
- Editing `nx.json` touches every project. Editing a `project.json` touches that project and its
  dependents (regression or a harness package: every module's device tests). Prefer plugin code
  (`tools/nx/*.mjs`) and new files for config changes.
- Device targets run 2 at a time with `REGRESSION_JOBS=2` (about 8 emulator cores on 16). Each vitest
  worker claims `dist/session-cache/<device>-<n>` with a pid lock.
- Daemons: `digi-control:daemons` writes `dist/daemons.json`. Helpers read it when `DIGI_EMUD_*` is
  unset.
- Remote cache: Cacheiro on 127.0.0.1:47311 (`tools/nx/cache-server`, own lockfile). State is in
  `<git common dir>/nx-remote-cache`. Control with `tools/nx/remote-cache.mjs` (start, status, stop).
- Cached targets: module `test-device` and `test-conformance`. Inputs are `DEVICE_INPUTS` in
  `tools/nx/modules.mjs` plus `tools/nx/device-inputs.mjs` (firmware, ready dirs, DT2 card).
- Python tests: `tools:python-tests` runs `py_select` (gate test, PY_SUITES, import graph, changed
  tests) on any change.

## Next steps

1. Waste review of the gate layout. For each gate project and target, list the triggers it needs.
   Split or narrow where needed (as with gate-fx and gate-tier). One config PR.
2. Cache the cheap targets: typecheck, `@digi/web` check, desktop `build:ui` and `check:ui`,
   rust-tests (inputs from cargo depfiles). Bundle with step 1, so `nx.json` and `project.json`
   change once.
3. Port the Python scenarios to TypeScript. Prove each family on both devices, then delete its Python
   runner code:
   - card-torn to `card-safety.device.test.ts`.
   - debug parts: capacity, fits, inferred-needs, isolation, memaudit, sticky-types, menu-open.
   - the upload full tier once, then delete the matching Python upload scenarios.
   - upload-cut and clear-cut.
4. Smaller items:
   - fx-reuse onset jitter: +-64 samples between identical runs, tolerance 128.
   - dkneeds lists broad inputs (`pyproject.toml`, `nx.json`, every `Cargo.toml`).
   - Breaking the Python tools import cycle (finding 104) would allow finer Python test selection.

## Process notes

- Prove a merge with a table of change to check that proves it. Rerun only what the last change can
  reach. Do not restart a full run after a config change that changes no test, but run every edited
  command once (a typo in `--parallel 2-t` broke `mise run gate` after #269).
- Coder sub-agents must Read before Edit. They cannot delete files or run commands; use a
  general-purpose agent for those.
- The main checkout's `agent_guard` hook blocks commands that contain the old full-gate task name.
  Put commit messages and PR bodies in files.
- Nx allows `{workspaceRoot}` only at the start of an option string.
- `mise.toml` is in the daemon cache key (`emud_key`). A task edit forces one daemon rebuild
  (about 9 min), shared by all worktrees.
