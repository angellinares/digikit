# Tests

Reference for the test levels, commands, inputs and gate. The gate is `mise run verify`. Other docs
link here. Results of runs belong in `docs/findings/`, not here. Planned changes:
[docs/plans/test-architecture.md](plans/test-architecture.md); gate speed:
[docs/plans/gate-speed.md](plans/gate-speed.md).

## Test levels

A level says what a test proves. Firmware inputs, device version, cost and exclusive resources are
execution requirements, not levels. A successful boot proves only what the run asserts.

| Level | What | Command |
| --- | --- | --- |
| Static (JS/TS) | oxlint, Prettier check, host Vitest, `pnpm -r run typecheck` | `pnpm verify:host` |
| Static (Python) | ruff check and format over pyproject `[tool.ruff] include`; mypy over `[tool.mypy] files` | `mise run lint:py`; `mise run check:layers` (module layering, docs/ARCHITECTURE.md) |
| Fast preflight | Changed files only; no builds, no devices | `mise run test:changed` |
| Unit (TS host) | Packages, desktop UI logic, regression helpers; no device | `pnpm test:host` |
| Unit (web) | Web app Node suites in `apps/web/tests` | `pnpm --filter @digi/web test` |
| Unit (Python) | RE, firmware and dev tools | pytest on named files under `tests/<owner>/`; `mise run test:pypy` for the PyPy subset |
| Unit (Rust) | Crates | `cargo nextest` through the gate step `rust-tests` or `mise run test` |
| Component | Module DSP conformance on the SHARC core | `mise run module:check ID` |
| Device: stock UI | DT2 stock pages and transport goldens (zero DSP) | `mise run test:stock` |
| Device: FX | Stock FX oracles | `mise run test:fx`, `mise run test:fx:dt2` |
| Device: runtime | Emulator oracles (`tests/regression/emulator`) and module-owned device tests (`modules/**/tests/*.device.test.ts`) | `pnpm nx:pilot run <module-id>:test-device` |
| Device: module loads | One shared conformance suite (`tests/conformance`) loads every shipped upload | `pnpm nx:pilot run-many -t test-conformance` |
| Device: Python | Digikit boot scenarios, patchkit smoke matrix, patched-boot pytest files | owner pytest files; `mise run patch:test` |
| App E2E | None yet | none |

A mocked UI test, a preview or a daemon scenario is not app E2E coverage.

## Which check for which change

Use the owner command while iterating. The coordinator runs the routed gate once per merge
(`mise run verify -- --base main`). `mise run verify -- --explain` prints the routing without running it.

| Change | While iterating |
| --- | --- |
| JS/TS host tests and helpers | `pnpm test:host`, `pnpm verify:host` |
| Web Node tests | `pnpm --filter @digi/web test` |
| Desktop UI or app | `pnpm verify:host`; `ui:preview` and `ui_shots.mjs` for pages (below) |
| Stock DT2 UI goldens | `test:stock` |
| DN2 or DT2 FX sources and oracles | `test:fx`, `test:fx:dt2` |
| Python under `tools/` or `tests/` | `test:changed`; then the named pytest files |
| Patchkit, ports | `test:changed`; `patch:test --cell ...` for one smoke cell |
| Leaf module | `module:check ID` |
| Device tests of one module | `pnpm nx:pilot run <module-id>:test-device` |
| Loading one module | `uv run python -m tools.devtools.verify --conformance ID[,ID...]` |
| Framework module, runtime | `pnpm nx:pilot run <module-id>:test-device`, `pnpm nx:pilot run-many -t test-conformance` |
| SHARC core or transpiler | regenerate the cores, then `test:changed` |
| Emulator crates | `test:changed`; the crates' Rust tests |
| PCM pins (`tests/scenarios/*.json`) | only with Em's approval |

## Commands

Device tasks never build a daemon or the emulator themselves. Prerequisites are in "Inputs".

`test:changed` sends its complete changed-path list to `pnpm nx:pilot affected --target=test-host`
on stdin, with one Nx task at a time. Nx selects whole registered host projects, including dependency
consumers, shared configuration and explicit filesystem inputs. Deleted paths retain their project
scope; newline-containing paths are rejected. Empty changes select no host work. `--explain` prints
the Nx command and its stdin paths without running it. The former full-host versus `vitest related`
branch is removed; lint, formatting, types and the full merge gate retain their existing selection.

