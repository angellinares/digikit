# The run-time registry: machines and parameters added by a loaded module

Status: built for Digitakt II 1.16 and 1.17 (2026-10-16) and Digitone II 1.11 and 1.12 (2026-10-17; pages, `own_dsp`, finding 33), ColdFire side only, emulator only. Plan: `docs/plans/digikit-runtime.md`
(Phase 5, ColdFire side). **Since the Phase 6 archive commit the rows are filled by the boot-time load only**: the unload, the in-use refusal and the machine-view hook and rebuild described for hot reload below are archived (docs/plans/digikit-hot-reload.md, tag `digikit-hot-reload-v1`); the paragraphs that describe them are marked. Results and evidence: `docs/findings/30-runtime-machines.md` (Digitakt II) and `docs/findings/33-runtime-machines-dn2.md` (Digitone II, unload refusal). File: `docs/design/digikit-dkm.md`. Runtime:
`docs/design/digikit-runtime.md`. Code: `modules/core/adapters/dynreg` (patch time), `modules/core/runtime/src/rs/src/registry.rs` (run time),
`packages/patchkit/src/register.rs` (records, packer).

Note (2026-10-27): the typed manifest sections named below (`[[machine]]`, `[[param]]`, `[[voice]]`, `[[page]]`) are replaced by `DK_MACHINE`,
`DK_PARAM` and `DK_PAGE` in C (`sdk/include/dk/machine/machine.h`). The registry records and rows are unchanged. `clone_of` is fixed by the device
profile and not written by authors.

## What it does

A `.dkm` carries typed machine and parameter records in its `register` section. The boot-time load checks them and writes them into rows that
`core/dynreg` reserved at patch time, before the firmware builds its machine view; the machine is then in the SRC machine list and can be put on a track. (`registry::remove` still gives the rows back when the DSP part of a boot-time load fails.)
Nothing is patched at run time: no firmware code is written, only rows, tables and one pointer.

## Records (the `register` section)

Built by `patchkit pack MODULE --device dt2` from the module's typed sections (`[[machine]]`, `[[param]]`; `typed.rs` resolves them with the
device's knob model), for a module with `dynamic = true`. Big endian; format and checks in `packages/patchkit/src/register.rs`:

    0x00 magic "DKRG"   0x04 version 1   0x08 device (1 dt2, 2 dn2)   0x0c record count (at most 32)
    0x10 records: u32 kind, u32 length, body
      kind 1 PARAM   (0x50): u32 index, template, slot; i32 min, max, default; name[32]; short[16]
      kind 2 MACHINE (0x6c): u32 type, clone_of, position, own_dsp, level; u32 knobs[8]; name[24]; short[24]
      kind 3 PAGE    (0x60): u32 type, knobs[8], level; name[24]; title[24]            (Digitone II only)
      kind 5 ACTION  (0x10): u32 param (index of a PARAM record of the file), offset (image offset of the click callback)
      kind 4 EFFECT  (0x1a4): u32 fx, mix, n_knobs, id; tab[8]; title[24]; 7 x (u32 position, display; f32 min, max, default; name[24]; short[8])   (Digitakt II)

- ACTION: `DK_PARAM_ACTION` of a machine's parameter (docs/design/machine-page-click.md). `param` is the index of a PARAM record of the same file (at most one ACTION per parameter), `offset` the image offset of
  `void fn(u32 track)` (inside the image); the runtime stores `load address + offset` for the parameter's row and calls it when the knob is clicked on the machine's page. It takes no row. A runtime
  that does not know the kind refuses the file (`E_FORMAT`).
