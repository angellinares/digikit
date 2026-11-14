# Runtime and services plan

Date: 2026-11-06. Base: `main` at 9a849b7b. Targets: DT2 OS 1.17, DN2 OS 1.12.

Status: **draft for Em's review. The service split and the `.dks` format are not built.** This is the only active plan for this work.

Built so far (SDK steps S2b and S3, runtime 0.7.0, see [sdk.md](sdk.md)): modules call the runtime through named imports (no Api table); a module reaches an optional provider (`core/ui`, `core/input`, `core/usb-bulk`, `core/dsp-link`, `core/sdhc-large`) through a handle, and an absent provider binds the imports to stubs that answer `DK_E_MISSING` and record the missing provider as the module's need; one runtime version with `since` per import and a loader check (same major and minor, patch not higher); and the builder commit (machines, pages and parameters declared in `init`, checked and written at commit). Section 12 item 1 (`since` and the loader check) is done; the text below is the design these parts follow.

2026-11-10: proposals added for Em's review, each marked **Proposal**: DSP cycles as a resource (5.3), sink slots (5.11), the owner of per-block DSP dispatch (6.1), DSP rules, cycle cost and `DK_SPAN` in the contract (now [sdk.md](sdk.md) section 9), and the SDK contract earlier in the order (13.1). They come from the [profiling plan](profiling.md). The order across the plans is in [sdk-runtime-roadmap.md](sdk-runtime-roadmap.md).

2026-11-12: the SDK design moved to [sdk.md](sdk.md). It replaces sections 7.1 and 7.2 and the SDK parts of items 1 and 5.

Evidence: facts come from read-only code reviews on 2026-11-06. Nothing was run. Line counts are approximate. Size figures come from `docs/plans/module-architecture.md` section 4 and `modules/core/runtime/build/runtime.dko.json`, not from a new measurement. Line references are pointers, read around that commit, some from a branch with a Rust format pass. Recheck them when an item starts. `R` means `modules/core/runtime/src/rs/src`.

## 1. Goal

The user installs a small runtime. The runtime provides boot protection, safe mode, module load and unload, and storage access. Where possible, everything else is a service or a module.

The runtime must have no concept of machine, effect, voice, page or sample. A device with only a tool module installed is a valid setup.

We also want a clean layered architecture with one home per concept, clear names and an easy path through the code.

Done means:

1. A layer check fails if runtime source names machine, effect, voice, page or sample.
2. With no modules selected and no loadable services on the card, the device behaves as stock, safe mode works, and modules can be uploaded.
3. A module that needs a missing service is refused with a reason before any of its code runs.
4. Service and module logic is device-neutral. Firmware knowledge exists only in adapters and port data.
5. Every service is a `.dks` file on the device's storage, loaded only when a selected module needs it.
6. When a service is not loaded, its firmware sites stay stock. Safe mode leaves them stock too.

## 2. Terms

| Term | Meaning |
| --- | --- |
| **Runtime** | The always-present core defined in section 3.1 |
| **Service** | A named, versioned interface, such as `samples 1.0`, defined in `sdk/sdk.toml`. It owns record kinds and resources |
| **Provider** | The `.dks` file that implements a service. During migration only (items 7, 9 and 10 in section 13) its code is linked into the image |
| **`.dks`** | A service file for one device and firmware: neutral service code, that device's adapter and a hook table. Uses the `.dkm` format with a role flag and added sections |
| **Hook table** | Data in a `.dks`: for each hook, the site, a hash of the stock bytes, the tail and the handler. Only a `.dks` may hold one |
| **Hook installer** | Generic runtime code: checks the stock bytes, writes the jump, flushes the caches. Knows nothing about machines or effects |
| **Binding** | The workbench step that turns an unbound service object into a `.dks` using the user's firmware |
| **Service registry** | Runtime table: service, provider, version, exports, resources |
| **Content module** | What the user selects: machine, effect or tool |
| **Adapter** | Device-specific firmware glue behind a neutral interface. A service's adapter is built into its `.dks`. The runtime has its own adapters in the image |
| **Adapter ABI header** | The header that declares calls between an adapter and the code above it. Replaces the term "port header" |
| **Resource** | A counted limit: rows, bytes, slots. Older documents call this a pool |
| **Use** | A module's derived demand on a resource. Older documents call this a claim. Not the `[[claim]]` manifest op |
| **Needs** | The services and resources a module requires. Always derived by the packer, never declared by the author |
| **Sample pool** | Only the thing the `samples` service manages |
| **Port** | Firmware-version data in `devices/ports`. No other meaning |
| **Profile** | Device data in `devices/*.toml` |

Word changes:

- "Capability" is dropped as an architecture term. Rename `DK_UI_CAP_*` and the INFO capability bits to feature flags.
- The `type` and `raw` terms go. `category` says what the user sees, and the packer derives what a module contains from what it declares ([sdk.md](sdk.md) section 7). Modules with `type = "service"` today become tool modules. The `service` contract byte goes when the contract bytes go.
- SysEx "service commands" become "module commands".
- A record kind always names its space: register, DSP or desc. Their numbers collide today.
- Concept names in the code get one name each: tracks, sample_pool, pool and sample_source; files, listing and catalog; worker and sample_job; `dyn*` adapters and their owners.

## 3. Target layers

This section describes the target, not today's code.

```text
content modules   machines, effects, tools
services          machines, fx, samples; each includes its adapter
runtime           boot protection, safe mode, load/unload (both CPUs), service registry,
                  storage and files, settings, jobs, upload, log, faults, menu and ui
runtime adapters  platform, dsp-link, storage, ui, input, health
port data         device profiles, ports, anchors
```

The layers say who may call whom. Where the code lives is a separate matter:

| Where | What |
| --- | --- |
| Device storage, loaded when needed | Modules (`.dkm`) and services (`.dks`) |
| Flashed image | Runtime and the runtime's own adapters |

The image holds nothing about machines, effects or samples. This depends on the spike in section 11. Whatever cannot move stays in the image as small stubs, generic where possible.

Rules:

1. Calls go downward only.
2. An adapter calls upward only through a registered callback. A service's adapter is part of its `.dks`, so when the service is not loaded its hooks are not installed.
3. The runtime names no service. It reports events to services, such as "DSP part loaded", instead of calling `sample::ready`.
4. Device branches exist only in adapters and generated profile code.
5. A service has one source home. During migration its code is linked into the image; its end state is a `.dks`. The source and the interface do not change between the two.

### 3.1 What stays in the runtime and why

| Part | Reason |
| --- | --- |
| Loader, arena, relocation, selection, boot planning, service registry | Module load and unload |
| Hook installer | Generic. Installs the hook tables of the services being loaded |
| DSP code load: section parse, relocation, DSP memory, transfer queue | A module has a ColdFire half and a DSP half. About 950 of the 1,462 non-test lines of `dsp.rs` |
| Boot counter, safe mode, fault records, small event log | Boot protection |
| Store access and card write gate | Storage access; the loader needs it |
| Files: paths and listing for both roots | Storage access for modules |
| Settings: 8 KiB key/value record, two copies on the card | The fault record is stored there (`crash.rs:547`). FX keys become the `fx` service's own data |
| Jobs: `worker.rs` queue on the firmware's sample-loader worker | Small and generic. Firmware anchors exist on both devices. Em's call: keep it in the runtime; revisit later |
| Upload | Without it we cannot reach the device. Safe mode needs it to replace a broken module |
| Menu and `ui` | The user must be able to change the selection on the device, also in safe mode |

The boundary may move. If a part sits behind named imports, moving it changes the `sdk.toml` rows, the runtime's import tables and one build file. Modules keep the same names (section 5.7).

### 3.2 What each service owns

| Service | Owns | Runtime code today, approximate non-test lines | Adapter in its `.dks` |
| --- | --- | --- | --- |
| `machines` | Machine types, sticky type-number policy, params, param actions, pages, native pages, voice rows | registry parts ~240; `assign.rs` 113; `machine_keys` 41; `nativepage.rs` 130; `pageclick.rs` ~275; `paramapi.rs` 111; `dsp.rs` voice parts ~220 | `machines` (absorbs `params`, `dynreg`); `dsp-voice` (absorbs `dyndsp`) |
| `fx` | Effect rows, FX pages, FX params, routes, sends, kit value sync | `fxpage.rs` 622; `fxsend.rs` 501; registry helpers ~40; `dsp.rs` FX parts ~250 | `fx` (absorbs `dynfx`, `fxpages`) |
| `samples` | Sample pool, peaks, track records | `tracks.rs` 698; `peakwin.rs` ~290; `sample_source.rs` 148; `dsp.rs` parts ~40 | `sample-pool` |

These are about 4,000 of the runtime's roughly 14,000 lines (tests included in the total).

What the concepts are:

- A **machine** gives the firmware a listed name and a type number. It behaves as a copy of its clone type. There is no ColdFire machine code.
- A **voice** is a DSP row with the same type number, pointing at DSP code. It makes the sound. Only machines use voice rows, so voices are part of `machines`.
- A **PAGE** record is data copied into a table row. DN2 only.
- A **native page** is draw and event callbacks reached through adapter hooks. It requires an owning machine.
- **Pages are not a service.** Machine pages and FX pages use different firmware mechanisms and share only helpers. No page exists without an owner. Shared drawing stays in the SDK library.
- An **effect** is a DSP row, a page, param rows, routes, sends and kit value sync. It has real logic.

