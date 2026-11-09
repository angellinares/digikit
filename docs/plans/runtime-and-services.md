# Runtime and services plan

Date: 2026-11-06. Base: `main` at 9a849b7b. Targets: DT2 OS 1.17, DN2 OS 1.12.

Status: **draft for Em's review. Nothing is built.** This is the only active plan for this work.

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
- The `type` and `raw` terms go. `category` says what the user sees, and the packer derives what a module contains from its records (section 7.2). Modules with `type = "service"` today become tool modules. The `service` contract byte goes when the contract bytes go.
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

If kind numbers and the `.dkm` format do not change in this step, modules stay byte-identical across it. This should hold; the hash task (section 7.1) proves it.

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

### 5.3 Resources

There is one rule for all. A service offers capacities. Every module and provider has derived uses. The runtime offers arena and DSP bytes. `machines` offers rows, `samples` offers pool slots. The adapter reports the device limit from the profile. Services have limits too: a provider's own arena and DSP use counts like any module's.

The broker sums uses per resource, checks exclusive keys for duplicates (module id, `<module>:<machine>`, effect id, settings prefix) and checks versions. It names no service. The same function runs in the packer, Transfer tab (wasm), menu and loader; it extends `dkrt-fit`. Today the loader enforces limits with separate code that shares constants and a few helpers (`loader.rs:183-262`, `dkrt_sdk::capacity`).

Dynamic demands stay run-time calls that return errors, such as PCM memory or `dk_alloc`. DSP cycles remain unbudgeted.

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

## 6. DSP

Three concepts are mixed today:

| Concept | Today | Target |
| --- | --- | --- |
| Transport: moves bytes using hooks | `dsp-link` adapter, ~900 lines. Names its users (`_dl_vrows`, `_dl_frows`, `_sp_tail`) | `dsp-link` adapter with window registration. It stops naming users |
| DSP code load and memory | `dsp.rs` | Runtime, with one transfer queue |
| What goes in DSP memory: voice rows, FX rows, sample tables | `dsp.rs`, `fxpage.rs`, `fxsend.rs`, `tracks.rs` | Services |

Only one transfer can run at a time (`dsp-link.c:133,153`). There is no queue, so writers check each other's `busy()` (`tracks.rs:197`, `sample.rs:130-136`, `fxpage.rs:585`, `fxsend.rs:417`, `dsp.rs:360-363`). The status is shared with no owner token (`dl_poll`, `dsp-link.c:168-182`).

Target: `submit(owner, addr, bytes, done)` with one FIFO. Delete all `busy()` cross-checks. Requirements:

- The queue owns the bytes of each request. The link reads buffers in place today (`d.word`, `STAGE`).
- In-order delivery per owner. Voice rows write the type last, FX rows write the pc last, and clears run in reverse. The FX row is an 18-step sequence (`FX_STEPS`, `dsp.rs:54`).
- Failure in the middle of a sequence needs rollback (`fx_pub`, `row_pub`).
- Unload needs a synchronous drain.

**[O] Possible bug.** The error and unload path `write_word_blocking` (`dsp.rs:1109`) may take the link while another writer is mid-transfer, so that writer may misread the result. Plausible from reading; not run, and whether the loader drains other writers first was not checked. Check this statically first. If it is real, record it in `docs/findings/`.

`watch.rs`: keep the heartbeat and hang record in the runtime. Move voice and FX guards and attribution to their services. Boot-counter safe mode does not use `watch.rs` (`boot.rs:254-309`); hang records do (`crash.rs:449`). The DIAG words move to a shared crate to break the `crash` and `watch` cycle.

`_dv_exp2` is exported by `dsp-voice` to machine DSP code. DSP-side imports are part of the `machines` contract and need to be declared as such.

## 7. SDK

### 7.1 Cleanup

Rule: **every built `.dkm` stays byte-identical.** Clean only what survives the contract change, so nothing is done twice.

Proof: `patchkit pack` and `dkm-info` print a sha256 covering the whole file except the stored hash. Add one small task that records and compares these hashes. It must first answer:

1. Do two builds of the same source match? No timestamps or build ids were found, but no double build has been run.
2. Does `module:pack-all` include `tests/fixtures/modules`? If not, the task must.
3. How do we check adapters that include SDK headers, such as `adapters/machines/src/cf/native-page.c`?

