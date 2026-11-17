# Developing digikit

The details behind the [README](../README.md): firmware files, the emulator and its options, tests, analysis tools and the repository
layout. For modules see [MODULE-AUTHORING.md](MODULE-AUTHORING.md) and [PATCHING.md](PATCHING.md); for tests [TESTING.md](TESTING.md).

For version ownership and preview/apply commands, see [Releasing digikit](RELEASING.md).

## Firmware files

Modules and patchkit target Digitakt II 1.17 and Digitone II 1.12. The emulator runs
these two by default. It also runs 1.16 and 1.11, which most earlier analysis used.
1.15C and 1.10E are kept for old analysis. Get the `.syx` files from Elektron's support
site for your device; the repository never includes them.

| device | version | file | sha256 |
| --- | --- | --- | --- |
| Digitakt II | 1.17 | `Digitakt_II_OS1.17.syx` | `26c22f6652625ac2cfd47f7ee970d388ed8b6427dae3563c0a6a2f2d334350d5` |
| Digitone II | 1.12 | `Digitone_II_OS1.12.syx` | `7e5b8a733d4ab1fdf7e5d22757f7d8eddaa3cbf71d0858874a791844c02ceb35` |
| Digitakt II | 1.16 | `Digitakt_II_OS1.16.syx` | `278541e466edcd77d6b3e018a91fb90185932d3c7de224dd3e68294dddf3a9ec` |
| Digitone II | 1.11 | `Digitone_II_OS1.11.syx` | `2af43e65e3d8390b41c9f66222620f8cce027d73ed87db00c80b440f628472e0` |
| Digitakt II | 1.15C | `Digitakt_II_OS1.15C.syx` | `62d588456e47194bd56dfee9568fb9dd4521c4ff1e8b5427eb461355532e8c6c` |
| Digitone II | 1.10E | `Digitone_II_OS1.10E.syx` | `9881ee5ca4624be2b833e5eb94e294f2f8425371e6729781baccd1c256cac504` |

Put the files in `firmware/`; the tools look there first (`tools/firmware/config.py`).
`firmware/README.md` lists the images and their SHA-256 values. `DIGI_EMU_SYX`
(Digitone II) and `DIGI_EMU_SYX_DT2` (Digitakt II) override the paths.

## Setup