**[O] FX without machines.** A user may install only an FX module. But `fxpage.rs` writes param rows through `registry.rs` helpers (`:324`, `:353`, `:424`), and those firmware rows are reserved by `dynreg`, which goes into `machines`. If FX really shares them, either `fx` depends on `machines` or the row reservation needs its own home. Trace this in item 4.

### 3.3 Source layout

```text
modules/core/
  runtime/        the runtime only
  adapters/       the runtime's own: platform, dsp-link, storage, ui, input, health
  services/
    machines/
      src/        neutral code
      adapter/    per-device code and hooks
    fx/           same shape
    samples/      same shape
  apps/           digikit-menu
  expansion/      reset-time installer
  fw/             generated firmware bindings
```

Concept files stay where they are until they are extracted, so each file moves once. Each service becomes its own Rust crate, which makes dependency cycles a build error.

The file is one, but the source stays split. Neutral code and adapter code are separate folders linked at build time, and the layer check stops neutral code from using firmware bindings. If separate files are ever needed, that stays cheap.

`services/` needs a folder role in `docs/ARCHITECTURE.md`, `docs/LAYOUT.md` and `tools/devtools/layercheck.py`.

### 3.4 What users see

Users see only modules. Services are a developer concept for people working in this repo.

- **Category** is declared in `module.toml`, as today. It is what the workbench and site show. It also goes into the `.dkm` header, so the device can read it.
- **Stable or experimental** comes from `[targets]`, which holds a value per firmware port (`untested`, `experimental`, `stable`, `unsupported`). The `status` field goes: the web app reads it but never shows it. Cards and the Transfer tab show the value for the user's device, replacing the site's blanket "Every module is experimental" text (`apps/web/src/pages/index.astro:204`).
- A module shows as experimental if any service it needs is experimental, so it cannot look more stable than what it depends on.
- **Missing service:** the workbench installs it as part of installing the module, in one click. Refusals are worded in terms of the module.

**Crashes.** A module runs in the same memory space as the firmware, with no MMU use found, so nothing isolates it. A service has the same risk. "Experimental" is a warning, not protection. The runtime recovers: a CPU crash records the fault, names the module from the crash address and restarts (`crash.rs:137-229`); after 2 bad boots (`SAFE_AFTER`) safe mode skips the selected modules (`boot.rs:268-307`); a DSP hang restarts into safe mode; bad audio output can switch off a machine type (`watch.rs:305-330`). Whether the crash record survives a restart on real hardware is open [O] (`crash.rs:11-13`).

## 4. Generic record handling

Today the runtime hard-codes seven register kinds (`registry.rs:20-27`, `nativepage.rs:6-9`), with the same numbers repeated in `dkneeds` and `patchkit/src/register.rs`. DSP-section kinds are literals (`dsp.rs:606-608, 967-979`, `loader.rs:85`).

Target: the loader walks a module's records and gives each one to the service that owns its kind. Kinds are declared once in `sdk.toml`, with their owning service. No owner means the module is refused with a reason.

Handler table, from the `DkCap` sketch in `docs/plans/module-architecture.md` 8.3:

```text
service, register kinds, DSP kinds, phase,
check, apply, remove, tick, ready, quiesce, counts, owner_of_row, invariants
```

Changes:

1. `registry.rs:250-299`: kind-to-length table and counts become table lookups.
2. Cross-record checks move into their owner: action to param (`:300-315`), native page to machine (`:316-368`), effect (`:369-388`), page to machine (`:492-507`).
3. `registry.rs` apply and remove (`:581-746`, `:832-920`) split by handler. Removal runs in reverse phase order. The current order is params, actions, pages, machines, then native pages and effects.
4. `loader.rs`: `machine_keys` (`:53-93`), the four contract bytes (`:150-164`) and the calls at `:258, :417-431, :533` become dispatch.
5. `dsp.rs`: voice, FX, bank and track records leave. `sample::ready` and `tracks::ready` (`:1361`) become an event.
6. Readers go through the table: `dkrt_capacity`, `dkrt_fx`, `dkrt_tick`, `table::check`, `watch` attribution (`module_of_*`), the MODULES reply (`upload.rs:445-482`).
7. `dkneeds`, patchkit and `fit` use one generated kind table.

Uses per record kind are declared once in `sdk.toml`, for example one machine record uses 1 machine row, 1 voice row and 1 key. Deriving them then needs no per-kind code.

One walker for register records and DSP sections, from `dkneeds`, serves the loader, host tools and menu. Today there are three DSP-section parsers (`dsp.rs`, `dkneeds`, patchkit) and two contract checks (`loader.rs:148`, `scan.rs:62`). The pin test also covers DSP strides and `FX_LEN`.

**Test of the design:** adding a fake kind, such as an LFO shape, needs no runtime edits.

The sticky type-number code is nearly generic. The runtime keeps "assign a number to a key, in a range a service gives"; the map stays an opaque field in the selection record. `machines` owns what the number means.

### 4.1 R7 as built

R7 changes the header table of the `.dkm` and nothing that runs. The image, relocations, imports and DSP section of every module are byte-identical before and after: `patchkit pack-all` packs every run-time module on each device and `patchkit dkm-identity BEFORE AFTER` checks the parts (102 files: identical). PCM pins and SHARC golden hashes cannot move.

**One kind table.** `sdk/sdk.toml` has `[records.kinds]` (kind id, whole record length, owning service, `generic`), `[records.resources]` (resource ids, with a reserved `DSP_COST` for S5) and `[[service]]` (the in-crate services in call order, with their Rust type). `patchkit gen` writes `records::` (`KIND_*`, `LEN_*`, `KINDS`, `KINDS_<SERVICE>`, `RES_*`, `GENERIC_KINDS`, `KIND_NAMES`, `HANDLE_SERVICES`) into `gen_sdk.rs` and `packages/dkneeds/src/gen_limits.rs`, the static service table into `modules/core/runtime/src/rs/src/gen_services.rs`, the kind numbers for Python and TypeScript into `dkproto.py` and `gen_proto.ts`, and `sdk/include/dk/gen/records.h` for C. No kind number is written by hand any more (`collect.rs`, `machines/records.rs`, `register.rs` and the dkneeds tests lost their literals).

**One service table.** The compile-time `ServiceList` tuple is a `ServiceTable` over `ServiceRow` values (`dkrt_abi::service`): the service's name, its kinds (kind and length from the generated table) and one function pointer for each method that `impl Service` has. The registry, DSP and tick walkers, capacity (`Service::cap(kind)`, so no fixed list of eight), watch attribution and the MODULES reply read it. `patchkit objdiff` against the commit before lists only the functions that touch the table (`ServiceList` methods and `CapSink::add` gone, `ServiceTable` methods and one `<S as Service>` method per row and method new, `record_len` and the walkers changed); the loop bodies are the same. The runtime object after R7 (all steps): text 196,290 to 212,474 bytes, data 79,250 to 80,158, bss 712,796 to 719,536. About 12 KiB of the text is the planner, the declare check and the generic collector; the rest is the table.

**Generic records.** `dk_record_add(ctx, kind, bytes, len)` (runtime 0.8.0) copies a record of a `generic` kind into the collector (at most 8 records and 640 bytes, `COLLECT_BYTES` 7206). `lower` appends it to the `register` section and the owning service checks and applies it with the walkers it already has. A typed kind (machine, page, effect, ...) stays a builder call: `dk_record_add` refuses it with `DK_E_FORMAT`.

**Header table.** The table at the header pair 0x90/0x94 is a versioned, tagged table (`packages/dkneeds/src/keys.rs`): magic `DKTB`, version 1, then entries NEEDS, USES, KEYS, KINDS, SLOTS, HOOKS, EXPORTS and RESOURCES. The packer writes NEEDS from the linked imports, USES from `[uses]`, KEYS from `[[machine]]`, `[[fx]]` and `[[record]]`, and writes at least 0.8.0 as the needed version. The R12 entries (KINDS, SLOTS, HOOKS, EXPORTS, RESOURCES) are in the format, parsed, planned and compared with `init`'s declarations; module.toml has no syntax for them yet, and the loader does not host a kind a module provides (R12). A file of the 0.7 format is refused with `E_CONTRACT`, a damaged table with `E_FORMAT`.

**Boot planner.** `plan.rs` is a pure function over the header tables of the selection. It orders by dependency (NEEDS and EXPORTS, KEYS and KINDS), then by selection index; refuses a hard need nobody provides (`E_MISSING`) or a newer version (`E_CONTRACT`); checks one owner for each kind, slot and hook site and unique keys of a generic kind (`E_PLAN_OWNER`, `E_PLAN_KEY`); refuses keys for a kind no service owns (`E_PLAN_KIND`); and sums USES against the capacity with `dkrt_fit::fits_now` and `take` (`E_PLAN_FIT`). The keys of the typed kinds keep their own rules (a machine key twice takes a fresh type number, an effect id twice is `E_FX_ID_DUP` at the load). The verdict type is `dkrt_abi::plan::{Verdict, Reason}`, defined once; R11 builds the module states and their UI on it. The boot keeps the table of each file while `assign::plan` reads it, loads in the planner's order and does not load a refused module. A service that an image provides (core/ui and the like) and that is missing stays the loader's `E_IMPORT`, with the missing module's name: the planner does not know the image.