Target layout, sorted by concept and not by language:

```text
sdk/
  sdk.toml            contract source
  include/dk/         contract for both CPUs, same name per concept
    machine.h  machine.inc
    fx.h       fx.inc
    sample_track.h  sample_track.inc
    trace.h    trace.inc
    page.h  params.h  ui.h  files.h  job.h  ...
    gen/              generated parts, C and asm
  lib/                optional code a module builds in
    dsp/              gain, noise, oscillators, filters
    samples/          SHARC readers, waveform drawing, sample picker
    ui/               widgets, chooser
  templates/          copied by module:new
  tests/
```

- `sdk/vectors/` holds expected results for the fit rule and settings format. It moves next to the runtime, because module authors do not need it. Readers: `patchkit/src/fit.rs:211`, `storedump.rs:444`, `runtime/fit/src/tests.rs:11`, `runtime/sdk/src/settings/tests.rs:234`, `apps/web/tests/fit.test.mjs`, `tests/digikit/test_dksettings.py`, `tests/regression/host/settings-record.test.ts:23`, `tests/patchkit/test_patchkit_dkrt_boot.py`, `tools/nx/cargo.mjs`.
- SHARC include paths come from `tools/patchkit/toolchain/pins.toml:17` (`sharc = ["sdk/asm", "sdk"]`). Check what a change there invalidates.
- `dk_fx.inc` and `dk_machine.inc` both define `DK_BLOCK` and must not be included together.
- Modules keep their shape: `src/module.c` plus `src/dsp/`.
- Update includes directly. No forwarding headers.
- The word "kernels" goes; `sdk/dsp` is a library.
- The `service` template becomes `tool`.

Work items:

- Naming: `pk_`/`PK_` to `dk_` (about 107 uses). Read `chooser`, `sample_picker` and `picker_logic` before deciding names; they may be one flow. Sort out `sample`, `sample_track` and `sample_region`.
- One definition of -40 (`sample.h:21`, `sample_track.h:21`, `chooser.h:31`). Generate `DK_SAMPLE_E_*`, `DK_STRACK_W_*` (`sdk/dsp/sample_track.inc:14-23`), `DK_SAMPLE_PEAK_BINS` and `DK_SAMPLE_WINDOW_MAX` from `sdk.toml`.
- Delete test-only `dk_sample_region*` wrappers (`sample_region.h:241-256`); move the tests to the draw functions.
- `DK_API_HAS` in `sample_picker.c:166` and `chooser.c:414`.
- `sdk/project.json`: add `src`, `dsp` and `asm` to the inputs. This is a cache correctness bug.
- `sdk/README.md`: remove stale text; document `DK_PARAM_LATCHED`, `DK_SAMPLE_TRACKS` and the native pages.
- `modules/machines/shot/tests/test_shot_playback.py`: check whether the leftover can go.
- One SDK helper for number-to-text and text centring, repeated today in SHOT `pages.c`, widgets and fixtures.
- Other modules' host tests compiled from Python move to the Vitest and C pattern SHOT uses (`tests/pages.test.ts`).

Left for the contract change, because it replaces them: merging native page macros, a macro for DSP descriptor blocks, `uses_api`, spreading the `shot_params.h` pattern (a hand-kept header of parameter word numbers shared by C and assembly, which generation replaces). Left until a second user exists: moving `sample_view.c` into the SDK.

### 7.2 Contract

Plain C const records with well-known names. The packer finds them by symbol.

```c
const dk_machine dk_machines[] = {
    { .id = "pluck", .name = "Pluck", .short_name = "PLK" },
};
const dk_param dk_params[] = {
    { .machine = "pluck", .id = "decay", .name = "Decay", .short_name = "DEC",
      .min = 0, .max = 127, .def = 64 },
};
```

Why consts and not registration calls: the menu and Transfer tab can say "does not fit" before restart without running module code; a refusal needs no rollback; the packer can write a report; and modules have no order problems at init. Registration calls stay for resources that are truly dynamic, such as memory through `dk_alloc`. Lifecycle functions say nothing about needs.

Design points:

