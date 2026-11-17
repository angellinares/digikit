# Runtime slimming: the core loads modules, keeps the card safe and reports faults

Status: not started. Roadmap rows: "RS-S storage restructure and samples.dks" and "RS-C machines and fx leftovers out of the core" in [sdk-runtime-roadmap.md](sdk-runtime-roadmap.md).

Inputs: [runtime-and-services.md](runtime-and-services.md) (sections 3, 13, 13.2), [storage.md](storage.md) (store format v3), [digikit-hot-reload.md](digikit-hot-reload.md), [test-architecture.md](test-architecture.md) (T2 test pyramid), [docs/ARCHITECTURE.md](../ARCHITECTURE.md). Paths below are relative to `modules/core/runtime/src/rs/src/` unless they are given another way.

## Goal

The runtime (`core/runtime`, patched into the image) holds only:

- module and service load and unload (discover, plan, link, install hooks, lifecycle);
- crash and hang capture, safe mode and the safe screen;
- the runtime menu API;
- SysEx transport (upload, store commands, INFO, MODULES, SELECT and the rest) and settings;
- one device-agnostic storage layer, with the device-specific routes behind it.

Everything else is a service file (`services/*.dks`) or an image adapter. Image size is not the goal; one owner per concern is.

## State before this plan

- `machines`, `fx` and `lfo` are service files (R12 services as `.dks`). `samples` is the last service linked into the image (`gen_services.rs`, `STATIC = 1`).
- The core still holds machines and fx code that the services call through `CoreApi`: about 1,000 of the 1,443 lines of `collect.rs` (builder state, lowering, commit checks), the registry tables of `registry.rs`, `params.rs`, `assign.rs`, `native.rs`, the guard logging of `watch.rs`, about 15 of the 38 `CoreApi` methods, `dkrt_fx` and `machine_types_of`.
- The core names services and adapters: `gen_services::ORDER` (including a test service), the C entry points in `handlers.rs`, `dyndsp_info`, `dynfx_info`, `dynreg_info`, `mf_native_*`, `dkpool_*`. The rule "L2 is kind-free" in ARCHITECTURE.md does not hold yet.
- Storage has cycles (store and store_writer, store and card, card and platform), four listers of the same table and three places that decide the root and route (`storage_adapter.rs`, `store_writer.rs`, samples `tracks.rs`). The DT2 +Drive write route sits inside `store_writer.rs` (2,260 lines).

## Target architecture

```
 ┌─ on the card ────────────────────────────────────────────────────────────────┐
 │  modules (.dkm): shot, bd, pluck, effects, tools, bootscreen ...             │
 │       │ builder calls (dk_machine_*, dk_page_*, dk_fx_*), dk_sample_pool_*   │
 │       │ → resolved to the services' EXPORTS at load                          │
 │       ▼                                                                      │
 │  services (services/*.dks), loaded at boot only when a module needs them:    │
 │   machines.dks        fx.dks        lfo.dks        samples.dks               │
 │   (collect, registry, (fx rows,                    (sample tracks, pool,     │
 │    params crate,       fx pages,                    worker job queue)        │
 │    sticky types,       params crate)                                         │
 │    native pages)                                                             │
 └───────────────┬──────────────────────────────────────────────────────────────┘
                 │ interface C: CoreApi (about 24 methods: log, settings, info(hash),
                 │   storage stat, link, module_of, irq)
                 │ interface B: service rows (module begin / commit / remove, tick,
                 │   DSP admit / claim / release)
 ┌───────────────▼─ RUNTIME (core/runtime, in the patched image) ───────────────┐
 │ L5  tick.rs: order of work per UI pass · entry points: boot, tick, MIDI, exc │
 │ L4  transport/  SysEx: upload, store commands, INFO, MODULES, SELECT ...     │
 │     menu/       dkrt_* API for the runtime menu, module list with fit        │
 │ L3  boot/       selection record, boot counter, safe mode, safe screen       │
 │     fault/      crash and hang capture, DSP heartbeat, health, event log     │
 │ L2  loader/     discover, plan, link imports/exports, install hooks,         │
 │                 load/unload lifecycle, records walk, declare check           │
 │     dsp/        DSP area, section relocation, DSP load state machine         │
 │     iface/      CoreApi and the service-row walker (names no service)        │
 │ L1  storage/    see "Storage layer"                                          │
 │     files/      module file-browsing API (dk_files_*), kept in the core      │
 │     settings/   8 KiB key/value record                                       │
 │ L0  state, arena, dlog, platform/ (RealPlatform: the only code naming dkfw)  │
 │                                     calls go down only; no cycles            │
 └───────────────┬──────────────────────────────────────────────────────────────┘
                 │ Platform trait · info(hash) tables · pointer slots
 ┌───────────────▼─ image adapters (patched in, device-specific) ───────────────┐
 │ core/platform (boot, tick, MIDI, exception hooks; device words)              │
 │ dsp-link · sample-pool · plusdrive-write · dynreg · dyndsp · dynfx           │
 │ machines (registry rows, native pages) · fxpages · params ...                │
 └───────────────┬──────────────────────────────────────────────────────────────┘
                 │ firmware addresses: names from devices/fw/*.toml, anchors per
                 │ version from devices/ports/, device constants from devices/*.toml
                 │ (patchkit gen; patchkit bind for service files)
 ┌───────────────▼──────────────────────────────────────────────────────────────┐
 │ stock firmware: Digitakt II 1.17 / Digitone II 1.12 (ColdFire and SHARC)     │
 └──────────────────────────────────────────────────────────────────────────────┘
```