**Declare, check, commit.** An in-crate service declares its kinds in `Service::declare`; at init the runtime compares them with the generated table, and a difference marks the service FAILED (`E_DECL_KIND`, logged as a `MOD_LOAD` of index 0xffffffff; its kinds are unknown to the registry). A service module declares with `dk_service_declare_kind`, `_slot`, `_hook`, `_export` and `_resource`; the commit compares them with the KINDS, SLOTS, HOOKS, EXPORTS and RESOURCES entries and refuses a difference with `E_DECL_KIND`.

**The new-kind proof.** `src/services/lfo/` owns `lfo_shape` (a shape id and 32 samples, in an owner-tracked table of eight). The commit that added it changed `src/services/lfo/`, one line of `src/services/mod.rs`, one `[[service]]` and one `[records.kinds]` entry of `sdk.toml` with their generated outputs, and tests (host tests, two fixtures, one registered device test): no edit to the registry, the collector, the loader, dkneeds or patchkit. It has no firmware effect. The run-time proof with a `.dks` comes with R12.

**Gate list for R7 (named, targeted).** R7 changes the runtime object, the `.dkm` header and the boot path, so the affected gate selects almost every device test. A named list covers the same boundaries with fewer runs, both devices each: `core/runtime-lfo-tests:test-device` (the new proof), `core/runtime-service-tests:test-device` (a head-only table), `core/runtime-fixture-tests:test-device` (a machine and an effect load), `core/runtime-tests:test-device--<file>` for `modules-capacity` (boot with modules, MODULES and CAPACITY, the planner), `safe-mode`, `boot-restart`, `upload`, `transfer-live`, `store-format` (the Transfer group reads the new table) and `platform-contract`, the module tests of `digikit/pluck` (a machine with pages and rows), `digikit/freqshift` (an effect), `digikit/shot` (the samples service), `digikit/debug`, `digikit/usb-fast` and the native-page fixture, and `pnpm nx:pilot run-many -t test-conformance`. The skipped tests do not cover another path of R7: the other machines and effects share the loader path with these. The full tier runs once after the merge.

**Not in R7.** Three DSP-section parsers remain (runtime, dkneeds, patchkit); the machine-only tables of the registry (`counts`, `types_of`, `free_rows`, `profile`) move with R9 and R10; the hosting of a kind or hook that a module provides is R12.

## 5. Service model

### 5.1 Registration is by declaration

A `.dks` declares what it provides: service name, version, record kinds, resources, exports and its hook table. The runtime fills the registry during boot planning, before any provider code runs. There is no run-time registration or lookup call. During migration, linked providers appear in a generated table with the same shape.

Reason: the menu and Transfer tab can say "does not fit" without running code, a refusal needs no rollback, and the packer can derive needs.

### 5.2 Needs and discovery

- A module uses a service by importing its functions or holding its record kinds. `sdk.toml` maps each import and kind to its service. The packer writes the needed services and versions into the `.dkm` header and report.
- Provider files use one path per service, such as `services/samples.dks`. No directory scan is needed. Check the folder name against `docs/design/digikit-store.md` and the 159-byte store path limit.
- There is one provider per service. Services are never bundled into content modules, and "first registration wins" is rejected.
- Services load only when a selected module needs them, transitively, in dependency order, and before the modules that use them. Load order is by dependency, then id, not selection order.
- A provider that fails makes its dependents REFUSED. Safe mode skips loadable providers and their dependents.
- The registry checks hook tables during planning. Two services claiming one site are refused before any code runs.
- [O] A module must be able to mark an imported service as optional (usb-fast imports sdhc_large but runs without it). The packer then writes only the hard needs into the `.dkm` header, the registry refuses a missing hard need before any module code runs, and stubs stay only for optional services. Until the registry exists, the loader records the need at `init` time (sdk.md 5.1).

### 5.3 Resources

There is one rule for all. A service offers capacities. Every module and provider has derived uses. The runtime offers arena and DSP bytes. `machines` offers rows, `samples` offers pool slots. The adapter reports the device limit from the profile. Services have limits too: a provider's own arena and DSP use counts like any module's.

The broker sums uses per resource, checks exclusive keys for duplicates (module id, `<module>:<machine>`, effect id, settings prefix) and checks versions. It names no service. The same function runs in the packer, Transfer tab (wasm), menu and loader; it extends `dkrt-fit`. Today the loader enforces limits with separate code that shares constants and a few helpers (`loader.rs:183-262`, `dkrt_sdk::capacity`).

Dynamic demands stay run-time calls that return errors, such as PCM memory or `dk_alloc`.

**Built (S5a, [sdk.md](sdk.md) section 9):** DSP cost per block is a derived use. `module:check` writes each module's worst steady block into `[uses.<device>]`, and the packer puts it into the header as `DSP_COST_VOICE` (a machine, per voice) and `DSP_COST_FX` (effects, summed). The broker (one no_std function, `dkrt_fit::check_selection`, in the boot planner, the menu and `patchkit fit`) accepts a selection when `max(per-voice costs) * voices + sum(effect costs)` stays within the headroom of the generated device profile; a module that goes over is refused with `E_PLAN_FIT` and the kind `DSP_COST`. It names no service.

Limits today: arena 512 KiB and `.dkm` 64 KiB on both devices; 8 machine rows and 16 param rows on both; 3 FX slots on both; send slots 2 on DT2 and 1 on DN2; page rows 0 on DT2 and 8 on DN2; DSP area 512 KiB on DT2 and 320 KiB on DN2.

### 5.4 Provider slots

`MAX_MODULES` is tied to the selection record through `SEL_MAX` (`sdk.toml:983`). Keep 8 selected modules, and give providers separate table slots without changing that record. Each slot costs up to 3.6 KB today, mostly listing state, which section 9 reduces. Owner fields are `u8`, so 16 entries are fine.

### 5.5 Adapters and hooks

A service does not name hooks. A neutral service calls `machine_add` or `voice_install`, and its adapter implements them per device. Hook sites do not translate one-to-one: of the 23 `machines` hook ops, 12 are DT2-only, 9 are DN2-only and 2 are shared.

A hook has two parts. Install is generic and belongs to the runtime's hook installer. Meaning (which sites, and code that reads firmware structures) belongs to the adapter inside the `.dks`.

At boot, a selected module needs a service. The registry reads the service's hook table as data. The installer checks each site's stock bytes, writes the jump and flushes the caches before modules start.

What this gains:

- Safe mode skips services, so their firmware sites stay stock.
- A hash mismatch refuses the service with a reason instead of patching the wrong code.
- Hook conflicts are found while planning.
- Most new ColdFire hooks need no reflash.
- A `.dkm` holding a hook table is refused, so content modules can never install hooks.

Hooks scale: no content module has a hook today. Most hooks are one hook plus a table of users. Another machine or effect needs no new hook. Only something new to the firmware needs a new adapter.

The runtime's own hooks (boot, UI loop, MIDI, and those of its other adapters) stay patched. So do hook sites that run before main-OS entry. What cannot move is decided by the spike. **Acceptance test:** anything that stays in the image is harmless with no service loaded.

Services call their adapter directly, linked in the same file. Services call runtime adapters through named imports, which already works for `core/ui`, `core/input` and `core/dsp-link`: an `sdk.toml` import with `requires` and `absent`, plus the runtime's `optional` list (`lib.rs:461-482`, `layercheck.py:489-498`).

### 5.6 Versions between provider and runtime

Providers are built and released with the runtime. The provider-to-runtime interface needs no compatibility promise, only an exact version match. Only the module-to-service interface carries versions (section 12). The workbench installs the runtime and services as a set, in one click. A mismatch is refused with a reason.

### 5.7 Moving code later

A `.dks` uses the `.dkm` format and tools, so the loader, hash check, upload and fit check stay one piece of code. It adds a role flag and sections for what it provides, its exports, its hook table and the firmware it is built for. The spike settles the details. Moving a service from linked to `.dks` changes `sdk.toml` rows, runtime import tables and one build file. Modules keep their import names. `tracks.rs:41-52` calling `dkpool_*` is the working pattern. Loadable also needs what `.dkm` lacks today:

- An export table. None exists. Estimated at 36 B per export. The format word must change (`loader.rs:125`). The header has precedent for growth (0xa4 menu extension, `:129`).
- Import resolution against loaded providers (`lib.rs:465-482`).
- Dependency order in boot (`boot.rs:317-342` is selection order).
- A Rust path to `.dkm`. It looks possible but is untested. The Rust build accepts only `R_68K_32` (`build/rust.rs:592-600`), which matches `cf.32`. Fixtures `rust-demo` and `dyn-demo*` exist and have not been read.
- Adapter code in the arena. Adapters link at fixed addresses today (`apply.rs:1003-1011`); `.dkm` relocation looks suitable but is untested for tails and trampolines.
- The 64 KiB limit may need raising or removing. It is a staging buffer limit. `machines` and `dsp-voice` output sizes are unmeasured.

A `.dks` is one file per device and firmware. Modules stay `.dkm`, packed per device where their records or DSP code need it.

### 5.8 Build and install

A hook's tail is firmware instructions copied from the image, so a finished `.dks` contains short pieces of Elektron's firmware. Our rule is never to commit firmware or derived material, and shipping it would be worse.

Flow:

1. We build unbound service objects and port data. They contain no firmware bytes.
2. The workbench binds them against the user's own firmware, the same flow as patching the image in the browser. It generates the tails and writes the finished `.dks`.
3. The workbench uploads the runtime and the `.dks` set to the device.
4. The user uploads modules. Services load only when a selected module needs them.

Not checked yet: whether the web patchkit can link these objects. The spike answers this.

