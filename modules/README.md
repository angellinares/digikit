# Core modules and patching

This file is the reference for modules that patchkit links into a stock Elektron `.syx`: the `core/*` frameworks and other patched-in modules. A
patched-in module is our own code (ColdFire C or assembly, SHARC+ assembly) plus a manifest, `module.toml`. patchkit links the
code into the image, places it in a free region, hooks firmware sites and writes a patched `.syx`.

Content modules (machines, effects and services that load from the card as a `.dkm`) are not described here. Their guide, with the manifest
sections for machines, parameters, pages, voices and effects, is [docs/MODULE-AUTHORING.md](../docs/MODULE-AUTHORING.md). How
patchkit works and how to run it: [docs/PATCHING.md](../docs/PATCHING.md). Rules that apply to every module:

- **No firmware bytes in a module.** A `write` carries the SHA-256 of the bytes it replaces, never the bytes.
  Tables of the stock firmware are copied from the user's image at patch time (`[[section]]`).
- **No addresses in a module.** Every firmware site is a named anchor, found by a rule declared in `devices/fw/`.
- **Do not name or copy the vendor DSP toolchain or its files.** Cite public manuals only
  ([docs/sharc/SOURCES.md](../docs/sharc/SOURCES.md)).
- Nothing patched has run on a device. Say "emulator only" in the gallery text until it has.

## Directory layout

```
modules/<category>/<name>/
  module.toml          the manifest (this file's reference)
  src/cf/*.c|*.s       ColdFire sources (m68k-elf gcc, -mcpu=54418, freestanding, relocatable)
  src/dsp/*.s          SHARC+ sources (algebraic syntax; every assembled word is re-decoded by our decoder)
  build/*.dko.json     built objects of a core/* module only (committed); the objects of any other module are build output in
                       out/cache/dko/<module id>/ (not committed; `patchkit build` and `patchkit pack` make them)
  tests/*.toml         harness tests
  README.md            notes: what it does, devices, limits
  gallery/icon.svg     the gallery card's icon
```

Sources may sit in subfolders (`src/cf/dt2/`, `src/cf/dn2/`) when one module serves both devices.

## Conventions

- **Module id**: `category/name`, lower case, hyphens (`digikit/freqshift`). The id need not equal the directory; patchkit
  refuses two modules with one id. Module roots: `modules/`, `tests/fixtures/modules/` (test fixtures, ids `test/NAME`) and the
  extra roots of `--modules-dir` and `PATCHKIT_MODULES`.
- **Categories**: `core` (frameworks other modules require), `fx`, `machines`, `ui`, `tools`, `test` (fixtures and
  research probes; they are not for users). `mise run module:new` makes run-time content modules only (`machine`, `fx` or `tool`; guide section 3). A patched-in
  module starts as a copy of a similar one, for example `tests/fixtures/modules/patched-pluck`.
- **Entry ids** (in `[[entry]]`): `[a-z][a-z0-9_]*`. They become symbols (`machines.pluck.index`).
- **Symbols** in expressions: `[A-Za-z_.][A-Za-z0-9_.]*`.
- **Version**: `major.minor.patch`; raise it when the module's behaviour or its interface to other modules changes.
  The version goes into the patch record of the image.
- **`status`**: `experimental` for everything today. Use `stable` only after hardware tests.
- **`[targets]`**: one line per port, `untested`, `experimental` (works in the emulator) or `stable` (works on a device).
  patchkit does not read it; the gallery shows it as a badge. Compatibility itself comes from the anchors: a module
  fits a firmware when all its anchors resolve there. Use `ports = [...]` to restrict a module to some firmware.
- **Claims**: declare every scarce resource the module takes with `[[claim]]`, for example the one hook site that a
  module takes (`dsp.fx.hook`). Two modules with the same claim cannot be patched together, and the gallery
  explains it. Registry entries claim themselves.
- **Dependencies**: `requires` lists framework modules. The command line and the gallery add them for you (the
  command line prints `added by requires: ...`); a missing module or a `requires` cycle is refused.
- **`summary` and `gallery/icon.svg`**: what the gallery card shows. `summary` is one short sentence for players (at
  most 120 characters, ending with a full stop). The icon is line art on a 48 grid: `currentColor` lines and one
  highlight in `style="stroke: var(--icon-accent)"`. `apps/web/tests/icons.test.mjs` checks it.