- `module.toml` holds identity: id, version, name, category, plus `[targets]` (section 3.4). `type`, `status`, `ports`, `conflicts`, `place` and `[[object]]` leave content manifests. `requires` goes once derived (see below). Core modules keep `role`.
- The packer derives what a module contains from its records: machine records make it a machine module, effect records an effect module, neither a tool. It already infers the contract bytes this way (`dkm.rs:409-414`). Check each reader of `type` (`shipped.mjs:12`, patchkit `main.rs:115`) when this is done.
- Sources are found by convention (`src/module.c`, `src/dsp/<id>.s`).
- DSP entry points are found by name (`_<id>_render`, `_<id>_note_on`). The packer refuses when one is missing.
- The packer fills defaults, so no override pragmas are needed. One parameter unit for machines and FX.
- Pages list knobs by param id, not word index.
- `DK_MENU` becomes an exported `dkm_menu`. `DK_PARAM_IS_ERROR` becomes a `static inline` function. `DK_TRACE_INIT` and `DK_TRACE_LOG` stay as compile-time switches, because their arguments must not be evaluated when off.
- Until this lands: no new `DK_*` macros. The packer reads both `.dk.desc` and the new symbols until the last module moves.

Open design issues this work must solve:

| Issue | Fact | Direction |
| --- | --- | --- |
| Array count | No symbol sizes exist: `PSymbol`, `DkoSymbol` (`elf.rs:37`, `dko.rs:41`) | Add `st_size`, or use `-fdata-sections` and the section size. Decide with a small test |
| Strings in records | Record text is inline arrays today. Pointers would add relocations to `.rodata` | Keep inline char arrays |
| Callbacks | `.dk.desc` refuses relocations (`desc.rs:379-385`). Callbacks are name strings resolved by the packer (`register.rs:488-511`) | Function pointers give compile errors. The packer must read desc relocations. This is new work |
| Generated `.inc` | The build is per object with no second phase (`build/mod.rs:343-405`). `patchkit gen` is the only writer of `build/gen/` (`module-architecture.md:607,752`) | Add a build phase; give the file an owner outside `build/gen/`. The hash must include it |
| `requires` | Real in `remote` (`core/input`) and `probe` (`core/dsp-link`) | Derive from imports before dropping it |
| Device restriction | `ports` is the only one today (`main.rs:788-799`) | Use `[targets]`: `unsupported` means the module does not apply there |
| Record size versioning | Kind and size fields carry it today (`desc.rs:391-418`) | Decide with the record format |
| Asm-only facts | `state_bytes`, `max_cycles_*`, `stack_bytes` | These need a C field or stay in asm |
| Latched params | `DK_PARAM_LATCHED` (`machine.h:60`) | A `.latched` field |
| Native pages | `DK_NATIVE_PAGE_V1/_V2/_V2_FLAGS` | A const record |
| Sample tracks | `DK_SAMPLE_TRACKS(0, _shot_rec, _shot_ctl)` names asm symbols | The record needs a C-to-asm reference by name |
| Constants | `DK_GLYPH_*`, `DK_POS_AUTO`, `DK_ROUTE_*`, `DK_PERCENT`, `DK_BIPOLAR`, `DK_MACHINE_STEREO` | Stay as generated constants |

The Api table becomes named imports, grouped by service in `sdk.toml`. `dkm_init` takes no argument. `uses_api` and `api->size` checks go. `fw_alloc`, `fw_free`, `fw_block_read` go: no content module, template or SDK source uses them (core users not fully checked). The runtime can only stop naming samples and params once this is done, because the Api table thunks name them (`lib.rs:521-579`).

Conversion scope: 37 declaration macros in the 6 content modules, 1 in `remote`, 6 in templates, 6 in proof adapters and 78 in 21 test fixtures. Use a scripted conversion. Update the `digikit-module` skill and docs.

Page canvas v2 with aligned text through the firmware's flags (`dk_fw_text.h:13`) and a text-width query retires SHOT's `text_width`, which uses glyph widths measured from the framebuffer. SDK components are named in the manifest, such as `sdk = ["ui", "samples"]`, instead of relative source paths (`shot/module.toml:25`); the identity-only rule gets this one exception.

Source-breaking changes land together, so modules break once. Runtime is 0.x.

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

Target: one neutral storage interface in the runtime, with a `storage` adapter behind it: stat (size, identity hash with its kind, entry kind), read head, read range, read a whole file into an arena, paged listing, root-relative paths with per-device limits. `dk_file_id` already carries a hash kind (sha256 on DN2, Elektron32 on DT2). The device branches in `tracks.rs:232-318`, `files.rs:131-132,281` and `sample_source.rs:28` go behind it. `dkrt_sample_fs` and `dkrt_files_fs` leave `device.s`.