You need [mise](https://mise.jdx.dev/) (it installs the pinned Rust and pnpm) and
[uv](https://docs.astral.sh/uv/) (Python 3.12).

```sh
mise install
mise run setup      # uv sync, pnpm install
```

`mise tasks` lists every task.

## Extract the firmware

```sh
uv run python -m tools.firmware.extract firmware/Digitone_II_OS1.12.syx -o out/sections/dn2-1.12
uv run python -m tools.firmware.extract firmware/Digitakt_II_OS1.17.syx -o out/sections/dt2-1.17
```

Keep each firmware in its own directory (`out/sections/dt2-1.17`, ...): the analysis
tools find images by these names. A sections directory records the SHA-256 of its
`.syx` in `out/sections/<image>/.source-sha256`, and the tools refuse to mix firmwares. Compare it with
`shasum -a 256 <the .syx>` before you trust a run.

## Ready states

The native emulator starts from a ready state: ColdFire booted to the main screen, DSP
running. Build it from the `.syx` alone:

```sh
mise run emu:ready -- dn2                     # Digitone II 1.12 (about 2 minutes)
mise run emu:ready -- dt2 [--card-image IMG]  # Digitakt II 1.17, optionally with a +Drive image
```

It needs the shared core (`mise run gen:core`). The files go to the directories in `tools/emulator/ready_inputs.json`
(`out/snapshots/<device>-audio-ready-cold-*`), where the patchkit harness and the device tests read them. Git ignores them.

## Desktop emulator

```sh
mise run workbench                # build once and open the app with no firmware loaded (`-- --build-only` builds only)
mise run workbench -- FILE.syx    # load that firmware at start (else the Emulator page's "Load on start" tick loads the last opened file)
mise run workbench:inputs         # the UI dist and the wasm files, from the Nx cache when unchanged (the workbench runs it first)
mise run wasm:dkprof              # only public/dkprof.wasm (`wb` addrmap, module and profile commands load it)
mise run workbench -- --cf-only --plus-drive DIR firmware/Digitakt_II_OS1.17.syx   # a +Drive folder (default: plus_drive/<device>/; --samples is the same option)
mise run workbench -- --dsp-patch FILE  # overlay a DSP patch (DN2 audio)
mise run workbench -- --cf-only   # ColdFire only: no DSP, no audio
```

With the ready inputs present, a firmware loaded in `mise run workbench` plays audio. Without them it
starts the ColdFire-only emulator. The DSP needs about 1.6 CPU seconds per audio second
([finding 18](findings/18-dsp-performance.md)), so real time is not reached yet.
The +Drive comes from `plus_drive/dt2/` or `plus_drive/dn2/` (git ignores both) when it exists, or from `--plus-drive DIR` (`--samples DIR` is the same option): the launcher builds a card image from the folder and caches it by content ([docs/SAMPLES.md](SAMPLES.md)).
Track N of the DT2 plays file N (sorted by name; more than 16 files wrap around).

**Choose samples (desktop app, Digitakt II).** With the DT2 running, the toolbar's "Choose
samples" button picks a folder. The app sends its top-level `.wav` files to the controller,
which builds the card in process, cold-boots the DT2 with it (about 12 s the first time at
the default fast tier, `build 1 s + boot 12 s` for three files), presses NO on the FILE SYSTEM
OK window and swaps the runtime; the toolbar shows "Building the card", "Booting the Digitakt
II with the card (n%)" and "Card ready". The card and its ready state are cached by a hash of
the firmware and the samples under `<run dir>/cards/<key>/` (default `out/native/run`), so the
same folder restores in well under a second. It needs the DT2 ready inputs (`mise run emu:ready --
dt2` once: the DSP image and `cold-idle.dsp` come from there). The app remembers the last card
(`<run dir>/cards/last.json`) and mounts it again when a DT2 starts without `--card-image`, in
well under a second; the toolbar shows "Samples: NAME (N files)", or "Choose samples to load a
folder of WAVs" when nothing is cached. A DT2 launch without `SHARC_GEN_DIR` uses the generated
core its PGO profile was trained on (the launcher prints which core and why), so no environment
settings are needed. To choose a core: `SHARC_GEN_DIR_DN2` sets the DN2 core, `SHARC_GEN_DIR_DT2`
sets the DT2 core, and `SHARC_GEN_DIR` sets one core for both devices (the device variable wins).
Without any of them the core registered for the DSP image is used (`out/native/gen-index.json`). The browser shows the button
disabled ("desktop only for now"). Headless: the control socket's `card.load {dir}`,
`card.status`; `uv run python -m tools.emulator.card_check` checks that trigs 1-3 play a 220 Hz sine, an
880 Hz sine and noise from three generated files.

**Log panel (desktop app).** The "Log" button in the status bar, or ⌘J (Ctrl+J on other systems), opens a panel with the events of this window: module reloads and builds, firmware loads, card changes, Transfer runs, emulator resets, and guest faults, stalls and DSP halts. Each entry has a time, a source, one line, and for long output (a compiler log) a ▸ button that shows it in full. The panel has a source filter, "Errors only", "Copy" (the entries shown, with details) and "Clear". It keeps the last 500 entries in memory. It opens by itself for a failed reload or build and for a guest fault, stall or DSP halt; other errors add to the badge on the Log button. The Log is a panel docked along the bottom of the window, full width: the page above it gets smaller, nothing is covered. Drag its top edge to change the height (double click resets it; the height is remembered). Metrics stays a small popover above the status bar. The app polls the controller for stalls and halts once a second, so they can show up to a second late.

### workbench-core, the two apps and the command line

`packages/workbench-core` is the code that the site (`apps/web`) and the desktop app (`apps/desktop`) share. The apps import it as `@digi/workbench-core/<path>` with the file extension (`@digi/workbench-core/transfer/app.ts`, `@digi/workbench-core/ui/transfer/TransferPage.tsx`); there is no `@web` alias.

- `src/`: logic and interfaces, no Solid components: `transfer/` and `firmware/` (the `*-core.ts` logic, `TransferEnv` and `FirmwareEnv`, `createTransferApp` and `createFirmwareApp`), `jobs/`, `modules/` (the upload client, needs, fit), `patch/`, `runtime/` (`EmulatorRuntime` and its helpers), `host/` (the `Host` and `EmuRpc` interfaces, `FakeHost`, the MIDI and bulk links over a host group), `storage.ts` (`KeyValue`), `blob-store.ts`, `workers.ts` and `workers-fake.ts`, `wasm-source.ts`, the split-session code and its two workers, `testing/` (the fake flash device) and `node/` (the NodeHost).
- `ui/`: the Solid pages, components and styles that both apps show the same way (Transfer, Firmware, the panel).
- `tests/`: the Vitest project `workbench-core` (`pnpm nx:pilot run @digi/workbench-core:test-host`). The tests of the site-only and desktop-only code stay in the apps.
- What stays in an app is the code that needs the app's platform: the site's WebMIDI, WebUSB, `browserWorkers()` (`apps/web/src/browser-workers.ts`, the only `new Worker`) and Astro pages; the desktop's `TauriHost` (`host/tauri-host.ts`, the only `invoke`), `createNativeRuntime` and its panels.
- The apps do not import each other, and workbench-core imports no app (oxlint `no-restricted-imports`; `pnpm lint` fails it). Code that both need moves into workbench-core.
- Nx: workbench-core is its own project. An edit in `src/` or `ui/` selects its own `test-host` and `typecheck`, the site's `check`, `test-host`, `test-wasm` and `build`, and the desktop's `typecheck`, `check:ui`, `test-host` and `build:ui`; no cargo, wasm-build or device target. An edit in `tests/` selects only its own targets.

`createNodeHost(options)` (`src/node/node-host.ts`) is the `Host` for a command line, so a command runs the workbench's code over the same interfaces as the app: `fs` over `node:fs` (the rules of `local.rs`: one level, folders first, no dot files, no path outside the folder), `library` (`out/modules` and the `devices/*.toml` profiles), `ports.midi` and `ports.bulk` over the `dkmidi` and `dkbulk` bridge processes, `firmware.patchkitWasm` and `sampleWasm` from `apps/web/public`, and `rpc` over the control socket of a running emulator (`packages/control-client`). A command that needs a window or the app's own emulator session (a dialog, `library.build`, `emu.*`, `firmware.prepare`) fails with `NodeHostUnsupported`. `wb` (`tools/devtools/wb.mjs`) builds one: `wb ports` lists the MIDI ports through `host.ports.midi.ports()`, and the samples commands choose their backend from `host.library.deviceStorage(...)`, as the desktop's Samples backend does. Test it with temp folders and stand-in bridge programs (`tests/node-host.test.ts`).

### Host and EmuRpc (desktop UI)

The desktop UI never calls Tauri's `invoke`. `packages/workbench-core/src/host/host.ts` types every Tauri command the UI uses, in groups (`fs`, `library`, `ports.midi`, `ports.bulk`, `firmware`, `emu`, `shell`), and `rpc` is `EmuRpc`: `call(method, params)` over `emu_call`. `host/tauri-host.ts` is the only module that imports `invoke` (oxlint `no-restricted-imports` fails any other file). `main.tsx` makes the host and passes it to `createDesktopApps(host)` and `<Console host={host}>`; modules take the host (or one group of it) as an argument. The Host types and the fake live in workbench-core; `TauriHost` is the desktop's, and `NodeHost` (above) is the command line's.

Add a command: write the Rust command, then add a method to the group in `host.ts`, its `invoke` line in `tauri-host.ts` and its `leaf(...)` line in `fake-host.ts`. `library.readMaps(device)` returns the address maps (docs/ADDRMAP.md): the ship maps `firmware_prepare` kept for the device's images and the maps `lib_build` writes beside each `.dkm`; `firmware.dkprofWasm()` returns `dkprof.wasm` (`mise run wasm:dkprof`; loader `ui/src/dkprof-env.ts`), which the Log pane uses to name addresses (the emulator's ship map and symbol addresses come from the controller call `runtime.maps`; the load table is read with PEEK of the debug module through `DkSession.peek`, for the emulator and a device). The emulator session commands (`emu_startup`, `emu_step`, `emu_tap`, `emu_stop`, `emu_audio_status`, `emu_stage_syx`, `emu_save_diagnostics`, `emu_card_*`) are in `host.emu` for `createNativeRuntime`.

Tests use `createFakeHost(replies)` from `host/fake-host.ts`: it records `calls` (command as `group.method`, arguments, order) and answers from `replies` (a value, or a function of the arguments). A command with no reply fails, except the ones that answer nothing (open, close, send, cancel). Examples: `ui/tests/samples-backend.test.ts`, `card-api.test.ts`, `links.test.ts`. The browser preview (`mise run ui:preview`) still replaces `@tauri-apps/api/core` with `ui/preview/tauri-mock.ts` behind `TauriHost`.

### Target facade

`packages/workbench-core/src/target-ops/` has `TargetOps { target; store: StoreOps; run: RunOps }`: what the UI asks of the selected target, the emulator or a device. `Host` stays the catalogue of platform services (files, dialogs, ports, firmware, `emu.*`); it does not describe a target.

`store` works the same on both targets. `sessionStoreOps(session)` reads the card store over `DkSession` (INFO, LIST, CAPACITY, MODULES, LOADS, READ, VERIFY). `run` is a stub for now: `unwiredRunOps` makes every operation say why it is not wired. It is filled later, when pause, resume, reset and restart move behind this layer (emulator through `EmulatorRuntime`, device only restart through the protocol).

Tests and the preview use fakes: `fakeStoreOps` and `demoStore` (`target/fake-store.ts`). The first user is the Files page (`packages/workbench-core/ui/files/FilesPage.tsx`), which shows the store read-only; open it in the desktop UI preview with `?page=files`.

### The app object, KeyValue and BlobStore (desktop UI and site)

The Transfer pane, the Firmware page and the target selector keep no module-level state. `createTransferApp(env, { storage, sessions })` (`packages/workbench-core/src/transfer/app.ts`) builds one app from the environment (`TransferEnv`): `app.target` (selected target, its sessions and transports), `app.connection`, `app.jobs`, `app.state` (the Transfer page's signals), `app.backend` and `app.loadLibrary`. `createFirmwareApp(env, transferApp)` (`packages/workbench-core/src/firmware/app.ts`) adds `defaults` (default modules) and `state` (the flash). An entry point builds the apps once and wraps its pages in `<AppProvider app firmware>`; a component takes them with `useTransferApp()` or `useFirmwareApp()` and destructures what it uses. Code that is not a component takes the app (or the part it needs) as an argument.

- Desktop: `createDesktopApps(host)` in `apps/desktop/ui/src/desktop-app.ts`, called by `main.tsx`.
- Site: `createWebTransferApp()` in `apps/web/src/transfer/web-env.ts` (Transfer) and `createWebFirmwareApp()` in `apps/web/src/firmware/web-env.ts` (Firmware), called by the Astro islands `WebTransfer.tsx` and `WebFirmware.tsx`.
- An environment needs the app it will belong to (its ports and transports call `app.target`, `app.connection`): its factory takes `app: () => TransferApp`, a function because the app is built from the environment.

What an app is given, so that a test gives something else:

- `storage: KeyValue` (`packages/workbench-core/src/storage.ts`): `get`, `set`, `remove` of small strings. `localStorageKeyValue()` is the browser's (every call guarded); `memoryKeyValue(initial)` is the fake (`.data` shows what was written). `remembered(storage, key, parse)` is a signal kept in it.
- `sessions: SessionHub` (`transfer/session-core.ts`): `get(key, transport)`, `drop`, `cancel`. `createSessionHub(make)` makes one `DkSession` per target; `createFakeSessions(make)` (`transfer/fake-app.ts`) makes whatever the test returns and records `made`, `dropped` and `cancelled`.
- `BlobStore` (`packages/workbench-core/src/blob-store.ts`): `get`, `put`, `delete` of bytes. `indexedDbBlobStore(database, objectStore)` is the browser's; `memoryBlobStore()` is the fake. The ready-state cache (`ReadyCache`) and the firmware hand-off (`patch/handoff.ts`) take one.

`createFakeTransferApp({ env, storage, sessions })` (`packages/workbench-core/src/transfer/fake-app.ts`) is the real `createTransferApp` over those fakes and a stub `TransferEnv` that a test overrides in part. Examples: `apps/web/tests/transfer-app.test.ts` (target and session selection, remembered values), `apps/desktop/ui/tests/module-loading.test.ts`, `samples-backend.test.ts`. Only `storage.ts` touches `localStorage`, `blob-store.ts` touches `indexedDB` and `apps/web/src/browser-workers.ts` calls `new Worker` (oxlint `no-restricted-globals`).

### WorkerFactory, WasmSource and the two runtimes

Code that needs a Web Worker takes a `WorkerFactory` (`apps/web/src/workers.ts`): `create(kind)` returns a `WorkerPort` (`postMessage`, `onmessage`, `onerror`, `addEventListener`, `terminate`) for the kind `runtime`, `dsp`, `cf` or `patch`. `browserWorkers()` is the browser's (`apps/web/src/browser-workers.ts`, the only `new Worker` in the apps; oxlint `no-restricted-globals` fails any other); `createFakeWorkers({ runtime: handler })` (`workers-fake.ts`) runs a function of the test instead of a thread: the handler gets each posted message and answers with `worker.reply(data)`; `made` and `posted` show what happened. A worker cannot be given the page's objects, so `patch-worker.ts`, `dsp-worker.ts` and `cf-worker.ts` build what they need from the URL or base in their start message.

Wasm files come from a `WasmSource` (`packages/workbench-core/src/wasm-source.ts`): `bytes(name)` for `patchkit.wasm`, `emulator-core-sharc.wasm` and (desktop only) `dksample.wasm`. The site's is `fetchWasmSource(BASE)` (it also compiles while it downloads); the desktop's is `hostWasmSource(host)` (`desktop-wasm.ts`, over `Host.firmware`); a test gives `memoryWasmSource(files)` and `EMPTY_WASM`. `TransferEnv.wasm` is the Transfer pane's. Where real wasm is not the subject, a test gives a stub of what it does: `PatchKitLike` (`patch/patchkit-core.ts`; `senderKit(wasm, create)` and `createWebFirmwareEnv({ createKit })` take one) and `NeedsLike` (`modules/dkneeds.ts`; `createFakeTransferApp` uses a stub). The tests that need the real wasm stay in `test-wasm` (`*.wasm.test.ts`).

`EmulatorRuntime` (`packages/workbench-core/src/runtime/runtime.ts`) has two implementations that share no code:

- `createNativeRuntime(host, onUpdate)` (`apps/desktop/ui/src/native-runtime.ts`): the controller's session and pump through `Host.emu` and `Host.rpc`; it resolves with `undefined` when `host.emu.inApp()` is false (a plain browser). `Console.tsx` makes it. Test it with `createFakeHost` (`ui/tests/native-runtime.test.ts`).
- `browserRuntime(onUpdate, { workers, wasm })`: the worker or the two-worker split session; `browserRuntimeDeps()` (`runtime/browser-deps.ts`) makes the browser's `deps`. Test it with fake workers (`apps/web/tests/browser-runtime.test.ts`).

## Web panel

```sh
pnpm dev                           # build the browser core (WASM) and serve the panel
pnpm build                         # static build; it fails if firmware-derived files would ship
mise run web:wasm                  # only the browser core
mise run web:parity trig1-tap-release   # browser PCM equals native PCM
```
The browser DSP is slow; the browser host is parked on the branch `wip/browser-host`.

## Control socket and the patch loop

```sh
mise run emu:serve                      # headless, paused; serves the control socket
mise run emu:ctl status                 # status, step N, tap CODE, frame out.png, events
uv run python -m tools.control.digi_client inject PATCHED.syx   # hot DSP injection
uv run python -m tools.patchkit.dsp_patch syx firmware/BASE.syx tools/patchkit/dsp_patches/dn2-1.12-master-half.patch OUT.syx
```

`tools/patchkit/dsp_patch.py` edits DSP code or data by SHARC+ address and writes a patched
`.syx`. Control socket reference: [docs/CONTROL.md](CONTROL.md). The protocol design is in [docs/design/emulator-control.md](design/emulator-control.md).

## Scenarios and tests

A scenario is a list of panel inputs (button presses, encoder turns) that the headless
runner plays against a ready state. It reports the PCM hash and level
([tests/scenarios/README.md](../tests/scenarios/README.md)).

```sh
mise run scenario:all         # every DN2 scenario; fails when a pinned PCM hash differs
mise run scenario trig1-taps --wav
```

Test levels, layout and all commands are in [docs/TESTING.md](TESTING.md); what a change selects and why a device test is keyed as it is: [TESTING.md, "Check what a change selects"](TESTING.md#check-what-a-change-selects). The most used:

```sh
mise run test:changed         # lint, format, types, host tests and Python checks of the affected projects; no builds or devices
pnpm test:host                # host Vitest projects; no device
pnpm verify:host              # host lint, format, tests and types
mise run test                 # the gate: Nx targets of the affected projects
mise run test:subset -- S:machines  # the device tests of record tags plus gate-smoke and gate-pins; add --run to run them (docs/TESTING.md "Named device subset")
mise run module:check ID      # one module: conformance and its own tests
pnpm nx:pilot run @digi/sharc-rig-kit:test-conformance-rig   # machine and effect conformance of every module on the SHARC-only rig (docs/TESTING.md "The Vitest kit")
DIGI_RIG_REPORT=1 pnpm exec vitest run --config vitest.host.config.ts --project module-rig modules/machines/analog/tests/   # verdict, plot.png and wav files for the pinned cases (docs/TESTING.md "Classifying a changed pin")
node tools/devtools/rig_profile.ts digikit/shot --device dn2 --case sample/stereo-fwd-oneshot/note60   # `wb profile report` of one case on the rig: instructions per function and loop (docs/TESTING.md "SHARC-only rig")
```

Pins are valid only for the firmware and ready state they were recorded with.

## Performance and instrumentation

```sh
mise run perf                 # DSP CPU per audio second, tier split, per-function cost ranking
mise run perf:cov             # fast-tier coverage per configuration
```

Method and results: [finding 18](findings/18-dsp-performance.md). Every run tool
writes `out/runs/<scenario-or-kind>/<UTC stamp>/` (report, audio, logs);
[docs/INSTRUMENTATION.md](INSTRUMENTATION.md) lists the output and the switches.
The ColdFire PC probe (`coupled-run --pc-probe`, RPC `pc.probe.set`): [INSTRUMENTATION.md](INSTRUMENTATION.md#pc-probe-coldfire).

## Analysis tools

For the DSP, `uv run python -m tools.sharc_dev.sharcdb build out/sections/*/section_7_BLOB.bin`
builds `out/sharcdb/<image>.sqlite`. `tools/sharc_dev/sharc.py` queries it: `sharc.load("dt2-1.17")`
gives `func`, `callers`, `callees`, `reach`, `last_def`, `refs`, `trace` and `sql`, and
`uv run python -m tools.sharc_dev.sharc dt2-1.17 "SQL"` runs one query. For the ColdFire OS, `tools/coldfire/ghidradump.py` exports a
Ghidra project, `tools/coldfire/cfdb.py build` makes a database from the dump, and `tools/coldfire/cf.py`
queries it. Decoding uses only public manuals ([docs/sharc/SOURCES.md](sharc/SOURCES.md)).
All tools are indexed in [docs/TOOLS.md](TOOLS.md).

Usage of the address map (`patchkit pack --addrmap`, `patchkit addr`, `wb addrmap lookup`, DSP names, load table snapshots): [ADDRMAP.md](ADDRMAP.md#usage).
Usage of the object compare for moves of runtime code (`patchkit objdiff OLD NEW`): [PATCHING.md](PATCHING.md#compare-two-objects-after-a-move).
Usage of the service binder (`patchkit bind IN.dks --firmware FW.syx -o OUT.dks`): [PATCHING.md](PATCHING.md#bind-a-service-to-your-firmware).
Usage of Transfer install and delete with services (`wb transfer install`, `wb transfer delete`): [MODULES.md](MODULES.md#install-with-services-from-the-command-line).

Owner hashes of the runtime object (`patchkit owners`, `objdiff --owners`, `modules/core/runtime/owners.toml`): [TESTING.md](TESTING.md#owner-hashes-patchkit-owners).
Usage of the SDK-surface coverage (`patchkit imports`, `patchkit coverage`, `patchkit:sdk-coverage`): [TESTING.md](TESTING.md#sdk-surface-coverage).
Usage of the module DSP analysis (`wb module analyse`): [packages/dksharc/README.md](../packages/dksharc/README.md#usage).
Usage of profiles (`wb profile report`, `wb profile record`, the `dk.profile/1` artifact): [INSTRUMENTATION.md](INSTRUMENTATION.md#profiles).

### ColdFire UI candidate review

After building both atlases and their catalogues, generate the complete inventories:

```sh
uv run python -m tools.firmware.catalog_page dt2-1.17 --all
uv run python -m tools.firmware.catalog_page dn2-1.12 --all
```

Open `out/atlas/dt2-1.17-review.html` or `out/atlas/dn2-1.12-review.html` in a browser. Keep both files together with these names for paired-device links. `--all` includes every catalogue row, including anchored functions, small groups and the ungrouped bucket. The page renders only the active group's filtered rows; search also covers tags, existing anchors and labelled SDK uses. An address links directly to its row (`#r-0x...`), revealing it through active filters. Pairs outside the other candidate inventory retain their address and matching evidence without a broken row link.

Each command also writes `out/atlas/<image>-review.tsv`, containing the **complete** catalogue, original evidence columns, flattened row/group labels and links to both review pages. This TSV is complete even without `--all`; omitting that option retains the curated visible selection. Curated pages keep omitted rows as lookup data so TSV and paired-device deep links can reveal any catalogue row on demand. `--labels FILE ...` replaces the discovered labels. Labels remain guesses [O] until checked against image bytes; stability, hash uniqueness, pair matches and rule agreement are shown separately.

Without Claude's database, Keep/Later/Skip uses local browser storage per firmware image. **Export decisions (JSONL)** saves `dt2-1.17-decisions.jsonl` or `dn2-1.12-decisions.jsonl`; **Import local decisions** merges a previous export for that image. Export before closing if browser storage is unavailable, and before moving or regenerating review files: storage for local files varies by browser. On a published Claude page, the existing database path remains in use; local imports are disabled there.

The **Bitmap candidates** and **Parameter candidates** shortcuts search the existing function names, callees and anchor tags. The catalogue does not contain glyph IDs, glyph previews or bitmap asset metadata, so these are entry points for investigation rather than an asset gallery.

## Patching

patchkit applies modules (new machines, parameters, master-bus effects) to your own stock `.syx` and writes a new
`.syx`. Modules hold our code only, never firmware bytes.

```sh
mise run patch:toolchains   # once: pinned ColdFire and SHARC+ toolchains for building modules
mise run patch -- --firmware firmware/Digitakt_II_OS1.17.syx --modules core/fx -o out/patch/fx.syx
mise run patch:inject -- out/patch/fx.syx   # into a running emulator (mise run workbench, or emu:serve)
pnpm dev                                # the gallery at /patch: patch in the browser, nothing is uploaded
```

**Writing a module** (create, build, patch, inject into the running emulator, test, and what modules can do):
[docs/MODULE-AUTHORING.md](MODULE-AUTHORING.md). Usage, the dev loop, tests, how it works and the safety rules:
[docs/PATCHING.md](PATCHING.md). Manifest reference: [modules/README.md](../modules/README.md). Facts behind it: [findings 01](findings/01-container-and-patching.md),
[02](findings/02-machines-and-parameters.md) and [21](findings/21-patchkit-core.md).

## Module loop

1. Create the module: `mise run module:new -- KIND AUTHOR/NAME [--dir DIR]` (`KIND` is `machine`, `fx` or `tool`). Guide: docs/MODULE-AUTHORING.md.
2. Build it: `mise run module:build -- AUTHOR/NAME [--check]` (`patchkit build`: ColdFire C, assembly and Rust, and SHARC+ with Selache as a library, all in the `patchkit` binary; an object whose sources did not change is skipped). The objects of a content module are build output in the cache `out/cache/dko/<module id>/` (not committed; `core/*` objects stay in their module); `mise run module:pack` and `dk:run` build what is stale themselves, so a module goes from source to `.dkm` in one command. `patchkit dko show FILE` prints what an object holds. `module:check` fails on a stale object.
3. Check it with the module owner's tests: `mise run module:check -- AUTHOR/NAME [--device dt2|dn2]` (machine modules run the conformance suite, then the module's own tests; it prints the packed size by section per device).
4. Run it in the emulator: `mise run dk:run -- fx/NAME --emu`. An emulator must already run (`mise run workbench` or `mise run emu:serve`). It builds, packs, uploads, selects the module, restarts the guest and follows the log.
5. Run it on the device: `mise run dk:run -- fx/NAME --port Digitakt`. The device needs the digikit runtime (version 0.1.0 or newer for the log levels): `mise run patch`, then `mise run patch:preflight` on the image before you flash it.

**A service** (`modules/core/services/NAME`): `mise run module:new -- service NAME` writes the layout of `sdk/templates/service`: the neutral crate
(`Cargo.toml`, `src/lib.rs`, `records.rs`, `slots.rs`, `tests/records.rs`; add `src/dsp/` when the service has DSP parts) and the service file `dks/` (`module.toml` with
id `core/services/NAME`, `src/lib.rs` with the `service!` call; add `src/hooks.rs` for hook assembler and `src/fw.rs` for firmware calls). It prints the steps that register it
(workspace member, `[[service]]` row and `[records.kinds]` in `sdk/sdk.toml`). `mise run module:check -- service/NAME` runs `patchkit service-check modules/core/services/NAME`
(the layout of both crates, the id and the role; the `[[hook]]` entries equal to the `service!` hooks per device; assembler only in `dks/src/hooks.rs`; firmware names only under `dks/`;
at most one exported symbol, `dk_module`; the record kinds that `Service::declare` makes equal to the `[records.kinds]` entries of the service in `sdk/sdk.toml`), then the crate's
`rust-tests`. Example, run in the repository root:

```text
$ patchkit service-check modules/core/services/lfo modules/core/services/fx
modules/core/services/lfo: ok
modules/core/services/fx: ok
```

Log levels: `mise run dk:log -- --port Digitakt level` prints them, `level debug` sets the default, `level --module NAME=trace` sets one module (`NAME=default` clears it), `follow --level L --module NAME` filters the display. Levels are not stored on the card.

After a change to `devices/*.toml` or `sdk/sdk.toml`, run `mise run gen:modules` (`-- --check` lists stale generated files).

Each task takes `-- --help`.

## Boot screens

A boot screen is a script `x.mjs` that exports `ticks` and `draw(ctx, t)` and draws with Canvas 2D (128x64). The agent skill `.agents/skills/digikit-bootscreen/SKILL.md` has the rules.

0. `mise run bootscreen` (no arguments): opens a gallery of `modules/tools/bootscreen/examples/*.mjs` in a browser. Click a tile to zoom it in; Esc closes. Tiles update when any file in the folder changes; `--no-open` skips the browser.
1. `mise run bootscreen -- dev x.mjs`: packs the script and opens a browser preview that reloads when the file changes.
2. `mise run bootscreen -- sheet x.mjs`: writes a contact sheet PNG of up to 24 ticks to `out/bootscreens/` (a way to see the result without a browser).
3. `mise run bootscreen -- put x.mjs [--port Digitakt]`: packs the script and uploads it to `data/tools/bootscreen/x.dkbs` on the device (`wb store put`, verified after the upload).
4. Or open Workbench, Transfer, Modules, tick `bootscreen`: the "Boot screens" area chooses or takes `.dkbs` files, previews them without a device, and sends them (the module too, when it is not on the device).

The device applies a file only after you choose it in the module's settings on the device and restart.

## Fast iteration: runtime, UI and modules

One edit and one look take seconds, not a boot. Iterate on the Digitone II (its boot is about half as long), check the Digitakt II at the end, and finish with the owner's check (`module:check` for a content module, the named Nx device target for a fixture).

### Visible preview

From the repository root after [setup](#setup), with the [DN2 firmware](#firmware-files) available:

```sh
mise run dk:menu       # the real SETTINGS > DIGIKIT menu
mise run dk:ui-demo    # the SDK UI controls fixture
mise run dk:module pluck  # zero-DSP machine UI preview, selected on its card
mise run dk:module pluck --dsp  # full module with SHARC audio
```

Each command starts or reloads a kept DN2 emulator, navigates to SETTINGS > DIGIKIT, and opens a local browser faceplate with its live display and controls. The command prints the exact panel URL. `dk:menu` uses the `emulator` patch set, including `core/digikit-menu`; `dk:ui-demo` uses the separate `test/ui-demo` fixture, which requires `core/ui` and conflicts with the real menu. These two previews automatically use zero DSP, so they have no audio.

`dk:module` accepts a full module id or a unique leaf alias (for example, `pluck`). It packs a card `.dkm`, saves its selection for boot, and uses the `emulator` runtime patch set. By default, a declarative machine (including pluck) uses a data-only UI preview pack and zero DSP, with no audio. Machines declaring a sample bank or sample tracks automatically use the full module with DSP, preserving sample loading and picker callbacks. The UI preview pack rejects actions, samples and hooks. ColdFire-only services use normal packing and zero DSP. Effects and DSP-backed services require `--dsp`, which packs the full module and selects SHARC when needed. Its dependencies must already be provided by that patch set. It opens the DIGIKIT MODULES list; use the panel controls to reach the module's machine or effect page. Add `--device dt2` for Digitakt II (with its firmware available); DN2 is the default.

On each invocation, `dk:module` delegates to the generic `dk:dev --store` path. It stages content and compares the exact store request and initial selection with the kept daemon. Unchanged content keeps the daemon and overlay, including any selection the user changed while it was running; changed content rebuilds the card and restarts that exact daemon. To restore the initial selected card after changing selection live, stop and restart the daemon. `mise run dk:module ID --stop` stops only that module's kept daemon for the chosen device, without packing or opening the browser.

Keep the command running while using the panel. Ctrl-C stops the Vite browser server; the emulator stays alive for the next reload. Stop it with `mise run dk:dev -- emulator --stop` or `mise run dk:dev -- test/ui-demo --stop`, respectively. For the controls and acceptance test, see the [UI demo README](../tests/fixtures/modules/ui-demo/README.md).

### A module or fixture of your own

`dk:dev` accepts a module ID (including a test fixture) or a device patch set such as `emulator`. It builds and reloads that target, runs a key script and writes screenshots; it does not open a browser. Choose `--device dn2` or `--device dt2`, or both as below:

```sh
mise run dk:dev -- AUTHOR/NAME --device dn2 --keys 'shot:main' # your module
mise run dk:dev -- test/ui-demo --device dn2 --keys 'tap:NO,tap:SETTINGS,wait:400,tap:DOWN*8,tap:YES,wait:600,shot:menu'
mise run dk:dev -- emulator --device dn2,dt2 --keys FILE.json   # the workbench's module set of devices/*.toml
mise run dk:dev -- core/digikit-menu --store 'test/dyn-demo*,synth:HUGE:8000000'  # a card with these modules (* = selected)
mise run dk:dev -- test/ui-demo --stop                          # end the kept daemon
cargo test --offline -p dkui-host                               # menu and UI logic on the host, about 1 s
```

For an interactive content module in an already running workbench or emulator, use `mise run dk:run -- AUTHOR/NAME --emu` ([module loop](#module-loop)). Finish with `mise run module:check AUTHOR/NAME`; for the UI fixture, use `pnpm nx:pilot run sdk-ui-zero-tests:test-device`. A preview or screenshot is not an acceptance check.

`dk:dev` builds what changed, reloads a daemon it keeps per device and module set, runs the keys from the main screen and writes `out/runs/devreload/<device>/sheet.png`. The reload (daemon method `dev.reload`) compares the new patched image with the running one:

- Only UI-layer modules changed (`core/ui`, `core/digikit-menu`, modules that require `core/ui`): it restores the main screen and writes the new code. About 1.5 s, or 4 s for a `dk:dev` round with a short key script.
- Anything else changed (the Rust runtime, another module, the DSP image): it boots the new image. Cold: DN2 5 s, DT2 10 s; an image booted before: under 1 s.

In the workbench (`mise run wb`) the **Reload modules** button (⇧⌘R) builds and reloads the open session: it calls `dev.reload {build: true}`, which builds the stale objects (ColdFire C, assembly, Rust; SHARC+ assembly) of the session's modules in the app, then does the reload above. A C edit takes about 0.25 s to build and 2.5 s to reload (DN2). A compiler error shows in the status line (short) and in full in the red box under the top bar; nothing is swapped. A SHARC+ source builds there too (Selache as a library, about 5 ms for an object); an assembler error or a refused encoding shows the same way. `dk:dev` keeps its own build call (`patchkit build`) and reloads without `build`. A watch task that rebuilds on save is still possible ([docs/plans/sdk-api.md](plans/sdk-api.md), "Dev loop").

Build times: the Rust runtime 2.3 s, a C object under 1 s. `uv run python -m tools.emulator.boot_phases --device dn2 --cold` measures where a boot spends its time. Details and timings: [TESTING.md, "Fast iteration"](TESTING.md#fast-iteration-dkdev-and-reload); the daemon methods: [CONTROL.md](CONTROL.md).

**Hardware warning**

- Patched images have run on a Digitakt II (OS 1.17) since 2026-10-18 (docs/findings/47-dt2-device-rungs.md). Flash
  only images that pass `mise run patch:preflight`.
- Never change section 2 (bootstrap), section 4 (updater) or section 8. patchkit refuses.
- Recovery is the bootstrap's STARTUP menu (hold FUNC at power-on, then the stock OS over MIDI DIN); it has not been rehearsed.
- Read `docs/findings/01-container-and-patching.md` before you install anything.

## Repository layout

```
firmware/          stock firmware images you supply (ignored; see firmware/README.md)
apps/              desktop (Tauri) and web shells (browser panel: Astro + Solid)
packages/emulator/ Rust workspace: runtime, ColdFire and SHARC execution, runners
packages/patchkit/ patch toolchain: port lockfiles, linker, apply, verify (CLI and wasm)
packages/*         TypeScript control client and test support
tools/             Python tools, one package per owner (tools/<owner>/, docs/LAYOUT.md)
tests/             pytest suite (native tests live in the crates); see docs/TESTING.md
tests/scenarios/   scripted panel inputs and pinned PCM hashes
modules/           patchkit modules
devices/           device profiles (firmware hashes, panel layout), LED maps, and ports/
docs/              findings, designs, handovers
out/               everything generated locally (ignored): builds, runs, caches, out/sections/<image>, out/snapshots
```

Who owns what, and planned moves: [docs/LAYOUT.md](LAYOUT.md).

## Documentation map

- [docs/FINDINGS.md](FINDINGS.md): the results, one file per topic in
  `docs/findings/`, plus an index of the other result documents.
- [docs/handovers/](handovers/): dated handovers with state and next steps. Read
  the newest to continue the work.
- [docs/design/](design/), [docs/plans/](plans/): designs and plans.
  [docs/archive/](archive/): superseded documents, kept for history.
  [packages/emulator/README.md](../packages/emulator/README.md): crate map and the coupled runner.

Record results in `docs/findings/`, not in handovers. Marks: **[V]** verified here,
**[D]** documented or read once, **[O]** open, **[C]** corrects an earlier claim.

## Limitations

- The DSP does not run in real time yet ([finding 18](findings/18-dsp-performance.md)).
- The browser DSP is slow; the browser host is parked on the branch `wip/browser-host`.
- The control socket is a Unix socket ([docs/design/emulator-control.md](design/emulator-control.md)).
- Some scenario PCM pins need the private ready inputs ([tests/scenarios/README.md](../tests/scenarios/README.md)).

Contributing: [CONTRIBUTING.md](../CONTRIBUTING.md).
