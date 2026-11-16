# Writing a digikit module

This is the how-to for content modules. A content module adds a machine, an effect or a tool to a Digitakt II or a Digitone II. You build it
into a `.dkm` file. The digikit runtime loads the file from the card at boot. You do not patch a firmware image for it.

[MODULES.md](MODULES.md) says what a module is and why it is built this way: the entry point, the builder calls, what is fixed in
`module.toml`, the load path and the versions. This guide gives the steps. The SDK headers and templates are described in
[sdk/README.md](../sdk/README.md).

Run every command from the repository root. Nothing here touches hardware. Flashing is a separate step (see "Safety and hardware" in
[PATCHING.md](PATCHING.md)). No file in the repository contains firmware bytes.

## 1. What a module is

There are three kinds of content module. The packer infers the kind from what the module contains. `module.toml` has `[[machine]]` entries
for a machine, `[[fx]]` entries for an effect, and neither for a tool.

| Kind | What it does | Language | What you write | Template |
| --- | --- | --- | --- | --- |
| machine | A new sound source. It renders one voice. | C for the declaration, SHARC+ assembly for the voice | `init` in `src/module.c`; `render`, `note_on`, `desc` in assembly | `sdk/templates/machine` |
| effect | An effect on the master block or on a send bus. It has its own FX page. | C for the declaration, SHARC+ assembly for the effect | `init` in `src/module.c`; `process`, `desc` in assembly | `sdk/templates/fx` |
| tool | ColdFire code. It adds commands, reads the store or reacts to the UI loop. | C | `dk_module` with `init`, `fini`, `tick`, `menu` | `sdk/templates/tool` |

The `core/*` modules are patched into the firmware. They are not content modules. They are described in [modules/README.md](../modules/README.md).
The author `core` is reserved for them. The author `test` is reserved for fixtures. Use your own author, or `digikit` for the project's modules.

## 2. Set up

Install once:

```sh
mise install                  # uv, pnpm, Rust and the other pinned tools
mise run setup                # uv sync and pnpm install
mise run patch:toolchains     # the ColdFire (m68k) compiler and the SHARC+ assembler, into out/tools
```

`patch:toolchains` takes about 15 minutes. It installs into `out/tools`, never globally.

To run a module in the emulator you also need your own stock firmware in `firmware/`: `Digitakt_II_OS1.17.syx` and `Digitone_II_OS1.12.syx`.
Never commit these files.

Check the setup after you create your first module (section 3):

```sh
mise run module:build -- modules/machines/NAME --check     # fails when the built object in out/cache/dko is stale
```

## 3. Start a module

```sh
mise run module:new -- KIND AUTHOR/NAME [--dir DIR] [--title T] [--short S] [--no-build]
```

| Part | Meaning |
| --- | --- |
| `KIND` | `machine`, `fx` or `tool`. |
| `AUTHOR/NAME` | The module id. The author has lowercase letters, digits and dashes. The name is lower snake case. `core` and `framework` are refused. |
| `--dir DIR` | Write `DIR/NAME` instead of the repository folder. |
| `--title T` | Display name, 1 to 23 characters. Default: the name in Title Case. |
| `--short S` | Short name, 3 to 6 characters of A-Z and 0-9. Default: the first 4 letters of the name in upper case. |
| `--no-build` | Write the files, but do not compile. |

Examples:

```sh
mise run module:new -- machine alice/drone
mise run module:new -- fx alice/crunch --title Crunch --short CRU
mise run module:new -- tool alice/hello --dir ~/my-modules
```

The command copies the template, replaces the names and builds it. You start from code that compiles.

| Kind | Folder | Files you get |
| --- | --- | --- |
| machine | `modules/machines/NAME` | `module.toml`, `README.md`, `src/module.c` (the declaration), `src/dsp/NAME.s` (a sine voice with one level parameter) |
| effect | `modules/fx/NAME` | `module.toml`, `README.md`, `src/module.c` (the declaration), `src/NAME.h` (the parameter words), `src/dsp/NAME.s` (a gain effect with one knob and a page) |
| tool | `modules/tools/NAME` | `module.toml`, `README.md`, `src/c/NAME.c` (`init` logs a line, `fini` is empty) |

The command ends with the next commands: `module:check`, `module:pack` and `dk:run -- AUTHOR/NAME --emu --seconds 10` (section 8).
`dk:run --emu` needs a running emulator: start `mise run workbench` in another shell first. To change a module and see it on the screen in a few
seconds, use `mise run dk:dev` (section 8).

For a folder outside the repository, list the parent folder in `PATCHKIT_MODULES` (several folders: separated by `:`). The module tasks find
the module through it. With `--dir`, `module:new` prints the variable you need.

```sh
export PATCHKIT_MODULES=~/my-modules
mise run module:check -- alice/hello
```

## 4. The module folder

```text
module.toml        the manifest
src/module.c       the declaration of the machine or effect: `init` with the builder calls
src/NAME.h         optional: parameter words shared by C and assembly
src/dsp/NAME.s     SHARC+ assembly (machine, effect)
src/c/NAME.c       ColdFire C (tool)
tests/             the module's own tests: test_*.py, *.test.ts, scenarios/*.toml (section 8)
README.md          for a player: what the module does, in one or two sentences
gallery/icon.svg   optional card icon
build/gen/         generated: module_ids.h and gallery.json (section 6)
```

Commit `build/gen/`. Do not commit built objects: a content module has no `build/*.dko.json` in its tree. `patchkit build` and `patchkit pack` keep
the objects in the cache `out/cache/dko/<module id>/<object path>`, keyed by the hash of the sources, and build what is stale. Only `core/*`
modules commit their objects (`mise run check:layers` fails when a content module has one). The browser has no compilers: the web build runs
`patchkit build` and puts the objects from the cache into the gallery bundle.

Do not edit anything in `build/gen/`. A `.dkmod` export leaves out `build/` (section 11).

## 5. The manifest and the declaration

`module.toml` holds what must be known before any module code runs. Everything else is declared by builder calls in `init` (sections 5.2 to
5.5). You write one declaration for both devices. The device profile (`devices/digitakt-ii.toml`, `devices/digitone-ii.toml`) fills in what is
specific to a device. [MODULES.md](MODULES.md) explains the split.

### 5.1 `module.toml`

`[module]`:

| Key | Meaning |
| --- | --- |
| `id` | `AUTHOR/NAME`. |
| `version` | `major.minor.patch`. Change it for every release. |
| `name` | Display name. |
| `category` | `machines`, `fx`, `ui`, `tools` or `test`. A group name for the folder and the gallery. |
| `type` | `machine`, `fx` or `service` (the template of a tool uses `service`). `module:check` uses it to choose the conformance suite. |
| `author` | The author part of the id. |
| `status` | `experimental` or `stable`. |
| `summary` | One sentence for players, 120 characters at most, ending with a full stop. |
| `dynamic` | `true` for a machine or an effect. The templates set it. |
| `ports` | Firmware versions the module names: `["dt2-1.17", "dn2-1.12"]`. |
| `requires`, `conflicts` | Other module ids. Empty for most content modules. |
| `site` | `false` keeps a developer tool off the public site. The desktop workbench still has it. Leave it out for a module meant for users. |

`[targets]` records the test status for each firmware: `untested`, `experimental` or `stable`.

`[[machine]]` and `[[fx]]` list what the module declares. One entry per machine or effect:

| Key | Meaning |
| --- | --- |
| `id` | Lowercase letters, digits, `-` and `_`. It is the identity of the machine or effect in saved projects and kits: never rename it after a release. |
| `dsp` | Optional. The prefix of the DSP entry symbols, when it is not the id. The symbols are `_<prefix>_render` and `_<prefix>_note_on` (machine), `_<prefix>_process` (effect), and an optional `_<prefix>_desc`. |

An id names the machine or effect only. The position of the entry in its list is its index: `patchkit gen` writes it to
`build/gen/module_ids.h` as `DK_MACHINE_ID_<ID>` or `DK_FX_ID_<ID>`, and `init` passes it to the builder call. Do not change the order after a
release. A tool has neither list.

