# Tests

Reference for the test levels, commands, inputs and gate. Use `mise run test` for affected verification
and the merge gate. Use `mise run test:plan` first when selection is unclear;
it explains the same task selection without executing tests or builds. `test:changed` is cheap preflight,
not complete verification. Other docs
link here. Results of runs belong in `docs/findings/`, not here. Planned changes:
[docs/plans/test-architecture.md](plans/test-architecture.md); gate speed:
[docs/plans/gate-speed.md](plans/gate-speed.md). Current implementation sequence and evidence:
[test/build workflow](plans/test-build-workflow.md). The current module and SDK iteration follow-up is
[fast module and SDK iteration](plans/module-sdk-iteration.md).

## Test levels

A level says what a test proves. Firmware inputs, device version, cost and exclusive resources are
execution requirements, not levels. A successful boot proves only what the run asserts.

| Level | What | Command |
| --- | --- | --- |
| Static (JS/TS) | oxlint, Prettier check, host Vitest, `pnpm -r run typecheck` | `pnpm verify:host` |
| Static (Python) | ruff check and format over pyproject `[tool.ruff] include`; mypy over `[tool.mypy] files` | `mise run lint:py`; `mise run check:layers` (module layering, docs/ARCHITECTURE.md) |
| Fast preflight | Affected projects only; no builds, no devices | `mise run test:changed` |
| Unit (TS host) | Packages, desktop UI logic, regression helpers; no device | `pnpm test:host` |
| Unit (web) | Web app Node suites in `apps/web/tests` | `pnpm --filter @digi/web test` |
| Unit (Python) | RE, firmware and dev tools | pytest on named files under `tests/<owner>/`; `mise run test:pypy` for the PyPy subset |
| Unit (Rust) | Workspace and standalone dkrt | Explicit Nx project target `rust-tests`; runtime-only: `pnpm nx:pilot run core/runtime:rust-tests` |
| Component | Module DSP conformance on the SHARC core | `mise run module:check ID` |
| Device: stock UI | DT2 stock pages and transport goldens (zero DSP) | `mise run test:stock` |
| Device: FX | Stock FX oracles | `mise run test:fx`, `mise run test:fx:dt2` |
| Device: runtime | Emulator oracles (`tests/regression/emulator`) and module-owned device tests (`modules/**/tests/*.device.test.ts`) | `pnpm nx:pilot run <module-id>:test-device` |
| Device: module loads | One shared conformance suite (`tests/conformance`) loads every shipped upload | `pnpm nx:pilot run-many -t test-conformance` |
| Device: Python | Module equivalence, pluck menu and pages scenarios, `upload-rust`, patchkit smoke matrix, patched-boot pytest files | owner pytest files; `mise run patch:test` |
| App E2E | None yet | none |

A mocked UI test, a preview or a daemon scenario is not app E2E coverage.

## Which check for which change

Use an explicit owner target or named test while iterating. Run `mise run test` once before merging;
iteration checks do not replace this gate. `mise run test:plan` explains task selection without running it.

| Change | While iterating |
| --- | --- |
| JS/TS host tests and helpers | `pnpm nx:pilot run <project>:test-host`, `<project>:typecheck` |
| Web Node tests | `pnpm --filter @digi/web test` |
| Desktop UI or app | `pnpm nx:pilot run @digi/desktop:test-host`, `@digi/desktop:check:ui` (source types); `@digi/desktop:typecheck` for test types. `ui:preview` and `ui_shots.mjs` for pages (below). |
| Stock DT2 UI goldens | `test:stock` |
| DN2 or DT2 FX sources and oracles | `test:fx`, `test:fx:dt2` |
| Python under `tools/` or `tests/` | `test:changed`; then the named pytest files |
| Patchkit, ports | `test:changed`; `patch:test --cell ...` for one smoke cell |
| Leaf module | `module:check ID` |
| Device tests of one module | `pnpm nx:pilot run <module-id>:test-device` |
| One device test file | `pnpm nx:pilot run <module-id>:pack`, then `pnpm --filter @digi/regression test:runtime <path filter>` |
| Loading one module | `pnpm nx:pilot run <module-id>:test-conformance` |
| Runtime loader fixtures | `pnpm nx:pilot run core/runtime-fixture-tests:test-device` checks a machine and FX on both coupled devices; `core/runtime-service-tests:test-device` covers a zero-DSP service. |
| Empty-store menu fixture | `pnpm nx:pilot run core/digikit-menu-zero-tests:test-device` checks common menu navigation on both devices with zero DSP; the existing `core/digikit-menu:test-device` keeps coupled capacity checks. |
| SDK UI fixture | `pnpm nx:pilot run sdk-ui-zero-tests:test-device` exercises the `test/ui-demo` SDK UI controls on both devices with zero DSP. |
| Public SDK headers | `pnpm nx:pilot run sdk:test-host` and `pnpm nx:pilot run sdk-ui-zero-tests:test-device`; add the runtime loader fixtures above when load or ABI behavior is affected. |
| Framework module, runtime | `core/runtime:rust-tests` for runtime units; an acceptance owner's `test-device` for iteration (runtime owners are listed below). Use `test:plan` for affected consumers. The default gate uses fixtures instead of unchanged shipped conformance only for an isolated `digikit-menu/src/menu.c` or `adapters/ui/src/ui.c` edit; other runtime and public SDK edits retain broad selection. |
| SHARC core or transpiler | regenerate the cores, then `test:changed` |
| Emulator crates | `test:changed`; the crates' Rust tests |
| PCM pins (`tests/scenarios/*.json`) | only with Em's approval |

## Commands

