# Writing a digikit module

This is the guide for content modules. A content module adds a machine, an effect or a service to a Digitakt II or a Digitone II. You build it
into a `.dkm` file. The digikit runtime loads the file from the card at boot. You do not patch a firmware image for it.

Run every command from the repository root. Nothing here touches hardware. Flashing is a separate step (see "Safety and hardware" in
[PATCHING.md](PATCHING.md)). No file in the repository contains firmware bytes.
The SDK (headers, includes, templates) is described in [sdk/README.md](../sdk/README.md).

## 1. What a module is

There are three kinds of content module. The kind is `[module] type` in `module.toml`.

| Kind | What it does | Language | Entry points | Template |
| --- | --- | --- | --- | --- |
| `machine` | A new sound source. It renders one voice. | SHARC+ assembly, declared in C | `render`, `note_on`, `desc` | `sdk/templates/machine` |
| `fx` | An effect on the master block or on a send bus. It has its own FX page. | SHARC+ assembly, declared in C | `process`, `desc` | `sdk/templates/fx` |
| `service` | ColdFire code. It adds commands, reads the store or reacts to the UI loop. | C | `dkm_init`, `dkm_fini`, `dkm_slot_N` | `sdk/templates/service` |

Two more types exist. `raw` is code with no contract and only the generic load checks. The menu shows it as unchecked. `framework` is
the internal type of the `core/*` modules.

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
| `KIND` | `machine`, `fx` or `service`. |
| `AUTHOR/NAME` | The module id. The author has lowercase letters, digits and dashes. The name is lower snake case. `core` and `framework` are refused. |
| `--dir DIR` | Write `DIR/NAME` instead of the repository folder. |
| `--title T` | Display name, 1 to 23 characters. Default: the name in Title Case. |
| `--short S` | Short name, 3 to 6 characters of A-Z and 0-9. Default: the first 4 letters of the name in upper case. |
| `--no-build` | Write the files, but do not compile. |

Examples:

```sh
mise run module:new -- machine alice/drone
mise run module:new -- fx alice/crunch --title Crunch --short CRU
mise run module:new -- service alice/hello --dir ~/my-modules
```

The command copies the template, replaces the names and builds it. You start from code that compiles.

| Kind | Folder | Files you get |
| --- | --- | --- |
| `machine` | `modules/machines/NAME` | `module.toml`, `README.md`, `src/module.c` (the declaration), `src/dsp/NAME.s` (a sine voice with one level parameter) |
| `fx` | `modules/fx/NAME` | `module.toml`, `README.md`, `src/module.c` (the declaration), `src/dsp/NAME.s` (a gain effect with one knob and a page) |
| `service` | `modules/tools/NAME` | `module.toml`, `README.md`, `src/c/NAME.c` (`dkm_init` logs a line, `dkm_fini` is empty) |

The command ends with the next commands: `module:check`, `module:pack` and `dk:run -- AUTHOR/NAME --emu --seconds 10` (section 8).
`dk:run --emu` needs a running emulator: start `mise run workbench` in another shell first.

For a folder outside the repository, list the parent folder in `PATCHKIT_MODULES` (several folders: separated by `:`). The module tasks find
the module through it. With `--dir`, `module:new` prints the variable you need.

```sh
export PATCHKIT_MODULES=~/my-modules
mise run module:check -- alice/hello
```

## 4. The module folder

```text
module.toml        the manifest
src/module.c       the declaration of the machine or effect (DK_MACHINE, DK_FX, ...)
src/dsp/NAME.s     SHARC+ assembly (machine, effect)
src/c/NAME.c       ColdFire C (service)
tests/             the module's own tests: test_*.py, *.test.ts, scenarios/*.toml (below)
README.md          for a player: what the module does, in one or two sentences
gallery/icon.svg   optional card icon
build/gen/         the generated gallery.json (section 6)
```

Commit `build/gen/`. Do not commit built objects: a content module has no `build/*.dko.json` in its tree. `patchkit build` and `patchkit pack` keep
the objects in the cache `out/cache/dko/<module id>/<object path>`, keyed by the hash of the sources, and build what is stale. Only `core/*`
modules commit their objects (`mise run check:layers` fails when a content module has one). The browser has no compilers: the web build runs
`patchkit build` and puts the objects from the cache into the gallery bundle.

Do not edit anything in `build/gen/`. A `.dkmod` export leaves out `build/` (section 10).

## 5. The manifest

`module.toml` has plain sections for every module. The machine or effect itself is declared in C (sections 5.2 and 5.4). The device profile
(`devices/digitakt-ii.toml`, `devices/digitone-ii.toml`) fills in what is specific to a device. You write one declaration for both devices.

### 5.1 Sections for every module

`[module]`:

| Key | Meaning |
| --- | --- |
| `id` | `AUTHOR/NAME`. |
| `version` | `major.minor.patch`. Change it for every release. |
| `name` | Display name. |
| `category` | `machines`, `fx`, `ui`, `tools` or `test`. A group name for the folder and the gallery. |
| `type` | `machine`, `fx`, `service` or `raw`. |
| `author` | The author part of the id. |
| `status` | `experimental` or `stable`. |
| `summary` | One sentence for players, 120 characters at most, ending with a full stop. |
| `dynamic` | `true` for a run-time `.dkm`. The machine and effect templates set it. |
| `ports` | Firmware versions the module names: `["dt2-1.17", "dn2-1.12"]`. |
| `requires`, `conflicts` | Other module ids. Empty for most content modules. |

`[targets]` records the test status for each firmware: `untested`, `experimental` or `stable`.

`[[object]]` names a built object:

