# Modules

This file says what a digikit content module is, what is fixed before it runs, what it declares while it runs, and why the design is this way. It states what is built. Plans are marked as plans.

Other files own other topics:

- Steps to write a module: [MODULE-AUTHORING.md](MODULE-AUTHORING.md).
- Layer rules, roles, `layercheck`, new devices: [ARCHITECTURE.md](ARCHITECTURE.md).
- Headers and where a new call goes: [sdk/README.md](../sdk/README.md).
- The binary contract (imports, `since`, records, limits, status codes): `sdk/sdk.toml`. Nothing in this file repeats its numbers except to explain a rule.
- Byte layouts: [.dkm format](design/digikit-dkm.md) and [registry rows](design/digikit-registry.md).

## 1. What a module is

A module is one `.dkm` file. The runtime loads it from the card at boot into memory it owns (the arena in `cf.expansion`, which the firmware does not use). A module has a ColdFire part (C) and, for machines and effects, a SHARC part (assembly).

Why this memory and this format:

- Changing the module set needs no reflash.
- A bad module cannot damage the firmware image. The firmware reaches a module only through hooks and table rows the runtime writes.
- Safe mode skips all modules.
- Limits are our own constants in `devices/*.toml`. The hard ceilings are the firmware's machine-table sizes and DSP cycles.

Kinds:

| Kind | Has |
| --- | --- |
| machine | `[[machine]]` entries, builder calls for its pages and parameters, a SHARC part |
| effect | `[[fx]]` entries, builder calls for its page, routes and mix, a SHARC part |
| tool | behaviour only: `init`, `fini`, `tick`, `menu`. A head-only header table, no SHARC part |

The runtime does not read a kind field. A module is a machine or an effect because it lists `[[machine]]` or `[[fx]]` entries. `module.toml` still has a `type` line, which patchkit parses; the loader ignores it.

Core modules (runtime, adapters, apps, installer) are patched into the image. They are not content modules. See [ARCHITECTURE.md](ARCHITECTURE.md).

A service is planned, not built (see section 4). It will be a module with a role: the same format, entry point and rules, plus exports, a hook table, record-kind handlers and its adapter in one file.

## 2. Files of a module

```text
modules/machines/analog/
  module.toml          identity, [[machine]] list, [uses.<device>] counts
  src/module.c         the entry point and the builder calls
  src/dsp/analog.s     SHARC code
  tests/               device tests
  gallery/             site gallery material
  build/gen/           committed generated files
    module_ids.h       written by `patchkit gen`; `patchkit gen --check` guards it
    gallery.json       written by `module:check`
```

`module.toml` has the identity (id, version, name, category, targets), the static lines of section 6, and the `[[object]]` list of sources. A module never lists SDK sources. The packer links the SDK code a module uses and drops the rest (`--gc-sections`).

## 3. Entry point and module context

A module defines one constant:

```c
const dk_module_def dk_module = {.init = init};
```

`dk_module_def` has four optional functions: `init`, `fini`, `tick`, `menu`. The packer finds the symbol `dk_module` and writes the four offsets into the header (0x80 to 0x8c). The type and the constant have two names because a C typedef and an object share one namespace.

`dk_module_ctx` is an opaque pointer. The parameter is always named `ctx`. It identifies the module to the runtime. Only the lifecycle functions, the `_open` calls and the `dk_module_ctx_*` calls take it.

- `init(ctx)` runs once, after the module is copied and relocated. It opens services and declares content (section 5). Any result other than `DK_OK` fails the load.
- `tick(ctx)` runs once per UI pass for every loaded module. It must be short.
- `menu(ctx, menu)` fills the module's SETTINGS... in DIGIKIT > MODULES. It runs only when the user opens that menu.
- `fini(ctx)` runs on failure paths and at restart. No module is unloaded at run time.

Because nothing is unloaded, every pointer a module gives to the UI must point at static data or code. The lifetime rule in [ARCHITECTURE.md](ARCHITECTURE.md) states this.

Why one entry constant and named imports, not a table of function pointers: a table needs a version per table, a size check per call and a "has this call" macro. Named imports give the same result with no global and one mechanism. The runtime resolves each import by name when it loads the file.

## 4. Services, imports and optional providers

A module reaches the device only through named imports. Each import is an `[[import]]` entry in `sdk/sdk.toml` with:

