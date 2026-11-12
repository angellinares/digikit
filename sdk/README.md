# digikit SDK

The SDK is the stable interface between content modules and the digikit runtime.
It does not depend on the device. Device differences live in the `core/*` adapters
and the device profiles (`devices/*.toml`).

## Layout

| Path | What | Edit |
| --- | --- | --- |
| `sdk.toml` | The contract spec: versions, the `dk_api` table (old entry), the imports with their `service`, `since` and `context`, machine and effect layouts, log kinds, protocol numbers, limits, settings. | yes |
| `include/dk/core/` | The module contract: `<dk/core/api.h>` (the `dk_api` table and run-time imports; provides `DK_API_HAS(api, field)`, true when the table is long enough to hold the field and the field is set), `<dk/core/trace.h>` (trace marks), `<dk/core/bytes.h>` (big-endian 32-bit access, header only), `<dk/core/text.h>` (bounded text: length, copy, equal, append, concatenate, number text, truncation, size and time text), `<dk/core/path.h>` (join, parent, leaf) and `<dk/core/status.h>` (the `DK_E_*` status codes; generated, do not edit). No device access. | `api.h`, `trace.h`, `bytes.h`, `text.h`, `path.h` yes; `status.h` no |
| `include/dk/platform/` | What the device provides, one header per service group: `ui.h` (menus, lists and dialogs on the screen), `param.h` (read and write a machine parameter from a `DK_PARAM_ACTION` callback), `settings.h`, `files.h` (read-only file listing, API 1.4), `file.h`, `io.h`, `job.h` (background jobs, API 1.8), `hash32.h`, `usb_bulk.h`, `intro.h`, `data.h`. | yes |
| `include/dk/page/` | Pages: `page.h` (`DK_PAGE` and `DK_KNOB`, shared by fx and machine) and `widgets.h` (small page widgets). | yes |
| `include/dk/machine/`, `include/dk/fx/` | The machine contract (`machine.h`) and the effect contract (`fx.h`). | yes |
| `include/dk/chooser/` | The slot list chooser (`chooser.h`) and its pure functions of row sort order and pool row text (`logic.h`). | yes |
| `include/dk/sample/` | Sample components: `bank.h` (module-owned mono float sample banks, API 1.3), `pool.h` (per-module stereo sample pool, API 1.5 to 1.7), `view.h` (waveform drawing for native pages) and `picker.h` (on-device sample picker for a 16-entry pool). | yes |
| `include/dk/gen/` | C layouts and constants generated from `sdk.toml` (including `imports.h`, the prototypes of the service imports, and `sample.h`, the word layout of a sample pool entry). Included by the headers above, not by modules. | no |
| `src/` | Compiled SDK sources, in the folders of `include/dk/`: `chooser/chooser.c`, `chooser/logic.c`, `core/text.c`, `core/path.c`, `sample/picker.c`. The packer builds them into one SDK library and links the functions a module calls (`--gc-sections` drops the rest); a module does not list them in its `module.toml`. | yes |
| `dsp/` | SHARC+ code by concept (`osc/`, `filter/`, `dynamics/`, `delay/`, `util/`, `sample/`). See [dsp/README.md](dsp/README.md). `dsp/contract/` is on the SHARC+ include path and holds the call convention (`call.inc`), the includes generated from `sdk.toml` (`dk_fx.inc`, `dk_machine.inc`, `dk_sample_pool.inc`) and the hand-written `dk_trace.inc` (see Tracing). | yes |
| `templates/` | Module templates for `mise run module:new`: `machine/`, `fx/`, `tool/`. | yes |
| `tests/` | Host C tests, in the folders of `include/dk/` (`sdk:test-host`). | yes |
| `../modules/core/runtime/vectors/` | Fit and settings expected results shared by the Rust, Python and web tests. They belong to the runtime, not to the SDK. | no |

### Where a new thing goes

Include direction: `core` <- `platform` <- components (`page`, `machine`, `fx`, `chooser`, `sample`). A component may include
another component, without cycles.

- `core/` is the module contract: entry point, status, log, small helpers. No device access.
- `platform/` is the only layer that declares imports. A component never declares an import: it calls `platform/`.
- A new concept is a new component folder in `include/dk/`, with `src/<concept>/` for compiled code, `tests/<concept>/` for its host
  test and `dsp/<concept>/` for SHARC+ code. If it needs the device, its imports go in `platform/<name>.h`.
- Generated files go in `include/dk/gen/`, `dsp/contract/` and `include/dk/core/status.h` only.
- Names: functions `dk_<area>_<verb>`, types `dk_<thing>`, constants `DK_<AREA>_<NAME>`.

`patchkit gen` writes every generated file. `patchkit gen --check` fails when one is stale
(`mise run gen:modules -- --check`).

`pnpm nx:pilot run sdk:test-host` compiles the machine and effect fixture
declarations with the host `cc` compiler and checks their public-header records:
machine parameter units, effect parameter values, DSP symbols and routing.
It needs no firmware or DSP toolchain. Each shipped module still owns its device
conformance checks; this fast SDK check does not replace them.

