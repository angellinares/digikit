# SDK design

Date: 2026-11-12. Base: `main` at 25378434. Targets: DT2 OS 1.17, DN2 OS 1.12.

Status: **design agreed with Em on 2026-11-12, S2 detail (sections 5, 5.1, S2a, S2b) agreed the same day. S1 (restructure) shipped
(PR 416); S2a (C hygiene, PR 424) and S2b (entry point and imports, the old contract deleted, PRs 426 and 433) shipped; S3 (builders and the runtime commit path, one runtime version, PR 443) shipped; the S4 templates are on the builder API and the S4 docs are in progress (`docs/MODULES.md`); S5 not built.**

This plan replaces sections 7.1 (Cleanup) and 7.2 (Contract) of [runtime-and-services.md](runtime-and-services.md), that is the SDK
parts of its items 1 and 5. The order of its steps is in [sdk-runtime-roadmap.md](sdk-runtime-roadmap.md). It links to
[profiling.md](profiling.md) for the DSP rule check and cycle cost.

## 1. Goal

A stable SDK with which users build their own modules (machines, FX, LFOs, filters, tools) from reusable parts of the stock DT2 and DN2
firmware, as device-agnostic as possible.

- Composable: a machine has ordered pages, a page has params. An FX is routable to send or insert and has pages.
- Clean C functions, few macros.
- A clear boundary between runtime and SDK, and between SDK and modules.
- Easy to navigate for users and LLMs: the directory tree says where a thing belongs and where a new thing goes.
- Extensible for new concepts (a mixer, audio buses) and for services a module asks for at run time.
- Raw modules stay possible and migrate to the SDK step by step.
- Tested on the host where possible. A module change never reruns SDK tests.
- Low churn: move files once, into the final structure.

Versions stay experimental. There is one version, the runtime release (semantic, 0.x until 1.0.0). S2b bumped it to 0.6.0 (the Api table is gone). S3 moves it to 0.7.0 and deletes the separate contract numbers (api, machine, fx, service): the `.dkm` header has only the runtime version the module needs.

## 2. Today

Facts from the 2026-11-12 review of `main`:

- Declarations are macros (`DK_MACHINE`, `DK_PARAM`, `DK_PAGE`, `DK_FX`, `DK_FX_PARAM`, `DK_NATIVE_PAGE_V1/_V2/_V2_FLAGS`,
  `DK_SAMPLE_TRACKS`, `DK_PARAM_LATCHED`, `DK_PARAM_ACTION`, `DK_PARAM_GLYPH`). Each expands to a const record in `.dk.desc`, in a
  separate ColdFire object that the device never sees. `patchkit` lowers the records into the `.dkm` register section and DSP section.
  Callbacks are name strings, because `.dk.desc` refuses relocations.
- Five of six machines and the one FX are only declarations (11 to 14 lines, no C functions). `shot` is the only machine with C code
  (pages, actions, sample view). Before S2b the tools were plain C over `dkm_init(const dk_api *api)`, and two runtime mechanisms existed: the Api table
  (`api->size`, `DK_API_HAS`, `uses_api`) and named imports. Only the named imports remain.
- Duplication: `put32`/`get32` in three tools; number-to-text and text centring in `shot/src/pages.c`; error constants copied by hand
  into each header (-40 three times); modules compile SDK sources by relative path (`../../../sdk/src/picker_logic.c`).
- Naming: `pk_*` beside `dk_*`; `chooser`, `sample_picker`, `picker_logic`; `sample`, `sample_track`, `sample_region`; `_v1` and
  `_V2_FLAGS` in public names.
- Build: m68k gcc with `-Wall -Wextra -Werror`, no `-std=`. Host tests use `cc -std=c11`. No C formatter or linter.
- Nx: `sdk` is one project. An SDK header is a build input of every module. Device tests key on built bytes, so an SDK change reruns
  only the device tests of modules whose `.dkm` changes.

## 3. Two contracts

- **Binary contract**: what the runtime sees. The `.dkm` header and sections, the imports grouped by service, status codes, record
  kinds. Defined in `sdk/sdk.toml`, generated for both sides. It must stay stable and changes rarely.
- **Source API**: the components (page, machine, fx, sample, later lfo and filter). C code compiled into each module. A change never
  breaks a module that is already built, so it can change faster.

Only the binary contract has to be stable. This is what keeps the SDK stable while the components grow.

## 4. Layers and tree

```text
sdk/
  sdk.toml            binary contract: records, imports, status codes, kinds, sample pool layout
  README.md           map of the layers and where to add new things
  include/dk/
    core/             module.h, log.h, mem.h, time.h, trace.h, status.h (generated)
    platform/         what the device provides, one header per service group:
                      ui.h, param.h, settings.h, files.h, host.h, samples.h, io.h, job.h, hash32.h,
                      usb_bulk.h, input.h, dsp_link.h, sdhc_large.h, debug.h, intro.h, data.h
    page/             page.h, widgets.h
    machine/          machine.h
    fx/               fx.h
    chooser/          chooser.h, logic.h (the slot list chooser and its pure functions)
    sample/           pool.h, bank.h, view.h, picker.h
    gen/              generated only (including sample.h); included by the headers above, not by modules
  src/                compiled sources in the folders of include/: chooser/{chooser,logic}.c, sample/picker.c
  dsp/                SHARC code by concept: osc/, filter/, dynamics/, delay/, util/, sample/;
                      contract/ is on the SHARC include path (call.inc, dk_trace.inc and the
                      generated dk_machine.inc, dk_fx.inc, dk_sample_pool.inc)
  templates/          machine/, fx/, tool/
  tests/              host C tests in the folders of include/ (page/, sample/)
modules/core/runtime/vectors/   fit and settings expected results (moved from sdk/vectors)
```

Shipped in S2b: `core/` has `module.h`, `log.h`, `mem.h` and `time.h` (`module.h` replaced `api.h`), `platform/` has `host.h`, `samples.h`, `input.h`, `dsp_link.h`, `sdhc_large.h` and `debug.h`, and `file.h` is merged into `files.h`.
Shipped in S2a: `core/` has `bytes.h`, `text.h` and `path.h`.
The canvas flags and callback types live in `page/page.h`; there is no `page/canvas.h`. Planned for later steps (not shipped): `platform/draw.h` when a text-width query exists; `dsp/env/` when a module
needs it.

Include direction: `core` <- `platform` <- components (`page`, `machine`, `fx`, `sample`, later `lfo`, `filter`, `mixer`).
Components may use other components, without cycles.

- `core/` is the module contract: entry point, lifecycle, status, log, small helpers. No device access.
- `platform/` is the device layer: thin, device-agnostic access to what the firmware and runtime provide. It is the only layer that
  declares imports. Components never declare imports; they call `platform/`.
- There is no vendor directory. Almost everything would end up in it; the layer line does that job.
- A new concept is a new component folder, plus `dsp/<concept>/` for SHARC code. If it needs the device, its imports go in
  `platform/<name>.h`, owned by a service. The README states this rule.
- `sdk/vectors/` (fit and settings expected results) moved to `modules/core/runtime/vectors/` in S1: module authors do not need it, and every reader uses the new path.