| Key | Meaning |
| --- | --- |
| `path` | The built object, for example `build/NAME-dsp.dko.json`. |
| `sources` | The sources `module:build` compiles into it. All sources of one object are for one CPU. |
| `place` | `dsp.l2` for SHARC+ code, `cf.expansion` for ColdFire code. |
| `devices` | Optional: `["dt2"]`, `["dn2"]` or both. Default: both. |

The declaration file (`src/module.c`) is an `[[object]]` with `place = "cf.expansion"`. It holds only `.dk.desc` records. `patchkit pack` reads
them and leaves them out of the `.dkm`; the device never sees them.

A manifest with `[[voice]]`, `[[machine]]`, `[[param]]`, `[[page]]` or `[[fx]]` is refused.

### 5.2 Declaring a machine

A machine is declared in C, in `src/module.c`, with the macros of `<dk/machine.h>`. This is `modules/machines/pluck/src/module.c`:

```c
#include <dk/machine.h>

DK_MACHINE(pluck, .name = "Pluck", .short_name = "PLK", .render = "_pk_render",
           .note_on = "_pk_note_on", .desc = "_pk_desc");
DK_PARAM(pluck, 0, pluck_decay, "Decay", "DECAY", 0, 127, 64);
DK_PARAM(pluck, 1, pluck_pick, "Pick position", "PICK", 0, 127, 32);
DK_PARAM(pluck, 2, pluck_bright, "Brightness", "BRIGHT", 0, 127, 80);
DK_PARAM(pluck, 3, pluck_color, "Excitation colour", "COLOR", 0, 127, 40);
DK_PAGE(pluck, "PLUCK", "Pluck", DK_KNOB(0), DK_KNOB(1), DK_KNOB(2), DK_KNOB(3));
```

`DK_MACHINE(ID, ...)`: `ID` is a C identifier. It is the machine's name inside the module. The other arguments are field initializers:

| Field | Meaning |
| --- | --- |
| `.name` | Display name, 23 characters at most. |
| `.short_name` | Label in the machine list. |
| `.render`, `.note_on` | The global symbols of the DSP routines. Strings. Both are required. |
| `.desc` | The data symbol of the `dk_machine_desc` (`<dk/machine.h>`). String. |

A machine needs DSP routines. A machine with no DSP code (a data-only clone of a stock machine) cannot be declared.

`DK_PARAM(MACHINE, WORD, ID, NAME, SHORT, MIN, MAX, DEF)` declares parameter word `WORD` of the machine:

| Argument | Meaning |
| --- | --- |
| `WORD` | The index of the word in the DSP `params`, 0 to 3. Words come in order, without gaps. |
| `ID` | A C identifier, unique in the module. Do not rename it after a release. |
| `NAME` | Name shown while the knob turns, 31 characters at most. |
| `SHORT` | Label under the knob, 15 characters at most. |
| `MIN`, `MAX`, `DEF` | Knob units, 0 to 127. Fractions are allowed. |

The stored word is the knob unit times 256, so 127.0 is the word `0x7f00`. The adapter clamps each word to `MIN` .. `MAX` before your code reads
it. The build does not check that `DEF` lies inside the range. Keep it true yourself. `pack` refuses `MIN` above `MAX`.
Names are printable ASCII. Text longer than its field is a compile error. The kind of a parameter is a convention for the word:

| Kind | Word range | UI | Rest value | Use for |
| --- | --- | --- | --- | --- |
| percent | 0 .. 0x7f00 | 0 .. 127 | none, use `DEF` | amounts, times, mix, decay |
| bipolar | 0 .. 0x7f00 | -64 .. +63 | 0x4000 | detune, pan, tilt |
| enum | step x 256 | step number | step 0 | a switch or a mode with n steps |
| pitch | 0 .. 0x7f00 | semitone offset | 0x4000 | a pitch offset the adapter adds to `pitch` |

Every parameter shows as a plain 0 to 127 value. For an enum, set `MAX` to n-1 (the word is (n-1) x 256). No module uses an enum yet.
A signed or named display is not available for a machine.

Scaling the word: the stock DSP converts a word to a float with the scale 1/32512 (0x7f00 = 1.0). Your routine may use any scale. Document
it in the source header, as `modules/machines/pluck/src/dsp/pluck.s` does.

`DK_PAGE(MACHINE, NAME, TITLE, DK_KNOB(WORD), ...)` declares the page of encoders:

| Argument | Meaning |
| --- | --- |
| `NAME` | The tab name. |
| `TITLE` | The page title. |
| `DK_KNOB(WORD)` | The encoder of parameter `WORD`. List the words in order. |

A machine has at most one `DK_PAGE`. Use `DK_KNOB` only. `DK_KNOB_AT` is for effects. The page is optional.

`patchkit desc MODULE_ID` prints the machines, parameters, pages, voices and effects of a module as JSON, from the records.

### 5.3 What the device profile fills in

You declare words. The profile (`[modules.knobs]`, `[adapter.machine]` and `[modules.frame]` in `devices/<device>.toml`) places them on
knobs. `clone_of` is fixed by the profile (`[modules.knobs] clone_of = 0`, the SAMPLE machine). You cannot set it.

| Item | Digitakt II | Digitone II |
| --- | --- | --- |
| Where knobs live | Fields of the machine row. Engine slots 0 to 7. | Columns of a page. Sound-array slots 25 to 32. |
| Fixed knobs | TUNE on knob 0. Sample slot on knob 3. LEV on knob 7. | None. |
| Knobs for your words | 1, 2, 4, 5, 6 (letters B C E F G; `param_slots`). Word w goes to the w-th of them. 5 words. | 1, 2, 4, 5, 6, 0, 3, 7 (B C E F G A D H). 8 words. |
| Page | The machine has its SRC page. `DK_PAGE` is not used. | The `DK_PAGE` name and title. Without a `DK_PAGE`, a page named after the machine. |

Rules that follow:

