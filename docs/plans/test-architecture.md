# Test architecture: tests follow layers (plan)

Roadmap item T1 in [sdk-runtime-roadmap.md](sdk-runtime-roadmap.md). Related: [test-speed.md](test-speed.md) (items 7, 16 to 18) and [TESTING.md](../TESTING.md).

Layers and fakes now exist (runtime layers in [ARCHITECTURE.md](../ARCHITECTURE.md); `FakeCore` and `FakeService` from R2c; `FakePlatform` from R3). Test selection can follow them.

## Goal

- A change runs only the tests that its code can affect.
- Emulator runs are reserved for what needs them.
- Each test runs at the cheapest level that answers its question. Tests that do not listen to audio do not run the DSP.
- Confidence stays at least as high as today. `test:full` per batch is the safety net.
- Tests and non-RE tools are TypeScript (Vitest) or Rust. Python stays for reverse engineering (`tools/sharc_dev`, `tools/coldfire`, Ghidra, firmware analysis).

## Principles

- (a) mise tasks stay the entry points. Nx does selection and caching.
- (b) Every Nx project has build targets and test targets. Build targets are cached and declare their outputs. Test targets depend on the build and key on its outputs, not on its sources (early cutoff). Test-speed item 16 did this for device tests. PR 461 started the general split with module-build, device-tests and device-test-keys.
- (c) Layer-scoped host tests run against fakes: `FakeCore` and `FakeService` (R2c), `FakePlatform` (R3).
- (d) One contract suite per interface. It runs against the fake and against the real implementation. The real side runs in the emulator once per device.
- (e) Device smoke per service: boot, load, play, edit.
- (f) Module DSP tests run on a SHARC-only rig without a ColdFire boot (`tests/support/machine_rig.py` exists).
- (g) `test:full` once per batch catches interaction effects.
- (h) Migrate by replacement, not by bulk port: a Python test is deleted when its TypeScript replacement checks the same oracle. Little change for the sake of tests: do the work where module or plan work needs it.
- (i) The Python SHARC core stays the reference for instruction semantics and generates the Rust core. PCM pins (`tests/scenarios/*.json`, module pins) change only with Em's approval.

## Levels

| Level | What | Runs on | When it runs |
| --- | --- | --- | --- |
| Host unit tests per layer | Logic of one layer (packing, manifests, routing, tables) against fakes | Rust and TS, no emulator | Every change that reaches the layer |
| Contract tests per interface | Same suite on the fake and the real implementation | Fake: host. Real: emulator, once per device | Fake: with the layer. Real: when the interface or its implementation changes |
| Device smoke per service | Boot, load, play, edit for one service. Checks the firmware handles it (menus, parameters, save and load, upload, memory audit) | ColdFire emulator; DSP only when the test needs audio | When the service or a layer below it changes |
| Module DSP tests | Is the machine or FX right, and does it sound right? Pitch, decay, RMS, DC, parameter response, pinned PCM hash | The module routine alone, through its contract, on the native SHARC engine | When the module or its DSP code changes |
| Coupled device tests | Does it sound right inside the device (stock filter, envelope, FX, master, hook glue)? One per module type per device | Both CPUs | When the module, the hook glue or a layer below changes |
| `test:full` | Everything | All of the above | Once per batch of merges |

Everything moves down a level where it can.

## Work packages

### T1a Baseline

- Scope: static, right after R2c merges. For each change class, record what `test:plan` selects and the gate wall time. Classes: workbench TS edit, web TS edit, services/fx edit, core edit, SDK header edit, module C edit, module DSP asm edit, patchkit refactor with no byte change, Python tool edit, docs-only.
- Find inputs that over-select.
- Output: a table in this plan, plus target numbers per class.
- Depends on: R2c.
- Status: done (2026-11-14, main 15637f55).

**Method.** One real file per class. `pnpm nx:pilot affected --files=<file> -t <MERGE_TARGETS of tools/nx/gate.mjs> --explain --verbose` (it runs no tests). "Device tasks" count the per-file, per-device leaf targets (`test-device--<file>--dn2|dt2`) that `test-device` fans out to, plus `test-conformance`. Gate roots (`smoke`, `card`, `fast-roots`, `pins`, `fx`, `fx-no-aot`, `fx-dt2`, `tier-quick`, `dt2-tier-quick`, `dt2-tone`, `stock`) are listed by name. Time: the lower quartile of each selected task's recorded duration in the 265 gate traces `out/runs/gate/*/nx-profile.json` (durations include the wait for an emulator slot, so they overstate the run), summed as task-min. Wall = task-min times 0.043 (the median of wall/task-min over the eight largest recorded gates: 360 to 412 tasks, 1500 to 2300 task-min, 47 to 100 min), and at least the longest selected root (smoke 13 min, card 15, fx-no-aot 17, fx-dt2 17 median). Accuracy about plus or minus 50%. The estimate assumes no cache replays: `affected` selects by declared inputs, and a narrow device key (`metadata.mergeTests`, test-speed item 5) can still replay a task.