The evaluated Rust discovery plugin is unregistered for host preflight, so host selection does not
require Rust toolchains or Cargo metadata. Explicit Rust targets retain their own pinned commands.
See the [Nx migration boundary](plans/nx-pilot.md#host-preflight-migration).

Control-client, test-support, dk-session, desktop-host and regression-host targets use local result
caching; web-transfer and the per-module `test-host` targets remain uncached. tools/nx/modules.mjs infers, for each shipped module, a `test-host` target (host tests under `tests/`), a `test-device` target (its `tests/**/*.device.test.ts`, run with Vitest directly after its packs) and, for each shipped dynamic, service or `host` module, a `test-conformance` target (the shared conformance suite for that module). Device targets set `parallelism: false`. Other Nx targets: `pack` (patchkit build, then pack for each device into `dist/modules/<device>/<leaf>.dkm`; only for the modules the gate packs, not for patched-in core modules or static fixtures), `pack-aliases` (renamed copies named by `runtimeAcceptance` aliases, same directory), `patchkit:cli` (`dist/patchkit/patchkit`) and `plusdrive-format:cli` (`dist/plusdrive/plusdrive`). Outputs go to `dist/`, never under `out/`. Shipped content modules (not `core/*`) depend on `core/runtime`; machines also on `core/dynreg` and `core/dsp-voice`, effects on `core/dynfx` and `core/fxpages`. Web tests retain ignored
generated WASM and optional inputs outside the cache boundary. Cache use assumes a frozen installation. New test helpers, environment
reads or external inputs require an input audit before relying on cached results. The
[bounded cache observations](findings/nx-pilot.md) include dependency failure/restoration probes;
only the support/session pair was timed, not whole preflight. Rust and runtime targets remain uncached.

| Command | What runs |
| --- | --- |
| `mise run test:changed` | `tools/devtools/changed_tests.py`: ruff and mypy on changed Python, seven audited tooling unit files, Nx-affected host Vitest projects, changed JS/TS lint and format, host types. Options: `--base`, `--head`, `--files`, `--deep`, `--explain`. Does not run Rust, module builds or devices. |
| `pnpm test:host` | All seven Vitest projects from `vitest.host.projects.json`: control-client, test-support, dk-session, web-transfer, desktop-host, regression-host, module-tests. One worker by default; `-- --maxWorkers N` changes it. |
| `pnpm verify:host` | oxlint, Prettier check, host Vitest, typecheck of control-client, dk-session, test-support, regression and desktop. Stops at the first failure. |
| `pnpm --filter @digi/web test` | `node --test` in `apps/web/tests`. Cases that need WASM or firmware skip when absent. |
| `mise run lint:py` | `ruff check` and `ruff format --check` over the pyproject include list. |
| `mise run lint` | `lint:py`, `lint:rust`, `pnpm lint`, `pnpm fmt:check`. |
| `mise run test:pypy` | The SHARC core tests under PyPy 3.11. Slow. |
| `mise run test:stock` | Vitest project `stock` (`tests/regression/dt2`): DT2 1.17 zero-DSP goldens. Filters pass through. |
| `mise run test:fx` | Project `fx-dn2`: 61 quick assertions; `DIGI_FX_SWEEP=1` runs 191 across 134 scenarios. `DIGI_JOBS` bounds rendering. |
| `mise run test:fx:dt2` | Project `fx-dt2`: 135 quick assertions; `DIGI_FX_SWEEP=1` runs 146 across 60 scenarios. `DIGI_DT2_CARD` selects a prepared card image. |
| `pnpm nx:pilot run <module-id>:test-device` | The module's `*.device.test.ts` files (Vitest project `runtime`) after its packs, aliases and the plusdrive CLI are built into `dist/`. |
| `pnpm nx:pilot run-many -t test-conformance` | Project `conformance`: `tests/conformance/load.device.test.ts` on DT2 1.17 and DN2 1.12, once per eligible module (`DIGI_CONFORMANCE_MODULES` is set per module). |
| `mise run module:check ID` | Machine/effect conformance and own Python tests, then any module-owned Nx `test-device` target. May build or emulate; size/fit follows both. Not an all-module load proof. |
| `mise run module:audit NAME` | ColdFire memory audit of one run-time module. |
| `mise run patch:test` | Smoke tour over `tests/patchkit/smoke/matrix.toml` (`-- --cell dt2-1.17:core/fx`, `-- --update`). |
| `mise run patch:proofs` | Every emulator proof on its quick tier. |
| `mise run dev:plusdrive-check` | The firmware's own view of an image built from a folder (about 1 min). |
| `mise run scenario NAME` | Replays one headless scenario. |
| `mise run scenario:all` | Every DN2 scenario; fails when a pinned PCM hash differs. |
| `mise run test` | `test_outcomes run --check` over `tests` (one process per file), then `rust_ws test`, `pnpm test:host`, `pnpm test`, the DN2 pins. Excludes slow gates and runtime acceptance. |
| `mise run test:slow` | The same runner with `--slow` and a one-hour per-file timeout, then `rust_ws gates`. |
| `mise run test:quick` | Launcher, PGO, target and coupled-tool tests, then `coupled test-quick`. |
| `mise run verify` | The merge gate, routed by changed files. Options: `--base BRANCH`, `--head REV`, `--only STEPS`, `--all-files`, `--explain`, `--rerun-failed`, `--jobs N`, `--foreground`, `--all-no-aot`, `--no-cache`. |
| `mise run verify:full` | `verify --full`: runtime acceptance, 191 DN2 FX assertions, `tier-sweep` instead of `tier-quick`. `-- --dt2` adds the DT2 full sweep. Only after generated-core changes. |
| `mise run clean:runs` | Lists old `digi-*` run directories under the temp dir (older than 24 h, not used by a running `digi-emud` or node process) and orphan `digi-emud` processes; `-- --yes` deletes the directories, `-- --hours N` sets the age. It never kills a process. A Session refuses to start with less than 50 GB free. |

`uv run python -m tools.devtools.test_outcomes BEFORE.xml AFTER.xml --renames-from REF` compares
outcomes and skip reasons test by test after moving tests.

## Fast iteration: dk:dev and Reload

For module code and the UI layer (`core/ui`, `core/digikit-menu`, `test/ui-demo`) one edit and one look should take seconds, not a
boot. `mise run dk:dev -- test/ui-demo --device dn2[,dt2] --keys 'tap:NO,tap:SETTINGS,wait:400,tap:DOWN*8,tap:YES,wait:600,shot:menu'`
(`tools/patchkit/pk_devreload.py`) builds the objects whose sources changed (`patchkit build`), reloads a daemon it keeps alive per device and
module set (`--stop` ends it), runs the key script from the main screen, and writes `out/runs/devreload/<device>/NAME.png` (4x) and
`sheet.png` (2x, four per row, labelled). The target is a patch set of `devices/*.toml` or module ids; `--keys` is a JSON list of harness
steps or inline text (`tap:KEY`, `wait:MS`, `turn:ENC A:BY`, `shot:NAME`, `*N` repeats). The workbench has the same call as the Reload
modules button (⇧⌘R), with `build: true`: it builds the stale objects of the session's modules first (ColdFire and SHARC+; docs/CONTROL.md). `dk:dev` builds in its own step
and reloads without `build`.
`--store ID,ID*,synth:NAME:BSS` boots the daemon on a card whose module store holds those modules (`*`: also in the saved selection; `synth:` needs BSS bytes of arena, so it never fits), which the MODULES list of `core/digikit-menu` needs; each list has its own kept daemon.

The daemon method is `dev.reload` (docs/CONTROL.md). It applies the modules again and compares the result with the running image:

| Path | When | What runs | Measured (PGO daemon, warm) |
| --- | --- | --- | --- |
| `swap` | only UI layer modules differ | restore the cached main-screen state, write the module bytes | DN2 1.4 s, DT2 1.7 s (apply 1.2 s, restore 0.3 and 0.4 s, write under 1 ms) |
| `boot` | the runtime, any other module, or a non-MAIN section differs | load the new image | cold boot DN2 5.3 s, DT2 10.2 s; image booted before: 0.3 s and 0.5 s |
| `none` | nothing differs | nothing | apply only, 1.2 s |

A `dk:dev` round for a UI change is about 3.6 s per device, with a two-key-step script of 1.8 s included. Build costs: Rust runtime 2.3 s,
C object about 0.3 to 1 s. The first run of a module set starts the daemon and boots the patched image (DN2 9 s, DT2 10 s with warm caches).
The harness stays the final proof: a swap shows what a change looks like and is not a substitute for `module:check`, the runtime suites or a
boot of the real image. `tools/emulator/boot_phases.py` measures where a boot of the workbench's patched image spends its time (cold and
warm `firmware.load`, `state.load`, first run of our runtime).