## 5. Module entry and lifecycle

Every module defines one well-known constant, `dk_module`: its single entry point.

```c
const dk_module_def dk_module = {
    .init = shot_init,   /* int32_t (dk_module_ctx *ctx): declares content, opens services */
    .fini = shot_fini,   /* void (dk_module_ctx *ctx) */
    .tick = shot_tick,   /* void (dk_module_ctx *ctx): every module's tick, once per UI pass */
    .menu = shot_menu,   /* void (dk_module_ctx *ctx, dk_ui_menu *menu): SETTINGS... in DIGIKIT > MODULES */
};
```

- The type is `dk_module_def` and the constant is `dk_module`: C keeps a typedef and an object in one namespace, so the two names differ. The
  packer looks up the symbol `dk_module`. Modules write `const dk_module_def dk_module = {...}`.
- The packer finds `dk_module` by symbol and writes `init`, `fini`, `tick` and `menu` into the `.dkm` header. The runtime calls them with a `dk_module_ctx`.
- `dk_module_ctx` is opaque. The runtime passes it to every lifecycle function, always as the parameter `ctx`. It identifies the module (today implicit in
  a global) and answers `dk_module_ctx_device()` and `dk_module_ctx_runtime_version()` (the runtime version). Callbacks (host command handlers, I/O and job steps, intro
  frames) keep their signatures: none of them needed the Api table either.
- `dk_module` grows by appended fields: event handlers such as "project changed" or "settings changed" (runtime-and-services section 8,
  ordered event tables).
- Settings: values are read and written with `dk_settings_get/put`.
- Instrumentation: the SDK and runtime own the dispatch into `init`, `tick` and the kind dispatchers. Debug and profiling hooks (`DK_SPAN`,
  log) attach there. Module code does not change.

### 5.1 Services

A module reaches the device through named imports, grouped by service in `sdk.toml`. There is no Api table.

- Stateless functions are plain imports: `dk_alloc`, `dk_log`, `dk_hash32_*`, `dk_io_post`.
- A service with per-module state, or one that may be absent, has a handle opened in `init`: `dk_settings_open`, `dk_files_open`,
  `dk_sample_pool_open`, `dk_ui_open`, `dk_input_open`, `dk_usb_bulk_open`, `dk_dsp_link_open`, `dk_sdhc_large_open`.
- When an optional provider is not loaded, its imports bind to stubs: `_open` returns `DK_E_MISSING`, the other functions return the same
  code or do nothing. The module decides whether it can run without the service. When `init` returns DK_E_MISSING, the load fails with E_IMPORT and the loader records the provider of the last `_open` stub that answered DK_E_MISSING during `init` as the module's need (DIGIKIT shows it in `DkrtBootInfo.needs`, as for an unresolved import); a module that tolerates the missing service and returns DK_OK records nothing.
- Records of a generic kind: `dk_record_add(ctx, kind, bytes, len)` (runtime 0.8.0, `<dk/gen/imports.h>`, kinds in `<dk/gen/records.h>`) emits a record of a kind that a service owns and `[records.kinds]` marks `generic`; the module lists its key in `module.toml` (`[[record]] kind = "lfo_shape", key = 7`). The typed kinds (machine, page, parameter, effect) stay builder calls. A service module declares its kinds, slots, hooks, exports and resources with `dk_service_declare_kind`, `_slot`, `_hook`, `_export` and `_resource`; the commit compares them with the header table and refuses a difference with `DK_E_DECL_KIND`.
- Sink channels: `dk_sink_set(channel, sink)` (runtime 0.7.1, `<dk/core/sink.h>`) sets the sink of the `log` or `span` channel for the calling module; one owner per channel
  (`DK_E_BUSY` for a second), cleared after the owner's `fini` (runtime-and-services 5.11). A new import is a PATCH: the runtime went from 0.7.0 to 0.7.1 and the import has `since = "0.7.1"`.
- Each import has `service`, `since`, `context` and `sig` in `sdk.toml`:

```toml
[[import]]
name     = "dk_usb_bulk_open"
service  = "usb_bulk"
requires = "core/usb-bulk"
absent   = "missing"          # stub returns DK_E_MISSING
sig      = "i32 (dk_module_ctx *ctx, dk_usb_bulk **out)"
context  = "init"             # init, ui, io-step, any, isr
since    = "0.7.0"          # the runtime version that added the import
```

- The packer writes the runtime version a module needs, the newest `since` among the imports it links, and at least the version that introduced the header table (`[runtime] table`, 0.8.0), into the header word at 0x30 (bit 31 is the trace flag), and derives the services it
  needs from their `service` tags. `uses_api`, `api->size` and `DK_API_HAS` are gone. The runtime loads the module only when that version has its own major and minor and at most its own patch (0.x semver: a minor may break, a patch only adds); otherwise the load fails with `E_CONTRACT`. The bytes 0x38 to 0x3f of the header are reserved zero.
- `service` uses the names of runtime-and-services: `runtime`, `samples`, `machines`, `fx`, `ui`, `input`, `usb_bulk`, `dsp_link`, `sdhc_large`. Today
  every `runtime`, `samples` and `machines` import resolves to code in the runtime image. When runtime-and-services moves `samples`,
  `machines` and `fx` into `.dks` services, the same names resolve to the service exports and the loader loads a service only when a
  selected module imports it. Modules do not change.

