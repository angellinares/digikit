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
- Gate projects in `tools/gate/` (smoke, card, stock, fx-dn2, fx-dt2, pins, dt2-tone, rust-modules)
  carry their triggers as such inputs. Nx affected ignores negated (`!`) inputs.
- Module projects depend on regression, digi-control (daemon), patchkit (packs), devices and sdk;
  modules with `test-conformance` also on conformance.
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

1. Done on branch `test/gate-waste`: waste review of the gate layout. Gate projects are split into
   fx-dn2, fx-dt2, pins and dt2-tone with traced inputs; smoke and card use the store daemon; module
   device targets use `production` cache keys.
2. Done on branch `test/gate-waste`: cheap targets are cached (typecheck, web check, desktop
   `build:ui` and `check:ui`, lint, host tests) and the hermetic rust-tests are cached.
3. Port the Python scenarios to TypeScript. Prove each family on both devices, then delete its Python
   runner code:
   - Done on branch `test/card-upload-ports`: card-torn in `card-safety.device.test.ts`.
   - Done on branch `test/card-upload-ports`: the upload full tier; matching Python upload scenarios deleted.
   - Done on branch `test/card-upload-ports`: upload-cut and clear-cut.
   - Done on branch `test/card-upload-ports`: cut-selection in `safe-mode.device.test.ts`, both devices.
   - Done on branch `test/runtime-pure-cores` (B1): the pure decisions of the runtime (sticky machine
     types, first-fit DSP placement, row counting, registry, selection and stage limits, module record
     choice) moved into `dkrt-sdk` (`sticky`, `capacity`, `sel`) with host unit tests; the runtime calls
     them. The `sticky-types` scenario and `run_sticky` are deleted (ledger: debug-card-safe.md).
   - Done on branch `test/full-tier-skips`: debug-log (periodic events), debug-guard, crash-capture, safe-fault
     and safe-late-fault pass on every device they list, with no skips; the Python scenarios, `run_debug`
     (except the memaudit part), `run_crash`, `run_safe*` and the `safe`/`crash` kinds are deleted.
   - debug parts still to do, revised plan:
     - B2 (done on branch `test/capacity-device`; ledger sections in debug-card-safe.md): one device boundary test per device. A full synthetic module set: every refusal code, no
       overlapping ranges, reference DSP bytes unchanged. Inferred-needs. One fits case on the real
       MODULES page. The TypeScript synthetic-module helper is written so it can become a fixture-module
       library.
     - B3: done on branch `test/memaudit-device`. memaudit clean and stray are
       `modules/core/runtime/tests/memaudit.device.test.ts`; `module:audit` runs
       `memaudit-module.device.test.ts` through `tools/devtools/module_audit.sh`. memaudit-full is done on branch
       `test/memaudit-full` (full-set builder moved to `synth.ts`); `memaudit-*-full`,
       `pk_digikit_memaudit.py`, `pk_capacity.py` and `test_capacity_sets.py` are deleted. `P.spec` of the
       Python tests is `tests/support/synth_spec.py`.
     - Dropped: menu-open and the isolation PCM comparison.
   - Future: capacity tests through a committed set of fixture modules uploaded into a running
     emulator (one per limit, arena-fill and expansion variants). This could replace B2's generator and
     much of B1.
4. Smaller items:
   - fx-reuse onset jitter: +-64 samples between identical runs, tolerance 128.
   - dkneeds lists broad inputs (`pyproject.toml`, `nx.json`, every `Cargo.toml`).
   - Breaking the Python tools import cycle (finding 104) would allow finer Python test selection.
   - nx_input_audit does not know what tools/nx/device-inputs.mjs and dependentTasksOutputFiles hash;
     teach it or port it to TypeScript.
   - rust-tests of the non-hermetic crates need hashes of the generated core and firmware before they
     can be cached.
   - Gate project Python inputs are hand-listed import closures; derive them from the import graph
     (tools/nx plugin) if they drift.
5. Overhaul AGENTS.md testing rules, check mise.toml for leftover Python gate tasks, and point
   README.md to docs/TESTING.md.

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