- A machine has at most as many parameters as the device has `param_slots` (Digitakt II 5, Digitone II 8), at most one page, and the words are in order. Free placement of knobs (`DK_KNOB_AT`) and
  several pages for a machine need adapter work. They are planned.
- A parameter's position is its slot. Projects store values by slot. After a release, do not move a parameter to another word.
  Removing a parameter is allowed.
- The runtime assigns the machine's type number at load. Never write a number into code or a test.

### 5.4 Declaring an effect

An effect is declared in C too, in `src/module.c`, with the macros of `<dk/fx.h>` (`DK_FX`, `DK_FX_PARAM`) and `DK_PAGE` from `<dk/page.h>`.

```c
#include <dk/fx.h>
#include "freqshift.h"

DK_FX(fshift, .process = "_fs_process", .desc = "_fs_desc", .mix = 127,
      .route = DK_ROUTE_SEND, .routes = DK_ROUTE_SEND | DK_ROUTE_MASTER);
DK_FX_PARAM(fshift, FSHIFT_SHIFT_WORD, "Freq Shift", "SHIFT", DK_BIPOLAR, -64, 63, 0);
DK_PAGE(fshift, "FSH", "Freq Shifter", DK_KNOB_AT(FSHIFT_SHIFT_WORD, 0));
```

`DK_FX(ID, ...)`: `ID` is a C identifier. It is the effect's name inside the module. With the module id it makes the effect ID of saved kits.
Never rename either. The other arguments are field initializers:

| Field | Meaning |
| --- | --- |
| `.process`, `.desc` | The global symbol of the process routine. The data symbol of its `dk_fx_desc`. Strings. |
| `.defaults` | Words p0 and up at load. Not allowed together with `DK_FX_PARAM`. Default: all 0. |
| `.mix` | Default of the framework's MIX knob, 0 to 127. Default: 127. |
| `.route` | Where the effect starts: `DK_ROUTE_MASTER` (default) or `DK_ROUTE_SEND`. A send needs a `DK_PAGE`. |
| `.routes` | The routes the user may choose in SETTINGS > DIGIKIT > FX ROUTES, `DK_ROUTE_*` values or-ed. Left out: master only, or send and master when `.route` is send. |

`DK_FX_PARAM(FX, WORD, NAME, SHORT, DISPLAY, MIN, MAX, DEF)` declares parameter word p_WORD, a float:

| Argument | Meaning |
| --- | --- |
| `NAME` | Name shown while the knob turns, 1 to 23 characters. |
| `SHORT` | Label under the knob, 1 to 6 characters. |
| `DISPLAY` | `DK_PERCENT` (0 to 127) or `DK_BIPOLAR` (-64 to +63). |
| `MIN`, `MAX` | The floats at the bottom and the top of the knob. |
| `DEF` | Inside `MIN` .. `MAX`. It is also the word's value at load. |

`DK_PAGE(FX, TAB, TITLE, KNOB, ...)` declares the page. An effect with a page has its own page after CHO in FUNC + FX.

| Argument | Meaning |
| --- | --- |
| `TAB` | The page tab label, 1 to 3 characters. |
| `TITLE` | The page title, 1 to 23 characters. Use mixed case ("Freq Shifter"). |
| `DK_KNOB(WORD)` | The encoder of parameter `WORD`, at the lowest free position. |
| `DK_KNOB_AT(WORD, POS)` | The encoder at position `POS`, 0 to 7. The effect's MIX is on the level slot. |

An effect may have up to 7 knobs. Effect knobs can sit at any position. Knob placement is the same on both devices. The device profile does not
change it.

Text longer than its field is a compile error. `patchkit pack` checks the rest: lengths, ranges, routes, `mix`, distinct positions and the
knob slots of the D1 device.

Put the word numbers in a header in the module's `src/` (`freqshift.h`). The C file and the SHARC+ assembly both include it, because the build
searches `src/` for both. Use only decimal `#define`s and `//` comments on their own line: the SHARC+ assembler keeps a trailing comment in the macro text and upper-cases macro text.

MIX, the route and the send amounts belong to the framework. Never declare them. The effect writes the wet signal only.

### 5.5 Service sections

A service has `[module]`, `[targets]` and one `[[object]]` with `place = "cf.expansion"` and a C source. It has no declaration macros. The
service template does not set `dynamic`.

### 5.6 Examples

For a machine, see `modules/machines/pluck` (section 5.2). The profile turns its declaration into this layout. On the Digitakt II, DECAY is on
knob 1, PICK on knob 2, BRIGHT on knob 4 and COLOR on knob 5 (letters B C E F). TUNE and LEV are on knobs 0 and 7. On the Digitone II the same
words sit on the same letters, page columns 1, 2, 4 and 5, on the page PLUCK. Words 0 to 4 use the letters B C E F G on both devices.

For an effect, see `modules/fx/freqshift` (section 5.4). It has one bipolar knob, `.route = DK_ROUTE_SEND` and both routes.

### 5.7 Persistence and limits

`patchkit pack` refuses a declaration that breaks a rule above: more machine parameters than the device's `param_slots`, words out of order, a second page for a
machine, `DK_KNOB_AT` on a machine, a machine without DSP symbols, and a manifest with the removed sections.

Not refused today: a `DEF` outside `MIN` .. `MAX`.

Layout facts:

- Digitakt II: the machine row holds the knobs as `f0..f7` and the level as `f8`. A page shows 8 of the 10 engine slots ([finding 100](findings/100-module-limits.md)). An added machine has one
  SRC page: TUNE on knob 0, sample slot on knob 3, level on knob 7.
- Digitone II: a stock page holds eight knobs and the track level. A machine gets one page for now (a patchkit rule; the firmware allows several, [finding 100](findings/100-module-limits.md)), from its `DK_PAGE` or named after the machine.
- Limits per module (run-time pack): 16 parameters, 8 machines, 8 pages, 32 records in a register section.

