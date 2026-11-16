# The `.dkm` module file

Status: built, 2026-10-16 (packer `packages/patchkit/src/dkm.rs`, loader `modules/core/runtime/src/rs/src/loader/mod.rs`). Plan:
`docs/plans/digikit-runtime.md` (Module file, Load sequence). Runtime: `docs/design/digikit-runtime.md`. Store the file lives in:
`docs/design/digikit-store.md`.

Machines and effects are declared by builder calls in `init`, so the file holds no register records; the module architecture is `docs/MODULES.md`.

A `.dkm` is one ColdFire module in a form the runtime can load at run time. It holds no firmware address and no host path: the
same file serves every image whose runtime has the imports it names. It is made from the module's `.dko` object (the object
`patchkit` links at patch time) by `patchkit pack`:

    patchkit pack test/dyn-demo -o plus_drive/dt2/digikit/modules/dyn-demo.dkm [--name NAME] [--runtime-min N]
    patchkit dkm-info FILE.dkm          # checks the sha256 and lists the contents

`pack` takes exactly one ColdFire object of the module (the module id names it; `--modules-dir DIR` adds module roots). The name
defaults to the module id (`test/dyn-demo`), at most 31 characters of `A-Z a-z 0-9 - _ . /`; `dkrt_module` reports this name (the run-time `unload` that took it is archived). The file is
deterministic: the same object gives the same bytes.

## Format

