# 57. Capacity and isolation of run-time modules (DT2 1.17, DN2 1.12)

Plan: `docs/plans/capacity-isolation.md` (PR 1: steps 1 to 5, merged; PR 2, step 6: the CAPACITY command, runtime 0.2.2, built). Marks as in AGENTS.md: **[D]** measured or read once here,
**[O]** open, **[C]** corrects an earlier claim; nothing is **[V]** (a second agent has not checked the sets against the image bytes). Emulator only; the device has
not run any of it. Pieces: `tools/patchkit/pk_capacity.py` (the sets), `tests/regression/runtime/synth.ts` (the TypeScript spec builder), `packages/patchkit/src/dkmsynth.rs`
(`patchkit synth-dkm`), the device tests `modules/core/runtime/tests/capacity/capacity.device.test.ts` (capacity and isolation of the reference's DSP bytes) and `modules/core/runtime/tests/capacity/inferred/inferred-needs.device.test.ts` (inferred needs, full tier), scenarios `memaudit-*-full` in `tests/digikit/scenarios/`, `tests/patchkit/test_capacity_sets.py`,
`tests/patchkit/test_limits.py`.

## Result

**[D]** Every limit of the runtime was filled on both devices and the next module was refused with the limit's own code. Modules already loaded kept their
ranges, rows, DSP bytes and sound. Nothing was found wrong in the loader.

| Limit | DT2 | DN2 | Case (ColdFire-only boot unless noted) | Refusal seen |
| --- | --- | --- | --- | --- |
| module records | 8 | 8 | 8 raw modules load; SELECT of 9 paths | `E_SEL_COUNT` (-41); a ninth module never reaches the loader |
| staging buffer | 64 KiB | 64 KiB | a file of exactly 65 536 bytes loads; 65 540 bytes | `E_TOO_BIG` (-13) |
| repeated name | | | a second module of a loaded name | `E_LOADED` (-10) |
| ColdFire arena | 512 KiB | 512 KiB | R (32-byte block) and F take every byte (free 0, largest 0); then a raw module, a module with a DSP section, a bss as large as the arena | `E_NOMEM` (-8) three times (the DSP module's refusal is at `dsp::reserve`: the arena block for its image copy) |
| bss above the arena | | | bss = arena + 4 | `E_FORMAT` (-5), not `E_NOMEM` |
| DSP run-time area | 512 KiB | 320 KiB | R and A fill it to the last whole granule; then one granule more, then an extent above the area | `E_DSP_NOSPACE` (-33), at `reserve` and at `check` |
| voice rows | 8 | 8 | one module with 8 pinned voices, then 1 more; the same split 7 + 1, then 1 more | `E_DSP_ROWS` (-34) (the row count is checked before the type) |
| voice type | | | a type used by a loaded module; a type outside 16..23 (rows free) | `E_DSP_TYPE` (-37) |
| voices in one section | 8 | 8 | a section with 9 voices | `E_DSP_FORMAT` (-31) |
| machine types | 8 | 8 | one module with 8 hashed machines, then 1 more | `E_TYPE_RANGE` (-22) |
| parameter rows | 16 | 16 | one module with 16 parameters, then 1 more | `E_PARAM_FULL` (-24) |
| page rows | 0 | 8 | one module with 8 pages, then 1 more | `E_PAGE_FULL` (-25) (DN2 only) |

After every case `DKRT_TABLE_BAD` is 0 and no `TABLE_BAD` event was logged, MODULES lists exactly the loaded modules in order, the ColdFire blocks lie in the arena and
the DSP ranges in the area, and all ranges are disjoint. A refused module left no range, row or owner behind (the table check would have failed otherwise).

### Which code a limit gives, and why **[D]**

- `E_TABLE_FULL` (-14) cannot be reached: the selection holds `SEL_MAX` = 8 paths and there are 8 module records. SELECT refuses a ninth path (`E_SEL_COUNT`), and the
  mailbox hook answered `E_SEL_PATH` (-40) for the same case until runtime 0.2.1 (a test hook); since 0.2.2 it answers `E_SEL_COUNT` too. `E_TABLE_FULL` stays a defence; a device that sets `modules` below 8 would
  reach it. The runner asserts `modules <= SEL_MAX`.
- Eight machine types, eight machine rows and eight voice rows are one number. The type assignment gives out numbers before the DSP checks run, so a ninth hashed machine is
  `E_TYPE_RANGE`, not `E_DSP_ROWS`. `E_DSP_ROWS` and `E_DSP_TYPE` are reached by voices with a pinned type number (a DSP-only module).
- `bss` is not stored in a `.dkm`, so a file of 200 bytes fills the arena or the DSP area. The sets use no fixture folder: `patchkit synth-dkm` builds each module from a
  TOML spec and `pk_capacity.py` computes the sizes from the device file, `types.toml` and the plan's `DKRT_ARENA` and `_dl_area` addresses.

### The DSP area is handed out in granules **[D]**

The first range of a module with `align = 8` starts at the first 8-aligned byte of `_dl_area`, which is offset 4 on both devices (the area's base is not 8-aligned). Ranges are
whole 64-byte granules. The area is therefore full when less than one granule is left: on both devices R takes 4..68 and A the whole granules behind it (DT2 68 + 524 160,
DN2 68 + 327 552), and 60 bytes stay unusable. The `dsp_free` figure of MODULES (64) counts that gap and the 4 bytes in front, so "free 0" never happens: the runner checks
"largest free range under one granule". Finding 56 and the plan assumed an exact fill from offset 0 **[C]**.

### The full set and the isolation test **[D]**

`pk_capacity.full_set` puts behind a reference module every limit at once: M takes the machine types, parameter rows and page rows the reference leaves, V the voice rows
(pinned types behind the reference's), D the DSP area to its last whole granule, F the arena to its last byte (the arena copies of the DSP images counted: a ColdFire-only rig
keeps them), and three modules that do not fit (X `E_NOMEM`, Y `E_DSP_ROWS`, Z `E_DSP_NOSPACE`). That is 5 loaded modules and 3 refused; refused modules take no record, so the
full set does not fill the module table (the `modules` case does).

- `memaudit-dt2-full`, `memaudit-dn2-full`: the memory audit (finding 39 and docs/TESTING.md) over `test/dyn-demo` plus the full set: guard total 0 while the UI is driven, no
  store during the load of the modules and their init, arena free 0.
- `isolation-dt2`, `isolation-dn2`: two coupled boots, `test/sine` alone and `test/sine` with the full set behind it. In both: the reference's DSP bytes read back from L2
  equal its packed image relocated at its range, before and after the note (0 bytes differ); the reference note is audible. Across the boots: the MODULES record (size, DSP
  range, rows, types) is equal; the PCM is equal sample for sample from the onset (0 differ after alignment, 96 000 samples).
- The onset of the note moves between the two boots: 64 samples (one block) on the DT2, 128 (two blocks) on the DN2. The check aligns the two captures at their onsets and then
  requires 0 differing samples; the tolerance for the shift is raised from 128 to 256 for this check, because 128 was exactly the old limit. The mechanism is the one of
  findings 32 and 37 (an onset shift of one block between two images whose ColdFire work before the key differs): the full boot loads 5 modules and refuses 3 before the panel
  script runs, so the key reaches the sequencer in a later block. That the shift is two blocks on the DN2 and one on the DT2 was not investigated **[O]**.
- Control: there is none for the PCM and the readback (a clobber cannot be forced without editing the runtime). The ColdFire control is the existing `memaudit-*-stray`
  pair, which must report exactly one store.

## Limits in the profile **[D]**

`sdk/sdk.toml` `[limits.max]` holds the contract maxima (modules 8, map entries 8, effects 4, DSP area 524 288, parameter rows 16, page rows 8); `[limits]` of each device file
holds the device's numbers, now with `fx_slots` (4), `param_rows` (16) and `page_rows` (0 on the DT2, 8 on the DN2). `patchkit gen` refuses a value above its maximum and a
`machine_rows` that is not the run-time type range of `types.toml`; a registry may name a device limit (`core/fx`: `limit = "fx_slots"`), and `apply` refuses more effects than
the device allows. `pk_device.limits()` and `contract_max()` replace the copies in `pk_dkrt.py`, `pk_dkrt_boot.py` and `pk_conform.py` (a test fails when a literal copy
returns). In PR 1 the new keys were not emitted into the runtime, so no framework module changed; PR 2 emits `LIMIT_FX_SLOTS` (below).

## A gate gap closed **[C]**

No pytest file ran the Digitone II scenarios of the kind `debug`: `tests/digikit/test_debug_dt2.py` selects `dt2` only. `memaudit-dn2`, `memaudit-dn2-stray` and the DN2 case of
`debug-guard` (full tier) were never run by `verify` although docs/TESTING.md called them quick. `tests/digikit/test_debug_dn2.py` now runs them. Its quick-tier cost
(`DIGIKIT_PROFILE=1`, one file, 5 scenarios: `memaudit-dn2`, `memaudit-dn2-stray`, `capacity-dn2`, `memaudit-dn2-full`, `isolation-dn2`): 96 s in `test:changed` (a file of its own, so it runs beside the others); the scenario that existed before, `memaudit-dn2`, costs 24 s cold and 14 s warm (the stray control was not timed), the other three are new (cost table).

## Cost **[D]**

One run each with `DIGIKIT_PROFILE=1` and `DIGIKIT_RUN_SALT` (cold: new boots; warm: the same card and salt again), on a machine at load average about 25 to 30 (other lanes were
running), so absolute times are high and noisy.

| Scenario | Boots | Cold | Warm |
| --- | --- | --- | --- |
| `capacity-dt2` | 8, ColdFire-only, in threads | 18.6 s | 5.4 s |
| `capacity-dn2` | 9, ColdFire-only, in threads | 18.7 s | 5.3 s |
| `memaudit-dt2-full` | 2, ColdFire-only | 29.1 s | 16.2 s |
| `memaudit-dn2-full` | 2, ColdFire-only | 25.2 s | 14.3 s |
| `isolation-dt2` | 2, coupled | 78.0 s | 45.0 s |
| `isolation-dn2` | 2, coupled | 43.5 s | 31.8 s |
| for comparison: `memaudit-dt2` (3 modules) | 2 | 37.0 s | 25.7 s |
| for comparison: `memaudit-dn2` | 2 | 23.8 s | 13.6 s |

(Two further measurements failed for reasons of the shared machine, not of the scenarios: a rebuild of the PGO daemon removed its binary under a run, and a boot-state file of the cache
of another run vanished. Both were repeated alone, which gave the capacity rows above.)

`isolation-dt2` is over the plan's 60 s limit when its boots are cold (78 s), so it is in the **full** tier; `isolation-dn2` stays quick.

The cases of one scenario boot in threads (`Ctx.parallel`). In `test:changed` (cold and warm mixed, machine loaded), `test_debug_dt2.py` took 202 s (8 scenarios, three at a time) and `test_debug_dn2.py` 96 s (5 scenarios).

## Open **[O]**

- `isolation-dt2` is in the full tier (the plan's 60 s rule, cold boots 78 s); the DT2 quick tier covers the same set through `capacity-dt2` and `memaudit-dt2-full`. Em can move it back.
- Device-file routing: `devices/*.toml` falls through to the fallback rule of `verify` (build, desktop, pins, check, python-tests, smoke); every `[limits]` edit runs the whole
  gate. No `verify_route.py` change was made.
- A DSP transfer that fails (`E_DSP_XFER`) and a DSP that runs a module's code are not covered by `capacity-*` (ColdFire-only boots); the isolation scenarios run the transfers
  and play the reference only.
- The DN2 sender of `core/dsp-link` accepts offsets up to 0x80000 bytes while the DN2 area is 0x50000: the C constant is the protocol's 17-bit width (shared by both devices). The
  runtime's `alloc_range` and the DSP check the real size. PR 2 made the constant a generated shared value (below); the gap itself is unchanged.
- The Rust host client (`packages/dkupload`) has no MODULES or CAPACITY request, so it only gets the generated numbers: a CAPACITY request there is **[O]** (P5).
- `fx_slots` is a shared constant in the runtime (one image for both devices), so the two device files must give it the same value; a device that wants another number needs it in a device object. The runtime reports no run-time effects (used 0).
- Machine-name hash collisions, `enum` parameters and 16-voice polyphony are still untested (finding 56 section 9).

## Reporting and generation (PR 2, runtime 0.2.2) **[D]**

- **CAPACITY** (command 0x04, capability bit 6; `docs/design/digikit-upload.md`): `u8 version, u8 n`, then per resource `u8 kind, u32 limit, u32 used, u32 largest`. Eleven kinds: modules, selection paths, arena, staging buffer, DSP area, machine
  rows, voice rows, parameter rows, page rows, effect slots, sticky map entries. The numbers are in `sdk/sdk.toml` (`[protocol]`, `[capacity.kinds]`) and generated for the Python and Rust hosts. MODULES, `DkrtHealth` and the on-device page are
  unchanged. `Uploader.capacity()` and `mise run dk:store -- --emu info` print it (`capacity arena: 64/524288  largest free 522816`); a runtime without the bit prints "not reported".
- Example, DT2 with pluck and xslice loaded (`debug-log`): modules 2/8, selection 2/8, arena 64 of 524 288 (largest free 522 816), DSP area 265 600 of 524 288 (largest 258 684), machine rows 2/8, voice rows 1/8, parameter rows 5/16, page rows 0/0,
  map entries 2/8.
- **Checks.** `part_modules` (debug-log, no extra boot) asserts the capability, every limit against the device file, and the use against MODULES. Every `capacity-*` case compares the reply with MODULES, the profile and what the case filled (arena
  and DSP area used up, every voice, machine, parameter and page row counted).
- **Version.** Runtime 0.2.1 to 0.2.2 (additive: no contract, store or MODULES change; `.dkm` files need no rebuild). Old devices keep working with every host; the host asks only when the capability bit is set. No flash image was built.
- **MEMORY page (runtime 0.2.3).** SETTINGS > DIGIKIT > MEMORY shows the same entries as native text rows with a ten-character text bar, in the firmware's font. The entries come from the new runtime call `dkrt_capacity(out, max)`; `cmd_capacity` calls it too, so the SysEx reply is unchanged and there is one place that counts. Rows, in order: `DSP` and `ARENA` (a percent), then `MODS`, `MACH`, `VOICE`, `PARAM`, `PAGE` (`used/limit`), for example `ARENA .......... 0%` and `MODS  .......... 0/8`. A resource with limit 0 has no row (a harness cell with only `core/digikit-menu` shows ARENA and MODS). Left out: the staging buffer, the selection, the effect slots and the map entries (nothing a user can act on). The bar uses `|` (used) and `.` (free): both are 2 pixels wide, while `#` is 6 pixels and a full bar of it runs off the row. The rows are also kept in `DK_MEM` and `DK_MEM_N` (24 bytes per row); the `menu` smoke test reads them back with `expect_text`. Size: core/digikit-menu +680 bytes code, +139 data, +204 bss; the runtime +46 bytes code. **[D]** DSP, MACH, VOICE, PARAM and PAGE rows are not read back in smoke (no cell loads them with the menu): **[O]**. Real bars are a later item. **[C]** The page's row is labelled `CPU`, not `ARENA`, since the follow-up in finding 60 ("The load line and the Memory panel"); the page otherwise reads as described here.
- **Generation.** `SEL_MAX` and `MAP_N` now come from `[limits.max]` (`gen_sdk.rs`), so the cargo tie test is replaced by one that checks the generated file; `LIMIT_FX_SLOTS` is emitted into the runtime. `dsp-link.c` takes the link's word-offset
  width from a generated header (`c-shared` gen kind, `build/gen/dl-limits.h`: 0x80000 bytes, 0x20000 words, the contract's maximum, shared by both devices); the committed objects of the runtime, dsp-link, digikit-menu and two fixtures were rebuilt
  (their bytes changed only through the SDK constants).
- **READ (runtime 0.2.4).** A new SysEx command READ (0x16, capability bit 7) returns up to 768 bytes of a store file (path, offset, length; reply: file size and the bytes), so hosts can read a `.dkm` header and later download files; see `docs/design/digikit-upload.md`. **[D]** Checked in the `upload-quick` scenario (DT2): header, a range across a sector boundary, the 768 cap, the end of the file, the whole file, and the three refusals.
- **VERIFY (runtime 0.2.4).** VERIFY (0x17, capability bit 8) returns the sha256 of a store file computed by the device from a fresh card read; hosts compare it with the local hash after an upload. **[D]** Checked in `upload-quick` (DT2): equal to the local sha256, a missing file refused.