- MACHINE `type` is the machine **key** (since sticky ids, 2026-10-20, docs/design/module-sdk.md 11.5 D7): `hash24 << 8 | hint`. The hash is the low 24 bits of FNV-1a 32 over the
  UTF-8 string `<module id>:<machine id>` (the machine's global id; `register.rs` `machine_hash`); the low byte is 0 ("assign a type at load"; there is no type hint in the manifest). The runtime
  gives the machine a number in `dynamic_first ..= last` (Digitakt II and Digitone II 16 to 23) and remembers it on the card (below), so a project that stores the number
  keeps its machine while the map holds the hash. A key with hash 0 and a low byte of 16 to 23 pins that number (hand-made files and tests; it is in no map and the loader
  refuses it when another machine of the boot has it). A PAGE's `type` and a DSP voice's type are the key of their machine (the DSP section's version is unchanged: the
  field was a type number, it is now a key). No table in the repository has the numbers any more: `[<dev>.dynamic]` of `types.toml` is gone.
- `knobs[i]` (Digitakt II: the descriptor fields f0 to f7 of the machine row): a stock parameter code, or `0x80000000 | j` for PARAM record `j` of the same
  file (the runtime replaces it by the code it allocated). `level` is the ninth field (f8, the level code).
- `clone_of` is a stock type (below `machines.first`): the permission long, the per-type byte and the name hint of that type are copied into the
  tables. `position` must be 255: a run-time machine is listed after the built-in ones, in type order (the list order is fixed at patch time, below).
- PARAM: `template` is a stock code whose row supplies owner, flag, formatter and the value-class object; `slot` is the knob position (0 to 7).
  A PARAM that no knob names is allowed (it only takes a row).
- PAGE (Digitone II): a parameter page of the machine with `type` (a machine of the same file). `knobs[i]` is `0x80000000 | j` (PARAM record `j`) or 0; the
  sound-array slot of knob `i` is the device's slot base (25) plus `i`, which is also the slot the PARAM record carries (`typed.rs` derives both from the
  knob position). `level` is the ninth column (the track level code, 10). `name` is the short page name (SYN page tab), `title` the title line. A machine
  record of a device that keeps its knobs in pages carries knobs 0. At most 8 pages per file and 8 at a time.
- `own_dsp` (MACHINE, Digitone II): 1 tells the frame builder to send the machine's own type to the DSP (the FM engine renders nothing for it); on the
  Digitakt II it must be 0. The DSP routine that renders the type is not part of this section (lane A of the plan).
- EFFECT (Digitakt II): the page of a run-time effect, see "Effect pages" below. `fx` is the index of the effect's record in the file's DSP section (version 4), `mix` the default of the MIX
  knob (0 to 127), `id` the effect's 16-bit ID (1 to 0xffff; the packer's `register::effect_id` of module id and effect id; the kit stores it with the values), `tab` the page's tab label (1 to 3 characters), `title` the page title (1 to 23), then 7 knob entries of which the first `n_knobs` are used: knob i is parameter word p_i
  of the effect's row, shown at `position` (0 to 7, distinct) with `display` 0 (0 .. 127) or 1 (-64 .. +63), `min`, `max` and `default` are IEEE single floats (`min < max`, the default
  inside), `name` the long name shown while the knob turns (1 to 23 characters), `short` the label under the knob (1 to 6). At most 4 per file, one for each effect; a file with one
  needs `core/fxpages` (`E_NO_REGISTRY` otherwise). The packer writes it from a `DK_FX` record with `tab` (`sdk/include/dk/fx/fx.h`; formerly `[[fx]]`) (`docs/design/module-sdk.md`, "Define an effect with its own page").
- Not carried yet: DSP code and `[[voice]]` records. The packer refuses a voice; the runtime refuses any record it does not know (`E_FORMAT`).
- `patchkit pack` of a module without code writes a four-byte image (`moveq #0,d0; rts` as `dkm_init`) so that the loader's rules stay as they are.

## Machine keys and sticky type numbers (D7)