`[[record]]` lists a record of a generic kind that `init` emits with `dk_record_add` (a kind that a service owns and that has no builder, such as `lfo_shape`): `kind` is the kind's lower-case name from `sdk/sdk.toml` `[records.kinds]` and `key` the identity of the record (the shape id of an `lfo_shape`). The packer writes the keys into the header table, so the boot planner refuses a kind no service owns and a key that another module of the selection has before the module loads. Machines and effects are builder calls, not `[[record]]`.

`[uses.dt2]` and `[uses.dn2]` hold the rows the module takes on each device: `machine_rows`, `param_rows`, `page_rows`, `record_bytes`, and the DSP cost
(`dsp_cost_voice` for a machine, `dsp_cost_fx` for an effect: instructions per block). `module:check` runs `init` in the emulator and the conformance
suite, and writes them. Never write them by hand. The device menu and the packer use them to tell whether the
module fits.

`[[object]]` names a built object:

| Key | Meaning |
| --- | --- |
| `path` | The built object, for example `build/NAME-dsp.dko.json`. |
| `sources` | The sources `module:build` compiles into it. All sources of one object are for one CPU. |
| `place` | `dsp.l2` for SHARC+ code, `cf.expansion` for ColdFire code. |
| `devices` | Optional: `["dt2"]`, `["dn2"]` or both. Default: both. |

`src/module.c` is an `[[object]]` with `place = "cf.expansion"`. The SDK sources a module uses are linked by the packer: do not list them.

A manifest with `[[voice]]`, `[[param]]` or `[[page]]` is refused: parameters and pages are builder calls.

### 5.2 Declaring a machine

A machine is declared in `init`. This is `modules/machines/analog/src/module.c`:

```c
#include <dk/core/module.h>
#include "module_ids.h"

static int32_t init(dk_module_ctx *ctx)
{
    dk_machine *m = dk_module_ctx_add_machine(ctx, DK_MACHINE_ID_ANALOG, "Analog", "ANA");
    dk_page *p = dk_machine_add_page(m, "Analog", "ANALOG");
    dk_page_add_param(p, "Detune", "DET", 40);
    dk_page_add_param(p, "Wave", "WAVE", 0);
    dk_page_add_param(p, "Sub", "SUB", 0);
    dk_page_add_param(p, "Fold", "FOLD", 0);
    return DK_OK;
}

const dk_module_def dk_module = {.init = init};
```

with this in `module.toml`:

```toml
[[machine]]
id = "analog"
dsp = "va"
```

A child is added by its parent: the context adds the machine, the machine adds a page, the page adds a parameter. Each call returns a handle for
the next level or for a setter. A call never fails on its own. On an error it returns a placeholder, and `init`'s result reports the first
error when the runtime commits the declaration. Do not check each return value.

`dk_module_ctx_add_machine(ctx, index, name, short_name)`:

| Argument | Meaning |
| --- | --- |
| `index` | `DK_MACHINE_ID_<ID>` from `module_ids.h`. |
| `name` | Display name, 23 characters at most. |
| `short_name` | Label in the machine list. |

A machine needs DSP routines. A machine with no DSP code (a data-only clone of a stock machine) cannot be declared. The runtime finds the routines
by name (section 5.1).

`dk_machine_add_page(m, name, short_name)` adds a page after the pages already added. `name` is the title. `short_name` is the tab label on a
device that keeps knobs in page rows. The first page of a machine is its knob page.

`dk_page_add_param(page, name, short_name, def)` adds a parameter on the next knob of the page:

| Argument | Meaning |
| --- | --- |
| `name` | Name shown while the knob turns, 31 characters at most. |
| `short_name` | Label under the knob, 15 characters at most. |
| `def` | Default in knob units. |

The order of the calls is the word number: the first parameter is word 0 of the DSP `params`. Words come in order, without gaps. The range is 0
to 127 knob units until `dk_param_range(param, min, max)` changes it. The stored word is the knob unit times 256, so 127 is the word `0x7f00`. The adapter clamps each word to its range before your code reads it.
The runtime does not check that `def` lies inside the range: keep it true yourself. It refuses a range that does not fit the glyph.
The kind of a parameter is a convention for the word:

| Kind | Word range | UI | Rest value | Use for |
| --- | --- | --- | --- | --- |
| percent | 0 .. 0x7f00 | 0 .. 127 | none, use `def` | amounts, times, mix, decay |
| bipolar | 0 .. 0x7f00 | -64 .. +63 | 0x4000 | detune, pan, tilt |
| enum | step x 256 | step number | step 0 | a switch or a mode with n steps |
| pitch | 0 .. 0x7f00 | semitone offset | 0x4000 | a pitch offset the adapter adds to `pitch` |

For an enum, set the range to 0 .. n-1 (the word is (n-1) x 256). A signed or named display is not available for a machine.

Setters change the parameter that a call returned:

| Call | Effect |
| --- | --- |
| `dk_param_range(prm, min, max)` | The range in knob units, 0 to 127. |
| `dk_param_glyph(prm, glyph)` | The look, the value text and the step of the knob. One glyph per parameter. The range must fit the glyph. Default: `DK_GLYPH_ENCODER`. |
| `dk_param_latched(prm)` | Snapshots the effective, clamped value at the actual `note_on`. `render` sees it until the next trigger; other words stay live. Snapshots are per adapter voice, including overlapping DN2 notes from the same source track. See [parameter views](design/param-views.md). |
| `dk_param_on_click(prm, fn, arg)` | A click on the knob (press and release, no turn between) calls `fn(arg, track)` in the UI task. `fn` may open a `<dk/platform/ui.h>` menu over the page. One click handler per parameter; a knob without one keeps the stock click. `arg` points at module memory, never at the stack. Details: [machine-page-click.md](design/machine-page-click.md). |

The glyph constants are in `<dk/gen/page_consts.h>`:

| Glyph | Range | Notes |
| --- | --- | --- |
| `DK_GLYPH_ENCODER` (default) | 0 .. 127 | plain encoder |
| `DK_GLYPH_FADER` | 0 .. 127 | fader |
| `DK_GLYPH_PAN` | 0 .. 127 | centre 64, shows L/R |
| `DK_GLYPH_ONOFF` | 0 .. 1 | on/off switch |
| `DK_GLYPH_TUNE` | 4 .. 124 | centre 64, one unit = one semitone |
| `DK_GLYPH_PLAY` | 0 .. 3 | REV, REV.L, FWD.L, FWD; the Digitone II has no PLAY row and shows 0 .. 3 |

Glyphs on effect parameters are not supported yet.

Scaling the word: the stock DSP converts a word to a float with the scale 1/32512 (0x7f00 = 1.0). Your routine may use any scale. Document
it in the source header, as `modules/machines/pluck/src/dsp/pluck.s` does.

Put the word numbers in a header in the module's `src/` when C and assembly both need them (`shot_params.h` of SHOT). Use only decimal
`#define`s and `//` comments on their own line: the SHARC+ assembler keeps a trailing comment in the macro text and upper-cases macro text.

### 5.3 What the device profile fills in

You declare words. The profile (`[modules.knobs]`, `[adapter.machine]` and `[modules.frame]` in `devices/<device>.toml`) places them on
knobs. `clone_of` is fixed by the profile (`[modules.knobs] clone_of = 0`, the SAMPLE machine). You cannot set it.

| Item | Digitakt II | Digitone II |
| --- | --- | --- |
| Where knobs live | Fields of the machine row. Engine slots 0 to 7. | Columns of a page. Sound-array slots 25 to 32. |
| Fixed knobs | TUNE on knob 0. Sample slot on knob 3. LEV on knob 7. | None. |
| Knobs for your words | 1, 2, 4, 5, 6, 7 (letters B C E F G H; `param_slots`). Word w goes to the w-th of them. 6 words. A word on knob 7 replaces LEV: the track's level is fixed at 100. | 1, 2, 4, 5, 6, 0, 3, 7 (B C E F G A D H). 8 words. |
| Page rows | None. The machine has its SRC page. | One per page added. |

Rules that follow:

- A machine has at most as many parameters as the device has `param_slots` (Digitakt II 6, Digitone II 8), and the words are in order.
- A parameter's position is its slot. Projects store values by slot. After a release, do not move a parameter to another word.
  Removing a parameter is allowed.