## Inputs

Tests that lack an input skip with a reason; `pytest -rs` lists them.

| Input | How to provide |
| --- | --- |
| Stock images in `firmware/` (DT2 1.17, DN2 1.12) | README "Firmware"; `DIGI_EMU_SYX`, `DIGI_EMU_SYX_DT2` override paths. |
| Sections `out/sections/<image>/` | `uv run python -m tools.firmware.extract firmware/SYX -o out/sections/<image>`. |
| SHARC database `out/sharcdb/<image>.sqlite` | `uv run python -m tools.sharc_dev.sharcdb build out/sections/*/section_7_BLOB.bin`. |
| ColdFire dump `out/ghidra/<image>-emac` and cfdb | Needs Ghidra: `tools.coldfire.ghidradump`, then `tools.coldfire.cfdb build`. |
| Native SHARC library, shared core | `mise run gen:native-lib` (firmware-free); `SHARC_NATIVE_LIB` overrides. `mise run gen:core` builds the generated core. |
| Module toolchains in `out/tools` | `mise run patch:toolchains` (about 15 min); `uv run python -m tools.patchkit.pk_toolchains status`. |
| Objects of content modules and fixtures in `out/cache/dko` | Not committed. The Rust tests that scan the repository (`Library::scan_repo`), `module:pack`, `module:pack-all`, `dk:run`, `dk:dev` and the web gallery build the stale ones on demand (`patchkit build` does it by hand). Without the m68k-elf toolchain the Rust tests that need them print `SKIP` and return. A checkout with an empty `out/` passes after the first build. |
| Ready inputs | `mise run emu:ready -- dt2` or `-- dn2` (about 2 min). |
| Captures `out/captures/*.dt2cap` | Not reproducible from the repository; those tests stay skipped. |
| Opt-in switches | `LIVE_AUDIO_DEVICE_TESTS=1` opens a real audio device; `--slow` runs long firmware runs. |

Device suites (`test:stock`, `test:fx*`, the Nx `test-device` and `test-conformance` targets) also need:

- Pinned daemons: `DIGI_EMUD_DT2` and `DIGI_EMUD_DN2` point to built `digi-emud` binaries
  (`mise run emu:serve -- --build-only`). `test:stock` takes `DIGI_EMUD`. The gate sets these from
  `build-daemons`.
- Complete ready inputs for each device, including the validated DSP files.
- Module artifacts for the device targets: `dist/modules/<device>/<leaf>.dkm`, built by the Nx `pack` and `pack-aliases` targets.
  `DIGI_RUNTIME_MODULES` (optional) overrides the directory. The gate packs the fixtures of the selected records and the modules named in `DIGI_CONFORMANCE_MODULES` with one `nx run-many`.
- `DIGI_PATCHKIT` (optional) names the patchkit binary. The Nx targets and the gate use `dist/patchkit/patchkit`; without it the tests build patchkit once through `patchkit.py bin`.
- DT2 card samples at `out/runs/patchkit/samples`.
- plusdrive: `test-device` builds it through `plusdrive-format:cli` (`dist/plusdrive/plusdrive`, also `DIGI_PLUSDRIVE`). The gate builds it before samples-live.

FX route and track SEND acceptance runs in the device tests of the owning modules (`modules/**/tests/*.device.test.ts`)
with `pnpm nx:pilot run <module-id>:test-device`. `tests/regression/runtime` keeps the shared helpers only. The legacy Python `fxroute` and `fxroutes` scenarios are retired;
their offline data and comparison helpers remain.

Details of the runtime and module fixtures: [tests/regression/README.md](../tests/regression/README.md).
FX coverage: [tests/regression/scenarios/README.md](../tests/regression/scenarios/README.md).
All device suites share `tests/regression/vitest.config.ts` with the projects `stock`,
`runtime`, `conformance`, `fx-dn2` and `fx-dt2`. A device project runs one file at a time. Moved tests
import helpers as `@digi/regression/<path under tests/regression>` and packages as `@digi/test-support`,
`@digi/dk-session` and `@digi/control-client`; the config and `tsconfig.json` resolve them for files
under `modules/` and `tests/conformance/`.

## Layout

| Directory | What it tests |
| --- | --- |
| `tests/sharc/{semantics,trace,generation,analysis,integration}` | SHARC+ semantics, tracer, assembler and generator, loader and sharcdb, firmware-backed harness and golden hashes |
| `tests/patchkit` | Patchkit, module primitives, DSP voice rig, PLUCK, proofs. `smoke/` holds the smoke matrix (`matrix.toml`, `tours/`, `goldens.json`). |
| `tests/modules` | Machine and FX conformance for every module |
| `modules/**/tests` | A module's own tests, `*.device.test.ts` included (below, "Module-owned tests") |
| `tests/coldfire` | ColdFire analysis tools |
| `tests/emulator` | Launcher, PGO, coupled runner helpers, live audio, bootstrap |
| `tests/control` | The control-socket client and daemon |
| `tests/firmware` | `.syx` container, device profiles, snapshots, +Drive format |
| `tests/dsp` | DSP maps, FX scenario generators, audio checks |
| `tests/devtools` | Benchlock, verify routing and budget, render-cache audit, test outcomes |
| `tests/digikit` | Digikit boot scenarios (below); data in `scenarios/*.toml` |
| `tests/regression` | Native Vitest: `dt2/` (stock), `emulator/` (emulator oracles), `runtime/` (helpers only), `scenarios/` (FX), `host/` (no device) |
| `tests/conformance` | The shared module-load suite (`load.device.test.ts`, `load.ts`) |
| `tests/scenarios` | Stock scenario inputs and PCM pins |
| `tests/support` | Shared helpers, not collected; `paths.py` gives `REPO_ROOT`, `TESTS`, `TOOLS` |
| `tests/golden`, `tests/fixtures` | Reference hashes and small fixtures |