Storage is not the whole device difference for samples. `sample-pool` also handles memory placement, DSP-visible addresses, DN2's DSP receiver and hooks, and DT2's borrowing of the stock pool. The DT2 adapter also reads files through the firmware directly (`dt2/pool.c:112-149`). So `samples` depends on the runtime's `storage` interface and contains the `sample-pool` adapter. `dkrt_pool.h` is close to the right adapter ABI header; `dkrt_project_changed` moves out of it.

Memory cleanup: files state is 33,980 B in `.data`, about 42 KB with scratch, for 8 module slots of 3.6 KB each. It ships as zero bytes in the image. One active listing needs about 4.3 KB (estimated). Allocate on first use. Tracks state (`CORE`, 35,838 B) is similar, mostly zeros in `.data`. Check how much of the runtime's `.data` is mostly-zero state.

## 10. Runtime cleanup

**Deletions and single sources:**

- Sample bank API 1.3, by Em's decision. Only `tests/fixtures/modules/sample-proof` declares a bank. Delete `R/sample.rs` and `DK_SAMPLE_BANK`; the Api entries stay as reserved fields until imports replace the table, so module bytes do not shift. Check first what still uses `sample_source.rs`, `sample.h` and `sdk/dsp/sample_bank.inc`: SHOT includes `sample_bank.inc`, so it may stay. Separate commit, outside the byte check, because the fixture and the runtime change.
- One generated source for record kinds (section 4). `fxpage::MAX_FX` literal goes. Extend the pin test (`dkm.rs:966-975`) to cover all kinds.
- After a caller check across modules, packages, apps, tools and tests: `dkrt_store_list`, `dkrt_settings_get/remove` externs, second poll registry, second log (`State.log`), `selection_set_one` duplication, hot-reload leftovers not on the failure path.
- One capacity table used by `dkrt_capacity` and `dkrt-fit`.
- Fix stale text: "emulator only" in the runtime, health, dynreg and dyndsp summaries; runtime README "Version 0.1.0"; `lib.rs:1-9` crate doc; `settings.rs:9`; `crash.rs:16` (names `dlog.rs` for `watch.rs`); `tracks.rs:5`; patch-set comments in `devices/digitakt-ii.toml:181-182`; `[targets]` "untested" on shipped `dsp-link` and `sample-pool`; `abi.toml` `STATE_UNLOADING` and `CAP_MAX` docs; the runtime `module.toml` summary.
- Proof adapters (`dsp-ddr-proof`, `dsp-lp0-proof`, `dsp-stereo-proof`, `file-catalog-proof`, `storage-fault-proof`) are old experiments among the shipped adapters. They go during this refactor. Before deleting each one, check that shipped tests cover what it proves.
- Check by symbol before removing: whether tools read `State.log`, `DKRT_REG`, `DKRT_DSP`, `DKRT_FXP` or `DKRT_FXS`. `dkrt_load` has a caller in `tools/patchkit/pk_dkrt.py:429`. `dkrt_project_changed` has no C caller found; check assembly and project work first.
- Keep `sample_stream.rs` out of the build until something uses it.

**Runtime split:** `lib.rs` into state, imports, api, tick, health, report, mailbox. `boot.rs` into selection, safe mode, boot. `upload.rs` into transport, store commands, admin commands, module commands. `dlog.rs` and module context. `crash.rs` becomes `fault.rs`, `table.rs` becomes `invariants.rs`. Split `apps/digikit-menu/src/menu.c` by page. Split crate `dkrt-sdk`, whose name wrongly suggests it is the SDK: host-shared ABI and formats go in `dkrt-abi`, internals into the runtime and services. Pure logic such as `sticky.rs` goes with its owner.

**Platform adapter** `modules/core/adapters/platform`. It takes:

- hooks `cf.view.hook.ui_loop`, `cf.rtos.hook.main_os_task`, `cf.midi.hook.rx` and their trampolines;
- exception hook install (`crash.rs:106-129`);
- soft reset, eSDHC busy check, block read and write, MIDI send and TX ring sizes, `project::serialize`, boot record write, `track_type`, UI queue walk for safe key (`boot.rs:198`), `scan.rs:26` allocation;
- the two `dkfw::samples` calls in `worker.rs:137,153` and its `std::function` layouts;
- firmware-backed imports.