- The runtime assigns the machine's type number at load. Never write a number into code or a test.

### 5.4 Declaring an effect

An effect is declared in `init` too. This is `modules/fx/freqshift/src/module.c`:

```c
#include <dk/core/module.h>
#include <dk/gen/page_consts.h>
#include "module_ids.h"

static int32_t init(dk_module_ctx *ctx)
{
    dk_fx *fx = dk_module_ctx_add_fx(ctx, DK_FX_ID_FSHIFT, "Freq Shifter", "FSH");
    dk_fx_routes(fx, DK_ROUTE_SEND | DK_ROUTE_MASTER, DK_ROUTE_SEND);
    dk_page *p = dk_fx_add_page(fx, "Freq Shifter", "FSH");
    dk_page_add_fx_param(p, "Freq Shift", "SHIFT", DK_BIPOLAR, -64.0f, 63.0f, 0.0f);
    return DK_OK;
}

const dk_module_def dk_module = {.init = init};
```

with this in `module.toml`:

```toml
[[fx]]
id = "fshift"
dsp = "fs"
```

With the module id, the effect `id` makes the effect ID of saved kits. Never rename either.

| Call | Meaning |
| --- | --- |
| `dk_module_ctx_add_fx(ctx, index, name, short_name)` | Declares the effect. `index` is `DK_FX_ID_<ID>`. |
| `dk_fx_routes(fx, allowed, def)` | The routes the user may choose in SETTINGS > DIGIKIT > FX ROUTES (`DK_ROUTE_MASTER`, `DK_ROUTE_SEND`, or-ed) and the route at the start. Without this call: master only. |
| `dk_fx_mix(fx, def)` | The default of the framework's MIX knob, 0 to 127. Default: 127. |
| `dk_fx_add_page(fx, name, short_name)` | The effect's page. `name` is the title, 1 to 23 characters (mixed case: "Freq Shifter"). `short_name` is the tab label, 1 to 3 characters. An effect with a page has its own page after CHO in FUNC + FX. A send route needs a page. |
| `dk_page_add_fx_param(page, name, short_name, kind, min, max, def)` | A float parameter on the next knob of the page. |

`dk_page_add_fx_param` arguments:

| Argument | Meaning |
| --- | --- |
| `name` | Name shown while the knob turns, 1 to 23 characters. |
| `short_name` | Label under the knob, 1 to 6 characters. |
| `kind` | `DK_PERCENT` (0 to 127) or `DK_BIPOLAR` (-64 to +63). The display only. |
| `min`, `max` | The floats at the bottom and the top of the knob. |
| `def` | Inside `min` .. `max`. It is also the word's value at load. |

An effect may have up to 7 knobs. The parameter order is the word order: the first is word p0. Knob placement is the same on both devices. The
device profile does not change it. The runtime checks lengths, ranges, routes and the effect id at commit and refuses the module on a failure.

MIX, the route and the send amounts belong to the framework. Never declare them as parameters. The effect writes the wet signal only.

Put the word numbers in a header in the module's `src/` (the effect template has `src/NAME.h`). The C file and the SHARC+ assembly both include
it, because the build searches `src/` for both (same rules for `#define` as in section 5.2).

### 5.5 Canvas pages

A machine page may be drawn by the module instead of the stock knob display. SHOT is the example (`modules/machines/shot/src/module.c`):

```c
dk_page *controls = dk_machine_add_page(m, "SHOT", "SHT");
dk_page_canvas(controls, DK_PAGE_STOCK_OVERLAY, controls_draw, range_event, 0);
dk_page_bind(controls, pitch, 0);
dk_page_bind(controls, smp, 3);

dk_page *wave = dk_machine_add_page(m, "SHOT WAVE", "SHT");
dk_page_canvas(wave, DK_PAGE_WIDE_CANVAS, range_draw, range_event, 0);
dk_page_bind(wave, start, 4);
dk_page_bind_local(wave, 0, 6);
```

SHOT has a knob page with five parameters (and `dk_param_range`, `dk_param_glyph`, `dk_param_latched`, `dk_param_on_click` on them) and two canvas
pages. A machine has one knob page and may add canvas pages.

| Call | Meaning |
| --- | --- |
| `dk_page_canvas(page, flags, draw, event, arg)` | Makes the page a canvas page. `draw` and `event` run in the UI task with `arg`. `flags`: 0, `DK_PAGE_STOCK_OVERLAY` or `DK_PAGE_WIDE_CANVAS` (in `<dk/page/page.h>`). |
| `dk_page_bind(page, prm, slot)` | Binds a parameter of the machine to the encoder `slot` (0 to 7). Turns edit the parameter as on a knob page. |
| `dk_page_bind_local(page, local, slot)` | Binds a control that exists only on this page. Its turns arrive in `event`. |

The draw callback is `void draw(void *arg, const dk_page_frame *frame)`. The event callback is `void event(void *arg, const dk_page_event *event)`.
Both structs are valid for the call only. `frame->canvas` is the drawing surface.

- `DK_PAGE_STOCK_OVERLAY` (the first canvas page of the machine only): the stock widgets draw first, then the callback on the same 99 x 53 canvas (global x25..123).
  Clear the rectangles you want to draw in with `fill_rect(..., 0)` before text or glyphs; only filled rectangles are composited. The LEVEL
  fader and the page dots stay.
- `DK_PAGE_WIDE_CANVAS` (not on the first canvas page, not with the overlay): a canvas of 107 x 53 pixels (global x17..123). The track badge and the page
  dots stay outside it.
- No flag: the 99 x 53 canvas.
- LEVEL stays the physical control and needs no binding. `frame->level` gives its value for display.

`<dk/page/widgets.h>` has code-drawn helpers: `dk_page_encoder` (17 x 17), `dk_page_level` (9 x 17), `dk_page_tune` (22 x 17, signed eighths of a
semitone) and `dk_page_sample` (22 x 17, a sample slot state). Coordinates are top-left canvas pixels. Drawing clips to the canvas. They do not
clear their background and never edit a parameter. `<dk/sample/view.h>` draws a waveform with its handles (`dk_sample_view_rect_q8`).
Draw order, clipping and explicit opaque rectangles are the contract. Firmware calls and bitmap layout are internal to the adapter. See
[native machine pages](design/native-machine-pages.md).

### 5.6 A tool

A tool has no `[[machine]]` or `[[fx]]`, no DSP and no declaration. It has `[module]`, `[targets]` and one `[[object]]` with `place =
"cf.expansion"` and a C source. Its entry point is `dk_module` with `init`, `fini`, `tick` and `menu` (section 7.3). The template is
`sdk/templates/tool`.

### 5.7 Persistence and limits

The runtime refuses a declaration that breaks a rule above: more machine parameters than the device's `param_slots`, a glyph whose range does
not fit, a machine without DSP symbols, a send route without a page, a duplicate effect id, an index outside the id list, or counts above
`[uses]`. The module is not loaded and nothing is written. The result is a status code such as `E_DECL_ID`, `E_DECL_USES` or `E_DECL_FULL`
(`[load.status]` in `sdk/sdk.toml`). A `def` outside its range is not refused.

Layout facts:

- Digitakt II: the machine row holds the knobs as `f0..f7` and the level as `f8`. A page shows 8 of the 10 engine slots ([finding 100](findings/100-module-limits.md)).
- Digitone II: a stock page holds eight knobs and the track level. Each page of a machine takes a page row.
- Limits per module: 16 parameters, 8 machines, 8 pages, 32 records in a declaration, 6320 bytes of record data.

Persistence and modulation:

- Values persist on both devices. Projects store raw words by slot, so a released parameter must keep its word.
- If the module is absent at load, the track keeps its type and saves it back unchanged. Stock firmware loads the track as SAMPLE
  (Digitakt II) or FM TONE (Digitone II) and loses the type on re-save.
- p-lock on a module parameter: not shown. LFO as a destination: the MOD page lists the parameter names on the Digitakt II, but that an LFO moves
  the DSP value is not known. The clamp stays on, because an LFO can drive a word out of range.