| # | Class | Example file | Selected (gate targets) | Device tasks | Est. wall | Over-selection (input responsible) | Target |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | workbench TS | `packages/workbench-core/src/index.ts` | 11 tasks, TS host only: lint, fmt-check, typecheck x2, test-host x3, check (web), check:ui, build:ui (desktop), `@digi/web:test-wasm` | 0 | about 4 min (test-wasm 3.6 min) | `@digi/web:test-wasm` reloads the wasm for a workbench-core edit: `packages/workbench-core/**/*` is an input of the web wasm project | TS targets only; test-wasm only when `apps/web/src/**` or wasm inputs change |
| 2 | desktop-only TS | `apps/desktop/ui/src/CardPanel.tsx` | 6 tasks: lint, fmt-check, typecheck, test-host, check:ui, build:ui | 0 | about 1 min | none | same |
| 3 | web-only TS | `apps/web/src/components/CoupledAudio.tsx` | 5 tasks: lint, fmt-check, check, test-host, test-wasm | 0 | about 4 min | none (the wasm tests import the web sources) | same |
| 4 | core runtime Rust | `modules/core/runtime/src/rs/src/collect.rs` | 94 tasks: runtime and gate-rust-modules rust-tests, rust-fmt, 4 pack, 15 build-objects, objects-check, python-checks and python-tests, smoke, fast-roots | 44 (22 files x dn2/dt2: core/runtime-tests and the other runtime test projects) | about 13 min (199 task-min; smoke bound) | python-checks and python-tests (`modules/**/*`) cannot see a Rust runtime change | runtime change: representative set from T1e (one cell and one quick scenario per module type per device); about 10 device tasks |
| 5 | service file | `modules/core/runtime/src/rs/src/services/fx/fxsend.rs` | identical to class 4 (94 tasks) | 44 | about 13 min | the whole `modules/core/runtime/**/*` input of every runtime-acceptance project (core-change policy, test-speed item 5): a service edit selects the tests of every service | that service's tagged device tests plus a smoke set (T1e): `fx-*` files only, about 8 device tasks |
| 6 | SDK header | `sdk/include/dk/core/mem.h` | 91 tasks: 48 pack, 3 pack-aliases, 23 build-objects, objects-check, smoke, rust-tests, test-host, python-checks and python-tests | 4 (`fx-send` of core/fx, digikit/remote) | about 14 min (38 task-min; smoke bound) | `sdk/include/**/*` selects every pack and object build, whether or not the module includes the header | rebuild the modules that include the header (compiler depfiles); device tasks of those modules only. After 2026-11-15: 19 tasks, 2 packs (include scan, test-speed 24) |
| 7 | module C source | `modules/machines/pluck/src/module.c` | 8 tasks: pack, test-device, gate-rust-modules rust-tests, python-checks, python-tests, fmt-check | 2 | about 5 min | rust-tests (`modules/**/src/**/*`), python-checks and python-tests (`modules/**/*`) | pack plus that module's device tests |
| 8 | module DSP asm | `modules/machines/pluck/src/dsp/pluck.s` | 7 tasks: as class 7 without fmt-check | 2 | about 5 min | same as class 7 | same |
| 9 | patchkit Rust refactor | `packages/patchkit/src/apply.rs` | 38 selected of 123 with dependencies: rust-tests x5 (patchkit, control, coupled, desktop, native-boot), rust-fmt, desktop typecheck, check:ui, build:ui, web check and test-wasm, regression test-host and typecheck, catalog-check, 12 build-objects, card, smoke, fast-roots | 0 (key on built bytes works) | about 15 min (43 task-min; card and smoke bound) | desktop TS targets and regression typecheck read `packages/patchkit/**/*` (a Rust crate, no TS); card, smoke and fast-roots read the daemon outputs, which rebuild | a refactor with unchanged bytes: patchkit rust-tests plus byte-compare; card, smoke and fast-roots replay. After 2026-11-15: the four TS tasks are gone (49 to 45 gate tasks); card, smoke and fast-roots stay (the daemon runs patchkit, test-speed 25) |
| 10 | Python tool | `tools/devtools/benchlock.py` | 67 tasks: python-checks, python-tests, card, dt2-tone, fast-roots, fx, fx-no-aot, fx-dt2, tier-quick, dt2-tier-quick, pins, smoke | 34 (17 files x 2: core/dynfx, fx-chain, fx-reuse, fx-send, fx-routes, fx-track-route, transfer-live, upload-faults) | about 20 min (312 task-min). After 2026-11-15: 2 tasks | `benchlock.py` is a per-file input of every device and gate-root task (the Python import closure, `pythonImportInputs` in `tools/nx/modules.mjs`); it only takes a lock and changes no result | python-checks and python-tests only; 0 device tasks |
| 11 | docs only | `docs/FINDINGS.md` | nothing affected | 0 | 0 | none | 0 tasks (already so) |
| 12 | device test file | `modules/machines/pluck/tests/equiv.device.test.ts` | 7 tasks: lint, fmt-check, test-device, python-checks, python-tests | 2 | about 5 min | python-checks and python-tests (`modules/**/*`) | lint, fmt-check and that file's two device tasks |
| 13 | `packages/dk-session` | `packages/dk-session/src/bulk.ts` | 280 tasks: 4 test-host, 4 typecheck, check, check:ui, build:ui, test-wasm, stock, 12 build-objects | 152 test-device leaves (35 projects) and 12 test-conformance | about 35 min (801 task-min) | the file is a per-file import input of every device test (the `importInputs` of `modules.mjs`, reached through the shared session code): a transport edit reselects the whole suite | dk-session host tests, typecheck of dependents and the transfer and upload device tests; the rest from byte keys. After 2026-11-15: 62 tasks, 35 device tasks (barrels are followed per imported name; `build-objects` no longer reads `^production`) |
| 14 | dkprof | `packages/dkprof/src/lookup.rs` | 2 tasks: dkprof rust-tests, rust-fmt | 0 | under 1 min | none (a single Rust crate) | same |
| 15 | emulator control crate | `packages/emulator/control/src/bin/digi_emud.rs` | 25 tasks: digi-control and desktop rust-tests, rust-fmt, desktop typecheck, check:ui, build:ui, regression test-host and typecheck, python-checks and python-tests, card, smoke | 0 (the daemon is content-keyed) | about 15 min (23 task-min; card and smoke bound) | desktop TS targets and regression typecheck hash `packages/emulator/control/**/*` (a Rust crate) | rust-tests of the crate and its dependents; smoke replays when the daemon bytes are unchanged. After 2026-11-15: the four TS tasks are gone (13 to 9 gate tasks) |

Reading the table:

- Cheap classes already match the target: 2, 3, 11, 14. Docs-only selects nothing.
- The expensive classes are 13 (800 task-min), 10 (310) and 4/5 (200). Classes 10 and 5 are the clearest waste: a lock script and a single service reselect the device suite.
- Classes 4, 5, 7, 8 and 12 also pay for python-checks and python-tests (about 1 to 2 min each) because their inputs name `modules/**/*`.
- Class 15 and class 9 reach card and smoke only through the daemon output, which rebuilds even for a change that leaves the binary unchanged.
- Fixes are open items in [test-speed.md](test-speed.md) (items 20 to 25).

### T1b Uniform build/test split in Nx

- Scope: all projects get build and test targets as in principle (b). Continues PR 461 and test-speed item 16.
- Proof: an `nx graph --file` dump diff, plus `test:plan` for the T1a classes.
- Depends on: nothing.

### T1c Host contract tests

- Scope: contract suites for `CoreApi` and `Service` now, and `Platform` with R3. Fake and real pass the same suite.
- First cases: load SDK-built FX and machine fixtures through the runtime's descriptor parsers on the host and check sizes and offsets against the generated SDK constants; one Storage contract suite run against FakeStorage on the host and the real adapter once on the device.
- Proof: both implementations run the suite.
- Depends on: R2c (`CoreApi`, `Service`), R3 (`Platform`).

### T1d Device-test audit

- Scope: test-speed item 7. Move logic checks to host tests. Tag each device test by the layer or service it exercises.
- Move checks that need only the ColdFire (menu, registry, upload, round trip, safe mode, cut) to a ColdFire-only rig without the DSP. Measure `--dsp zero` first: its cost, and whether the ColdFire side needs DSP replies. Only PCM comparisons stay coupled.
- Proof: deletions go through an independent review (rule: review test deletions).
- Depends on: T1c.

### T1e Service-scoped selection

- Scope: map changed functions (patchkit `objdiff`) or changed files to a layer or service. Run that service's tagged device tests plus a fixed smoke set. Smoke cells and quick scenarios are filtered to the cells that contain a changed module. A `core/*` or runtime change runs a representative set (one cell and one quick scenario per module type per device).
- Proof: measure against T1a before it becomes the default.
- Depends on: T1c, T1d.

### T1f SDK-surface fixtures and coverage

- Scope: fixtures cover every `sdk.toml` import and builder call. Measure it: imports linked per fixture against `sdk.toml`, and ColdFire PC coverage with `cf.profile`.
- A conforming module then needs the static checks (packer, `module:check`, needs and fit) plus one load-and-smoke test. Module-specific emulator tests remain for DSP behaviour.
- Proof: the coverage numbers.
- Depends on: nothing (own lane).

### T1g SHARC-only rig

- Scope: module DSP and PCM tests run on a SHARC-only rig. The rig is a native call harness for module routines: load the module objects, set registers, call `note_on` and `render` for N blocks, return output, cycle counts and recorded stores. Its request format is language neutral (Python now, TypeScript later). A Vitest kit over it checks machines (`render`, `note_on`, `desc`) and FX (`dk_fx_process`) against sound checks that a module declares in its `tests/*.toml`. The Python conformance suite and pluck sweeps are retired when the kit checks the same pins.
- Proof: the same PCM results as the full-device tests, in less time.
- Depends on: nothing (own lane).

### T1h Workbench and web composability