Nx owns task selection and prerequisites; mise supplies thin entry points. Nx 24 selects individual
affected tasks using their declared inputs and dependencies. Both local plugins (`tools/nx/modules.mjs`
and `cargo.mjs`) discover projects without running Cargo. Explicit Rust targets use their pinned commands.
`nx.json` owns shared inputs, target defaults and plugin registration. `tools/nx/modules.mjs` infers
module targets and patch-set inputs; `tools/nx/cargo.mjs` infers Rust targets and path dependencies.
Each owner's `project.json` supplies its explicit targets and overrides. The real plan regressions
in `tools/nx/tests/selection.test.ts` verify native Nx input resolution and affected selection.
Device targets may build their declared packs, CLIs and daemons on demand; direct Vitest invocations
require those artifacts to exist already (see "Inputs").

`test:changed` runs only affected lint, format, types, host tests and Python checks. `test` runs the
complete affected target list, with `test:plan` adding Nx `--explain` to that same invocation.
The default gate requests affected module `pack` targets directly, even when a module has no device-test
target. A `module.toml` edit also selects the host-only `patchkit:catalog-check` target once to validate
repository-wide manifest IDs and requirements; each module's pack and conformance read its own manifest
and declared requirement closure.
`DIGI_GATE_BASE` defaults to `main` and is exported so Nx and the Python selector compare the same base.
Set `DIGI_GATE_BASE=HEAD mise run test:plan` to inspect only uncommitted work, or use
`mise run test -- --explain=/tmp/test-plan.json` to save native Nx selection reasons.
Use `DIGI_GATE_BASE` rather than forwarding `--base`, so Python sees the same comparison.
For a proposed file, invoke `pnpm nx:pilot affected --files=path/to/file -t TARGET --explain` directly;
Nx rejects `--files` together with the main command's `--base`.

Control-client, test-support, dk-session, desktop-host and regression-host targets use local result
caching; web-transfer and the per-module `test-host` targets remain uncached. tools/nx/modules.mjs infers, for each shipped module, a `test-host` target (host tests under `tests/`), a `test-device` target (its `tests/**/*.device.test.ts`, run with Vitest directly after its packs) and, for each shipped dynamic, service or `host` module, a `test-conformance` target (the shared conformance suite for that module). The gate runs two tasks concurrently; device targets set `REGRESSION_JOBS=2`. Other Nx targets: `pack` (patchkit pack for each device, building stale module objects on demand, into `dist/modules/<device>/<leaf>.dkm`; only for the modules the gate packs, not for patched-in core modules or static fixtures), `pack-aliases` (renamed copies named by `runtimeAcceptance` aliases, same directory), `patchkit:cli` (`dist/patchkit/patchkit`) and `plusdrive-format:cli` (`dist/plusdrive/plusdrive`). Outputs go to `dist/`, never under `out/`. Shipped content modules (not `core/*`) depend on `core/runtime`; machines also on `core/dynreg` and `core/dsp-voice`, effects on `core/dynfx` and `core/fxpages`. Web tests retain ignored
generated WASM and optional inputs outside the cache boundary. Cache use assumes a frozen installation. New test helpers, environment
reads or external inputs require input/output review and scoped invalidation/restoration evidence before relying on cached results. The
[bounded cache observations](findings/nx-pilot.md) include dependency failure/restoration probes;
only the support/session pair was timed, not whole preflight. The standalone runtime unit target
remains uncached; all test-only device targets, including `core/runtime-tests`, are cached like other module device targets.
Plugin sources are not inputs of module device targets. A `runtimeAcceptance` record may set `dksampleWasm: true`
(depends on `dksample:wasm`) and `pythonTools` (`true` hashes the Python tools in `PYTHON_TOOL_SOURCES`; `"client"` hashes only the smaller
`PYTHON_CLIENT_SOURCES`; both in `tools/nx/module-targets.mjs`, for tests that spawn Python tools).