- MIDI CC and NRPN are ignored for added parameters. Effect knobs cannot be locked or modulated either.

Patching a stock machine will become a separate module type. It is not part of the content modules.

### 5.8 Module settings

A run-time module keeps its own values with the settings imports of `<dk/platform/settings.h>`: `dk_settings_open(ctx, &settings)` (only in `init`, gives the handle),
`dk_settings_get`, `dk_settings_put` and `dk_settings_remove`.

- The runtime stores the key as `<module name>/<key>`. A module reaches its own keys only; the prefix stops mistakes, not hostile code.
- The module name and the key together are at most 47 printable ASCII characters (no space). A key may not be empty, hold `/` or hold `..`: such a key
  returns `E_SET_KEY` and nothing is written.
- A value is 0 to 1024 bytes. All modules and the runtime share one 8 KiB record; a full record returns `E_SET_FULL`, an off settings store `E_SET_OFF`.
- Writes reach the card 2 s after the last change, and before a restart.
- `get` works in `init` and in the UI task. `put` and `remove` are for the UI task only (`init`, `tick` or a menu row callback): not from a host command handler.
- The module owns the layout and the version of its values: write a version byte first and read older values or reset them. The runtime never interprets them.
- The settings popup is the module's `menu` function with the `dk_ui_menu_add_*` rows: a row changes a value and its callback calls `dk_settings_put`; `init` reads it at the next boot.

### 5.9 Module files and `dk_intro_take`

A module's own files live under `data/<module id>/` in the store, not next to the `.dkm`: the `modules/` list holds at most 32 entries. `dk_data_path` (`<dk/platform/data.h>`)
builds the path and refuses a name that is empty or holds `/` or `..`. Read a file with `dk_files_read` in `init`, or list the folder with the `dk_files_*` calls (root
`DK_FILES_STORE`). Only the host writes these files for now. `dk_files_read` runs one at a time and in `init` only.

`dk_intro_take` (`<dk/platform/intro.h>`) replaces the boot animation. It runs after the REC, safe-mode and boot-counter checks and does no storage I/O: read the frames
in `init`, then call it.

- `frame(tick, ctx)` returns the 1024-byte native bitmap of tick 0 .. `ticks`-1 (30 a second), or NULL to end the animation. It runs on the intro task (priority 7, more urgent than the main-OS task, priority 6): it must return quickly, because it blocks every lower-priority task, and a callback that never returns starves the main OS until power-off. It must not block and
  must not call any import. The last frame stays on the screen.
- `done(ctx)` (may be NULL) runs once in the UI task at the first UI pass. Free the buffer there, never in `frame`.
- `ticks` is 1 to 300. Errors: `E_NOT_FOUND` (outside `init`), `E_CONTRACT` (no frame, bad ticks), `E_BUSY` (taken already), `E_BOOT_ONLY` (the intro is over). After an error the stock intro runs.
- A fault in `frame` reaches the firmware's handler. The boot counter stays raised, so repeated failed boots end in safe mode.
- In a safe boot no module loads, so `dk_intro_take` is never called. The runtime itself replaces the stock intro with a fixed safe-mode screen for 4 s: `SAFE MODE`, the reason (`REC HELD`, `2 FAILED BOOTS`, `SAFE ONCE`, `DSP FAULT`, `CRASH` or `SELECTION UNREADABLE`) and `NO MODULES LOADED`.

See `modules/tools/bootscreen`.


## 6. Generated files

`patchkit gen` writes two files in `build/gen/` of a module. Do not edit them.

- `module_ids.h`: `DK_MACHINE_ID_<ID>` and `DK_FX_ID_<ID>`, the index of each id in the `[[machine]]` and `[[fx]]` lists of `module.toml`. A typo in
  the name is a compile error. `gen --check` fails when the file is stale.
- `gallery.json`: the machines with their knobs and parameters, for each device, for the web gallery. `module:check` writes it from what `init`
  declared in the emulator. It holds no firmware bytes.

The packer computes the keys of the machines and effects from the packed name and the ids. They are not in your source.

The SDK headers are generated from `sdk/sdk.toml`. Modules include the public headers in `sdk/include/dk/`.
The generated files are in `sdk/include/dk/gen/`, `sdk/dsp/contract/` and `sdk/include/dk/core/status.h`.

| File | What it holds | Who includes it |
| --- | --- | --- |
| `<dk/core/module.h>` | `dk_module_def`, the constant `dk_module`, `dk_module_ctx` and the prototypes of every import, including the builder calls (generated `<dk/gen/imports.h>`). | Every module in C. |
| `<dk/core/bytes.h>` | `dk_bytes_put32`, `dk_bytes_get32`: big-endian 32-bit fields of protocols and files. Header only. | Modules that read or write wire fields. |
| `<dk/core/text.h>` | Bounded text without a C library: `dk_text_len`, `dk_text_copy`, `dk_text_equal`, `dk_text_append`, `dk_text_cat`, `dk_text_utoa`, `dk_text_itoa`, `dk_text_trunc`, `dk_text_size`, `dk_text_secs`. Every writer takes the buffer size and ends with a NUL. The packer links the source (`sdk/src/core/text.c`) when a module calls one. | Modules that build row text or names. |
| `<dk/core/path.h>` | `dk_path_join` (refuses over 255 bytes), `dk_path_parent`, `dk_path_leaf`. The packer links `sdk/src/core/path.c`. | Modules that handle file paths. |
| `<dk/gen/machine.h>` | `dk_voice_in`, `dk_machine_desc` and the constants. | The machine's DSP data and C helpers. |
| `<dk/gen/fx.h>` | `dk_fx_in`, `dk_fx_desc` and the constants. | The effect's DSP data and C helpers. |
| `<dk/page/page.h>` | `dk_page_frame`, `dk_page_event`, the canvas and the page flags of a canvas page. | Modules with a canvas page. |
| `<dk/gen/page_consts.h>` | The knob glyphs (`DK_GLYPH_*`), the display of an effect parameter (`DK_PERCENT`, `DK_BIPOLAR`) and the routes (`DK_ROUTE_*`). | Modules with a glyph or an effect. |
| `sdk/dsp/contract/dk_machine.inc` | The machine constants and field offsets for assembly. | The machine's `.s`. |
| `sdk/dsp/contract/dk_fx.inc` | The effect constants and field offsets for assembly. | The effect's `.s`. |
| `<dk/core/status.h>` | The status codes `DK_E_*` (`[load.status]` in `sdk/sdk.toml`): the results of loading and of module calls. | Modules that check a status. |
| `sdk/include/dk/gen/sample.h` | The word layout of one sample pool entry, `DK_SAMPLE_POOL_*` (`[sample]`). | `<dk/sample/pool.h>`. |
| `sdk/dsp/contract/dk_sample_pool.inc` | The same offsets for assembly. | `sdk/dsp/sample/pool.inc`. |

Do not include `dk_fx.inc` and `dk_machine.inc` in one assembly file. `DK_BLOCK` and `DK_LEVEL_BOUND_BITS` are in both.

To write the generated files again:

```sh
mise run gen:modules                                     # all modules and the SDK
mise run gen:modules -- --check                          # list stale files, write nothing
uv run python -m tools.patchkit.patchkit gen --modules AUTHOR/NAME    # one module
```

Run `gen:modules` after you change `module.toml`, `src/module.c`, `devices/*.toml` or `sdk/sdk.toml`. Generation reads existing cached content
objects and never builds them. For a new module with no cached objects, generation skips its gallery until the first build; run it again
afterward.

## 7. Contracts

The headers are the reference. This section lists what you need to know before you read them. The machine and effect contracts are the DSP side
of a module: the routines that the runtime calls.

### 7.1 Machine voice (`<dk/gen/machine.h>`)

| Entry | When it runs | Result |
| --- | --- | --- |
| `note_on(state, in, params)` | Once per note, before the first `render`. It makes no sound. | `0`, or a negative number (a fault). |
| `render(state, in, params)` | Once per block of `DK_BLOCK` (32) samples while the voice runs. | `DK_OK`, or `DK_DONE` when the voice has ended. |
| `desc` | A `dk_machine_desc` in read-only data: magic, contract word, parameter count, voices, state bytes, bytes cleared at note-on, call depth and stack bytes. The DSP cost is not in it: `module:check` measures it. | Read by the adapter. |