- Scope: TypeScript only. Clearer boundaries, mocked integration points.
- Proof: `test:plan` shows that a TS-only change selects only Vitest targets.
- Depends on: nothing (own lane, any time).
- [x] Step 1, fix test selection for TypeScript changes: `patchkit:wasm` keyed on the Rust crates only, `@digi/web:test-host` without wasm (real-wasm tests moved to `test-wasm`: `*.wasm.test.ts`; the node tests followed in step 5), desktop `test-host`, `rust-tests` and web `check` with the inputs they read. Result: an edit in `apps/web/src/transfer` selected 13 tasks (patchkit wasm build, desktop `rust-tests`, web `test`) and now 11, with no wasm build and no cargo; an edit in `apps/desktop/ui/src` selected 9 (cargo `rust-tests`, web `check`) and now 7, desktop TS targets only.
- [x] Step 2, Host and EmuRpc with FakeHost: typed `Host` (`packages/workbench-core/src/host/host.ts`), `TauriHost` as the only `invoke` importer (oxlint rule), `EmuRpc` replacing both `emuCall` wrappers, a recording `FakeHost`; bulk-link, midi-transport, samples-backend, CardPanel, LogPanel, Console, desktop-env and desktop-firmware take the host. New Vitest files: samples-backend, card-api, log-save, links. [DEVELOPMENT.md](../DEVELOPMENT.md) "Host and EmuRpc".
- [x] Step 3, no singletons: `createTransferApp(env, { storage, sessions })` and `createFirmwareApp` replace the module-level `target`, `transfer-state`, `connection-state`, `jobs`, `default-modules-state`, `firmware-state` and `prefs`; `KeyValue` and `BlobStore` with in-memory fakes (the ready-state cache and the hand-off take a `BlobStore`); `SessionHub` is built and passed in; `setTransferEnv` and `setFirmwareEnv` are gone. Desktop and site each build their apps at startup and pass them down through `<AppProvider>`. [DEVELOPMENT.md](../DEVELOPMENT.md) "The app object, KeyValue and BlobStore".
- [x] Step 4, `WorkerFactory` and `WasmSource`: `browserWorkers()` and `createFakeWorkers()` replace the five `new Worker` calls (runtime, split session, patch worker); `WasmSource` (`fetchWasmSource`, `hostWasmSource`, `memoryWasmSource`) and the `PatchKitLike` and `NeedsLike` stubs replace the loading of `patchkit.wasm` and the core; `nativeRuntime` moved to the desktop as `createNativeRuntime(host)` over `Host.emu` (ten more typed commands, no dynamic `@tauri-apps/api/core` import), `browserRuntime(onUpdate, { workers, wasm })`. New Vitest files: browser-runtime, web-firmware-env, native-runtime. [DEVELOPMENT.md](../DEVELOPMENT.md) "WorkerFactory, WasmSource and the two runtimes".
- [x] Step 5, the `node --test` files ported to Vitest: one runner in `apps/web`. The fake devices (`FakeDevice`, `FakeDrive`, `FakeUpgrade`) and the shared helpers are TypeScript in `apps/web/tests/support/`; tests that need no wasm and no command line run in `test-host` (`audio-health`, `icons`, `ready-cache`, `gallery`, `rpcfiles`, `samples`, `dktransfer` with a fit stand-in); the others are `*.wasm.test.ts` in `test-wasm` (`dkneeds`, `fit`, `firmware-kit`, `dksample`, `dkupload`, `patchkit`, `gallery`, `dktransfer`, 120 s per test); the Samples-mode logic test moved to `apps/desktop/ui/tests/samples-core.test.ts`. `node --test` is gone from the `test-wasm` script.
- [x] Step 6, `packages/workbench-core` and the NodeHost: the code that the site and the desktop app share (`transfer/`, `firmware/`, `jobs/`, `modules/`, `patch/`, `runtime/`, `host/`, `storage`, `blob-store`, `workers`, `wasm-source`; Solid pages and styles in `ui/`) moved out of `apps/web/src`; both apps import `@digi/workbench-core`, the `@web` alias is gone, and oxlint fails an import between the apps (or from workbench-core into an app). `createNodeHost` (`src/node/node-host.ts`) is the `Host` for command lines; `wb ports` and the samples backend choice in `wb` run through it, with the same output. An edit in workbench-core selects 11 tasks: its own `test-host` and `typecheck`, the web `check`, `test-host`, `test-wasm` and `build`, the desktop `test-host`, `typecheck`, `check:ui` and `build:ui`, lint and format; no cargo, wasm build or device target. [DEVELOPMENT.md](../DEVELOPMENT.md) "workbench-core, the two apps and the command line".

## Order

- T1a after R2c merges.
- T1b any time.
- T1c alongside R3.
- T1d after T1c.
- T1e after T1c and T1d.
- T1f, T1g and T1h in parallel lanes.

## Rules

- No kept tests for tooling.
- No new Python test infrastructure outside the RE tooling.
- Deletions are reviewed.
- The per-batch `test:full` stays.
- Two test systems run side by side only per level, and only until that level's replacement passes: one Python file deleted per TypeScript file that replaces it.
- Each package reports before and after (files, task-seconds, gate wall time) against T1a.
- Design is agreed with Em in chat before code.

## FX consolidation design (2026-11-01)

Status: Em selected design work and proposed reusing a daemon through the control socket with
setup/teardown fixtures. The first lifecycle/PCM pilot is now implemented under subsequent approval;
its earlier fixture proofs passed. The safety increment below records the current PR #242 state. This design supersedes the historical one-PR-per-item rule: keep the reusable fixture
and compatible FX coverage in one coherent change, with explicit checkpoints rather than a bulk
seven-family port. No legacy deletion, new RPC or benchmark is authorized by this checkpoint.

### Fixture lifecycle and isolation

Extend the existing Session lifecycle with Vitest `beforeAll`, `beforeEach`, `afterEach` and
`afterAll` hooks; do not introduce a parallel test framework. `beforeAll` owns one daemon per device
per test file/worker, reused by serial cases. Never attach to another agent's or the user's daemon.
A Session baseline-reload method used by `beforeEach` updates its private load result, card and event
state; a raw-client load with stale Session metadata is insufficient. Reload through the existing
control-socket `firmware.load` operation (also used by the workbench): `replace: true`, `force: true`, explicit
pristine card/image/runtime patch set and `start: auto`. This rebuilds firmware state in the same
process; firmware boot still occurs. The stock-only
[Session restart wrapper](../../packages/test-support/src/session.ts) is not a limitation of the
underlying load operation. In implementation, extend the existing
[FirmwareLoadParams type](../../packages/control-client/src/types.ts) with its missing `replace`
field rather than inventing a reset RPC.

Use a fixture-owned pristine card directory, not just a pristine base file:
[controller card loading](../../packages/emulator/control/src/controller.rs) applies a saved sibling
overlay. There must be no inherited `overlay.bin`; keep mirror/card synchronization off. Reuse the
fixture-owned pristine base without copying a huge card per case only when read-only backing and
disabled synchronization are guaranteed. Specify `auto_no` consistently on every load. Recreate DkSession and EmuUsbTransport per case, fence observations
by the new epoch, and clear collected events. Actor-level options survive runtime replacement:
explicitly set CPU exception delivery (normally false), watchdog stall policy and mirror settings;
reapply guards/watchdog configuration rather than assuming load resets them.

A load is rejected while a controller job or patch job is active. Settle or cancel owned work within
a deadline before loading; replacement does not promise to cancel every job. Failed or dirty reset
fails setup, preserves diagnostics and discards the owned daemon; never silently continue as a clean
case. `afterEach` collects the case's guard/fault/card/PCM closing evidence before discarding state,
then stops capture, pauses the runtime and cancels pending protocol work. Input helpers own release
in `finally`; no release-all RPC is assumed. `afterAll` closes the owned daemon.

Within a persistence scenario, use protocol restart to preserve guest writes and RAM according to
that scenario's contract. An independent-case clean load must not replace that restart. The first
batch introduces no generic reset RPC, global daemon pool, result cache or snapshot restoration.
CF-only snapshots do not prove matched DSP state, and the existing state-load operation requires
instruction count zero; neither establishes the desired coupled reset contract.