Other owners: `apps/web/tests` (web Node tests), `packages/control-client/tests`,
`packages/dk-session/tests`, `packages/test-support/tests`, `apps/desktop/ui/tests` (host Vitest), and
Rust tests in their crates.

## Writing a test

- Put the test in the directory of the code it tests.
- Import repo paths from `support.paths`. Never count parent directories.
- Import tools as `from tools.<owner> import <module>`. pytest puts the repository root and `tests/`
  on `sys.path` (`pythonpath` in `pyproject.toml`). Never change `sys.path`. A subprocess runs
  `[sys.executable, "-m", "tools.<owner>.<module>", ...]` with `env=python_env()` (`from tools import python_env`).
- Share helpers through `tests/support`. Never import another test module.
- Test file basenames are unique across `tests/`. There is no `__init__.py`.
- Mark long firmware runs `@pytest.mark.slow`. They are skipped unless `--slow` is given.
- A test that needs firmware or generated artifacts skips with a reason when they are absent.
- Lint and types come from `pyproject.toml`: new Python files are covered by `[tool.ruff] include`;
  add new typed files to `[tool.mypy] files`. Both run as commands in the gate step `python-tests`.

## Module-owned tests

A module keeps its tests in its own folder, `modules/<kind>/<name>/tests/`. No list names them; globbing finds them.

- `test_*.py`, also in subfolders: pytest files. The basename is unique across the repository (prefix it with the module name). A helper
  next to the test is imported by name. Use `Path(__file__).parents[1]` for the module folder.
- `*.test.ts`: Vitest project `module-tests` of `vitest.host.projects.json` (`pnpm test:host`), found recursively under `modules/`.
- `*.device.test.ts`: device tests, registered in the module's `project.json` (`metadata.runtimeAcceptance`, files relative to the module
  root) and run by the `runtime` project. `pnpm nx:pilot run <module-id>:test-device` runs one module's files; `verify --conformance ID`
  loads the module through the shared conformance suite.
- `scenarios/*.toml`: digikit scenarios. `pk_digikit_scen list|run NAME` and the tiers find them next to `tests/digikit/scenarios/`.
- `module:check ID`, `verify` and `test:changed` select them. A change in the module selects its tests and those of the modules that depend
  on it; a change only in `tests/` runs python-tests (and `host` for `.ts`), no Rust or web step.
- A scenario that pins the PCM of a module lives with that module. A shared test needs "a module" only: it uses a fixture of
  `tests/fixtures/modules` (`test/sine`, a dynamic machine on both devices; `test/dyn-fx-gain`, an effect; the table is in
  docs/MODULE-AUTHORING.md), or a made-up id such as `acme/demo` when nothing is built. A list of all shipped modules is a scan of
  `modules/{machines,fx,tools}/*/module.toml`, never a literal list.
- `mise run check:layers` (rule `self-contained`): no tracked file outside a module's folder names its id or path. Docs, `*.md`, `.agents/`
  and the fixtures may. `host = true` in `[module]` exempts a module the workbench speaks to (the remote control service).

## The gate

`mise run verify` runs the steps the changed files need. Routing is in `tools/devtools/verify.py`
(path rules, `PY_SUITES`) and `tools/devtools/verify_route.py` (finer rules).

`--only STEPS` restricts that diff-derived plan to the requested steps, keeping its Python files,
acceptance files, smoke cells and Rust packages. For example,
`mise run verify -- --only runtime-fixture --base main --explain` shows only the relevant runtime
acceptance files; an unrelated or empty diff selects nothing. This changes the former behavior
where `--only` forced requested steps and broad acceptance or Rust suites.
To force complete requested suites, use `mise run verify -- --only runtime-fixture --all-files`.
`--all-files` uses the full Python selection, all acceptance files, the whole smoke matrix and the
whole Rust workspace for whichever steps are requested. It does not change the tier or add steps;
normal build prerequisites still apply. It requires `--only` and cannot be combined with named files,
`--full` or `--rerun-failed`. `--only` and `--full` are also incompatible.

Named `--tests` and `--py-tests` files override selection for their respective steps and imply those
steps. With `--only`, their steps must be requested; other requested steps keep their diff-derived
scope. For example, `mise run verify -- --only runtime-fixture,stock --tests modules/core/runtime/tests/service-commands.device.test.ts`
runs that runtime file plus any stock files routed by the diff. `--head REV --explain` uses the same
selection for a historical range, including with `--only`.

Steps: build, desktop, host, web, stock, runtime-fixture, module-load, pins, check, check-no-aot,
tier-quick, tier-sweep, python-tests, rust-tests, card, smoke, dt2-tone, dt2-check, dt2-tier-quick,
dt2-tier-sweep.

- `host` runs `pnpm verify:host`. `web` depends on `host`.
- `python-tests` runs ruff, ruff format, mypy, layercheck and `patchkit gen --check` (generated sources) as commands, then
  `tests/patchkit/test_pk_current.py` (module and port staleness), then the selected test files, one
  process per file. Selected files are the tests that reach a changed module in the import graph
  (`changed_tests.gate_tests`), the files changed, and the `PY_SUITES` the change triggers.
  The exact launcher modules `tools.emulator.coupled`, `tools.emulator.digi_env`,
  `tools.emulator.native_emu` and `tools.emulator.native_pgo` retain every direct/test-helper
  importer and every deep consumer except legacy `tests/digikit` scenarios downstream of
  `tests.digikit.scen_cases`. Their focused `launcher` suite and other consumer tests, plus
  the existing full cross-device smoke matrix, replace that broad legacy feature selection.
  This is a bounded routing contract, not symbol-aware analysis or proof of no behavioral impact.
  Filtering applies per changed source: changing a scenario dispatcher, runtime or harness helper
  alongside a launcher restores its scenario consumers. Changed tests and full-mode declared
  suites still run; neighboring modules keep conservative graph selection. Each launcher alone
  still selects `build`, `pins`, `python-tests` and the full `smoke` matrix.
  Changes to `verify.py` and `verify_route.py` keep devtools owner tests, import-graph consumers,
  `PY_GATE` and static checks, without selecting unrelated production steps or the Rust workspace.
  Explicit `--full` retains full coverage. An unclassified `tools/device` module keeps its own tests,
  transitive importers and scenario kinds mapped to those importers. This does not exempt deleted modules from conservative import-graph handling.
  Framework modules (`FRAMEWORK` in `verify_route.py`) select all of `tests/patchkit` and `tests/digikit`.
