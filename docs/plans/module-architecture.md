# Module architecture: categories, contracts, capabilities and enforced layers (plan, 2026-11-01)

Status: agreed design, nothing built. It was written on the module-architecture design branch. Steps 1 and 2 start after the sampler branch
(the DSP sampler discovery branch) is merged.

Inputs: issue #240 (the store upload for patch-based images), PR #209 (machines patched into the firmware),
[finding 73](../findings/73-dnfw-coexistence.md) (dnfw and the runtime in one image), the [capabilities plan](capabilities.md), the
[module limits plan](module-limits.md) and [digikit hot reload](digikit-hot-reload.md).

Code references, line numbers and sizes are from the sampler branch at 94c8ee1a. `tracks.rs`, `sample.rs`, `sample_source.rs`, `files.rs`
and `core/sample-pool` reach `main` with that branch. Paths without a directory are in `modules/core/runtime/src/rs/src/`.

## 1. Goals

1. Module development is quick and easy. A module author writes against the SDK and does not touch the runtime. The runtime, the adapters
   and the device data are platform tooling that we own.
2. The contract that module authors see is stable. It changes once (step 1) while the runtime is 0.x; after that it only adds.
3. A user can submit a module before the SDK has an API for what it needs (`raw`). The module moves to an SDK contract later, without a
   change to how it is shown or grouped.
4. The runtime core keeps only what loading modules needs. "Small" means few responsibilities, not few bytes (section 8.6).
5. Tools check the layer boundaries. The checks are defensive: modules and changes are often written by AI agents, so a rule that matters
   is a check, not only a sentence in a document.
6. Little churn: move code once, delete duplicates, keep fields that work.

## 2. Terms

| Term | Meaning | Not |
| --- | --- | --- |
| Category | What a module is to a user: `machines`, `fx`, `tools`, `test`; `core` for `core/*`. Every UI shows it and groups by it. It does not change when the module moves to another contract. | Not the API the module uses. |
| Contract | The SDK API a module conforms to: `machine`, `fx`, `service` or `raw`. Each has a [major, minor] version. `raw` has no contract: only the generic load checks. | Not a user-facing label. |
| Capability | A device-neutral feature a module can use: FX pages, FX sends, track samples, value text per parameter, 8 knobs, chords. Its logic is device neutral and lives in the runtime crate (`src/cap/*`). | Not device code. |
| Adapter | The patch-time code of one firmware plug-in point: hook sites, copied tables, reserved rows. It implements capabilities on one firmware. One module per plug-in point under `modules/core/adapters/`. | Not loaded from the card (today). |
| Device data | Values in `devices/digitakt-ii.toml` and `devices/digitone-ii.toml` (limits, ids, offsets, sizes, patch sets), firmware declarations in `devices/fw/*.toml`, firmware addresses in `devices/ports/*.toml` (anchors by hash). `patchkit gen` turns them into `build/gen/*`. | Not hand-written numbers in sources. |
| Runtime core | The part of the runtime that loads and unloads modules, handles faults and safe mode, and keeps the basic stats (section 8.1). | Not the capabilities. |

## 3. Current state (read from the code)

### 3.1 Manifest, type and category

- `[module] category` is a free string (`packages/patchkit/src/module.rs:280`). Values in use: `machines` 5, `fx` 1, `tools` 3, `core` 21.
  `docs/MODULE-AUTHORING.md:123` also lists `ui`; no module uses it. Patchkit special-cases only `core`: it needs `role`
  (`module.rs:503`) and it decides where build objects live.
- `[module] type` is `ModuleKind`: `machine`, `fx`, `service`, `raw`, `framework` (`module.rs:255`, serde name for `kind`). In
  `modules/`: `machine` 5, `fx` 1, `service` 3, `framework` 21, `raw` 0. About 14 fixtures in `tests/fixtures/modules/` use `raw`.
- Nothing in patchkit branches on `type`. The packer infers the contract bytes of the `.dkm` header (0x38..0x3f: api, machine, fx,
  service) from the content: a DSP section or a machine `register` record sets machine (`packages/patchkit/src/dkm.rs:165`), effect
  records set fx (`dkm.rs:366`), slot functions or init/fini set service. api is always set.
- The loader refuses a contract whose major differs or whose minor is newer than the runtime's (`loader.rs:148-154`, `scan.rs:63-75`). It
  never reads a type.
- The `.dkm` header has no type or category field.
- The Transfer tab groups by the contract bytes: Machines if the machine contract is set, Effects if fx, else Tools
  (`apps/web/src/modules/dktransfer.ts:77` `groupOf`, `tools/device/dktransfer.py:88` `group_of`). **A `raw` machine would show under
  Tools.** This is the defect that the category field fixes.
- The module gallery shows the manifest category (`apps/web/src/data/modules.ts:26` and `:87`, `apps/web/src/components/ModuleCard.tsx`).
- `module:new` picks the folder from the type (`tools/patchkit/pk_new.py:28`, `KIND_DIRS`).
- `raw` in `docs/design/module-sdk.md` D2: "no contract, only the generic load checks, shown as unchecked in the menu". The badge is not
  built.

### 3.2 Adapters and images

- Every adapter is its own patch-time module under `modules/core/adapters/`. patchkit writes its hooks into the OS image and places its
  objects in `cf.expansion` (ColdFire) or `dsp.l2` (SHARC). None is a `.dkm`.
- `[patch_sets]` of the device files put the same eight roots into every image: `core/dynreg`, `dyndsp`, `dynfx`, `fxpages`, `ui`,
  `digikit-menu`, `health`, `input`. `requires` adds the rest (`runtime`, `expansion`, `machines`, `params`, `dsp-link`, `dsp-voice`,
  `fx`). DT2 `device` and `emulator` include `core/sample-pool`; the `sample-tracks` set has the same roots.
- Adapter code is small: from about 30 B of data (`dynfx`) to about 6 KB of text (`ui`).
- The runtime calls adapters through optional symbols (`modules/core/runtime/module.toml:30`: `dynreg_info`, `dl_send`, `dyndsp_info`,
  `dynfx_info`, `fxpages_info`, `dkpool_*`, `health_*`, `input_*`, `dk_ui_*`). Each resolves to 0 when its adapter is not in the image.
- Most adapters list `requires = ["core/runtime"]`, but **only `core/sample-pool` imports runtime symbols** (`dkrt_pool_done`,
  `dkrt_pool_lost`). The others import only firmware anchors and other adapters' symbols (checked in every `build/*.dko.json` of
  `modules/core/adapters/`). So `requires` mixes "useful only with the runtime" with "calls the runtime".

### 3.3 Hooks today

- All ColdFire hooks are written at patch time: patchkit writes the branch into the OS image; the bootstrap copies the OS into SDRAM; the
  hooks are there before the first OS instruction runs.
- The runtime's hooks: `cf.view.hook.ui_loop`, `cf.rtos.hook.main_os_task` (boot load) and `cf.midi.hook.rx` (upload)
  (`modules/core/runtime/module.toml:76/88/100`). The runtime has no audio-block hook.
- Adapter hooks: `machines` (many sites), `params`, `fxpages`, `ui`, `health`, `dynreg`, `dsp-voice` (SHARC), `fx` (SHARC).
- The crash handler sets the firmware's exception hook at run time by writing a pointer word (`crash.rs:109`, `EXC_HOOK_LIT`). That is a
  data write, not a code patch.
- Run-time attachment for a `.dkm` is data only: rows that adapters reserve (dynreg, dyndsp, dynfx, fxpages), and `Api.register_cmds` for
  SysEx commands. No code is patched at run time (`docs/design/digikit-registry.md`).
- `docs/ARCHITECTURE.md` rule 5 ("a hook costs at most one flag check when unused") is a design rule. No flag-check mechanism exists.

### 3.4 The runtime: sizes and memory

From `modules/core/runtime/build/runtime.dko.json`:

| Part | Bytes | Largest items |
| --- | --- | --- |
| text | 144,532 | |
| data | 73,689 | tracks state 35,802; files backends 28,864; files jobs 5,116 |
| bss | 666,916 | arena 524,288; staging 65,536; upload buffer 36,896; settings 8,192; files scratch 4,096 |

- **The arena** (`DKRT_ARENA`, `arena.rs`) is the memory of loaded modules. The loader copies each `.dkm`'s ColdFire code and data into it;
  modules allocate from it (`dkrt_alloc`). DSP code has its own area on the DSP side (`[digikit] dsp_area_bytes`).
- **`cf.expansion`** is the free RAM above the firmware's own use. At reset `core/expansion` copies the runtime and every adapter into it.
  DT2 1.17: 0x47e8b000 to 0x47fe0000, 1,396,736 B (1.33 MiB, `devices/ports/dt2-1.17.toml`). DN2: about 25 MiB.
- On DT2 the runtime alone takes about 864 KiB of the 1.33 MiB. All loaded modules share the 512 KiB arena for their ColdFire parts. A
  larger arena on DT2 means that something else gets smaller (module limits plan).
- A per-file split of the text is not reliable (inlining hides the source). A rough count puts about half the text in capability logic.

### 3.5 The runtime: what each file does

| File | Kind | Job |
| --- | --- | --- |
| `loader.rs` | core | Checks a `.dkm` (magic, sha, contracts, imports, relocs, slots), copies and relocates it into the arena, calls init, binds slots; unload and failure cleanup. Also reads kind-2 MACHINE records for `assign::plan` (`machine_keys`, :53-93) and drops the dsp-link tick callback (:472-484). |
| `scan.rs` | core (stats) | `dkrt_scan`, `dkrt_fit`, `module_header` for the menu: headers from the store, the fit rule through `dkneeds`. |
| `arena.rs` | core | First-fit allocator over `DKRT_ARENA`. |
| `table.rs` | core | Invariant check after every load and unload; reads `dsp::ds()` and `registry::DKRT_REG`. |
| `boot.rs` | core | Boot hook: selection record, card check, settings load, crash probe, safe mode, boot counter, `assign::plan`, the load loop. |
| `crash.rs` | core | Exception hook, CPU crash, DSP hang and panic records in keep RAM; soft reset. |
| `watch.rs` | core + capability | DSP diagnostics poll and heartbeat (core); maps DSP fault rows to modules through `dsp::module_of_*`; the health snapshot. |
| `dlog.rs` | core | Log ring and sink; per-module log source (`enter_module`, `leave_module`). |
| `settings.rs` | core | The settings record (8 KiB RAM, two card copies, written 2 s after the last change). |
| `card.rs` | core | Card ownership check (`check`), the write gate (`admit`), the claim of a blank card (finding 46). |
| `store.rs` | core + capability | Reader for the loader (`find`, `open`, `load_to_stage`, `layout`); `read_file`, `list`; the store figures for HEALTH (`note_commit`, `figures_stale`). |
| `sha256.rs` | core (library) | Pure SHA-256. |
| `assign.rs` | capability (rows) | Run-time machine type numbers; the sticky map in the selection record. |
| `registry.rs` | capability (rows) | `register` records: PARAM, PAGE, MACHINE, EFFECT; writes parameter, page and machine rows; calls `fxpage::apply`/`remove`. Adapter `core/dynreg`. |
| `dsp.rs` | core + capability | DSP section check (versions 1-5), range reserve, relocation, the SEND and ROWS phases (core); voice rows, effect rows, bank and track records (capabilities). Optional `dl_*`, `dyndsp_info`, `dynfx_info`. |
| `fxpage.rs` | capability (fx) | FX pages: effect records, page rows, kit load and save. Adapter `core/fxpages`. |
| `fxsend.rs` | capability (fx) | FX routes and per-track send amounts; uses `settings`. |
| `sample.rs`, `sample_source.rs` | capability (sample bank) | The module-owned sample bank (API 1.3); file read and decode into a DSP-link buffer. |
| `tracks.rs` | capability (track samples) | Per-track stereo samples (API 1.5). Adapter `core/sample-pool`. |
| `files.rs` | capability (files) | Read-only listing for modules (API 1.4). |
| `upload.rs` | capability (store upload) + core commands | The `F0 7D 64 6B` handler: store commands and runtime commands in one table. |
| `lib.rs` | core | Entry points, `dkrt_tick`, the import table, `block_read`/`block_write`, `dkrt_capacity`, `dkrt_health*`, the mailbox. |

### 3.6 The runtime: how capabilities are tied to the core

There is no registration. The core calls each capability directly:

- `loader.rs:258` calls `registry::check`, `:428` calls `registry::apply`. `registry::apply` runs one pass per kind in a fixed order
  (PARAM, PAGE, MACHINE, EFFECT) and calls `fxpage::apply` (`registry.rs:623`), which calls `fxsend::apply` (`fxpage.rs:388`).
- `dsp::check` (`dsp.rs:614`) parses voice, effect, bank (kind 1) and track (kind 2) records of the DSP section. `dsp::reserve` calls
  `tracks::reserve` and `sample::reserve` (`:1005-1007`); `dsp::step` calls `sample::ready`, `tracks::ready` and `boot::note_result`
  (`:1395-1397`).
- The loader calls `files::init_begin/ready/retire` (`loader.rs:385-497`) and `upload::release_cmds` (`:440`, `:539`).
- `dkrt_tick` (`lib.rs:1048-1061`) calls, in order: `boot::ui_pass`, `upload::restart_poll`, `crash::install`, `dsp::step`, `sample::tick`,
  `tracks::tick`, `files::tick`, `fxpage::tick` (which runs `fxsend::tick`), `settings::tick`, `watch::tick`.
- Unload order (`loader.rs:496-550`): files retire, clear slots, wait for tasks in the module (else `E_BUSY`), `dsp::quiesce`, `fini`,
  release SysEx commands, drop the tick callback, `registry::remove` (fxpage and fxsend first), `dsp::release_range` (sample and tracks),
  free the arena block, `table::check`.

Seven ties must be cut for a clean split (section 8.4 gives the answer for each):

1. One DSP-link word queue. sample, tracks, fxpage and fxsend wait for each other through `busy()` (`sample.rs:133-135`, `tracks.rs:190`,
   `dsp.rs:361`, `fxpage.rs:585`, `fxsend.rs:417`).
2. The fixed order of record kinds and of unload steps.
3. `table::check` reads the state of dsp and registry directly.
4. `dsp::fail` calls `loader::undo_early` (`dsp.rs:1290`): the DSP state machine and the loader call each other.
5. Shared helpers: fxpage and fxsend use registry's `write_fx_param_row`, `write_send_param_row`, `reset_param_row` and `make_rep`;
   `registry::check` imports `fxpage::MAX_FX`, `KIND_EFFECT` and `LEN_EFFECT`.
6. Fault attribution: `watch.rs:407-436` maps DSP fault rows to modules through `dsp::module_of_*`.
7. Readers across capabilities: CAPACITY (`dkrt_capacity` reads store, settings, assign, fxsend, registry, dsp), MODULES
   (`upload.rs:456-484` reads dsp and `registry::types_of`) and the health figures.

### 3.7 Loading is at boot only

`loader.rs:109-112`: "the module set is fixed per session: only the boot hook loads"; outside boot `dkrt_load` returns `E_BOOT_ONLY`.
SELECT writes the selection record and loads nothing; RESTART applies it at the next boot. The only unload left is the failure path
(`undo_early`). Hot reload is deferred, not dropped ([digikit hot reload](digikit-hot-reload.md)).

### 3.8 Content modules, SHARC assembly and what a module can reach