| Command | What runs |
| --- | --- |
| `mise run test:changed` | `nx affected` against `DIGI_GATE_BASE` (default `main`) over the targets `lint`, `fmt-check`, `typecheck`, `test-host` and `python-checks`. Does not run Rust, module builds or devices. |
| `pnpm test:host` | All eight Vitest projects from `vitest.host.projects.json`: control-client, test-support, dk-session, web-transfer, desktop-host, regression-host, module-tests, nx-host. One worker by default; `-- --maxWorkers N` changes it. |
| `pnpm verify:host` | oxlint, Prettier check, host Vitest, typecheck of control-client, dk-session, test-support, regression and desktop. Stops at the first failure. |
| `pnpm --filter @digi/web test` | `node --test` in `apps/web/tests`. Cases that need WASM or firmware skip when absent. |
| `mise run lint:py` | `ruff check` and `ruff format --check` over the pyproject include list. |
| `mise run lint` | `lint:py`, `lint:rust`, `pnpm lint`, `pnpm fmt:check`. `lint:rust` runs `workspace:rust-fmt` (`cargo fmt --check` on every workspace crate except `dkfw`). `dkfw` is left out because its crate root is generated. Other generated files carry `#[rustfmt::skip]` on their `mod` line. A format change of `dkrt-sdk`, `dkrt-fit` or `dkneeds` changes `source_sha256` in the committed `build/*.dko.json` objects, so rebuild them with `core/runtime:build-objects`. |
| `mise run test:pypy` | The SHARC core tests under PyPy 3.11. Slow. |
| `mise run test:stock` | Vitest project `stock` (`tests/regression/dt2`): DT2 1.17 zero-DSP goldens. Filters pass through. |
| `mise run test:fx` | Project `fx-dn2`: 61 quick assertions; `DIGI_FX_SWEEP=1` runs 191 across 134 scenarios. `DIGI_JOBS` bounds rendering. |
| `mise run test:fx:dt2` | Project `fx-dt2`: 135 quick assertions; `DIGI_FX_SWEEP=1` runs 146 across 60 scenarios. `DIGI_DT2_CARD` selects a prepared card image. |
| `pnpm nx:pilot run <module-id>:test-device` | The module's `*.device.test.ts` files (Vitest project `runtime`) after its packs, aliases and the plusdrive CLI are built into `dist/`. |
| `pnpm nx:pilot run-many -t test-conformance` | Project `conformance`: `tests/conformance/load-{dt2,dn2}.device.test.ts` on DT2 1.17 and DN2 1.12, once per eligible module (`DIGI_CONFORMANCE_MODULES` is set per module). |
| Other Nx gate targets | `digi-control:daemons` builds the daemons and writes `dist/daemons.json`; device helpers read it when `DIGI_EMUD_*` is unset. `digi-coupled:build` and `build-dt2` build the coupled emulator. `@digi/web:patchkit-wasm` builds the patchkit Wasm (the web `test` depends on it). `workspace:lint` and `workspace:fmt-check` run `pnpm lint` and `pnpm fmt:check`. Project `regression`: `test-device` runs `tests/regression/emulator/` after the packs of its runtime-acceptance records; `test:stock` depends on `daemons`, `test:fx` and `test:fx:dt2` on the coupled builds; `test:fx-no-aot` runs `test:fx` in the no-AOT configuration. Project `@digi/desktop`: `rust-tests` runs the app crate tests. Owner projects `tools`, `devices`, `sdk` and `tests` own the paths no other project owns; `tools` carries `python-checks` and `python-tests`. The emulator gate targets live in the gate projects under `tools/gate/` (see "Nx gate"). |
| `mise run module:check ID` | Machine/effect conformance and own Python tests, then any module-owned Nx `test-device` target. May build or emulate; size/fit follows both. Not an all-module load proof. |
| `mise run module:audit NAME` | ColdFire memory audit of one run-time module. |
| `mise run patch:test` | Smoke tour over `tests/patchkit/smoke/matrix.toml` (`-- --cell dt2-1.17:core/fx`, `-- --update`). |
| `mise run patch:proofs` | Every emulator proof on its quick tier. |
| `mise run dev:plusdrive-check` | The firmware's own view of an image built from a folder (about 1 min). |
| `mise run scenario NAME` | Replays one headless scenario. |
| `mise run scenario:all` | Every DN2 scenario; fails when a pinned PCM hash differs. |
| `mise run test:slow` | `test_outcomes run --check` over `tests` with `--slow` and a one-hour per-file timeout, then `uv run python -m tools.emulator.coupled gates`. |
| `mise run test:quick` | Launcher, PGO, target and coupled-tool tests, then `coupled test-quick`. |
| `mise run test` | Complete affected verification and merge gate: `nx affected` against `DIGI_GATE_BASE` (default `main`). |
| `mise run test:plan` | The same selection with native Nx `--explain`; no tasks execute. |
| `mise run test:full` | Every gate target on every project, plus `tier-sweep` and `dt2-tier-sweep`. Only after generated-core changes. |
| `mise run clean:runs` | Lists old `digi-*` run directories under the temp dir (older than 24 h, not used by a running `digi-emud` or node process) and orphan `digi-emud` processes; `-- --yes` deletes the directories, `-- --hours N` sets the age. It never kills a process. A Session refuses to start with less than 50 GB free. |

### Nx gate

- `mise run test` runs `nx affected` over the gate targets against `DIGI_GATE_BASE` (default `main`), through `node tools/nx/gate.mjs --tier merge`; the Nx launcher holds the shared benchlock and runs at nice 10.
- `mise run test:full` runs every gate target on every project, plus `tier-sweep` and `dt2-tier-sweep`. Run it only after generated-core changes.
- Gate projects in `tools/gate/` carry their triggers as `{workspaceRoot}` inputs: `gate-smoke` (target `smoke`), `gate-card` (`card`), `gate-stock` (`stock`, cached), `gate-fx-dn2` (`fx`, `fx-no-aot`, `tier-quick`, `tier-sweep`), `gate-fx-dt2` (`fx-dt2`, `dt2-tier-quick`, `dt2-tier-sweep`), `gate-pins` (`pins`), `gate-dt2-tone` (`dt2-tone`) and `gate-rust-modules` (`rust-tests`). The Vitest suites behind `stock`, `fx` and `fx-dt2` are the `test:stock`, `test:fx` and `test:fx:dt2` scripts of `tests/regression`. `smoke` and `card` run through `tools/nx/daemon-env.mjs`, which sets `DIGI_EMUD_<DEV>` from `dist/daemons.json`, so they use the same content-keyed daemon as the Vitest device tests. Module projects with device tests depend on `regression` (harness), `digi-control` (daemons), `patchkit` (packs) and `devices`; shared SDK contract/compiler surfaces are explicit target inputs. Only modules with `test-conformance` also depend on `conformance`. `python-checks` and `python-tests` run on any change outside `docs/`.
- The number of concurrent Nx tasks and Vitest workers comes from the machine budget; see "CPU budget of the gate".
- Nx's `summary` output reports counts and paths to full failure logs; `--output-style=static-failures-only` prints failing task output inline. No custom status router is involved.

Wait without polling: `node tools/devtools/job.mjs start NAME [--timeout SEC] -- CMD ARGS...` runs a command detached and returns at once; `node tools/devtools/job.mjs wait NAME` blocks (default 540 s, `--max SEC`) and prints `job NAME: exit N after Ns` and the last lines of the log, so one call replaces a loop of `sleep` and `tail`. It exits with the job's code, 75 while the job still runs (call `wait` again), 70 if the process died, 124 on `--timeout`. `status [NAME]` lists the jobs of this worktree. State is in `out/runs/jobs/<worktree>/NAME/` (`log`, `meta.json`, `exit.json`). Nx detects agents (`isAiAgent`, which reads `CLAUDECODE` among others; `AI_AGENT` alone does not trigger it) and then prints the `summary` style, so task output appears only in the log paths it names. A person who wants a live log sets `NX_DEFAULT_OUTPUT_STYLE=stream` (or passes `--output-style=stream`).

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

