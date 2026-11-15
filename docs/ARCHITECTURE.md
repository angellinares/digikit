# Architecture

This file is the reference for the layers (L0 to L4), what each layer may depend on, the interfaces between the runtime, services and modules, and how a new device or firmware version fits. `tools/devtools/layercheck.py` enforces the rules it can check today (`mise run check:layers`).

## Layers

Five layers, L0 to L4. Calls go down. The diagram is the target; "today and target" below says which boxes exist now.

```
 L4  Content modules (.dkm, loaded from the card)          apps
     machines: pluck, shot, analog, bd …                   digikit-menu (runtime UI)
     fx: freqshift …   tools: bootscreen, debug
        │ A: sdk.toml imports by service group,                │ dkrt_* runtime API
        │    dk_<service>_open handles                         │
 ───────┼──────────────────────────────────────────────────────┼──────────
 L3  Services (in the runtime crate now; .dks at R12)          │
     src/services/machines   machine/dp/page/voice rows, keys, │
                             native pages, page key/turn slot  │
     src/services/fx         FX pages, sends, FX param/send/   │
                             effect rows, FX DSP parts         │
     src/services/samples    tracks, peak windows, sample-     │
                             pool exports, track-table DSP     │
     each: mod.rs · records.rs · slots.rs · dsp/ · tests/      │
        │ C: sdk.toml imports        ▲ B: only declared:       │
        │    + service-only set      │    lifecycle, record-kind handlers,
        ▼                            │    slots/hooks, resources, sinks
 ───────────────────────────────────────────────────────────────┴──────────
 L2  Runtime core (kind-free: never names a machine, effect or service)
     state      State, statics, imports, C API (api.rs), mailbox
     boot       selection, safe mode, boot plan (dependencies, fit, refusals)
     loading    loader, collect (relocate, commit records),
                registry walker + record-kind handler table, params (param-table writer)
     dsp core   DSP area allocator, dsp-link + windows, row table,
                DSP load state machine   (R6a: its transfers go through the link queue)
     storage    store, files, card, scan, arena, worker, settings (broker)
     transport  SysEx/USB: transport, store/admin/module commands
     diagnostics dlog, sinks log/span (R2d), fault (crash), watch, invariants,
                health, report (status, capacity, diag)
     tick       scheduler: service ticks, poll table
        │ platform calls only
 ───────┼─────────────────────────────────────────────────────────────────
 L1  Platform and firmware adapters (in the patched image)
     platform glue     st, flush, block timing, MIDI send, raw memory
                       (platform.rs: RealPlatform behind the Platform trait; the
                       hooks are in the platform adapter, core/platform)
     patched adapters  ColdFire hooks and *_info tables: platform (UI loop,
                       main-OS entry, MIDI receive, exception hook), dynreg, dyndsp,
                       dynfx, fxpages, ui, input, health, dsp-link …
                       SHARC hook stubs with pointer slots (finding 128)
     device profiles / anchors  per-device and per-firmware addresses
 ───────┼─────────────────────────────────────────────────────────────────
 L0  Stock firmware and hardware: ColdFire OS, SHARC DSP image, card, USB
```

Rules between layers:

- Calls go down only.
- The one upward path is L2 to L3, through what services declared (interface B). The runtime resolves it at boot planning from a static table that `module:check` writes. It is derived and declarative, the same as S3 for modules: the service's `init` declares through builder calls, and the table is written from them.
- L3 never calls L3 directly. If a service needs another, it imports the other's exports through `sdk.toml` (interface A) and declares the dependency. The boot plan orders them and refuses the first if the second is missing.
- L4 modules reach services only through `sdk.toml` imports. The digikit menu is an app on the runtime's C API.
- Everything that depends on the device or the firmware version sits in L1: adapters, device profiles and anchors. It is not in the SDK, in L2, L3 or in modules.

Three interfaces (details in [runtime and services](plans/runtime-and-services.md) section 5.12):

- **A, module to service:** `sdk.toml` imports by service group and `dk_<service>_open` handles.
- **B, runtime to service:** only what a service declared: lifecycle, record-kind handlers, slots and hooks, resources, and the log and span sinks.
- **C, service to runtime:** the same `sdk.toml` imports as modules, plus a small service-only set.