### Static scope and coverage groups

**[D]** Historical counts before the approved gain-leg deletion: these describe rig entries and explicit restarts, not
actual uncached boots or elapsed time. Quick tier has 14 scenario/device pairs; full tier has 15.
The earlier estimate of 80 boots per device reduced to nine is unsupported and is withdrawn.

| Family / source anchor | Quick DT2 rig entries | Quick DN2 rig entries | Explicit restarts per device |
| --- | ---: | ---: | ---: |
| fx (retired runner, historical line 312) | 4 | 4 | 1 |
| [fxsend](../../tools/patchkit/pk_digikit_fxsend.py) line 192 | 5 | 8 | 0 |
| fxroute (retired runner, historical line 558) | 11 | 11 | 0 |
| fxroutes (retired runner, historical line 637) | 8 | 8 | 2 |
| fxpage (retired runner, historical line 513) | 3 | 3 | 1 |
| fxpersist (retired runner; `REF:tools/patchkit/pk_digikit_fxpersist.py`, historical line 275) | 1 | 1 | 0 |
| fxshift (retired device runner, historical line 499) | 4 | 4 | 0 |
| Total | 36 | 39 | 4 |

Quick totals are therefore 40 DT2 and 43 DN2 static operations. Full tier adds one DT2 persistence
rig entry and two explicit restarts, three additional operations. These counts make no savings
promise. Use three coverage groups for the design: chain plus page baseline; send plus routes;
persistence plus frequency shifter. These are coverage boundaries, not a proven nine-boot schedule.

Start with the reusable fixture, one existing representative gain-FX lifecycle/PCM flow and its
cleanup assertion on DT2 OS 1.17 and DN2 OS 1.12; no large new test matrix is needed for this pilot.
Only then fold compatible fx/fxpage checks into the same coherent PR, using an
assertion ledger that maps each old assertion, device, image/seed state and PCM/PIX oracle to its
replacement. Retain special no-hook/fxpages-off images, capacity/overflow and no-module cases,
deselect/restart checks and each case's closing checks. Do not delete a family until every unique
assertion is mapped and its port passes. Contract-feature coverage is a safety aid, not proof that
these scenario facts are interchangeable.

### Proof checkpoint and unresolved implementation choices

The focused isolation proof dirties a case, clean-loads in the same daemon, and checks expected
registry, card/settings, CF/DSP baseline, input state and a new epoch. Compare that reset baseline
with a fresh-process reference once per device using focused cases. A separate persistence case
must prove that writes survive protocol restart. Keep the closing guard/fault/card/PCM checks;
there is no broad sweep for this experiment. Stop the first slice if useful process reuse or exact
isolation cannot be established. Process-reuse payoff is unmeasured; use the existing bounded
measurement workflow only if a later decision needs it, without making a performance claim now.

Preboot seeding must reuse the existing Rust store/settings codecs. Inspect the
[native card builder](../../packages/emulator/control/src/cards.rs) and available APIs before proposing
a narrow operation; do not duplicate the codec in TypeScript. Postboot upload cannot substitute for
assertions about a preboot-seeded state. Exact seed transport and fixture implementation remain open.

PCM capture is already available through generic client calls to `pcm.tap`; `--no-audio` disables
playback only, not capture. The existing
[FX support](../../tests/regression/scenarios/fx-support.ts) uses a Python renderer/NumPy path.
Choose minimal analyzer reuse for live captures or a small TypeScript comparison only after inspecting
the needed oracles; do not port DSP analysis wholesale or invent protocol APIs. Seed/analyzer choices
and the complete actor-state reset checklist are bounded checks during the first implementation
slice; resolve them before relying on the affected assertion, rather than blocking every fixture step.


### First implementation checkpoint (2026-11-01)

**[D]** Historical first fixture proof, before the safety increment: both device proofs and fast preflight passed. the [gain-FX pilot](../../modules/core/adapters/dynfx/tests/fx-reuse.device.test.ts)
uses [Session baseline reload](../../packages/test-support/src/session.ts) and the existing Vitest
[regression fixture](../../tests/regression/support.ts). No legacy family is deleted or declared equivalent.
One serial scenario per device covers dry → gain → protocol deselection/restart → pristine dry,
plus one sequential fresh-process dry reference while the kept daemon is paused.

Clean reload uses the existing firmware-load replacement path in the same owned process, updates
Session metadata, verifies a newer epoch and creates fresh protocol/USB receive state. Firmware
boot still occurs. The deselection stage instead uses protocol restart, retaining guest writes;
it must not be substituted with pristine reload. Reset/cleanup failures discard only the owned
daemon and retain its diagnostics. Native plusdrive builds the preboot selections from prepared
DKMs in owned overlay-free card directories, with mirroring/synchronization disabled. No TypeScript
store codec, new reset/seed RPC, snapshot restore or daemon pool was added.

At that first checkpoint, the following assertions passed the focused DT2 and DN2 proofs:

| Area | Pilot asserts | Retained legacy facts / limits |
| --- | --- | --- |
| Boot and registry | Normal boot, count/loaded/results/UI; assignment identity; reported module types/contracts, FX slots and free/used rows | No-hook/plain, full-capacity/overflow, no-module and fxpage variants remain |
| Gain chain | Enabled/default parameters, descriptor/state within the module DSP range, adapter fault fields and one-call-per-block state progress | Inferred resource needs, exact relocated DSP bytes and exact row pointers remain |
| Reset isolation | New epoch, old protocol invalidation, restored normalized module/FX capacity/input/settings/DSP semantics and fresh-process comparison | Not a whole-RAM/tick digest or deliberate dirty-settings isolation proof |
| PCM | Dry × 0.5 within 2 ULP, RMS within 0.001 dB and onset shift at most 128 samples; deselected/restored/reference dry bit-identical from onset | Existing PCM/PIX oracles and all other FX family scenarios remain |
| Closing evidence | No runtime fault/DSP halt/nonfinite PCM, audible sine, targeted post-load boot-identity guard and exported card overlay | Launch-time expansion write-watch, flush, card-sector allow policy and DT2 selected-track type/retry proof remain |

Captures pause execution, choose current DSP frame + 300 after panel setup (the legacy restarted-note
strategy), step 44,000 CF instructions beyond the boundary and hold TRIG 1 for 1,500 DSP frames.
Key-up runs in `finally`. Input idle is observed while free before pausing. The existing
[PCM comparator](../../tools/patchkit/hook_ab.py) now exposes onset alignment and JSON metrics; the
legacy scaled-comparison alias uses the same function. No WAV codec or DSP analyzer was duplicated.
Absent pristine settings are null only for the structured `E_SET_NOKEY` status; other errors fail.

Prerequisites are prepared separately, not built by the test. For the default module directory:

```sh
uv run python -m tools.firmware.plusdrive_check --build-only
mkdir -p out/regression/modules/dt2 out/regression/modules/dn2
for device in dt2 dn2; do
  uv run python -m tools.patchkit.patchkit pack test/sine --device "$device" -o "out/regression/modules/$device/sine.dkm"
  uv run python -m tools.patchkit.patchkit pack test/dyn-fx-gain --device "$device" -o "out/regression/modules/$device/dyn-fx-gain.dkm"
done
```

The test uses the existing fixed native plusdrive binary and preflighted daemon/ready inputs.
`DIGI_RUNTIME_MODULES` may name an already prepared module directory. Gate ownership maps both
fixtures to the pilot, and both explain/execution select plusdrive preparation for an FX-only run.