- Every machine (analog, bd, pd, pluck, shot) and the fx module (freqshift) is C (`src/module.c`, the declaration) plus one SHARC `.s` (the
  DSP code). The tools (debug, probe, remote) are C only. MODULE-AUTHORING.md: machine and fx are "SHARC+ assembly, declared in C".
- The SHARC files include only SDK files: `dk_machine.inc` or `dk_fx.inc` (generated from `sdk/sdk.toml`) and `sdk/dsp/*.inc` helpers.
- The C runs on the ColdFire and the assembly on the SHARC. The assembly cannot be wrapped in a C function; the C only declares it. So the
  assembly needs its own boundary check.
- A public C compiler for SHARC+ exists: `selcc` of the Selache toolchain, whose assembler `selas` builds all our SHARC code
  (`tools/patchkit/pk_sharc.py`). No module is built with it, and its output is not measured against hand-written code.
- ColdFire link surface of a `.dkm` (`loader.rs:207-221`, `lib.rs:361-477`): the `IMPORTS` table (`fw_block_read`, `fw_alloc`,
  `fw_free`, libc and libgcc helpers, `dkrt_blocks`, `dkrt_log_sink_set`, `dkrt_log_ev`, `dkrt_peek_range`, `dkrt_track_type`), the
  optional exports of `core/input` and `core/ui`, and the `Api` table. There is no import by firmware anchor or address.
- SHARC link surface: names in the `dyndsp_info` export table only (`dsp.rs:600-611`); an unknown name is `E_DSP_IMPORT`.
- **Not prevented:** a literal address cast in C or a literal address in assembly. The fixture `stock-pool` calls firmware functions at
  fixed addresses (`stock_pool.c:38-47`); `dyn-fault` writes 0xEC090000. layercheck sees only undefined symbols, and a cast creates none.
  Stray stores are found only at test time: `module:audit` (ColdFire) and the write-set checks of the conformance suites (SHARC,
  `tests/support/machine_rig.py`, `tests/support/fx_rig.py`).
- Checks per type today: machine and fx run their conformance suites; service and raw get only the objects-against-sources check
  (MODULE-AUTHORING.md, `tools/patchkit/pk_modcheck.py`).

### 3.9 Enforcement today

- `mise run check:layers` runs `tools/devtools/layercheck.py`. Rules: roles fit folders; content modules and apps include only their own
  files and the SDK (C `#include`, assembly `.include`, Rust `dkfw::`, Cargo path deps); built objects of apps and content modules import
  only SDK symbols, never anchors; apps have no `[[op]]` or `[[claim]]`; `<dk/ui.h>` functions are SDK imports; no tracked file names a
  user module (self-contained).
- layercheck does not check the core layers against each other, or the `requires` graph.
- Nx 22.7 runs the TypeScript host tests with a local cache and `affected` (`docs/plans/nx-pilot.md`). 8 projects, no tags, no boundary
  rule. The linter is Oxlint.
- The gate's `gen-check` regenerates `build/gen/` and compares (`tools/devtools/verify.py:114`). Generated files carry a header
  "generated by `patchkit gen` ...; do not edit".

### 3.10 Settings and versions