Persistence and modulation:

- Values persist on both devices. Projects store raw words by slot, so a released parameter must keep its word.
- If the module is absent at load, the track keeps its type and saves it back unchanged. Stock firmware loads the track as SAMPLE
  (Digitakt II) or FM TONE (Digitone II) and loses the type on re-save.
- p-lock on a module parameter: not shown. LFO as a destination: the MOD page lists the parameter names on the Digitakt II, but that an LFO moves
  the DSP value is not known. The clamp stays on, because an LFO can drive a word out of range.
- MIDI CC and NRPN are ignored for added parameters. Effect knobs cannot be locked or modulated either.

Patching a stock machine will become a separate module type. It is not part of the content modules.

## 6. Generated files

`patchkit gen` writes `build/gen/gallery.json` from the module's declaration and the device profiles. It holds the machines with their
knobs and parameters, for each device, for the web gallery. It holds no firmware bytes. Do not edit it.

There is no generated header for the module. The module declares its numbers in C and in its own `src/` header (section 5.4).

The SDK headers are generated from `sdk/sdk.toml`. Modules include the public headers in `sdk/include/dk/`.
The generated files are in `sdk/include/dk/gen/` and `sdk/asm/`.

| File | What it holds | Who includes it |
| --- | --- | --- |
| `<dk/api.h>` | The `dk_api` table, the import names and the entry points of a `.dkm`. | Service modules in C. |
| `<dk/machine.h>` | The machine contract and `DK_MACHINE`, `DK_PARAM`. | The machine's `module.c`. Includes `gen/machine.h`. |
| `<dk/fx.h>` | The effect contract and `DK_FX`, `DK_FX_PARAM`. | The effect's `module.c`. Includes `gen/fx.h`. |
| `<dk/page.h>` | `DK_PAGE`, `DK_KNOB`, `DK_KNOB_AT`. Shared by both. Includes `gen/desc.h`, the record layouts from `[desc]` in `sdk/sdk.toml`. | `<dk/machine.h>`, `<dk/fx.h>`. |
| `sdk/include/dk/gen/machine_consts.h` | The machine constants as C `#define`s. | `<dk/machine.h>`. |
| `sdk/asm/dk_machine.inc` | The machine constants and field offsets for assembly. | The machine's `.s`. |
| `sdk/include/dk/gen/fx_consts.h` | The effect constants as C `#define`s. | `<dk/fx.h>`. |
| `sdk/asm/dk_fx.inc` | The effect constants and field offsets for assembly. | The effect's `.s`. |
| `sdk/include/dk/gen/limits.h` | The `.dkm` format limits (`[limits.format]`) as C `#define`s, such as `DK_ARENA_HDR`. | Core apps in C. |

Do not include `dk_fx.inc` and `dk_machine.inc` in one assembly file. `DK_BLOCK` and `DK_LEVEL_BOUND_BITS` are in both.

Do not edit generated files. To write them again:

```sh
mise run gen:modules                                     # all modules and the SDK
mise run gen:modules -- --check                          # list stale files, write nothing
uv run python -m tools.patchkit.patchkit gen --modules AUTHOR/NAME    # one module
```

Run `gen:modules` after you change `module.toml`, `src/module.c`, `devices/*.toml` or `sdk/sdk.toml`.
Generation reads existing cached content objects and never builds them. After changing a C declaration, run
`mise run module:build -- AUTHOR/NAME` before `gen:modules` to refresh the declarations in the cache.
For a new module with no cached objects, generation skips its gallery until that first build; run generation again afterward.

## 7. Contracts

The headers are the reference. This section lists what you need to know before you read them.

### 7.1 Machine voice (`<dk/machine.h>`)

| Entry | When it runs | Result |
| --- | --- | --- |
| `note_on(state, in, params)` | Once per note, before the first `render`. It makes no sound. | `0`, or a negative number (a fault). |
| `render(state, in, params)` | Once per block of `DK_BLOCK` (32) samples while the voice runs. | `DK_OK`, or `DK_DONE` when the voice has ended. |
| `desc` | A `dk_machine_desc` in read-only data: magic, contract version, parameter count, voices, state bytes, bytes cleared at note-on, cycle budgets. | Read by the adapter. |

`render` writes `DK_BLOCK` floats to `in->out`. `in` also holds `pitch` (semitones shifted left by 8, the tune included), `velocity`, `block` and the source `track`.
`params` holds `DK_MAX_PARAMS` words. The words after your parameters are 0.

The adapter enters your routine with `I13` = return address and `JUMP (M13, I12)`. The arguments are `R4` = state, `R8` = `dk_voice_in`,
`R12` = `params`. The result is in `R0`. In `dk_voice_in` the words are: 0 `pitch`, 1 `velocity` (0 to 127, shifted left by 8), 2 `block`
(blocks since the note-on), 3 `out` (pointer to 32 floats), 4 `track` (0 to 15, latched at note-on).
`track` requires machine contract 1.1; the first four offsets and the V1 descriptor are unchanged. On Digitone II it identifies the source
track independently of the physical voice slot, and stays latched until retrigger. The first trigger-edge block remains silent; the owner is
latched on the following note-on block. An invalid owner ends only that slot without calling the module.
`render` also runs on the note-on block, after `note_on`.