**Device constants reach only the modules that use them.** **[D]** Each `[[gen]]` entry of kind `rust`, `gas-shared`, `gas` or `sharc` lists the constants it uses (`uses` in `module.toml`). The generator writes only those, and refuses a listed name that matches nothing or that the module's sources never reference. A new constant then changes only the includes of the modules that list it. The device profile stays the one home of device constants (future devices need only a profile). Each `.dks` will declare its own list the same way, because it is bound per device and should depend only on the profile entries it reads.

This does not by itself narrow test selection: a profile edit still selects every device test, because three inputs hash the whole profile. The emulator's device-profile crate embeds both profiles (`packages/emulator/device-profile/src/lib.rs`), so the daemon changes. The patchkit CLI embeds them. The device-test inputs list `devices/**/*.toml` (`READ_BY_TESTS` in `tools/nx/device-test-keys.mjs`). The runtime and the adapters are in every test image anyway. Narrowing selection is step 4 of the [test speed plan](test-speed.md) (device tasks keyed on built bytes).

### 5.9 Report

The packer writes one report per file, in the file and in build output:

```text
pluck 1.0.0   category machines
needs    machines 1.2
uses     machine.rows 1, voice.rows 1, param.rows 2, dsp.bytes 1856, arena 32
keys     machine pluck:pluck
```

Readers: reviewers (CI output), the Transfer tab and menu (fit before restart), and the loader. An experimental dependency appears here too.

### 5.10 New firmware plug-in points

A new plug-in point starts as an experimental `.dks` with its own hook table, and needs no change to the image. Modules import from it by name, and the report shows "experimental". It is not part of the default service set. When proven (two users, or Em's call), the mark comes off and module sources do not change. Only we build `.dks` files for now, because they carry firmware-level power. Whether third parties can write them is a later decision.

### 5.11 Sink slots

**Built (R2d, runtime 0.7.1).** The runtime has numbered sink channels, `log` (0) and `span` (1), in `dlog.rs`. The import is `dk_sink_set(channel, sink)` ([sdk.md](sdk.md) section 5.1, `<dk/core/sink.h>`).

- The runtime keeps one pointer and one owner per channel and names no service or kind. The owner is the calling module (its source index). Producers pass opaque owner ids (`owner_of_row`) in span records.
- One owner per channel. A second owner gets `DK_E_BUSY` from `dk_sink_set` at the call, not while planning (a module sets its sink in `init` or later, so the table cannot know it).
- The channel is cleared after the owner's `fini` and after a failed `init`.
- `log` receives every event the runtime logs, in addition to the older `dk_debug_log_sink_set` pointer, which is unchanged (the debug module has not moved to the channel; there is no ring replay on the channel).
- `span` receives `(owner, cycles)` records from `dlog::span_emit`; no producer exists yet (`DK_SPAN`, S5).
- With no sink set, a producer pays one word test per channel and calls nothing.

Users: `digikit/debug` (`log`, later), `profile.dks` (`span`, [profiling plan](profiling.md) section 7). The probe's per-block `dl_set_tick_cb` slot in dsp-link can follow the same rule later. Usage for a sink owner: [INSTRUMENTATION.md](../INSTRUMENTATION.md).

### 5.12 Service anatomy

A service is a module with a role.

- Same as a module: `module.toml` with `role = "service"`, the folder layout (`src/`, `tests/`, `build/gen/`), the `dk_module` entry point with `init`, `fini` and `tick`, imports through `sdk.toml`, the naming rules of sdk.md section 10, the declare-check-commit pattern of sdk.md 6.3, and tests that live with the service.
- Only a service has: exports (`sdk.toml` imports with `requires`), a hook table, slots, record-kind handlers, and its adapter linked into the same file.

The contract is the ABI in `sdk.toml`, so a Rust service uses the generated bindings, as the runtime does today. The target firmware is bound at bind time (5.8), not written in the manifest.

**Manifest.** The service section of `module.toml` has: `name`, `exports` (a group of `sdk.toml`), `record_kinds`, `resources`, `[[hook]]` (a ColdFire site that the service installs, movable) and `[[slot]]` (a fixed site, ColdFire or SHARC, that the service fills). The file format of a `.dks` waits for R12. R2 only writes the schema here and in `docs/ARCHITECTURE.md`.

**Slot.** A hook has two shapes ([finding 128](../findings/128-r4-hooks-in-services.md)). A movable ColdFire hook is installed by the service. A fixed site stays in the image or the runtime and calls through a pointer that a service registers: the SHARC stubs, the page key and turn hooks, and any hook that runs before main-OS entry. That fixed site plus its pointer is a **slot**. The registry treats slots and ColdFire hook sites alike in its one-owner check: a second claimant is refused while planning. A zero slot means the stock path, so a device with no service behaves as stock. The page key and turn hooks therefore stay in the runtime, and the `machines` service fills their slot.

**Three interfaces.** The layer picture, the rules between layers and the crossings to remove are in [ARCHITECTURE.md](../ARCHITECTURE.md); this section gives the detail.

- **A, module to service.** As in [MODULES.md](../MODULES.md) section 4: imports named by service group in `sdk.toml`, `dk_<service>_open` handles, `DK_E_MISSING` for a service that is absent, and needs derived by the packer.
- **B, runtime to service.** Derived and declarative, the same rule as S3 for modules. The service's `init` declares through builder calls (record kinds, slots, hooks, exports, resources), and these calls are the only source of truth. `module:check` and the packer write a static table into the file. Boot planning reads only that table (dependency order, fit with the service's needs added to the module needs, slot and hook conflicts) and runs no code. At `init` the runtime checks the declarations against the table and commits them; a mismatch means FAILED. The fixed entry points are: lifecycle, a handler table per record kind, slots and hooks, broker resources, and the log and span sinks (5.11). The kernel never names a service.
- **C, service to runtime.** The same `sdk.toml` imports as modules, plus a small service-only set (registry queries, installing slots and hooks).

`check:layers` enforces that the rest of the runtime reaches a service only through these interfaces (added in R2c; [ARCHITECTURE.md](../ARCHITECTURE.md) says what it checks today).

**Template.** In the runtime crate a service is a folder `src/services/<name>/` with `mod.rs`, `records.rs`, `slots.rs`, `dsp/` and `tests/`. It maps one to one to `modules/core/services/<name>/{module.toml, src/, adapter/, tests/}` when the services are linked in (R9 and R10), and `module:new service` generates it at R12. R2 moves code only: no behaviour change. R7 proves that interface B is generic.

## 6. DSP

Three concepts are mixed today:

| Concept | Today | Target |
| --- | --- | --- |
| Transport: moves bytes using hooks | `dsp-link` adapter, ~900 lines. Names its users (`_dl_vrows`, `_dl_frows`, `_sp_tail`) | `dsp-link` adapter with window registration. It stops naming users |
| DSP code load and memory | `dsp.rs` | Runtime, with one transfer queue (R6a: `link.rs`) |
| What goes in DSP memory: voice rows, FX rows, sample tables | `dsp.rs`, `fxpage.rs`, `fxsend.rs`, `tracks.rs` | Services, through the link API (6.2) |

Only one transfer can run at a time (`dsp-link.c:133,153`). Before R6 there was no queue: writers checked each other's `busy()` and the status was shared with no owner token (`dl_poll`, `dsp-link.c:168-182`).

`watch.rs`: keep the heartbeat and hang record in the runtime. Move voice and FX guards and attribution to their services. Boot-counter safe mode does not use `watch.rs` (`boot.rs:254-309`); hang records do (`crash.rs:449`). The DIAG words move to a shared crate to break the `crash` and `watch` cycle.

`_dv_exp2` is exported by `dsp-voice` to machine DSP code. DSP-side imports are part of the `machines` contract and need to be declared as such.

### 6.1 Per-block DSP dispatch

Today the per-block calls into machine and FX DSP code sit in the `dsp-voice` and `fx` adapters (28 and 2 SHARC hooks, section 11.1). This plan does not yet say who owns that dispatch in the target.

**Decided** ([finding 128](../findings/128-r4-hooks-in-services.md)): SHARC hooks cannot move into a `.dks`, so the image keeps every SHARC hook site as a thin stub. The stub jumps through a pointer slot; a zero slot runs the stock path. Each kind's per-block dispatch code and rows belong to the service that owns the kind: `machines` for voice rows, `fx` for effect rows, and a new kind's service brings its own. A service loads its DSP code at run time (already relocated by the runtime) and writes its entry into the slot. `DK_SPAN` ([sdk.md](sdk.md) section 9) and the DSP stack guard (section 8) live in the stub, so content modules need neither. The stub overhead per block is checked in R8 whole-path spike.

### 6.2 R6: the DSP link queue

R6 has two steps. **R6a is built; R6b is planned.**

**R6a, the link API in the runtime core (built; no ISR change, no SHARC change, no wire change).** The core owns the link (`link.rs`). The wire protocol stays as it is: one transfer at a time through `dl_send`, `dl_send_win` and `dl_poll`. R6a adds who may send and in what order.