## The entry point and the services

A module of the new contract defines one constant, `dk_module`, and calls the device through named imports grouped by service. There is no
Api table and no `dkm_*` symbol (docs/plans/sdk.md section 5). `include/dk/core/module.h` has `dk_module_def` and `dk_module_ctx`; the prototypes of the
imports are generated into `include/dk/gen/imports.h` from the `[[import]]` entries of `sdk.toml` that have a `service`.

```c
#include <dk/core/module.h>              /* first */
#include <dk/core/log.h>
#include <dk/platform/settings.h>

static dk_settings *settings;

static int32_t init(dk_module_ctx *ctx)
{
    int32_t r = dk_settings_open(ctx, &settings);   /* a handle per service, opened in init */
    if (r != DK_OK)
        return r;
    dk_log(DK_LOG_INFO, "ready", 0);
    return DK_OK;
}

const dk_module_def dk_module = {.init = init};  /* init, fini, tick, menu: all optional */
```

- `tick(ctx)` runs once per UI pass for every loaded module. `menu(ctx, menu)` adds the rows of the module's SETTINGS... menu.
- The type is `dk_module_def` and the constant is `dk_module` (the packer looks the symbol up). `ctx` is only the module context; the user-data pointer of a callback is `arg`, and callback types end in `_fn` (`dk_io_step_fn`). The naming rules are in docs/plans/sdk.md section 10.
- Each import has a `service` (`runtime`, `samples`, `machines`, `fx`, `ui`, `input`, `usb_bulk`, `dsp_link`), a `since` (the api minor
  that added it) and a `context` (`init`, `ui`, `io-step`, `any`, `isr`). The generated header repeats the last two in the comment of each
  prototype. `patchkit pack` takes the runtime minor a module needs from the newest `since` it links and prints the services it needs
  (`services <file>: ...`).
- A service that another module provides (`ui`, `input`, `usb_bulk`, `dsp_link`) may be absent: its `_open` returns `DK_E_MISSING`, the other
  calls return the same code or do nothing, and the module still loads. Without `dk_ui_service_open` returning `DK_OK` a module does not call
  the menu functions.
- The headers of the services are `core/log.h`, `core/mem.h`, `core/time.h`, `platform/host.h`, `settings.h`, `files.h` (listing and writing),
  `io.h`, `hash32.h`, `intro.h`, `param.h`, `samples.h`, `ui.h`, `input.h`, `usb_bulk.h`, `dsp_link.h` and `debug.h`.

The old entry (`dkm_init(const dk_api *api)`, the `dk_api` table, `DK_API_HAS`, `uses_api`, the `dkm_slot_<n>` functions and the
`dkm_menu` export) still works in this version and goes away in S2b-2 when the modules and templates move. A module uses one entry or the
other, never both (the packer refuses it). The old headers keep their old declarations for it; where an old name is the new name of
another call (`dk_settings_get`, `dk_settings_put`, `dk_settings_remove`, `dk_intro_take`) the old wrapper is defined only when
`<dk/core/module.h>` was not included before the header, and `module.h` stops the build if the old wrapper is already there. `dk_ui_open` is
still the old call that opens a menu; the service accessor is `dk_ui_service_open` until then, when it is renamed `dk_ui_open`. The templates and the module guide still show the old entry.

## Include paths

The build passes these search paths, in this order:

- ColdFire C and assembly: the module's `build/gen/`, the module's `src/`, `modules/core/fw/inc`, then `sdk/include`.
- SHARC+ assembly: the module's `build/gen/`, the module's `src/`, `sdk/dsp/contract`, `sdk` (for `#include "dsp/<concept>/<name>.inc"`), then the directories of the module's sources.

A header in the module's `src/` is shared by C and SHARC+ sources. Use only decimal `#define`s and `//` comments on their own line: the SHARC+ assembler keeps a trailing comment in the macro text.

## Declaring an effect

An effect declares itself, its parameters and its page in a C file of the module, with `DK_FX`, `DK_FX_PARAM` and `DK_PAGE`
from `<dk/fx/fx.h>`. This is `modules/fx/freqshift/src/module.c`:

```c
#include <dk/fx/fx.h>
#include "freqshift.h"

DK_FX(fshift, .process = "_fs_process", .desc = "_fs_desc",
      .mix = 127, .route = DK_ROUTE_SEND, .routes = DK_ROUTE_SEND | DK_ROUTE_MASTER);
DK_FX_PARAM(fshift, FSHIFT_SHIFT_WORD, "Freq Shift", "SHIFT", DK_BIPOLAR, -64, 63, 0);
DK_PAGE(fshift, "FSH", "Freq Shifter", DK_KNOB_AT(FSHIFT_SHIFT_WORD, 0));
```

List the file as an `[[object]]` in `module.toml`. `patchkit pack` reads the records; the device never sees them.
`patchkit desc MODULE_ID` prints them. The fields and rules are in
[docs/MODULE-AUTHORING.md](../docs/MODULE-AUTHORING.md), section 5.4.