Focused checks passed: 50 host cases plus the one canonical-path assertion correction retry (51
across the runs); 10 PCM synthetic cases; seven fault/settings cases; 41 routing cases plus four
new prerequisite/explain cases. Affected package/regression types passed. Independent foundation
and pilot reviews completed with their reported blockers corrected. Both device proofs passed
on their first runs, each with one selected case and the other device's case skipped:

| Device | Invocation | Case | Local log |
| --- | ---: | ---: | --- |
| DT2 OS 1.17 | 89.66 s | 73.23 s | `/private/tmp/fx-reuse-dt2.log` |
| DN2 OS 1.12 | 62.06 s | 53.506 s | `/private/tmp/fx-reuse-dn2.log` |

These runs include the fresh reference, gain PCM, protocol deselection and clean restore. They are
functional evidence, not a speedup benchmark, complete migration or broad-gate acceptance. Legacy
coverage remains retained. Final `mise run test:changed` passed: 612 Python tests, 301 host passed
and one skipped, plus lint, format, types, path, layers and generated checks. Log:
`/private/tmp/fx-reuse-preflight.log`. Final independent static review found no issues after the
reported blockers were corrected. The gate plan was explained only; the broad gate was not run.

At that checkpoint, exact inferred needs, relocated DSP bytes/row pointers, selected-track type
and launch-time write-watch/flush/card-sector policy were not established as equivalent. No legacy
deletion is authorized by this evidence. Next, close the assertion ledger before parallel family
migrations. The coordinator retains integration, remaining acceptance and publication decisions.


### Safety increment in PR #242 (2026-11-01)

**[D]** Both revised device cases and required final fast preflight passed. This is the
same PR #242, with no legacy `one()` deletion and no merge-acceptance claim. Eleven new Python
synthetic cases, 57 affected mocked host cases, affected types and scoped lint passed.

The existing Python runtime rig and the TypeScript test now share the offline watch-range calculation,
including its DMA-buffer holes and allowed writer-PC range. The patch plan is prepared before spawn,
then validated against the loaded image after pristine baseline load. Existing write-watch/flush
instrumentation is configured in the daemon's environment from startup, with CACR activation and
per-emulator sequence output. After the owned daemon closes, the check requires contiguous watch
logs matched by complete summaries, at least the observed epochs, zero write events and zero flush
hits. Every exported overlay is parsed by the existing Python parser: store writes are limited to
the two selection sectors, and writes below sector 0x50000 are limited to those same sectors.
Missing/truncated summaries or malformed overlays fail rather than becoming missing evidence.

The gain oracle reuses the independent Python DSP parser/relocator and the existing needs wrapper.
It now compares the exact relocated DSP image, exact first 12 chain-row words and MODULES/FX-slot
resource figures against the inferred expectations. The row defaults, enabled/was/disabled/fault
fields are checked again after capture, preserving the post-note assertion rather than setup alone.
No new emulator instrumentation, RPC, runner, store codec or relocation implementation was added.

The earlier fresh-process reference passed on both devices as a one-time fixture-equivalence proof;
it is historical evidence, removed from default acceptance without an optional hidden test. The
permanent case remains initial dry → gain → protocol deselection/restart → pristine dry restore.
Reuse now skips Session's redundant SDK patched-image load; the existing CLI stock startup still
occurs. Clean baseline replacement and guest-write-preserving protocol restart remain distinct.

| Revised proof | Invocation | Case | Watch/overlay result | Local log |
| --- | ---: | ---: | --- | --- |
| DT2 OS 1.17 | 63.62 s | 60.64 s | 8 emulator summaries, zero write/flush hits, 4 valid overlays | `/private/tmp/fx-safety-dt2.log` |
| DN2 OS 1.12 | 50.16 s | 47.441 s | 8 emulator summaries, zero write/flush hits, 4 valid overlays | `/private/tmp/fx-safety-dn2.log` |

Both passed on the first revised run. These durations describe correctness runs, not a paired
benchmark or measured speedup. At this safety checkpoint, the selected-track-type decision was
still pending and legacy `one()` remained. The approved replacement below resolves that boundary;
plain/full/fxpage and other family coverage remain. The coordinator owns
acceptance and publication; the broad merge gate remains unrun.

Required fast preflight passed (`/private/tmp/fx-safety-preflight.log`): 612 audited Python routing
tests and 75 host cases (four test-support, 71 regression), plus lint, format, path, layers, generated
checks and all workspace TypeScript types. Mypy was skipped because no changed files were in its
typed set. The separate 11 new Python synthetic and 57 focused mocked-host passes above remain
iteration evidence, not additional unique cases to add to these totals. This is a verified safety
checkpoint in draft PR #242, before the approved gain-leg replacement below.


### Approved gain-leg replacement (2026-11-01)

Em explicitly accepted the FX-behavior contract: the TypeScript gain/deselect sequence proves behavior
on audible input, without a direct selected-track-type assertion. The removed `one()` leg previously
made DT2 type observations for both its wet note and its post-restart dry note. Those observations
are intentionally not reproduced; the independent legacy dry/full legs retain their DT2 selected-sine
checks. This is a bounded coverage decision, not a claim that PCM identifies the selected machine.

Only the nested Python `one()` leg, its parallel entry and its gain/back PCM comparisons are removed.
The remaining plain/dry/full functions, plain-versus-dry and full-chain PCM, overflow/fit rules and
common launch-time checks are unchanged. No scenario TOML, wrapper file or other FX family is deleted.
The replacement owner is `mise run test:runtime -- fx-reuse`; `test:fx` / `test:fx:dt2` own stock-FX
suites and are not the replacement owner.

Runtime ownership also follows the consumed CLI helpers: direct changes to the existing machine
oracle or arena-safety module select the gain case; their import closure, including the function-local
needs wrapper, retains runtime coverage through the existing dependency graph. No synthetic
legacy-runner-to-TypeScript edge was added. The three focused routing cases passed; coordinator
fast preflight of this deletion follow-up passed (final checkpoint below).

| Removed `one()` assertion | Replacement / accepted boundary |
| --- | --- |
| Boot results, safe/loaded state, module names/contracts/rows and inferred DSP range/FX slot | Verified TypeScript `inspect` plus the existing independent needs wrapper |
| Relocated DSP image, exact first 12 chain-row words/defaults and free rows | Independent Python parser/relocator JSON oracle, compared with live DSP bytes/rows |
| Post-note enabled/was/disabled/fault/parameter fields and state call counter | Post-capture row checks and the unchanged >1000 / 90–110 call-count assertions |
| Gain ×0.5 PCM | Existing comparator, at most 2 ULP, RMS tolerance 0.001 dB, onset shift at most 128 samples |
| SELECT/restart, dry-only registry/boot/free rows/capacity and restored dry PCM | Protocol deselection/restart in the TypeScript sequence; bit-identical dry PCM from onset |
| Lifetime write/flush and card-sector policy | Sequenced startup-to-close summaries and every exported overlay, proven on both devices in the safety increment |
| Direct DT2 selected-type observations inside this leg | Explicitly accepted narrower audible-input FX contract; legacy dry/full retain direct type coverage |

Historical static counts above remain the pre-deletion baseline. This deletion subtracts one legacy
rig entry and one explicit restart per device: the `fx` family is now three rig entries and zero
restarts per device. Across the seven legacy quick families, remaining counts are 35 DT2 / 38 DN2
rig entries plus three restarts each (38 / 41 static operations). Full tier still adds one DT2 rig
entry and two restarts; scenario/device pair counts remain 14 quick and 15 full. These are remaining
legacy source counts, excluding the TypeScript case, not uncached boots or measured speedup.