- `stock` runs the DT2 zero-DSP goldens. A changed case selects its file; configuration and shared
  session code select all files. It gives no audio coverage.
- `runtime-fixture` packs the modules in `verify_route.RUNTIME_MODULES` (test/input-probe,
  digikit/remote, test/svc-echo), builds plusdrive before samples-live, and runs the runtime project.
  It runs for relevant changes with filename filters; `--full` or `--only runtime-fixture --all-files`
  runs every runtime file. Helpers inside stock/runtime/module acceptance directories use Vitest's
  import discovery to select importing test files, without collecting or running tests. Discovery
  errors, missing tools, unconsumed helpers and deleted files retain the whole owner suite.
  A changed Python tool selects every runtime file only when it is in the shared preparation closure
  (patchkit, stablebin, sharc_disasm) or is deleted while still named; otherwise it selects the records
  whose own Python `sources` reach it by import (`verify_route.acceptance_record_closure`), or nothing.
  A test that runs a Python tool lists it in its record's `sources`.
  It needs a two-core budget share and one fresh coupled daemon at a time.
- `module-load` runs the shared conformance suite `tests/conformance/load.device.test.ts` (Vitest project
  `conformance`) with `DIGI_CONFORMANCE_MODULES` set to the changed modules, on both devices, and packs
  exactly those modules (plus `digikit/debug`, the log). The uploads are all shipped modules that are
  `dynamic`, a service or `host` (`verify_route.RUNTIME_UPLOADS`, built from the manifests;
  `tests/conformance/load.ts` scans them by the same rule). Shared ABI, protocol and unknown modules
  select all uploads. `verify --conformance ID[,ID...]` runs only this step for the named modules and
  fails on an id that is not an upload.
- `rust-tests` runs `cargo nextest run --workspace --release`, without the desktop app crate. The
  `desktop` step builds the console and tests the app crate. Crate-level Rust integration-test changes
  under `packages/emulator/<crate>/tests/` keep affected Rust packages and their existing build step,
  without unrelated native acceptance or smoke. Examples keep production routes because production
  code and ready/card preparation consume some of them. Mixed source/test changes retain source coverage.
- `smoke` runs the patchkit smoke matrix (`pk_harness run`, 16 cells, one daemon each).

Smoke is skipped when every changed path is:

- UI-only (`UI_ONLY` in `verify.py`: `apps/desktop/ui/`, `apps/web/src/components/`,
  `apps/web/src/styles/`, `apps/web/tests/*.mjs`, `docs/`, `*.md`);
- a host test file matched by `vitest.host.projects.json`; or
- one of the three `SMOKE_EXEMPT` paths: `tools/devtools/agent_guard.py`,
  `tools/devtools/layercheck.py`, `.claude/settings.json`;
- a test-only path (`TEST_ONLY`: devtools tooling/tests, patchkit and digikit Python test files,
  module device tests `modules/**/tests/*.device.test.ts`, TypeScript helpers of `tests/regression/emulator`, `tests/regression/runtime` and `tests/conformance`, `tests/regression/project.json` and digikit
  scenario TOMLs),
  or a crate-level Rust integration-test source under
  `packages/emulator/<crate>/tests/`;
- a patchkit source in a `PATCHKIT_GROUPS` group that omits smoke (checks, flash, wasm, codegen);
- a `tools/**/*.py` outside the import closure of `tools.patchkit.pk_harness` (function-local
  imports and `-m tools.x` calls included), or a deleted one that no tracked file still names; or
- a `pyproject.toml` change limited to `tool.mypy` and `tool.ruff`.

These may be mixed. If every other changed path is in `digikit/pluck`, `digikit/freqshift` or
`digikit/remote`, only the matrix cells that depend on those modules run (PLUCK keeps its DT2 and DN2
cells; freqshift and remote have none). Any other path keeps the full matrix. `--explain` prints the
reason. Desktop sources `card-core.ts` and `samples-core.ts` also select `web`, because
`apps/web/tests/samples-page.test.mjs` imports them.

Each run writes `jobs.tsv` to `out/runs/verify/<run>/`: one row per node with start, end, CPU share and
load average.

### Digikit boot scenarios

The runtime loads its module set at boot. A TOML file in `tests/digikit/scenarios/` names the device,
the module sets and the expectations. `uv run python -m tools.patchkit.pk_digikit_scen list|run [NAME ...] [--image IMG] [--tier quick|full]`
boots the patched firmware through `pk_dkrt_boot` and checks. A scenario file has `[scenario] name, kind, devices, tier`
and a table named like the kind. Kinds:

| Kind | Checks |
| --- | --- |
| `equiv` | Machine, parameter, page and voice rows of a boot with set S equal those of the image patchkit builds with S's static twin; PCM of a note equal, or pinned by `pcm_sha256` (Em approves a new value). |
| `upload` | Module upload over USB SysEx: `ops`, `cut`, `stock`, `playback`, `rust`, `screenshot`, `quick`. |
| `menu` | Ticking a set in SETTINGS > DIGIKIT > MODULES and restarting loads the set; PCM equals the patched-in set. |
| `safe`, `cut` | Safe mode after a module resets the machine; card states after a power cut. |
| `debug` | Parts `memaudit`, `capacity`, `isolation` (below). |

Every scenario ends with the write-watch over `cf.expansion` (0 stray writes), the flush check (0 hits)
and the card-sector check. Tiers: `DIGIKIT_TIER=quick` is the default; `verify --full` sets `full`.
Both use the newest image of each device. Cards are cached by content in `out/cache/digikit/cards/`.
Jobs: `DIGIKIT_JOBS` (default 3 outside `verify`). `DIGIKIT_PROFILE=1` prints the phase table,
`DIGIKIT_RUN_SALT=x` forces cold boots.