Stereo machines (machine contract 1.2, additive). A machine that has two output channels declares `.stereo = 1` in `DK_MACHINE`, sets
`DK_MACHINE_STEREO` in the contract word of its `dk_machine_desc` (`DK_MACHINE_V1 | DK_MACHINE_STEREO`, 257; `pack` refuses a mismatch), writes
`DK_BLOCK` floats of the right channel to `in->out_r` (word 5 of `dk_voice_in`) and returns `DK_STEREO` or-ed into the status (`DK_OK | DK_STEREO`,
`DK_DONE | DK_STEREO`) on every block. A mono 1.1 machine is unchanged: it never touches `out_r` and the adapter copies L to R. On the Digitakt II
the adapter writes R into the odd voice of the stock voice pair (the stock engine pans the two buffers independently); the guard checks both
blocks. On the Digitone II `out_r` is a scratch block for now and the status bit is dropped, so a stereo machine plays its L only until the shadow
right-channel stages are production (TODO in `dsp-voice-dn2.s`). `digikit/shot` is the example.
Do not place `note_on` at offset 0 of the image: an offset of 0 reads as "no note_on".

The `dk_machine_desc` has 10 words: magic, contract, parameter count, voices (1 to 16), state bytes per voice (a multiple of 4), bytes cleared at
note-on, max cycles of a block, max cycles of the note-on block, max call depth, stack bytes.

The adapter in `core/dsp-voice` zeroes `state_clear_bytes` at note-on and clamps the parameter words to your ranges. After each call it checks the
output. A block with a sample that is not finite or is louder than 8.0 is zeroed. The voice ends. After 8 faults in a second the machine is
switched off.

Registers:

- You may change R0-R15, I0-I5, M1-M4 and B3.
- Keep I6-I15, M0, M5-M15, L0-L7 and MODE1.
- The adapter pins M5 = 0, M6 = 1, M7 = -1, M13 = 0, M14 = 1 and M15 = -1, L0-L5 = 0, SIMD off and RND32 set. Return with `JUMP (M13, I13)`.
- `_dv_exp2` (F0 = 2^F0) is a helper of `core/dsp-voice`. Declare it with `.EXTERN _dv_exp2;`. It clobbers R0 to R3.
- A 16-bit form `Rn = Rn op Rx` can be read differently by the emulator and the silicon. The build refuses it. Use three distinct registers.
  Other assembler traps: `.agents/skills/digikit-module/references/selas-pitfalls.md`.

Memory: your code may write `in->out`, its own voice's state, its data stack (`stack_bytes`) and the data and bss of its own object. Data and bss
are shared by all voices. State that differs per voice goes in `state`. The conformance suite records every store and fails on any other.

The template `src/dsp/NAME.s` shows all of this in about 90 lines. Read it first. Pitch tests: `tests/patchkit/test_sine.py`.

### 7.2 Effect (`<dk/fx.h>`)

| Entry | When it runs | Result |
| --- | --- | --- |
| `process(state, in, params)` | Once per block while the effect is on. | `DK_FX_OK` (0), or a negative number. A negative number is a fault: the block plays dry. |
| `desc` | A `dk_fx_desc` in read-only data: magic, contract version, `n_params`, `state_bytes`, cycle budget, call depth, stack bytes. | Read by the loader. |

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
- The runtime reserves the state behind your image. There is no state symbol. A page needs no ColdFire code.
- Values arrive in `params` as the float `min + (max - min) * raw / 0x7f00`, about 3 ms after a turn. At load every word is its knob's `DEF`.
  Choose the range so that a turn is audible.
- The adapter makes `master = dry + mix * (wet - dry)` per sample, with mix = raw / 127. MIX 127 is exactly the wet signal and 0 is exactly the
  dry one. It costs about 7 DSP instructions a block at mix 1.0 and about 400 at another mix.
- Kits keep the knob values and MIX of each loaded effect, with an effect ID made from the module id and the effect `ID`. Load Kit gives an
  effect its stored values only when the stored ID for its row is its own. Otherwise the effect gets its declared defaults. Effects count by row,
  so the same effects loaded in another order get defaults. A kit saved with effects also loads on stock firmware.
- Not supported: p-locks, LFO and other modulation on a knob, MIDI CC, a second page for one effect, and a knob that does not reach the DSP as a
  float. An image without `core/fxpages` refuses a module with a page (`E_NO_REGISTRY`). The Digitone II image needs `core/dynfx` and
  `core/fxpages` in its module list (they are in its `device` patch set).
- Text is printable ASCII. Write titles in mixed case. The firmware font draws a capital Q like an O.
- Routes: a send effect gets the sum of the tracks' send amounts as its input block. Its wet output times MIX is added to the FX return. The
  user chooses the route in SETTINGS > DIGIKIT > FX ROUTES (the setting `fx/route/<module id>`). A stored route that the effect no longer allows is
  ignored. Use `DK_ROUTE_MASTER` only for a gain or limiter or any effect with a dry path of its own, `DK_ROUTE_SEND` only for a pure return such as a reverb, and
  both for the rest. The send amounts live in the settings store and the DSP row, never in your state or in a kit file. FUNC + FX lists DEL, REV,
  CHO, the send effects, then the master effects. A changed route takes effect after a restart. `pack` refuses an unknown or repeated route, an
  empty `.routes`, a `.route` that `.routes` does not list, and any send route without a `DK_PAGE`.
- A centred knob: `DK_BIPOLAR`, `MIN = -64`, `MAX = 63`, `DEF = 0`. A mapping with `2^x - 1` and no constant term gives exactly 0.
  A send effect with MIX default 127 changes nothing until a track has a send amount. On the master the same default means fully wet, so say in the
  README what it means there.
- Clamp the output to +-7.9. An input at the level bound times a filter's overshoot would otherwise refuse the block. Use an integer phase
  accumulator for anything that must not drift.
- Fixtures: `tests/fixtures/modules/dyn-fx-gain` (insert), `dyn-fx-gain-send`, `dyn-fx-gain-both`, `dyn-fx-gain-sendonly`. A real effect with a
  float32 reference model: `modules/fx/freqshift`.