- **Live values.** One table of desired state. A slot is a window and a word offset. `link::set(slot, value)` keeps the newest value with a version and a dirty bit. Several sets before the next send give one write of the newest value. A write that failed (the link refused it, or reported an error) stays dirty and is sent again after a short hold. Writers: the effect pages' knob and MIX words, the send amounts.
- **Owner requests.** `link::submit(owner, stream)` returns a `Ticket`. `link::result(ticket)` answers its owner only, once. There are two kinds of stream:
  - *Bulk*: bytes, either copied into the queue (at most 28 bytes) or a buffer the caller keeps valid until the result is taken or the owner is drained. A bulk is sent in chunks of 4 KiB.
  - *Sequence*: word writes in order, at most 18 (an effect row). The last word publishes (the voice row's type, the effect row's pc). A step may carry an undo word. When a transfer of a sequence fails, the steps with an undo word that were sent are undone in reverse order, then the request fails (`Failed`, or `RollbackFailed` when an undo failed too).
  - Writers: the loader (the module image as a bulk, each row as a sequence), the sample track writer (each table write as an owned bulk), and the core's own clears (`link_run`: submit a sequence and wait; the effect row clear is one sequence, the voice row clear one word).
- **Retained state.** As before the queue, a retained loader fails an area send (the answer -2 of `dl_send`, which fails the image or a track write) and holds word writes (they wait; a blocking `link_run` gives up at its spin bound). Nothing is polled or sent while retained.
- **Order and priority.** Requests are sent oldest first, so each owner's requests stay in order. Priority: an undo first, then a dirty live value, then the oldest request. A live edit therefore goes between two bulk chunks and between two steps of a sequence.
- **Drain.** `link::drain(owner)` cancels the owner's queued requests and waits for the transfer in flight (bounded). `dsp::quiesce` drains the loader before a module's rows are cleared, so no transfer reads a buffer that is about to be freed.
- **Confirmed state on the ColdFire side.** The link reports DONE when the DSP has taken the bytes and copied them, so the confirmed version is the version that was sent. `link_confirmed(slot)` gives the value. A DSP restart marks every slot dirty and clears the confirmed values (`dsp_restarted`; no event source calls it yet, see the open items below).
- **The pump runs in the UI task** (`dkrt_tick`, before the loader step and the service ticks) and in the blocking waits (`link_run`, drain). It never runs in an interrupt.
- **Windows.** The window field is 2 bits: 0 the area, 1 the voice rows, 2 the effect rows. Window 3 is reserved for the profiler's statistics (P8); no writer may use it (`submit` and `set` refuse it). The direction of the statistics (DSP to ColdFire needs a read path) and their transport are decided in P8, not here.
- **Observability.** Each request logs submit, start, done and error, and a live word logs resend and taken, as the `DSP_LINK` event (kind 46) through the log sink of R2d. Without a sink the cost is one flag test. There is no span event: the span sink carries `(owner, DSP cycles)` records of the profiler, and a link request has no cycles. Its duration in blocks would be a second meaning in a channel with one, so the log event is the only record of a request.
- **The writers use only this API.** The `busy()` cross-checks and the interim claim bits (`LINK_CLAIM`, `link_busy`, `link_claimed`) are deleted. Services reach the link through `CoreApi::link_*`; the core calls it directly. The finding-129 hazard cannot occur: no interim fix was made.

Decisions:

- The pump runs in the UI task for R6a. The frame tick (`dl_tick`, 1500 Hz) is an interrupt and stays unchanged.
- The confirmed state is kept on the ColdFire side. A DSP echo of the version would cost reply-record space that is not free.
- Windows stay 2 bits wide, with window 3 reserved.
- Live edits go between bulk chunks (a priority policy of the pump), not between whole requests.

Open items of R6a: no event source calls `dsp_restarted` yet (the DSP restart is not observable in the runtime today); the module image is the only chunked bulk, and its chunk size is not tuned (4 KiB, one transfer of about six END frames each).

**R6b, a batched state-sync transport (planned, not built).** Each block's frame tail carries a batch of `(window, offset, value)` taken from the desired-state table. One acknowledgement confirms the batch, and there are no END frames between batches. It needs SHARC-side changes to `dsp-link` and an ISR A/B measurement first (`tools/devtools/coupled_ab.py`, the host instruction ratios). Trigger: the modulation rate. One word costs about 8 blocks today (about 190 words per second), so R7 (LFO) or P4 needs R6b when the modulated words per block exceed that. The API of R6a does not change: `set` and `confirmed` stay, only what the pump does with a dirty slot changes.

**[D] The latent bug of the shared result** ([finding 129](../findings/129-dsp-link-result-ownership.md)): link results had no owner, so a writer that had not polled yet could read the result of the next writer. R6a removes it by construction.

## 7. SDK

The SDK design is in [sdk.md](sdk.md): layers and tree, the `dk_module` entry point, builder calls in `init` with a commit after `init`, the boundaries, raw modules, the DSP contract (DSP rules, cycle cost, `DK_SPAN`), style, tests and the steps S1-S5. It replaces the former sections 7.1 (Cleanup) and 7.2 (Contract).

## 8. Lifecycle

Today's gaps:

- `dkm_init` runs before DSP code is on the SHARC. C cannot see when a machine's DSP is ready (`loader.rs:366-392`).
- `dkm_fini` can run for a DSP part that never ran (`loader.rs:451`).
- A hang resets without `dkm_fini` (`watch.rs:356-417`).
- A machine has no DSP init hook. Globals come from the image, and only `state_clear_bytes` is zeroed at note-on.
- Voice-steal behaviour is undocumented. `core/dsp-voice` has not been read for it.
- `worker::retire` returns busy and keeps the module memory; nothing retries it (`worker.rs:278-299`).
- `unload_rec` runs only on the DSP failure path (`loader.rs:451`, `dsp.rs:1256`), despite covering the full teardown.
- No single document describes any of this.

Target: one state model for modules and providers.

```text
SELECTED -> PLANNED or REFUSED(reason)
PLANNED -> INIT -> DSP LOADING -> ACTIVE
any stage -> FAILED(reason)
safe mode -> SKIPPED
```

- A provider must be ACTIVE before its dependents start. Provider DSP parts load asynchronously, so planning must order this.
- The MODULES page and Transfer tab show the reason ("REFUSED: no FX slot, used by master-drive"). Device tests assert the state, not log text.
- Proposal, not decided: an optional "DSP ready" call to the module.
- Refusal fixtures: one must-fail and one must-pass `.dkm` per loader and packer error code. A test fails when a code has none.
- Ordered event tables: a runtime table per event (tick, key, encoder, project changed), with entries from records `{event, order, fn}`, sorted by order and then module id. For input, the first non-zero return wins and hides the event from stock. This generalises `dkm_menu` and slot 0. Slots 1-7 are bound but never called (`lib.rs:1155-1157`); delete or use them.
- DSP stack guard: a guard word per module's DSP stack. On overrun, mute that voice or effect. Finer than today's DSP hang restart.
- Unload at restart, built on `unload_rec`, using the same teardown as the failure path.
- MISSING placeholder: a saved machine type or effect from an absent module keeps its id, shows MISSING and plays silence. Projects need this. First check statically what the firmware does with a type number whose row is empty.
- Hot reload stays parked, archived at tag `digikit-hot-reload-v1`.
- `MODULE-AUTHORING.md` gets the lifecycle section after this is built.

## 9. Storage and files

Two concepts exist today:

- **digikit store:** same code on both devices, with a different card area. The firmware block read is reached by anchor (`lib.rs:221-223`). The runtime needs it at boot: selection record, settings, `.dkm` files.
- **Firmware files:** DT2 only. Read-only path filesystem, the user's sample files. On DN2, samples live in the store under `samples/`.

R13 built one neutral `Storage` trait in the runtime's Platform composition: stat (size, identity hash with its kind, entry kind), read head, read range, read a whole file, and paged listing with caller-owned state and the steps Entry, Pending and End. Paths are root-relative with per-device limits. `dk_file_id` carries a hash kind (sha256 on DN2, Elektron32 on DT2). The storage adapter (`storage_adapter.rs`) holds two routes: the store route and the firmware-filesystem route. Profile data chooses the route for each root. The device branches in `tracks.rs`, `files.rs` and `sample_source.rs` go through the trait. Listing memory comes from the arena, for at most 2 listings at once. `dkrt_sample_fs` and `dkrt_files_fs` are defined in the adapter's `src/cf/<dev>/storage.s` (DN2: zero tables) and read only by `storage_adapter.rs`. Settings stay on the block path.

Storage is not the whole device difference for samples. `sample-pool` also handles memory placement, DSP-visible addresses, DN2's DSP receiver and hooks, and DT2's borrowing of the stock pool. The DT2 adapter also reads files through the firmware directly (`dt2/pool.c:112-149`). So `samples` depends on the runtime's `storage` interface and contains the `sample-pool` adapter. `dkrt_pool.h` is close to the right adapter ABI header; `dkrt_project_changed` is declared only in the generated `dkrt.h`.

Memory cleanup: files state is 33,980 B in `.data`, about 42 KB with scratch, for 8 module slots of 3.6 KB each. It ships as zero bytes in the image. One active listing needs about 4.3 KB (estimated). Allocate on first use. Tracks state (`CORE`, 35,838 B) is similar, mostly zeros in `.data`. Check how much of the runtime's `.data` is mostly-zero state.

## 10. Runtime cleanup

**Deletions and single sources:**