The verified TypeScript execution is unchanged by this deletion-only follow-up; its audible-input
assertion text and selection comment now state the accepted contract. Earlier safety/device results
remain evidence for that unchanged execution. The follow-up fast preflight passed; the coordinator
owned the remaining acceptance decision at that checkpoint; the broad gate remained unrun. Other FX families remain unmodified; subsequent migrations will use the completed assertion mapping and fixture.


Deletion follow-up fast preflight passed (`/private/tmp/fx-gain-deletion-preflight.log`): 345 Python
cases (57 changed-tests and 288 verify), 71 regression-host cases, Ruff, mypy, path, layers,
generated checks, oxlint, Prettier and all workspace types. No device rerun was needed: both-device
safety proof at `b963e257` covers unchanged TS execution; only its wording/comment changed afterward.

The gate was explained against the actual PR base `origin/main` at `0feb02cb`: host/web, three stock
files, 11 runtime files, three module-load files, 49 Python files, smoke and an implicit daemon build.
`/private/tmp/fx-gain-merge-explain-origin-main.txt` supersedes the local-main explanation
(`/private/tmp/fx-gain-merge-explain.txt`): local main at `365b45b` was stale and overselected already
merged changes. At this pre-merge checkpoint, the broad gate had not run and no focused-acceptance
exception existed yet; the subsequent merge decision below supersedes that pending status. Historical proof/count tables and the bounded type-coverage decision stand.


### PR #242 merged and next-batch bookkeeping (2026-11-01)

PR #242 merged at `a41699e5` on 2026-11-01T09:46:15Z. Em explicitly accepted the prior both-device
proofs, helper/host/routing tests and static/type checks under a one-time focused-acceptance exception.
This supersedes the pending acceptance status above; the broad routed gate was not run. The device
runs remain correctness evidence, not a paired speedup benchmark. The bounded audible-input contract
and the retained legacy plain/dry/full coverage remain as recorded in the assertion mapping.

The next coherent batch is mapping the remaining basic-FX and fxpage work, alongside the other
families, for independent workers using the shared fixture and completed assertion mapping. Em
already endorsed parallel migration after the first proof and deletion. This is bookkeeping while
the shared design is mapped, not a broad bulk-deletion plan or a new performance claim.


Next-batch prerequisite: shared gain/SEND support is extracted mechanically into
`tests/regression/runtime/fx-live.ts`. The first migration target is FX SEND: its five DT2 and eight
DN2 same-image legs can use clean reload without alternate images or watch policies. Shared setup
remains explicitly sine/gain on the default emulator image. Optional input hints and per-case
silent-evidence policy do not relax mandatory no-fault/nonfinite/watch/overlay checks. Remaining
basic-FX plain/dry/full and fxpage stay unchanged, retaining their direct type checks; no new SEND
case or further legacy deletion is part of this extraction.


### FX SEND implementation and device proof (2026-11-01)

The SEND replacement covers 13 independent baselines in three owned daemon processes: five common
legs on DT2, five on DN2, and three DN2 chord/idle legs. Each independent leg reloads its pristine
native sine/gain card in the kept process; this is not a persistence restart. The shared fixture
retains startup-to-close write/flush summaries, every-overlay policy checks, guards, finite PCM and
no-fault checks. Idle alone permits silence. This is a coverage/process-count description, not a
measured speedup; stock startup and firmware reloads still occur.

| Legacy SEND facts | Replacement |
| --- | --- |
| Dry audible input; safe boot/module results; clean default row, enabled state and zero route/amount words | Common case plus shared gain-fixture inspection and mandatory closing evidence |
| ZERO/OTHER route 1 and packed amount readback; dry-identical PCM | Common case, unchanged bit-identity and onset-shift limit of 128 samples |
| FULL return adds 0.5; HALF follows `(64/127)^2` | Existing independent added-factor oracle against ZERO, relative tolerances 0.001 and 0.01; positive full gain proves nonidentity with dry |
| Every wet post-note WAS/DIS/NFAULT and >1000 call count; +100 frames gives 90–110 calls | Common and chord cases retain the exact counter assertions; legacy idle retains activity without the delta constraint |
| DN2 chord sums at least two sounding voice slots; idle has >1000 samples and exactly zero peak | DN2-specific anchored slot-bus and existing PCM/added-factor oracles; final fixed-frame proof passed |

Input selection follows the accepted audible-input FX contract, without a direct selected-type
observation in these TS sequences. Basic legacy dry/full retain their direct type proof; plain,
fit/overflow and fxpage remain unchanged. No further family deletion follows from this mapping.

Final common proofs passed: DT2 87.51 s invocation / 82.090 s case
(`digi-fx-reuse-dt2-PJYUEo`), DN2 67.634 s file / 65.074 s case
(`digi-fx-reuse-dn2-KSWG4L`). ZERO and OTHER both had identity shift 0; FULL added about 0.500003
and HALF about 0.126977. Each lifetime check passed eight emulator summaries and five exported
overlays. These durations are correctness evidence only. Earlier `current + 300` timing produced
DT2 shift 192; sharing the first dry target then failed DN2's late-setup guard. The final shape
restores established absolute targets DT2 7500 / DN2 7000, retaining the 20-frame setup guard,
44,000-instruction offset, capture windows and all PCM tolerances. Per-WAV timing artifacts expose
setup frame and target; a missed deadline fails rather than adapting.

Final fixed-7000 DN2 extras passed: 42.055 s file / 39.564 s case (DN2 invocation 109.92 s;
`digi-fx-reuse-dn2-ulBe72`). Both chord legs had exactly two sounding buses, added factor
0.5000029707401741, and idle had 48,000 finite samples with peak 0. Lifetime safety passed five
summaries/three overlays with zero write/flush hits. All three final device cases now pass; no
additional device run is needed for deletion. Static review found no issues. Legacy retirement
checkpoint `42028b59` is integrated as `804bdae2`, after runtime/routing checkpoint `da4fdf7f`: the
SEND wrappers/scenarios and device runner are removed while the analysis CLI remains. No device
execution changed after the final proofs.
Final fast preflight passed: `mise run test:changed -- --base origin/main` ran 621 Python cases
(270 + 57 + 294) and 73 host cases, with Ruff check/format, mypy (one source), pathcheck (zero stale
references), layer/generated checks, oxlint, Prettier and workspace types. Earlier 21 analyzer and
two shared-helper tests also passed. The gate explanation selects host, all 13 runtime fixtures,
47 Python files, smoke and daemon prerequisites; that routed gate was not run. Focused acceptance
for this batch is pending Em's decision after the concrete PR; PR #242's exception
does not transfer.


### Routing ownership checkpoint (2026-11-01)

PR #244 merged at `097eefa2`. The next routing change uses Vitest's existing import discovery
for TypeScript helpers inside the stock, runtime and module acceptance directories. It discovers
filenames without collecting tests or executing hooks; it adds no separate dependency graph.
`runtime/fx-live.ts` selects the three FX files, while `runtime/live.ts` selects all 13 runtime
files and retains all module lifecycle files through its explicit shared ownership. Missing tools,
discovery failures, unconsumed helpers and deleted files retain the whole owner suite. Production, shared session and configuration routes remain conservative.

Runtime TypeScript and digikit scenario TOMLs no longer select the unrelated smoke matrix by
themselves. Production sources, module-owned scenarios, smoke inputs and shared/configuration
files retain smoke. The gain fixture's two TOML sine hints remain explicit runtime data owners,
because filesystem reads are outside Vitest's import discovery. Deleted digikit scenarios still
select the whole Python suite; historical ownership recovery is deferred. These are static
selection changes, with no measured execution speedup or new device proof.