- The instruction budget of the conformance suite is `pk_conform.fx_budget()` (about 31,000 for the effect today, 900 for the adapter). Read it
  from the report (`-rA`).

### 7.3 Service (`<dk/api.h>`)

A service is C code for the ColdFire CPU. The runtime calls these symbols of your object:

| Symbol | When it runs |
| --- | --- |
| `int32_t dkm_init(const dk_api *api)` | Once after load. Return 0. Any other value unloads the module. |
| `void dkm_fini(const dk_api *api)` | When the module is unloaded. |
| `void dkm_menu(dk_ui_menu *m, void *ctx)` | Optional: the module's settings. `DK_MENU(fill)` of `<dk/ui.h>` defines it. The DIGIKIT menu shows SETTINGS... in the side panel of the loaded module and runs `dkm_menu` only when the user opens it; `fill` adds the rows (`dk_ui_text`, `dk_ui_action`, ...). The menu is built once and kept, and no module is unloaded at run time, so the `ctx` and callbacks of a menu must point at the module's own memory, never at the stack. |
| `void dkm_slot_N(const dk_api *api)` | `N` is 0 to 7. You bind a slot by exporting its symbol. Slot 0 runs once per pass of the UI event loop, about 120 Hz. A taken slot gives `E_SLOT`. |

Export at least one of them.

The `dk_api` table that `dkm_init` receives. Fields are only appended, each is 4 bytes. Check `size` before you use a late field.

| Field | Purpose |
| --- | --- |
| `version`, `size`, `device`, `runtime` | Table layout version, table size, the device id, the packed runtime version. |
| `import(name)` | Address of a firmware function or runtime helper by name. 0 when the image has none. |
| `alloc(size)`, `free(p)` | A 16-byte aligned block from the module arena, and its release. `alloc` returns null when full. |
| `store_read(path, dst, cap)` | Read a store file. Returns its size, or a negative code. |
| `log(msg, value)` | Append to the runtime log. API 1.0. |
| `log_lv(level, msg, value)` | The same with a level. API 1.1. |
| `register_cmds(first, count, handler)` | Register SysEx commands. API 1.2. |
| `sample_bank(local_id)`, `sample_load(handle, path)`, `sample_status(handle)` | Resolve, load and query a module-owned sample bank. API 1.3; see the bounded proof contract below. |
| `files_client()`, `files_list(client, root, path, notify, context)`, `files_poll(client, ticket, out, capacity)`, `files_cancel(client, ticket)` | Read-only file/directory listing. API 1.4, runtime 0.5.4; see below. |
| `memcpy_addr`, `memset_addr`, `memcmp_addr`, `muldi3_addr`, `udivdi3_addr`, `umoddi3_addr` | Addresses of the runtime's helpers. The compiler calls the 64-bit ones by itself. |

For the bounded sample-bank proof, include `<dk/sample.h>` and declare
`DK_SAMPLE_BANK(LOCAL_ID, BANK, DESCRIPTOR, CAPACITY)` in the module's C declaration. Exactly one bank is supported:
reserve 1–8,192 mono float32 frames in the module's own DSP BSS and a four-word initialized zero descriptor
(READY, normal-word source pointer, frame count, sample rate). Check the API 1.3 table `size` covers the appended
sample functions before calling them. Resolve the positive opaque handle with `sample_bank(local_id)` during init.

Call `sample_load(handle, path)` from init or a service callback. It copies the NUL-terminated path and queues work
for the UI task; the callback performs no file I/O. `sample_status(handle)` returns UNUSED=0, QUEUED=1, LOADING=2,
READY=3, or a negative `DK_SAMPLE_E_*` error. Only an unused bank accepts a load. Input must be a complete canonical
mono 48 kHz PCM16 native sample, nonempty and at most 8,192 frames. DT2 uses an absolute +Drive path (`/...`), opens
read-only and requires no concurrent Transfer writes during acquisition. DN2 uses `samples/...` in the digikit store
and validates a private snapshot before delivery.

Once delivery starts, the bank stays pinned until restart, including on failure; no replacement or eviction is
supported. Runtime publishes the immutable descriptor's READY word last, after payload and view acknowledgements.
The [sample-proof fixture](../tests/fixtures/modules/sample-proof/README.md) demonstrates the declarations, service
calls and tested DSP bridge. Transferred-file playback acceptance passes in the emulator on DT2 OS 1.17 and
DN2 OS 1.12. Hardware behavior and product capacity/replacement/selection remain unproved; this is not a product sampler.

A stereo or mono sample pool (API 1.5, [design](design/sample-api-1.5.md)) uses `<dk/sample_track.h>` and
`DK_SAMPLE_TRACKS(LOCAL_ID, RECORDS, CONTROL)` instead of a bank (a module has one family): 16 `dk_strack` records (512 bytes), one for each
pool entry, and 16 control sets (256 bytes) in the DSP data (`.PKDATA`, zero, no relocations in them). `pack` writes them as the kind 2 record of
the DSP section and declares API 1.5; a module without a table declares 1.4. `sample_tracks(local_id)` gives the handle in init;
`sample_pool_add(handle, path)` queues a PCM16 native file (mono or stereo, 48 kHz) from a service or UI callback and returns its pool index
0 to 15: the first free entry, or the entry that already holds the path (`DK_SAMPLE_E_POOL` when all 16 are in use). The path is the
root-relative path of a `DK_FILES_SAMPLES` listing row (`KICKS/K1`); the runtime adds the device root (DT2 `/`, DN2 `samples/`), and a DT2 path
that already starts with `/` stays as it is. `sample_pool_clear(handle, index)` leaves a hole: indexes are stable, nothing moves. A machine picks
the entry of a note in its DSP, usually from a parameter (SHOT: SMP, word 0, `word >> 8`); an entry that is not READY is silent.
`sample_pool_path(handle, index, buf, cap)` (API 1.6) gives the path back in the same form, in every state but UNUSED. The pool list is kept per
project. The runtime closes an entry's old generation before it publishes a new record. A module that only calls these entries has no table and
must check the table `size` before it calls them. `pack` declares the lowest Api minor that has what a module needs; a module that calls a newer
entry without checking `size` lists it in `[module] uses_api = ["sample_pool_path"]` and is then refused by a runtime that lacks it.