| Service | Header | Today in | Target (runtime-and-services) |
| --- | --- | --- | --- |
| memory (the module arena and the firmware's allocator and card read), log, time | `core/mem.h`, `core/log.h`, `core/time.h` | runtime | runtime |
| host commands (SysEx) | `platform/host.h` | runtime | runtime |
| settings, files (read and write), I/O and jobs, hash, intro | `platform/settings.h`, `files.h`, `io.h`, `hash32.h`, `intro.h` | runtime | runtime |
| debug reads and log sink | `platform/debug.h` | runtime | runtime |
| sample pool (and the sample bank, removed with the bank in S3) | `platform/samples.h` (drawing and picker stay in `sample/`) | runtime | `samples` service |
| params | `platform/param.h` | runtime | `machines` service |
| UI menus, dialogs, ticks | `platform/ui.h` | `core/ui` module | runtime-and-services decides |
| input, USB bulk, DSP link, large card commands | `platform/input.h`, `usb_bulk.h`, `dsp_link.h`, `sdhc_large.h` | provider modules | provider modules |

Renames that make the names consistent (the naming rules are in section 10): the file calls are one service, `dk_files_*` (`dk_files_open` gives one handle for
listing and writing; `dk_file_create/write/close/abort/delete` become `dk_files_create/write/close/abort/delete` and take that handle first); menu functions become `dk_ui_menu_*` (`dk_ui_menu_new/open/close/select/refresh`), items
`dk_ui_menu_add_*` (`text`, `action`, `toggle`, `value`, `submenu`) and `dk_ui_menu_check`/`dk_ui_menu_checked`, dialogs keep `dk_ui_toast/confirm/choice`; `register_cmds` becomes
`dk_host_register`; `dkrt_blocks` becomes `dk_time_blocks`; `dkrt_log_ev` becomes `dk_log_event`; `dkrt_peek_range`, `dkrt_track_type`,
`dkrt_selected_params` and `dkrt_log_sink_set` move to `platform/debug.h` as `dk_debug_*`. `fw_block_read`, `fw_alloc` and `fw_free` become
`dk_fw_block_read`, `dk_fw_alloc` and `dk_fw_free` (service `runtime`, documented in `core/mem.h`; `usb-fast` uses them for its staging buffer and its storage bench).
`DK_TRACE_INIT` is gone; `DK_TRACE_LOG` calls `dk_log` and stays a macro, so that its arguments are not evaluated when `DK_TRACE` is off.

Limits checked on 2026-11-12: at most 64 imports per module (the largest module, `usb-fast`, needs about 25); runtime import names at most
31 characters (the table field is 32 bytes, as in the `.dkm`; all proposed names fit). `dk_store_read` becomes `dk_files_read(files, path, buf, cap)`.

Example, a tool module:

```c
#include <dk/core/module.h>
#include <dk/core/log.h>
#include <dk/platform/settings.h>
#include <dk/platform/ui.h>

static dk_settings *settings;
static dk_ui *ui;                                  /* stays NULL without core/ui */

static int32_t init(dk_module_ctx *ctx)
{
    int32_t r = dk_settings_open(ctx, &settings);
    if (r != DK_OK)
        return r;
    if (dk_ui_open(ctx, &ui) != DK_OK)
        dk_log(DK_LOG_INFO, "no core/ui: settings menu hidden", 0);
    return DK_OK;
}

static void menu(dk_module_ctx *ctx, dk_ui_menu *menu)
{
    dk_ui_menu_add_text(menu, "BOOT SCREEN");
}

const dk_module_def dk_module = { .init = init, .menu = menu };
```

## 6. Declarations

### 6.1 What is static

Only what the device needs before any module code runs:

| What | Where | Why |
| --- | --- | --- |
| Module id | `module.toml` | Identity |
| One `[[machine]] id` or `[[fx]] id` line per machine or effect | `module.toml` | Machine type numbers are assigned for all selected modules before any loads (`assign::plan`), and stay the same across boots so projects stay valid. Projects refer to these ids |
| `[uses.dt2]`, `[uses.dn2]`: machine rows, param rows, page rows and record bytes per device | `module.toml`, written by `module:check` | The tick check in the device menu (`dkrt_scan`, `dkrt_fit`) reads counts and sizes only |
| DSP section (code, relocations, named imports, voice and FX entries) | Built from `src/dsp/` as today | Voice rows, FX slots, DSP memory |

`module:check` runs the module in the emulator and writes each device's `[uses.<device>]` table from what `init` declared on that device, like a lock file. Nobody maintains it by
hand. The tables are per device because the runtime builds different rows per device (DT2 has no page rows); a module has only the tables of the devices it builds for.

The packer computes the machine and effect keys (below) and copies them, with the device's `[uses.<device>]` counts into the keys table of the `.dkm` header (6.3). `record_bytes` is the `records_size` of the `MODULE_COMMIT` event: the module keeps that record block in the arena for as long as it is loaded, so the needs of the module (`ARENA`) include it.

Ids are written once. `patchkit gen` writes `build/gen/module_ids.h` from the `module.toml` ids, in the same way it writes other
committed per-module files (for example `build/gen/dl-limits.h`), and `patchkit gen --check` guards it. Every module has `build/gen`
first on its include path, so no new build phase is needed. Each constant is the index of the id: its 0-based position in the `[[machine]]`
(or `[[fx]]`) list of `module.toml`, which is the row of its key in the keys table. The names are `DK_MACHINE_ID_<ID>` and `DK_FX_ID_<ID>`, for
example `#define DK_MACHINE_ID_ANALOG 0u` and `#define DK_FX_ID_FSHIFT 0u`. The `_ID_` part keeps them apart from existing constants such as
`DK_MACHINE_GATE`. The keys are not in the header: the packer computes them from the packed module name (the `--name` value when given, else the
module id) and the ids, with `register::machine_key` (hash of `<name>:<machine id>`) and `register::effect_id` (hash of `<name>:<effect id>`), and
writes them into the keys table in `module.toml` order. A normal pack therefore gives the keys the runtime has always used (saved projects stay
valid), and an alias made with `pack --name` gets keys of its own, so two copies of one module do not share an effect id. At commit the runtime
maps each index to its key through the keys table; an index outside the table is refused with `E_DECL_ID`.
A typo is a compile error. `module.toml` accepts `[[machine]]` and `[[fx]]` entries with an `id` and an optional `dsp` (`packages/patchkit/src/module.rs`).

DSP entry points are found by name, so the packer needs no declaration to build the DSP section. The C name of an entry point is
`<id>_render` and `<id>_note_on` for a machine and `<id>_process` for an effect: machine `analog` has `analog_render` and
`analog_note_on`, effect `fshift` has `fshift_process`. The toolchain adds a leading underscore, so the DSP object holds
`_analog_render`, `_analog_note_on` and `_fshift_process` (as `shot` declares `_shot_render` today), and the packer looks up the
underscored symbol. A machine with a sample pool also has `<machine>_sample_rec` and `<machine>_sample_ctl` (`_shot_rec` and
`_shot_ctl` for `shot`, passed today to `DK_SAMPLE_TRACKS(0, _shot_rec, _shot_ctl)`). The packer refuses a module when a symbol is
missing; the message names the symbol and says whether the prefix came from the id or from `dsp`.

The symbol prefix is the id (with `-` as `_`) by default. For DSP code that exists under another prefix, `dsp = "<prefix>"` on the
`[[machine]]` or `[[fx]]` line overrides it (lowercase ascii letters, digits and `_`). The id stays what the keys hash, and
`module_ids.h` uses the id for its constant names; only the symbol names use the prefix.

The first argument of `DK_SAMPLE_TRACKS` is a per-module table id: the number of the sample table of the module. A module has one table
(`dk_sample_pool_open` takes the lowest id), so the id is always 0 and the packer writes 0; the pool symbols carry no id.

A machine may also have a DSP descriptor, the symbol `_<id>_desc` (data); the packer reads its flags (stereo) when the symbol is
defined. An effect needs `_<id>_desc` and `_<id>_process`. The DSP effect records are written in the order of the `[[fx]]` lines, which
is the order of the keys table.

Machine and effect ids are the keys' input: the key is the hash of `<module id>:<id>`. Projects and kit values refer to the keys, so a
machine or effect id stays the id it had before S3. Renaming an id loses the user's
projects.

```toml
[module]
id = "fadedlimes/analog"

[[machine]]
id = "analog"
dsp = "va"          # optional: the prefix of the DSP symbols (_va_render, ...) when it is not the id

[uses.dt2]
machine_rows = 1
param_rows = 4
page_rows = 0       # DT2 has no page rows
record_bytes = 444  # the record block the module keeps in the arena (payload bytes)

[uses.dn2]
machine_rows = 1
param_rows = 4
page_rows = 1
record_bytes = 540
```

The Transfer tab does not check fit: the card holds as many modules as fit on disk. Limits matter only for the modules ticked to load.

### 6.2 Everything else comes from builder calls in `init`

Rule: a child is added by `<parent>_add_<kind>`, as in `dk_ui_menu_add_<kind>`. Only the lifecycle functions, the `_open` accessors
and the `dk_module_ctx_*` functions take `ctx` (section 10). So the module context adds machines and effects, a machine or effect
adds pages, and a page adds params.

| Call | Meaning |
| --- | --- |
| `dk_machine *dk_module_ctx_add_machine(dk_module_ctx *ctx, uint32_t index, const char *name, const char *short_name)` | Add a machine; `index` is `DK_MACHINE_ID_<ID>` |
| `dk_fx *dk_module_ctx_add_fx(dk_module_ctx *ctx, uint32_t index, const char *name, const char *short_name)` | Add an effect; `index` is `DK_FX_ID_<ID>` |
| `dk_page *dk_machine_add_page(dk_machine *m, const char *name, const char *short_name)` | Add a page to a machine; `short_name` is its tab label |
| `dk_page *dk_fx_add_page(dk_fx *fx, const char *name, const char *short_name)` | Add a page to an effect; `short_name` is its tab label (at most 3 characters) |
| `dk_param *dk_page_add_param(dk_page *p, const char *name, const char *short_name, int32_t def)` | Add a machine param; it goes on the next knob of the page |
| `dk_param *dk_page_add_fx_param(dk_page *p, const char *name, const char *short_name, kind, float min, float max, float def)` | Add an effect param |

```c
#include <dk/machine/machine.h>

static int32_t analog_init(dk_module_ctx *ctx)
{
    dk_machine *m = dk_module_ctx_add_machine(ctx, DK_MACHINE_ID_ANALOG, "Analog", "ANA");   /* DSP: va_render, va_note_on (dsp = "va") */

    dk_page *p = dk_machine_add_page(m, "Analog", "ANALOG");
    dk_page_add_param(p, "Detune", "DET",  40);
    dk_page_add_param(p, "Wave",   "WAVE", 0);
    dk_page_add_param(p, "Sub",    "SUB",  0);
    dk_page_add_param(p, "Fold",   "FOLD", 0);
    return DK_OK;
}

const dk_module_def dk_module = { .init = analog_init };
```

On a device that keeps a machine's knobs in page rows (Digitone II), the machine's knob page gets a page row with exactly the given names: `dk_machine_add_page(m, "Analog", "ANALOG")` gives tab label `ANALOG` and title `Analog`. The runtime does not change the case.

Optional properties are setters with sensible defaults (range 0 to 127, no glyph, not latched). The setters keep their names:
`dk_param_range`, `dk_param_glyph`, `dk_param_latched`, `dk_param_on_click`, `dk_page_canvas`,
`dk_fx_routes` and `dk_fx_mix` (the MIX knob default 0 to 127, 127 without the call). The callback types are `dk_param_click_fn`, `dk_page_draw_fn` and `dk_page_event_fn`.

```c
dk_param *play = dk_page_add_param(p, "Play", "PLAY", 0);
dk_param_range(play, 0, 3);
dk_param_glyph(play, DK_GLYPH_PLAY);
dk_param_latched(play);

dk_param *smp = dk_page_add_param(p, "Sample", "SMP", 0);
dk_param_range(smp, 0, 15);
dk_param_on_click(smp, smp_click, 0);            /* a dk_param_click_fn and its arg; the compiler checks the type */

dk_page *wave = dk_machine_add_page(m, "Shot wave", "SHT");
dk_page_canvas(wave, DK_PAGE_WIDE_CANVAS, range_draw, range_event, 0);   /* the last argument is arg, passed to both callbacks */
```

A param shown on more than one page is bound to a slot of each canvas page. `dk_page_bind(page, param, slot)` places a param.
`dk_page_bind_local(page, local, slot)` places a page-local control (a value that is not a param) at a slot. The canvas flags
`DK_PAGE_WIDE_CANVAS` and `DK_PAGE_STOCK_OVERLAY` are the `flags` argument of `dk_page_canvas`. A shot-like machine, with a knob page
and two canvas pages (`modules/machines/shot/src/module.c`):

```c
dk_machine *m = dk_module_ctx_add_machine(ctx, DK_MACHINE_ID_SHOT, "SHOT", "SHT");

dk_page *knobs = dk_machine_add_page(m, "SHOT", "SHT");         /* knob page: params go on knobs 1 to 5 */
dk_param *smp   = dk_page_add_param(knobs, "Sample", "SMP",   0);
dk_param *start = dk_page_add_param(knobs, "Start",  "START", 0);
dk_param *end   = dk_page_add_param(knobs, "End",    "END",   127);
dk_param *play  = dk_page_add_param(knobs, "Play",   "PLAY",  0);
dk_param *pitch = dk_page_add_param(knobs, "Pitch",  "PITCH", 64);
dk_param_on_click(smp, smp_click, 0);

dk_page *ctl = dk_machine_add_page(m, "SHOT", "SHT");           /* canvas page over the stock page */
dk_page_canvas(ctl, DK_PAGE_STOCK_OVERLAY, controls_draw, range_event, 0);
dk_page_bind(ctl, pitch, 0);
dk_page_bind(ctl, play,  1);
dk_page_bind(ctl, smp,   3);
dk_page_bind(ctl, start, 4);
dk_page_bind(ctl, end,   7);

dk_page *wave = dk_machine_add_page(m, "SHOT WAVE", "SHT");    /* wide canvas */
dk_page_canvas(wave, DK_PAGE_WIDE_CANVAS, range_draw, range_event, 0);
dk_page_bind(wave, start, 4);
dk_page_bind_local(wave, 0, 6);
dk_page_bind(wave, end, 7);
```

An FX:

```c
dk_fx *fx = dk_module_ctx_add_fx(ctx, DK_FX_ID_FSHIFT, "Freq shift", "FSH");    /* short name: at most 3 characters; DSP: fshift_process */
dk_fx_routes(fx, DK_ROUTE_SEND | DK_ROUTE_MASTER, DK_ROUTE_SEND);              /* allowed, default */
dk_page *p = dk_fx_add_page(fx, "Freq shift", "FSH");
dk_page_add_fx_param(p, "Shift", "SHFT", DK_BIPOLAR, -1.0f, 1.0f, 0.0f);
```

A tool or raw module declares nothing:

```c
static int32_t init(dk_module_ctx *ctx)
{
    dk_usb_bulk *usb;
    if (dk_usb_bulk_open(ctx, &usb) != DK_OK)
        return DK_E_MISSING;
    return DK_OK;
}

const dk_module_def dk_module = { .init = init, .fini = fini, .tick = tick, .menu = menu };
```

Builder calls never fail one by one. They record what was asked; errors are reported at commit (6.3). On overflow a call returns a
placeholder, so chained calls stay safe.

### 6.3 Commit

The load order is: store and file checks (magic, size, sha256, runtime version, name, import resolution with `E_IMPORT`, relocations, keys table), the module record, the DSP check, a comparison of the header keys with the DSP voice and effect counts (`collect::check_dsp_keys`, `E_DECL_ID`), `dsp::reserve`, the arena copy, then `init`. The DSP check and reserve stay before `init`. They read only the static DSP section and the machine and effect keys from the
header: reserve copies the DSP image into its own arena block and fills the voice and FX row tables. After reserve the staged file is not read again.

The `.dkm` header stays 0xa0 bytes. The pair at 0x90 and 0x94 is `table_off` and `table_size`. They point to the header table, a versioned, tagged table (R7; layout in `packages/dkneeds/src/keys.rs` and `sdk/sdk.toml` `[limits.format]`): magic `DKTB`, version, then entries in tag order, each `u16 tag, u16 count, u32 payload bytes, payload`. NEEDS lists the services the linked imports name with the newest `since` of each and whether the need is hard; USES lists `(resource id, count)` for the `[uses]` counts and `record_bytes`; KEYS lists `(kind, n, keys)` for the machine keys, the effect ids and the keys of `[[record]]` entries; KINDS, SLOTS, HOOKS, EXPORTS and RESOURCES are the entries of a service module (R12). All fields are big-endian, like the rest of the header. `dkneeds::needs_of` and the device menu read USES from the table; `needs_of` adds the arena block of `record_bytes` (rounded to 16, plus the 16-byte block header, as `arena::alloc` takes it) to `ARENA`. Every `.dkm` has a table (a module that needs and keeps nothing has the 8-byte head); a file with no table or another magic is the format of runtime 0.7 and the loader refuses it with `E_CONTRACT`. The packer writes at least 0.8.0 as the version the module needs. The version is the runtime 0.8.0.

The runtime collects the builder calls during `init`, in the module's arena share, bounded by `MAX_RECORDS` (32) and by the size of the `Collect` block, `collect_bytes` (7206 in `sdk.toml` `[limits.format]`, asserted at compile time in `collect.rs`). A shot-like
machine needs about 10 records, the other modules 3 to 7. The collection is refused with `E_DECL_ID` (-27: an index outside the keys table), `E_DECL_USES` (-28: more rows than `[uses]` says) or `E_DECL_FULL` (-74: more calls than the limits allow). When `init` returns `DK_OK`:

1. Check the collection (device limits, `[uses]`, the keys in the header, FX ids of modules already loaded: a duplicate is `E_FX_ID_DUP`), lower it to records and check the records with the `registry::check` rules.
2. Fill each voice row's 8 ranges and its latched mask from the collected params. The ranges are the knob params' min and max, a clamp
   guard the DSP voice code reads.
3. Apply the records (`registry::apply`). FX pages need their FX row, which reserve already claimed.
4. Start the DSP upload (`dsp::launch`). For a module with a DSP part the commit runs before `launch`, and the load returns LOADING.

If any step fails, nothing is written, the runtime undoes the DSP reservation, and the module is refused with a reason. A refused module changes no byte of the firmware tables. If the DSP part or the files cannot be given back, the result is `E_BUSY` and not the original reason (`loader.rs`, `complete`).

Facts behind this (reviews of 2026-11-12):

- `dsp::check` keeps pointers into the staged file only until `dsp::reserve`; `launch` and later passes use the arena copy (`Pending`).
- Type numbers need every selected module's machine keys up front only for the eviction rule (`sticky.rs:173`). Static machine ids in
  the header keep `assign.rs` unchanged.
- FX ids are a 16-bit hash (`register.rs`); a clash (about 1 in 65535 per
  pair) makes one effect load the other's saved kit values. Machines have a sticky map for clashes; FX get a refusal (`E_FX_ID_DUP`).
- The runtime's firmware views are built after the boot hook, so rows written at commit land before the firmware reads them.

### 6.4 Users of declaration content off the device

- Conformance (`pk_conform`) builds its cases from params (slot, min, max, default). It reads the committed rows from the emulated device
  after load, instead of from records. This tests what the device actually has.
- The site gallery keeps its "Knobs:" line (`ModuleCard.tsx:28-29`). `module:check`, which already runs each module in the emulator to
  write `[uses]`, also writes the param names it sees into the committed `build/gen/gallery.json`. This replaces the generator's second
  pass that builds module objects only to read declarations (`patchkit main.rs:513-514`).

### 6.5 Rejected

- Static const records (nested or as statement macros): no runtime change, but Em prefers builder functions for readability and
  composability.
- A builder run by the packer in the ColdFire CPU core at pack time: adds the CPU core to the packer and the browser build.
- Builder calls plus a hand-written copy of the full content as metadata: the same content in two places.

## 7. Boundaries

- **Runtime and SDK**: only `sdk.toml` crosses. Records, imports, status codes and limits are generated for both sides. One generated
  `core/status.h` replaces the error constants copied into each header.
- **SDK and modules**: modules include only public `<dk/...>` headers, never `sdk/src` paths or `gen/`. SDK component code is linked
  into the module automatically; `--gc-sections` (already on) drops what a module does not use. Modules stop listing SDK sources in
  `module.toml`.
- **Firmware**: modules never reach firmware directly. Firmware access goes through `platform/`, implemented by adapters and services.
- `module.toml` holds identity: id, version, name, category, `[targets]`, plus the static lines of 6.1. `type`, `status`, `ports`,
  `conflicts`, `place` and `[[object]]` leave content manifests. The packer infers machine, effect or tool from what the module declares.
  `requires` goes once derived from imports (real today in `remote` and `probe`). Core modules keep `role`.

## 8. Raw modules

A raw module is a `dk_module` with behaviour only (`init`, `fini`, `tick`, `menu`), using `core/` and `platform/` and no components. The
`tool` template makes one. It migrates by adopting components one at a time. The binary contract stays stable, so it keeps working while
the SDK grows. Today `type = "raw"` exists only in test fixtures.

## 9. DSP contract

These come from the profiling work and land after the steps in section 12 that they depend on.

- **DSP rules.** The table `[dsp.registers]` in `sdk/sdk.toml` is the one source of the register contract: registers a module may clobber,
  must keep and never writes, and the entry registers. `patchkit gen` writes the rule comments of the generated headers, `sdk/dsp/REGISTERS.md`
  and `packages/dksharc/src/rules_table.rs` from it. The check lives in `dksharc::rules` and runs in patchkit at module build. It warns first
  and refuses when every module in the repo builds with no warning. Mode bits and memory rules are still to do; parts exist today in
  `sdk/dsp/README.md` "Block routines" and `sdk/dsp/delay/delay.inc` (MODE1.CBUFEN).
  The stock side comes from sharcdb (`regdef`/`reguse` of the interrupt context save at `0x1c02a4`, the same code on DT2 1.17 and DN2 1.12, and of the
  adapter dispatchers; `mem_access` for memory stock code owns). [Finding 130](../findings/130-sharc-interrupt-save-set.md) records the stock save set [D]; a second agent checks it against image
  bytes before [V]. `pk_conform` checks each module against the rules,
  using sharcdb tables built for the module's own DSP section ([profiling.md](profiling.md) 3.3).
- **Cycle cost (S5a, built).** Nobody declares a cost: the DSP descriptors have no `max_cycles_*` words (the machine descriptor is 8 words, the effect
  descriptor 6) and the manifest has no `[conformance]` overrides. `module:check` runs the conformance suite and writes the module's own cost into
  `[uses.<device>]` of `module.toml`, like the row counts: `dsp_cost_voice` for a machine (the worst steady block of one voice, over every conformance case)
  and `dsp_cost_fx` for an effect (its worst process call, the `core/fx` adapter around it included). The packer copies them into the header table as the
  two resources `DSP_COST_VOICE` (id 5) and `DSP_COST_FX` (id 6) of `[records.resources]`; `dkneeds` reads them into `Needs`. The unit is **instructions**
  per block (SHARC+ instructions the concrete core counts) until the device calibration (P9); the name "cycles" waits for it. A module that was not
  measured has cost 0 and passes any headroom. The note-on block has no resource: the conformance suite checks it for each module alone against the whole
  headroom, never added to the other voices' steady blocks. Today's costs are a floor for a module whose worst case the conformance cases do not drive
  (`digikit/shot` plays no sample in the suite, so its cost is far below its worst block); the cases are the place to fix that.
- **Voice rule and broker (S5a, built).** The rule is one integer function, `check_selection(voices, headroom, machines, fx)` in `dkrt-fit` (`no_std`):
  `max(machine costs) * voices + sum(effect costs) <= headroom`. Every voice may run any selected machine, so the largest per-voice cost counts for all voices;
  the effects add up. The same code runs in the boot planner (`plan.rs`, `judge`), the DIGIKIT menu (`fit_set`), `patchkit fit` (and `patchkit.wasm`, so the
  Transfer tab) and `module:check`; `dkprof::budget` re-exports it for the host. A capacity carries the load as two kinds of `[capacity.kinds]`:
  `DSP_COST` (limit: the device's headroom; used: the load so far; largest: the largest per-voice machine cost so far, the state of that maximum) and
  `DSP_VOICES` (limit: the device's voices). The planner keeps no state of its own: `fits_now` and `take` update those two entries. A module that takes the
  load over the headroom is refused with `E_PLAN_FIT` and the kind `DSP_COST`; the menu shows `DSP LOAD need > free FREE` (instructions per block).
- **Headroom, voices and the margin come from the generated device profile.** `patchkit gen` writes `DSP_HEADROOM` and `DSP_VOICES` for each device
  (`modules/core/runtime/build/gen/device-<dev>.inc`, and `DeviceProfile.dsp_headroom` and `dsp_voices`); the runtime reads two words, no float, no table
  on the device. The headroom is what `dkprof::budget` computes from `[board]`, `[adapter.machine] voices` and `devices/cycle-factors.json`: the block at 85
  percent (the safety margin until P9), with instructions projected to cycles by the "high" factor, less the stock firmware's load (`stock_busy`). Today
  194,244 instructions on the Digitakt II and 92,796 on the Digitone II (`patchkit budget`). Both devices have 16 voices.
- **`DK_SPAN` (S5a, defined; producers are P8a).** `DK_SPAN_BEGIN(owner)` and `DK_SPAN_END(owner)` in `<dk/core/trace.h>` (ColdFire C) and
  `DK_SPAN_BEGIN(SKIP, RA, RB)` and `DK_SPAN_END(SKIP, ROW, RA, RB, RC)` in `dsp/contract/dk_trace.inc` (SHARC+). They are always compiled in: they test
  the sink's pointer (C) or the stats window's pointer slot `_dk_stats_ptr` (SHARC+), and only when it is set read the cycle counter and report
  `(owner, cycles)` to the `span` sink (C: `dk_span_emit`, runtime-and-services 5.11) or to the stub's `_dk_span_add` (SHARC+). With no sink the cost is one
  load, one test and one branch for each macro, and the arguments are not evaluated; the SHARC+ counter read (`EMUCLK`, which takes the emulator out of the
  fast tiers, `UNSAFE_READ_NAMES` in `tools/sharc_dev/sharc_rsgen.py`) runs only while a window is registered. Only service dispatchers and the image
  stub use them; `mise run check:layers` (rule spans) refuses them in content modules and apps. Checked by reading the disassembly of a test use (ColdFire
  `tstl`/`bnes` on the sink word before any call; SHARC+ load, `PASS`, `IF EQ JUMP` before the `EMUCLK` read). The no-sink cost of the real dispatchers (a paired
  A/B of instruction counts within 0.02 percent) is measured with the first producer (P8a).
  The **stats window** is words in DSP data memory: a header of 4 words (magic `DKSW` 0x444b5357, version 1, rows 16, blocks measured since the window was
  cleared), then 16 rows of 4 words (owner row number, calls, cycles summed, largest span), all 32-bit and wrapping; the reader takes differences. The
  pointer slot `_dk_stats_ptr` holds the window's word address (0: no window, nothing is measured). The numbers are defined twice, in `dk_trace.inc` and in
  `trace.h` (the ColdFire reader); the third reader (P8b, dsp-link) takes them from `sdk.toml` if the copies drift.

## 10. Style and enforcement

- C11: `-std=c11` for ColdFire builds, as the host tests already use.
- A repository `.clang-format` and a format check target for C. `clang-tidy` later.
- Names: functions `dk_<area>_<verb>`, types `dk_<thing>`, constants `DK_<AREA>_<NAME>`. No version suffixes (`_v1`, `_V2`); versions
  live in `sdk.toml`: one runtime version, and `since` of each import.
- Naming (agreed 2026-11-12):

| Rule | Form | Examples |
| --- | --- | --- |
| The module context is `ctx` and only that | type `dk_module_ctx`; parameter `ctx` | `int32_t init(dk_module_ctx *ctx)`, `void fini(...)`, `void tick(...)`, `void menu(dk_module_ctx *ctx, dk_ui_menu *menu)` |
| Entry point | type `dk_module_def`, constant `dk_module` (the packer looks the symbol up) | `const dk_module_def dk_module = { .init = init };` |
| A function is named after the handle type it works on | `dk_<type>_<verb>`; the `_open` accessors are named after the service they open: `dk_settings_open(ctx, &s)` returns a `dk_settings` | `dk_settings_get` works on `dk_settings`; `dk_module_ctx_device(ctx)` and `dk_module_ctx_runtime_version(ctx)` (the runtime version) work on `dk_module_ctx` |
| A function with no handle is named after its service | `dk_<service>_<verb>` (stateless calls, or calls that act on the calling module) | `dk_log_event`, `dk_time_blocks`, `dk_hash32_init`, `dk_param_get`, `dk_debug_peek`, `dk_host_register` |
| Rows of a menu belong to `dk_ui_menu` | `dk_ui_menu_<verb>`; adding a row is `dk_ui_menu_add_<kind>` | `dk_ui_menu_add_text/action/toggle/value/submenu`, `dk_ui_menu_check`, `dk_ui_menu_checked` |
| A child is added by its parent: `<parent>_add_<kind>` | `dk_<parent>_add_<kind>` | `dk_module_ctx_add_machine`, `dk_machine_add_page`, `dk_page_add_param`, `dk_ui_menu_add_text` |
| A user-data pointer is `arg` | in sdk.toml `sig` strings, generated and hand-written headers | `dk_io_post(step, done, arg)`, `dk_job_post`, `dk_intro_take`, `dk_files_list(f, root, path, notify, arg)`, `dk_ui_menu_add_action(menu, text, run, arg, value)` |
| Callback types end in `_fn` | `dk_<service>_<what>_fn` | `dk_host_cmd_fn`, `dk_io_step_fn`, `dk_io_done_fn`, `dk_job_step_fn`, `dk_job_done_fn`, `dk_intro_frame_fn`, `dk_intro_done_fn`, `dk_files_event_fn`, `dk_ui_action_fn`, `dk_param_click_fn`, `dk_page_draw_fn`, `dk_page_event_fn` |
| Raw bytes | `void *` and `const void *`; lengths `uint32_t` | `void *dk_alloc(uint32_t size)`, `void dk_free(void *p)`, `dk_files_write(f, h, offset, const void *buf, len)` |
| One handle per service | `dk_<service>_open(ctx, &handle)` gives it; every other call takes the handle first | `dk_files_open`, then `dk_files_list/poll/cancel/create/write/close/abort/delete(files, ...)` |
| Which functions take `ctx` | only lifecycle functions, `_open` accessors and `dk_module_ctx_*` functions (the device and version accessors and the two add builders) | `dk_host_register(first, count, fn)` takes none |

  A callback of `dk_ui_action_fn` gets `void (void *arg, int32_t value)`: `arg` is the user pointer, `value` the value the row was added with (an action) or the
  new value (a toggle or number row). The old typedef names (`dk_service_cmd`, `dk_file_event`, `dk_ui_act`, `dk_io_step`, `dk_io_done`, `dk_job_step`, `dk_job_done`,
  `dk_intro_frame`, `dk_intro_done`) are gone; only the `_fn` names exist.
- Macros only where a function cannot do the job: `DK_TRACE_*` and `DK_SPAN_*` (arguments must not be evaluated when off). Error checks
  such as `DK_PARAM_IS_ERROR` become `static inline` functions.
- `layercheck` rules: the include direction between layers; imports declared only in `platform/`; modules include only public headers;
  no relative SDK paths.
- The README says, for each layer, what belongs there. A new SDK feature goes in the layer and folder its concept names.

## 11. Tests and Nx

- SDK tests mirror the include tree (`sdk/tests/page/`, ...) and use the Vitest and C pattern (`tests/pages.test.ts`).
- Components are tested on the host against a small fake `platform`.
- A module change never selects SDK tests. An SDK change reruns only device tests whose module bytes change (true today). Split `sdk` into
  per-folder Nx projects only if its host tests grow slow.
- Python-compiled host tests of other modules move to the Vitest and C pattern.

## 12. Steps

One PR per step. The roadmap names them S1 to S5.

### S1 SDK restructure (byte-identical): done

Rule: every built `.dkm` stays byte-identical. What shipped:

- Hash task `mise run sdk:hashes` (`tools/devtools/sdk_hashes.mjs`: `record`, `compare`, `--force`). It packs every non-core module and
  fixture (`tests/fixtures/modules` included) for each device, hashes the `.dkm` files and the core objects, and hashes the DT2 1.17 and
  DN2 1.12 images the device patch set makes. A build failure, a missing firmware image or an unexpected unpackable module is fatal.
  `compare --force` shows 0 differences against a baseline recorded on the commit before the move.
- The tree of section 4: `include/dk/{core,platform,page,machine,fx,chooser,sample}`, `src/{chooser,sample}`, `dsp/` by concept with
  `dsp/contract` (formerly `sdk/asm`) on the SHARC include path, `templates/{machine,fx,tool}` (the `service` template became `tool`) and
  the vectors in `modules/core/runtime/vectors`. Every include was updated directly; there are no forwarding headers.
- Names: `chooser` is its own component (`chooser.h`, `logic.h`); `picker_logic` became `chooser/logic`; the `pk_*` helpers became
  `dk_text_*`, `dk_path_*` and `dk_chooser_row*`; "pool" replaces "track" and "strack" for the 16-entry sample store (`dk_sample_pool_*`,
  `DK_SAMPLE_POOL_*`); the waveform helpers are `dk_sample_view_*`; `dk_picker_*` is `dk_sample_picker_*`. The test-only region wrappers
  were deleted and their tests draw through `view.h`.
- Generated from `sdk.toml`: `core/status.h` (one definition of each `DK_E_*` code; the family error names stay as aliases), `gen/sample.h`
  and `dsp/contract/dk_sample_pool.inc` (the pool entry and control word offsets, `DK_SAMPLE_POOL_*`, `DK_SAMPLE_PEAK_BINS`,
  `DK_SAMPLE_WINDOW_MAX`). The runtime's `gen_sdk.rs` carries the same offsets and `sample_tracks.rs` asserts its struct layout against them.
- `sdk/project.json`: `sdk:check-generated` names the generator's inputs and outputs only; `sdk/src`, `sdk/dsp` (except the generated
  includes) and `sdk/templates` select no SDK task.

### S2a C hygiene: done

- A repository `.clang-format` that matches the dominant style today: 4-space indent, function braces on their own line, control braces
  on the same line, `*` next to the name, 100 columns. Reformat all C in one whitespace-only commit; `sdk:hashes compare --force` proves it
  byte-identical. Add a C format check to the static checks.
- `-std=c11` for ColdFire builds. The 2026-11-12 scan found no GNU extension that strict C11 rejects (`__attribute__` and `__asm__` are
  accepted). Fall back to `-std=gnu11` if the compiler disagrees.
- Helpers into `core/`: `core/bytes.h` (big-endian put and get, replacing about 10 copies of `put32`/`get32`), `core/text.h` and
  `src/core/text.c` (length, bounded copy, equal, unsigned and signed to text, and the `dk_text_*` functions now in `chooser/logic.c`, all
  with `uint32_t` lengths), `core/path.h` and `src/core/path.c` (`dk_path_*`). Text centring stays in `shot` until `platform/draw` offers a
  text-width query.
- SDK sources are linked automatically: the packer builds the SDK sources a module uses and links them with `--gc-sections`. Modules stop
  listing `sdk/src` files in `module.toml`. A received `.dkmod` (source only) builds against the receiver's SDK, with a version check.
- Check: hashes for the format commit; device tests of the modules whose bytes change; static checks.

### S2b Entry point and imports: done

What shipped, in three steps on `lane/sdk-design`:

- **S2b-1, the new contract beside the old one.** The entry type is `dk_module_def` and the constant `dk_module` (a typedef and an object of one name share a namespace in
  C, so `const dk_module dk_module` does not compile). The imports got `service`, `since`, `context` and `sig`; `dk_module_ctx` and the handle types are opaque; the
  providers (`core/ui`, `core/input`, `core/usb-bulk`, `core/dsp-link`) export the new names; a per-module `tick` runs once per UI pass; the loader binds a stub for an
  absent provider and records the provider of the last `_open` stub that answered `DK_E_MISSING` during `init` as the module's need when `init` returns `DK_E_MISSING`.
- **S2b-2, migration.** The five tools, `shot` and its picker, the chooser, the C fixtures, the two Rust fixtures and the `tool` template moved to `dk_module` and the named
  imports. The UI calls take the `dk_ui *` handle (`dk_ui_menu_new`, `dk_ui_toast`, `dk_ui_confirm`, `dk_ui_choice`, `dk_ui_tick`). The tests of the Api mechanism itself were
  rewritten as tests of the version refusal (a module that declares a newer api minor, or major 1, gets `E_CONTRACT`) and of a missing service. These test changes get an
  independent review before merge.
- **S2b-2 end, the old contract deleted.** The Api table (`[api]` and `[api.types]` of `sdk.toml`, `gen/api.h`, `dkrt_api`, `uses_api`, `DK_API_HAS`, `api->size`,
  `core/api.h`), the loader's `dkm_init`, `dkm_fini`, `dkm_menu` and `dkm_slot_<n>` path with the slot table (`SLOTS`, `E_SLOT`, `H_SLOT_*`, slot ownership, the slot-0 call of the UI pass),
  `DK_MENU`, the legacy branches of `settings.h` and `intro.h`, `platform/file.h`, the old imports without a service (`input_key`, `input_turn`, `usb_bulk_*`, `dl_set_tick_cb`,
  `dkrt_blocks`, `dkrt_log_ev`, `dkrt_peek_range`, `dkrt_track_type`, `dkrt_selected_params`, `dkrt_log_sink_set`, `fw_*`, `register_cmds`) and the providers' old exports are gone. Only a compiler
  helper (`memcpy`, `memset`, `memcmp`, `__muldi3`, `__udivdi3`, `__umoddi3`) is an import without a service.

What changed in the binary contract (runtime 0.6.0, every `.dkm` changes):

- The packer writes the newest `since` among the imports a module links (0 when it links none) into the header (S3 replaced the contract pairs by the one runtime version).
- The `.dkm` header is 0xa0 bytes in one form: `init`, `fini`, `menu` and `tick` are the offsets at 0x80, 0x84, 0x88 and 0x8c (the slot table is gone, so no header size 0xa4 or 0xa8). A data-only module has the
  stub as `init`. The packer refuses an object that has no `dk_module`, or that still exports `dkm_init`, `dkm_fini`, `dkm_menu` or `dkm_slot_<n>`.
- `DkrtModule.slots` and `DkrtStatus.slot_calls` are gone (`DkrtStatus.tick_calls` counts the module ticks); the `MOD_LIST` event carries whether the module has a tick, where it carried the slot mask.
- The crash record's `slots_entered` word is now the phase of the module code at the fault (`dlog::PHASE_*`: 1 `init`, 2 `tick`, 3 `fini`, 4 a callback, 0 none); `CRASH_MOD` names its
  argument `phase_stable`, and `dklog crash` prints "during init", "during tick" and so on.
- The runtime `State` shrinks to 904 bytes (no slot table: `ST_*` offsets of `tools/patchkit/pk_dkrt.py`). `E_SLOT` (-11) is retired.
- The firmware memory and card calls are `dk_fw_alloc`, `dk_fw_free` and `dk_fw_block_read` (service `runtime`). The temporary sample bank (three imports) was removed with its fixture in S3.
- `DK_TRACE_LOG` calls `dk_log` at the trace level and stays a macro, so the arguments are not evaluated when `DK_TRACE` is off; `DK_TRACE_INIT` is gone.

### S3 Builders and the runtime commit path (done)

Work packages, in this order:

- **A. Interfaces first.** Sections 5, 6 and 10 of this text, the `sdk.toml` entries for the builder calls and callback types, and the
  header layout (the keys table of 6.3). B to D start only after A is merged into the lane.
- **B. Runtime commit path.** Collect builder calls during `init` in the module's arena share (`MAX_RECORDS` stays 32); commit as in
  6.3, with DSP check and reserve unchanged before `init`; refuse a duplicate FX id across loaded modules. After F the runtime loads
  only files with a keys table, and a module is refused when the runtime version it needs has another major or minor, or a higher patch
  (runtime 0.7.0).
- **C. Packer and dkneeds.** The packer finds DSP entry points by name with the leading underscore (6.1), writes the `module.toml`
  machine and effect keys and `[uses]` into the keys table of the header (`keys_off`, `keys_size` at 0x90 and 0x94), and stops reading
  `.dk.desc`. `module.toml` accepts `[[machine]]` and `[[fx]]` with an id again. `dkneeds::needs_of` and the device menu read `[uses]`
  from the keys table.
- **D. Gen and module:check.** `patchkit gen` writes `build/gen/module_ids.h` (`DK_MACHINE_ID_<ID>`, `DK_FX_ID_<ID>`); `gen --check`
  guards it. `module:check` writes `[uses]` and the gallery param names (`build/gen/gallery.json`). Conformance reads committed rows
  from the emulated device. B, C and D run in parallel, each in its own lane.
- **E. Migration.** The machines and the FX, and the fixtures (scripted for the fixtures), by three coders.
- **F. Cleanup.** Delete the `[contract]`
  table (one version: the runtime's; `since` is a runtime version string) and the major-2 `.dkm` loading path. The declaration macros, their
  headers and `desc.rs` stay for the two patched-in twins (`test/patched-pluck`, `test/twin-dn2`): the headers moved out of the public SDK to
  `sdk/patched-in/include`, which patchkit adds to the include path of patched-in (not `dynamic`) modules only; a follow-up deletes the
  patched-in content path.
- Check: every module's and fixture's device tests on both devices; SHARC golden hashes and PCM pins unchanged; refusal cases for "more
  than `[uses]`", "unknown machine id", "missing DSP entry symbol", "duplicate FX id" and "no free row". `MAX_RECORDS` check: no module
  collects more than 32 records (the largest, `shot`, about 10: 1 machine, 5 params, 1 page, 1 click action, 2 canvas pages); the
  runtime refuses a module that overflows it, and a refusal test covers that case.
- Size: about 4 agent days. Risk: medium, because it changes the boot path; lower than first planned, because DSP binding stays
  before `init`.

### S4 Templates and docs

- The `machine`, `fx` and `tool` templates are on the builder API (done). Nx generators: not done.
- Write `docs/MODULES.md` (architecture and reasons); update `sdk/README.md`, `MODULE-AUTHORING.md` and the `digikit-module` skill. The skill is done.

### S5 DSP contract

- DSP rules, cycle cost and `DK_SPAN` (section 9). Depends on profiling P3 sharcdb tables for module DSP sections, P4 per-module cost,
  and runtime-and-services R4 spike (static part).
- **S5a (done):** the derived cost in the header (`DSP_COST_VOICE`, `DSP_COST_FX`), the shared `check_selection` in the planner, the menu and `patchkit fit`,
  the headroom and voices in the device profile, the `max_cycles_*` words and the `[conformance]` overrides deleted, and the `DK_SPAN` definition.
- **S5b (in progress):** the DSP rules section and its check (dksharc slice 2, the stock save-set finding), see section 9, "DSP rules".

## 13. Open items

- [O] `type`, `status`, `ports`, `conflicts` and `[[object]]` are still in content manifests (analog, shot, the templates), and the packer still parses `type`. Section 7 states the end state. Remove them in a code item.
- [O] `state_bytes` and `stack_bytes` stay in the DSP descriptor block (asm); the packer reads them as today.