- `service`: the group it belongs to (`runtime`, `samples`, `machines`, `fx`, `ui`, `input`, `usb_bulk`, `dsp_link`, `sdhc_large`).
- `since`: the runtime version that added it (section 8).
- `context`: the task it may run in (`init`, `ui`, `io-step`, `any`, `isr`).
- `sig`: the C signature. `patchkit gen` writes the headers from it.

Stateless calls are plain imports. A call that needs state, or that can be absent, goes through a handle that `init` opens: `dk_<service>_open(ctx, &handle)`.

SDK headers have layers: `core`, then `platform`, then the components (`page`, `machine`, `fx`, `chooser`, `sample`). Only `platform/` declares imports.

### Optional providers, built

Some services are provided by a patched-in module, not by the runtime: `core/ui`, `core/input`, `core/usb-bulk`, `core/dsp-link`, `core/sdhc-large`. They may be missing from an image. The imports of such a service have `requires` and `absent` in `sdk.toml`.

- When the provider is in the image, the import is its function.
- When it is not, the import binds to a stub. The stub returns the `absent` value (`0`, `-1`), the status `DK_E_MISSING`, or nothing for a void call.
- `dk_<service>_open` returns `DK_E_MISSING`. The module decides what to do. A module that can run without the service records nothing.
- If `init` returns `DK_E_MISSING`, the load fails with `E_IMPORT`. The runtime keeps the provider of the last stub that answered `DK_E_MISSING`, and DIGIKIT shows it as the module's need.

Today the `runtime`, `samples`, `machines` and `fx` imports resolve to code in the runtime image. The runtime's import table holds the imports without `requires`.

### Registry: built (R7) and planned (R9 to R12)

R7 is built: one generated table of record kinds, resources and in-crate services; `dk_record_add` for generic kinds; the header table with the R12 entries; a boot planner that returns one verdict for each module of the selection before anything loads; declare-check-commit for services ([runtime-and-services.md](plans/runtime-and-services.md) 4.1). What is planned: R9 to R12 move code out of the runtime into services (`.dks` files). The layers, the three interfaces and the rule that services do not call each other are in [ARCHITECTURE.md](ARCHITECTURE.md). The design:

- A service declares its name, record kinds, resources, exports and hook table. The runtime fills a registry during boot planning, before any module code runs. There is no run-time registration call.
- A service loads only when a selected module needs it. Load order follows dependencies.
- The packer separates hard needs from optional needs. The registry refuses a missing hard need before any module code runs. Stubs remain only for optional needs.
- One resource function sums the derived uses of all selected modules and checks them against the device limits. It runs in the packer, the Transfer tab, the menu and the loader.

Reason: the menu and the Transfer tab can say "does not fit" without running code, a refusal needs no undo, and the packer can derive needs. Module source does not change when a service moves, because the import names stay.

Until the registry exists, the stub and the `init` check above are the only mechanism. A refusal for a missing optional provider happens during `init`, so the undo path of section 7 applies.

## 5. Builder calls and the commit

A module declares its content by calls in `init`. A child is added by its parent:

```text
dk_module_ctx_add_machine / dk_module_ctx_add_fx
  dk_machine_add_page / dk_fx_add_page
    dk_page_add_param / dk_page_add_fx_param
```

Setters refine them: `dk_param_range`, `dk_param_glyph`, `dk_param_latched`, `dk_param_on_click`, `dk_page_canvas`, `dk_page_bind`, `dk_page_bind_local`, `dk_fx_routes`, `dk_fx_mix`. The signatures are in `sdk/sdk.toml`.

The Analog machine (`modules/machines/analog/src/module.c`) is the shortest example: one machine, one page, four parameters.

```c
dk_machine *m = dk_module_ctx_add_machine(ctx, DK_MACHINE_ID_ANALOG, "Analog", "ANA");
dk_page *p = dk_machine_add_page(m, "Analog", "ANALOG");
dk_page_add_param(p, "Detune", "DET", 40);
```

The SHOT machine (`modules/machines/shot`) uses most of the surface: a knob page, a canvas page over the stock overlay, a wide canvas page, a click handler and a sample pool.

### The calls collect; the commit decides

During `init` the calls only record into one `Collect` block in the arena. They are bounded by `max_records` (32) and by the size `collect_bytes` (6320, checked at compile time against the ColdFire layout).

A call never fails on its own. On overflow, a bad argument, a call outside `init` or a handle that is not ours, it returns a placeholder handle. The first error is kept. The setters ignore a placeholder. The error is reported when `init` returns.

When `init` returns `DK_OK`, the runtime commits in one step:

1. Check the collection: device limits, the `[uses]` counts, the keys table, a duplicate effect id among loaded modules, free rows.
2. Lower the collection to registry records and check the records with the same rules as before builders existed.
3. Fill each voice row's parameter ranges and latch mask, and each effect row's routes and defaults.
4. Apply the records to the firmware rows.

If any step fails, nothing is written, the DSP reservation is undone and the module is refused with a reason (section 7). The record block stays in the arena while the module is loaded. The `MODULE_COMMIT` event carries its address and size. That size is `record_bytes` (section 6).

Why calls and a commit, and not static records:

- Calls read well and compose. A loop or a helper can add a page.
- Collecting first makes the update one transaction. A half-declared module never reaches the firmware tables.
- The registry has one code path for records, and the commit feeds it.

Rejected: static `const` records or macros (no runtime change needed, but hard to compose); a builder that the packer runs in a ColdFire CPU core (it adds the CPU core to the packer and the browser build); builder calls plus a hand-written copy of the content as metadata (the same content in two places).

## 6. What is static, and why

Static means: needed before any module code runs. Everything else is a builder call.

| What | Where | Why it is static |
| --- | --- | --- |
| Module id | `module.toml` | identity |
| One `[[machine]] id` or `[[fx]] id` per machine or effect | `module.toml` | type numbers for all selected modules are assigned before any module loads |
| Optional `dsp` on that entry | `module.toml` | the DSP symbol prefix, when DSP code exists under another name |
| `[uses.<device>]` counts and `record_bytes` | `module.toml`, written by `module:check` | the fit check reads sizes without module code |
| DSP section | built from `src/dsp/` | voice rows, effect slots and DSP memory are reserved before `init` |

### Ids are indexes

`patchkit gen` writes `build/gen/module_ids.h`. Each constant is the 0-based position of the id in the `[[machine]]` (or `[[fx]]`) list, for example `#define DK_MACHINE_ID_ANALOG 0u`. This is also the row of its key in the keys table. A typo is a compile error. An index outside the table is refused at load with `E_DECL_ID`.

### Keys are computed by the packer

The keys are not in the header file. The packer computes them from the packed name and the id, in `module.toml` order, and writes them into the keys table of the `.dkm`:

- A machine key is `machine_key(name, id)`: a 24-bit FNV-1a hash of `<name>:<id>`, shifted left by 8.
- An effect id is `effect_id(name, id)`: a 16-bit fold of the 32-bit hash of `<name>:<id>`, never 0.

`name` is the `--name` value of `pack` when given, else the module id. So a normal pack gives the same keys every time, and saved projects stay valid. An alias made with `pack --name` gets keys of its own, so two copies of one module do not share an effect id. Renaming a module id changes the keys and loses the projects that use it.

Why the keys must be known before load: the runtime plans the type numbers of all selected machines at boot, before the first module loads (`assign::plan`). A card-side map of up to 8 entries keeps a number for a machine across boots; a clash gets a fresh number. Effect ids are 16 bits, so a clash is possible (about 1 in 65535 per pair). The loader refuses it with `E_FX_ID_DUP`.

### DSP names

The packer finds the SHARC entry points by name, with the toolchain's leading underscore:

- machine: `_<x>_render`, `_<x>_note_on`, and an optional `_<x>_desc`
- effect: `_<x>_process` and `_<x>_desc`

`<x>` is the id with `-` written as `_`. `dsp = "va"` replaces only this prefix. Analog uses it because its DSP code already existed as `va_*`. The keys and `module_ids.h` still use the id.

### `[uses]` is written, not authored

`module:check` runs the module in the emulator and writes `[uses.<device>]` from what `init` declared: `machine_rows`, `param_rows`, `page_rows` and `record_bytes`. After its conformance run it adds the DSP cost: `dsp_cost_voice` (a machine: the worst steady block of one voice) or `dsp_cost_fx` (an effect: its worst process call), in instructions per block. Nobody declares a cost by hand; the DSP descriptor has no cycle words. It is like a lock file. Nobody edits it. The tables are per device because the rows differ (the Digitone II has page rows, the Digitakt II has none). Analog needs 444 record bytes on DT2 and 540 on DN2.

The packer copies the counts into the header table. The device menu check (`dkrt_scan`, `dkrt_fit`) and `dkneeds::needs_of` read only counts and sizes. So "does it fit" needs no module code. `record_bytes` goes into the `ARENA` need, rounded to 16 plus the block header.