The interfaces are Rust traits in the crate `dkrt-abi` (`modules/core/runtime/abi`): `CoreApi` (C), `Service` and `ServiceList` (B), with the pure types and constants they share (`DspState`, `RegState`, the DSP section types, the row and word constants, memory helpers). The core implements `CoreApi` as `RealCore` and services as `Services`, the static list in `handlers.rs`. A service never calls another service and never names a core module: it calls the core through `crate::core_api` (`REAL_CORE`, or a generic `C: CoreApi` where a host test drives it), and the core calls a service only through `Services` or the C entry points listed in `handlers::exports`.

The crossings of the R2b code, and what R2c made of them:

| R2b crossing | Now |
| --- | --- |
| `tracks` checks `fxpage` and `fxsend` busy; `fx_live` checks `tracks::busy` | The DSP link queue of the core (R6a, `link.rs`): live values with `link_set`, owner requests with `link_submit`. No writer asks whether the link is busy |
| `fx` needs `machines` rows | Both use `CoreApi` rows (`reg_state`, `dynreg`, `write_param_row`, `make_rep`) and no function of the other |
| `pageclick` reads registry rows | `CoreApi::param_first`, `param_owners` |
| `fxsend` uses `settings` | `CoreApi::settings_get` and `settings_put` (a broker resource) |
| `assign` (sticky type numbers) in `machines` | Back in the core (`assign.rs`): it is kind-free. Services call `CoreApi::type_resolve` |
| core calls of `assign`, `fxpage::tick`, `pageclick::tick`, `tracks::tick`, `fxsend::fx_entry`, the nativepage kinds | `Service::tick(phase)`, `handlers::exports`, `dkrt_abi::registry::KIND_NATIVE*` |

### Today and target

Today the runtime is one crate with the L2 core and the L3 services in separate folders (`src/services/NAME`), joined by the interfaces of the crate `dkrt-abi`. The adapters, the app and the installer are separate patched-in modules. The target is reached in steps:

- **R2b** moved files into L2 and L3 folders inside the runtime crate.
- **R2c** is done: the interfaces exist with fakes (`FakeCore`, `FakeService`), the shims of R2b are gone, and `check:layers` enforces the arrows (rule 6). What Cargo enforces: `dkrt-abi` cannot see the runtime. What only `check:layers` enforces: that services see only `core_api`. A separate service crate reaches `RealCore` through a type parameter `C: StaticCore` (R9). Exceptions left for R3b and later: the optional `fxpages_info`, `dkpool_*` and `dkpw_*` symbols of the adapters are still declared where they are used; the DSP row table (`DspState`: voice, effect and track rows) is service data inside a core-owned object, because `DKRT_DSP` is read by host tools (`DKRT_REG` likewise); the walkers of `registry.rs` and `dsp/mod.rs` still hold the machine-only tables (`counts`, `types_of`, `free_rows`, `profile`) that R9 and R10 move.
- **R3a** is done: the `Platform` trait (`dkrt-abi`: device profile, block clock, interrupt mask, flush, card block calls, MIDI, OS upgrade flag, soft reset, with `Tasking`, `SamplePool` and `StagedWriter`) and `RealPlatform`, a zero-sized type in `platform.rs`. `platform.rs` is the only runtime file that names `dkfw`, besides an adapter's own `fw.rs` (the machines service's `paramapi::Fw` implementation). The runtime branches on no device id: the device files give `[capabilities] card` and `[digikit] sample_path_max`, which `patchkit gen` writes into the device objects and into the `DeviceProfile` constants of `abi/src/profiles.rs`. `FakePlatform` drives the host tests, and the contract table `modules/core/runtime/tests/platform/contract.toml` is checked against it and by `platform-contract.device.test.ts` on the device (rule 7). The device objects gain two data words, so R3a is gated together with R3b.
- **R3b** is done: the hooks `cf.view.hook.ui_loop`, `cf.rtos.hook.main_os_task` and `cf.midi.hook.rx`, their trampolines and the exception hook install (`dkplat_exc_install`, which `Platform::install_exception_hook` calls every UI pass) are the L1 adapter `core/platform` (`modules/core/adapters/platform`). Same hook sites, the same stock-byte checks and the same trampoline code as before; the runtime requires the adapter, and the two link by name (the adapter calls `dkrt_tick`, `dkrt_boot`, `dkrt_midi_rx` and `dkrt_exc`), so every image with the runtime has it and `requires` has no cycle. The runtime module keeps only the machine page hooks (finding 128: runtime-owned, with slots); the hooks that must exist before the main OS starts stay patched in the image. Rule 7 checks it.
- **R7** is done: the record kinds, resources and the in-crate services are one table generated from `sdk/sdk.toml` (`gen_services.rs`, `records::`); the core walks a `ServiceTable` of `ServiceRow` function pointers, not a tuple of types; a module emits a record of a generic kind with `dk_record_add`; the `.dkm` header carries a tagged table (needs, uses, keys, and the entries R12 needs); a pure boot planner (`plan.rs`) returns one `Verdict` for each module before anything loads; a service declares its kinds and the runtime compares them with its table. The `lfo` service (`src/services/lfo/`) is the proof: a new kind with no edit to the registry, the collector, the loader, dkneeds or patchkit. What is still target: services loaded from `.dks` files, a kind or hook that a module provides (R12), and the module states and their UI on the verdict (R11).
- **R9** is built: the `fx` service is the crate `dkrt-svc-fx` (`modules/core/services/fx`), linked into the runtime object. Its functions are generic over `StaticCore`; the service table names `Fx<RealCore>` and `handlers::exports::fx_entry` calls `fx_entry::<RealCore>`. `CoreApi` gained `device_id`. The link queue is the crate `dkrt-link` and `FakeCore` is in `dkrt-test-support` (`modules/core/runtime/{link,test-support}`), so the service crate tests without the runtime. The code is the same up to inlining across the crate boundary (`lto = false`): text +112 bytes (+0.05%).
- **R10 to R12** make the L3 services real modules (`.dks`). The movable ColdFire hooks go from L1 into their services, and the runtime shrinks to L2.