The `memaudit` part (`tools/patchkit/pk_digikit_memaudit.py`) proves that a loaded module set writes
no ColdFire memory outside what each module owns while the UI is driven. The rig is ColdFire-only. It
protects the `cpu = "cf"` regions of `devices/ports/<image>.toml` minus each module's block, allows
writes only from placed patch code, and expects `guard.report` total 0. DMA and host writes are not
seen. Negative controls: `memaudit-dt2-stray`, `memaudit-dn2-stray`. `mise run module:audit NAME` runs
it for one module (docs/MODULE-AUTHORING.md).

The `capacity` and `isolation` parts fill every runtime limit with synthetic modules
(`patchkit synth-dkm`; limits in `[limits]` of `devices/*.toml`) and check the refusal codes.
`tests/patchkit/test_capacity_sets.py` checks the set sizes without an emulator;
`tests/patchkit/test_limits.py` keeps tools from repeating a limit as a number.

### Machine and FX conformance (`tests/modules`)

`mise run module:check NAME [--device dt2|dn2]` (NAME: `<author/name>` or the name alone) selects the suite by
module type and runs the module's own Python tests. A module with `tests/**/*.device.test.ts` files gets
an uncached Nx `test-device` target (inferred by `tools/nx/modules.mjs`, read from the Nx graph); `module:check` runs it only after Python success, propagates a
failure, and reports size/fit only after both stages pass. With no Python tests, the existing source
check still precedes an owned target. Frequency shifter owns the exact existing TypeScript shifter
suite on both devices, including when `--device` limits other checks. Its daemons and ready inputs
use the existing harness resolution/overrides; only resolution holds a shared benchlock, released
before the target. The target reuses the existing named verify workflow and does not run the broad
gate. Its bounded source inputs are conservative affected-selection inputs, not a cache-closure or
fine-routing claim. This does not replace automatic gate routing. The suite alone:
`DIGI_CONFORM_MODULES=<author/name> uv run python -m pytest tests/modules/test_machine_conformance.py -rf`.

Machine rules (`pk_conform.py`, rig `tests/support/machine_rig.py`, workers `tests/support/conform_worker.py`),
checked in every block of every case:

- the call returns within 400,000 instructions;
- M5, M6, M7, M13, M14, M15, L0 to L5 and MODE1 are preserved;
- all 32 output samples are finite and at most 8.0 in magnitude;
- instructions per block stay within the DSP budget headroom: `headroom / 16` per block, `headroom`
  for a note-on block. A manifest `[conformance] voice_block`, `note_on_block` replaces either number.

FX rules (`tests/modules/test_fx_conformance.py`, rig `tests/support/fx_rig.py`): the hook returns
within 200,000 instructions; registers the stock function expects are preserved; the core/fx fault
count stays 0; the module stores only to its output block, data, bss and stack; the master stays finite
within 8.0; the effect stays within `min(headroom) / 4` plus 900 for core/fx. Negative fixtures:
`test/dyn-dsp-hang` (machine), `test/fx-nan` (effect). `DIGI_CONFORM_JOBS` sets the worker count.
A change in a machine or effect module, `core/dsp-voice`, `core/fx`, `pk_conform.py` or the rigs selects the suite.

### DSP budget gate (patchkit smoke)

Every `pk_harness run` cell ends with a DSP measurement: TRIG 1 held for 1 s of audio, the mean busy
instructions per block from `status.audio.dsp_tiers`, minus the stock baseline of the same device and
OS (cached in `out/runs/patchkit/dsp/`). Projected load = busy x factor / 666,667. The factors are in
`devices/cycle-factors.json`. A cell fails when the projected high load exceeds `--dsp-budget`
(default 85 %); `--no-dsp-budget` skips it. Results go to `out/runs/patchkit/dsp-budget.json`. Unit
test: `tests/patchkit/test_pk_dsp_budget.py`.

### AOT versus interpreter equality

The no-AOT halves of `tier-quick` and `tier-sweep` use `NO_AOT_SUBSET` in `tools/devtools/verify.py`:
17 of the 134 DN2 FX scenarios and 8 of the 60 DT2 ones. `check-no-aot` runs 17 of the 61 oracle tests
(memberships in `tests/regression/scenarios/dn2-trackfx.test.ts` and `dn2-sendfx.test.ts`).
`--all-no-aot` widens this: all 134 scenarios in `tier-sweep` and all assertions in `check-no-aot`,
every seventh scenario in routed `tier-quick`.

## CPU budget of the gate

`verify` has one budget of cores for everything it runs (`tools/devtools/verify_budget.py`).

- Budget: min(physical cores - 2, memory limit), at least 1. `--jobs N` or `DIGI_VERIFY_BUDGET` overrides it.
  Memory limit: (RAM - `RESERVE_GB` 4) at `GB_PER_EMULATOR` 1 GB per emulator process (2 cores).
  Cargo jobs are capped at (RAM - 4 GB) / `GB_PER_CARGO_JOB` 6 GB, at least 1.
  Physical cores: macOS `sysctl hw.physicalcpu`; Linux sysfs topology within the affinity and cgroup
  quota; fallback logical CPUs / 2.
- Admission: each step asks for a fraction of the budget (`WANT`). It starts when at least half is free
  and gets at most its ask. At most 6 steps run at once. Shares never add up to more than the budget
  (`tests/devtools/test_verify_budget.py`).
- A share sets the step's pools: `CARGO_BUILD_JOBS`, `RUST_TEST_THREADS`, `RAYON_NUM_THREADS`,
  `DIGI_JOBS`, `DIGI_TIER_JOBS`, `DIGI_PY_JOBS`, `DIGIKIT_JOBS`, `DIGI_CONFORM_JOBS`, `DIGI_PK_JOBS`,
  `DIGIKIT_PART_JOBS=1`, node `--test-concurrency`. A pool variable you set replaces the share.
  One coupled emulator process counts as 2 cores.