Validation for this routing checkpoint: 648 Python cases and 73 host cases passed across the
final preflight and the single stale-expectation retry. Lint, formatting, layers, generated checks
and workspace TypeScript types passed; mypy was skipped. The overall preflight is not a clean
pass: pathcheck retains three pre-existing references in `docs/plans/module-architecture.md`
(lines 3, 4 and 284), unchanged from `origin/main`. The new adapter references were resolved by
staging the file, and scoped pathcheck then reported zero stale references.

The 15-case static comparison selects FX helper files 13→3 and remote helper files 13→1;
`live.ts` and `runtime-plan.ts` remain 13→13. Production and configuration selections remain
unchanged, apart from correcting `live.ts`'s shared module lifecycle coverage. Replaying PR #244
selects runtime files 13→3 with all 37 Python plan entries retained; its harness change still
requires smoke. The final gate explanation selects host, three stock files, 13 runtime files,
three module files, Python checks and smoke. That gate was not run. No measured speedup or new
device proof is claimed.


### FX route migration checkpoint (2026-11-01)

PR #245 merged at `dd542a11` under Em's explicit focused-acceptance exception, with three prior
baseline path errors retained. That decision was not a full-gate pass and does not transfer to
this batch.

The route port covers all eight `fxroutes` and eleven `fxroute` legacy legs per device: 19 independent
baselines across four owned fixture contexts (panel, persistence, no-hook reference and hooked
track cases). Each context has one exact prepared image, symbol plan and startup-to-close write-watch
policy. The three image variants remain `RT + dynfx`, that set plus `fxpages`, and that set plus
`fxpages + digikit-menu`. Independent cases use pristine cards; the two menu-driven route changes
retain real guest restarts and before/after persistence evidence. Different images do not share
allowed-PC sets. The hooked track context runs its ten legs sequentially, retaining the no-hook
reference in its own process and preserving the final four-overlay project-lock comparison.

Independent review of track port `cbe4566a` found no missing assertions. Fixes preserve paused
protocol queries through the existing stepped USB transport (`fdb745ad`), initialize PCM observation
before route inspection (`a0f52563`), and use the existing epoch/UI restart barrier (`761aebba`).
Prepared loads retain the stock bytes needed to compute the DSP difference while still forcing
firmware reconstruction with the explicit clean card (`f296f998`). Artifacts were repacked through
the standard current-source SDK 1.2 wrapper after a shared-binary mismatch. Gate preparation/routing
checkpoint `440f676a` is pushed. Restart inspection now receives the original captured epoch, while
fresh baselines remain strict; only the combined ten-leg test has a 600-second wall budget
(`9f0cad73`). Guest timing, note margins, PCM tolerances and assertions remain unchanged.

All six device/family proofs passed:

| Device / family | Wall duration | Artifact suffixes | Lifetime safety |
| --- | --- | --- | --- |
| DN2 panel | 151.99 s invocation / 146.644 s case | `rj0mZ0` | 11 watch windows, 5 overlays passed |
| DN2 persistence | 121.88 s invocation / 115.753 s case | `cCWGMv` | 11 watch windows, 8 overlays passed |
| DT2 panel | 179.062 s file / 173.855 s case | `XQjKMS` | 11 watch windows, 5 overlays passed |
| DN2 track | 428.32 s invocation; 16.751 s reference + 401.264 s combined case | reference `8hTbj1`, hooked `dJ0EdG` | reference 3 windows / 1 overlay; hooked 15 windows / 13 overlays passed |
| DT2 persistence | 151.408 s file / 146.082 s case | `lX6mnZ` | 11 watch windows, 8 overlays passed |
| DT2 track | 360.238 s file; 24.078 s reference + 324.879 s combined case | reference `fgYuXW`, hooked `rGPXvd` | reference 3 windows / 1 overlay; hooked 15 windows / 13 overlays passed |

The final DT2 persistence/track invocation took 511.90 s. All safety reports passed with zero
write/flush hits. Watch windows are emulator summaries within the
owned fixture lifecycle, not a count of operating-system processes. The DN2 panel load report confirms
DSP application at cold boot. These durations establish correctness only: no paired A/B or measured
speedup claim is made.

Reviewed legacy retirement `eaf709b9` is integrated and pushed as `86e1a7ae` (23 files, +49/-1400;
349 tests and 15 subtests reported for the retirement work). The legacy route device scenarios
are removed; the independent analysis oracles remain. No device execution changed after the final
proofs.

Final preflight exited 1 only because pathcheck found three pre-existing references in
[module-architecture.md](module-architecture.md), at lines 3, 4 and 284; the file is unchanged from
`origin/main`. This is not a fully green preflight. All 666 Python cases (270 + 57 + 339) and
95 host cases (4 + 91) passed. Ruff check/format, mypy on two sources, layer/generated checks,
oxlint, Prettier and workspace types passed. Scoped pathchecks for the changed docs also passed.

The gate explanation selected host, three stock files, 16 runtime files, three module files,
54 Python files, smoke and native prerequisites. The broad routed gate was not run. PR #246 merged
at `e8c0e8ace2572f9a43a1d58f1f4c5343c7db3cde` on 2026-11-01T19:00:08Z under Em's explicit
PR-246-only focused exception, using the both-device and scoped validation above. This is not a
broad-gate pass, and the exception does not transfer to the next batch.

### BASIC FX and FUNC + FX pages checkpoint (2026-11-02)

The approved next batch ports the remaining BASIC chain and FXPAGE assertions together. Shared
`fx-session.ts` accepts explicit image roots, inventory, required symbols and safety policy; these
ports choose `default` explicitly. Plain/reference images remain independent, while hooked legs
run sequentially in kept contexts with pristine baseline cards. Fresh readiness remains strict;
only loader result words are interpreted as signed int32, including expected `-34`. The generic
extraction preserves the existing route configurations and their route-specific policy.
Legacy note margins, guest timing, PCM tolerances and audibility thresholds remain in their owners;
FXPAGE relative PCM oracles retain audibility responsibility without relaxing finite/safety checks.

| Legacy assertion group | Committed replacement ledger |
| --- | --- |
| BASIC capacity, selected type, free/live rows, overflow refusal and unchanged serial-gain PCM | [fx-chain.device.test.ts](../../modules/core/adapters/dynfx/tests/fx-chain.device.test.ts), with separate plain reference and sequential dry/full baselines |
| FXPAGE stock hashes, free-index poke invariance, page/index metadata, GAIN/MIX and A–E controls, SELECT/restart removal | [fx-pages.device.test.ts](../../modules/core/adapters/fxpages/tests/fx-pages.device.test.ts), with independent reference and sequential stock/effect/deselect context |

Both ports and the reviewed parity fixes passed their four family/device proofs. DN2 BASIC took
58.75 s and DN2 FXPAGE 293.71 s. The combined DT2 invocation took 201.23 s, with BASIC 76.193 s
and FXPAGE 124.770 s per file. These are correctness durations, not a speedup comparison.
On each device BASIC passed 3 + 5 lifetime watch windows and 1 + 2 overlays; FXPAGE passed
3 + 7 windows and 1 + 4 overlays. All eight lifetime-safety reports passed the default policy,
with zero write/flush hits and no invalid sectors.