## Declaring a machine

A machine is declared the same way, with `DK_MACHINE`, `DK_PARAM` and `DK_PAGE` from `<dk/machine/machine.h>`.
This is `modules/machines/pluck/src/module.c`:

```c
#include <dk/machine/machine.h>

DK_MACHINE(pluck, .name = "Pluck", .short_name = "PLK", .render = "_pk_render",
           .note_on = "_pk_note_on", .desc = "_pk_desc");
DK_PARAM(pluck, 0, pluck_decay, "Decay", "DECAY", 0, 127, 64);
DK_PARAM(pluck, 1, pluck_pick, "Pick position", "PICK", 0, 127, 32);
DK_PARAM(pluck, 2, pluck_bright, "Brightness", "BRIGHT", 0, 127, 80);
DK_PARAM(pluck, 3, pluck_color, "Excitation colour", "COLOR", 0, 127, 40);
DK_PAGE(pluck, "PLUCK", "Pluck", DK_KNOB(0), DK_KNOB(1), DK_KNOB(2), DK_KNOB(3));
```

Parameter values are knob units 0 to 127. The stored word is the value times 256. The DSP symbols are required: a machine has no data-only form.
A machine has at most 6 parameters on the Digitakt II and 4 on the Digitone II, in word order, and one page with `DK_KNOB` only. The adapter
places word w on a fixed knob.
The rules are in [docs/MODULE-AUTHORING.md](../docs/MODULE-AUTHORING.md), sections 5.2 and 5.3.

`DK_PARAM_GLYPH(machine, word, GLYPH)` (in `<dk/page/page.h>`) sets the knob look of a parameter. Without it the glyph is `DK_GLYPH_ENCODER`:

| Glyph | Range |
| --- | --- |
| `DK_GLYPH_ENCODER` (default) | 0 .. 127 |
| `DK_GLYPH_FADER` | 0 .. 127 |
| `DK_GLYPH_PAN` | 0 .. 127 (centre 64, shows L/R) |
| `DK_GLYPH_ONOFF` | 0 .. 1 |
| `DK_GLYPH_TUNE` | 4 .. 124 (centre 64, one unit = one semitone) |
| `DK_GLYPH_PLAY` | 0 .. 3 (REV, REV.L, FWD.L, FWD; the Digitone II shows 0 .. 3) |

## Tracing

Trace marks tell you where a module was when it faulted or hung. They are opt-in and cost nothing when off.

| Side | Include | Macro | What it does when on |
| --- | --- | --- | --- |
| ColdFire C | `<dk/core/trace.h>` | `DK_TRACE_INIT(api);` once per source file in `dkm_init`, then `DK_TRACE_LOG("msg", value);` | Calls `api->log(msg, value)`: the message (24 bytes kept) and a word go to the runtime's log ring (`dk:log`). |
| SHARC+ | `#include "dk_trace.inc"` | `DK_STEP(3, R0);` | Loads the step number 3 into the scratch register `R0` and stores it in the word `_dk_step`. Pick a register that is free at that point: SHARC+ cannot store an immediate to memory. |

The C macro is `DK_TRACE_LOG` because the build define is `DK_TRACE` and a function-like macro of that name would clash with it.

Build with `--trace` to switch them on: `mise run module:pack -- ID --device dt2 --trace`, `mise run module:check -- ID --trace`
(size lines), `mise run dk:dev -- ID --store ID --trace`. `--trace` defines `DK_TRACE` for the C sources (`-DDK_TRACE`) and the SHARC+
sources (`#ifdef DK_TRACE`), and sets bit 31 of the header word at 0x30 of the `.dkm` (`patchkit dkm-info` prints `trace yes`; a runtime
that does not know the bit ignores it). Traced and plain objects are cached apart (`out/cache/dko/<id>+DK_TRACE/`).

What you see:

- DIGIKIT, ABOUT: the module's result row ends in ` TRACE` (for example `- OK TRACE`).
- The fault record: the core clears `_dk_step` before each call into a module, the block tail copies it into the block's reply record, and the fault record prints the step of the last block that ended, for example `FX SEND STEP 3`. A step stored in the call that stops the DSP is lost with the core: put the mark before the call that you suspect on an earlier block, or make it a state that holds from block to block.
  A traced module imports `_dk_step` from `core/dyndsp`; a runtime without that export refuses it with `DSP IMPORT`.
- The log: the `DK_TRACE_LOG` messages.

Off means off: without `--trace` both macros expand to nothing (the arguments of `DK_TRACE_LOG` are not evaluated), the objects have no extra
instruction, data or import, and the `.dkm` is byte-identical to one built from sources without the marks
(`packages/patchkit/tests/trace.rs`). A `DK_STEP` costs two instructions when on, so keep it out of the innermost loop.

## Writing modules

See [docs/MODULE-AUTHORING.md](../docs/MODULE-AUTHORING.md). The plan for the SDK API is
[docs/plans/sdk-api.md](../docs/plans/sdk-api.md).