- `runtime-fixture` and `module-load` run their files in parallel, one coupled daemon (2 cores) per
  Vitest worker. The step asks for 2 cores x min(4, selected files) and starts on any whole number of
  daemons; the gate sets `REGRESSION_JOBS` to the daemon count. `DIGI_RUNTIME_JOBS=N` forces the count
  (1 is the serial run). A no-progress watchdog (`tests/regression/watchdog.ts`) closes a daemon whose
  DSP frames, main loop hits and audio frames stay the same for `DIGI_NO_PROGRESS_MS` (default 120000)
  of wall time, so the test fails at once; a paused daemon without a job is idle and not counted. Step
  logs are live in the run dir (`out/runs/verify/<id>/<step>.log`); the START line names the path.
- Every python-tests file is its own job of 2 cores, queued longest first by recent timings
  (`tools/devtools/verify_times.py`).
- The seven audited devtools files in `changed_tests.FAST_PYTHON_TESTS` need no build barrier.
  Daemon-driving Python files wait for `build-daemons`; other files retain the existing build barriers
  when the plan builds daemons. The audit is shared with fast preflight; new Python files are not
  presumed safe to overlap builds.
- Builds run in a chain: `build`, `build-daemons`, `build-dt2` (`tools/devtools/verify_build.py`).
  Test steps use the immutable daemons of the daemon store (`<git common dir>/emud-cache`) as `DIGI_EMUD_<DEV>`.
- `verify` runs at nice 10 (`--foreground` keeps the priority). `--explain` prints the worker plan.

## Render cache

Scenario renders are cached under `out/cache/renders/` (`tools/devtools/render_cache.py`). The key
hashes the coupled-run binary, the input files, the scenario JSON, the peek specs, the device, the
device TOMLs, and every `SHARC_*`, `DIGI_*` and `DSP_*` variable except a denylist. A hit restores
the report entry and the WAV. Runs with `--diag` bypass the cache.
`--no-cache` or `DIGI_RENDER_CACHE=0` disables it; `uv run python -m tools.emulator.coupled cache stats|clear`
shows or empties it. Only emulator output is cached. Oracles, hash compares and pins run fresh.

Two guards protect against a stale hit:

- Audit: `tests/devtools/test_render_inputs.py` scans the crates linked into coupled-run. Every
  environment read must be in the key, in `DENYLIST` or in `IGNORED_ENV`. Counts of computed-name reads
  and runtime file reads are in `tests/devtools/render_inputs.json`. After review, regenerate with
  `uv run python -m tests.devtools.test_render_inputs --scan`.
- Spot check: `DIGI_RENDER_CACHE_CHECK=p` re-renders each hit with probability p and fails on any
  difference. `verify` sets p=0.03.

Use `verify --no-cache` for release and publication gates.

## Nx cache

`pnpm nx:pilot` keeps the task cache and the workspace data in `.nx/` of each worktree. The cache is
per worktree: Nx 22's database cache does not restore entries that another worktree recorded. A hit
replays the stored output of a target with `cache: true`; the hash covers only the declared `inputs`.

A target may be cached only when `uv run python -m tools.devtools.nx_input_audit PROJECT:TARGET`
passes. The audit runs the command with `tools/devtools/fs_trace.mjs` preloaded and compares every
file read with the expanded inputs. Run it after changing the inputs or the command of a cached
target. `--all-cached` audits every cached target; `--json FILE` keeps the full result.

### Daemon store

`verify_build daemons` (the gate's `build-daemons` node) keeps the coupled `digi-emud` daemons in
`<git common dir>/emud-cache/<dev>/<key>/` (`digi-emud`, `key.txt`, `depfile.d`). All worktrees of the
clone share it and it is never committed. A worktree with the same key restores the daemon and builds
nothing; a stored file is never rewritten. The last 4 used keys per device stay.

The key is printed by `uv run python -m tools.devtools.emud_key key --device DEV`: a sha256 over the
content of the repository files in `SOURCE_GLOBS` and `RECIPE_FILES`, every file of the generated core
directory (resolved as `pk_emud_pgo` does), the PGO profile, `rustc -vV` of the pinned toolchain, and the
build flags. A device without its stock firmware or ready directory is skipped.

Each build audits itself: `verify_build` compares the dependency file cargo wrote (`digi-emud.d`) with
the key. A dependency that the key does not cover (and that is not under the cargo target dir) fails the
build, so nothing is stored with an incomplete key. Add the missing path to `SOURCE_GLOBS` in
`tools/devtools/emud_key.py`. `uv run python -m tools.devtools.emud_key audit --device DEV --depfile FILE`
repeats the check on a kept file.

To clear the Nx cache: `pnpm nx:pilot reset`.

## PCM pins

PCM pins in `tests/scenarios/*.json` change only with Em's approval. `tests/sharc/integration/test_sharc_golden.py`
hashes six SHARC core outputs: a refactor keeps them; an intended change updates them with
`--update` and says why.

## Desktop UI preview

Type checks do not show a misaligned row. `mise run ui:preview` serves the desktop pages
(`apps/desktop/ui`) at http://127.0.0.1:5199/ over a fake device (state in the address:
`?dev=ok|empty|noreply|error|slow&folder=files|empty|none&speed=MiB/s&mode=modules|samples`).
`node tools/devtools/ui_shots.mjs --out DIR [--prefix before] [scene ...]` writes one PNG per scene at
1440x900 and at 700 px wide (`--list` lists scenes). Look at the pictures before and after a UI change.