All integers are big endian. The file is at most 64 KiB in v0 (the runtime's staging buffer). Offsets are from the start of the
file; the loaded image starts at a 16-byte aligned file offset.

| Offset | Size | Field |
| --- | --- | --- |
| 0x00 | 4 | magic `DKMF` (0x444b4d46) |
| 0x04 | 4 | format version = 1 |
| 0x08 | 4 | file size in bytes |
| 0x0c | 4 | header size = 0xa0 |
| 0x10 | 32 | sha256 of the whole file with these 32 bytes read as zeros |
| 0x30 | 4 | `runtime`: the runtime version the module needs, packed `major << 16 \| minor << 8 \| patch`: the newest `since` of the imports it links (0 when it links none). The loader refuses (`E_CONTRACT`) a module whose version has another major or minor than the runtime's (`dkrt_sdk::RUNTIME_VERSION`) or a higher patch (0.x semver). Bit 31 is the **trace flag**: the module was packed with `patchkit pack --trace` (built with `DK_TRACE`); the loader ignores it, the DIGIKIT menu shows `TRACE` for the module (`dkrt_module_header` `flags`), `dkm-info` prints it. A version never reaches bit 31 |
| 0x34 | 4 | module version, `major << 16 \| minor << 8 \| patch` |
| 0x38 | 4 | `role`: 0 a module (`.dkm`), 1 a service (`.dks`); another word is `E_FORMAT` (earlier formats had the (major, minor) pairs of the api, machine, fx and service contracts here; the one version is at 0x30) |
| 0x3c | 4 | `H_SVC_ABI`: the ABI fingerprint of a service file, the value of its data symbol `dk_svc_abi` (`dkrt_abi::service::SVC_ABI`, emitted by `dkrt-svc-rt`'s `service!`); 0 for a module and for a file that uses neither interface B nor C. A word that is not 0 and differs from the runtime's (INFO bytes 41 to 44, capability `SVC_ABI`) is `E_SVC_ABI` |
| 0x40 | 32 | name, NUL padded |
| 0x60 | 4 | `image_off` (multiple of 16) |
| 0x64 | 4 | `image_size`: text and data, as the patch-time linker lays them out at base 0 |
| 0x68 | 4 | `bss_size`: zeroed memory right behind the image |
| 0x6c | 4 | alignment: 4, 8 or 16 (the arena aligns every block to 16) |
| 0x70 | 4 | `reloc_off` |
| 0x74 | 4 | `reloc_count` (at most 4096) |
| 0x78 | 4 | `import_off` |
| 0x7c | 4 | `import_count` (at most 64) |
| 0x80 | 4 | `init`: image offset of `dk_module.init`, or 0xffffffff |
| 0x84 | 4 | `fini`: image offset of `dk_module.fini`, or 0xffffffff |
| 0x88 | 4 | `menu`: image offset of `dk_module.menu`, or 0xffffffff |
| 0x8c | 4 | `tick`: image offset of `dk_module.tick`, or 0xffffffff |
| 0x90 | 4 | `table_off`: offset of the header table (multiple of 4) |
| 0x94 | 4 | `table_size`: its size (at least the 8-byte head; 0 is the format of runtime 0.7) |
| 0x98 | 4 | `dsp_off`: offset of the DSP section (0: none) |
| 0x9c | 4 | `dsp_size`: its size in bytes |

The four entry points are the fields of the constant `dk_module` of the module (`dk_module_def`, `sdk/include/dk/core/module.h`); the packer finds the symbol, follows the relocation of each field to a function of the image and writes the offsets above. The runtime calls `init(ctx)`, `fini(ctx)`, `tick(ctx)` and `menu(ctx, menu)` with a `dk_module_ctx *` (docs/plans/sdk.md section 5). Until 0.5 the header had a slot table at 0x88 and a header size of 0xa0, 0xa4 or 0xa8; runtime 0.6.0 reads only the layout above.

After the header, in this order, each table where its offset says: imports, relocations, the header table,
the DSP section (16-byte aligned), padding to 16, then the image. 

- **Imports**: `import_count` entries of 32 bytes, a NUL-padded name. Every undefined symbol of the object that a relocation
  names. The names the runtime provides: the `dk_*` imports of `sdk/sdk.toml` (`<dk/gen/imports.h>`), and `memcpy`, `memset`, `memcmp`, `__muldi3`, `__udivdi3`, `__umoddi3` for the compiler. A name the runtime does not have makes the load fail with `E_IMPORT` before anything is written.
- **Relocations**: entries of 8 bytes: `u32 offset` (into the loaded memory; the image only, never bss; any byte alignment, since
  m68k code holds 32-bit operands at even addresses) and `u32 kind << 24 | index`. Kind 0: the 32-bit word at the offset is a
  module-relative address; the loader adds the module's load address. Kind 1: the word is an addend; the loader adds the address
  of import `index`. The word is stored big endian. Only 32-bit absolute relocations (`R_68K_32`) can be loaded; PC-relative
  references between the module's own sections are resolved by the packer (the base cancels), and a PC-relative reference to an
  import is refused by the packer (the distance to the firmware would depend on where the module is loaded).
- **Header table** (`table_off`, layout in `sdk/sdk.toml` `[limits.format]` and `packages/dkneeds/src/keys.rs`): magic `DKTB`, a version, then tagged entries: NEEDS (services and versions), USES (`(resource id, count)`: the `[uses]` lines of `module.toml` for this device and `record_bytes`), KEYS (`(kind, n, keys)`), and the entries of a service module (KINDS, SLOTS, HOOKS, EXPORTS, RESOURCES). The machine and effect keys are hashes the packer computes from the packed name and the id (`register.rs`: `machine_key`, `effect_id`). The machine, parameter, page and effect records are not in the file: `init` declares them and the runtime builds them at commit (`docs/design/digikit-registry.md`).
- **Image**: the linked bytes at base 0; every word named by a relocation holds `S + A` (symbol offset plus addend) for kind 0 and
  `A` for kind 1.

The module defines the constant `dk_module` (`const dk_module_def dk_module = {.init = ..., .fini = ..., .tick = ..., .menu = ...}`), every field optional; an object without it, or one that still exports `dkm_init`, `dkm_fini`, `dkm_menu` or `dkm_slot_<n>`, is refused by the packer. `init` runs once after the copy, relocation and cache flush; if it returns
non-zero the load is undone (`E_INIT`; `E_IMPORT` when it returns `DK_E_MISSING` because a provider module the module needs is not in the image). `tick` runs once per pass of the UI task after `init` succeeded.

## Needs are inferred from the contents

A module does not say what a load of it takes of the device. The needs are worked out from the file's own contents, by one function, `needs_of(source)` of
the crate `packages/dkneeds` (no_std, no dependency). Why: a module that reported its own needs could misreport them, by mistake or on purpose, and every reader
(the menu, the host app, the tools) would have to trust it; a contributed or hand-built `.dkm` has no field to lie in. The runtime enforces the same limits when it
loads (its loader reads the real tables), so the inferred needs are the answer before the load, never the check itself. There is no needs block in the header: the header is the 0xa0-byte
header of format 1, and every format 1 file has needs.

`needs_of` reads the file through a byte source (`dkneeds::Bytes`: a size and a read at an offset), so the device can feed it from store sector reads without staging the file and a host from a
slice. It reads, and checks as the loader checks (bounds, counts; a file the loader refuses for its format has no needs, never a crash):

- the header: magic, format 1, the size fields against the file, the image size and offset, `bss`, alignment, and the reloc and import tables in the file;
- the header table: its bounds, magic and entries;
- the DSP section: its header (magic, version 1 to 4, the sizes, the table bounds and counts), for a version 3 or 4 section the `state_bytes` and `voices` words of each voice record, and for a version 4 section the `state_bytes` word of each effect record.

It does not read the image, the tables' entries, the records' bodies or the DSP image, and it does not check the file's sha256 (the loader does).

| Need | Meaning |
| --- | --- |
| `file_size` | the file's size: it must fit the staging buffer |
| `ARENA` | the arena bytes the loaded module keeps: `(image_size + bss_size)` rounded up to 16, plus the 16-byte block header, plus `record_bytes` of the header table (the record block `init` commits) |
| `ARENA_XFER` | arena bytes held only while the DSP code is sent: the same for the relocated DSP image (its `image_size`); 0 without a DSP section |
| `DSP_AREA` | the DSP range: `align8(image + bss)` plus the voice states (`voices * align8(state_bytes)` per voice record of version 3 and 4) and the effect states (`align8(state_bytes)` per version 4 effect record), rounded up to the 64-byte granule; 0 without a DSP section |
| `DSP_ALIGN` | the range's placement alignment (the DSP section's `align`): the range starts at the next multiple of it, so up to `DSP_ALIGN - 8` bytes of the free range can be lost; 0 without a DSP section |
| `MACHINE_ROWS`, `PARAM_ROWS`, `PAGE_ROWS` | the `[uses]` counts of the header table (an effect's page counts nothing: it belongs to the effect slot of its DSP record) |
| `VOICE_ROWS` | the number of voice records of the DSP section |
| `FX_SLOTS` | effect slots: the number of effect records of the DSP section (version 4) |

Names that are capacity kinds (`[capacity.kinds]` of `sdk/sdk.toml`) are in the unit of that kind's CAPACITY entry, so "fits" is need <= limit - used, with the largest free block for the arena and the DSP area.

Who calls it: the DIGIKIT menu (`modules/core/apps/digikit-menu`, on the card's files, once per open of the menu); patchkit (`patchkit needs [--json] FILE.dkm...`, and a `needs` line of `dkm-info`);
`patchkit.wasm` (`pk_needs`, read by the Transfer page through `apps/web/src/modules/dkneeds.ts`); the Python tools through the command line (`tools/device/dkneeds.py`: `dk run`, `module:check`, `dktransfer`). The arithmetic exists once.

The rule that compares the needs with a device is `fits_now` (docs/plans/what-fits.md; `dkrt_fit` in `modules/core/runtime/fit`, vectors in `modules/core/runtime/vectors/fit-vectors.json`; the compare and the needs are each written once, in Rust: the web app calls the compare through `pk_fit` and the tools through `patchkit fit --json -`): the staging buffer holds the file, a module record and a selection path are free,
the DSP range (with up to `DSP_ALIGN - 8` bytes for alignment) is at most the largest free range, the arena holds the module's block and the block of its DSP image together (each block at most the largest free block), and every row count is at most
limit - used. A kind that a device's CAPACITY lacks is not judged.
The `inferred needs` tests of `modules/core/runtime/tests/capacity/inferred/inferred-needs.device.test.ts` (full tier) load a set of run-time modules and fixtures alone, coupled, and compare
CAPACITY and MODULES with `needs_of` of the file (rows of a DSP module come in after the DSP acknowledges). The Rust tests of `dkneeds` cover the arithmetic and malformed files (truncated sections, absurd counts, every cut of a file).

## DSP section

A run-time machine can bring its own SHARC+ code (finding 32). `patchkit pack MODULE --device dt2|dn2` links the module's DSP object
(`[[object]] place = "dsp.l2"` for that device; at most one) into the section; the `[[machine]]` and `[[fx]]` entries of `module.toml` (the id, and the optional `dsp` prefix) name the entry points and give the voice rows. The code holds no address of the image it
runs in: it is linked at the nominal base `0x20000000` (L2 byte base) and **relocated by the runtime on the ColdFire** before it sends
the bytes, so several DSP modules coexist in the DSP area (`_dl_area` of `core/dsp-link`, 512 KiB on the Digitakt II, 320 KiB on the
Digitone II; first fit, freed when a boot-time DSP part fails). Relocation is cheap here: the repository's DSP objects use only two kinds, `sharc.abs24`
(a call or jump target, a pc) and `sharc.abs32` (a 32-bit immediate: a data address, or a pc), both of which the linker already
patches into a 48-bit instruction, and 24 relocations per 3.5 KB for `pluck`.

All integers are big endian. Offsets are from the start of the section.

| Offset | Size | Field |
| --- | --- | --- |
| 0x00 | 4 | magic `DKDS` (0x444b4453) |
| 0x04 | 4 | version = 4 (effect records, see below; the packer writes it only when the module has an effect, its voices are those of version 3) or 3 (voices of 64 bytes; the packer writes it only when a voice has a note_on entry or state, else version 2; versions 1 and 2 are still loaded: note_on 0, no state; version 1, voices of 16 bytes without ranges: every range undeclared) |
| 0x08 | 4 | device (1 DT2, 2 DN2): must equal the runtime's |
| 0x0c | 4 | `image_size`: bytes of text and data that are sent (a multiple of 4) |
| 0x10 | 4 | `bss_size`: bytes reserved behind the image (`.PKBSS`; not sent, not zeroed) |
| 0x14 | 4 | `align`: placement alignment in bytes, 8 to 64, a power of two |
| 0x18 | 4 | `image_off` (16-byte aligned) |
| 0x1c | 4 | `reloc_off`; 0x20: `reloc_count` (at most 4096) |
| 0x24 | 4 | `import_off`; 0x28: `import_count` (at most 32) |
| 0x2c | 4 | `voice_off`; 0x30: `voice_count` (at most 8) |
| 0x34 | 4 | `fx_off`: offset of the effect records (version 4; 0 before) |
| 0x38 | 4 | `fx_count`: effect records, at most 4 (`DK_FX_MAX`) (version 4; 0 before) |
| 0x3c | 4 | `sample_off` (version 5): offset of a 32-byte record of little-endian words, 0 before. Word 0 is the kind: 1 a sample bank (local id, bank byte offset, capacity frames, descriptor byte offset), 2 the per-track sample table of API 1.5 (local id, byte offsets of the 16 `dk_sample_pool_entry` records, 512 bytes, and of the 16 control sets, 256 bytes; four zero words). The blocks lie in the initialized image, are zero and have no relocations in them. Version 5 keeps the voices and effects of version 4 |

- **Imports**: 32-byte NUL padded names of DSP symbols the object references and does not define: today the helpers of `core/dsp-voice`
  (`_dv_exp2`, `_dv_frame_hw`, `_dv_frame_sw`). The runtime resolves them in `dyndsp_info` (module `core/dyndsp`): a name it does not
  list makes the load fail with `E_DSP_IMPORT` (-32). Their values are in natural units (a pc for code).
- **Relocations**: 8 bytes: `u32` byte offset of the 48-bit instruction in the image, then `u32 kind << 28 | class << 24 | import`.
  Kind 0 is `abs24` (bits 23..16 of the address are the byte at +2, bits 15..0 the little-endian halfword at +4), kind 1 `abs32`
  (the halfwords at +2 and +4, high then low). Class 0: the field holds a pc of this object (add `(B - 0x20000000) / 2`, `B` the
  L2 byte address where the image is placed); class 1: a byte address of this object (add `B - 0x20000000`); class 2: the addend of
  the import `import` (add its value). `abs24` with class 1 is refused by the packer (a data address does not fit 24 bits).
  A result that does not fit its field fails the load.
- **Voices**: 48 bytes (version 2): `u32` machine type, `u32` byte offset of the entry routine in the image, eight `u32` ranges of machine
  knob 0 to 7 (`min | max << 16`; 0: a stock code, an empty knob or no range declared) for the runtime guard of
  `core/dsp-voice` (finding 51), two reserved words. The type is in the run-time range 16 to 23, not used by a loaded module. The routine's pc at placement `B` is `0xb80000 + (B - 0x20000000 + entry) / 2`.
  Version 3 voices are 64 bytes: 0 `type`, 4 render entry, 8 note_on entry (image byte offsets; note_on 0: none), 12 `state_bytes` (per voice),
  16 `clear_bytes` (zeroed at note-on), 20 to 51 the eight ranges, 52 `voices` (number of voice states to reserve), 56 to 63 reserved 0. The packer
  finds the entry points by name (`<id>_render`, `<id>_note_on`, `_<id>_desc`: a `dk_machine_desc` data symbol; `dsp` of the `[[machine]]` entry replaces the `<id>` prefix). The runtime reserves
  `align8(image + bss)` plus, per voice record, `voices * align8(state_bytes)` for state, in record order, in the module's range.
- **Effects** (version 4): 64 bytes each, the effects of the FX contract (`<dk/fx/fx.h>`) that the module brings, (found by the names `<id>_process` and `_<id>_desc`, the `dk_fx_desc` data symbol). Record: 0 `process` (image byte offset of the routine), 4 `desc` (image byte offset of its `dk_fx_desc`), 8 `state_bytes`, 12 `n_params` (both
  from the descriptor, which the packer reads and checks: magic `DKFX`, contract 1, `n_params` at most 8, `state_bytes` a multiple of 4), 16 flags (the routes, below; 0 = master only), 20 to 51 the words
  p0 to p7 of the row (0 where no default is given), 52 to 63 reserved 0. The effect's state is not a symbol of the object: the runtime reserves
  `align8(state_bytes)` for each record in the module's range, behind the image, the bss and the voice states, in record order (the adapter zeroes the state when the effect
  switches on). The process routine's pc at placement `B` is `0xb80000 + (B - 0x20000000 + process) / 2` (as a voice's). The runtime checks the record against the descriptor in the
  image (a record that says more state than the descriptor, or another parameter count, is `E_DSP_FORMAT`).
  **Flags (the routes, FX routing step 4).** bit 0 `FXF_SEND` (1): the effect may run on a send bus; bit 1 `FXF_NOMASTER` (2): it may not run on the master chain; bit 2 `FXF_DEFAULT_SEND` (4): its
  default route is the send bus. The master is always allowed unless bit 1 says not, so the word 0 that every record made before the routes has means "master only, default master": an old `.dkm` loads unchanged, and a module that leaves the
  fields out packs to the same bytes as before. The packer writes a valid placeholder pair (MASTER|SEND, flag `FXF_SEND`, 1); the runtime overrides it at commit with the flags from `dk_fx_routes(fx, allowed, default)` in `init`: no call gives 0 (master only, the legacy value); `(MASTER|SEND, MASTER)` gives 1 (both, default master); `(MASTER|SEND, SEND)` gives 5 (both, default send); `(SEND, SEND)` gives 7 (send only). Other combinations (a default that is not allowed) are refused at commit, and the loader refuses a record with other flag bits (`E_DSP_FORMAT`). The runtime uses the default when the settings
  have no `fx/route/<module id>` and when the stored route is not allowed; a runtime older than the flags ignores them (the effect runs on the master).
- **Image**: the linked bytes at the nominal base; every relocated field holds `S + A`. The sections are text, data, bss in that order,
  so a `.PKBSS` buffer (pluck's 256 KiB delay lines) follows the sent bytes in the reserved range.

The runtime reserves `image_size + bss_size` rounded up to 64 bytes (`E_DSP_NOSPACE`, -33, when no range is free), copies and relocates
the image in an arena block, sends it with `dl_send` (window 0 of the link) and, when the DSP acknowledges (the DSP has the bytes in L2
and has invalidated its instruction caches), publishes the voice rows (window 1: **one word per transfer**, a row is 24 words: 0 type, 1 pc, 2 to 9 the eight ranges, 10 to 15 the guard's words, 16 note_on pc, 17 state_base, 18 stride, 19 clear, 20 gen, 21 level, 22 and 23 spare; written: the ranges, the pc, words 16 to 19, the guard's disabled-type word cleared and the type
last; a free row has type `0xffffffff`, which no machine equals, so the DSP's render scan never sees a half-written row). The machine and parameter rows are written later, at commit after `init`.

**Effect rows.** An effect record takes a free row of the effect chain of `core/fx` (`core/dynfx` reserves one for each of the device's `fx_slots`: 3 on the Digitakt II; a free row has pc
`0xffffffff`, which the adapter skips). The runtime claims the lowest free rows when it reserves the range (CAPACITY `FX_SLOTS` counts them from then on) and publishes each row
after the code is acknowledged, through **window 2** of `core/dsp-link` (the whole table of the registry `fx`, 24 words a row; **one word per transfer**): the parameter words p0 to p7, desc, state, `enabled` = 1
the 4 amount words and the route (a send effect never runs on the master, finding 64) and the pc last (word indices 4 to 11, 1, 2, 3, 19 to 22, 18, 0 of the row; 24 words: 0 pc, 1 desc, 2 state, 3 enabled, 4 to 11 p0 to p7, 12 `was`, 14 `dis`, 15 to 17 the fault window and counts, 18 route (0 master chain, 1 send bus; Digitakt II, finding 62), 19 to 22 the 16 send amounts as bytes, 23 spare).
A row is freed the other way round: the pc to `0xffffffff` first, then the route and the amounts, `was`, `dis` and the fault window to 0. `dynfx_info` (core/dynfx) is the table the runtime reads: 0 `DFXI`, 4 version, 8 device,
12 rows base, 16 index of the first free row, 20 free rows, 24 rows of the registry, 28 words of a row. An image without `core/dynfx` refuses a module with effect records with `E_NO_DSP`; more
records than free rows is `E_DSP_ROWS`. When the DSP part fails at boot the module is taken down the other way: machine row, voice row (type
to `0xffffffff` first), then the range (the run-time unload that did this is archived). Refusal codes -30 to -39 (`dkrt-sdk`): `E_NO_DSP` -30 (no `core/dsp-link` and `core/dyndsp` in
the image), `E_DSP_FORMAT` -31, `E_DSP_IMPORT` -32, `E_DSP_NOSPACE` -33, `E_DSP_ROWS` -34 (no free voice or effect row), `E_DSP_XFER` -35 (transfer
failed or timed out), `E_DSP_DEVICE` -36, `E_DSP_TYPE` -37 (a voice key that has no assigned type, a pinned voice type outside 16..23, or a type in use).

The DSP's per-type **selector** word (which render path a track uses) is not set by this section: the selector of a run-time type is
type 0's (finding 30). A machine that clones SAMPLE, like `pluck`, is right already.

## What the loader checks, in order (nothing is written to the arena before the last one)

1. Store: header, table checksum, the path, the table's file hash of the file (`E_NOT_FOUND`, `E_BAD_STORE`, `E_SHA`, `E_TOO_BIG`).
2. Magic, format 1, size fields equal the file (`E_FORMAT`); the file's own sha256 (`E_SHA`).
3. Version (0x30): the runtime version the module needs has the runtime's major and minor and a patch that is not above the runtime's (`E_CONTRACT`: rebuild the module); 0 passes.
4. Name, alignment, table bounds, reloc kinds and offsets, entry point offsets, header table (`E_CONTRACT` for the 0.7 format, else `E_FORMAT`).
5. Every import resolves (`E_IMPORT`; the name is kept as the module's need); no module of that name is loaded (`E_LOADED`); a free module record (`E_TABLE_FULL`).
6. The DSP section, if any (`E_NO_DSP`, `E_DSP_DEVICE`, `E_DSP_FORMAT`, `E_DSP_IMPORT`, `E_DSP_NOSPACE`, `E_DSP_ROWS`, `E_DSP_TYPE`), then the keys against the DSP voice and effect counts (`E_DECL_ID`). Nothing is reserved yet.
7. Then: reserve the DSP range and rows; allocate `image_size + bss_size` (`E_NOMEM`), copy, zero bss, relocate, flush both caches over the range, record; `init` (builder calls are collected; `E_INIT`, or `E_IMPORT` when it returns `DK_E_MISSING`). At commit the runtime checks the collection against `[uses]` and the keys (`E_DECL_USES`, `E_DECL_ID`, `E_DECL_FULL`, `E_FX_ID_DUP`), lowers it to records, checks them (`E_NO_REGISTRY`, `E_REG_DEVICE`, `E_TYPE_RANGE`, `E_TYPE_USED`, `E_PARAM_FULL`, `E_PAGE_FULL`; docs/design/digikit-registry.md) and writes the rows. Any failure undoes the load; the result is `E_BUSY` when a part cannot be given back.
   With a DSP section the commit runs before `dsp::launch`, and `dkrt_load` returns `LOADING`; the DSP code and the voice rows follow over the next UI-loop passes once the DSP has acknowledged the code (docs/design/digikit-runtime.md, "DSP code of a module").

## Check

`packages/patchkit/src/dkm.rs` has a reference loader (`Dkm::memory`) and a test that its result at any base equals the
patch-time linker's output at that base; `packages/patchkit/src/dkmdsp.rs` the same for the DSP section (`DspSection::relocated`, also
`patchkit dkm-dsp-image FILE BASE`), and `tools/patchkit/pk_dkrt_machine.py` compares the DSP's L2 after a load with a second
implementation of it. The runtime's loader is checked in the emulator (`tools/patchkit/pk_dkrt.py proof`,
`tests/patchkit/test_patchkit_dkrt.py`).

## Open

- Streaming load for files over 64 KiB; compression.
- Signing: the sha256 is an integrity check, not authentication (the store is on the user's own card).
- Pages and `own_dsp` machine records (Digitone II, lane B).
- The selector word of a run-time type (finding 32, open).