The four legacy wrappers and four scenario TOMLs are retired after those proofs:
`REF:tests/digikit/test_fx_dt2.py`, `REF:tests/digikit/test_fx_dn2.py`,
`REF:tests/digikit/test_fxpage_dt2.py`, `REF:tests/digikit/test_fxpage_dn2.py`,
`REF:tests/digikit/scenarios/fx-dyn-dt2.toml`, `REF:tests/digikit/scenarios/fx-dyn-dn2.toml`,
`REF:tests/digikit/scenarios/fx-pages-dt2.toml`, and `REF:tests/digikit/scenarios/fx-pages-dn2.toml`.
The Python kind dispatch and runner bodies are removed. Shared row/PCM oracles and profile,
kit, screenshot and knob utilities remain for `fxpersist`, `fxshift` and the TypeScript suites.
Gain aliases 2/3/4 are prepared only for the complete BASIC inventory, and sine selection now
uses the same numeric 7/5 defaults without reading the retired TOMLs. Retirement `bb3972ee`
is committed (21 files, +61/-737, eight deleted). Focused checks passed 14 offline tests plus
15 subtests, 14 routing tests and 12 changed-test cases; independent review reported no issues.

The final required `test:changed` invocation exited 1 on three baseline path errors in
[module-architecture.md](module-architecture.md) (lines 3, 4 and 284, unchanged from `origin/main`)
and two stale expected-list failures. Commit `0adc052d` corrected those two expectations, and both
retry cases passed. Across the run/retry, 674 Python cases passed (270 budget + 57 changed +
345 verify + two retries), along with 99 host cases. Ruff check/format, mypy on two sources,
layers/generated checks, oxlint, Prettier and workspace types passed. Scoped changed-doc
pathchecks passed. The original preflight was not a fully green run.

The authoritative gate explanation uses `origin/main` at `e8c0e8ac` and selects eight FX runtime
files, 41 Python files, host checks, smoke and native prerequisites. The initial `--base main`
explanation used a stale local ref and is superseded. The broad gate was not run.
[PR #247](https://github.com/ElekLabs/digikit/pull/247) merged at `4bd50f5b` on
2026-11-01T19:45:12Z under Em's PR-247-only focused exception, accepting the three baseline path
errors and the focused evidence. This was not a broad-gate pass; the exception does not transfer.
Timings remain correctness evidence only, without a paired speedup claim.

Deleted-scenario routing remains deferred because path-only callers need merge-base ownership
evidence. No cache or tombstone tooling is introduced by this batch.

### FX kit persistence and frequency shifter design (2026-11-02)

The approved remaining FX batch keeps one owned context per device and family, with pristine
cards and explicit baseline reloads for independent legs. DT2 persistence uses the full real
SELECT/restart flow, which subsumes the quick table-poke setup; DN2 retains its existing quick
coverage. Preserve every unique kit-storage, page/row, PCM, project and restart oracle.
Frequency-shifter dry, master, send and other-track legs run on both devices: the DN2 TOML header
is stale, while the existing runner invokes those legs unconditionally. Preserve its independent
PCM/model, parameter, send and settings observations without changing thresholds or guest timing.

The shared prerequisite is the fixed `fx-kit` card-safety policy: exactly the existing per-device
`KIT_AREAS` firmware ranges, with no settings-sector allowance. Default and `fx-routes` policies
retain their existing behavior; callers cannot supply arbitrary allowlists.

The policy prerequisite `9b23b0ff`, kit/UI helpers `6f12384f`, persistence/shifter ports
`73711a01` / `01594832` and gate ownership `8b7c248e` are pushed. Source reviews found no issues.
DN2 persistence passed in 129.50 s invocation / 123.760 s case (`zDOVuw`), with three watch windows
and two overlays under `fx-kit`. DN2 shifter passed in 215.72 s / 209.578 s (`WDkalo`), with nine
windows and four overlays under `fx-routes`. Both lifetime reports had zero write/flush hits.
DT2 shifter passed in 398.171 s file / 375.478 s case (`acXXAd`), and DT2 persistence in
328.322 s / 320.201 s (`86nKeR`), with a combined invocation of 726.77 s. DT2 persistence passed
seven watch windows/four overlays under `fx-kit`; DN2 persistence passed three/two. Both shifter
reports passed nine/four under `fx-routes`. All four lifetime reports passed with zero write/flush
hits and empty invalid-store/low-sector lists. These timings are correctness evidence, not a
performance comparison; no new gate exception exists.

The Nx audit confirms safe use of the five current cached host targets and partial adoption.
Caching and affected-test routing are separate concerns. A next family-project migration remains
unimplemented and needs an exact folder layout, validated project graph and selection matrix;
see the [Nx audit checkpoint](nx-pilot.md#family-project-audit-checkpoint-2026-11-02). No custom
plugin, cache expansion or selector deletion is authorized by this audit alone.

The proved migration retires the four fxpersist/fxshift wrappers, three persistence scenario TOMLs
(including full DT2 restart) and two module-owned shifter scenario TOMLs. Historical inputs:
`REF:tests/digikit/test_fxpersist_dt2.py`, `REF:tests/digikit/test_fxpersist_dn2.py`,
`REF:tests/digikit/test_fxshift_dt2.py`, `REF:tests/digikit/test_fxshift_dn2.py`,
`REF:tests/digikit/scenarios/fx-persist-dt2.toml`, `REF:tests/digikit/scenarios/fx-persist-dn2.toml`,
`REF:tests/digikit/scenarios/fx-persist-restart-dt2.toml`,
`REF:modules/fx/freqshift/tests/scenarios/fx-freqshift-dt2.toml`, and
`REF:modules/fx/freqshift/tests/scenarios/fx-freqshift-dn2.toml`.
The persistence runner module is deleted after moving its exact kit ranges into arena safety.
FXPAGE retains only its PCM CLI; BASIC retains range/fit/peak analysis; the shifter retains its
unchanged spectral/model PCM oracles with an explicit model argument. Module unit, reference and
conformance tests remain. Source ownership follows the reduced imports, with pack/aliases,
production module coverage and conservative unknown/deleted-source fallbacks retained. Final
preflight and gate acceptance remain pending; no device execution changed after the final proof.

The frequency-shifter module's own `project.json` declares its uncached Nx `test-device` target,
reusing the existing named verify selection/preparation workflow. `module:check` runs this target
after Python tests (also for an owned target without Python tests), propagates failures and reports
size/fit only after both pass. Both-device daemon/ready resolution uses the existing harness and
overrides, with a shared lock around resolution only. The parent module-tests project's dependency
retains prior host ownership after introducing the nested project. Conservative bounded target
source inputs cover acceptance, preparation and model code; this is not the proposed family-wide
routing replacement or a cache-closure claim. The command's device acceptance is preserved while
retiring its old TOML discovery path; no second Python module-ownership registry is introduced.


### PR #248 continuation checkpoint (2026-11-02)

[PR #248](https://github.com/ElekLabs/digikit/pull/248) is draft, with code pushed through
`942cae59`. Retirement `ec112ced` removes ten legacy files; module-local placement `501f0482`
keeps the frequency-shifter scenario, analyzer tests and real-module routing assertions together
under its module. The four device proofs above stand unchanged; the moved scenario differs only
in imports and the analyzer tests are byte-identical. Shifter screenshots are captured evidence,
not pixel-equality assertions; its four legs are dry, master, send and other-track send.

Parent preflight passed 680 Python cases, 336 host cases (one skipped), all types/lint/format,
layer and generated checks. Pathcheck alone failed; all new references were corrected and scoped
pathcheck passed, leaving three baseline reports in the separate module-architecture plan.
Focused metadata/routing/collection checks and independent review passed. The broad routed gate
is unrun; no exception from an earlier PR transfers. Exact evidence, inputs and next actions are in
[the handover](../handovers/HANDOVER-2026-11-02-test-consolidation.md).


### PR #248 acceptance decision (2026-11-02)

Em explicitly approved merging PR #248 using the recorded focused checks, accepting the three
baseline pathcheck reports. The full routed gate remains unrun; this is a PR-248-only exception,
not a full-gate pass or permission to skip later gates. The PR and external testing roadmap
record the final merge state/commit; earlier draft/pending statements above are superseded.