`core/expansion` is the reset-time installer: it runs at reset and copies all patched-in code to its place. `modules/core/fw` holds the generated firmware bindings (a library, not a module).

## Boundaries and tests

This section is the target. Interface B exists as the `Service` trait and its static table (`ServiceTable`, generated into `gen_services.rs`; `handlers.rs` hands it out), with `FakeService` in `handlers/tests.rs`. C is `CoreApi` in `dkrt-abi`, with `RealCore` and `FakeCore` in `core_api.rs`, and the Platform interface is `Platform` in `dkrt-abi`, with `RealPlatform` and `FakePlatform` (`fake_platform.rs`). After R3, test-speed item 7 moves logic tests from the device to the host. The rule for review of deleted tests applies to each move: a host test replaces a device test only when it covers the same path.

Each layer has entry points (what the layer above calls) and exit points (what the layer calls), and is tested at those points.

| Layer | Entry points | Exit points | Tested how |
| --- | --- | --- | --- |
| L1 platform | The Platform interface: time and blocks, flush, MIDI send, raw memory, the `*_info` tables | Stock firmware | A contract suite, run once per device and firmware in the emulator. Everything above uses a `FakePlatform` |
| L2 core | The C API `dkrt_*`, the boot and tick entries, the static service tables | Platform, the interface B handler tables, the DSP link | Host unit tests with `FakePlatform` and `FakeService` (it records handler calls): loader, boot plan, fit, refusals, registry walker, later the R6 queue |
| L3 services | The interface B handlers | Interface C, a `CoreApi`: params, rows, link, settings, log | Host unit tests of each service against a `FakeCore` (crate `dkrt-test-support`: it records row writes and runs the real link queue of `dkrt-link` over a model of the link) |
| L4 modules | The `dk_module` lifecycle and the kind contract | `sdk.toml` imports | Conformance tests, and module host tests against fake services |

Rules:

- Parts inside a layer depend on small interfaces, not on each other's internals, so each part has its own unit tests.
- Each interface has one contract test suite. It runs against the fake and against the real implementation, so the fake cannot drift.
- Target pyramid: many host tests per layer, one contract suite per boundary (the only emulator part, once per device), and a small set of end-to-end device scenarios.

## Roles