### Header table

The header pair at 0x90 and 0x94 is `table_off` and `table_size`. The table is versioned and tagged: magic `DKTB`, a version, then entries in tag order (`u16 tag, u16 count, u32 payload bytes, payload`, all big-endian):

| Entry | What it holds |
| --- | --- |
| NEEDS | the services the linked imports name, the newest `since` of each, hard or optional (the packer derives them) |
| USES | `(resource id, count)`: the `[uses]` rows and `record_bytes` (resource ids are in `sdk.toml` `[records.resources]`; `DSP_COST_VOICE` and `DSP_COST_FX` are the measured DSP cost; the broker takes the largest voice cost times the voices plus the sum of the effect costs, [sdk.md](plans/sdk.md) section 9) |
| KEYS | `(kind, n, keys)`: the machine keys, the effect ids, and the keys of `[[record]]` entries (the shape id of an `lfo_shape`) |
| KINDS, SLOTS, HOOKS, EXPORTS, RESOURCES | what a service module provides (R12; the runtime compares the `dk_service_declare_*` calls of `init` with them) |

A module that needs and keeps nothing has the 8-byte head. The packer always writes a table and at least 0.8.0 as the needed version, so a module of runtime 0.7 (no table, or the old row counts) is refused with `E_CONTRACT`. The header is 160 bytes. `packages/dkneeds/src/keys.rs` owns the layout.

A module that emits records of a generic kind (a kind a service owns that has no builder, such as `lfo_shape`) calls `dk_record_add(ctx, kind, bytes, len)` in `init` and lists each record as `[[record]] kind = "lfo_shape", key = 7` in `module.toml`. The boot planner reads the keys from the table: a kind no service owns, or a key another module of the selection has, is refused before the module loads (`E_PLAN_KIND`, `E_PLAN_KEY`).

## 7. The load path and what a refusal guarantees

```text
 card (store)
    |  find, load to the staging buffer              E_NOT_FOUND E_BAD_STORE E_READ E_SHA E_TOO_BIG
    v
 staging buffer (at most 64 KiB; the only place the file is read from)
    |  checks; nothing is written outside the buffer
    |   - magic, header size, file size, sha256       E_FORMAT, E_SHA
    |   - runtime version (header 0x30)               E_CONTRACT
    |   - name, image, relocation and import bounds   E_FORMAT
    |   - every import resolves                       E_IMPORT (the provider's name is kept as the need)
    |   - relocations; header table (0x90/0x94)       E_CONTRACT (the 0.7 format), E_FORMAT
    |   - DSP section check                           E_DSP_*
    |   - header keys match the DSP rows              E_DECL_ID
    |   - free module record, name not loaded         E_TABLE_FULL, E_LOADED
    v
 dsp::reserve   DSP range, relocated DSP image, voice and effect rows     E_DSP_NOSPACE, E_DSP_ROWS ...
    v
 arena: allocate, copy, relocate, flush caches                            E_NOMEM
    v
 init(ctx)      builder calls append to the Collect block; no firmware row is written
    |  non-zero result: E_INIT, or E_IMPORT when an absent provider answered DK_E_MISSING
    v
 commit         check ([uses], keys, duplicate effect id, free rows)  E_DECL_ID, E_DECL_USES, E_DECL_FULL, E_FX_ID_DUP, E_PARAM_FULL ...
                lower to records, check records, voice ranges and latch mask
                apply: firmware rows are written here; the record block stays in the arena
    |  any failure: undo (below)
    v
 dsp::launch    a module with a SHARC part returns LOADING; DSP code and voice rows follow
    v
 loaded         tick(ctx) every UI pass; menu(ctx, menu) when the user opens SETTINGS...
 fini(ctx)      failure paths and restart only
```

All checks run on the staged file before any write. The module set is fixed after the boot hook; a later load returns `E_BOOT_ONLY`. The firmware builds its machine view after the boot hook, so rows written at commit are in place before the firmware reads them.

Why the DSP check and reserve stay before `init`: they need only the static DSP section and the keys table. They keep pointers into the staged file only until `reserve`. The cheap refusals come first.

### The guarantee

A refused module writes no byte into the firmware tables. The module's rows are written only at commit, and a failure at or after `init` undoes everything that was taken: the boot animation, the module's files, host commands, registry rows (`registry::remove`), the DSP reservation (`dsp::release`) and the arena block.

The caveat: if something cannot be given back (the DSP part is still in use, or files are not retired), the load returns `E_BUSY` and not the original reason. The module memory then stays allocated.