- The settings record is digikit's one key-value store (`docs/design/digikit-store.md`, "Keys by owner"). Keys now: `fx/route/<module
  id>`, `fx/send/<module id>`, `dk/fault` (the crash handler's fault record, `crash.rs:545-548`). Keys `<module id>/...` are reserved for
  the module itself; there is no module API yet. A module can already export a settings menu (`dkm_menu`, `docs/plans/sdk-api.md`).
- The runtime has a semantic version (`sdk/sdk.toml` `version`, now 0.5.4; `docs/design/module-sdk.md` D9). While it is 0.x, MINOR may
  break. Adapters carry the version of the runtime image they are built into.
- Each contract has [major, minor] (`sdk/sdk.toml` `[contract]`, api is 1.5). It decides compatibility (D3).
- A module's manifest `version` (major.minor.patch) is its own; the runtime does not read it.

## 4. Decisions

| # | Decision | Reason |
| --- | --- | --- |
| D1 | `category` is the user-facing field, an enum: `machines`, `fx`, `tools`, `test`, `core`. `ui` is dropped (unused). | Every UI shows and groups by it; it must not change when a module changes contract. Keeping today's strings avoids churn. |
| D2 | `contract` replaces `type`: `machine`, `fx`, `service`, `raw`. It defaults from the category (`machines` → `machine`, `fx` → `fx`, `tools` → `service`); `raw` is written out. `framework` is deleted. | `type` reads as a category today. `contract` says what it is. `framework` repeats `category = "core"` plus `role`. |
| D3 | The `.dkm` header carries the category. The Transfer tab and the desktop group by it. The contract bytes still decide compatibility. | Fixes the raw-machine-under-Tools defect (3.1). |
| D4 | The packer checks the declared contract against the content, instead of inferring it. A `raw` module gets the api contract only. | A declared contract that the content does not meet is an error the author must see. |
| D5 | A minimal content manifest (section 6.1). | Fewer fields for a contributor to get wrong; less for conformance to check. |
| D6 | A module's needs are derived from the built `.dkm`, never declared: sizes and rows (as `packages/dkneeds` does now), capabilities from its imports and record kinds. | Nothing to misreport; nothing to keep in sync. |
| D7 | `raw`: no contract checks. Same link surface as every content module. No absolute firmware or I/O address. | `raw` is the place for user modules until the SDK has their API. An absolute address breaks with every OS update and no tool can check it. |
| D8 | An experimental tier for adapters, with merge rules (section 7.3). | Contributions that need new hooks (PR #209) can merge without changing the default images. |
| D9 | The migration path: experimental adapter → `raw` `.dkm` → contract module. The category stays the same in every step. | Users submit now; we migrate as the SDK grows. |
| D10 | SHARC assembly is a first-class module language. A C compiler for SHARC+ is measured later. | All DSP code is assembly today; it is the fastest path we have. |
| D11 | Capability = device-neutral logic (runtime crate, `src/cap/*`) plus an adapter per firmware where the firmware needs glue. "Adapter" is the right name. Adapters stay separate modules, one per firmware plug-in point. | Keeps device code in adapters and device values in TOML. |
| D11a | No new device-specific layer. The adapters with their per-device objects, the device profiles (`devices/*.toml`) and the ports are that layer. | The layer exists; it needs one source of values, not another layer. |
| D12 | The runtime core keeps: load and unload, record dispatch, arena, boot, selection, safe mode, crash and fault handling, log, settings, store reader and card gate, DSP link transport, basic stats (CAPACITY, MODULES, what fits). | These are what loading modules and recovering from faults need. Settings stays because the crash handler stores its fault record there. |
| D13 | Capabilities are split **inside the runtime crate** (option A), behind one registration table with the **shape of a C interface** (option 2). A capability becomes its own object only for a reason; the first is the store upload (#240). | No memory or speed cost, no new ABI to version, little churn. A capability change touches only its folder and one line of the table. A later move to its own object is a build change (sections 8.3 and 8.7). |
| D14 | The DSP link transport owns one claim flag; capabilities claim, send and release. | Removes the cross-checks of `busy()` between capabilities. |
| D15 | `requires` is a code dependency. `[patch_sets]` decide what an image holds and name `core/runtime` themselves. | Today `requires` points the wrong way for a layer check (3.2). |
| D16 | Device values come only from TOML through `patchkit gen`. Per-device code only where the firmware differs. One source per adapter where the devices differ only in values. | One source of truth per value; nothing to fix on one device and forget on the other. |
| D17 | Generated files: an AGENTS.md rule and `gen-check` in `test:changed`. No tool-specific edit hooks. | Contributors use different tools; the checks must work for all of them. |
| D18 | Versioning rules (section 12). | Users must know when a module or its settings stop working. |
| D19 | Module settings: keys `<module id>/...`; the module owns its values and their version; the runtime never interprets them. | The runtime cannot know a module's value layout. |
| D20 | Hot reload stays deferred. The table keeps `quiesce` and `remove`, so hot reload needs no change to the table. | The failure path needs them anyway. |
| D21 | Hooks stay patch-time until the spike (step 8) proves that the runtime can install them at boot. | All decisions above hold either way (section 11.3). |
| D22 | Enforcement: a local Nx plugin that reads `module.toml`, a boundary check over the Nx graph against one rules table, and layercheck for the rules a graph cannot express (section 13). | No free tool covers C, Rust and assembly together; Nx Conformance needs a paid licence. |
| D23 | Issue #240: the store upload becomes its own object (section 9). | A dnfw image can then accept uploads without the runtime. |
| D24 | PR #209: merges as experimental adapters, in parts; then its capabilities move into the SDK one at a time (section 7.4). | It gets a home upstream without changing default images. |

## 5. Layers and rules

```
content module (.dkm)       C (ColdFire) and SHARC assembly, against the SDK only
        │ SDK imports, Api table, records
SDK contract                sdk/: headers, sdk.toml imports, contract versions
        │
runtime: capabilities       src/cap/*: device-neutral logic; registered in the core's table
        │ core API                      │ optional symbols (0 when the adapter is absent)
runtime: core               ────────────┼──────────────▶ adapters: patch-time firmware glue per firmware
        │                               │                       │
firmware bindings, anchors  modules/core/fw (generated from devices/fw), devices/ports
```

Rules:

1. A content module uses only the SDK: C includes `<dk/...>`, assembly includes `sdk/asm` and `sdk/dsp`, imports from `sdk/sdk.toml`.
2. The core names no capability. It calls the registration table.
3. A capability uses the core only through the core API, and another capability only through that capability's declared API, never its
   state.
4. An adapter uses firmware bindings and other adapters' headers. It calls the runtime only through declared callbacks (today
   `dkrt_pool_done`, `dkrt_pool_lost`, `dkrt_project_changed`, all of `core/sample-pool`).
5. An app (`modules/core/apps/*`) has no `[[op]]` and no `[[claim]]`; an adapter calls it through an optional `dk_app_*` symbol.
6. `requires` is a code dependency; `[patch_sets]` compose an image.
7. A device value comes from TOML through `patchkit gen`. A source never includes a device-named file (`device-dt2.inc`).
8. Code is per device only where the firmware differs in structure.
9. A capability costs nothing when unused: data before hooks; at most one flag check on a hot path ([capabilities plan](capabilities.md)).

The rules are one table (step 4), read by the boundary check and by layercheck.

## 6. Manifest and `.dkm`

### 6.1 The content manifest

| Field | Today | After |
| --- | --- | --- |
| `id`, `version`, `name`, `author`, `summary` | yes | kept |
| `category` | free string | enum (D1) |
| `type` | `machine`, `fx`, `service`, `raw`, `framework` | replaced by `contract`; default from the category; `raw` written out |
| `dynamic` | always `true` for content modules | removed |
| `requires`, `conflicts` | always `[]` for content modules | removed: needs come from capabilities (D6) |
| `ports` | repeats the keys of `[targets]` | removed |
| `status` | repeats the values of `[targets]` | removed if it always equals them (checked in step 1) |
| `[targets]` | yes | kept |
| `[[object]]` | path, sources, place per object | derived from the source layout: `src/*.c` → `cf.expansion`, `src/dsp/*.s` → `dsp.l2`; written out only to differ |

Example, `modules/machines/shot/module.toml`. Today:

```toml
[module]
id = "digikit/shot"
version = "0.1.0"
name = "SHOT"
category = "machines"
type = "machine"
author = "digikit"
status = "experimental"
summary = "Plays the stereo sample bound to the track once per note, with start, end, reverse and pitch."
dynamic = true
ports = ["dt2-1.17", "dn2-1.12"]
requires = []
conflicts = []

[targets]
"dt2-1.17" = "experimental"
"dn2-1.12" = "experimental"

[[object]]
path = "build/shot-cf.dko.json"
sources = ["src/module.c"]
place = "cf.expansion"

[[object]]
path = "build/shot-dsp.dko.json"
sources = ["src/dsp/shot.s"]
place = "dsp.l2"
```

After:

```toml
[module]
id = "digikit/shot"
version = "0.1.0"
name = "SHOT"
category = "machines"          # contract defaults to "machine"
author = "digikit"
summary = "Plays the stereo sample bound to the track once per note, with start, end, reverse and pitch."

[targets]
"dt2-1.17" = "experimental"
"dn2-1.12" = "experimental"
```

A `raw` machine adds one line: `contract = "raw"`.

`core/*` modules keep `category = "core"` and `role`, and lose `type = "framework"`.

### 6.2 The `.dkm` header and the packer

- The header gets the category (one byte). Whether a free byte exists or the format version goes up is open (section 15).
- The packer reads `contract` and checks the content against it: `machine` needs a machine declaration and DSP code; `fx` needs effect
  records; `service` needs slot functions or init/fini; `raw` sets only the api contract. It no longer infers the contract from the
  content.
- The loader is unchanged: contract bytes decide compatibility.

### 6.3 The user interface

- The Transfer tab and the desktop group by the header's category: Machines, Effects, Tools (Test behind the test-module toggle).
- A `raw` module shows as unchecked (the D2 badge of `docs/design/module-sdk.md`).
- The gallery keeps showing the category; its enum matches D1.

### 6.4 Files that change in step 1

- patchkit: `packages/patchkit/src/module.rs` (the enum and the field), `packages/patchkit/src/dkm.rs` (the header, the contract check).
- Python tools: `tools/patchkit/pk_new.py` (folder from the category), `tools/patchkit/pk_package.py`, `tools/device/dktransfer.py`
  (`candidates`, `group_of`), `tools/devtools/verify_route.py`, `tools/devtools/layercheck.py`.
- Web and desktop: `apps/web/src/modules/dktransfer.ts` (`groupOf`), `apps/web/src/transfer/transfer-core.ts`,
  `apps/web/src/data/modules.ts`, `apps/web/src/data/shipped.mjs`, `apps/web/src/components/ModuleCard.tsx`,
  `apps/desktop/ui/src/memory-backend.ts` and its preview fixtures.
- Templates: `sdk/templates/*/module.toml`.
- Manifests: the 9 content modules, every fixture in `tests/fixtures/modules/`, the 21 `core/*` modules (`type` removed).
- Tests that pin the strings: `tests/patchkit/test_pk_new.py`, `tests/patchkit/test_pk_package.py`, `tests/devtools/test_layercheck.py`,
  `tests/devtools/test_verify.py`, `tests/conformance/load.ts`.
- Docs: `docs/MODULE-AUTHORING.md`, `docs/design/digikit-dkm.md`, `docs/design/module-sdk.md` (D2, D10).

## 7. Raw modules and experimental adapters

### 7.1 The migration path

| Step | Shape | Checks | Example |
| --- | --- | --- | --- |
| A. Experimental adapter | Patch-time module that owns a new hook and exposes an experimental capability as data rows or optional symbols. The content is a `raw` `.dkm` where possible, else inside the adapter. | layercheck, its own tests, default images unchanged | PR #209 today |
| B. Raw `.dkm` | Uses the capability. | generic load checks, the absolute-address check, `module:audit` | TB303 once it loads from the card |
| C. Contract module | The capability is stable in the SDK. | the conformance suite of the contract | shot, pluck |

From B to C: change `contract`, replace calls that are not SDK calls, pass the conformance suite. The category does not change, so the UI
does not change. The more a raw module uses the SDK headers from the start, the easier the move.

### 7.2 What `raw` allows

- Allowed: any C and SHARC code; no entry contract; SysEx service commands through `Api.register_cmds`; the full SDK.
- Not allowed: absolute firmware or I/O addresses, firmware anchors, `[[op]]` or `[[claim]]` (that is an experimental adapter).
- Defences, from static to run time:
  1. **The absolute-address check (new, step 3).** A module's own addresses always carry a relocation. A literal address in the OS code or
     data range, or in the I/O range, with no relocation, is a firmware or hardware access: refused. ColdFire objects are decoded by the
     `isacheck` decoder (`tools/coldfire/isacheck.py`), SHARC objects by `tools/sharc_dev/sharc_core/`. A computed address is not found;
     the checks below catch it at test time.
  2. **The loader's import checks** (exist): `E_IMPORT`, `E_DSP_IMPORT`.
  3. **The boundary rules** (exist, extended in step 4): includes and imports only from the SDK.
  4. **Test time** (exist): `module:audit` for ColdFire stores outside the module's blocks; the write-set checks of the conformance rigs
     for SHARC.

### 7.3 Experimental adapters

An experimental adapter:

- has `stability = "experimental"` in `[module]` (a field, not a folder, so a promotion moves no files);
- has its own patch set and is never in the `emulator` or `device` set;
- takes device values only through `patchkit gen`, and has per-device code only where the firmware differs (rules 7 and 8);
- has its own tests, and its README names the capability it prototypes;
- leaves the default images unchanged: the smoke matrix shows no change.

Promotion to a stable adapter: the capability enters the SDK (a contract minor), the adapter's `stability` is removed and it may join a
default patch set. The values need no change, because they come from TOML from the first commit.

### 7.4 PR #209

PR #209 adds TB303 (Open303), two paraphonic sample machines (Para, ParaWT), eight overdrive flavours, and core changes in `params`,
`dsp-voice`, `dsp-link`, `dyndsp`, `dynreg`, new `polynotes` and `trackflags` modules, `devices/*.toml` and `devices/ports/dt2-1.17.toml`.
Its author built them into the firmware because of memory and knob limits, and offered to split the PR.

1. Split into three PRs: overdrive, Open303, the paraphonic machines.
2. Each lands as experimental adapters with their own patch set, under the rules of 7.3. Track flags are not planned (a parameter does the
   same job, [capabilities plan](capabilities.md)).
3. Its capabilities enter the SDK one at a time, in the order of the capabilities plan: value text per parameter, all 8 knobs (module
   limits step 4), track sample, stereo voice, chords.
4. TB303 then loads as a `raw` `.dkm` on DT2, then moves to the machine contract. Para and ParaWT follow.

The author should rebase after step 1, so their modules are migrated once. The paraphonic machines run out of DSP time today (the
author's comment on the PR); we help measure them once they are split out.

## 8. Runtime: core and capabilities

### 8.1 What stays in the core

| Core part | Files | Why it stays |
| --- | --- | --- |
| Load and unload, record dispatch | `loader.rs`, the table | The core job. |
| Arena | `arena.rs` | Module memory. |
| Invariants | `table.rs` (asks each capability through the table) | Refuses loads after a broken state. |
| Boot, selection, safe mode | `boot.rs` | Decides what loads and when not to. |
| Crash and fault handling | `crash.rs`, the fault part of `watch.rs` | Recovery. |
| Log | `dlog.rs` | Every part logs. |
| Settings | `settings.rs` | The crash handler stores `dk/fault` there; restart flushes it; capabilities and modules store through it. |
| Store reader and card gate | the reader of `store.rs`, `card.rs`, `sha256.rs` | Loading reads modules from the card; every card write passes the gate (finding 46). |
| DSP link transport and placement | the transport part of `dsp.rs` | Loading a module with DSP code sends it to the DSP. |
| Basic stats | CAPACITY and MODULES core rows, `scan.rs` (what fits) | What is loaded and what space is left on ColdFire and DSP. Each capability adds its own rows through the table. |

### 8.2 What becomes a capability

| Capability (`src/cap/*`) | Code today | Adapters it uses |
| --- | --- | --- |
| `rows`: machines, parameters, pages | `registry.rs` (rows), `assign.rs`, `machine_keys` of `loader.rs` | dynreg, machines, params |
| `voices` | voice rows of `dsp.rs` | dyndsp, dsp-voice |
| `fx`: rows, pages, sends | effect rows of `dsp.rs`, `fxpage.rs`, `fxsend.rs` | dynfx, fxpages, fx |
| `samples`: the module's sample bank | `sample.rs`, `sample_source.rs`, bank records of `dsp.rs` | none |
| `tracks`: per-track samples | `tracks.rs`, track records of `dsp.rs` | sample-pool |
| `files` | `files.rs` | none |
| `store_upload` | the store commands of `upload.rs`, the store writer and figures of `store.rs` | none |
| `health` | `dkrt_health*` of `lib.rs`, the snapshot of `watch.rs` | health |

### 8.3 The registration table

The table has the shape of a C interface (option 2): `#[repr(C)]` descriptors and `extern "C"` function pointers, pointer and length
instead of slices, error codes instead of `Result`. Only the seam has this shape; code inside a capability is normal Rust. With this shape,
moving one capability into its own object later is a build change: the table entry becomes a symbol that patchkit links. With Rust traits
(option 1) the interface would have to be rewritten first. Entries of the two kinds cannot be mixed in one table.

```rust
#[repr(C)]
pub struct DkCap {
    name: *const u8,                                                      // for the log and CAPACITY
    reg_kinds: u32,                                                       // `register` record kinds it handles (bit mask)
    dsp_kinds: u32,                                                       // DSP-section record kinds it handles (bit mask)
    phase: u32,                                                           // apply order: rows of PARAM, PAGE, MACHINE, then EFFECT, ...
    boot: Option<extern "C" fn(sel: *const DkSelection)>,                 // before the first load (assign::plan, settings reads)
    check: extern "C" fn(kind: u32, rec: *const u8, len: u32) -> i32,     // before any write
    apply: extern "C" fn(owner: u32, kind: u32, rec: *const u8, len: u32, place: *const DkPlace) -> i32,
    pre_init: Option<extern "C" fn(owner: u32)>,                          // around the module's init (files)
    post_init: Option<extern "C" fn(owner: u32)>,
    ready: Option<extern "C" fn(owner: u32)>,                             // after the DSP rows are published
    quiesce: Option<extern "C" fn(owner: u32) -> i32>,                    // before fini; E_BUSY keeps the module
    remove: extern "C" fn(owner: u32),                                    // reverse phase order
    tick: Option<extern "C" fn()>,                                        // once per UI pass, in table order
    counts: Option<extern "C" fn(out: *mut DkCapCounts)>,                 // its rows of CAPACITY and MODULES
    owner_of_row: Option<extern "C" fn(space: u32, row: u32) -> u32>,     // fault attribution
    invariants: Option<extern "C" fn() -> i32>,                           // part of table::check
}
static CAPS: [&DkCap; 8] = [&ROWS, &VOICES, &FX, &SAMPLES, &TRACKS, &FILES, &STORE_UPLOAD, &HEALTH];
```

`DkPlace` carries what DSP placement decided for the module: the DSP base address and the rows the core claimed. The field list is the
design; step 6 fixes the exact types.

### 8.4 How each tie of 3.6 is cut

| Tie | Answer |
| --- | --- |
| 1. DSP-link queue and `busy()` | The transport owns one claim flag (8.5). |
| 2. Fixed order | `phase` in the descriptor; apply in phase order, remove in reverse; `dsp::release_range` stays last in the core. |
| 3. `table::check` | The core checks its own state; each capability checks its own through `invariants`. |
| 4. `dsp::fail` ↔ `loader::undo_early` | Both are core. The cycle stays inside the core. |
| 5. Shared helpers | `fx` uses the declared API of `rows` (`write_fx_param_row`, `write_send_param_row`, `reset_param_row`, `make_rep`). The constants `MAX_FX`, `KIND_EFFECT`, `LEN_EFFECT` move to `dkrt_sdk`. |
| 6. Fault attribution | `owner_of_row` in the descriptor; `watch.rs` asks the table. |
| 7. Readers across capabilities | `counts` in the descriptor; CAPACITY, MODULES and the health figures sum the core's rows and each capability's rows. |

### 8.5 The DSP link claim

The ColdFire sends data to the DSP through one channel: one chunk per audio block, in the frame tail. A transfer takes many blocks. If two
users interleave chunks, the DSP receives garbage. So today each user checks the others' `busy()` before it starts, and every user knows
every other.

After: the transport owns one claim flag. A user calls `claim`, sends, then `release`. No user knows another. The priority is the tick
order, as today. Step 6 first lists every ordering rule that the current `busy()` checks express, and keeps each one.

### 8.6 Cost, and what a small runtime means

- Speed: the capability code runs at boot, in the UI loop (about 30 passes a second) and in the MIDI task. It does not run in the audio
  block. A UI pass makes about 10 calls through the table instead of direct calls.
- Memory: the table is about 256 B. Inlining across the seam stops, so the text may change by a few hundred bytes either way. It stays one
  object with one copy of the Rust `core` code.
- Moving code out of the runtime moves bytes; it does not remove them. Only an image that leaves a capability out saves memory. A small
  runtime is about responsibilities and churn. The memory lever on DT2 is the arena size (module limits plan).
- Proof in step 6: the runtime object's text, data and bss before and after; the regression suites; one health CPU reading. A change above
  noise stops the step.

### 8.7 Inside the crate or separate objects

| | A. Inside the crate (chosen) | B. Separate objects |
| --- | --- | --- |
| Memory | Same as today | More: the table, pointer calls, maybe a copy of the Rust `core` code per object (not measured) |
| Leave a capability out of a DT2 image | No | Yes |
| Other images link a capability | No | Yes |
| A new versioned ABI | No: internal, built together | Yes: core to each capability |
| Build | One build | One per capability |
| Churn | Code moves inside the crate; tests stay | Also the build, the manifests, the ABI checks |
| Toolchain to change a capability | Rust for ColdFire | The same, unless written in C |
| Layering checked | Yes: Rust module paths (`cap::*` does not use core internals) | Yes: object symbols |

B is used only for a reason: today the store upload (#240). Later, a capability that DT2 images should leave out.

## 9. The store upload (#240)

### 9.1 The problem

dnfw patches stock DN2 1.11 with its own machine framework. It and the runtime patch the same sites, so they cannot share an image until
dnfw's machines are digikit modules ([finding 73](../findings/73-dnfw-coexistence.md), decision of 2026-10-28). A dnfw image can read the
store but cannot receive files, because the upload handler is part of the runtime. Users must flash a runtime image, upload, and flash the
dnfw image again.

- dnfw runs on DN2 1.11. `core/runtime` targets `dn2-1.11`, and the MIDI anchors exist there (`devices/ports/dn2-1.11.toml`, `rx` and
  `tx_inflight_lit`).
- dnfw's fixed RAM chunks are already kept out of `cf.expansion` on 1.11: 17 `excludes` in `devices/ports/dn2-1.11.toml`.
- Both projects are AGPL, so linking our object into a dnfw image raises no licence question.

### 9.2 The split

`dkrt_midi_rx` (`upload.rs:1120`) matches `F0 7D 64 6B`, unpacks the body and looks the command up in one 17-entry table
(`upload.rs:1248-1266`); unknown commands go to the module-registered service commands.

| Group | Commands |
| --- | --- |
| Store (the object) | INFO 0x01 (store part), LIST 0x02, CAPACITY 0x04, BEGIN 0x10, DATA 0x11, END 0x12, DELETE 0x13, CLEAR 0x14, ABORT 0x15, READ 0x16, VERIFY 0x17, MKDIR 0x18 |
| Runtime | MODULES 0x03, SETTINGS 0x19, FAULT 0x23, SELECT 0x30, RESTART 0x31 |
| Registered by modules | LOG 0x20, PEEK 0x21, LOGCFG 0x22 (`digikit/debug`) |

The object also holds the store table code (`dkrt_sdk::store`, already `no_std` with `Io { rd, wr }`), SHA-256 and the card rules of
finding 46: writes only inside the store range; the first write to a blank card claims it; a card whose area is neither the store's nor
blank is read-only.

INFO today (`upload.rs:406-441`) returns 41 bytes: version, free space, maximum chunk, a flags byte (safe boot, upload open, card
read-only, debug sink, crash seen), the log range, the packed runtime version, a u32 capability field at offset 25 and the 8-byte image
id. The capability bits are 0 LOG, 1 PEEK, 2 LOGCFG, 3 SELECT, 4 RESTART, 5 MODULES, 6 CAPACITY, 7 READ, 8 VERIFY, 9 SERVICE_CMDS,
10 FOLDERS, 11 SETTINGS (`sdk/sdk.toml`); LOG, PEEK and LOGCFG are set only while a log sink is set. There is no bit for the store itself:
INFO gets one. The flags byte and the runtime facts come from the host image (9.3).

### 9.3 What the host image gives the object

- the MIDI-task hook (`cf.midi.site.rx`) and the TX room check (`cf.midi.site.tx_inflight_lit`; `devices/fw/midi.toml`, per version in
  `devices/ports/*.toml`);
- block read and block write;
- an optional log sink;
- a callback for the INFO fields that belong to the image: image id, device name, safe boot, crash seen;
- a callback after a store change (the runtime updates its store figures there; a dnfw image can do nothing);
- a callback for commands the object does not know (the runtime adds MODULES, SETTINGS, FAULT, SELECT, RESTART and the registered commands
  there).

### 9.4 Packaging

- `src/cap/store_upload` in the runtime crate, with the C-interface shape, then built as a 5th `[[object]]` of `core/runtime`
  (`module.toml` already has four: runtime, tramp, device-dt2, device-dn2).
- We build it and commit the object, as with `runtime.dko.json` (framework objects are committed: `docs/design/module-sdk.md`).
- The interface between the object and the host image has a version: it is an outside contract.
- It is a bridge until dnfw's machines are digikit modules.
- Open: whether the object in the runtime image carries its own copy of the store reader and card gate, or uses the core's through the
  table (section 15).
- Open: how dnfw links a `.dko` (patchkit's linker over their image, their own reader of `.dko.json`, or an ELF object). Asked in the
  reply to #240.

## 10. Device data

### 10.1 The data path

```
devices/digitakt-ii.toml, digitone-ii.toml   values (limits, ids, offsets, sizes, patch sets)
devices/fw/*.toml                             firmware declarations → modules/core/fw (generated)
devices/ports/*.toml                          firmware addresses, by hash
        │  patchkit gen  (the only writer of build/gen/)
        ▼
adapters/<name>/build/gen/{dev}/device.inc    generated, never edited
        │  .include "device.inc"
        ▼
adapters/<name>/src/…                         one device-neutral source per adapter
adapters/<name>/src/cf/dt2/…, dn2/…           per-device code only where the firmware differs
        │  built once per device
        ▼
adapters/<name>/build/<name>-{dev}.dko.json   one object per device
```

Gen kinds today are `gas`, `gas-shared`, `sharc`, `sharc-adapter`, `gas-sample-pool`, `rust` and `c-shared`
(`packages/patchkit/src/devgen.rs`). `rust` and `gas-shared` are one file for all devices; a constant that differs between devices is an
error there (`docs/design/device-data.md`).

### 10.2 One source per adapter

These five adapters have DT2 and DN2 sources that differ only in the `.include` line and comments (compared with comments removed):

| Adapter | Files | Lines each |
| --- | --- | --- |
| dynfx | `src/cf/dynfx-dt2.s`, `dynfx-dn2.s` | 22 |
| dyndsp | `src/cf/dyndsp-dt2.s`, `dyndsp-dn2.s` | 40 |
| fxpages | `src/cf/fxpages-dt2.s`, `fxpages-dn2.s` | 318 |
| input | `src/cf/dt2/device.s`, `dn2/device.s` | 18 |
| dsp-link | `src/dsp/dt2.s`, `dn2.s` | 14, 11 |

Example, `core/dynfx`. Today the two files differ in one line (`.include "device-dt2.inc"` against `"device-dn2.inc"`), and
`module.toml` has two `[[object]]` blocks with the same externs. After:

```toml
[[gen]]
kind = "gas"
path = "build/gen/{dev}/device.inc"     # src/cf/dynfx.s has .include "device.inc"

[[object]]
path = "build/dynfx-{dev}.dko.json"
sources = ["src/cf/dynfx.s"]
place = "cf.expansion"
devices = ["dt2", "dn2"]

[object.externs]
_df_rows = "fx.rows"
_df_first = "fx.df0.index"
_df_count = "fx.count"
```

The output is the same: one object per device. This deletes about 410 lines. Proof: the objects are byte-identical before and after.
Open: `{dev}` in `[[object]] path`, and a per-device include path in the build (section 15).

### 10.3 Real differences that stay per device

- `machines`: DT2 has clone tests, a sort rank, a type argument, a source page and a slot map; DN2 has page rows, type maps, a frame type,
  a type reader, an owner and a group hook (`src/cf/dt2/machines.s`, `src/cf/dn2/machines.s`).
- `dynreg`: DN2 has a list hook that replaces a `jsr` (`cf.machine.hook.list.copy_call`); the `dynreg_info` tables differ in values.
- `params` and `sample-pool`: DT2 only.
- `dsp-voice` and the fx send: separate SHARC wrappers, because the two engines differ.
- The runtime's `src/cf/{dt2,dn2}/device.s`: DT2 lists the firmware file-system functions `cf.storage.fn.path.open/read/close/size`.

## 11. Hooks: today and at boot

### 11.1 Today

All hooks are patch-time (3.3). An adapter's hooks are in every image whose patch set holds it, used or not.

### 11.2 The spike (step 8)

The aim: the runtime writes an adapter's hook branches into the OS code in SDRAM during boot, so the image does not carry them, and only
the hooks of the capabilities that the selected modules use are installed. This is the condition for loadable adapters.

Method:

1. Classify every hook site with a tool under `tools/`: the code region; when the site first runs compared with the install point (the
   reset `init_calls` step, before any task starts); what the hook writes (a branch or trampoline, or a table write); the context (UI
   task, MIDI task, audio interrupt, SHARC). Sites on the reset path, or that run before the install point, stay patch-time.
2. One emulator run: the runtime installs the default set's hooks at boot; compare the RAM at each site with the bytes patchkit writes.
   Identical bytes and the regression suites passing is the proof.
3. A new hook (an FX, a parameter page, a mod page) is one more site through the same classification and comparison. The install
   mechanism does not change per hook.

End state if every adapter site passes: the image holds only the runtime core and the installer (`core/expansion`); adapters are
per-firmware files that the runtime installs at boot, only for the capabilities the selected modules use.

Install at boot only, never while running: the module set changes only at boot (3.7). With hot reload later, a module loaded mid-session
may need a capability whose hooks are not installed; then either all hooks of the image's capabilities install at boot, or hooks install
live. Neither changes the module contract.

### 11.3 What is known and what is not

Known, in favour: the OS runs from SDRAM (bootstrap copy, [finding 1](../findings/01-container-and-patching.md)); nothing checks the OS
image at boot; ACR0 covers 0x40000000-0x47ffffff and is not write-protected; the runtime already flushes caches after loading a `.dkm`
(`cpushl`, `modules/core/runtime/src/cf/tramp.s`); dsp-link already writes DSP code while the DSP runs and then invalidates the instruction
cache (`modules/core/adapters/dsp-link/src/dsp/dsp-link.inc`).

Not known, and what each one changes:

| Item | It affects | If it turns out badly |
| --- | --- | --- |
| Whether the MMU is enabled | ColdFire install at boot | Those hooks stay patch-time, as today |
| Whether the firmware sends the DSP boot stream again | SHARC install at boot | SHARC hooks stay in the boot stream, as today |
| The classification of each site | How many adapters can become loadable | Sites that fail stay patch-time |
| `{dev}` in object paths, per-device include path | Step 7 | Keep the copies, or add the option to patchkit |

None of them changes D1 to D24. They decide how much of the adapter layer can load at boot later.

## 12. Versioning and module settings

- **Runtime:** a semantic version (D9 of `docs/design/module-sdk.md`). While 0.x, MINOR may break the contract major, the store format or
  the SysEx protocol; from 1.0.0, MAJOR breaks, MINOR adds, PATCH fixes.
- **Contracts:** [major, minor] each. They decide whether a module loads. Step 1 changes the `.dkm` format once. Each new capability is a
  minor of the `api` or `machine` contract and only adds.
- **Modules:** `version` (major.minor.patch) is the module's own. The runtime does not read it.
- **The registration table:** internal and built with the runtime, so it has no version. A capability built as its own object gets a
  version in its interface (the store upload first).
- **Adapters:** versioned with the runtime image they are built into.
- **Module settings:**
  - Keys `<module id>/...` in the settings record. A module reaches only its own prefix.
  - The module owns its values and their version: it writes a version field in its own values, and reads older values or resets them. The
    SDK documents this rule.
  - The runtime never interprets a module's values. Unloaded modules keep their entries (`settings.rs`).
  - Values saved with the project (parameters) are the firmware's and are not part of this.
  - The module settings API (get, put, remove within the module's prefix) is a later item; the settings menu (`dkm_menu`) exists.

## 13. Enforcement

### 13.1 Tools considered

- `@nx/enforce-module-boundaries`: TypeScript imports and `package.json` only. The docs say it works with ESLint or Oxlint
  (https://nx.dev/docs/features/enforce-module-boundaries); verify with our Oxlint setup in step 4.
- `@nx/conformance` `enforce-project-boundaries`: checks every graph edge in any language, but needs a paid Nx Enterprise licence
  (https://nx.dev/docs/reference/conformance/overview). Not used.
- Nx language plugins: community plugins for Rust (`@monodon/rust`, tried and removed in the Nx pilot; `@eddacraft/nxrust`) and Python
  (`@nxlv/python`); none for C or assembly. They read manifests, not source imports
  (https://nx.dev/docs/features/multi-language-support). Not used.
- GritQL: JS/TS, Python, JSON, Java and HCL (alpha); no C, Rust or assembly (https://docs.grit.io/language/target-languages). Not used.
- ast-grep: C, Rust, Python, TypeScript; no assembly (https://ast-grep.github.io/reference/languages.html). Not needed.
- Nx project graph plugins (`createNodesV2`, `createDependencies`): free; a local plugin can add projects and edges for any file type.
  Chosen.

### 13.2 The design

- **One rules table** (one file under `tools/devtools/`): which role, category, contract and stability may depend on which. Read by both
  checks below.
- **A local Nx plugin** reads every `module.toml` and adds a project per module with tags `role:`, `category:`, `contract:`, `stability:`.
  Edges come from `requires`, C `#include`, assembly `.include` and `#include` (a line match; no assembler needed), Rust `dkfw::` paths and
  Cargo path dependencies. It also gives `nx affected` and `nx graph` for modules. Check the plugin field names against the installed
  `@nx/devkit` types before writing it.
- **A boundary check** reads `nx graph --file` and the rules table, and reports each edge that breaks a rule, with file and line.
- **TypeScript packages** get tags and the Oxlint boundary rule.
- **layercheck keeps** the rules a graph cannot express: undefined symbols of built objects, apps without `[[op]]` and `[[claim]]`, the
  self-contained rule, `<dk/ui.h>` imports.
- **layercheck adds:** the absolute-address check on content objects (7.2); no device-named include in a source (rule 7); capability
  folders do not use core internals (rule 3); `requires` as code dependencies (rule 6).
- **The gap:** a number typed into a source instead of taken from TOML cannot be found reliably. Review and the AGENTS.md rule cover it;
  one source per adapter (10.2) leaves fewer places for it.
- **Generated files:** an AGENTS.md rule (never edit `build/gen/`; change the TOML and run `mise run gen:modules`); `test:changed` runs
  `gen-check` when a generated file or its source changes; the gate keeps `gen-check`.

### 13.3 Documents

- `docs/ARCHITECTURE.md`: the terms, the layers and rules of section 5, the migration path; fix line 32, which says the runtime runs "in
  the UI and block loops" (its hooks are the UI loop, boot and MIDI RX).
- `docs/MODULE-AUTHORING.md`: category, contract, the minimal manifest, `raw`, experimental adapters, module settings.
- `AGENTS.md`: two short rules (module contracts and layer checks; generated files), linking the documents above.
- `README.md`: a link to the module development guide.

## 14. Steps

Order: #240 and #209 first, and the contract changes once.

Why this order:

- Step 0 lets both contributors start their side now.
- Step 1 comes before both contributor tracks, so their modules land on the final manifest and are migrated once.
- Step 2 is the first use of the capability shape and unblocks uploads for dnfw users.
- Step 3 gives PR #209 a home; its layercheck rules go in before outside PRs land in volume.
- Step 4 must exist before outside PRs land in volume.
- Step 5 moves PR #209 along the migration path, one capability at a time.
- Steps 6 and 7 are internal; neither PR needs them (except the store-upload piece, done in step 2).
- Step 8 decides how much of the adapter layer can load at boot.

The contract stays stable: only step 1 breaks it, once, while the runtime is 0.x. Step 2 adds a protocol bit. Step 5 only adds (minors).
Steps 3, 4, 6 and 7 change no contract; 6 is checked against today's behaviour (regression suites, sizes), 3 and 7 by byte-identical
default images and objects.

- [ ] **0. This plan, and replies to #240 and #209.** Static. The #240 reply agrees to the split with the line of 9.2, answers its three
      questions (yes; we build and commit the object; a 5th `[[object]]` of `core/runtime`) and asks how dnfw links a `.dko`. The #209
      reply explains category, contract, `raw` and experimental adapters, and asks for the three-part split and a rebase after step 1.
- [ ] **1. Contract change, once.** After the sampler branch is merged. About 1 day. Contract: one change.
  - [ ] Check for a free byte in the `.dkm` header for the category, or bump the format version.
  - [ ] `category` enum and `contract` field in patchkit (`module.rs`); `framework` deleted; the packer checks the contract (`dkm.rs`).
  - [ ] The minimal manifest (6.1): migrate the 9 content modules, the fixtures and the 21 `core/*` modules in one change.
  - [ ] Transfer tab and desktop group by category; raw shows as unchecked; the gallery enum matches.
  - [ ] Templates and `module:new` use the category.
  - [ ] Docs of 6.4 updated.
  - Proof: `mise run test:changed`, `mise run test:host`, `mise run web:test`; every content module packs and loads as before
    (`mise run test:conformance`).
- [ ] **2. Store upload (#240).** After step 1. 1 to 2 days. Contract: a protocol minor (the INFO store bit).
  - [ ] `src/cap/store_upload` with the C-interface descriptor; the command split of 9.2; the unknown-command callback.
  - [ ] The host interface of 9.3, with a version.
  - [ ] The 5th `[[object]]` of `core/runtime`; build and commit the object.
  - [ ] Decide: own copy of the store reader and card gate, or the core's.
  - [ ] Decide where the upload test knobs live (`DKRT_UPLOAD_CUT`, `DKRT_UPLOAD_TEAR`, `DKRT_STORE_CAP`, `upload.rs:94` and `:310-343`).
  - Proof: the upload regression tests unchanged; an emulator scenario with only the object (no runtime); dnfw's power-cut scenario when
    they send it.
- [ ] **3. Experimental tier.** About 1 day plus review. No contract change.
  - [ ] `stability` in `[module]`; its own patch set; the merge rules of 7.3 in MODULE-AUTHORING.md.
  - [ ] layercheck: values only through `patchkit gen` (no device-named include); the absolute-address check on content objects.
  - [ ] PR #209 in three parts as experimental adapters.
  - Proof: `mise run check:layers` catches a fixture with an absolute address and one with a device-named include; default images
    byte-identical.
- [ ] **4. Enforcement.** About 1 day. No contract change.
  - [ ] The rules table; the Nx plugin; the boundary check; `requires` as code dependencies and `core/runtime` in the patch sets.
  - [ ] TS tags and the Oxlint boundary rule.
  - [ ] `gen-check` in `test:changed`; the AGENTS.md rules; ARCHITECTURE.md, MODULE-AUTHORING.md, README.md.
  - Proof: the boundary check passes on the tree and fails on one bad edge per rule.
- [ ] **5. The PR #209 capabilities**, one at a time, in the capabilities plan's order. Contract: an api or machine minor each.
  - [ ] Value text per parameter; all 8 knobs (module limits step 4); track sample; stereo voice; chords.
  - [ ] TB303 as a `raw` `.dkm` on DT2, then on the machine contract; then Para and ParaWT.
- [ ] **6. Runtime split** into core and `src/cap/*` (sections 8.3 to 8.5). 3 to 5 days. No contract change.
  - [ ] List every ordering rule of the current `busy()` checks; the DSP link claim flag.
  - [ ] The table and the eight capabilities; the seven ties cut as in 8.4.
  - Proof: runtime object sizes before and after; the regression suites; one health CPU reading.
- [ ] **7. One source per adapter** for the five adapters of 10.2. 2 to 3 hours. No contract change.
  - Proof: every adapter object byte-identical before and after.
- [ ] **8. Follow-ups.**
  - [ ] Hooks at boot: the site classifier, then one emulator run (11.2). If it passes, the runtime installs only the hooks of the
        capabilities the selected modules use, hot-path hooks first, and adapters that no module uses leave the default images.
  - [ ] The cost of each adapter when no module uses it (image bytes, RAM, cycles per block).
  - [ ] The Selache C compiler against hand-written assembly, on one machine.

Later, not ordered: the module settings API (12).

## 15. Open

- **[O]** A free byte in the `.dkm` header for the category, or a format version bump (step 1).
- **[O]** Whether `status` always equals the `[targets]` values (step 1).
- **[O]** The store-upload object: its own copy of the store reader and card gate, or the core's through the table (step 2).
- **[O]** How dnfw links a `.dko` (reply to #240).
- **[O]** A power cut during the very first sector write on a blank card (the claim of finding 46; [finding 46](../findings/46-digikit-debug.md),
  "Card safety"). The object inherits this risk (step 2).
- **[O]** `{dev}` in `[[object]] path`, and a per-device include path in the build (step 7).
- **[O]** The ordering rules hidden in today's `busy()` checks (step 6).
- **[O]** Hooks at boot: whether the MMU is enabled; whether the DSP boot stream is sent again (step 8).
- **[O]** The cost of each adapter when unused (step 8).
- **[O]** Selache C output against hand-written assembly (step 8).

## 16. Not doing

- A tool-specific edit hook for generated files (for example a Claude Code `PreToolUse` hook): contributors use different tools. AGENTS.md
  and `gen-check` instead.
- Nx Conformance: paid licence. Nx language plugins and GritQL: they do not cover C, Rust and assembly together.
- Separate objects for every capability: memory, an ABI per capability and churn for no gain today (8.7).
- Declared needs or capabilities in `module.toml`: derived from the `.dkm` instead (D6).
- Moving settings or the basic stats out of the core (D12).
- Hot reload now (D20).
- Track flags from PR #209: a parameter does the same job.