| Layer | Folder | `role` | Runs | May depend on |
| --- | --- | --- | --- | --- |
| L2 Runtime core | `modules/core/runtime` | `runtime` | At boot and in the UI and block loops | Firmware bindings, SDK crate `dkrt-sdk`, interface crate `dkrt-abi`, adapters through optional symbols |
| L3 Service | `modules/core/services/NAME`, a crate linked into the runtime object (`fx`; `src/services/NAME` in the runtime crate for the others until R10) | `service` (planned) | Loaded when a selected module needs it | The core through `CoreApi` (interface C) and `dkrt-abi`, other services through imports (interface A) |
| L1 Installer | `modules/core/expansion` | `installer` | At reset, once | Firmware bindings |
| L1 Firmware bindings | `modules/core/fw` | none (not a module) | Never; generated by `patchkit gen` from `devices/fw/` | Anchors |
| L1 Adapter | `modules/core/adapters/NAME` (`platform`: the runtime's hooks) | `adapter` | Inside firmware hooks, in the firmware's contexts | Firmware bindings, runtime exports, other adapters' headers |
| L4 App | `modules/core/apps/NAME` | `app` | In the task context, called by an adapter | SDK, `dkrt.h`, `dk_ui_core.h` |
| L4 Content module | `modules/machines`, `fx`, `tools`, `tests/fixtures/modules` | none | Loaded at run time from the card (a `.dkm`) | SDK only |
| SDK contract | `sdk/` | none | Not code that runs; the contract between L4 and L2 and L3 | Nothing |

## Rules

`layercheck` reads manifests, sources and built objects. It builds nothing; for the objects of content modules it asks patchkit where the objects of this tree's sources are stored (`patchkit build --paths`, about 10 s), so a cache entry another worktree built is never read.

1. **Roles.** Every module under `modules/core` has `role` in `[module]`, and the role fits the folder: `adapters/` is `adapter`, `apps/` is `app`, `runtime/` is `runtime`, `expansion/` is `installer`. Roles map to layers as in the table above.
2. **Sources.** Content modules and apps include only their own files and the SDK (`<dk/...>`, `sdk/dsp/contract`). An app may also include `dkrt.h` and `dk_ui_core.h`. Only the runtime (its `platform.rs`, rule 7), the installer and adapters include `dk_fw_*` headers or depend on the `dkfw` crate. C, ColdFire and SHARC assembly (`#include`, `.include`) and Rust (`dkfw`, path dependencies) are checked.
3. **Objects.** The undefined symbols of the built objects of apps and content modules are SDK imports (`sdk/sdk.toml` `[[import]]`), `dkrt_*`, `dk_ui_*`, `dk_app_*` or the SHARC externs the SDK documents. They never name a firmware anchor (`cf.*`, `dsp.*`, `hook.*`, `pk.*`), and `[object.externs]` never maps a symbol to one.
4. **Apps do not patch.** An app has no `[[op]]` and no `[[claim]]`. If an app needs a firmware hook, the adapter that owns that plug-in point installs the hook and calls the app through an optional symbol (`dk_app_*`).
5. **Capabilities cost nothing unused.** A module asks for capabilities; adapters implement them per device. Prefer data (a field an adapter already passes) to a new firmware hook. A hook must cost at most one flag check on a hot path when no loaded module asks for its capability ([capabilities plan](plans/capabilities.md)).

6. **Arrows inside the runtime.** A service (`src/services/NAME`) names no other service folder and no core module except `crate::core_api`; pure types come from `dkrt-abi`. A service crate (`modules/core/services/NAME`) depends only on `dkrt-abi`, `dkrt-sdk` and `dkneeds`, and on `dkrt-test-support` as a dev-dependency; it takes the core as a type parameter `C: StaticCore` (a zero-sized core) or as a `&impl CoreApi`. The core names a service only in `handlers.rs` and the generated `gen_services.rs`.

7. **Platform.** The runtime module installs no firmware hook except the machine page hooks (`cf.page.hook.source_key`, `source_turn`); the rest are in the platform adapter. Only `platform.rs` of the runtime crate and an adapter's `fw.rs` name the firmware crate `dkfw`; the rest of the core and the services use the `Platform` trait. No runtime file but `platform.rs` compares the device id with a number: it asks the device profile (`Platform::samples_in_store`, `sample_path_max`), whose values come from the device files.

Today `check:layers` enforces these seven rules: roles, which sources and objects each role may use (so L4 never reaches firmware names and L1 is the only layer with `dk_fw_*`), no patching by apps, the capability cost, the arrows between L2 and L3 (including the dependencies and sources of the service crates), and the platform boundary of the runtime.

Exceptions are listed in `layercheck.py` with a reason: `test/registry-demo` patches the firmware itself (it is patchkit's example of a hook), and `test/ui-demo` stands in for an app.

Run it:

```sh
mise run check:layers          # or: uv run python -m tools.devtools.layercheck
```

Each violation prints `file:line: rule: what is wrong. Instead: what to do.` The gate runs it in `python-checks` (with ruff and mypy) whenever modules, the SDK or the firmware declarations change; `mise run test:changed` runs it too.

## How a new device or firmware version fits

A new firmware version of a known device:

1. Add the anchors to `devices/ports/` (they resolve by hash) and check the declarations in `devices/fw/`.
2. Run `patchkit gen`. The bindings in `modules/core/fw` follow.
3. Fix what changed in the adapters. The adapters hide the difference; no SDK header, no content module and no app changes.

A new device:

1. Add the device profile (`devices/NAME.toml`): limits, patch sets, panel, storage and the values the adapters read (`docs/design/device-data.md`).
2. Add its ports and, where its firmware differs, device-specific adapter sources (`src/cf/NAME/`, `src/dsp/NAME.s`) behind the same adapter API.
3. Keep the SDK contract. A content module that builds against the SDK loads on the new device when the runtime reports the contract version.

## Where the device specifics live

| What | Where |
| --- | --- |
| Firmware addresses and hook sites | `devices/ports/` (anchors by hash) |
| Firmware function and structure declarations | `devices/fw/`, generated into `modules/core/fw` |
| Limits, patch sets, panel, storage | `devices/*.toml` (device profiles) |
| Code that hooks or wraps firmware | `modules/core/adapters/*` |
| The module contract | `sdk/` (device-neutral) |

## Worked example: the UI path

A menu or screen of an app or module goes through these steps:

1. **Module.** A module calls `dk_ui_menu_new`, `dk_ui_text`, `dk_ui_open` and the other calls of `<dk/platform/ui.h>` (`sdk/include/dk/platform/ui.h`). It knows no firmware.
2. **Runtime.** The runtime links the module's imports when it loads the `.dkm`. The `dk_ui_*` calls are `[[import]]` entries of `sdk/sdk.toml` with `requires = "core/ui"`: the runtime's object names them as `optional` symbols, and patchkit resolves them to the functions of `core/ui`, or to 0 when `core/ui` is not in the image. For 0 the runtime hands out a stub that returns the API's failure value (the entry's `absent`: null, -1 or 0, or nothing for a void call), so a module that uses the UI still loads. An app (a core module) links the same symbols directly through patchkit.
3. **Adapter `core/ui`.** `modules/core/adapters/ui` implements the calls. It builds the firmware's own views (`VerticalMenuView`, list and text views) through the generated bindings (`dk_fw_view.h`, `dk_fw_menu.h`) and keeps their state.
4. **Firmware views.** The firmware draws and handles the keys.

The SETTINGS entry shows the rule for hooks. `core/ui` owns the hook at the end of the SETTINGS list rebuild (`cf.menu.hook.settings_list.tail`). It calls the optional symbol `dk_app_settings_rows(settings_menu)` (declared in `dk_ui_core.h`). The app `core/digikit-menu` implements it with `dk_ui_stock_row` and adds the DIGIKIT row. The app has no hook, no claim and no firmware include. When no app is in the image, patchkit resolves the symbol to 0 and the hook does nothing.

A module with settings sets the `menu` field of its `dk_module` (`void menu(dk_module_ctx *ctx, dk_ui_menu *menu)`; the `.dkm` header carries its address and `DkrtModule.menu` and `DkrtModule.ctx` give it to the app). The MODULES list of `core/digikit-menu` shows SETTINGS... in the side panel of a loaded module that has one, builds a menu once for it (titled with the module's name) and calls `menu` to fill it only when the user opens it. The check mark of the row stays the only on/off of a module; the runtime owns it, and the module owns only its own settings.

**Lifetime rule.** No module is unloaded at run time, and menus are never freed. So every `ctx`, callback and descriptor that a module hands to `<dk/platform/ui.h>` must point at the module's own static data or code, never at the stack, and menus are built when the user opens them, after all modules are loaded. Then no UI pointer outlives the module that owns it.

The same holds for card reads: the INFO page of the menu gets the module header from `dkrt_module_header` of the runtime, which does the card read in its own buffer.

## See also

- [Repository layout](LAYOUT.md), [testing](TESTING.md)
- [Modules: what they are and why](MODULES.md), [module reference](../modules/README.md), [SDK](../sdk/README.md)
- [Firmware declarations](design/firmware-decls.md), [device data](design/device-data.md), [runtime](design/digikit-runtime.md), [boot screen format](design/dkbs.md)