- Sample bank API 1.3: **done.** The runtime, the fixture and the SDK entries are gone, and the host side (`SampleBankDef`, the bank record in the DSP section, the kind 1 branch of `dkneeds`) was deleted in R2a. `sdk/dsp/sample/bank.inc` stays: SHOT includes it. The DSP v5 record keeps kind 2, the track table.
- One generated source for record kinds (section 4), built in R7 with the generic records. The `fxpage::MAX_FX` literal is gone (R2a); the kind literals remain (`registry.rs`, `collect.rs`, `fxpage.rs`, `nativepage.rs`, patchkit `register.rs`).
- After a caller check across modules, packages, apps, tools and tests: the `dkrt_store_list` extern, the `dkrt_settings_get/remove` externs and the second poll registry are **deleted** (R2a). `State.log` stays until `tools/patchkit/pk_dkrt.py` reads `dlog` instead (it reads `ST_LOG`). `selection_set_one` stays: it is the mailbox test hook (`lib.rs`, `pk_dkrt_boot.py`, `safe-mode.device.test.ts`), and only `boot.rs` duplicates two lines of it. No hot-reload leftovers remain: `STATE_UNLOADING`, `E_BUSY` and `E_TYPE_IN_USE` stay as reserved numbers and the retire paths of `loader.rs` are in use.
- One capacity table used by `dkrt_capacity`, `dkrt-fit` and patchkit: **done** (R2a), `[capacity.kinds]` in `sdk.toml` with `fit` and `label`, generated as `CAP_KINDS` and `CAP_FIT`.
- Fix stale text: "emulator only" in the runtime, health, dynreg and dyndsp summaries; runtime README "Version 0.1.0"; `lib.rs:1-9` crate doc; `settings.rs:9`; `crash.rs:16` (names `dlog.rs` for `watch.rs`); `tracks.rs:5`; patch-set comments in `devices/digitakt-ii.toml:181-182`; `[targets]` "untested" on shipped `dsp-link` and `sample-pool`; `abi.toml` `STATE_UNLOADING` and `CAP_MAX` docs; the runtime `module.toml` summary.
- Proof adapters: `dsp-ddr-proof` and `dsp-lp0-proof` are already gone. `dsp-stereo-proof`, `file-catalog-proof` and `storage-fault-proof` stay: no shipped test covers their paths (`dsp-stereo-proof.test.ts`, `file-catalog.test.ts`, `storage-fault-proof.test.ts` and the patchkit `storage_fault_*` tests use them).
- Check by symbol before removing: whether tools read `State.log`, `DKRT_REG`, `DKRT_DSP`, `DKRT_FXP` or `DKRT_FXS`. `dkrt_load` has a caller in `tools/patchkit/pk_dkrt.py:429`. `dkrt_project_changed` has no C caller found; check assembly and project work first.
- `sample_stream.rs` is deleted (R2a); add it back when something uses it.

**Runtime split:** `lib.rs` into state, imports, api, tick, health, report, mailbox. `boot.rs` into selection, safe mode, boot. `upload.rs` into transport, store commands, admin commands, module commands. `dlog.rs` and module context. `crash.rs` becomes `fault.rs`, `table.rs` becomes `invariants.rs`. Split `apps/digikit-menu/src/menu.c` by page. Split crate `dkrt-sdk`, whose name wrongly suggests it is the SDK: R2c created `dkrt-abi` with the interfaces (`CoreApi`, `Service`) and the pure types they share. Renaming `dkrt-sdk` to hold only the host-shared ABI and formats, and moving the fault and selection types, is left. Pure logic such as `sticky.rs` goes with its owner.

**Platform adapter** `modules/core/adapters/platform`. **R3a and R3b done (the trait, `RealPlatform`, `FakePlatform`, the contract table, the layer rule and the `core/platform` adapter; branches rt/r3a-platform and rt/r3b-adapter).** Differences from the design below: the trait has `profile()` with `DeviceProfile` (generated, from `[capabilities] card` and `[digikit] sample_path_max`) in place of raw device ids, so the device objects gained two data words and R3a gates together with R3b; `track_type` returns `Option<i32>`; the `dkpool_*` and `dkpw_*` calls stay with their users and the trait only holds `present()` and, for the pool, the worker calls; the machines service's calls are in `services/machines/fw.rs`; the interrupt helper `locked`, `blocks_since`, the card gate (`block_write_with`, `check_with`), the MIDI reply (`reply_with`) and the sample route (`root_route`, `path_too_big`) are generic over `P: Platform`, so host tests drive them with `FakePlatform`.

R3 splits into two steps.

**R3a, Platform trait (Rust only, no image change).**

- Platform methods: `device_id`, `device_name`, `image_id`, `blocks() -> Option<u32>` (firmware block counter, about 1500 per second), `irq_mask` and `irq_restore`, `flush(start, len)`, `block_read(sector, bytes, buf)`, `block_write_raw(...)`, `midi_send(port, msg)`, `midi_tx_in_flight() -> Option<u32>`, `os_upgrade_active`, `soft_reset() -> !`, `track_type(track)`, `key_down(key)` (safe key, the UI queue walk of `boot.rs:198`).
- A `Tasking` sub-trait: task spawn, semaphores, sleep, current task, firmware alloc and free (including the `scan.rs:26` allocation).
- Optional providers are sub-traits with `present()`: `SamplePool` (`dkpool_*`) and `StagedWriter` (`dkpw_*`). The two `dkfw::samples` calls in `worker.rs:137,153` and their `std::function` layouts go behind `SamplePool`.
- `RealPlatform` is a zero-size type over today's `platform.rs` and `dkfw` calls. Use `#[inline]` only on small hot paths: lane E measured that inlining larger helpers cost about 4% text. The core is generic over `P: Platform`, like `C: CoreApi`.
- Not in the trait: `st()` (core state), and `rd32`, `wr32` and `cstr_*` (pure helpers in `dkrt-abi`).
- Device id, name and image id come from the generated device data (`device.s`, the patchkit stamp). Firmware addresses resolve at patch time through anchors and the `*_info` tables, never as runtime literals. Per-device numbers come as generated consts, like `dkrt_sdk::device` `DDR_CACHED_BASE`.
- The `machines` service's firmware calls (`dkfw` page, input and param calls in `pageclick`) go behind that service's own adapter interface (`paramapi::Fw` exists), not into the core `Platform`. This matches finding 128: page key and turn hooks stay runtime-owned with slots.
- About 20 `dkfw::` lines exist in 6 runtime files today. They move behind the trait.

`FakePlatform` (`cfg(test)`) models:

- a sector-addressed card with a write log and injected errors;
- a block clock that tests advance, including `None` and wrap;
- IRQ nesting checks;
- captured MIDI output and a settable `tx_in_flight`;
- the `*_info` tables as bytes;
- presence and version of each optional provider;
- a reset flag;
- `os_upgrade_active`.

It takes a `DeviceProfile` from the same generator as `RealPlatform`.

**Contract suite.** One expectation table, checked by host tests against `FakePlatform` and by a device test per device (DT2 1.17, DN2 1.12; Syntakt later) against the real platform. Checks:

- device id and name equal the profile;
- blocks advance at about 1500 per emulated second and wrap cleanly;
- a known sector reads back;
- out-of-range and foreign-card writes are refused;
- MIDI output appears and `tx_in_flight` drains;
- IRQ state round-trips;
- flush, then execution of fresh code, works;
- every anchor resolves or returns `None` deliberately;
- every `*_info` table is well formed with a matching version.

**Layer rules.** Remove the runtime's exemption at `layercheck.py:243-244` (the firmware-call rule) and fix `docs/ARCHITECTURE.md:32`. Forbid device-id comparisons outside L1.

**Proof for R3a:** `patchkit objdiff` (near-identical, since `RealPlatform` is a zero-size type); host tests; the DT2 text-size check; the contract device test once per device.

**R3b, adapter move (image change, needs a gate; done).** The hooks `cf.view.hook.ui_loop`, `cf.rtos.hook.main_os_task` and `cf.midi.hook.rx`, their trampolines, and the exception hook install (`crash.rs:106-129`) move into `modules/core/adapters/platform`. The runtime then sees a neutral interface: tick, boot, `sysex_rx` returning consumed, and the `Platform` calls above.

**Checks made for the build:**

- The profile-driven rule holds (`docs/plans/syntakt.md` on the syntakt branches): device facts come from `devices/*.toml`, code branches on capabilities. The runtime had 12 raw `device() == N` tests (sample path limits, the samples root route, the +Drive tick, the track sample probe); all became capabilities.
- Per-device numbers generated: `[capabilities] card` (`slots` is `samples_in_store`) and `[digikit] sample_path_max`, as `SAMPLES_IN_STORE` and `SAMPLE_PATH_MAX` words of `device.s`, and a `DeviceProfile` constant for each device file (`rust-profiles` gen kind, `abi/src/profiles.rs`).
- `dynreg_info`, `dyndsp_info` and `dynfx_info` are read in the core (`registry.rs`, `dsp/mod.rs`); services get them through `CoreApi::dynreg`, `dsp_info` and `fx_info`. `fxpages_info` is still read by the effect service itself (`fxpage.rs`): it moves with the neutral adapter functions below.

**Size:** R3a about 2 agent days, R3b about 1 (scout estimates; recent ones ran pessimistic).

**Neutral adapter functions**, done as each service is extracted: replace `dynreg_info`, `dyndsp_info`, `dynfx_info`, `fxpages_info` raw tables, so the firmware math (`registry.rs:478-536`, string layout `:192-207`, `dsp.rs` row words, `fxpage.rs:248`) moves into the adapters. One device-split style. Generated manifest rows. One anchor convention. Correct `requires`. `ui`: replace `dk_ui_stock_row` and `dk_app_settings_rows(uint8_t*)` with a neutral settings row callback. Update `devices/*.toml [patch_sets]`.