### Core modules

| Directory | Owner | What it does |
| --- | --- | --- |
| `lib.rs`, `tick.rs` | core | Crate root and panic handler. `tick.rs` is the composition root: the order of work in each UI pass. |
| `state/` (state, arena, mailbox) | core | Core statics, the module arena, the mailbox select. |
| `iface/` (core_api, svc_imports, handlers) | core | Interface C (`RealCore`, the `dkc_*` shims) and the interface B walker over the service rows. It names no service. |
| `loader/` | load and unload | Module and service lifecycle (stage, check, place, init, commit, undo), boot plan (`boot_plan.rs`, `plan.rs`), imports and exports, the hook installer, the generic `dk_record_add` walker, `decl_matches` and the keys copy (`keys.rs`, `records_walk.rs`), service rows. |
| `dsp/` (with link.rs) | load and unload | The DSP half of a module: area allocator, section check and relocation, link queue, load state machine. After R12-8 SHARC stubs it reads no voice or fx layout. |
| `boot/` (boot, selection, safe_mode, safe_screen) | safe mode | Boot entry, selection record, boot counter and safe key, safe screen. |
| `fault/` (fault, watch, health, invariants, dlog, report) | crash | Crash and hang capture, DSP stall heartbeat, health, generic table invariants, event log, status and capacity reports. |
| `settings/` | settings | The key/value record. |
| `files/` (files.rs and the SDK broker) | module file API | `dk_files_open/list/poll/cancel` for content modules (shot's picker, bootscreen). Kept in the core; a later move to a service is possible but not planned. |
| `menu/` (api, scan) | menu | The `dkrt_*` C API of the menu app and the module list with fit. |
| `transport/` (transport, admin, module and store commands) | transport | SysEx entry point and command handlers. |
| `storage/` | storage | See "Storage layer". |
| `platform/` (platform.rs, io, aux_task) | platform | `RealPlatform` (the only file that names `dkfw`) and the runtime I/O and helper tasks. |

Levels (calls go down only): 5 `tick` and the `dkrt_*` entry points; 4 transport, menu; 3 boot, fault; 2 loader, dsp, iface; 1 storage, files, settings; 0 state, dlog, platform. `iface/handlers` is a passive table that loader and dsp call. `dsp` never calls `loader`; it returns `Advance`. Services reach only `iface`. A layer rule in `mise run check:layers` enforces that the core names no service.

### What leaves the core

| Piece | Goes to |
| --- | --- |
| `collect.rs` machines and fx part: builder state, the 15 builder calls, lowering, `check_commit`, `check_dsp_keys`, voice and fx declarations | `machines` and `fx` crates; the builder calls become provider EXPORTS |
| `collect.rs` generic part: `dk_record_add`, `dk_service_declare_*`, keys copy, `decl_matches`, block store | stays: `loader/keys.rs`, `loader/records_walk.rs` |
| `registry.rs` tables (`DKRT_REG`, `profile`, `free_rows`, `info`) | `machines`; the walker stays in `loader/records_walk.rs` |
| `params.rs` (parameter row writers) | a shared crate compiled into `machines.dks` and `fx.dks`; each writes only the rows its adapter reserves (`core/dynreg` for machine rows, `core/fxpages` for fx rows), so neither service needs the other |
| `assign.rs` (sticky machine type numbers) | `machines`; the map moves from the selection record to a settings key owned by machines, with a migration read of the old field for one release |
| `native.rs` (`mf_native_*`) | `machines`, through the function words of `machines_info` |
| row methods `has_voice`, `declare_voice`, `declare_fx`; `fx_entry`; `machine_types_of` | internal to the services; `fx_entry` becomes the fx export `dk_fx_entry`; `machine_types_of` becomes a generic `owned_ids`, so the MODULES reply stays the same |
| `dkrt_fx` C ABI | a generic `dkrt_export(name_hash)` for the menu app |
| watch guard logging, `guard_*`, the DSP state voice and fx fields, `dsp_info`, `fx_info`, `extent()` | with R12-8 SHARC stubs (the guard sits in the service dispatch there) |
| invariant checks of service rows | the services' host tests; the core keeps the generic checks |
| `gen_services::ORDER` | the planner's dependency order; `STATIC = 0` |
| samples: the crate link, the C entry points in `handlers.rs`, `SampleCore`, `SampleHost`, the entry glue, the platform `SamplePool` | `samples.dks` |
| `worker.rs` (the job queue on the stock sample-loader task) | `samples.dks`, its only shipped user; `dk_job_post` becomes a samples export; needs B1's firmware address imports |
| `dkrt_project_changed` | deleted (no caller; confirm with `tools/coldfire/refscan.py` first) |

Optional cleanup, done when convenient: `dk_debug_selected_params` to a machines export; the device tables in `src/cf/{dt2,dn2}/` and `tramp.s` to the `core/platform` adapter; the SDK files `sample*.rs` to the samples crate, `sticky.rs` to machines, `capacity.rs` to `dkrt-fit`.

## Storage layer

One device-agnostic layer. Device specifics sit behind one route decision and two backends. Backends are chosen by device profile data (`samples_in_store` in `devices/*.toml`), never by a device id.

```
 storage/
  mod.rs        RealStorage: Storage (read) and StoreWriter (write): the only entry
   ├─ route.rs  (device profile, root, path) → backend      ← the one decision
   │    ├─ store/              our store area on the card: modules, services,
   │    │                      settings (both devices), samples on DN2
   │    ├─ backends/fwfs.rs        DT2 sample reads through the firmware file system
   │    └─ backends/plusdrive.rs   DT2 sample writes through +Drive
   ├─ writer/   write session, pending files, commit, read-back verify
   ├─ lock.rs   store lock, read leases, shared buffers
   ├─ card.rs   card safety gate and the one gated block write
   └─ layout.rs store position and size from device words, one crc32
```

| File | Content | Removes |
| --- | --- | --- |
| `layout.rs` | Store first sector and size from the device words; one crc32 | card to store dependency |
| `card.rs` | The card gate and the one gated write over `Platform::block_write_raw` | platform and card cycle |
| `lock.rs` | Store lock, read leases, shared buffers (from `store_writer.rs`) | store and store_writer cycle |
| `store/` | Read side: find, stage with hash check, `read_all`, one lister, health figures | four listers become one |
| `writer/` | Session, pending files, commit, verify, delete (from `store_writer.rs`); power-cut test knobs become `cfg(test)` | writer dependencies on boot and io |
| `route.rs` | The only root and route decision; the `samples/` prefix and the path limit live here | three deciders |
| `backends/fwfs.rs` | DT2 firmware file system reads (from `storage_adapter.rs`) | |
| `backends/plusdrive.rs` | DT2 +Drive staged writes (from `store_writer.rs`) | |
| `mod.rs` | `RealStorage`: the `Storage` trait (read) and a `StoreWriter` trait (write); the `dk_files_*` write front end | |

Per device: DT2 sample reads go to `fwfs`, sample writes to `plusdrive`, everything else to `store`. DN2 sends everything to `store` (samples under `samples/`).

Operations kept, from their callers: layout and card gate; `find` and `load_to_stage` (loader); `list` (loader, scan, SysEx LIST, files API); `stat`, `stat_head` (loader, samples); `read_all` (`dk_files_read`, moved off `store::read_file`); `path_max`; the writer (begin, write, close, abort, delete, mkdir, rename, clear, verify, commit, pending); the store lock; read leases (DN2 sample pool); health figures. `Storage::read_head` and `read_range` are dropped: they have no production caller.

## Interfaces

**CoreApi.** About 24 methods stay: `device_id`, `module_name`, `settings_get`, `settings_put`, `log`, `current_source`, `info`, `dsp` (it shrinks with R12-8), the 8 `link_*` methods, and new `module_of(ctx)`, `storage_stat`, `storage_stat_head`, `irq_mask`, `irq_restore`. The machines and fx methods (`dynreg`, `reg_state`, `param_first`, `param_owners`, the row writers, `make_rep`, `type_resolve`, `type_map_used`, `native_*`; later `guard_*`, `dsp_info`, `fx_info`) leave with their code. `SampleCore` and `SampleHost` are deleted.

**Service data without the core naming it.** A service finds its adapter's data through `core.info(name_hash)`. New `[[info]]` entries in the adapters' own `module.toml`: `dynreg_info`, `dynfx_info`, `sample_pool_info` (magic, version, device, the `dkpool_*` function words). The core reads only its own platform tables.

**Builder calls.** The builder imports are marked `provided = true` in `sdk/sdk.toml` (they already name `service = "machines"`). The loader resolves them from the services' EXPORTS, as it does for `dk_param_get`. Each service keeps a module's builder state during its `init` and maps the module context to its record through `module_of`. Interface B gains `module_begin(rec)` and `module_commit(rec, &ModuleCommitCtx)`, beside the existing remove. The loader's commit becomes: declare check, add the row, each row's `module_commit`, remove on every row on failure.

**samples.dks.** Imports: `log`, `current_source`, `link_*`, `dsp`, `info`, `module_of`, `storage_stat`, `storage_stat_head`, `irq_mask`, `irq_restore`. Exports: the 9 `dk_sample_pool_*` functions and `dk_job_post`. Row hooks as today: `on_loaded`, `tick(EARLY)`, `dsp_check_sections`, `dsp_admit`, `dsp_claim`, `dsp_release`. It has no ColdFire hooks of its own.

**Events.** `dkrt_pool_done` and `dkrt_pool_lost` become pointer slots in the sample-pool adapter, filled by `samples.dks` through `dk_service_declare_slot`; the adapter calls a slot only when it is set. This is the slot mechanism R12-6 uses for frametype, and it keeps the hot reload rule that no image word points into a service file except slots.

## Work

Every commit is green on the host unit tests and `core/runtime:test-assembly`. Pure moves are checked with `patchkit objdiff` (identical, or the difference explained).

### Tests first, at the boundaries that stay

1. Storage contract suite (`tests/storage/contract.toml`): add writer cases (session, pending, abort, rename, clear, verify, lock busy) over the FakePlatform card, and DT2 cases with fake firmware file system and +Drive providers.
2. Route table test against `route.rs`: (profile, root, path, operation) gives the backend. Today's three deciders must agree with it before they are deleted.
3. One-lister test on a card image built from packed fixtures: SysEx LIST, scan, discover and `Storage::list` give the same entries.
4. Golden lowered records before the collect move: the cases of `collect/tests.rs` record today's lowered blocks as golden bytes; the tests then move into the machines and fx crates with the same bytes.
5. Interface B lifecycle with a fake service: begin, commit, remove, and the undo on a failed commit.
6. Assembly cases: shot with `samples.dks` (found by need, exports resolved, slots written; refused without the file); `test/sine` with its builder calls resolved from `machines.dks`; discover over a card image written by the workbench packer code.

### RS-S storage restructure and samples.dks

Own lane. It can run beside B1 bind-time firmware addresses: the only shared file is `loader/role.rs`, which this stack does not move.

| Step | Content | Proof | Agent time |
| --- | --- | --- | --- |
| S0 tests first | Items 1, 2, 3, 6 (storage and samples parts) above | host | done (branch `rs/storage-tests`) |
| S1 tree moves | Files into the directories above, no code change | `objdiff` | under 1 h |
| S2a storage split | `layout`, `lock`, `card`; the cycles cut | `objdiff` explained, contract suite | 1 h |
| S2b route and backends | `route.rs` as the one decision; `fwfs` and `plusdrive` backends; one lister | contract, route and lister tests | 1-2 h |
| S2c read path | `dk_files_read` on `read_all`; `read_head` and `read_range` dropped | contract suite | under 1 h |
| S3 core imports for services | `module_of`, `storage_stat`, `storage_stat_head`, irq; `sample_pool_info`; pool event slots | service contract tests, adapter build | 1 h |
| S4 samples.dks | Service wrapper, exports, worker job queue moved in (after B1's bind resolver), delete the samples glue in the core, `STATIC = 0`, order from the planner | samples tests over FakeCore, assembly cases | 1-2 h |

After S2, a `mise run dk:dev` boot on DN2 and DT2 checks the boot path (storage is on it). Its device cases (storage contract device cases, settings round trip, safe mode, upload, usb-fast upload (DN2), samples-live (DT2), samples-store-live (DN2), shot (pool, lease, picker)) run in the one gate shared with B1 and RS-C.

### RS-C machines and fx leftovers out of the core

After B1 bind-time firmware addresses: the native page move uses the `machines_info` words that B1 renumbers.

| Step | Content | Proof | Agent time |
| --- | --- | --- | --- |
| C1 lifecycle hooks | `module_begin`, `module_commit`, `ModuleCommitCtx`; the loader calls them; no users yet | loader tests over fakes | under 1 h |
| C2 the move | Collect lowering, registry tables, params crate, sticky types, native pages into machines and fx; builder calls as exports; `dynreg_info` and `dynfx_info` | golden lowered bytes, assembly | 2-3 h |
| C3 CoreApi split | Delete the methods and `dkc_*` shims; `dkrt_export`; `owned_ids`; invariants per service | host, service contract tests | 1 h |
| C4 docs and layer rule | ARCHITECTURE.md; the layer rule that the core names no service | `mise run check:layers` | under 1 h |

Device gate: one gate for B1 bind-time firmware addresses, RS-S and RS-C together (the host layers, incl. the assembly tests that run each service's real declarations, catch wiring first; a failure is bisected over the stack's commits). Machine fixture suite (dynreg, equiv, round trip), FX contract suite, capacity commit, refusals, needs, providers, frametype slot (DN2), the storage and samples cases of RS-S, gate-smoke, gate-pins. `mise run test:full` runs once after this gate.

R12-8 SHARC stubs follows RS-C; its SHARC slice proof can run beside RS-C.

## Payoff, confidence, cut line

Total about 1-1.5 days of agent work (measured pace: the RS-S tests took about 20 minutes, the B1 resolver about 35) and one device gate shared with B1; wall time is set by the device gate and reviews, not by coding. Payoff: the core does what its name says; the "kind-free" layer rule becomes true and checked; storage loses its cycles and has one route decision; the generic module lifecycle is what hot reload needs. Confidence: storage high (card model and contract suite exist); `samples.dks` medium-high (the slot mechanism is proven by R12-6); the collect move medium (largest and most coupled). Cut line: if the golden lowered bytes do not match within a day, stop after C1 and do C2 after R12-8.

## Fixtures

Each stack updates its canonical fixtures (`tests/fixtures/modules`, the table in docs/TESTING.md) and the SDK templates in the same stack. A new fixture is made only when no existing one can be extended.

| Stack | Fixture work |
| --- | --- |
| RS-S storage restructure and samples.dks | `track-select` opens its own sample pool, so a samples user runs without shot (zero DSP, assembly and device tests of `samples.dks`); `stock-pool` covers the DT2 firmware file system read route and `file-write` the +Drive write route on the device; the storage contract writer cases run on the host over the FakePlatform card. The service template follows the shape `samples.dks` settles on. |
| RS-C machines and fx leftovers out of the core | `test/sine`, `test/dyn-dn2` and the `dyn-fx-gain` fixtures keep working with their builder calls resolved through the machines and fx service exports; an assembly case loads `dyn-fx-gain` without `fx.dks` and expects the refusal (the sine case exists); `commit-demo` (machine, effect, parameters, click action) is the source of the golden lowered records. The machine and fx templates need no change of source. |

## Risks

- Readers of `DKRT_REG` (device tests round trip, capacity commit, boot restart, log names; `tools/patchkit/pk_dkrt_{machine,boot}.py`) break when the table moves into `machines.dks` memory. C2 adds one test-support resolver (record base plus the service symbol offset). `DKRT_DSP` stays in the core.
- `EXPORTS_MAX` is 32 (`loader/exports.rs:9`); about 34 exports are needed. Raise it to 64 and check `Declare::MAX`.
- A module's builder state inside a service file: size and arena attribution during module `init` are not measured yet. Measure in C2.
- The boot planner calls `assign::plan` before modules load (`boot.rs:184,201`). `machines.dks` must plan in its `init` or in the settle step from the selected modules' keys. Not verified.
- DN2 pool events may come from receiver context; the handler code must stay unchanged.
- S1 tree moves conflict with open runtime lanes: do them when only B1 is open.
- `dkrt_fx` to `dkrt_export` changes the `dkrt_*` C ABI (`abi.toml`, `dkrt.h`) and the menu app.

## Related follow-up

`patchkit service-check` compares the `service!` hooks with `[[hook]]`; it should also compare `exports` with `[[export]]` and `fills` with `[[fill]]`. A missing export declaration made `machines.dks` fail its declare check on the emulator (branch `rt/r12-batch`); the assembly case that runs each service's real declarations now catches it on the host.