The Firmware page (`?page=firmware&fwdev=dt2|dn2&fwstate=stock|runtime|noreply|none`) runs the real patchkit checks (`patchkit.wasm`) over a fake
device that restarts after the last message. `node tools/devtools/firmware_shots.mjs --app desktop|web [--url URL] [--out DIR] [scene ...]` takes its
pictures for the workbench (this preview) and for the site (`astro dev` or `astro preview`, with a fake Web MIDI port); it needs the stock files in
`firmware/` and `mise run wasm:patchkit` and `pnpm build` (or the site's `gen-modules`) for `patchkit.wasm` and the module bundle.
The same flash against an emulated device at OS UPGRADE: `os_upgrade_rig.py ... --exec` runs `apps/web/tests/firmware-emulator.test.ts` (finding 75).

To test pages against a running emulator, `uv run python -m tools.devtools.emu_live --device dn2 --run-dir DIR`
starts a daemon, `apps/desktop/ui/preview/live.vite.config.ts` serves the pages over it
(`DIGI_LIVE_SOCKET=SOCKET`), and `node tools/devtools/memory_live.mjs SOCKET [--check]` measures the
Memory panel.

## Known issues

- DN2 chorus-width sweep assertion (full `fx-dn2` tier): an open question whether the oracle or the
  DSP is wrong. See [tests/regression/scenarios/README.md](../tests/regression/scenarios/README.md).
- `tests/sharc/generation/test_sharc_pcode_language.py::test_temporary_artifacts_match_in_tree_when_present`
  fails: the in-tree Ghidra `.slaspec` is stale versus the generator.

## Resumable SHARC matrix

`mise run perf:matrix` runs correctness before paired timings and resumes from
saved artifacts. For an environment-only experiment, reuse the **same frozen
executables** on A and B and put the differing settings in the configuration's
`variants` object. `run` and `status` never build emulator binaries:

```sh
mise run perf:matrix -- run out/matrix/config.json --baseline out/matrix/frozen --candidate out/matrix/frozen --out out/matrix/results
mise run perf:matrix -- status out/matrix/results
```

To freeze existing binaries without Cargo, use `freeze --import-manifest FILE` as
described below. Source changes require an explicit build: freeze A before editing
native sources, then freeze B after the change. Each destination must be new; the
command never switches source revisions.

```sh
mise run perf:matrix -- freeze --out out/matrix/baseline
# Make the candidate source change, then explicitly build and freeze it:
mise run perf:matrix -- freeze --out out/matrix/candidate
mise run perf:matrix -- run out/matrix/config.json --baseline out/matrix/baseline --candidate out/matrix/candidate --out out/matrix/results
```

Freeze builds the existing coupled-run and coupled daemon targets for both devices,
under the shared benchlock and target build locks, then copies the executables into
the destination. Generated cores and current stock firmware must already exist.
For an existing build, `freeze --import-manifest FILE` avoids Cargo. The manifest is:

```json
{
  "dt2": {"coupled": "/absolute/coupled-run-dt2", "daemon": "/absolute/digi-emud-dt2", "profile_firmware": "/absolute/stock-dt2-1.17.syx"},
  "dn2": {"coupled": "/absolute/coupled-run-dn2", "daemon": "/absolute/digi-emud-dn2", "profile_firmware": "/absolute/stock-dn2-1.12.syx"}
}
```

Imported executables have unknown source provenance; current sources are recorded
as the freeze tooling identity, not claimed as their build inputs. Each device's
fast-region defaults are resolved from `profile_firmware` at freeze time and stored
with its hash. For a historical baseline, supply an optional `env` object on each
manifest device to replace those defaults with its previously recorded values.
Runtime replay images use these stock defaults even when their patched hash has no
entry in the current fast-region table.

The run configuration lists prepared replay cells; it creates no replay fixtures:

```json
{
  "pairs": 3,
  "seconds": 3,
  "timeout": 300,
  "instrument_timeout": 1800,
  "instruments": ["dt2", "dn2"],
  "env": {},
  "variants": {"A": {}, "B": {}},
  "cells": [{
    "id": "dt2-stock",
    "device": "dt2",
    "kind": "stock",
    "inputs": {
      "firmware": "/absolute/stock-dt2-1.17.syx",
      "DSP image": "/absolute/dsp.bin",
      "DSP state": "/absolute/dsp.state",
      "CF snapshot": "/absolute/cf.snap"
    },
    "card": "/absolute/card.img",
    "scenarios": "tests/scenarios/dt2.json",
    "names": ["dt2-sample-trig"],
    "env": {},
    "variants": {"A": {}, "B": {}}
  }]
}
```

Add cells for DT2 runtime, DN2 stock and DN2 runtime to complete replay coverage.
`card` is optional; scenario sources may be JSON or executable TypeScript. IDs and
scenario names must contain only letters, digits, underscores, dots and hyphens,
starting with a letter or digit. Environment precedence is frozen device defaults,
run `env`, cell `env`, run label overrides, then cell label overrides; a JSON null
unsets a variable. Only emulator prefixes (`SHARC_`, `DIGI_`, `DSP_`, `DIGIKIT_`) and
OMP/Rayon/OpenBLAS thread counts are accepted. Inherited values in those namespaces
are removed before applying the recorded values. Values beginning with `@` name
files whose full contents are included in the resume identity.

The preflight checks all replay inputs and selected scenarios before starting a
child. Each replay exports the full WAV and SHRD state. Every sample must exit
successfully, pass report gates, produce finite positive audio duration and thread
CPU, and match the report's full PCM hash. A/B must agree on full PCM, complete
SHRD hash, final DSP instruction count and actual audio duration. Requested duration
allows normal native runner frame overshoot. Both labels then run
`equiv-dyn-pluck` on each selected device with their frozen daemon; the existing
scenario gates must pass and both full `run.wav` and `ref.wav` PCM hashes must agree
across labels. The PLUCK helper prepares its normal patch/module/card artifacts;
only emulator binaries are frozen by this command.

Any correctness failure stops before timings. Timing pairs alternate A/B then B/A
under one exclusive benchlock, using the same validated sample helper as
`coupled_ab.py`. Each timing sample must also match its earlier correctness result.
The JSON summary records B/A ratios and tier counts; it makes no performance
acceptance decision or automatic rollback. DSP patch experiments remain available
through `coupled_ab.py --patch LABEL=FILE`. The matrix requires strict full-state
equivalence and does not support arbitrary firmware patches that intentionally
change the exported state.

Repeating `run` resumes completed steps. Config, consumed inputs (including cards
and environment files), binary hashes, effective environment, device tables,
scenario documents and runner/source hashes must match. Reports, WAVs, SHRD dumps,
argv, filtered environment, logs and kept instrument artifacts are retained;
completed artifact hashes are rechecked, and checkpoints are atomic. A failed or
interrupted pair restarts both sides. One process may write a run directory at a
time. `status` validates the recorded artifacts before printing compact JSON.

Full coverage requires declared stock and runtime cells plus PLUCK for **both**
devices. A bounded pilot may use `--allow-partial` and `"instruments": []`; its
successful status is always `partial-pass` with missing coverage listed. Such a
result cannot stand in for full validation. Firmware and generated artifacts stay
under ignored output directories and must never be committed.