**Dependency cycles.** The crate split turns these into build errors, so each needs its cut:

| Cycle | Cut |
| --- | --- |
| `dsp` and `loader`/`boot` (`dsp::fail` calls `loader::undo_early` and `boot::note_result`) | `dsp::fail` returns a code; the loader acts on it |
| `registry` and `fxpage`, `fxsend`, `pageclick` | The generic walker dispatches to handlers; each service owns its rows |
| `fxpage` and `fxsend` | Both go into `fx` |
| `dsp` and `sample`, `tracks` | "DSP part loaded" event; DSP records go through handlers |
| `sample`, `tracks` to `fxpage`, `fxsend` through `busy()` | The link queue (section 6.2, R6a): done |
| `fxsend`, `tracks`, `files` to the module table and `dlog::current_source` | A small module-context interface in the runtime |
| `crash` and `watch` | DIAG words in a shared crate |
| `tracks` and `peakwin` | Both go into `samples` |

**R2b and R2c: service moves and the crossing list.** R2b moved whole files and whole functions. R2c split the walkers (`registry::check`, `apply` and `remove`, and the `dsp` check, reserve and step) into handlers, made the interfaces, and resolved every crossing:

- Services: `services/{machines,fx,samples}` with `mod.rs`, `records.rs` (the `Service` impl) and `dsp/` parts. `assign` is not a service part: it only maps machine keys to type numbers from the selection, the store and the log, so it is core (`assign.rs`), and services call `CoreApi::type_resolve`.
- Core to services: only `handlers.rs`: the static list `Services` (record kinds, DSP parts, capacity, attribution, `on_loaded`, `tick(phase)`) and `handlers::exports` (`fx_entry`, `param_get` and `param_set`, the pool events `dkrt_pool_done`, `dkrt_pool_lost` and `dkrt_project_changed`, and the `dk_sample_pool_*` imports). The `pub use` shims of R2b are deleted.
- Services to the core: only `crate::core_api`: `CoreApi` (rows and strings of the registry, `write_param_row`, `make_rep`, `type_resolve`, guard counts, module names, settings, `store_probe`, log and source, jobs, `dsp`, `dsp_info`, `fx_info`, the link API of R6a: `link_set`, `link_confirmed`, `link_submit`, `link_result`, `link_drain`, `link_run`) plus the C glue it re-exports (`client`, `give`, `own_rec`). Pure types, constants and memory helpers come from `dkrt-abi`. A service names no other service: the effect pages, send amounts and track tables do not check each other any more; they use the link API (R6a).
- Moved into a service: `counts` (registry rows for CAPACITY) and `rows_counts` (voice rows) into `machines`; the effect row word address (`fx_word_start`) into `fx`; the memory helpers that `fxsend` took from `fxpage` into `dkrt-abi`.
- Left for R3 and R6: platform calls in `CoreApi` (`device`, `irq_mask`, `irq_restore`); `dkfw` and adapter symbols still named by `pageclick`, `fxpage` and `tracks`; the row arrays of `DspState` and `RegState`, which stay in core-owned objects (`DKRT_DSP`, `DKRT_REG`) because host tools read them.
- Build note: the rodata string pool of the runtime object changes with the order of the code, so `patchkit objdiff --pool` is the proof for a move ([PATCHING.md](../PATCHING.md#compare-two-objects-after-a-move)). The `samples/` rule of `.gitignore` is negated for `services/samples`. Functions of `dkrt-abi` that are not generic are not marked `#[inline]` when they are larger than a few instructions: an `#[inline]` hint made the runtime object 4.6% larger.

**Encapsulation.** Visibility by layer; state behind functions; list the `#[no_mangle]` statics host tools read and keep only those; replace `cfg(test)` seams in `loader`, `arena` and `dsp` with traits.

**Layer rules to add:** runtime has no firmware bindings; adapter-to-runtime calls only through adapter ABI headers; `requires` matches real links and imports; neutral service code has no firmware bindings.

**Build files:** moves update Nx project files, Cargo paths and docs links. Where compiler output should not change, byte-compare the runtime object.

**Shared helpers:** one place each for read and write words, text copy, sector buffer and card write path. Host copies of sha256, the 8-in-7 codec and the native sample format: check, then share.

**Fixed during items 2, 3, 6, 7, 9 and 10:** SDK headers and `sdk/include/dk/gen/*`, except as the SDK steps change them ([sdk.md](sdk.md) S1-S5); the `.dkm` format; the `dkrt_*` C ABI (`abi.toml`, `dkrt.h`), where internal moves are allowed and exports with no caller may go; the SysEx protocol; settings, store and selection record formats; settings keys.

**Done check for those items:** pluck, shot, freqshift, bd, analog, pd, remote and the test fixtures build and load unchanged; their device tests and the runtime and adapter device tests pass on DT2 1.17 and DN2 1.12; SHARC golden hashes and PCM pins are unchanged; `mise run gen:modules -- --check` passes.

Moving files breaks open lanes: do the split when no other lane edits the runtime.

## 11. Spike: boot-time hooks and `.dks` services

Question: can a service with its hooks load from device storage at boot? If yes, nothing about machines, effects or samples needs to be in the image.

### 11.1 Static part (item 4)

1. Static classifier under `tools/`, as described in `module-architecture.md` 11.2: when each hook site first runs, what it writes, its context. No such tool exists. Hooks are `op = "hook"` entries in module manifests: 55 ColdFire and 38 SHARC, proof adapters included. Port files hold anchors only.
2. SHARC hooks (`dsp-voice` 28, `fx` 2): **answered, they cannot move** ([finding 128](../findings/128-r4-hooks-in-services.md), [D], about 80% confidence). The boot routine uploads the whole DSP image before our code can read the card, and live writes into running stock DSP code are unproven. The image keeps each SHARC hook site as a stub that jumps through a pointer slot; section 6.1.
3. Row reservations: firmware table changes written at patch time. Can they happen at boot, before the firmware reads the tables?
4. Writing a jump while other tasks and interrupts run: a 6-byte write is not atomic. Mark sites where this is a risk, such as the audio interrupt.
5. Measure `machines` and `dsp-voice` output sizes against the 64 KiB limit.
6. Trace whether FX really shares firmware param rows with machines (section 3.2).

**Install point: main-OS task entry.** That is where the first card read happens (`boot.rs:239`). The block read needs a mutex and a semaphore (`docs/findings/36-boot-load-spike.md:35`, [D]), so reset `init_calls` cannot read the card. This amends `module-architecture.md` 11.2. Sites that run earlier stay patched.

### 11.2 Whole path (item 8)

Prove it once on ColdFire with one real hook, on the emulator:

1. The workbench binds a small `.dks` against the firmware.
2. It uploads to the device.
3. A selected test module pulls it in at boot, through exports, import resolution, demand loading and dependency order.
4. The installer checks the stock bytes and writes the jump. Compare site bytes with patchkit's output.
5. The hook runs. Safe mode leaves that site stock.
6. A second service claiming the same site is refused.
7. Build one small provider in C and one in Rust. The Rust path is untested.

Then, after emulator confidence: hardware check of cache maintenance on a hook write. The emulator keeps caches coherent and cannot prove this.

If SHARC hooks or row reservations cannot move, small generic stubs for them stay in the image and the ColdFire side still works this way. If the whole path fails, stop and report to Em before going further: services would stay linked in and the small-image goal would not be met.

Evidence, measured once on DT2 1.17 and DN2 1.12 with the same values [D]:

- The OS runs from DDR: the bootstrap copies and depacks MAIN to 0x40000400. Nothing checks a checksum or HMAC of MAIN.
- The MMU is off: nothing in the bootstrap or MAIN writes MMUBAR. At reset MMUBAR is invalid and the MMU registers are not visible.
- ACR0 is written once, 0x4007e020: 0x40000000-0x47ffffff, copyback, write protect off. No ACR1-3 writes in MAIN. CACR is written once, 0xa50ce100, default write protect off.
- No run-time self-check of the code region was found. This was a string and literal search only; a computed-base loop would not show.
- The runtime already writes and runs code in `cf.expansion`, flushing with `cpushl` (`tramp.s:25-45`, `loader.rs:318`).
- The emulator keeps caches coherent and does not model the MMU or ACRs, so only hardware can prove protection and cache handling.

CACR and ACR0 are in `docs/findings/01-container-and-patching.md:360-364`. The MMU and checksum facts are in no finding, and `module-architecture.md` 11.3 still says the MMU state is unknown. A second agent must check against image bytes before [V], then record them in `docs/findings/`.

Today's only run-time hook write is a data word (`crash.rs:109`). No code jump is written at run time. Patch-time anchor hash checks are repeated by the installer. The instruction decoder and tail generation stay in patchkit, so the runtime does not grow.

## 12. Versions and projects

1. Each import and record field in `sdk.toml` carries `since`. The packer derives the minimum version per service. The loader checks the same major and a minor not newer. This replaces the four contract bytes (`loader.rs:148-164`).
2. Runtime semver follows mechanically: a service major bump means a runtime major, an added import or field a minor, fixes a patch.
3. Module semver covers the module's own saved data: settings under `<module>/` and project data. Projects store module id, version and hash. A newer module with the same major loads; a different major warns.
4. A test compares `sdk.toml` and `abi.toml` with the last release tag.
5. Nx release bumps and tags. Declare 1.0 only after this plan's contract work lands.
6. One shared compatibility function, built for the device and wasm, used by the packer, Transfer tab, menu and loader.

Projects (`docs/plans/digikit-project.md`) stay on their own branch, after module states. `dk_data_*` become imports. "Project changed" becomes an event table entry. Where the project feature lives (runtime or service) is open; recommendation: a loadable service, as a module that uses no project data should not pay for it. It needs module states and the MISSING placeholder.

## 13. Items

The order of these items, together with the items of other plans, lives in [sdk-runtime-roadmap.md](sdk-runtime-roadmap.md). Item numbers are ids, not an order. Section 13.1 lists the dependencies the roadmap must keep.

Before each item over a day: state payoff and confidence, and stop when they are weak. Items of this plan run one after the other, not in parallel, so there is less to review at once.

Items 7, 9 and 10 link services into the image as a temporary step, so each boundary is proven before loading is added. Item 12 is the end state.

| # | Item | Sections | Agent time | Payoff | Confidence |
| --- | --- | --- | --- | --- | --- |
| 1 | SDK cleanup with hash task: moved to [sdk.md](sdk.md) S1 | 7 | | | |
| 2 | Runtime deletions and split. Settle the service anatomy (section 5.12) before R7. | 10 | 2-3 days | High: navigation | High |
| 3 | Platform adapter, layer rule | 10 | 1-2 days | High: no firmware calls in the runtime | Medium |
| 4 | Spike, static part | 11.1 | 1 day | High: decides how adapters get built | High for the answer |
| **Stop** | Report to Em; next estimates | | | | |
| 5 | SDK contract: moved to [sdk.md](sdk.md) S2-S5; versions stay in section 12 | 7, 12 | | | |
| 6 | DSP transfer queue: R6a built (the link API, section 6.2), R6b planned (batched state-sync transport, trigger: modulation rate) | 6 | 1-2 days | High: removes the coupling; may fix a bug | Medium-high |
| 7 | Generic records, service registry, `samples` linked in; done (R7, section 4.1). The storage interface and files memory moved to item 13. | 4, 5 | 4-5 days | High: first real proof of the model | Medium |
| 8 | Spike, whole path | 11.2 | 3-4 days | High: decides item 12 and the small-image goal | Medium for ColdFire; unknown for SHARC |
| **Stop** | Report to Em; review the model against what was learned | | | | |
| 9 | `fx` linked in, with neutral adapter functions. Check that each service object is a separate input of the device tests (`tools/nx/device-inputs.mjs`), so a service edit selects only the tests that load it. | 3.2, 10 | 4-5 days | High | Medium |
| 10 | `machines` linked in, with neutral adapter functions; layer check final | 3.2, 10 | 4-5 days | High: done criterion 1 | Medium |
| 11 | Boot plan with one verdict per module; module states; refusal fixtures | 5, 8 | 2-3 days | High: users see why | Medium-high |
| 12 | Services as `.dks`, hooks at boot, workbench binding and install | 5 | 6-8 days | High: small runtime; done criteria 5 and 6 | Medium |
| 13 | Storage interface and files memory (split out of item 7): the `Storage` trait in the Platform composition (stat, read head, read range, read all, paged listing), the store and firmware-fs routes in the adapter chosen by profile data, listing memory from the arena (at most 2), `dkrt_sample_fs` and `dkrt_files_fs` in the adapter's `storage.s` | 9 | 2-3 days | Medium: a clean boundary for the store | Medium |
| Later | Projects, event tables, DSP stack guard, MISSING placeholder | 8, 12 | Own plans | | |

Total to item 12: about 36-47 agent days. None of the `.dks` path is proven until item 8.

Note on payoff: moving code into a `.dks` saves memory only when it is unused, and moves it to the arena when it is used. The main value is the clean boundary, a small runtime, stock firmware sites when unused, and adding services without runtime edits.

### 13.1 Dependencies

**Proposal (2026-11-10, for Em's review).** Goal: fix the module contract first, then refactor the runtime under it, so modules break once and later runtime work does not change them. The resulting order is in [sdk-runtime-roadmap.md](sdk-runtime-roadmap.md). Dependencies checked in this plan:

- Items 1 and 5 moved to [sdk.md](sdk.md) as steps S1-S5. They come before items 2 and 3, so modules break once and later runtime items do not change them.
- 4 only decides where DSP dispatch and `DK_SPAN` live (6.1, [sdk.md](sdk.md) section 9), so it gates S5, not S1-S4.
- S3 changes the loader and registry (builder calls collected in `init`, commit after `init`, DSP binding at commit) before items 2 and 7. Item 7 builds its generic record walker on that commit path.
- **Blocked:** the new-kind acceptance test (an LFO shape with no runtime edits) needs the generic record walker of item 7 (section 4). S3 ports the existing modules and fixtures; the new-kind proof stays at the end of item 7.
- The sink slots (5.11) fit item 2 (the `dlog.rs` and module context split). The dsp-link windows the profiler needs fit item 6.

## 14. Checks

- SDK steps: the checks in [sdk.md](sdk.md) section 12.
- All items: Nx owner targets from `AGENTS.md` during work; `mise run test` once before merge. When the gate selects almost everything, offer a named targeted list with its skips.
- Items 2, 3, 6, 7, 9, 10: bounded device tests of the moved area on both devices, as selected. `.dkm` hashes stay unchanged where the module contract does not change. SHARC golden hashes and PCM pins stay unchanged.
- Rust crate split: paired size comparison of runtime text before and after, because `lto = false` limits cross-crate inlining (`Cargo.toml:17-24`).
- Hardware only after emulator confidence.
- Each PR that changes a test or workflow rule updates `AGENTS.md`. Correcting a document means replacing the wrong text.

Base documents to rewrite when each item lands. These decisions are amended: `docs/plans/module-architecture.md` D2, D4, D7, D8, D13, D14, D22, D23, D24, 6.1, 7.1-7.3, 8.2-8.3, 10.1, 11.2, 11.3, section 12, 13.2; `docs/plans/sdk-api.md` decisions 1 and 6; `docs/plans/capabilities.md` terms; `docs/plans/digikit-project.md` decisions 2 and 7; `docs/ARCHITECTURE.md` roles and rule 3; `docs/LAYOUT.md`.

## 15. Open decisions

1. **Upload as a separate object** (D23, so an image can accept uploads without the runtime). Direction: keep in the runtime, as a separate crate if simple. Revisit if a real need appears.
2. **Two major versions of one service loaded at once.** Recommendation: no, because they would compete for the same firmware rows and hook sites. One module is refused with a reason.

## 16. Risks

- The Rust crate split can raise text size on DT2, where about 0.46 MiB of `cf.expansion` remains after the runtime.
- Extraction order may reveal coupling not seen in the reviews. Each stop point can change the plan.
- A `.dks` failing at boot adds a new failure mode. Fault attribution and safe mode must name it.
- Anything that stays in the image is not verified as harmless with no service loaded.
- Fixtures under `tests/fixtures/modules` are large in number; the scripted conversion must cover them.
- Moves break open lanes and worktrees.
- Service files contain copied firmware instructions. They must be built on the user's machine and never shipped by us or committed.
- A non-atomic hook write can crash the device if the site runs at that moment. The spike must cover this.
- Adapter code moves into the arena. Measure this, especially on DT2.
- SHARC hooks and row reservations may have to stay in the image.

## 17. Not in this plan

- Flaky device tests: DN2 `throughput` floor of 200 pages/s fails on `main` too (about 190-193); DN2 "no reply from the device" or "no new boot within 90 s" while another emulator runs. Em fixes these later.
- Findings to record: DT2 1.17 seek by the path handle's position word ([D], emulator only; `FUN_4015824e` read-at-offset, `path.read` at 0x4015860a reads at handle word 1). DN2 1.12 firmware font glyph widths, if they stay in use: most capitals and digits 4 px; T, x and 1 at 3 px; M 5 px; '.' 1 px; 1 px spacing.
- Gate width for runtime and SDK changes: once needs are derived, make tests depend on the services and kinds they load, so the Nx selection follows what a change can reach.
- Arena size: measure the footprint first (`docs/plans/module-limits.md:45`). Keep DT2 at 512 KiB until then.

## 18. Rationale

Why modules live in memory we own. Modules are copied into the runtime's arena inside `cf.expansion` (1.33 MiB on DT2, about 25 MiB on DN2), memory the firmware does not use. The firmware reaches them only through hooks and rows. Changing the module set needs no reflash, a bad module cannot damage the image, and safe mode skips it. Most limits are our own constants in `devices/*.toml [limits]`. Hard ceilings are the firmware's machine-table sizes and DSP cycles.

Why services are separate from the runtime, also for tests. Today every device test loads the runtime, so a runtime edit selects almost every device test. When a service is its own object, a test's inputs are the runtime and the services it loads: an edit to `fx` reruns only the tests that load `fx`, and the others replay from the cache. A small runtime changes rarely. A service talks to the rest only through `sdk.toml`, so host tests can drive it with fake imports, and its pure checks become unit tests.

Ideas not adopted:

| Idea | Why not |
| --- | --- |
| Link all modules into a custom image on the host and reflash for every change | Loses run-time upload, safe mode, health and fit checks |
| Call firmware routines by hard-coded address in module source | Breaks on every firmware release. We use anchors resolved by hash |
| Free-text resource claims written by authors | Typos and forgotten claims. We derive typed keys and uses from records |
| Object format as JSON with hex sections | Large and slow to parse. `.dkm` is compact binary with a sha256 |