A sampler machine can add the on-device sample picker with `<dk/sample_picker.h>`: list `../../../sdk/src/sample_picker.c` and
`../../../sdk/src/picker_logic.c` in the `[[object]]` sources, call `dk_picker_init(api, handle)` in `dkm_init` and `dk_picker_fini()` in
`dkm_fini`, and call `dk_picker_open_pool(current, pick, ctx)` from the click action of the SMP parameter. The picker shows `+ ADD` and the 16
entries; `pick(ctx, index)` runs after a file was added or an entry chosen, and the machine sets its SMP value to `index`.
`modules/machines/shot` is the example (design: [sample picker](design/sample-picker.md)). The picker needs no manifest field: it checks the Api
`size` before it calls `sample_pool_path`, and without `core/ui` it does nothing.

For reusable browsing, include `<dk/files.h>` and acquire a positive `files_client()` handle during init.
`files_list` copies a canonical root-relative NUL-terminated path and returns a positive request ticket;
empty path lists the root. `DK_FILES_SAMPLES` maps to DT2 +Drive or DN2 store `samples/`, and
`DK_FILES_STORE` lists the runtime store on either device. A new request supersedes the previous one.
The public path holds at most 255 bytes; provider limits are smaller (DT2 sample paths 254 bytes,
store paths 159 bytes, DN2 nonempty sample paths 151 bytes after the virtual prefix).

`files_poll` performs no storage I/O: it returns pending (0), a copied `dk_file_entry` (1), end (2),
or a negative error. Pass space for the whole 304-byte record; only return 1 changes the output.
Rows retain the full path and file/directory kind. Check validity flags before using size or hash.
Size is physical source bytes: DT2 includes the 16-byte trailer omitted by stock ReadDir.
Hashes are typed stored metadata: SHA-256 or DT2's odd 32-bit hash in the first four bytes, big endian.
Listing neither pins contents nor validates a sample; revalidate the selected source when loading it.

An optional callback receives `(client, ticket, state, context)` in the UI task without owning slot 0.
It is a deferred, coalesced readiness hint: poll the supplied ticket and tolerate a stale-ticket error.
Keep callback context in module memory, never on the stack. Cancel or request another folder without
waiting for old work; late results are discarded. Fill/action callbacks should queue or poll, not read
storage. See the [listing design](design/sdk-files.md) and
the [public boundary fixture](../tests/fixtures/modules/files-proof/src/files-proof.c).

Names a module may import (from `[[import]]` in `sdk/sdk.toml`):

| Name | Purpose |
| --- | --- |
| `fw_block_read`, `fw_alloc`, `fw_free` | The firmware's eMMC block read and allocator. |
| `memcpy`, `memset`, `memcmp`, `__muldi3`, `__udivdi3`, `__umoddi3` | Runtime helpers. |
| `input_key`, `input_turn` | Press a key or turn an encoder. They need `core/input`. Without it the import is not found. |
| `dk_ui_*` | The calls of `<dk/ui.h>`: menus, lists, rows and dialogs. They need `core/ui`. Without it the module still loads and each call returns its failure value (`dk_ui_menu_new` 0, the int calls -1, `dk_ui_has`, `dk_ui_is_open` and `dk_ui_checked` 0, the void calls nothing). `dk_ui_tick(fn, ctx, period_ms)` is a timer in the UI task (`DK_UI_CAP_TICK`): redraw a screen while a load runs; 0 stops it. |
| `dkrt_blocks` | The 1500 Hz block clock. |
| `dl_set_tick_cb` | Sets one per-block callback in the block interrupt (`core/dsp-link`; a stub without it): `void cb(const volatile u16 *rec)` gets the previous DSP reply record. Short C only; clear it in `dkm_fini`. |

A load fails with `E_IMPORT` when a name is not in the runtime's table.

`register_cmds` adds SysEx commands. Call it from `dkm_init`. The range is released after `dkm_fini`. The range must lie inside a range reserved in
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
| Memory audit | `mise run module:audit -- NAME [--device dt2\|dn2]` | Loads the module on a ColdFire-only emulator, drives the UI and reports ColdFire stores outside the module's blocks. About 10 to 30 s per device. |
| Pack | `mise run module:pack -- ID --device dt2\|dn2 [-o FILE]` | Builds the objects that are stale and makes the `.dkm` from them in one command (no separate build step). Default: `out/patch/NAME-DEVICE.dkm`. It refuses a bad declaration, a bad route and a send without a page. |
| Pack all | `mise run module:pack-all [-- --force]` | Packs every run-time module for every device into `out/modules/<device>/<id>.dkm`. |
| Emulator | `mise run workbench` | Starts the emulator. Run it in another shell. |
| Run | `mise run dk:run -- ID --emu` | Builds, packs, uploads, selects, restarts and follows the log. |
| Run on a device | `mise run dk:run -- ID --port Digitakt` | The same on a device that has the digikit runtime. The port is a part of the MIDI port name. |
| Log | `mise run dk:log -- --emu follow` | The runtime log. See section 11. |
| Card | `mise run dk:store -- --emu info` | Lists the store. Prints one `capacity` line per resource. |

Every task takes `-- --help`. Hardware is a deliberate step: do not flash a device from these commands.

`module:check` runs one pytest process. What it runs depends on the kind:

| Kind | What `module:check` runs |
| --- | --- |
| `machine` | `tests/modules/test_machine_conformance.py` on both devices, or on one with `--device`. Every parameter at its limits, many notes, modulation, long notes and retriggers. The write-set check is part of it. |
| `fx` | `tests/modules/test_fx_conformance.py`: signals, parameter limits, modulation, switching and a long run. It checks returns, registers, the write set and the instruction budget. |
| `service`, `raw` | No conformance suite. It checks the objects against their sources. |
| all | The module's own tests: `tests/**/test_*.py` of the module, and the digikit test files of the kinds of its `tests/scenarios/*.toml`, limited to those scenarios. These may build or launch an emulator. |

A module owns its tests. They live in the module folder and nothing else lists them:

| File | Found by |
| --- | --- |
| `tests/test_*.py` (also in subfolders) | `module:check`, `verify`, `test:changed`, `python-tests --full`. The basename must be unique in the repository (no `__init__.py`): start it with the module name, for example `test_hello_range.py`. A helper next to the test is imported by its name. The repository root and `tests/` are on `sys.path` (`tools.*`, `support.*`). Use `Path(__file__).parents[1]` for the module folder. |
| `tests/*.test.ts` | The Vitest project `module-tests` (`mise run test:host`). |
| `tests/scenarios/*.toml` | The digikit scenario runner (`pk_digikit_scen list\|run NAME`) and the tiers, next to `tests/digikit/scenarios/`. Scenario names are unique across both places. |

A change in a module selects the module's own tests and those of the modules that depend on it. A change only in `tests/` runs the tests and no
build. A PCM pin of a module stays in that module's `tests/scenarios`. Nothing outside the module folder names a user module: `mise run
check:layers` (rule `self-contained`) fails on a module id or path in code or tests. Docs and `*.md` files may name modules. A module with
`host = true` in `[module]` is exempt: the workbench speaks its protocol (the remote control service). Tests of shared tools use the fixtures in
`tests/fixtures/modules` (`test/sine` is the small dynamic machine, `test/dyn-fx-gain` the effect), or a made-up id such as `acme/demo`.

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

## 9. Versions and compatibility

- `sdk/sdk.toml` is the single source of the runtime version and the contract versions. Each contract is `[major, minor]`.

  | Contract | Version | Covers |
  | --- | --- | --- |
  | `api` | `[1, 5]` | The `dk_api` table. Every module has it, `raw` included. `pack` declares 1.5 only for a module with a pool table, else 1.4. |
  | `machine` | `[1, 2]` | `<dk/machine.h>`; minor 1: `track`; minor 2: stereo machines |
  | `fx` | `[1, 0]` | `<dk/fx.h>` |
  | `service` | `[1, 0]` | Service modules |

- `pack` writes the contracts a module uses into the `.dkm` header.
- The runtime refuses a module when the major version of a declared contract differs, or when its minor version is above the runtime's. The error is
  `E_CONTRACT` (-48): rebuild the module.
- An added `dk_api` function is a minor bump. A changed or removed one is a major bump.
- `[module] version` is yours. Change it for every release. The export file name carries it.
- A run-time machine has no fixed type number. The runtime assigns one at load, and the card remembers it under the global id
  `<module id>:<machine id>`. Never write the number into code or a test.
- The effect ID of saved kits comes from the module id and the effect `ID`. Do not rename either after users have kits.

## 10. Export and import

```sh
mise run module:export -- AUTHOR/NAME                    # out/patch/AUTHOR-NAME-VERSION.dkmod
mise run module:import -- FILE.dkmod --dir ~/my-modules  # unpack into ~/my-modules
```

A `.dkmod` is a deterministic zip. It holds the module folder without `build/` and without OS junk files. Entries are sorted and share one time
stamp. A top-level `package.toml` lists id, version, author, type and the SHA-256 of every file. Two exports of one tree are byte-identical.

`import` checks every hash. It refuses extra or missing files, the author `core`, the type `framework` and an id that is already known. `--replace`
allows a known id. Without `--dir` it uses the first folder of `PATCHKIT_MODULES`. After an import, run `module:check` before you trust the code.

## 11. Logging

- ColdFire code logs with `api->log(msg, value)` (level info; 24 characters of the message are kept) or `api->log_lv(level, msg, value)`.
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

## 12. Looking things up

| Question | Where |
| --- | --- |
| Version numbers, imports, log kinds, protocol numbers | `sdk/sdk.toml` |
| Machine, effect and service contracts | `<dk/machine.h>`, `<dk/fx.h>`, `<dk/page.h>`, `<dk/api.h>` |
| What differs between the devices | `devices/digitakt-ii.toml`, `devices/digitone-ii.toml`, `.agents/skills/digikit-module/references/devices.md` |
| Knob and parameter rules | Sections 5.2, 5.3 and 5.7, `docs/findings/02-machines-and-parameters.md` |
| DSP recipes and assembler traps | `.agents/skills/digikit-module/references/dsp-recipes.md`, `selas-pitfalls.md` |
| What each reference module taught | `docs/findings/54-module-cookbook.md` |
| The SDK design | [design/module-sdk.md](design/module-sdk.md), [design/digikit-dkm.md](design/digikit-dkm.md) |
| `core/*` modules, patching, anchors | [modules/README.md](../modules/README.md), [PATCHING.md](PATCHING.md) |
| The module loop and tests | [DEVELOPMENT.md](DEVELOPMENT.md), [TESTING.md](TESTING.md) |
| The same workflow for coding agents | `.agents/skills/digikit-module/SKILL.md` |

The firmware reference (`mise run fw:reference`, which writes `out/reference/fw.html`) is for `core/*` adapters. Content modules do not use
firmware declarations or anchors.