The runtime calls a neutral interface: tick, boot, `sysex_rx` returning consumed, block read and write, MIDI send, TX room, soft reset, project serialize, track type, safe key held, clock.

About 20 `dkfw::` lines exist in 6 runtime files today. Remove the runtime's exemption at `layercheck.py:243-244` and fix `docs/ARCHITECTURE.md:32`. Page key and turn hooks move to the `machines` adapter with `pageclick.rs`'s firmware half (`:185, :238, :260`).

**Neutral adapter functions**, done as each service is extracted: replace `dynreg_info`, `dyndsp_info`, `dynfx_info`, `fxpages_info` raw tables, so the firmware math (`registry.rs:478-536`, string layout `:192-207`, `dsp.rs` row words, `fxpage.rs:248`) moves into the adapters. One device-split style. Generated manifest rows. One anchor convention. Correct `requires`. `ui`: replace `dk_ui_stock_row` and `dk_app_settings_rows(uint8_t*)` with a neutral settings row callback. Update `devices/*.toml [patch_sets]`.

**Dependency cycles.** The crate split turns these into build errors, so each needs its cut:

| Cycle | Cut |
| --- | --- |
| `dsp` and `loader`/`boot` (`dsp::fail` calls `loader::undo_early` and `boot::note_result`) | `dsp::fail` returns a code; the loader acts on it |
| `registry` and `fxpage`, `fxsend`, `pageclick` | The generic walker dispatches to handlers; each service owns its rows |
| `fxpage` and `fxsend` | Both go into `fx` |
| `dsp` and `sample`, `tracks` | "DSP part loaded" event; DSP records go through handlers |
| `sample`, `tracks` to `fxpage`, `fxsend` through `busy()` | The transfer queue (section 6) |
| `fxsend`, `tracks`, `files` to the module table and `dlog::current_source` | A small module-context interface in the runtime |
| `crash` and `watch` | DIAG words in a shared crate |
| `tracks` and `peakwin` | Both go into `samples` |

**Encapsulation.** Visibility by layer; state behind functions; list the `#[no_mangle]` statics host tools read and keep only those; replace `cfg(test)` seams in `loader`, `arena` and `dsp` with traits.

**Layer rules to add:** runtime has no firmware bindings; adapter-to-runtime calls only through adapter ABI headers; `requires` matches real links and imports; neutral service code has no firmware bindings.

**Build files:** moves update Nx project files, Cargo paths and docs links. Where compiler output should not change, byte-compare the runtime object.

**Shared helpers:** one place each for read and write words, text copy, sector buffer and card write path. Host copies of sha256, the 8-in-7 codec and the native sample format: check, then share.

**Fixed during items 2, 3, 6, 7, 9 and 10:** SDK headers and `sdk/include/dk/gen/*`, except as item 5 changes them; the `.dkm` format; the `dkrt_*` C ABI (`abi.toml`, `dkrt.h`), where internal moves are allowed and exports with no caller may go; the SysEx protocol; settings, store and selection record formats; settings keys.

**Done check for those items:** pluck, shot, freqshift, bd, analog, pd, remote and the test fixtures build and load unchanged; their device tests and the runtime and adapter device tests pass on DT2 1.17 and DN2 1.12; SHARC golden hashes and PCM pins are unchanged; `mise run gen:modules -- --check` passes.

Moving files breaks open lanes: do the split when no other lane edits the runtime.

## 11. Spike: boot-time hooks and `.dks` services

Question: can a service with its hooks load from device storage at boot? If yes, nothing about machines, effects or samples needs to be in the image.

### 11.1 Static part (item 4)

1. Static classifier under `tools/`, as described in `module-architecture.md` 11.2: when each hook site first runs, what it writes, its context. No such tool exists. Hooks are `op = "hook"` entries in module manifests: 55 ColdFire and 38 SHARC, proof adapters included. Port files hold anchors only.
2. SHARC hooks (`dsp-voice` 26, `fx` 2): can they move? DSP code comes from the boot stream. `dsp-link` writes to DSP memory while running, but only inside its own area today (`README:74`). Writing live hook sites is unproven. This is the biggest unknown.
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

## 13. Order