### States, planned

Today the boot loads modules in selection order and a load either returns OK, LOADING or an error. The plan (R7) adds one state model: SELECTED, PLANNED or REFUSED with a reason, INIT, DSP LOADING, ACTIVE, FAILED. Services load in dependency order, and the MODULES page shows the reason of a refusal.

## 8. Versions

There is one version: the runtime release (`[runtime] version` in `sdk/sdk.toml`, semantic, now 0.8.0).

- While it is 0.x, a minor may break (an import changed or removed, the module file format, the store format, the SysEx protocol). A patch only adds. Stable is 1.0.0.
- There are no separate numbers for the API, the machine contract, the effect contract or a service.
- Each import has `since`, the runtime version that added it.
- The packer writes the newest `since` among the imports a module links into header word 0x30, and at least the version that introduced the header table (`[runtime] table`, 0.8.0). Bit 31 of that word is the trace flag.
- The loader accepts a module when its major and minor equal the runtime's and its patch is at most the runtime's. Otherwise the load fails with `E_CONTRACT`. The fix is to rebuild the module. A file with the old keys table (runtime 0.7) has no header table and fails with `E_CONTRACT` too.
- A `.dkmod` (a source package) builds against the SDK of the machine that receives it, so the rule holds after the build.

Why one version: the rule is simple, and the bump is mechanical.

The module's own semantic version (header word 0x34) covers its saved data. Projects will store module id, version and hash. This is planned (section 12 "Versions and projects" of [runtime and services](plans/runtime-and-services.md)) and not built.

## 9. Layers and boundaries

[ARCHITECTURE.md](ARCHITECTURE.md) owns the layer stack and the rules. This is the view from a module:

```text
 content module   machine, effect, tool (.dkm)      sees only <dk/...> headers
 ------------------------------------------------   boundary 1: sdk/sdk.toml
                                                    (imports, records, status codes, limits, `since`)
 SDK              core <- platform <- components    no device code; platform/ declares every import
 ------------------------------------------------   boundary 2: the same names, linked at load
                                                    (a stub when an optional provider is absent)
 runtime + provider modules                         core/ui, input, usb-bulk, dsp-link, sdhc-large
 ------------------------------------------------   boundary 3: exported functions (dkrt.h), optional symbols
 adapters         one per firmware plug-in point    hooks and wrappers
 ------------------------------------------------   boundary 4: generated bindings (dk_fw_*), anchors by hash,
                                                    device profiles
 firmware         stock Elektron code
```

Services (`.dks`, planned) sit between modules and adapters. Module imports do not change when they arrive.

Who owns what: the SDK has no device code. Adapters and device profiles carry the device and firmware facts. Modules are device-neutral. The builder calls take device facts (knob positions, stock rows, level) from the profile that the runtime reads at commit, so a module does not name a device.

## 10. Design choices

| Choice | Reason | Rejected |
| --- | --- | --- |
| Builder calls in `init`, committed as one transaction | readable, composable, no half-declared module in the tables | static records; a packer-side builder; duplicate metadata |
| Ids are indexes; keys computed by the packer | no hash in source; aliases get their own keys | hand-written key constants |
| `[uses]` written by `module:check` | cannot drift from the code; fit needs no module code | author-declared needs |
| Needs derived from the file, never declared | a hand-built `.dkm` has no field to lie in | free-text resource claims |
| One runtime version with `since` | simple rule, mechanical bump | four contract numbers |
| Named imports, no function table | one mechanism; optional providers by stubs | a versioned table with size checks |
| Modules in our arena, loaded at boot, no hot reload | no reflash, safe mode, no image damage | hot reload (archived); linking all modules into a custom image |
| Anchors resolved by hash | survive firmware releases | hard-coded firmware addresses in module source |
| Compact binary `.dkm` with sha256 | small, fast to check | JSON object files |
| Services separate from the runtime (planned) | small runtime; a test depends only on the services it loads | one large runtime |

## 11. Where to go next

- Write a module: [MODULE-AUTHORING.md](MODULE-AUTHORING.md).
- Headers, tracing, per-kind samples: [sdk/README.md](../sdk/README.md).
- Layers and devices: [ARCHITECTURE.md](ARCHITECTURE.md).
- Plans: [SDK plan](plans/sdk.md), [runtime and services](plans/runtime-and-services.md), [roadmap](plans/sdk-runtime-roadmap.md).