`render` writes `DK_BLOCK` floats to `in->out`. `in` also holds `pitch` (semitones shifted left by 8, the tune included), `velocity`, `block` and the source `track`.
`params` holds `DK_MAX_PARAMS` words. The words after your parameters are 0.

The adapter enters your routine with `I13` = return address and `JUMP (M13, I12)`. The arguments are `R4` = state, `R8` = `dk_voice_in`,
`R12` = `params`. The result is in `R0`. In `dk_voice_in` the words are: 0 `pitch`, 1 `velocity` (0 to 127, shifted left by 8), 2 `block`
(blocks since the note-on), 3 `out` (pointer to 32 floats), 4 `track` (0 to 15, latched at note-on).
`track` identifies the source track. On the Digitone II it is independent of the physical voice slot and stays latched until retrigger. The
first trigger-edge block remains silent; the owner is latched on the following note-on block. An invalid owner ends only that slot without
calling the module.
Word 6 of `dk_voice_in` is `gate`: 1 from the note-on until the note is released, then 0 (the Digitone II has no release yet and gives 1). A
machine that reads `gate` sets `DK_MACHINE_GATE` in the contract word of its `dk_machine_desc`. `render` also runs on the note-on block, after
`note_on`.

Stereo machines. A machine with two output channels sets `DK_MACHINE_STEREO` in the contract word of its `dk_machine_desc`, writes `DK_BLOCK`
floats of the right channel to `in->out_r` (word 5 of `dk_voice_in`) and returns `DK_STEREO` or-ed into the status (`DK_OK | DK_STEREO`,
`DK_DONE | DK_STEREO`) on every block. A mono machine never touches `out_r` and the adapter copies L to R. On the Digitakt II the adapter writes
R into the odd voice of the stock voice pair (the stock engine pans the two buffers independently); the guard checks both blocks. On the
Digitone II `out_r` is a per-slot R buffer. The adapter checks R, scales it as the engine scales L, and runs each stock stage between the
machine and the pan a second time on R ([DN2 stereo](design/dn2-stereo.md)). At most 8 slots play stereo in a block. The other slots play R = L.
A stereo slot costs about 2,900 to 5,500 more DSP instructions per block than a mono slot (design section 3). `digikit/shot` is the example.
Do not place `note_on` at offset 0 of the image: an offset of 0 reads as "no note_on".

The `dk_machine_desc` has 8 words: magic, contract, parameter count, voices (1 to 16), state bytes per voice (a multiple of 4), bytes cleared at
note-on, max call depth, stack bytes. It carries no cycle count: `module:check` measures the worst steady block of one voice and writes it as
`dsp_cost_voice` into `[uses.<device>]`. The broker accepts a selection when `max(per-voice costs) x voices + sum(effect costs)` stays within the device's headroom.

The adapter in `core/dsp-voice` zeroes `state_clear_bytes` at note-on (the first note-on of a track after the module loads zeroes its whole state: voice states start zeroed; unload and load again zeroes again) and clamps the parameter words to your ranges. After each call it checks the
output. A block with a sample that is not finite or is louder than 8.0 is zeroed. The voice ends. After 8 faults in a second the machine is
switched off.

Registers:

- Which registers you may change, keep or never write: `sdk/dsp/REGISTERS.md`.
- The adapter pins M5 = 0, M6 = 1, M7 = -1, M13 = 0, M14 = 1 and M15 = -1, L0-L5 = 0, SIMD off and RND32 set. Return with `JUMP (M13, I13)`.
- `_dv_exp2` (F0 = 2^F0) is a helper of `core/dsp-voice`. Declare it with `.EXTERN _dv_exp2;`. It clobbers R0 to R3.
- A 16-bit form `Rn = Rn op Rx` can be read differently by the emulator and the silicon. The build refuses it. Use three distinct registers.
  Other assembler traps: `.agents/skills/digikit-module/references/selas-pitfalls.md`.

Memory: your code may write `in->out`, its own voice's state, its data stack (`stack_bytes`) and the data and bss of its own object. Data and bss
are shared by all voices. State that differs per voice goes in `state`. The conformance suite records every store and fails on any other.

The template `src/dsp/NAME.s` shows all of this in about 90 lines. Read it first. Pitch checks: `tests/fixtures/modules/sine/tests/rig/sine.toml` (docs/TESTING.md "Sound checks").


### 7.2 Effect (`<dk/gen/fx.h>`)

| Entry | When it runs | Result |
| --- | --- | --- |
| `process(state, in, params)` | Once per block while the effect is on. | `DK_FX_OK` (0), or a negative number. A negative number is a fault: the block plays dry. |
| `desc` | A `dk_fx_desc` in read-only data: magic, contract word, `n_params`, `state_bytes`, call depth, stack bytes (6 words). | Read by the loader. |

- `in->in` is the input block: `DK_FX_BLOCK` (64) floats, planar. 32 left, then 32 right. It is read only.
- `in->out` is the output block of 64 floats. Write all of them.
- `params` holds `DK_FX_MAX_PARAMS` (8) words. Knob i is word p_i and arrives as a float. A page has at most 7 knobs, so p7 is free for a constant.
  `n_params` in the `desc` must cover your knobs.
- The runtime reserves `state_bytes` and passes the address in R4. It zeroes the state when the effect switches on.
- The registers and the entry convention are the same as for a machine.
- A fault, a sample that is not finite or a sample louder than 8.0 makes the block play dry. 8 faults in 1500 blocks switch the effect off.
- MIX, the route and the send amounts belong to the framework. Do not assume you are last in the chain. Write the wet signal only.
- Limits: `fx_slots` effects at a time (3 on both devices). `send_slots` of them on a send bus (2 on the Digitakt II, 1 on the Digitone II). More
  effects stay inserts.

- Arguments: `R4` = state, `R8` = `dk_fx_in` (words: 0 `in`, 1 `out`, 2 `block`), `R12` = `params`. The result is in `R0`.
- Switching on or off is a linear crossfade over one block that the adapter makes. While the effect is off it is not called, so a tail (reverb,
  delay) freezes.
- Data stack: `stack_bytes` below `I7`. Write nothing outside your own data. Include `dk_fx.inc`, not together with `dk_machine.inc`.
- The runtime reserves the state behind your image. There is no state symbol. A page needs no ColdFire code beyond `init`.
- Values arrive in `params` as the float `min + (max - min) * raw / 0x7f00`, about 3 ms after a turn. At load every word is its knob's `def`.
  Choose the range so that a turn is audible.
- The adapter makes `master = dry + mix * (wet - dry)` per sample, with mix = raw / 127. MIX 127 is exactly the wet signal and 0 is exactly the
  dry one. It costs about 7 DSP instructions a block at mix 1.0 and about 400 at another mix.
- Kits keep the knob values and MIX of each loaded effect, with an effect ID made from the module id and the effect `id`. Load Kit gives an
  effect its stored values only when the stored ID for its row is its own. Otherwise the effect gets its declared defaults. Effects count by row,
  so the same effects loaded in another order get defaults. A kit saved with effects also loads on stock firmware.
- Not supported: p-locks, LFO and other modulation on a knob, MIDI CC, a second page for one effect, and a knob that does not reach the DSP as a
  float. An image without `core/fxpages` refuses a module with a page (`E_NO_REGISTRY`). The Digitone II image needs `core/dynfx` and
  `core/fxpages` in its module list (they are in its `device` patch set).
- Text is printable ASCII. Write titles in mixed case. The firmware font draws a capital Q like an O.
- Routes: a send effect gets the sum of the tracks' send amounts as its input block. Its wet output times MIX is added to the FX return. The
  user chooses the route in SETTINGS > DIGIKIT > FX ROUTES (the setting `fx/route/<module id>`). A stored route that the effect no longer allows is
  ignored. Pass `DK_ROUTE_MASTER` only for a gain or limiter or any effect with a dry path of its own, `DK_ROUTE_SEND` only for a pure return such as a reverb, and
  both for the rest. The send amounts live in the settings store and the DSP row, never in your state or in a kit file. FUNC + FX lists DEL, REV,
  CHO, the send effects, then the master effects. A changed route takes effect after a restart. The runtime refuses an unknown route, empty
  `allowed` routes, a default route that `allowed` does not list, and any send route without a page (`dk_fx_routes`).