- **`README.md`**: the module's notes. Write for a player: what it adds, which firmware, how to hear it, limits,
  "emulator only". No firmware bytes, no vendor tool names.
- **Comments in sources** say what the code does at each firmware site and which registers are free there, and cite
  the finding that shows it (`docs/findings/NN-...`).
- **Findings**: write results into `docs/findings/` with the marks of AGENTS.md, one file per topic.

## Manifest reference

Numbers can be written as TOML integers or as strings holding an *expression* (see
[Expressions](#expressions)). patchkit ignores unknown keys in most tables, so a misspelled key is silent: check each key against the tables below. (Unknown fields of an `[[entry]]` are an error.)

### `[module]`

| Key | Meaning |
|---|---|
| `id` | `category/name`, unique in the library |
| `version` | module version, `major.minor.patch` |
| `name` | display name in the gallery |
| `category` | `core`, `fx`, `machines`, `ui`, `tools` or `test` |
| `role` | layer of a `core` module, required for them: `runtime` (`core/runtime`), `installer` (`core/expansion`), `adapter` (`core/adapters/*`) or `app` (`core/apps/*`). It must fit the folder; `tools/devtools/layercheck.py` checks it ([docs/ARCHITECTURE.md](../docs/ARCHITECTURE.md)) |
| `type` | `machine`, `fx`, `service`, `raw` or `framework` (`framework` is internal, for core modules) |
| `author` | who wrote the module (`digikit` for the modules of this repository) |
| `status` | `experimental` or `stable` |
| `summary` | one sentence for the gallery card |
| `requires` | module ids that must be selected too (frameworks) |
| `conflicts` | module ids that must not be selected |
| `ports` | optional: only these ports, for example `["dt2-1.17"]` (empty: any firmware whose anchors resolve) |
| `dynamic` | optional, default false: a run-time module (a content module, see [docs/MODULE-AUTHORING.md](../docs/MODULE-AUTHORING.md)). It is never patched into an image (patchkit refuses it). `patchkit pack` makes a `.dkm` from it. Its machines have no number in `modules/core/adapters/machines/types.toml`: the runtime assigns a type in 16 to 23 when it loads the module (docs/design/digikit-registry.md "Machine keys") |

```toml
[module]
id = "digikit/freqshift"
version = "0.1.0"
name = "Frequency shifter"
category = "fx"
type = "fx"
author = "digikit"
status = "experimental"
summary = "Moves every frequency up or down by the same number of Hz."
dynamic = true
ports = ["dt2-1.17", "dn2-1.12"]
requires = []
conflicts = []
```

### `[targets]`

Test status per port (`dt2-1.17`, `dn2-1.12`, and `dn2-1.11` for the few modules ported to it): `untested`, `experimental` or `stable`.

```toml
[targets]
"dt2-1.17" = "experimental"
"dn2-1.12" = "experimental"
```

### `[[object]]`

A built object that patchkit places in a region. One module may have several (for example one per device).

| Key | Meaning |
|---|---|
| `path` | built object, relative to the module (`build/x.dko.json`) |
| `sources` | source files `module:build` compiles into it (`.c`, `.s`), relative to the module; all of one CPU |
| `place` | region: `dsp.l2` (SHARC+ code and data), `cf.expansion` (ColdFire code, data, bss) or `cf.flash_cave` (only the built-in loader) |
| `devices` | optional: `["dt2"]`, `["dn2"]` or both (default: both) |
| `[object.externs]` | undefined symbol of the object = anchor name (a symbol named exactly like an anchor needs no entry) |
| `optional` (key of `[[object]]`) | undefined symbols that resolve to 0 when no selected module defines them (an optional link to another module; the runtime uses it for `core/dynreg`) |

```toml
[[object]]
path = "build/fx-dsp.dko.json"       # core/fx
sources = ["src/dsp/fx.s"]
place = "dsp.l2"

[object.externs]
_fx_table = "fx.rows"
_fx_master = "dsp.master.value.q31_converter.float_buf"
_fx_back = "hook.dsp.fx.fn.entry.tail"
```

An object placed in `cf.expansion` makes patchkit add the built-in `core/expansion` module (loader, segment chain).
Other undefined symbols resolve to symbols of other modules, registries (`voices.rows`), sections and hook tails.

### `[[op]]`

An edit at a firmware site. Common keys: `op`, `anchor`, `offset` (bytes from the anchor, default 0), `devices`.

`op = "write"`: replace bytes.

| Key | Meaning |
|---|---|
| `hex` | the new bytes (hex string); or |
| `value` + `width` | an expression and its width in bytes (1, 2 or 4), range checked; not both with `hex` |
| `expect_sha256` | SHA-256 of the bytes being replaced. Needed when the write reaches outside the anchor's own bytes |
| `anchors` | `"PREFIX.*"` instead of `anchor`: repeat a `value` write for every anchor `PREFIX.<n>` in numeric order; `this` in the expression names the current anchor |
| `[[op.reloc]]` | relocations to module symbols inside `hex`: `offset`, `type` (`cf.32`, `cf.pc32`, `cf.pc16`, `cf.pc8`, `sharc.abs24`, `sharc.abs32`), `symbol`, `addend` |

```toml
[[op]]                       # core/expansion: `jsr copy; jsr clear` becomes `jsr pk_loader; nop`
op = "write"
anchor = "cf.rtos.fn.reset.init_calls"
hex = "4eb9000000004e71"
[[op.reloc]]
offset = 2
type = "cf.32"
symbol = "pk_loader"
```

```toml
[[op]]                       # raise every stock bound by the number of added rows (see core/params)
op = "write"
anchors = "cf.param.sites.descriptor_table.bound.*"
offset = 2
value = "image.u32(this + 2) + params.count"
width = 4
```

`op = "hook"`: attach a routine of the module to a hook declared in `devices/fw/` (see
[devices/fw/README.md](../devices/fw/README.md)). The declaration gives the site, the offset, the CPU, the `mode` and
the register contract. `mode = "before"` runs the routine, then the displaced instructions (copied to
`hook.<site>[.<offset>].tail`, which jumps back to `.return`). `replace` drops the displaced instructions.
`retarget` changes the target of an existing call at the site.

| Key | Meaning |
|---|---|
| `hook` | the hook name, for example `dsp.fx.hook.entry` |
| `to` | the routine (symbol or expression) |
| `place` | region of the tail (default `cf.expansion` or `dsp.l2`); optional |
| `expect_sha256` | optional; needed when the site reaches past the anchor |

`anchor`, `offset`, `displaced` and `cpu` are errors on a hook op. `op = "retarget"` and `kind = "call"` no longer
exist.

```toml
[[op]]
op = "hook"
hook = "dsp.fx.hook.entry"   # core/fx
to = "_fx_entry"
```

On ColdFire a hook needs a site of whole instructions; `jmp.l` is 6 bytes and the rest is padded with NOPs. PC-relative
operands in displaced instructions are re-encoded as absolute, except `(d8,PC,Xn)`, which is refused. A SHARC+ hook is
one 48-bit jump at a parcel-aligned offset; keep it out of delayed-branch slots and loop ends.
What registers are free at a hook site is the module's business: document it in the source header.

### `[[claim]]`

| Key | Meaning |
|---|---|
| `resource` | name of the resource, for example `dsp.fx.hook` or `cf.rtos.fn.reset.init_calls` |
| `devices` | optional device filter |

### Anchor rules (`devices/fw/`)

A module manifest has no rules. Rules are declared in `devices/fw/*.toml` ([design](../docs/design/firmware-decls.md)).
To look up anchors, addresses and binding names, run `mise run fw:reference`.
They tell `mise run patch:port` how to find a firmware site. It writes the result (address, length, SHA-256) into
`devices/ports/*.toml`. The same anchor name may have one rule per device (`devices = [...]`). A rule that finds no
match or two matches is an error. The keys below go in the `rule` table of a declaration.

Common keys: `name`, `cpu` (`cf` or `sharc`), `kind`, `devices`, `mark` (`"D"`: measured or read once; use `"V"` only
after a second agent checked it), `offset`, `len` (bytes).

| `kind` | Extra keys | Finds |
|---|---|---|
| `sharc_func` | `reloc_hash`, `offset` in parcels | `len` bytes at a fixed offset in a function with this relocation-insensitive hash |
| `sharc_literal` | `reloc_hash`, `offset` | the constant loaded by the instruction at that offset (a data address) |
| `sharc_mem_addr` | `reloc_hash`, `offset` | the absolute data address of a direct DM access |
| `sharc_table` | `reloc_hash`, `offset`, `len` | the table a literal points to |
| `cf_func` | `hash`, `ordinal` (default 0), `offset` | `len` bytes at an offset in a function with this address-masked hash |
| `cf_ptr` | `hash`, `ordinal`, `offset`, `len` | the table behind an address literal in a function |
| `cf_words` | `words = [u32 or "*", ...]` | a table row by masked signature |
| `cf_params_t1`, `cf_params_refs`, `cf_params_bounds`, `cf_params_sites` | | the parameter table and its readers (used by `core/params`; `exclude_anchors` drops sites inside named anchors) |
| `cf_reset_init_calls`, `cf_reset_helper` | `which = "copy"\|"clear"` | the reset-path calls (used by `core/expansion`) |

```toml
# devices/fw/<subsystem>.toml
[[site]]
name = "dsp.master.site.q31_converter.call"
cpu = "sharc"
rule = { kind = "sharc_func", reloc_hash = "65c7ebdfe23c9a760bc6077a912e16c0474c182ee9849925cb0c10af0689f984", offset = 0x15c, len = 6 }
mark = "D"
```

To write a SHARC rule for a site: `uv run python -m tools.patchkit.port_resolve rule-sharc dt2-1.17 0x1c7734 --len 6` prints the
hash and offset. ColdFire rules come from the function hashes of the `cfdb` database (`tools/coldfire/cf.py`; the
`catalog emit-rule` command will print them, see the design). Then run `mise run patch:port` and check that the diff of
`devices/ports/` shows your anchors on every firmware you claim. Full syntax: the docstring of `tools/patchkit/port_resolve.py`.

### `[[registry]]` and `[[registry.field]]`

A framework declares a table that other modules add rows to.

| Key | Meaning |
|---|---|
| `name` | registry name (`machines`, `params`, `voices`) |
| `first` | index of the first entry (an expression over anchors and image reads; no module symbols) |
| `max` | most entries the framework can take |
| `region` | where the rows are placed |
| `devices` | optional; a registry with one name may be declared once per device |
| `terminator` | `true`: one all-zero row follows the entries (not counted), for a routine that scans until a zero row |
| `index_table` | path of a TOML file with permanent entry numbers (see below); entries are numbered by it, not by order |
| `[registry.gap]` | with `index_table`: the field values of a gap row (the field defaults fill the rest) |
| `[[registry.field]]` | `name`, `kind`, `max_len` (`cstr`), `default` |

Field kinds: `u8`, `u16`, `u32`, `cstr` (a pointer to a string placed behind the rows; `max_len` checks the length)
and `sym` (an expression: a symbol of any module or `<registry>.<id>.index`). Rows use natural alignment, the row
size is rounded to the widest field, the byte order is the region's CPU. A field with no `default` must be given by
every entry.

patchkit defines `<reg>.first`, `<reg>.count`, `<reg>.entries`, `<reg>.rows`, `<reg>.row_size` and
`<reg>.<id>.index` (= first + k, entries in canonical module order; with an `index_table` see below).

**Permanent numbers (`index_table`).** A project stores only a machine's type number, so the number must not depend
on which other modules are selected. `core/machines` therefore ships `types.toml`:

```toml
[dt2]
last = 23                          # the highest number the framework supports on this device
[dt2.alloc]
"test/patched-pluck:pluck" = 9         # "<module id>:<entry id>" = number
```

The numbers are allocated by hand (the next free number on each device) and committed with the module. A number is never reused or changed, also not
after its machine is removed. Digitakt II numbers start at 7, Digitone II at 5 (stock types are never allocated); `last`
is 23 on both (Digitakt II: the DSP selector table was read to entry 23). patchkit refuses an entry without a number,
two entries with one number (the table itself is checked at load) and a number outside `first ..= last`. The table
spans `first` to the highest *selected* number: a number nobody selected is a **gap row** built from `[registry.gap]`
(for machines: a clone of type 0 with position `0xffffffff`, which the machine list does not show; a stored project
that names it plays as type 0). `<reg>.count` counts the rows including gaps, `<reg>.entries` the real entries; plan.json
lists the gap indices as `gaps`. Give a new framework with a persistent number the same two things: a table file and
a gap row that its routines can skip.

```toml
[[registry]]
name = "voices"
first = "0"
max = 16
region = "dsp.l2"
terminator = true

[[registry.field]]
name = "type"
kind = "u32"
[[registry.field]]
name = "pc"
kind = "u32"
```

### `[[entry]]`

(Machine modules declare their rows in C; see "Machine rows" below.) A patched-in module adds a row to a framework's registry. Keys: `registry`, `id`, `devices`, and one key per registry
field. Numbers, strings for `cstr` fields and expressions or symbols for `sym` and numeric fields.

```toml
[[entry]]
registry = "voices"
id = "pluck"
type = "machines.pluck.index"     # the permanent type number of the machine below (core/machines types.toml)
pc = "_pk_render"                 # the routine, a symbol of this module's DSP object
devices = ["dt2"]
```

patchkit refuses an entry for an unknown registry, an unknown or missing field, a value that does not fit, a
`cstr` that is too long and an id used twice.

### Machine rows (lowering of the C declarations)

**Note.** A machine is declared in C with `DK_MACHINE`, `DK_PARAM` and `DK_PAGE` (`sdk/include/dk/machine/machine.h`; see
[docs/MODULE-AUTHORING.md](../docs/MODULE-AUTHORING.md), section 5.2), not in the manifest. The manifest parser ignores unknown keys,
so `patchkit` keeps a short refusal: a manifest with `[[fx]]`, `[[voice]]`, `[[machine]]`, `[[param]]` or `[[page]]` fails with the
name of the C macro to use. The text below describes how the declarations are lowered into registry rows.

The declarations are lowered per device into the rows of `core/machines`, `core/params` and `core/dsp-voice`
(`packages/patchkit/src/typed.rs`; the lowering also runs in the browser).

- You write parameters and knobs. The device profile places them (`[modules.knobs]`, `[modules.templates]` and `[modules.frame]` in
  `devices/<device>.toml`). A device file that has no `[modules]` section takes no machine declarations, and a new device is a new TOML file.
- A `DK_PARAM` takes the profile's `encoder` template unless a `DK_PARAM_GLYPH` record picks `fader`, `pan`, `onoff` or `tune`. The knobs of a `DK_PAGE` (`DK_KNOB(n)`) name parameters of the module.
- Parameter codes follow the order of the `DK_PARAM` records (`params.<id>.index`). Type numbers stay permanent
  (`modules/core/adapters/machines/types.toml`); the lowering needs none.
- Lowered rows, per device: `voices` (`type = machines.<id>.index`, `pc = render`), `machines`, `params`, `pages`.
  On a device whose knobs go to the machine row (`to = "machine"`: Digitakt II) the machine row gets `f0..f7` and the
  level `f8`. On a device whose knobs go to a page (`to = "page"`: Digitone II) the `[[page]]` gets `c0..c7`, the slots
  `s0..` and the level `c8`; a stock code on a page is refused.
- A typo is an error: unknown keys of a device table, a device table for a device without a model, more knobs than
  the device has, a page on a device that keeps knobs in the machine row.
- Hand-written `[[entry]]` rows still work, also next to declared machines (for example for a registry the typed form does not cover).

The device model, from `devices/digitakt-ii.toml` (comments in the files):

```toml
[modules.knobs]   # count, slot_base, to = "machine" | "page", knob_field, slot_field, level_field, level_default, own_dsp
[modules.templates] # template names for [[param]] (`encoder`, `fader`, `pan`, `onoff`, `tune`: the glyphs) and their stock codes
[modules.frame]   # base, stride, group, gap, mirror_base: where the DSP finds knob j of track t
```

The frame word of knob `j` of track (voice) `t` is halfword `base + stride * t + j + (j / group) * gap`; the parameter
table gives it the mirror index `mirror_base + j + (j / group) * gap`.

#### Generated files (`patchkit gen`)

`mise run gen:modules` (`uv run python -m tools.patchkit.patchkit gen`, or the binary; add `-- --check` to only report stale files) writes
`modules/<id>/build/gen/` for every module with a machine declaration; `gen --check` fails when a committed file is
stale (`packages/patchkit/tests/typed.rs` runs it). The files hold no firmware bytes. The files and who includes them are in
[docs/MODULE-AUTHORING.md](../docs/MODULE-AUTHORING.md), section 6. In `<device>.inc` the values are in decimal (the assembler upper-cases macro text, so `0x` literals break after `+`).
`gallery.json` is part of the web bundle (`files["build/gen/gallery.json"]`) and shown on the module card.

A module can also ask for constants from the device files (`devices/*.toml`: `[board]`, `[digikit]`, `[card.store]`) and
`core/machines` `types.toml` with `[[gen]]` entries in its `module.toml` (`kind` = `rust`, `gas-shared`, `gas`, `sharc` or `sharc-adapter` (only the `ADM_*` defines of `[adapter.machine]`), a
`path`, and for `rust`, `gas-shared`, `gas` and `sharc` a `uses` list of the constants the module reads; `{dev}` in the path makes one file per device). They are written by the same `patchkit gen` and checked by `gen --check`;
what each kind holds, which fact lives where and what a new device needs are in `docs/design/device-data.md`.

`sdk/sdk.toml` is the one source of the runtime version (0.1.0, packed `major<<16|minor<<8|patch`; runtimes 1 to 5 read as 0.0.1 to 0.0.5), the
contract versions (api, machine, fx, service) and the log kinds. `gen` writes the SDK headers in `sdk/include/dk/gen/` (`api.h`, `machine.h`, `fx.h`
and their constants) and the SHARC+ includes in `sdk/dsp/contract/`, `modules/core/runtime/sdk/src/gen_sdk.rs` and
`tools/device/dklog_kinds.py` from it, and the per-type limits (`[limits]` in `devices/*.toml`: modules, machine_rows, arena_bytes,
stage_bytes) into the runtime's `device.rs`. The patchkit tests fail on a stale file. `runtime_min` (header byte 0x30) is informational;
a module is refused only on a contract mismatch (`E_CONTRACT` -48: rebuild the module). The runtime refuses further loads when its
module table is inconsistent (`E_TABLE` -49). `patchkit pack` and `dkm-info` print a `size` line by section. In a SHARC
include the note of each constant is on the line before its `#define`: a trailing comment would become part of the macro.

Example (pluck, Digitakt II): `R1 = FRAME_BASE;` and `R0 = R13 + PLUCK_DECAY_FRAME_OFF;` instead of `0x6d` and `1`.
`modules/core/adapters/dsp-voice/tests/test_pk_gen_include.py` shows that this assembles to the committed object, byte for byte.

### `[[section]]`

Bytes copied from the user's image when the patch is applied, optionally extended with one row per registry entry,
then placed in a region. The firmware table is never stored in the repository.

| Key | Meaning |
|---|---|
| `name` | section name; the symbol `<name>` is its address and `<name>.size` its length |
| `region` | where to place it |
| `from` | `{ anchor, offset = 0, len = EXPR, expect_sha256 }`; `len` defaults to the rest of the anchor; `expect_sha256` is needed when the range reaches outside the anchor |
| `append` | `{ registry, row = [EXPR, ...] }`: for each entry append the row; a word is `EXPR` (32-bit), `u8:EXPR` or `u16:EXPR`; `$field` is an entry field and `$index` its index |
| `edit` | `{ registry, at = EXPR, fields = [{ offset, value = EXPR }, ...] }`: for each entry rewrite words of one copied row in place: `at` is the row's byte offset in the copied bytes, `offset` a word's offset in the row, `value` as a word of `append` (core/params `param_edits`) |
| `align` | bytes, default 4 |
| `devices` | optional device filter |

```toml
[[section]]
name = "mf_names"
region = "cf.expansion"
from = { anchor = "cf.machine.data.name.table", len = "(image.u8(cf.machine.fn.dispatch + 1) + 1) * 12" }
append = { registry = "machines", row = ["$name", "$short", "image.u32(cf.machine.data.name.table + 12 * $clone_of + 8)"] }
```

### Expressions

Allowed wherever a number is accepted (`value`, `len`, `first`, section rows, field values): integers (decimal or
`0x`), symbols, `+ - * / & | << >>`, parentheses, and `image.u8(ANCHOR + OFF)`, `image.u16(...)`, `image.u32(...)`:
a read from the user's image at patch time. A read must lie inside the anchor (its SHA-256 is checked), and the
CPU of the anchor sets the byte order. `first`, `len` and table sizes must be static: anchors and image reads only.

## Tests

Module tests live in `tests/*.toml` and run in the smoke matrix for every module set that contains the module.

```toml
[test]
name = "audible"            # unique within the module
devices = ["dt2", "dn2"]    # which devices
quick = true                # also run by `module:dev` after every edit

[[step]]
tap = "TRIG 1"              # one action per step
[[step]]
wait_ms = 400
[[step]]
expect_pcm_db = { min = -60, max = -3 }
[[step]]
expect_no_fault = true
```

Actions: `tap` (a button name or a chord list), `hold` + `ms`, `turn` + `by` (+ `events` for a burst), `wait_ms`;
checks: `expect_frame` (a name; the golden is the frame's SHA-256, `differs_from` compares with another name),
`expect_pcm_db`, `expect_no_fault`, `expect_mem = { addr, mask, eq }` (a ColdFire word), `expect_dsp = { addr, hex }`,
`stress = { seed, actions }`; `device = "dt2"` on a step limits it. Every input also checks fault, DSP halt and
main-loop stall. The complete list is the docstring of `tools/patchkit/pk_harness.py`.

Workflow:

1. Copy a similar patched-in module (`tests/fixtures/modules/patched-pluck`) and fill in the manifest and the sources.
2. `mise run module:dev -- --modules category/name`: edit, see one summary line per save.
3. Add module tests; a `smoke.toml` like the one of `modules/core/adapters/fx` only checks that the patched firmware is audible and does not fault.
4. Add a cell to `tests/patchkit/smoke/matrix.toml` for each device and firmware you support, then
   `mise run patch:test -- --cell "dt2-1.17:category/name" --update` records the frame goldens (hashes only) once,
   and later runs compare against them. A change that is invisible on screen must give the stock frames.
5. A module that changes sound needs a proof against a reference (PCM level, pitch, bit-identity at the neutral
   setting). The proofs are `tools/patchkit/pk_proof_*.py`; write one the same way and report numbers in a finding.
6. `mise run module:build -- --check` must pass (the built objects equal fresh builds; only `core/*` objects are committed).

## Worked example 1: `core/fx` (SHARC+ only, both devices)

`modules/core/adapters/fx/` runs the effect chain on the final stereo block: the master block (64 floats) goes through the chain
before the firmware packs it for USB and converts it to the 24-bit output. With an empty chain the audio is the stock audio.

- **Object.** `src/dsp/fx.s` is placed in `dsp.l2`. `[object.externs]` maps its undefined symbols: the rows of the `fx` registry
  (`fx.rows`), the master float buffer (`dsp.master.value.q31_converter.float_buf`) and the tail of the hook
  (`hook.dsp.fx.fn.entry.tail`).
- **Hook.** One `hook` op (`hook = "dsp.fx.hook.entry"`, declared mode `before`) replaces the first instruction of the function that the
  block orchestrator calls once per block with a jump to `_fx_entry`. The displaced instruction runs in the hook's tail, which
  jumps back to the second instruction of the stock function. The header of the source lists the registers that are free at this site
  ([finding 55](../docs/findings/55-fx-adapter.md)).
- **Claim.** `dsp.fx.hook`, so that no other module can take the same site.
- **Anchor rules.** One `sharc_func` rule per device (`devices/fw/dsp/fx.toml`), found by the relocation-insensitive hash of the
  function. The object is the same on the Digitakt II and the Digitone II.
- **Test.** `tests/smoke.toml`: expect audible output and no fault.

```sh
mise run module:build -- modules/core/adapters/fx --check
mise run patch -- --firmware firmware/Digitakt_II_OS1.17.syx --modules core/fx -o out/patch/fx.syx
mise run patch:explain -- --firmware firmware/Digitakt_II_OS1.17.syx --modules core/fx
mise run module:dev -- --modules core/fx
mise run patch:test -- --cell "dt2-1.17:core/fx"
```

## Worked example 2: `test/patched-pluck` (both CPUs, built on frameworks)

`tests/fixtures/modules/patched-pluck/` adds a PLUCK machine (Karplus-Strong string) to the Digitakt II. It is the patched-in twin of the
run-time module `digikit/pluck` (`modules/machines/pluck/`), which builds from the same DSP source. It needs no ColdFire code
of its own: three frameworks do the firmware work, and the module only declares data.
The machine is declared in C (`src/module.c`: `DK_MACHINE`, `DK_PARAM`, `DK_PAGE`); see "Machine rows" above.

- **`requires = ["core/dsp-voice", "core/machines", "core/params"]`** and `ports = ["dt2-1.17", "dn2-1.12"]`. Patch it
  with all four modules: `--modules core/dsp-voice,core/machines,core/params,test/patched-pluck`.
- **The DSP voice.** `[[object]]` places `../../../../modules/machines/pluck/src/dsp/pluck.s` in `dsp.l2` (`devices = ["dt2"]`). An `[[entry]]` in the
  `voices` registry of `core/dsp-voice` gives the machine type (`machines.pluck.index`) and the routine (`_pk_render`).
  The framework hooks the sample renderer once; a track whose type is registered renders with the routine, every
  other type renders as stock. (`DK_MACHINE` names the routine; the lowering produces the row.)
- **The machine.** The `DK_MACHINE`, `DK_PARAM` and `DK_PAGE` records of the module (see "Machine rows" above) lower into
  these rows. The machine row in the `machines` registry of `core/machines` has `name` and `short` (list and page
  header), `clone_of = 0` (take SAMPLE's permission, per-type byte and DSP selector), and `f0` to `f8`, the nine
  descriptor fields that choose which parameter occupies each knob of the SRC page. Fields are stock descriptor ids
  (`f0 = 0xca` is TUNE) or `"params.<id>.index"` for a parameter this module adds. The voice row in `voices` gives the type
  (`machines.pluck.index`) and the render routine (`_pk_render`).
- **The parameters.** Four rows in the `params` registry of `core/params`: `name`, `short`, a `template`
  stock row to copy the formatter and value class from (the profile's `plain` row), `slot` (the engine slot the value travels in), `min`, `max`
  and `default`.
- **No numbers of the firmware's types.** The machine's type number is permanent: its row in
  `modules/core/adapters/machines/types.toml`, added by hand. The
  parameter indices are assigned in canonical module order. The voice row and the machine row both name `machines.pluck.index`, so they always agree.
- **Tests.** `tests/select.toml` opens the machine list, selects PLUCK, checks the committed type, the SRC page frame,
  that a trig sounds, that the DSP knows the type and that turning a knob changes the page. `tests/smoke.toml` checks
  that the module leaves the stock voice alone.
- **Gallery.** The README and the icon of the machine are in `modules/machines/pluck/` (the knobs, and what projects saved with PLUCK do on stock firmware).

### An effect: `[[entry]] registry = "fx"`

An effect module requires `core/fx` (the effects adapter, `modules/core/adapters/fx/`) and adds one row to its registry `fx`; the chain runs in module order, at most
`DK_FX_MAX` (4) effects. The routine, its `dk_fx_desc` and its state block are symbols of the module's DSP object (`<dk/fx/fx.h>`,
[docs/MODULE-AUTHORING.md](../docs/MODULE-AUTHORING.md), section 7.2):

```toml
[[entry]]
registry = "fx"
id = "gain"
pc = "_fg_process"       # process(R4 = state, R8 = dk_fx_in, R12 = parameter words)
desc = "_fg_desc"        # dk_fx_desc
state = "_fg_state"      # .PKBSS _fg_state, <state_bytes>;
```

`tests/fixtures/modules/fx-gain` (0.5 gain) is the example and the fixture of `core/fx`'s tests; `fx-nan` is the negative one. An empty chain plays the stock audio.

A run-time effect (`dynamic = true`, a `.dkm`) declares a `DK_FX` record in C (`modules/fx/freqshift/src/module.c`)
instead of the `[[entry]]` row. It takes one of the 3 free rows that `core/dynfx`
reserves, and `core/fxpages` reserves the pages. Its manifest, pages and routes are in [docs/MODULE-AUTHORING.md](../docs/MODULE-AUTHORING.md),
sections 5.4 and 7.2. The format is `docs/design/digikit-dkm.md`; the design of the pages is "Effect pages" in `docs/design/digikit-registry.md`.
Example and fixture: `tests/fixtures/modules/dyn-fx-gain`.

The frameworks themselves are ordinary modules: `modules/core/adapters/machines/` (about 24 ColdFire anchors, six copied
tables, the hooks in `src/cf/dt2/machines.s` and `src/cf/dn2/machines.s`), `modules/core/adapters/params/` and
`modules/core/adapters/dsp-voice/`. Their routines are unit-tested on the emulated ColdFire core (`tools/patchkit/pk_cfunit.py`,
`modules/core/adapters/machines/tests/test_pk_machines_fw.py`, `modules/core/adapters/params/tests/test_pk_params_fw.py`), and the proofs are `tools/patchkit/pk_proof_machines.py` and
`tools/patchkit/pk_proof_pluck.py`. Contract and evidence: [finding 02](../docs/findings/02-machines-and-parameters.md).