For a no-argument DN2 preview, `mise run dk:menu` opens the real DIGIKIT menu in a visible browser faceplate;
`mise run dk:ui-demo` opens the separate [`test/ui-demo` fixture](../tests/fixtures/modules/ui-demo/README.md), which conflicts with the real menu. See [visible previews](DEVELOPMENT.md#visible-preview) for the quick start. Both start or reload their kept daemon with the same automatic DSP selection as
`dk:dev`, navigate to the menu, then open `/panel.html` through the live Vite proxy on an available localhost port against that exact daemon.
The command prints the exact panel URL. `dk:dev` itself writes screenshots without opening a browser.
Keep the command running while using the panel. Ctrl-C stops its browser server and keeps the emulator for the next reload;
`mise run dk:dev -- emulator --stop` or `mise run dk:dev -- test/ui-demo --stop` stops the respective emulator.
The commands also write `out/runs/devreload/dn2/digikit-menu.png` or `ui-demo.png`. Use `dk:dev` directly to choose DT2, load a particular module or card store, or provide another key script.

For a content module, `mise run dk:module ID [--device dn2|dt2] [--dsp]` packs its card `.dkm`, selects it for boot and opens the MODULES list in the live faceplate. It uses the `emulator` patch set. Declarative machines default to a data-only UI preview pack with zero DSP and no audio; preview packs reject actions, samples and hooks. ColdFire-only services keep normal packing with zero DSP. Effects and other DSP-backed modules require `--dsp` for the full pack and automatic SHARC selection. `dk:module` delegates to generic `dk:dev --store`: the generic path stages content and compares the exact request and initial selection. Unchanged content keeps the daemon and overlay, including a user-edited live selection; changed content rebuilds the card and restarts that exact daemon. Stop and restart to restore the initial selected card after changing selection live. `mise run dk:module ID --stop` stops it. See [visible previews](DEVELOPMENT.md#visible-preview) for aliases and selection behavior. This interactive preview does not replace the module's acceptance checks.

The Pluck zero-DSP UI proof is `pnpm nx:pilot run digikit-pluck-preview-tests:test-device`. It runs two UI-only tests across DT2 1.17 and DN2 1.12 (42.5s for the named target) and has no dependency on the full Pluck pack. Full module conformance remains coupled to SHARC.

`dk:dev` automatically reports and selects DSP `zero` for ColdFire-only module closures (such as `test/ui-demo`), skipping PCM
capture. `tools/patchkit/pk_devreload.toml` lists base patch sets whose menu/runtime supports a zero DSP
peer despite optional DSP adapters (`emulator` on both devices). Store module closures are always checked independently: adding
`--store digikit/pluck` selects coupled SHARC, as do content DSP objects, unknown object placements and arbitrary `file:` store content.
Other patch sets, including `sample-tracks`, use the conservative object check. Each DSP mode has its own kept daemon directory;
the mode and audio status are checked after connecting and reloading. No additional flag is needed.

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
| `tests/devtools` | Benchlock, agent guard, render-cache audit, test outcomes |
| `tests/digikit` | Digikit boot scenarios (below); data in `scenarios/*.toml`, plus `modules/*/*/tests/scenarios/` |
| `tests/regression` | Native Vitest: `dt2/` (stock), `emulator/` (emulator oracles), `runtime/` (helpers only), `scenarios/` (FX), `host/` (no device) |
| `tests/conformance` | The shared module-load suite (`load-dt2.device.test.ts`, `load-dn2.device.test.ts`, `load.ts`) |
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
  add new typed files to `[tool.mypy] files`. Both run as commands in the gate target `python-checks`.

Device sessions run in a claimed `dist/session-cache/<device>-<n>` (per worktree, gitignored). A vitest
worker process claims the first free `<n>` with a pid lock file (`<device>-<n>.lock`) and keeps it until it
exits; later processes reuse the dir, so the daemon's boot cache stays warm. It is pruned to 4 snapshot entries.
`liveOptions(..., { fresh: true })` and mirror sessions use a fresh temp dir.

## Module-owned tests

A module keeps its tests in its own folder, `modules/<kind>/<name>/tests/`. No list names them; globbing finds them.

- `test_*.py`, also in subfolders: pytest files. The basename is unique across the repository (prefix it with the module name). A helper
  next to the test is imported by name. Use `Path(__file__).parents[1]` for the module folder.
- `*.test.ts`: Vitest project `module-tests` of `vitest.host.projects.json` (`pnpm test:host`), found recursively under `modules/`.
- `*.device.test.ts`: device tests, registered in the owner's `project.json` (`metadata.runtimeAcceptance`, files relative to that
  project root) and run by Vitest's `runtime` project. Runtime acceptance is owned by `core/runtime-tests`
  (`modules/core/runtime/tests/project.json`) with CF-only service commands and settings in nested
  `core/runtime-service-tests` and `core/runtime-settings-tests` (`service-commands/project.json` and
  `settings/project.json`). Each owner's `test-device` runs its files;
  `mise run module:check core/runtime` discovers and runs all owners serially.
  `pnpm nx:pilot run <module-id>:test-device` runs one module's files; `pnpm nx:pilot run <module-id>:test-conformance`
  loads the module through the shared conformance suite. Uploadable modules without a DSP image run
  with the zero SSI peer and no SHARC audio session; modules with DSP images retain coupled execution
  and the DSP registration wait. Both paths retain upload/restart, persisted bytes/hash, SDK contract,
  registration, UI and fault acceptance, including the same forbidden runtime log kinds. Task and
  cache ownership remain per module.
- `scenarios/*.toml`: digikit scenarios. `pk_digikit_scen list|run NAME` and the tiers find them in `tests/digikit/scenarios/` and `modules/*/*/tests/scenarios/`.
- `module:check ID`, `gate` and `test:changed` select them. A change in the module selects its tests and those of the modules that depend
  on it; a change only in `tests/` runs python-tests (and `host` for `.ts`), no Rust or web step.
- A scenario that pins the PCM of a module lives with that module. A shared test needs "a module" only: it uses a fixture of
  `tests/fixtures/modules` (`test/sine`, a dynamic machine on both devices; `test/dyn-fx-gain`, an effect; the table is in
  docs/MODULE-AUTHORING.md), or a made-up id such as `acme/demo` when nothing is built. A list of all shipped modules is a scan of
  `modules/{machines,fx,tools}/*/module.toml`, never a literal list.
- `mise run check:layers` (rule `self-contained`): no tracked file outside a module's folder names its id or path. Docs, `*.md`, `.agents/`
  and the fixtures may. `host = true` in `[module]` exempts a module the workbench speaks to (the remote control service).

## The gate

### Gate selection

Nx selects by project. `pnpm nx:pilot show projects --affected --base main` prints the selection
without running it.

- The gate projects `gate-smoke`, `gate-card`, `gate-stock`, `gate-fx-dn2`, `gate-fx-dt2`, `gate-pins`,
  `gate-fast-roots` (`fast-roots`), `gate-dt2-tone` and `gate-rust-modules` (in `tools/gate/`) carry their trigger paths as positive
  `{workspaceRoot}` globs in their target inputs. Nx affected ignores negated (`!`) inputs, so inputs
  that decide selection must be positive globs.
- `gate-fast-roots` replays fixtures built from the current runtime and fails when interpreted guest instructions per audio second (or, where a budget sets it, generated-block entries per audio second) exceed `tools/emulator/fast_budgets.json`, the sign that runtime patches bar hot generated regions or the generated core lacks links for hot blocks. DN2 has two cases: `first-boot` (the state right after the first boot) and `reset` (the state after a guest soft reset, which installing modules and Apply & Restart boot into another kit with effect sends active; about 40% more host work). DT2 keeps its first-boot state, so it has one case.
- Module projects with device tests depend on `regression` (harness), `digi-control` (daemons),
  `patchkit` (packs) and `devices`; shared SDK contract/compiler surfaces are explicit target inputs.
  Only modules with `test-conformance` also depend on `conformance`. A change in a dependency selects the modules that use it. The module targets
  `test-device` and `test-conformance` hash `^production` of their dependencies. `production` is the
  named input in `nx.json` (default minus `{projectRoot}/tests/**` and Markdown); for `regression` it is
  the harness only (see `tests/regression/project.json`). So a change to a stock test, a golden, an FX
  scenario, a host test or another module's tests still selects the dependent modules, but their device
  results replay from the cache. Packs still run (they are not cached).
- Build, pack and device inputs share `sdk/sdk.toml`, public `sdk/include`, `sdk/asm`, `sdk/dsp` and `sdk/vectors`.
  Optional implementation sources under `sdk/src` belong to their explicit consumers: pack manifests name the C
  files, with adjacent private headers included, and `dkui-host:rust-tests` names the picker sources and headers
  compiled by its build script. Module and Cargo source ownership remains project-wide. A picker helper edit
  selects Shot's pack, device and conformance checks and the UI host tests, plus their prerequisites; unrelated
  module packs and conformance are excluded. Public headers and the SDK contract retain broad selection.
  `sdk:check-generated` names the shared generator inputs and all generated output surfaces, including outputs
  outside `sdk`; changes to generator output paths must update this list. It excludes optional `sdk/src` sources.
- `tools:python-checks` and `tools:python-tests` run on changes to tools, tests, modules, sdk, devices, Python and
  Cargo manifests, and the patchkit, dkneeds, control and plusdrive packages. `python-checks` runs ruff, ruff
  format, mypy, layercheck and `patchkit gen --check`. `python-tests` first runs
  `tests/patchkit/test_pk_current.py` (module and port staleness), then the Python test files that
  `tools/devtools/py_select.py` selects by the diff: the gate test (`PY_GATE`), the `PY_SUITES` the
  change triggers, the tests that reach a changed module in the import graph, and the changed tests.
  One process runs one file.
- Inputs name the toolchain by version (runtime inputs such as the pinned `rustup run <version> rustc -vV`, node versions), not by
  `mise.toml`, so task edits in `mise-tasks.toml` do not select every project.
- If the selection is wider or narrower than the change can reach, fix the inputs and edges in
  `tools/nx/*.mjs` or the `project.json` files.
- The default `mise run test` and `test:plan` use `tools/nx/gate.mjs` for two reviewed exceptions: when the only change is a modification to either `modules/core/apps/digikit-menu/src/menu.c` or `modules/core/adapters/ui/src/ui.c`, Nx runs its affected targets without unchanged shipped-module `test-conformance`. An edit to both files is mixed and retains normal selection. The menu's zero-DSP and coupled owners, runtime fixtures, and `dkui-host:rust-tests` remain selected. Its host test declares the real `menu.c` input in `modules/core/adapters/ui/host/project.json`.
- For isolated `ui.c` edits, `sdk-ui-zero-tests:test-device`, both menu owners, both runtime fixture owners, every other native-selected consumer device test, and their build prerequisites remain selected. Nx recalculates dependencies after conformance is omitted. This accepts losing the per-shipped-artifact upload, stored-byte/reboot, registration, contract/DSP-status and fault-containment conformance proof for that edit; the fixtures do not establish that every shipped artifact loads. The raw hypothetical plan for `pack,test-device,test-conformance,rust-tests` has 20 device targets, nine conformance targets and 34 pack tasks (87 tasks including prerequisites). The policy removes only the nine requested conformance targets; Nx owns the resulting prerequisite graph.
- Both exceptions require one explicit nonempty `--base`, no explicit or `NX_HEAD` head override, and all of `pack,test-device,test-conformance,rust-tests` in the requested targets. Mixed changes, additions, deletions, renames, unknown metadata, public SDK headers, UI `shim.s`, loader/runtime changes, alternate selectors (`--files`, stdin, uncommitted, untracked or all), unsupported comparisons and incomplete target sets use normal Nx affected selection. Named conformance targets and their full cache keys are unchanged.
- Loader and public SDK changes have no automatic fixture-first exception: retain shipped conformance and every native-selected consumer device behavior check. The loader controls relocation/BSS, menu/function addresses, registry initialization and callback publication. The machine/FX fixture shares shipped load assertions (upload, persistence, registration, version/contracts, DSP status and faults), and a separate zero-DSP service fixture covers service behavior; these fixtures do not prove compatibility of every shipped artifact. Load conformance also does not prove rendered audio or all later module behavior. Use the owner targets above for iteration, then inspect `mise run test:plan` and run the full affected `mise run test` before merge. Any further omission needs an assertion-level mapping of omitted unique behavior and replacement coverage; see the [runtime/SDK coverage decision](plans/module-sdk-iteration.md#decision-runtime-loader-and-public-sdk-coverage).

### Remote cache

All worktrees of a clone share one Cacheiro server on `127.0.0.1:47311`. Its state is in
`<git common dir>/nx-remote-cache` (config with a read/write and a read-only token, mode 0600).
`mise run test` and `test:full` write; `test:changed` reads. `tools/nx/remote-cache.mjs` starts the
server on demand (`status` and `stop` subcommands) and sets the Nx variables only when it answers.

Cached targets: module `test-device` and `test-conformance`. Inputs: the module and its graph deps,
the outputs of the pack, CLI and daemon targets, the toolchain, the emulator env switches, the harness
packages (`test-support`, `control-client`, `dk-session`), every `module.toml`,
`tools/emulator/fast_regions.json`, and the firmware, ready dirs and DT2 sample card (hashed by
`tools/nx/device-inputs.mjs`). That digest also hashes path, contents and file permissions of
`DIGI_EMUD`, device-specific/proof daemon overrides, `DIGI_PATCHKIT`, `DIGI_PATCHKIT_BIN` and
`DIGI_PLUSDRIVE` (following binary symlinks). Nonempty `PATCHKIT_MODULES` or `DIGI_PK_ROOT`, or any
explicit `DIGI_RUNTIME_MODULES` (including empty), deliberately prevent cache reuse with a fresh runtime nonce: arbitrary
sources/includes, variant toolchains and mutable object caches have no proven bounded input closure.
Overrides remain supported; these rare runs can write entries that will never be reused in local/remote
caches. Default runs retain a deterministic digest and do not scan override trees.
Specialized catalogue/files/LP0 card, patched-image, plan and boot-fixture overrides also prevent
reuse, because provenance can name further external files. LP0 case-selection flags are in the key.
Ignored/external edits alone do not trigger Git affected selection: invoke the owning Nx target
directly after changing an override binary/root or ignored firmware/ready/card input.
Not in the key on purpose: `dist/session-cache` (boot cache, cannot
change a result) and directory scans such as `modules/README.md`. Failed tasks are not cached.

### Digikit boot scenarios

The runtime loads its module set at boot. A TOML file in `tests/digikit/scenarios/` or `modules/*/*/tests/scenarios/` names the device,
the module sets and the expectations. `uv run python -m tools.patchkit.pk_digikit_scen list|run [NAME ...] [--image IMG] [--tier quick|full]`
boots the patched firmware through `pk_dkrt_boot` and checks. A scenario file has `[scenario] name, kind, devices, tier`
and a table named like the kind. Kinds:

| Kind | Checks |
| --- | --- |
| `equiv` | Machine, parameter, page and voice rows of a boot with set S equal those of the image patchkit builds with S's static twin; PCM of a note equal, or pinned by `pcm_sha256` (Em approves a new value). |
| `upload` | Module upload over USB SysEx: only `rust` is Python. The other parts are in `modules/core/runtime/tests/upload.device.test.ts` and `upload-faults.device.test.ts`. |
| `menu` | Ticking a set in SETTINGS > DIGIKIT > MODULES and restarting loads the set; PCM equals the patched-in set. |
| `pages` | Parameter pages of a machine on track 1: the run-time module's frames equal the patched-in twin's. |

Every scenario ends with the write-watch over `cf.expansion` (0 stray writes), the flush check (0 hits)
and the card-sector check. Tiers: `DIGIKIT_TIER=quick` is the default. The default `mise run test` gate does not select full-tier tests. `mise run test:full` selects the separate `core/runtime-inferred-needs-tests:test-device-full` target and sets `DIGIKIT_TIER=full`; `mise run module:check` sets it for a module's own scenarios.
Both use the newest image of each device. Cards are cached by content in `out/cache/digikit/cards/`.
Jobs: `DIGI_JOBS`. `DIGIKIT_PROFILE=1` prints the phase table,
`DIGIKIT_RUN_SALT=x` forces cold boots.

The memory audit proves that a loaded module set writes no ColdFire memory outside what each module
owns while the UI is driven. The clean sets and the negative control (`test/dyn-demo-stray` stores one
word in the arena) are `modules/core/runtime/tests/memaudit.device.test.ts`; `mise run module:audit NAME`
runs it for one module as `memaudit-module.device.test.ts` (docs/MODULE-AUTHORING.md). The shared code
is `tests/regression/runtime/memaudit-kit.ts` and `memaudit-suite.ts`. The rig is ColdFire-only. The
first daemon protects the `cpu = "cf"` regions of `devices/ports/<image>.toml` minus each module's
block, allows writes only from placed patch code, drives the UI and expects `guard.report` total 0. A
second daemon boots the same card with the write watch on the same ranges, so the load and `dkm_init`
are audited too. DMA and host writes are not seen. The full-tier variant loads the full synthetic set
of the capacity tests behind `test/dyn-demo` (`fullSet` in `synth.ts`) and also expects no free arena byte and
no whole free DSP granule.

The quick TypeScript capacity tests in `modules/core/runtime/tests/capacity/capacity.device.test.ts` fill runtime limits with
synthetic modules (`patchkit synth-dkm`, built by `tests/regression/runtime/synth.ts`; limits in `[limits]` of `devices/*.toml`) and check refusal codes and
the reference module's DSP bytes. The separate full-tier `modules/core/runtime/tests/capacity/inferred/inferred-needs.device.test.ts` checks needs inferred
by `patchkit needs` against real modules and fixtures. `modules/core/apps/digikit-menu/tests/fits.device.test.ts` checks the MODULES page.
`tests/patchkit/test_limits.py` keeps tools from repeating a limit as a number.

### Machine and FX conformance (`tests/modules`)

`mise run module:check NAME [--device dt2|dn2]` (NAME: `<author/name>` or the name alone) selects the suite by
module type and runs the module's own Python tests. A module with `tests/**/*.device.test.ts` files gets
Nx `test-device` targets (inferred by `tools/nx/modules.mjs`, read from the Nx graph); `module:check` runs all owners serially after Python success, propagates a
failure, and reports size/fit only after both stages pass. With no Python tests, the existing source
check still precedes the owned targets. Frequency shifter owns the exact existing TypeScript shifter
suite on both devices, including when `--device` limits other checks. Its daemons and ready inputs
use the existing harness resolution/overrides; only resolution holds a shared benchlock, released
before the target. The target does not run the broad gate. Its bounded source inputs are conservative affected-selection inputs, not a cache-closure or
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
`test/dyn-dsp-hang` (machine), `test/fx-nan` (effect). `DIGI_JOBS` sets the worker count.
A change in a machine or effect module, `core/dsp-voice`, `core/fx`, `pk_conform.py` or the rigs selects the suite.

### DSP budget gate (patchkit smoke)

Every `pk_harness run` cell ends with a DSP measurement: TRIG 1 held for 1 s of audio, the mean busy
instructions per block from `status.audio.dsp_tiers`, minus the stock baseline of the same device and
OS (cached in `out/runs/patchkit/dsp/`). Projected load = busy x factor / 666,667. The factors are in
`devices/cycle-factors.json`. A cell fails when the projected high load exceeds `--dsp-budget`
(default 85 %); `--no-dsp-budget` skips it. Results go to `out/runs/patchkit/dsp-budget.json`. Unit
test: `tests/patchkit/test_pk_dsp_budget.py`.

### AOT versus interpreter equality

The no-AOT halves of `tier-quick` and `tier-sweep` use `NO_AOT_SUBSET` in `tools/devtools/fx_tier.py`:
17 of the 134 DN2 FX scenarios and 8 of the 60 DT2 ones. `fx-no-aot` runs 17 of the 61 oracle tests
(memberships in `tests/regression/scenarios/dn2-trackfx.test.ts` and `dn2-sendfx.test.ts`).

## CPU budget of the gate

`pnpm nx:pilot` runs `tools/nx/nx.mjs`, the one Nx launcher. It computes a budget from the machine:
B = max(2, cores - 2), or `DIGI_BUDGET=N`. A value already set in the environment wins over the derived one.

| Setting | Formula | 4 cores | 8 cores | 12 cores | 16 cores |
| --- | --- | --- | --- | --- | --- |
| Nx tasks | clamp(floor(B/4), 1, 3) | 1 | 1 | 2 | 3 |
| `REGRESSION_JOBS` (Vitest workers per task) | clamp(floor(B/(tasks*2)), 1, 2) | 1 | 2 | 2 | 2 |
| `DIGI_JOBS` (emulator processes) | clamp(floor(B/2), 1, 6) | 1 | 3 | 5 | 6 |
| `CARGO_BUILD_JOBS` | B | 2 | 6 | 10 | 14 |
| `NEXTEST_TEST_THREADS` | ceil(B/tasks) | 2 | 6 | 5 | 5 |

Formatting an emulator crate or `dkneeds` changes the daemon key and the PGO key, so the PGO profile needs a retrain.

- For verbs that run tasks the launcher runs at nice 10 and takes the shared benchlock, except when all
  targets are light (`lint`, `fmt-check`, `rust-fmt`, `typecheck`, `test-host`, `check`, `check:ui`,
  `build:ui`, `python-checks`): those take no lock. `show`, `graph`, `--graph` and `--explain` take no lock. Nx has no per-kind limit: a target with `parallelism: false`
  (cargo build targets `cli`, `daemons`, `digi-coupled:build`, `build-dt2`) runs alone.
- `DIGI_JOBS` is the only job variable; the `fx`, `fx-no-aot` and `fx-dt2` targets set it to 1.
  `DIGIKIT_PART_JOBS` stays. One coupled emulator process keeps 2 cores busy.
- A no-progress watchdog (`tests/regression/watchdog.ts`) is on by default. It closes a
  daemon whose DSP frames, main loop hits and audio frames stay the same for 120 s of wall time
  (`DIGI_NO_PROGRESS_MS`, in ms; 0 turns it off), so the test fails at once; a paused daemon without a
  job is idle and not counted. The runtime and conformance wall timeouts are 30 min backstops.
- `pnpm nx:pilot run digi-control:daemons` builds the daemons and writes `dist/daemons.json`; helpers
  read it when `DIGI_EMUD_*` is unset. The builds use the daemon store (`<git common dir>/emud-cache`,
  `tools/devtools/verify_build.py`).

Test builds of `digi-emud` and `coupled-run` use the current PGO profile through `native_pgo.profile_for_tests`.
A missing or stale profile prints one warning: a stale profile is still applied to `digi-emud`, and `coupled-run` builds plain.
`DIGI_REQUIRE_PGO=1` makes this an error; the gate does not set it, because every core change makes the profile stale until a retrain.
`DIGI_EMU_PGO=0` turns PGO off. Measuring tools (`coupled_ab`, `sharc_matrix`, `perf`, `fastcov`, `fast_roots`) keep their own builds.

`tools/devtools/sharc_cells.py` compares host instruction counts of two built `coupled-run` binaries on runtime fixtures (`uv run python -m tools.devtools.sharc_cells A=BIN B=BIN --state dn2-reset --jobs 2 --cov`). It runs each binary at two lengths, prints steady-state counters per audio second, the B/A instruction ratio and PCM and DSP state parity (non-zero exit on a difference), and with `--cov` the fast-tier root and exit tables. It does not take the bench lock, because counters are not timing; use `coupled_ab` for the final timing A/B.

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
  difference.

Set `DIGI_RENDER_CACHE=0` for release and publication gates.

## Nx cache

Patched-image device and conformance targets first run `build-objects` for the core modules in the
supported profiles' image closure. The plugin derives the `acceptance-image` tag from those profiles;
it is not a manually maintained module list. Stock-only targets and runtime host units have no such prerequisite.
The existing regression device target mixes stock and patched-runtime cases, so its prerequisites prepare the image too.

Each producer checks generated inputs with `patchkit gen --modules ID --check`, then runs the existing
incremental `patchkit build MODULE_ROOT` under shared benchlock. If generation is stale, run
`mise run gen:modules -- --modules ID`, inspect the generated diff, and retry. The module filter scopes
owner-generated files; SDK and firmware-interface generation is shared and is also checked or written.
Core objects are tracked `build/*.dko.json` outputs: a stale build may update them, and their diff needs review.
These producers are uncached; patchkit's source hashes skip current objects. Conservative default inputs
remain in place, so existing graph edges can overselect preparation; narrowing awaits clearer test ownership.
Repeated per-owner generation checks currently repeat shared SDK/interface checks. This is a future optimization.

Catalog selection has a scoped proof: hypothetical edits to a shipped manifest and a fixture manifest both
select `patchkit:catalog-check`; an edit to Pluck's C source does not. This verifies selection behavior only.

`pnpm nx:pilot` keeps the task cache and the workspace data in `.nx/` of each worktree. The local cache is
per worktree; the "Remote cache" section covers sharing across worktrees. A hit
replays the stored output of a target with `cache: true`; the hash covers only the declared `inputs`.
Plugin sources (`tools/nx/*.mjs`) are not inputs: Nx hashes each project's resolved configuration, so a plugin
edit invalidates only the targets whose definition changes. `UPDATE_GOLDENS` is a cache input of module device
targets and `gate-stock:stock`.

Inspect declared inputs with `pnpm nx:pilot show target inputs PROJECT:TARGET --json`, or use
`pnpm nx:pilot show target inputs PROJECT:TARGET --check PATH` for one file. Native Nx resolves
nested ownership, dependency inputs and prerequisite outputs; inspection executes no target.
This describes the declared hash plan, not every file a command or subprocess actually reads.

Before enabling or changing a cache, review source/dependency reads, outputs, toolchain and
environment identities, including setup commands and runtime digests. Run scoped behavior checks
appropriate to the change: invalidation for relevant inputs and restoration of declared outputs.
The remote `test-conformance` cache proof measured 20.9s on its first run (two cells), 1.8s on an
identical repeat (1/22 cache hit, the test target), and 14.5s after changing declared
`DIGI_NO_PROGRESS_MS=180000`. The module source input and sibling exclusion were inspected against
affected plans. The cached test target declares no outputs; pack and CLI producers remain
`cache: false`, so `dist/` output restoration is not yet applicable or proven.
Verify affected selection separately: a runtime hash does not select a task after an ignored input
changes. The former custom auditor/tracer was removed; its historical passes did not prove
subprocess closure or runtime-digest provenance. Unresolved coverage is not a cache-safety proof.

Cached targets: module `test-device` and `test-conformance`, `gate-stock:stock`, `typecheck` (set in
`nx.json` `targetDefaults`; the `typecheck` of `regression` is not cached because its tsconfig reads every
module's device tests), `@digi/web:check`, `@digi/desktop:build:ui` and `check:ui`, `workspace:lint`, the
host tests, and `rust-tests` of the crates in `HERMETIC` in `tools/nx/cargo.mjs`
(coldfire, periph, emmc-card, dkneeds, dkrt-sdk, dkrt-fit, device-profile, dkupload, dksample, machine,
sharc-rt, dkmidi). Those run
`rustup run <mise rust> cargo nextest run -p NAME --release --locked --offline --target-dir dist/cargo-tests`.
Their runtime key includes the sorted present `CARGO_PROFILE_RELEASE_*` names and values, including
build-override options; changing these environment settings requires a direct target invocation when
there is no tracked change selecting the crate.
Other crates run uncached through `node tools/nx/rust-env.mjs --`; only `sharc-native` and the desktop crate
add `--gen`, because only they need the generated core. Crates without tests have no `rust-tests`
target. `workspace:fmt-check` is not cached: prettier also opens files it never formats (`.py`, `.rs`),
and nested `.gitignore` and `.editorconfig` files change its result.

Compiler depfiles provide separate evidence. For the `HERMETIC` `rust-tests`, run
`node tools/nx/cargo-depfiles.mjs` after the targets ran. It checks every repository file in cargo's
dep-info files under `dist/cargo-tests` against the crate, its path dependencies and its extra inputs.
Dep-info does not show run-time reads (`std::fs`), so a crate is `HERMETIC` only if its tests read no
firmware, generated core or `out/` data.

### Daemon store

`verify_build daemons` (the `digi-control:daemons` target) keeps the coupled `digi-emud` daemons in
`<git common dir>/emud-cache/<dev>/<key>/` (`digi-emud`, `key.txt`, `depfile.d`). All worktrees of the
clone share it and it is never committed. A worktree with the same key restores the daemon and builds
nothing; a stored file is never rewritten. The last 4 used keys per device stay.

The key is printed by `uv run python -m tools.devtools.emud_key key --device DEV`: a sha256 over the
content of the repository files in `SOURCE_GLOBS` and `RECIPE_FILES`, every file of the generated core
directory (resolved as `pk_emud_pgo` does), the PGO profile, the `tools.rust` pin from `mise.toml`,
`rustc -vV` of that toolchain, and the build flags. Task definitions in `mise-tasks.toml` are excluded.
Narrowing this input changes the key once, so existing stored daemons miss on the next request.
A device without its stock firmware or ready directory is skipped.

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

- fx-reuse: the onset shift varies by +-64 samples between identical runs (free-running coupled
  emulator); the tolerance is 128.
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