- A centred knob: `DK_BIPOLAR`, `min = -64`, `max = 63`, `def = 0`. A mapping with `2^x - 1` and no constant term gives exactly 0.
  A send effect with MIX default 127 changes nothing until a track has a send amount. On the master the same default means fully wet, so say in the
  README what it means there.
- Clamp the output to +-7.9. An input at the level bound times a filter's overshoot would otherwise refuse the block. Use an integer phase
  accumulator for anything that must not drift.
- Fixtures: `dyn-fx-gain` (insert), `dyn-fx-gain-send`, `-both`, `-sendonly`, `-bipolar` ([fixture table](TESTING.md#fixture-table)). A real effect with a
  float32 reference model: `modules/fx/freqshift`.
- The instruction budget of the conformance suite is `patchkit budget` (about 31,000 for the effect today, 900 for the adapter). Read it
  from the report in `dist/conformance-rig`. The worst call it measures is written as `dsp_cost_fx`.


### 7.3 Entry point and services (`<dk/core/module.h>`)

#### The entry point

Every module defines one constant, `dk_module` (`<dk/core/module.h>`). `patchkit pack` finds it by its symbol and writes its four function
pointers into the `.dkm` header. All four are optional. A machine or effect module uses only `init`, to declare its content (sections 5.2 to 5.5).
See [MODULES.md](MODULES.md) for why the entry is one constant.

```c
#include <dk/core/module.h>
#include <dk/core/log.h>
#include <dk/platform/settings.h>

static dk_settings *settings;

static int32_t init(dk_module_ctx *ctx)
{
    int32_t r = dk_settings_open(ctx, &settings);
    if (r != DK_OK)
        return r;
    dk_log(DK_LOG_INFO, "ready", 0);
    return DK_OK;
}

static void tick(dk_module_ctx *ctx) { (void)ctx; }

const dk_module_def dk_module = {.init = init, .tick = tick};
```

| Field | When it runs |
| --- | --- |
| `int32_t init(dk_module_ctx *ctx)` | Once after load. Opens the services, registers host commands. `DK_OK` (0), or the load fails and the declaration is discarded. |
| `void fini(dk_module_ctx *ctx)` | When a load fails after `init`, and at restart. Modules are not unloaded at run time. |
| `void tick(dk_module_ctx *ctx)` | Once per UI pass (about 120 Hz), for every loaded module that has one. Keep it short; no waiting. |
| `void menu(dk_module_ctx *ctx, dk_ui_menu *menu)` | Optional: the module's settings. The DIGIKIT menu shows SETTINGS... in the side panel of the loaded module and calls it when the user opens it; it adds the rows (`dk_ui_menu_add_text`, `dk_ui_menu_add_action`, ...). The menu is built once and kept, and no module is unloaded at run time, so the `arg` and callbacks of a menu must point at the module's own memory, never at the stack. |

The type is `dk_module_def`; the constant keeps the name `dk_module`, which the packer looks up. `ctx` is only the module context; the user-data pointer of a callback is `arg`. The device is reached through named imports grouped by service
(`<dk/gen/imports.h>`, generated from `sdk/sdk.toml`; the comment of each prototype gives its context and the runtime version that added it):

- Without a handle: `dk_alloc`, `dk_free`, `dk_log`, `dk_log_event`, `dk_time_blocks`, `dk_host_register`, `dk_module_ctx_device`,
  `dk_module_ctx_runtime_version`, `dk_hash32_*`, `dk_io_*`, `dk_job_*`, `dk_intro_take`, `dk_param_get/set`, `dk_debug_*`.
- With a handle opened in `init`: `dk_settings_open`, `dk_files_open` (one handle for listing, `dk_files_read` and for `dk_files_create`, `_write`, `_close`, `_abort` and `_delete`), `dk_sample_pool_open`, `dk_ui_open`, `dk_input_open`,
  `dk_usb_bulk_open`, `dk_dsp_link_open`. The service functions take the handle first (`dk_settings_get(s, key, out, cap)`).
- A service that another module provides (`core/ui`, `core/input`, `core/usb-bulk`, `core/dsp-link`) may be absent. Its `_open` returns
  `DK_E_MISSING` and leaves the handle NULL; the module loads and decides whether it can run without. The other calls of an absent service
  return `DK_E_MISSING` or do nothing. A module that cannot run without the service returns the code from `init`: the load fails with `E_IMPORT` and
  DIGIKIT shows the provider as the module's need (as for an import that no module in the image provides). A module that tolerates its absence returns `DK_OK` and records nothing.
- `patchkit pack` writes the newest `since` of the imports a module links as the runtime version it needs and prints the services it needs
  (`services <file>: usb_bulk (dk_usb_bulk_open); runtime (...)`). It refuses an object that has no `dk_module`.
- A `.dkm` that needs another runtime major or minor than the loader's, or a higher patch, is refused with `E_CONTRACT`: build it again against the SDK.

The tool template and `tests/fixtures/modules/entry-demo` show the entry point. The crash record names what the module code was doing (`init`, `tick`, `fini` or a callback) when the CPU faulted: `dklog crash` prints it.

A stereo or mono sample pool ([design](design/sample-api-1.5.md)) uses `<dk/sample/pool.h>`. The module names its table in the DSP data (`_<id>_sample_rec`, `_<id>_sample_ctl`): 16 `dk_sample_pool_entry` records (512 bytes), one for each
pool entry, and 16 control sets (256 bytes) in the DSP data (`.PKDATA`, zero, no relocations in them). `pack` writes them as the kind 2 record of
the DSP section. `dk_sample_pool_open(ctx, &pool)` gives the handle in `init`;
`dk_sample_pool_add(pool, path)` queues a PCM16 native file (mono or stereo, 48 kHz) from a host command handler or a UI callback and returns its pool index
0 to 15: the first free entry, or the entry that already holds the path (`DK_SAMPLE_E_POOL` when all 16 are in use). The path is the
root-relative path of a `DK_FILES_SAMPLES` listing row (`KICKS/K1`); the runtime adds the device root (DT2 `/`, DN2 `samples/`), and a DT2 path
that already starts with `/` stays as it is. `dk_sample_pool_set(pool, index, path)` loads a file into that entry and replaces what was there (the old file is released; a file another entry holds is shared). `dk_sample_pool_clear(pool, index)` leaves a hole: indexes are stable, nothing moves. A machine picks
the entry of a note in its DSP, usually from a parameter (SHOT: SMP, word 0, `word >> 8`); an entry that is not READY is silent.
`dk_sample_pool_path(pool, index, buf, cap)` gives the path back in the same form, in every state but UNUSED. The pool list is kept per
project. The runtime closes an entry's old generation before it publishes a new record.

A sampler machine can add the on-device sample picker with `<dk/sample/picker.h>`: include the header and call `dk_sample_picker_init(pool, files, ui)` in `init` (with the handles from `dk_sample_pool_open`, `dk_files_open` and `dk_ui_open`) and `dk_sample_picker_fini()` in
`fini`, and call `dk_sample_picker_open_pool(current, pick, arg)` from the click action of the SMP parameter. The picker shows the 16
slots; YES on a slot opens the file browser for that slot, and YES on a file loads it there (`dk_sample_pool_set`). `pick(arg, index)` runs after a file was loaded, and the machine sets its SMP value to `index`.
`modules/machines/shot` is the example (design: [sample picker](design/sample-picker.md)). Without `core/ui` the picker does nothing.

For reusable browsing, include `<dk/platform/files.h>` and open the `dk_files` handle in `init` (`dk_chooser_*` of `<dk/chooser/chooser.h>` is the ready-made list).
`dk_files_list(files, root, path, notify, arg)` copies a canonical root-relative NUL-terminated path and returns a positive request ticket;
empty path lists the root. `DK_FILES_SAMPLES` maps to DT2 +Drive or DN2 store `samples/`, and
`DK_FILES_STORE` lists the runtime store on either device. A new request supersedes the previous one.
The public path holds at most 255 bytes; provider limits are smaller (DT2 sample paths 254 bytes,
store paths 159 bytes, DN2 nonempty sample paths 151 bytes after the virtual prefix).

`dk_files_poll(files, ticket, out, capacity)` performs no storage I/O: it returns pending (0), a copied `dk_file_entry` (1), end (2),
or a negative error. Pass space for the whole 304-byte record; only return 1 changes the output.
Rows retain the full path and file/directory kind. Check validity flags before using size or hash.
Size is physical source bytes: DT2 includes the 16-byte trailer omitted by stock ReadDir.
Hashes are typed stored metadata: SHA-256 or DT2's odd 32-bit hash in the first four bytes, big endian.
Listing neither pins contents nor validates a sample; revalidate the selected source when loading it.

An optional callback receives `(client, ticket, state, arg)` in the UI task.
It is a deferred, coalesced readiness hint: poll the supplied ticket and tolerate a stale-ticket error.
Keep `arg` in module memory, never on the stack. Cancel or request another folder without
waiting for old work; late results are discarded. Fill/action callbacks should queue or poll, not read
storage. See the [listing design](design/sdk-files.md) and
the [public boundary fixture](../tests/fixtures/modules/files-proof/src/files-proof.c).

Names a module may import (from `[[import]]` in `sdk/sdk.toml`; every one is documented in `<dk/gen/imports.h>`):

| Name | Purpose |
| --- | --- |
| `dk_fw_alloc`, `dk_fw_free`, `dk_fw_block_read` | The firmware's allocator (a large buffer, 32 MiB pool) and eMMC block read (`<dk/core/mem.h>`). |
| `memcpy`, `memset`, `memcmp`, `__muldi3`, `__udivdi3`, `__umoddi3` | Runtime helpers that the compiler calls by itself. |
| `dk_input_*` | Press a key or turn an encoder (`dk_input_open` first). They need `core/input`; without it `dk_input_open` returns `DK_E_MISSING`. |
| `dk_ui_*` | The calls of `<dk/platform/ui.h>`: menus, lists, rows and dialogs. They need `core/ui`. Without it the module still loads, `dk_ui_open` returns `DK_E_MISSING` and each call returns its failure value (`dk_ui_menu_new` NULL, the int calls -1, `dk_ui_menu_is_open` and `dk_ui_menu_checked` 0, the void calls nothing). `dk_ui_tick(ui, fn, arg, period_ms)` is a timer in the UI task: redraw a screen while a load runs; 0 stops it. |
| `dk_time_blocks` | The 1500 Hz block clock. |
| `dk_sink_set` | Sets the sink of a runtime channel (`DK_SINK_LOG`, `DK_SINK_SPAN`) for this module; one owner per channel, cleared after `fini` (`<dk/core/sink.h>`). For debug and profiling modules. |
| `dk_dsp_link_*` | Sets one per-block callback in the block interrupt (`core/dsp-link`; `dk_dsp_link_open` returns `DK_E_MISSING` without it): `void cb(const volatile u16 *rec)` gets the previous DSP reply record. Short C only; clear it in `fini`. |

A load fails with `E_IMPORT` when a name is not in the runtime's table, or when `init` returns `DK_E_MISSING` because a provider the module needs is absent.

`dk_host_register(first, count, fn)` adds SysEx commands. Call it from `init`. The range is released after `fini`. The range must lie inside a range reserved in
`sdk/sdk.toml` under `[protocol.ranges]`. Three ranges exist today; `digikit/remote` uses the first two and the probe module (`digikit/probe`) the third:

| Range | Numbers |
| --- | --- |
| `remote` | `0x38` to `0x3d` |
| `remote_state` | `0x3e` to `0x3f` |
| `probe` | `0x32` to `0x35` |

Another range is a change to `sdk/sdk.toml`. Run `mise run gen:modules` after it. A handler runs in the firmware's MIDI task. Keep it short and never
wait. The request body is at most 160 bytes. The reply body is at most 879 bytes. Return the reply length, or a negative status.


## 8. Build, check, pack, run

| Task | Command | What it does |
| --- | --- | --- |
| Build | `mise run module:build -- PATH [--check]` | Compiles the sources to objects in `out/cache/dko/<id>/` (`patchkit build`). `PATH` is the module folder or the module id; no path builds every module. An object whose sources did not change is skipped. `--check` fails when the objects are stale; `--force` compiles anyway. `patchkit dko show FILE` prints what an object holds. |
| Check | `mise run module:check -- ID [--device dt2\|dn2]` | See below. |
| Memory audit | `mise run module:audit -- NAME [--device dt2\|dn2]` | Packs the module and the run-time modules it requires, loads them on a ColdFire-only emulator, drives the UI and reports ColdFire stores outside the module's blocks (pc, address, the code it belongs to). It runs the Vitest device test `modules/core/runtime/tests/memaudit-module.device.test.ts` through `tools/devtools/module_audit.sh`, which first builds the patchkit CLI and the daemons when they are stale. About 1 to 2 minutes per device. |
| Pack | `mise run module:pack -- ID --device dt2\|dn2 [-o FILE]` | Builds the objects that are stale and makes the `.dkm` from them in one command (no separate build step). Default: `out/patch/NAME-DEVICE.dkm`. It refuses a module that cannot be packed, for example one with no `dk_module`. |
| Pack all | the workbench app | Builds and packs the run-time modules of a device in-process when its library is read (the Transfer page, the memory panel, the default modules), into `out/modules/<device>/<id>.dkm`. |
| Emulator | `mise run workbench` | Starts the emulator. Run it in another shell. |
| Run | `mise run dk:run -- ID --emu` | Builds, packs, uploads, selects, restarts and follows the log. |
| Run on a device | `mise run dk:run -- ID --port Digitakt` | The same on a device that has the digikit runtime. The port is a part of the MIDI port name. |
| Iterate | `mise run dk:dev -- ID --device dn2 --keys 'tap:...,shot:NAME'` | Builds, reloads a kept emulator daemon and writes `out/runs/devreload/<dev>/sheet.png`. A UI-only change swaps in about 4 s; any other change boots (DN2 5 s, DT2 10 s). Iterate on DN2 and check DT2 at the end. It is no proof: finish with `module:check`. See [TESTING.md](TESTING.md). |
| Log | `mise run dk:log -- --emu follow` | The runtime log. See section 12. |
| Card | `mise run dk:store -- --emu info` | Lists the store. Prints one `capacity` line per resource. |

Every task takes `-- --help`. Hardware is a deliberate step: do not flash a device from these commands.

`module:check` runs the conformance suite on the SHARC-only rig, then one pytest process for the module's own tests. What it runs depends on the kind:

| Kind | What `module:check` runs |
| --- | --- |
| `machine` | The machine conformance of `packages/sharc-rig-kit` on both devices, or on one with `--device`. Every parameter at its limits, many notes, modulation, long notes and retriggers. The write-set check is part of it. |
| `fx` | The effect conformance of the kit: signals, parameter limits, modulation, switching and a long run. It checks returns, registers, the write set and the instruction budget. |
| `service` (tool) | No conformance suite. It checks the objects against their sources. |
| machine and effect | Also runs `init` in the emulator and writes `[uses.dt2]`, `[uses.dn2]` and `build/gen/gallery.json`. |
| all | The module's own tests: `tests/**/test_*.py` of the module, and the digikit test files of the kinds of its `tests/scenarios/*.toml`, limited to those scenarios. These may build or launch an emulator. |

A module owns its tests. They live in the module folder and nothing else lists them:

| File | Found by |
| --- | --- |
| `tests/test_*.py` (also in subfolders) | `module:check`, `gate`, `test:changed`. The basename must be unique in the repository (no `__init__.py`): start it with the module name, for example `test_hello_range.py`. A helper next to the test is imported by its name. The repository root and `tests/` are on `sys.path` (`tools.*`, `support.*`). Use `Path(__file__).parents[1]` for the module folder. |
| `tests/*.test.ts` | The Vitest project `module-tests` (`pnpm test:host`). |
| `tests/scenarios/*.toml` | The digikit scenario runner (`pk_digikit_scen list\|run NAME`) and the tiers, next to `tests/digikit/scenarios/`. Scenario names are unique across both places. |

A change in a module selects the module's own tests and those of the modules that depend on it. A change only in `tests/` runs the tests and no
build. A PCM pin of a module stays in that module's `tests/scenarios`. Nothing outside the module folder names a user module: `mise run
check:layers` (rule `self-contained`) fails on a module id or path in code or tests. Docs and `*.md` files may name modules. A module with
`host = true` in `[module]` is exempt: the workbench speaks its protocol (the remote control service). Tests of shared tools use the fixtures in
`tests/fixtures/modules` (`test/sine` is the small dynamic machine, `test/dyn-fx-gain` the effect; all fixtures are in the [fixture table](TESTING.md#fixture-table)), or a made-up id such as `acme/demo`.

After the tests pass, `module:check` packs the module for each device. It prints a `size` line and two more lines, `needs` and `fits`.

"Fits" means this: the load needs CPU memory, DSP memory, machine rows, voice rows, parameter rows and page rows. The `fits` line says whether an
**empty** device has enough. The limits come from `[limits]` in `devices/*.toml`.

| Limit | Digitakt II | Digitone II |
| --- | --- | --- |
| Modules at once | 8 | 8 |
| Module file | 64 KiB | 64 KiB |
| ColdFire memory, all modules | 512 KiB | 512 KiB |
| DSP area, all modules | 512 KiB | 320 KiB |
| Machine types and voice rows | 8 | 8 |
| Parameter rows | 16 | 16 |
| Page rows | none | 8 |
| Effects at once | 3 | 3 |

`fits an empty device` does not mean it fits next to your other modules. On a running emulator or device, `dk:store -- --emu info` shows what is
used and what is left. The runtime refuses a module that does not fit, with its own code, for example `E_NOMEM` or `E_DSP_NOSPACE`.

Device facts for authors are in `.agents/skills/digikit-module/references/devices.md`.

Trace marks (`DK_TRACE_LOG`, `DK_STEP`) are switched on with `--trace` on `module:pack`, `module:check` and `dk:dev`: see `sdk/README.md`, "Tracing".


## 9. What happens when a module loads

At boot the runtime gives each selected module a state: SELECTED, then PLANNED or REFUSED, then INIT, DSP LOADING and ACTIVE. FAILED and SKIPPED are the other ends. The boot plan reads only the header tables, so a REFUSED module never ran any code. A refusal and a failure each carry a load code and a reason that Transfer and the DIGIKIT menu show:

- `E_MISSING`, `E_CONTRACT`: a service the module needs is not there, or needs a newer runtime.
- `E_PLAN_FIT`: the module's `[uses]` do not fit what the modules before it in the selection left (the reason names the resource).
- `E_PLAN_KIND`, `E_PLAN_OWNER`, `E_PLAN_KEY`: a record kind has no owner, two modules own one kind, slot or hook site, or two modules give one key.
- `E_INIT`: `init` returned non-zero. `E_DECL_KIND`: the header table has record kinds, slots, hooks or exports that `init` did not declare. Both end FAILED.
- other codes (`E_NOMEM`, `E_DSP_*`): the load failed after the plan accepted the module.

In safe mode no module loads and every one is SKIPPED. The table of every code with the tests that prove it is `modules/core/runtime/tests/refusal/refusal-table.ts`.

## 10. Versions and compatibility

[MODULES.md](MODULES.md) explains the version rule. What you do:

- The runtime release in `sdk/sdk.toml` (`[runtime] version`, semantic; 0.x until 1.0.0, where a MINOR may break and a PATCH only adds) is the
  only SDK version. Each import has `since`, the runtime version that added it.
- `pack` writes the newest `since` among the imports a module links into the `.dkm` header (the runtime version the module needs).
- The runtime refuses a module when that version has another major or minor than the runtime's, or a higher patch. The error is
  `E_CONTRACT` (-48): rebuild the module.
- `[module] version` is yours. Change it for every release. The export file name carries it.
- A run-time machine has no fixed type number. The runtime assigns one at load. Never write the number into code or a test.
- The machine key and the effect ID of saved projects and kits come from the packed name and the `id` in `[[machine]]` or `[[fx]]`. Do not
  rename either after users have saved data. A pack with `--name` makes an alias with its own keys.

## 11. Export and import

```sh
mise run module:export -- AUTHOR/NAME                    # out/patch/AUTHOR-NAME-VERSION.dkmod
mise run module:import -- FILE.dkmod --dir ~/my-modules  # unpack into ~/my-modules
```

A `.dkmod` is a deterministic zip. It holds the module folder without `build/` and without OS junk files. Entries are sorted and share one time
stamp. A top-level `package.toml` lists id, version, author, type and the SHA-256 of every file. Two exports of one tree are byte-identical.
A `.dkmod` holds no SDK code: it builds against the SDK of the machine that imports it (the packer links the SDK components the module calls). Build it again after an SDK update.

`import` checks every hash. It refuses extra or missing files, the author `core`, the type `framework` and an id that is already known. `--replace`
allows a known id. Without `--dir` it uses the first folder of `PATCHKIT_MODULES`. After an import, run `module:check` before you trust the code.

## 12. Logging

- ColdFire code logs with `dk_log(level, msg, value)` (`<dk/core/log.h>`; 24 characters of the message are kept). `DK_TRACE_LOG(msg, value)` of `<dk/core/trace.h>` is `dk_log` at the trace level and exists only in a `--trace` build.
  Levels: 0 error, 1 warn, 2 info, 3 debug, 4 trace.
- A DSP routine has no log call. The runtime logs its faults (`MACHINE_FAULT`, `MACHINE_DISABLED`) and a stopped DSP.
- Each event carries its level and its source, the runtime or a module. With DEBUG off in the menu, only errors are kept.
- Read the log:

  ```sh
  mise run dk:log -- --port Digitakt follow --level L --module NAME     # use --emu for the emulator
  mise run dk:log -- --port Digitakt level                              # print the levels
  mise run dk:log -- --port Digitakt level debug                        # set the default level
  mise run dk:log -- --port Digitakt level --module NAME=trace          # one module; NAME=default clears it
  ```

  `dk:run` follows the log for you. Levels are not stored on the card. They reset at boot.
- Details: [DEVELOPMENT.md, "Module loop"](DEVELOPMENT.md#module-loop) and `docs/findings/46-digikit-debug.md`.


## 13. Looking things up

| Question | Where |
| --- | --- |
| What a module is, why it is built this way | [MODULES.md](MODULES.md) |
| Version numbers, imports, log kinds, protocol numbers | `sdk/sdk.toml` |
| Builder calls, headers per layer, tracing | [sdk/README.md](../sdk/README.md), `<dk/gen/imports.h>` |
| Machine, effect and tool contracts | `<dk/gen/machine.h>`, `<dk/gen/fx.h>`, `<dk/page/page.h>`, `<dk/core/module.h>` |
| What differs between the devices | `devices/digitakt-ii.toml`, `devices/digitone-ii.toml`, `.agents/skills/digikit-module/references/devices.md` |
| Knob and parameter rules | Sections 5.2, 5.3 and 5.7, `docs/findings/02-machines-and-parameters.md` |
| DSP recipes and assembler traps | `.agents/skills/digikit-module/references/dsp-recipes.md`, `selas-pitfalls.md` |
| What each reference module taught | `docs/findings/54-module-cookbook.md` |
| The `.dkm` and registry formats | [design/digikit-dkm.md](design/digikit-dkm.md), [design/digikit-registry.md](design/digikit-registry.md) |
| `core/*` modules, patching, anchors | [modules/README.md](../modules/README.md), [PATCHING.md](PATCHING.md) |
| The module loop and tests | [DEVELOPMENT.md](DEVELOPMENT.md), [TESTING.md](TESTING.md) |
| The same workflow for coding agents | `.agents/skills/digikit-module/SKILL.md` |

The firmware reference (`mise run fw:reference`, which writes `out/reference/fw.html`) is for `core/*` adapters. Content modules do not use
firmware declarations or anchors.