Before each item over a day: state payoff and confidence, and stop when they are weak. One after the other, not in parallel, so there is less to review at once.

Items 7, 9 and 10 link services into the image as a temporary step, so each boundary is proven before loading is added. Item 12 is the end state.

| # | Item | Sections | Agent time | Payoff | Confidence |
| --- | --- | --- | --- | --- | --- |
| 1 | SDK cleanup with hash task | 7.1 | 2 days | High: navigation, single sources, proof tool | High |
| 2 | Runtime deletions and split | 10 | 2-3 days | High: navigation | High |
| 3 | Platform adapter, layer rule | 10 | 1-2 days | High: no firmware calls in the runtime | Medium |
| 4 | Spike, static part | 11.1 | 1 day | High: decides how adapters get built | High for the answer |
| **Stop** | Report to Em; next estimates | | | | |
| 5 | SDK contract: records, imports, generated DSP facts, manifests, versions | 7.2, 12 | 6-7 days | Medium-high: no hand-kept agreements; runtime no longer names concepts in the Api table | Medium-high |
| 6 | DSP transfer queue | 6 | 1-2 days | High: removes the coupling; may fix a bug | Medium-high |
| 7 | Generic records, service registry, `samples` linked in, storage interface, files memory | 4, 5, 9 | 4-5 days | High: first real proof of the model | Medium |
| 8 | Spike, whole path | 11.2 | 3-4 days | High: decides item 12 and the small-image goal | Medium for ColdFire; unknown for SHARC |
| **Stop** | Report to Em; review the model against what was learned | | | | |
| 9 | `fx` linked in, with neutral adapter functions | 3.2, 10 | 4-5 days | High | Medium |
| 10 | `machines` linked in, with neutral adapter functions; layer check final | 3.2, 10 | 4-5 days | High: done criterion 1 | Medium |
| 11 | Boot plan with one verdict per module; module states; refusal fixtures | 5, 8 | 2-3 days | High: users see why | Medium-high |
| 12 | Services as `.dks`, hooks at boot, workbench binding and install | 5 | 6-8 days | High: small runtime; done criteria 5 and 6 | Medium |
| Later | Projects, event tables, DSP stack guard, MISSING placeholder | 8, 12 | Own plans | | |

Total to item 12: about 36-47 agent days. None of the `.dks` path is proven until item 8.

Note on payoff: moving code into a `.dks` saves memory only when it is unused, and moves it to the arena when it is used. The main value is the clean boundary, a small runtime, stock firmware sites when unused, and adding services without runtime edits.

## 14. Checks

- Item 1: `.dkm` hashes unchanged, plus `mise run test:changed`.
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
- `mise.toml` task name clash `wb`: rename the tool task, for example to `wb:cli`.
- Findings to record: DT2 1.17 seek by the path handle's position word ([D], emulator only; `FUN_4015824e` read-at-offset, `path.read` at 0x4015860a reads at handle word 1). DN2 1.12 firmware font glyph widths, if they stay in use: most capitals and digits 4 px; T, x and 1 at 3 px; M 5 px; '.' 1 px; 1 px spacing.
- Gate width for runtime and SDK changes: once needs are derived, make tests depend on the services and kinds they load, so the Nx selection follows what a change can reach.
- Arena size: measure the footprint first (`docs/plans/module-limits.md:45`). Keep DT2 at 512 KiB until then.

## 18. Rationale

Why modules live in memory we own. Modules are copied into the runtime's arena inside `cf.expansion` (1.33 MiB on DT2, about 25 MiB on DN2), memory the firmware does not use. The firmware reaches them only through hooks and rows. Changing the module set needs no reflash, a bad module cannot damage the image, and safe mode skips it. Most limits are our own constants in `devices/*.toml [limits]`. Hard ceilings are the firmware's machine-table sizes and DSP cycles.

Ideas not adopted:

| Idea | Why not |
| --- | --- |
| Link all modules into a custom image on the host and reflash for every change | Loses run-time upload, safe mode, health and fit checks |
| Call firmware routines by hard-coded address in module source | Breaks on every firmware release. We use anchors resolved by hash |
| Free-text resource claims written by authors | Typos and forgotten claims. We derive typed keys and uses from records |
| Object format as JSON with hex sections | Large and slow to parse. `.dkm` is compact binary with a sha256 |