`assign.rs` of the runtime (`plan`, before the first module of a boot loads) reads the machine keys of every selected module, in load order, and gives each machine a number; the
loader, the registry and the DSP loader then take the number from `assign::resolve(key)` (the module's selection index and the key). The card remembers numbers in the **map** in the
reserved bytes of the selection record (`docs/design/digikit-store.md`): 8 entries, each `hash24 << 8 | type`, 0 = empty. For each machine of the boot, in order:

1. the map has an entry for its hash and that type is not taken in this boot: use it (`MACHINE_TYPE_ASSIGNED`, sticky 1);
2. else the lowest type in 16..23 that no map entry references and this boot has not given out; the entry is recorded (sticky 0);
3. else drop the lowest-index entry whose hash is not a machine of this boot (`MACHINE_TYPE_EVICTED`, an error event, so it is kept), take its type, record the entry in its slot;
4. two machines with the same key in one boot (a hash collision, or a file twice under two names): the second takes a fresh number that is not recorded.

The boot hook writes the record once after the loads (the boot counter's single-copy `update`, seq + 1) and only when the map changed; `SELECT` and the menu's save keep the map
bytes. A card made by the store builder has a zero map, so a fresh card gets 16, 17, ... in load order. `DKRT_ASSIGN` (peekable) holds the boot's table; `dkupload modules`
and the MODULES command show each module's assigned types. Scenario: `sticky-types`.

## Reserved rows (patch time, `core/dynreg`)

`core/dynreg` requires `core/runtime`, `core/machines` and `core/params`. It adds ordinary registry entries (so nothing in the frameworks changes
and an image without `core/dynreg` is byte-identical to before):

| What | How many | Where | Looks like |
| --- | --- | --- | --- |
| machine rows | 8: types 16 to 23 (`machines.dyn16.index` ...) | `machines.rows` (64 bytes each) in `cf.expansion` | a gap row (clone of type 0, no name), but **listed** (position 255) |
| parameter rows | 16: codes `params.dp0.index` ... `dp15` (`params.first` + 0 ...) | `pf_table` (60 bytes) and `params.rows` | a copy of stock row 0xd1 (Digitone II: 0xd5), empty name |
| page rows (Digitone II) | 8: `pages.dpg0.index` ... `dpg7` | `pages.rows` (84 bytes each) | empty name and title, no knobs, level 10, and a `machine` no type has (0x7fffffff; not -1, which a track without a sound reads) |

So the machine tables span types 7 to 23 (`machines.count` 17): `mf_names`, `mf_permit`, `mf_pertype`, the DSP selector block, the persistence bound
(`8 + N`), the name, permission and per-type bounds. The run-time type numbers 16 to 23 are the range of `types.toml` (`dynamic_first = 16`; `core/dynreg:dyn16 ...
dyn23` hold the reservation); a patched machine never takes one, and the runtime picks a number from the range for each run-time machine at load. Numbers 10 to 15 are plain gap rows.

`core/dynreg` exports `dynreg_info`, a table of the addresses and layouts the registry needs (`dynreg.s`, 264 bytes since version 2): the machine rows and their
field offsets, the name, permission and per-type tables, the parameter table and registry rows, the first reserved type and parameter code, how to
build a parameter row (a spec of 15 words: copy template word n, or min, max, default, name, short, mirror index, kind word, 0x80 + slot), the knob
count, and from byte 200 the page rows and their layout and the value of a free page row. Words 192, 196 and 244 to 256 (the view variable and builder, the three firmware functions of the unload refusal and the track count) are 0 since the archive commit; the offsets stay. The runtime links to it with `[object] optional = ["dynreg_info"]` (patchkit resolves an optional
undefined symbol to 0 when no selected module defines it); without `core/dynreg` a file with `register` records is refused (`E_NO_REGISTRY`) and a
file without records loads as before. The runtime's code names no device: the layouts are data.

## What is derived state, and how it is kept right

The machine framework builds tables lazily (`mf_ensure`: list order, rank pairs, std::string representations of the upper-cased names) and the firmware
builds more from them. What was found, and what the registry does (evidence in finding 30):

| State | Who reads it | What the registry does |
| --- | --- | --- |
| machine row (`f0..f8`, `clone_of`, `position`, name pointers, `O_LONG`/`O_SHORT` representations) | `mf_dispatch` (descriptor of a type), the clone tests, `typearg` | writes the fields; builds the two std::string representations itself in its own state (length, capacity, reference count -1, upper-cased characters); the name pointer is the last store |
| `mf_names[t]` {long, short, hint}, `mf_permit[t]`, `mf_pertype[t]` | name accessors, permission gate, per-type lookups | long and short point to strings in the registry's state; permission and per-type byte copied from the clone's entry, the hint likewise |
| `mf_list_buf`, rank pairs, rank map, static sorted list | the list getter `FUN_40052a08` (static vector sorted once at first use) | nothing: the reserved types are in all of them from patch time, in type order, at the end |
| the list the getter returns | the machine selection view | `dynreg_copy` (retargets the `std::copy` at the end of the getter) leaves out the types whose row has an empty name |
| the machine selection view's item list | the SRC machine list | **the firmware builds it once at boot and keeps it.** The boot hook runs before the view exists, so the stock builder (`FUN_40060f50`) reads the rows. (Hot reload had `dynreg_view_hook` remember the view and the registry call the builder again after a change: archived.) |
| parameter row (60 bytes) in `pf_table`, `params.rows` row | the parameter accessors, `pf_t2` (value-class object by template) | written from the spec: template words, min, max, default, name and short pointers, mirror index, kind word, 0x80 + slot |
| page row (84 bytes) in `pages.rows` (Digitone II) | `mf_typemap0/1/2` (page count, descriptor, slot -> code), `mf_reader` (a machine with pages is its own type for every caller) | written from the PAGE record: name and title as std::string representations (not upper-cased, in the registry's state), nine codes, nine slots, `machine` last (the page belongs to the type from that store on) |
| the DSP's type -> selector table (Digitakt II) | the DSP | not touched: it is part of the DSP image (reserved types select as type 0). Open |

The defaults refresh and the other loops over parameter codes keep the stock count (core/params), so a new code does not enter them: the same as for
a patched machine.

## Writing, publishing, and what must hold

All of it runs in the main-OS task at boot (`dkrt_boot_hook`) or, for the DSP part's failure path, in the UI task, and the
machine list, the descriptor dispatch and the parameter accessors are the UI task's. There is no second reader.

1. **Check before any write** (`registry::check`, from the loader's check phase): header, device, record kinds and lengths, text fields, parameter
   slot and template, knob references, machine type in the reserved range and free (owner none and name empty: `E_TYPE_USED`), `clone_of` a
   built-in type, enough free reserved parameter rows (`E_PARAM_FULL`). A refused file changes no byte of any row or table (checked).
2. **Order** (`registry::apply`, before `init`): parameter rows (they carry no visibility), then the page rows (their machine is not listed yet), then per machine: the row fields, the string representations,
   `mf_names`, permission, per-type byte, owner, and last the **name pointer, one aligned 32-bit store**: the row counts as used from that store
   on (the list filter and the `E_TYPE_USED` test read it). (Hot reload then rebuilt the list the view keeps: archived.)
3. **Take-down** (`registry::remove`, after `fini`; at boot only for a failed DSP part): the name pointer first (the row is hidden), then the row and tables, then the page rows (`machine` first),
   then the parameter rows are reset to the reserved values. Strings and representations live in the registry's state (about 2 KB of the runtime's bss), never in
   the arena, so no pointer in a row can dangle.
4. **Refusal of an unload** (`registry::in_use`, the three firmware functions `cf.dynreg.project`, `kit_of`, `track_type`): archived with hot reload. There is no unload, so no
   track can be left on a type with no row. The anchors and their port entries are removed; docs/plans/digikit-hot-reload.md lists them.
5. A project (or a track) that names a type whose row is free behaves as on a gap row: the type byte stays; the row is a clone of type 0 with no knobs
   (the SRC page shows empty boxes); the permission, per-type byte and the DSP selector are type 0's.

## Effect pages (Digitakt II, F4 step 2)

`core/fxpages` (patch time) and `fxpage.rs` (run time) give each run-time effect row of `core/dynfx` a page after CHO in the FX view; DEL, REV, CHO and then the pages of the loaded effects in
effect-row order. Facts of the firmware and the measurements are in `docs/findings/61-fx-pages.md`; this is the design.

- **Reserved resources** (patch time): one page row for each effect slot (page numbers `[fx] page_first` and up, answered by a hook in the page accessor), the list the FX view reads its pages from
  (a hook in the view builder), the tab label and graph hooks, and 24 parameter rows (`params.fp0` ..): effect row k owns the rows `8k .. 8k + 7`, seven knobs and MIX. `fxpages_info` is the table
  the runtime reads (rows, list, anchors of the stock list, the first parameter code, the kit pointer and array offset, the template codes and the value indices, all device-file facts: `[fx]`).
- **What an EFFECT record makes** (`fxpage::apply`, in `registry::apply` after the machines, when `dsp::reserve` has claimed the effect's chain row k): the parameter rows `8k + j` from the
  knobs (template by display: `template_percent` or `template_bipolar`, range 0 to 0x7f00, default `round((default - min) / (max - min) * 0x7f00)`, names; mirror index `value_indices[8k + j]`), MIX
  as row `8k + 7` (template `template_mix`, name "Mix", short "MIX", default `mix * 256`), the page row (tab, title, the knob codes at their positions, MIX on the level slot), then the page list and its
  count (stock pages first). `remove` takes the page out of the list first, then resets the rows (the same order as machines). Rows are claimed with the effect row, so a page needs no row of
  its own in CAPACITY: it is counted by the effect slot (`FX_SLOTS`).
- **Values.** The firmware's page code reads and writes the kit's array of s16 values at the rows' mirror index, so the display, the turn and the long name (`Gain=98`) are the firmware's. The kit's array holds the defaults
  from the initialiser hook (or the stored values from the loader hook); the runtime polls the array once per UI-loop pass and
  sends a changed value to the row in the DSP: knob j as the float `min + (max - min) * raw / 0x7f00` into p_j, MIX as `raw / 0x7f00` into the row's mix word, about 3 ms after a turn (one word per
  transfer, only when no module load is in progress). `core/fx` applies the mix as the dry/wet blend (finding 55): `master = dry + mix * (wet - dry)`; the effect writes only the wet signal.
- **Persistence** (step 3, finding 61 "Persistence"): three hooks of `core/fxpages` on the firmware's kit initialiser, loader and saver of the effect value array. The saver writes the 24 values and one effect ID per row
  into the 28 spare halfwords of the kit's storage record (`[fx] store_off`, `store_halfwords`: stock ignores them, so a kit with effects loads on stock firmware); the loader gives a row the stored values when the stored ID is
  the ID of the effect now in the row (`fxp_ids`, written by `fxpage::apply` from the record) and the record's defaults otherwise (`fxp_defs`); the initialiser (a new kit) gives the defaults. All three run
  inside the firmware's own call, so the poll never sees a zero. The tick no longer re-seeds a new kit object; it only treats a row of all-zero values as untouched.
- **Limits.** 3 effects on the Digitakt II (`fx_slots`), each with up to 7 knobs and MIX; 1 to 3 characters in the tab, 23 in the title, 6 under a knob; no p-locks, no LFO or other modulation on the
  knobs, no MIDI CC (the rows carry the template's kind and key words, so the firmware's modulation and CC maps would alias a stock parameter: not supported); the values are saved with the kit (above); a knob that is not listed has an empty cell. An image without `core/fxpages` refuses a module with an EFFECT record; a module without one loads on every image.

## Digitone II

Same rows and order, with differences that are data in `dynreg_info` or code in `core/machines` already:

- The machine's knobs are in its page rows (`pages`), read by the type maps; the machine row's `f0..f8` fields are not used for knobs (the PAGE record carries them).
- No per-type byte table and no clone tests; the descriptor of a machine is its row; `own_dsp` is the row's `O_OWN`.
- The parameter table is the same 60-byte row; the mirror index is the sound-array slot itself (spec word 0x108 of the lowering spec) and the template is 0xd5 (A Env Decay).
- The list getter (`FUN_4004d8b6`, 1.11 and 1.12) copies its static list into the returned vector through `jsr (A3)` (a library `std::copy` held in a register, not a `jsr abs`):
  the six bytes `jsr (A3); move.l D0,(4,A2)` at +0xde are replaced by a hook (`dynreg_list_hook`: `jsr dynreg_copy; move.l d0,4(a2)`; the displaced bytes are dropped).
  `dynreg_copy` is the same filter as on the Digitakt II.
- The machine selection view's item builder is `FUN_4005b2c0(view)`, the same function (same `h_strict` hash) as the Digitakt II's `FUN_40060f50`; the view is built once at
  boot and keeps its list; the boot hook runs before it exists, so nothing rebuilds it.
- The list order is that of `mf_list_buf` (stock order, then the machines at their positions; the reserved rows at the end, after MIDI); the group separator shows between
  MIDI and the first run-time machine.

## Limits and open items

- 8 machines, 16 parameters and (Digitone II) 8 pages at a time (numbers are in `dynreg.s` and `module.toml`; more costs 64 + about 90 bytes per type, 60 + 28 per parameter, 84 per page).
- The machine plays through the DSP as type 0 (SAMPLE) on the Digitakt II: the selector word of a reserved type is in the DSP image. Options: reserved rows with a fixed
  selector (a slice clone gets 5) refused for a clone of another type; or the Phase 5 SHARC side loads the selector words. A machine that is a
  data-only clone of SLICE is therefore not audio-complete yet. On the Digitone II an `own_dsp` machine renders silence until a DSP routine for its type is loaded (lane A).
- `E_*` codes of the registry (-20 to -26) are in `dkrt-sdk` (`modules/core/runtime/sdk/src/lib.rs`); -30 to -39 belong to the DSP side. `result_text` has no text for them yet.
