# The +Drive format

All addresses are **DT2 1.16** (`out/ghidra/dt2-1.16-emac/`, file offset =
`VA - 0x40000400`), read from the static Ghidra decompile plus the raw bytes
of `Digitakt_II_OS1.16.syx`'s extracted sections
(`sections/.source-sha256` = `278541e4...`). Nothing here required the
emulator; two independent static passes (one per topic) plus a third pass on
the filesystem layer were run, and a few claims were cross-checked against
raw bytes captured from a real firmware-formatted card overlay
(`snapshots/dt2-1.16/running.snap`), which counts as image-byte
cross-checking for those specific claims. A fourth, independent agent then
re-read the filesystem layer's 8 most load-bearing claims cold (record
layout, extent format, root id, the readdir/`0x10001` mechanism, the raw
write path) against the same decompile before seeing this document, per this
repo's rule that a finding needs a second agent's check against the image
bytes before it's marked **[V]**; one claim (the readdir/`0x10001`
mechanism) came back **[C]**, corrected in place below. Everything else is a
single static pass and marked **[D]**, not **[V]**.

`docs/findings/02-machines-and-parameters.md` (~line 328-340) and
`docs/findings/07-emulator.md` (~line 1345) cite `FUN_401239be`,
CMD18 = `0x401208fe`, CMD25 = `0x40120ae4`. **[C]**: none of these addresses
exist in the 1.16 dump (`0x401208fe` decompiles as an unrelated SysEx
receive/ack state machine). The real 1.16 functions, found by matching
behaviour and by the ESDHC `XFERTYP` command-index literal, are:

- `FUN_40130f9a` (0x40130f9a) — the "INITIALIZING +DRIVE..." driver: sets the
  header's fixed bytes and calls the two format gates.
- `FUN_4012deda` (0x4012deda) — CMD18 read (`XFERTYP = 0x123a0036`).
- `FUN_4012e0c0` (0x4012e0c0) — CMD25 write (`XFERTYP = 0x193a0022`).

Both arm eDMA channel `0x3b` = 59, matching `docs/findings/04`'s existing
"eDMA channel 59 is eSDHC block I/O" line — that citation used the same stale
address and should be read as `FUN_4012deda`/`FUN_4012e0c0` going forward.

## Two separate on-disk regions, not one

+Drive is **not** a single filesystem. There are two independent structures,
addressed by disjoint, hard-coded absolute sector numbers (not derived from
card capacity or a superblock pointer):

1. **`MmcFs`'s three fixed-capacity object pools** — projects, sounds
   (patches/presets), kits. Flat, slot-indexed, no directories. This is what
   the first-boot "Factory reset" worker (`FUN_400334bc`) formats.
2. **A real, arbitrarily-nested, path-addressable filesystem** — inode-style
   128-byte records, extent-mapped 32 KB content pages, hierarchical
   directories — reached through a `Directory`/`FileSystemDirectory` class
   pair and a generic `elektron::MidiRpcFsSample*` MIDI SysEx RPC family used
   by companion software (e.g. Transfer) to browse/read/write it remotely.
   **This is where +Drive sample files live**, and it's the one this finding
   and `tools/plusdrive.py` targets.

Nothing found ties the two regions together at the code level (a `MmcFs`
vtable scan for the filesystem's sector constants came up empty), only by
their sector numbers being far apart. **[D][O]**: not exhaustively proven
disjoint, only not found joined.

## Region 1: `MmcFs`'s header, table, and pools **[D]**, header partly **[V]**

### Header sector (block 0)

Real freshly-formatted bytes, captured from `running.snap`'s `Esdhc` card
overlay (`emu/snapshot.py`'s pickled `components['esdhc']['card_overlay']`,
a sparse `{byte_offset: value}` dict — no Machine construction needed to read
it):

```
offset 0x000: be ef ba ce 00 00 00 01 01 01 00 00 00 00 00 00
offset 0x1c0: 00 00 00 00 00 00 00 02 43 17 2d e4 00 00 00 00
offset 0x1d0: 00 00 00 00 45 05 5b c0 45 05 5b d0 45 05 5b d0
(everything else in the 512-byte sector is zero)
```

`FUN_40130f9a` (0x40130f9a), quoted:

```c
param_1[0x779c] = -0x41104532;      // ctx+0x1de70 (sector +0x00): 0xBEEFBACE
param_1[0x779d] = 1;                // ctx+0x1de74 (sector +0x04): 1
*(undefined1 *)(param_1 + 0x779e) = 1;        // ctx+0x1de78 (sector +0x08): 1
*(undefined1 *)((int)param_1 + 0x1de79) = 1;  // ctx+0x1de79 (sector +0x09): 1
cVar4 = FUN_4012fd92(param_1);
```

`FUN_4012fd92` (0x4012fd92), the writer, requests **exactly 12 bytes** be
written to block 0 (`FUN_4012fcb6(param_1,0,0xc,param_1+0x1de70,...)`), but
the CMD25 primitive `FUN_4012e0c0` always rounds the DMA up to a whole
512-byte sector (`uVar2 = (param_2+0x1ff)>>9`, forced to 1). **So bytes
12-511 of the transferred sector are whatever RAM happened to be at
`ctx+0x1de70+12..+0x200`, not code-written data.** `ctx+0x1e034` is used
throughout this object's methods as an RTOS mutex lock handle
(`FUN_4019e0ee`/`FUN_4019e114`), and `0x1e034 - 0x1de70 = 0x1c4` — inside the
observed nonzero region (0x1c0-0x1e3). The three near-identical 32-bit words
at 0x1d4/0x1d8/0x1dc (0x1d8 and 0x1dc *identical*) have exactly the shape of
an empty RTOS wait-queue's `next==prev` sentinel. **[V]**: the mechanism (a
12-byte intentional write, rounded up to 512, leaking live mutex/queue state
into the tail) is proven by the code; the exact semantics of those leaked
words are not, and don't need to be — they are not part of the format.

**Header field table:**

| offset | bytes | meaning | confidence |
|---|---|---|---|
| 0x00-0x03 | `BE EF BA CE` | magic | **[V]** literal write |
| 0x04-0x07 | `00 00 00 01` | format/version = 1 | **[V]** literal write |
| 0x08 | `01` | flag | **[V]** literal write |
| 0x09 | `01` | flag | **[V]** literal write |
| 0x0A-0x1FF | zero in the sample | not written by this function; leaked live RAM in general | **[D]**, not a format field |

`0xBEEFBACE` is a generic Elektron container magic reused elsewhere (e.g.
`FUN_4000b8c0` checks it as a sound-bank/preset blob magic; `FUN_4012f6d8`
reuses the literal as an internal end-marker) — not unique to +Drive.

### Table sector (block 0x800 = byte offset 0x100000)

Real captured bytes: **all zero**, matching a freshly formatted (nothing
allocated) card, not a missing write. `FUN_4013009c` (0x4013009c) writes
**400 bytes** from `ctx+0x1de7d` to block `0x800`. `FUN_40130f9a` zero-fills
three sub-ranges of that same buffer before the first format:

```c
FUN_40135ad8((int)param_1 + 0x1de7d, 0x10);   // 16 bytes  = 128 bits  -> PROJECT pool
FUN_40135ad8((int)param_1 + 0x1de8d, 0x100);  // 256 bytes = 2048 bits -> SOUND pool
FUN_40135ad8((int)param_1 + 0x1df8d, 0x80);   // 128 bytes = 1024 bits -> KIT pool
```

16+256+128 = 400 bytes exactly. **[V]**: block 0x800 is three fixed-size
per-pool occupancy bitmaps (128 project slots / 2048 sound-preset slots /
1024 kit slots), not a directory, inode table, or disk-wide free-space map.
Confirmed bit setters: `FUN_40130234` (project, base `ctx+0x1de7d`),
`FUN_40132b8a` (kit, base `ctx+0x1df8d`, clamped `<= 0x3ff`); the sound-pool
setter wasn't traced line-by-line but the 0x100-byte zero-fill and a
2048-iteration rescan (`FUN_4012fa8c`) match exactly.

### Pools have no growth mechanism and no directories **[V]**

Slot→block address is a pure clamped linear formula, not a chain:

```
FUN_4012eef2 (project):  slot>0x7f -> 0x7f;   block = ((slot+0xb)&0xff)<<0xf
FUN_4012f596 (sound):    slot>0x7ff -> 0x7ff; block = (slot+0x2400)*8
FUN_401324cc (kit):      slot>0x3ff -> 0x3ff; block = (slot*0x10000+0x2c00000)>>9
```

Each sound-pool slot's own on-disk header (`FUN_4012f6d8`, reads 0x600 bytes
per slot) carries a 4-byte magic (`0x44543153`="DT1S" Digitakt /
`0x444e3153`="DN1S" Digitone — a shared cross-device format), a 16-byte name
field, and two more 4-byte fields (size, format code). "FACTORY
PROJECT/SOUNDS/KITS >> +DRIVE..." (`FUN_40131282`, strings at 0x4025a6df /
0x4025a6fc / 0x4025a718) are the three progress captions for formatting each
pool in turn, **not** evidence of a folder hierarchy — at this layer storage
is flat and fixed-capacity. This corrects any folder-hierarchy reading of
those strings in `docs/findings/02`/`03`. No literal "2,120 erase groups"
(`docs/findings/07`) was found compiled in along this path; it's presumably
read from the card's own CSD/EXT_CSD at erase time — **[O]**, not chased.

## Region 2: the real filesystem — what `tools/plusdrive.py` writes **[D]**

Two record kinds:

- **A 128-byte per-file/per-directory metadata record** ("inode"), reached
  only through `FUN_4015b64e(id)`.
- **A variable-length directory-entry record**, embedded in a *directory's
  own content stream*, one per child.

### Disk layout (absolute sector numbers, hard-coded in the firmware)

| region | sector base | source |
|---|---|---|
| record-id occupancy bitmap | `0x5d8040` | `FUN_4015b7ac` |
| content-page occupancy bitmap | `0x5d80c0` .. `0x5d8180` | `FUN_40155698` (flush loop ends exactly at 0x5d8180 — cross-check) |
| record array (128 B/record) | `0x5d8180` | `FUN_4015b64e`: `id*0x40 + 0x5d8180` groups 256 records/32 KB page |
| main content-page area (32 KB/page) | `0x5ee180` | `FUN_401559f4`: `page*0x40 + 0x5ee180` |
| second, smaller content region (purpose unresolved) | `0x458000` | `FUN_401559f4`, for page ids `>= 0x1000800` |

These numbers are far below the pools' ~214 MB footprint on one side and
well inside the default 0x00760000-sector (≈3.69 GiB) card capacity
`emu/esdhc.py` already uses, so both regions fit in the emulator's existing
default card size unmodified.

**Root directory has a fixed, literal id: 2.** `FUN_40159f50` (the path
walker): `if (*param_1 == '/') { local_128 = 2; ... }`. The id-2 record sits
at absolute byte offset `0x5d8180*512 + 2*128`. There is no separate
superblock region — "the superblock" is just "id 2, resolved through the
ordinary record path". **[V]**: also confirmed the literal `"/\0..\0\0"` path
string at raw file offset `0x25d545` (`0x4025d945 - 0x40000400`) directly
against section bytes, not just the decompiler.

Nesting is real, not flat: `FUN_40159f50` walks arbitrary `/`-separated path
depth, calling `FUN_401572e0` (name→child lookup) and descending into
whichever child it returns, recursively. **[V]**.

### The 128-byte record ("inode")

```c
int FUN_4015b64e(uint param_1) {
  if (param_1 - 2 < 0x57ffe) {          // valid on-disk ids: 2 .. 0x57fff
    ... /* 16-slot LRU cache, then on miss: */
    FUN_4012deda(((param_1 & 0x1ffffff) >> 8) * 0x40 + 0x5d8180, 0x8000, 0x40941d90);
    FUN_401360ac(iVar3, (param_1 & 0xff) * 0x80 + 0x40941d90, 0x80);
    return iVar3;
  }
  else if (param_1 - 0x1000000 < 0x800)
    return (param_1 - 0x1000000) * 0x80 + 0x47d73ed0;   // RAM-resident ids, purpose unresolved
  return 0;
}
```

Field offsets, each confirmed by a one-line accessor:

| offset | size | meaning | confidence |
|---|---|---|---|
| 0x00 | 1 (bit 0) | attribute bit, tested by `FileSystemDirectory::vfunc_7` (`FUN_4015bb34`) | **[D]**, semantics (file-vs-dir?) unresolved |
| 0x01 | 1 (bit 0) | attribute bit, tested by `vfunc_9` (`FUN_4015bb7c`); fresh records default to `2` (bit0=0); a parent must have this bit **clear** before a child can be created under it (`FUN_40158626`) | **[D]**, "is-directory"/"protected" both plausible, not disambiguated |
| 0x02 | 2 (u16) | a counter incremented at link time (`FUN_40156334`) | **[O]** link count vs open-file count unresolved |
| 0x04 | 4 (u32) | content byte size | **[V]**, independently read in 3 places (`FUN_4015ba22`, `FUN_40155cfa`, `FUN_4015838e`/`File::write`'s grow-on-write check) — a second agent confirmed these three but found a fourth function that looks like a size-getter, `FUN_4015ba7c`, does **not** read this field: it instead sums `extentLength * 0x8000` over the extent list, i.e. *allocated* bytes, a different quantity. Don't cite `FUN_4015ba7c` as a size-field reader. |
| 0x08 | 4 (u32) | parent directory's own record id, written at link time | **[V]** |
| 0x0c | 4 (u32, bit 0) | attribute bit, tested by `vfunc_14` (`FUN_4015bc10`) | **[O]** unresolved |
| 0x10 | 4 (u32) | monotonic global sequence number, set only at allocation | **[V]** field exists; purpose inferred (generation/tiebreak counter) |
| 0x1e | 2 (u16) | extent count | **[V]** |
| 0x20-0x7f | 96 B | up to 8 inline 12-byte extents (count <= 8), or up to 24 4-byte pointers to indirect 32 KB extent-index pages (count >= 9, 2730 extents/page) | **[V]**, arithmetic cross-check: `8*12=96=0x60`, `0x20+0x60=0x80` = record end; `32768/12` truncates to 2730 = the exact divisor used |

An extent is a 12-byte triple `{logicalStartPage: u32, lengthInPages: u32,
physicalBasePageId: u32}` (proven by the lookup function's bounds check and
mapping arithmetic), not a `(start,length)` pair.

**Reserved logical pages, addressed the same way as real content pages via
extents in the directory's own record:** `0x10000` (name-hash index, binary
search, used by `FUN_401572e0` when doing a by-name lookup with more than
4094 real entries per the report that found it — direction not fully
resolved), `0x10001` (the enumeration/"jump to entry N" index that
`FUN_40157072`, the readdir primitive `FileSystemDirectory` uses to build
its on-screen list, reads **unconditionally**), `0x10002` (id-sorted index).
`FUN_40156334` (the link/create function) maintains all three on every
insert.

`FUN_40157072`, quoted (this is what the SRC/sample browser's enumeration
ultimately calls, through `FileSystemDirectory`'s `std::_Sp_counted_ptr<
dirent2_t*,...>::ctor_dtor` at 0x40159a2c):

```c
undefined4 FUN_40157072(undefined4 *param_1,int param_2) {
  ...
  puVar3 = (ushort *)FUN_40155c16(*param_1,0x10001);
  if (*puVar3 < 0xfff) {
    iVar1 = param_1[3];                      // current entry index
    if ((int)(uint)*puVar3 <= iVar1) { ...; return 0; }   // >= count -> end of directory
    uVar2 = *(uint *)(puVar3 + iVar1 * 4 + 6);            // packed (page,offset) for entry iVar1
    param_1[3] = iVar1 + 1;
    param_1[2] = uVar2 & 0x7fff;                          // byte offset within page
    param_1[1] = uVar2 >> 0xf | (int)-((int)uVar2 < 0) << 0x11;  // page index, sign-extended
  }
  ...
  uVar4 = FUN_40155cfa(param_1,param_2);      // decode the entry at (page,offset)
  ...
}
```

**This is the load-bearing detail for our writer, and a second-agent check
of this specific claim found the mechanism I first wrote here was wrong —
correcting it: [C].** `FUN_40155c16(dirId, 0x10001)` resolves through
`FUN_40157e60`'s extent lookup; when no extent in the directory's record
covers logical page `0x10001` (never allocated), that lookup's not-found
path returns 0, so `FUN_40155c16` returns **NULL**, not a pointer to a
zero-filled page. `FUN_40157072` then does `*puVar3` with **no NULL check
anywhere in the function** — this is an unguarded NULL dereference, not a
clean "reads as zero, reports empty" fallback. What actually happens depends
on runtime content at address 0 (on this ColdFire target, plausibly the
exception-vector table's initial supervisor-stack-pointer value, which would
almost certainly not read as `< 0xfff`, so even the branch this paragraph
originally described wouldn't be taken — the function would instead fall
through to decode whatever stale cursor state it already had). The
conclusion for the tool is unchanged and, if anything, stronger: **a
directory missing its `0x10001` index page is not a graceful "shows up
empty" case, it's undefined behaviour / a likely crash**, so
`tools/plusdrive.py` must always write this page for the root directory.

Page `0x10001` layout, inferred from the read pattern (u16 count at offset
0, then a 4-byte gap, then one 4-byte packed pointer per entry starting at
offset 6): `count:u16` at 0, 4 reserved bytes, then `count` x 4-byte packed
`(page:17bit signed, offset:15bit)` pointers into the directory's real
content pages. **[D]**, not independently cross-checked against a real
populated directory (none was available to capture).

### Directory content-page entry format (variable length)

Confirmed from both the reader (`FUN_40155cfa`) and the writer
(`FUN_40156334`):

| entry offset | size | meaning |
|---|---|---|
| 0x0 | 4 | child's own record id (0 = empty/free slot) |
| 0x4 | 2 (u16) | total slot length in bytes (stride to next entry) |
| 0x6 | 1 | name length in bytes |
| 0x7 | 1 | type/kind byte — a copy of the child record's own offset-0x00 byte, cached here so listing doesn't need a second `FUN_4015b64e` fetch |
| 0x8.. | nameLen | raw name bytes, not NUL-terminated on disk |

**[V]**.

### Write path does no interpretation of the payload **[V]**

`File`'s write vfunc (`FUN_4015838e`) allocates/grows pages
(`FUN_4015816c`) then either writes a whole 32 KB page straight from the
caller's buffer (`FUN_40155ba8`) or `memcpy`s a partial page
(`FUN_401360ac`). **No chunk-tag check, no header validation, no
format-specific branching on the bytes being written, anywhere in this
path.** Whatever bytes a caller (the MIDI RPC handler, or our own tool)
supplies become the file's bytes, unmodified.

Separately: `docs/findings/04-coldfire-dsp-link.md` (line 1372) already
established the *unrelated* fact that a debug-console command handler
(`FUN_400cae8c`) can resample raw PCM through the ColdFire's EMAC against a
fixed SDRAM window, but that path is **only reachable from the debug
console**, not the real track engine.

### On-disk sample payload format: **[O]**, genuinely open

No `RIFF`/`WAVE`/`fmt `/`data` chunk-tag string exists anywhere in any
extracted 1.16 section (`section_2_DSP.bin` through `section_8_SECTION.bin`)
— checked twice, once by raw byte scan and once against Ghidra's own
`strings` table; the few `RIFF`/`WAVE` substring hits are all inside
unrelated words (`SHERIFF`, `WAVELENGTH`, ...). No function under a
`Sample*`/`Wav*`/`Pcm*` name does bit-depth/channel/samplerate range
checking. The only registered file extensions anywhere in MAIN_OS are
`.dt2prj`/`.dt2kit`/`.dt2pst` (project/kit/preset, not samples), registered
alongside the literal string `"48000"` (the only bare samplerate string in
the image — inferred to be the device's native/internal samplerate, not
proof of anything about sample-file encoding).

Given the write path is a raw, unvalidated byte copy (previous section),
either (a) the SRC browser/loader validates and converts format at *load*
time (in code not identified by this pass — `SampleManager::vfunc_30`
through `vfunc_45`, 0x40023e84-0x4002c794, and `SampleLoaderBgWorker`'s own
read path, 0x400f0d30/0x401cbb1c/0x401cbb42, are the untraced candidates),
or (b) any WAV→internal conversion happens entirely on companion desktop
software before the bytes ever reach this filesystem, and firmware always
treats sample-file content as an opaque blob. **`tools/plusdrive.py` stores
each input WAV file's bytes verbatim (whole file, including its RIFF
header)** — the only choice consistent with the proven write-path behaviour,
and the one to falsify or confirm with an emulator run against a real SRC
browser/loader trace.

## What `tools/plusdrive.py` builds

Given the above, a minimal image that should make the root directory
enumerate and be loadable:

1. Header sector (block 0): the four verified bytes, rest zero (the leaked
   region is not part of the format and zero is a valid substitute).
2. Table sector (block 0x800): all zero (nothing allocated in the three
   pools — we don't touch them).
3. Record-id bitmap (`0x5d8040`): bits set for id 2 (root) and one id per
   sample file.
4. Content-page bitmap (`0x5d80c0`-`0x5d8180`): bits set for every physical
   page used (root's content page, root's `0x10001` index page, each
   sample's data page(s)).
5. Record array (`0x5d8180`): root's 128-byte record (parent=2, one extent
   for its content page, one extent for its `0x10001` index page at logical
   page `0x10001`) and one 128-byte record per sample (parent=2, size=file
   length, one extent covering however many contiguous 32 KB pages the file
   needs, laid out contiguously so a single extent triple suffices even for
   multi-page files).
6. Root's content page: the directory-entry list (one variable-length entry
   per sample, per the table above).
7. Root's `0x10001` index page: `count` + one packed `(page,offset)` pointer
   per entry, in the same order as the content page.
8. Each sample's data page(s): the input WAV file's bytes, verbatim.

**Explicitly not attempted** (deferred pending emulator feedback): the
`0x10000` name-hash and `0x10002` id-sorted index pages (only needed for
by-name path resolution — `FUN_401572e0` — which on-device browsing/loading
may not exercise, since the browser already holds each entry's id from
enumeration and can plausibly open by id directly); this is the first thing
to add back if the emulator run shows a path-based open failing. The exact
attribute-byte values for "regular file" vs "directory" at record offsets
0x00/0x01/0x0c (table above) are also inferred, not proven — if the browser
misclassifies our sample entries, trace `FileSystemDirectory::vfunc_7/9/14`
(`0x40159380`/`0x40159466`/`0x401594ce`) against a value sweep.

## Region 2 has a third, undocumented on-disk structure: a superblock at sector 0x5D8000 **[V]**

Neither this document nor `tools/plusdrive.py` accounted for this before now.
The real filesystem's mount routine, `FUN_4015a450`
(`out/ghidra/dt2-1.16-emac/decomp/4015a450_FUN_4015a450.c`, called from the
boot task `FUN_400cc864` at call site `0x400ccc00`, guarded by
`DAT_4029e9b0 & 0x10`), is the actual "is this card already mounted, real
filesystem" check (as opposed to `FUN_4015a424`/`FUN_4015a164(1)`, the
unconditional (re)format path, called instead when that bit is set):

```c
undefined4 FUN_4015a450(undefined4 param_1) {
  _DAT_44f2bd68 = 0;                                    // mount flag
  FUN_4012deda(0x5d8000, 0x200, &DAT_46f4dcb0);         // CMD18 read, 512 B
  if (_DAT_46f4dcb0 == 0x656b4653                        // magic
      && FUN_4015abb2(&DAT_46f4dcb0,0x1fc,0x31323334) == _DAT_46f4deac  // checksum
      && _DAT_46f4dcb4 - 3U < 2                          // version byte in {3,4}
      && FUN_4015a124(param_1) >= 0) {
    _DAT_44f2bd68 = 1;                                   // mount flag = mounted
    ...
    return 0;
  }
  return 0xffffffff;
}
```

The corresponding writer, `FUN_4015a164` (called via `FUN_4015a424`,
`docs`/finding-07's branch (A)/(B) format path), builds this exact 512-byte
buffer in RAM at `&DAT_46f4dcb0` and writes it to sector `0x5D8000` with the
CMD25 primitive (`FUN_4012e0c0`) at the very end of formatting:

| offset | value | meaning |
|---|---|---|
| 0x00 | `0x656B4653` | magic |
| 0x04 | `4` | version (mount accepts 3 or 4) |
| 0x08 | `0x8000` | = `PAGE` (32 KiB) |
| 0x0c | `0x58000` | |
| 0x10 | `0xA0080` | |
| 0x14 | `0x40` | |
| 0x18 | `0xC0` | |
| 0x1c | `0x180` | |
| 0x20 | `0x16180` | |
| 0x24 | `0x40` | |
| 0x28 | `0x20` | |
| 0x2c | value from `FUN_40155698(0x2c,...)` | |
| 0x30 | value from `FUN_40155698(0x2c,...)` (called twice; second call's value) | |
| 0x1fc-0x1ff | not covered by the checksum | reserved/unused by the check |

Fields at 0x2c/0x30 were not independently re-derived (they come from a
runtime call, not a literal); the rest are literals confirmed directly in
`FUN_4015a164`'s decompilation. **[O]**: exact meaning of most fields beyond
0x00/0x04/0x08 is inferred from context (block/page-count-shaped values),
not proven field-by-field.

**The checksum at 0x2c (RAM-cached as `_DAT_46f4deac`, computed over bytes
`[0x00:0x1FC]` of the buffer, seed `0x31323334`) is Bob Jenkins' public-domain
`lookup3.c` `hashlittle` (see `docs/sharc/SOURCES.md`-style citation: Bob
Jenkins, "lookup3.c", public domain, <https://burtleburtle.net/bob/c/lookup3.c>)
-- not a vendor algorithm.** Confirmed by matching, instruction-for-
instruction, the exact rotate constants of Jenkins' `mix()` macro (4, 6, 8,
16, 19, 4) in the streaming/block path (`FUN_4015aa20`) and `final()` (14,
11, 25, 16, 4, 14, 24) in the finalizer (`FUN_4015a6dc`), and the classic
seed pattern `a=b=c=seed+0xDEADBEEF` (`-0x21524111 == 0xDEADBEEF`) in
`FUN_4015abb2`'s init. For our fixed 508-byte (`0x1FC`) input this is the
"aligned, length%4==0" path: 42 full 12-byte `mix()` rounds, one trailing
4-byte word folded into `c` (case 4 of `FUN_4015a6dc`'s switch), then
`final()`. **[V]**: the algorithm identification is solid (the constants are
exact and too specific to be coincidence); a byte-exact Python
reimplementation has not yet been written or round-tripped against a real
firmware-computed checksum (needs a card that actually reaches this code
path -- see finding 07's new stall section -- to capture ground truth).

**Consequence for `tools/plusdrive.py`:** even once the finding-07 boot
stall is fixed, `FUN_4015a450` will reject any image this tool builds today,
because sector `0x5D8000` is all zero (never written). The tool needs to
also write this superblock, with a byte-exact `hashlittle` implementation,
for the real mount path to ever set `_DAT_44f2bd68 = 1`.

**Implemented.** `tools/plusdrive.py`'s `hashlittle()`/`build_superblock()`
now write this sector; `build()` calls it unconditionally. The checksum
routine was transliterated instruction-for-instruction from
`FUN_4015aa20`/`FUN_4015a6dc` (not reconstructed from the textbook
lookup3.c, since this variant's seed step omits the length term the
textbook one-shot version adds) and checked bit-for-bit against 44 real
calls to the firmware's own `FUN_4015abb2`, executed via a bounded
`emu.harness.call` on `snapshots/dt2-1.16/running.snap` -- lengths 0-29,
508 (the real superblock range), 511, 512, 600, 1199-1201 (either side of
`FUN_4015aa20`'s 0x4b0-byte streaming-chunk boundary) and 1211, 2401, with
random content. Every result matched. Two firmware quirks this uncovered,
both reproduced in the Python: (1) the seed is `initval + 0xDEADBEEF` with
**no length folded in**, unlike lookup3.c's one-shot `hashlittle`; (2) a
zero-length input's tail `switch` jumps straight past `final()`, returning
the raw seed unfinalized -- `tests/test_plusdrive.py` has the golden
vectors and the finalize-skip test. **[V]**.

Fields at superblock offsets `0x2c`/`0x30` (table above) are now also
resolved: they are two runtime-allocated physical sector numbers, each the
start of a 44-page run reserved by two calls to `FUN_40155698(0x2c,...)` in
the writer. Their consumer, `FUN_4015ae12` (run at both format- and
mount-time, right after the magic/checksum/version check succeeds), scans
exactly the sector range `[0x5ee980, 0x5eff80)` -- 88 pages, i.e. the same
two consecutive 44-page runs -- so the literal values are `0x5ee980` and
`0x5ef480` (confirmed by exact arithmetic, not just plausible-shaped
numbers). What that scan is *for* remains open (see below); it doesn't
block mount or a correct root listing either way, since our image leaves
that whole range zero (all-zero words read as "nothing cached", not a
crash -- `FUN_4015ae12` only extracts bit-0-set words into a lookup table).
Fields `0x24`/`0x28` are writer-side literals, not runtime-derived:
`0x24 = PAGE / SECTOR` (sectors per 32 KiB page) and `0x28` is the literal
argument the writer itself passes to one `FUN_40155698` call, kept as-is.
None of these five fields (`0x24`/`0x28`/`0x2c`/`0x30`, plus the whole
middle of the sector) are read by the mount path itself
(`FUN_4015a450`/`FUN_4015a124` only check magic/version/checksum), so
getting them wrong cannot fail a mount -- they're written for fidelity to
the real format, not because mount depends on them. **[V]** for what reads
them and where the values come from; **[O]** still for what the
`FUN_4015ae12` scan's cached table is actually used for downstream.

## `SampleManager` holds two `Directory` implementations, not one **[D]**

`SampleManager`'s real constructor is Ghidra-mislabeled as
`std::_Sp_counted_ptr_inplace<SamplePoolDirectory,...>::ctor_dtor`
(`out/ghidra/dt2-1.16-emac/decomp/40029e7a_...ctor_dtor.c`; its own leading
comment says it "loads the vtables of SampleManager,
`_Sp_counted_ptr_inplace<FileSystemDirectory,...>`,
`_Sp_counted_ptr_inplace<SamplePoolDirectory,...>`" -- i.e. this function
*is* `SampleManager::SampleManager`, not `SamplePoolDirectory`'s). It
constructs **both**:

- a `FileSystemDirectory` (the real, path-addressable filesystem this
  document and `tools/plusdrive.py` target -- region 2), stored at
  `this+0x81`/`+0x82` (a `shared_ptr` pair), and
- a `SamplePoolDirectory` (a *different* class from the `MmcFs` pools --
  region 1), stored at `this+0x83`/`+0x84`,

then picks **one** of the two as "active" (`this+0x7f`) based on a boolean
(`param_5` in the deepest constructor signature) that could not be pinned
to a concrete literal by static reading alone: the intermediate forwarding
layer, `std::_Sp_counted_ptr_inplace<SampleManager,...>::ctor_dtor`
(`0x4019baa6`), reads its own extra arguments via `in_stack_...`
pseudo-variables that Ghidra's decompiled C does not show being passed by
its only caller, `FUN_4019bb28` -- confirmed by reading
`out/ghidra/dt2-1.16-emac/disasm/4019bb28_FUN_4019bb28.s` directly: the
decompiled C shows one literal `0` argument where the raw disassembly pushes
several more stack words. **This mismatch means the decompiled call-argument
counts for this whole `_Sp_counted_ptr_inplace<X>::ctor_dtor` family cannot
be trusted without checking the disassembly.** [O]: which `Directory` is
actually active for the sample browser -- static reading was inconclusive;
needs a live read of `this+0x7f` against `+0x81`/`+0x83` in a booted
snapshot (blocked on finding 07's boot stall).

`FileSystemDirectory::ctor_dtor` itself
(`out/ghidra/dt2-1.16-emac/decomp/40159ca0_FileSystemDirectory__ctor_dtor.c`)
does no disk I/O -- it just sets the directory's current id (via
`FUN_401598ec`, presumably `2`, the root) -- so construction alone proves
nothing about mount state; readdir only happens on actual navigation
(`FUN_40157072`, already in this document).

## Open questions

- Exact semantics of record offsets 0x00 bit0, 0x01 bit0, 0x0c bit0
  (candidates: is-directory, protected/read-only, some third flag) — not
  disambiguated from MAIN_OS code alone.
- The RAM-resident record ids (`0x1000000`-`0x1000800`, table at
  `0x47d73ed0`) and RAM-resident page ids (same range for pages, table at
  `0x46f63ed0`) — not traced; unclear if they're a boot-time cache of disk
  content or synthetic pseudo-entries, and whether the sample path ever uses
  them.
- The second content-page region at sector base `0x458000` — purpose
  unresolved.
- `0x10001` index page's exact byte layout beyond the count field and the
  4-byte-per-entry packed pointer array — inferred from the read pattern,
  not cross-checked against a real populated directory.
- The RPC-opcode dispatcher connecting `elektron::MidiRpcFsSample
  WriteFileV1/V2`/`CreateDir` (whose own vfuncs are ctor/dtor boilerplate)
  to the generic `File`/`FileSystemDirectory` layer — not located; doesn't
  change the "raw write, no interpretation" conclusion since `File::write`
  is the only place bytes could be inspected.
- On-disk sample payload format (verbatim WAV vs. something converted at
  load time) — the single biggest open question; needs an emulator trace of
  an actual "load sample from browser into track" action, or the untraced
  `SampleManager::vfunc_30`-`vfunc_45` / `SampleLoaderBgWorker` read path.
- Whether firmware ever re-validates the two filesystem bitmaps
  (`0x5d8040`, `0x5d80c0`-`0x5d8180`) against record/extent data outside the
  allocator (a mount-time consistency check) — not searched for.
- The sector-`0x5D8000` superblock's fields beyond magic/version, and a
  byte-exact `hashlittle` implementation for `tools/plusdrive.py` to write
  a valid one — **answered**, see the new section above: implemented and
  checked bit-for-bit against 44 real firmware executions. `out/plusdrive/dt2.img`
  now carries a superblock that self-verifies (`build_superblock()`'s own
  checksum recomputes to the same value; confirmed against a freshly
  rebuilt image with `tools/plusdrive.py ls`).
- **`FUN_4015a450` (mount) was never called at all on a `--card-image`
  boot, independent of the superblock's correctness — this is now fixed
  and verified, but a separate, deeper emulator hang was uncovered right
  behind it.** `FUN_400cc864` (the boot task that calls `FUN_4015a450`/
  `FUN_4015a424`) gates that whole call behind `_DAT_42940a48 == 0`, set
  from an eMMC identity check (`FUN_4012dc80`→`FUN_4012db90`[build a
  struct]→`FUN_4012dbe0`→`FUN_4012da2c`×2, a scan of a 7-entry
  manufacturer/product-name whitelist at `0x402b4a24`-`0x402b4b04`, values
  `0x11`×4/`0x15`/`0x70`×2, names like `"004GE0"`, eMMC part numbers).

  **The fix, confirmed correct by execution.** Disassembling (not
  decompiling) `FUN_4012db90`/`FUN_4012d4b2`/`FUN_4012db32` found the
  decompiler had misread the manufacturer-ID read as `_DAT_44e3fe9c & 0xff`
  (a byte mask on a 32-bit load); the real instruction is `mvz.w
  (DAT_44e3fe9c).l,D0` (a 16-bit load) then `mvz.b D0b,D0` (keep the word's
  low byte) — i.e. the byte actually compared is at `0x44e3fe9d`, which is
  CID RSP3 bits `[23:16]`, exactly where a real CID's manufacturer ID
  (JEDEC CID\[127:120\]) lives. `emu/esdhc.py`'s existing
  `self.cid = [0, 0, 0, 0x00110000]` already put `0x11` there — so the
  manufacturer half of a prior, incomplete fix attempt was already right;
  only the product name was missing. The product name is 6 ASCII chars,
  RSP2's 4 bytes followed by RSP1's top 2 bytes (`FUN_4012db32`,
  confirmed by disassembly), compared with `FUN_40189ff4` (literally
  glibc's word-at-a-time `strcmp`). `emu/esdhc.py`'s `Card.cid` now encodes
  `"004GE0"` there (`self.cid = [0, 0x45300000, 0x30303447, 0x00110000]`),
  one of the four valid names for manufacturer `0x11`.

  A second, nested check inside `FUN_4012dbe0` (only reached once the first
  matches) compares the caller's own struct fields 2-4 -- a 24-bit
  big-endian pack of EXT_CSD bytes `0x9C:0x9D:0x9E` (`FUN_4012db90`) plus
  bytes `0xDE`/`0xE3` -- against the *same matched row*'s own fields 2-4
  (`0x1D8`/`0x01`/`0x08` for "004GE0"); `emu/esdhc.py`'s `ext_csd()` now
  sets those three fields (previously a single `PARTITIONS_ATTRIBUTE` byte
  fixed at `1`). A third, final check compares a computed capacity against
  one of that row's own two capacity constants (`0x760000` or `0x3B0000`
  sectors), selected by EXT_CSD byte `0x98` (`FUN_4012da80`; previously
  modeled, under the name `SLC_OK`, as an unrelated "MMC NOT IN SLC MODE"
  flag some *other*, now-stale-cited address wanted `1` for -- see
  `emu/esdhc.py`'s `SLC_OK` comment for the full correction). **The
  capacity constant can't be reached through EXT_CSD's real `SEC_COUNT`
  field**: encoding it there so a native 32-bit big-endian load recovers
  `0x760000` (this project's existing default card capacity, needed since
  it's smaller than +Drive's own layout) made a real cold boot **hang**
  outright, confirmed by bisection (`emu.dspboot.run` directly, reverting
  only that one field's byte order) -- some other consumer of the same 4
  bytes evidently depends on the JEDEC byte-serial (little-endian)
  convention this project's `SEC_COUNT` already used, and a ~30x larger
  value there stalls something indefinitely. So EXT_CSD's `SEC_COUNT` is
  now left at a fixed `0` (a real, if less common, eMMC behaviour: no
  extended-capacity report), and the smaller capacity constant
  (`0x3B0000`) is supplied instead through the CSD-1.0 fallback formula
  `FUN_4012d4b2` computes from `_DAT_44e3fe84`/`fe88` (a second CMD9/
  SEND_CSD copy) when `SEC_COUNT` reads as 0 -- reproduced exactly (down to
  matching the disassembled bit-shift sequence in Python) and set via
  `Card.csd`'s `CSD_RSP1`/`CSD_RSP2`. **Verified this whole chain is
  correct with a bounded, isolated call to the real `FUN_4012dc80`**
  (`tests/test_esdhc_identity.py`, gated on `DT2_SYX` since it needs the
  real firmware image): built from `emu.esdhc.Card()`'s own live CID/
  EXT_CSD/CSD values (not hand duplicated), it returns D0=0. A live,
  instrumented cold boot (code hooks on every function in the chain, not
  just the unit test) confirms the *exact same* result happens for real:
  `FUN_4012dc80` runs to completion and returns 0, having read
  `fe7c=1 fea0=0x3b0000 fe88=0xa03 fe84=0xafc00380` -- byte-for-byte the
  intended values -- immediately after a real `CMD9`/SEND_CSD exchange.
  **This part of the fix is done and correct.**

  **The boot-task hang immediately after the identity check is now fixed --
  root cause was an emulator wiring gap, not firmware or image content.**

  Two corrections to this file's own earlier passes at this, from
  disassembling the real code (addresses were off by exactly `0x400` in an
  earlier pass, and a `bra.b $-2` at `0x400CC7AE` belongs to an unrelated,
  low-priority task idling normally, confirmed by reading its own TCB live
  -- not evidence of a hang by itself). The real call sequence, read from
  `out/ghidra/dt2-1.16-emac/disasm/400cc864_FUN_400cc864.s`, is: identity
  check succeeds -> `_DAT_42940a48` stored at `0x400ccb5a` -> `FUN_400f0628`
  (`0x400ccb6a`) -> ... -> `FUN_4015a43a`/`FUN_4015a450` (`0x400ccbe6`/
  `0x400ccc00`). An instrumented boot following the boot task's own TCB
  (not a whole-image PC sample) showed it enter `FUN_400f0628`, issue a
  `CMD18(0x40000)` read, and then legitimately block in `sem_pend`
  (`0x4000141a`) forever. `tools/guirun.py`'s own `sem_pend`/`pend_b` hook
  pattern, reused directly, named the exact semaphore: after several
  ordinary `cmd_sem`/`data_sem` waits that all resolved normally, the last
  pend was on `sd_dma_sem` (`0x44e3feb8`) from inside `FUN_4012e0c0` (the
  CMD25/multi-block-WRITE primitive) -- a **write's** DMA completion that
  never arrived.

  **Root cause: `emu/dspboot.py`'s own `Esdhc(...)` construction never
  passed `dma_sem`.** `emu/longrun.py`'s construction of the same class
  always has (`dma_sem=profile.sd_dma_sem`); `emu/dspboot.py`'s (used only
  for the *initial* cold-boot pass, i.e. exactly where this hang occurs)
  passed `cmd_sem`/`data_sem` but silently left `dma_sem` at its `None`
  default. `Esdhc._post(None)` is a documented no-op (see its own
  docstring), so the guest's real eDMA-channel-59-completion `give` this
  model performs on a write's behalf (see `vector_192_handler`'s own
  disassembly below) never lands, and any multi-block CMD25 write's own
  `sem_pend(sd_dma_sem)` -- reads, which pend on `data_sem` instead, are
  unaffected -- blocks forever. **Fixed**: pass `dma_sem=profile.sd_dma_sem`
  in `emu/dspboot.py`, matching `emu/longrun.py`. New regression test,
  `tests/test_dspboot_esdhc_wiring.py` (`DT2_SYX`-gated); the `Esdhc` model's
  own CMD25-posts-`dma_sem` behavior was already covered by
  `tests/test_esdhc.py`'s `test_cmd25_consumes_host_buffer_and_is_visible_to_cmd18`
  -- this was purely a construction-site wiring gap, not a model bug.

  For completeness (per the coordinator's own ask), the real firmware's
  eDMA-channel-59 completion ISR, `vector_192_handler`
  (`out/ghidra/dt2-1.16-emac/disasm/4012d3c4_vector_192_handler.s`, 40
  bytes) is trivial: `move.b #0x3b,(EDMA_CINT)` (acks/clears channel 59's
  own interrupt request) then unconditionally `give_b(sd_dma_sem)` -- no
  IRQSTAT/TCD inspection at all, confirming this model's own
  synchronous-completion shortcut (posting straight from `Esdhc.command()`,
  never actually raising vector 192 -- see `emu/longrun.py`'s own comment
  on this) is a faithful enough simplification of what the real ISR does;
  the bug was purely that this specific construction site left the target
  semaphore address as `None`.

  **With this fixed, the boot progresses dramatically further** (a cold
  ladder to 400M instructions now creates 10 tasks, including the
  priority-6 Main-OS task, `entry=0x400337ba`, that finding 07's own
  branch-A/B/C investigation names) **and `_DAT_42940a48` (the identity
  check's own gate) reads `0` at the boot task's own terminal idle loop
  (`0x400cccd8`, confirmed unmoving after 1B total instructions) -- i.e.
  `FUN_4015a450` (mount) is now definitely called.** It still does not
  succeed: `_DAT_44f2bd68` (the mount flag) stays `0`.

  **A third, previously-undocumented on-disk structure blocks mount's own
  success, past the superblock at `0x5D8000`.** `FUN_4015a450`'s own
  `FUN_4015a124(0)` call (already read in this file's earlier section) goes
  on to call `FUN_4002cd6a`, which -- via `FUN_4002ccd0` -- reads **32 KiB
  from sector `0x458000`** (finding 14's own earlier table already flagged
  this exact sector as "purpose unresolved") and checks: magic
  `0x4D61476A` ("MaGj") at offset 0, then a CRC-32 (`FUN_4013e06c`, IEEE
  polynomial `0xEDB88320`, confirmed by its table-init loop matching the
  standard algorithm byte-for-byte) over a length-prefixed span starting at
  offset 8, chained into a second CRC-32 pass over 4 more bytes at offset
  4, expected to equal the literal constant `0xDEBB20E3`. All zero in our
  image (never written), so this check fails, `FUN_4002ccd0` returns 0,
  `FUN_4002cd6a` returns `0xFFFFFFFF`, `FUN_4015a124` returns `-1`, and
  `FUN_4015a450` returns failure without ever setting the mount flag.
  **[O], not yet implemented**: the exact byte widths of the two small
  fields at offsets 8 and 10 (`_DAT_47e203d4`/`_DAT_47e203d6`, read as if
  overlapping -- the same decompiler unreliability this file has already
  hit more than once, see the CID/EXT_CSD section above) were not pinned
  down by disassembly this session, so the exact bytes needed to make the
  chained CRC land on `0xDEBB20E3` were not computed; this needs either a
  disassembly read of `FUN_4002ccd0`/`FUN_4013e06c` (not just the
  decompile) or a bounded, isolated call to `FUN_4002ccd0` on a candidate
  buffer this tool constructs, the same technique
  `tests/test_esdhc_identity.py` already used for the CID/EXT_CSD chain.
  Until this is written, no card-image boot reaches a valid mount, and item
  3 (browser lists `hat.wav`) still can't be demonstrated end-to-end.
- Which `Directory` (`FileSystemDirectory` vs `SamplePoolDirectory`)
  `SampleManager` actually browses — **answered**: a live crash trace
  (see `docs/findings/07-emulator.md`'s corrected section below) found
  `SampleManager`'s "currently selected" item is a `FileSystemDirectory`
  instance (vtable `0x4022584c`) in both a cardless boot and a
  `--card-image` boot, and it is not yet `valid()` (its `vfunc_12`, a
  flag at offset `0x120`, reads false) in either case — i.e. this project
  has never observed a `FileSystemDirectory` that successfully mounted.
- Booting with *any* already-formatted card (ours or the firmware's own)
  currently stalls before `running` for a reason unrelated to +Drive
  content — see `docs/findings/07-emulator.md`'s new section. This blocks
  items 2 and 3 of the +Drive-in-RAM goal (reading the live `Directory` for
  `hat.wav`, saving a running card snapshot) until it is fixed.
- Separately, once `running` is reached, opening the sample-pool list or
  the SampleManager/+Drive browser screen itself (SRC, encoder, FUNC, YES)
  panics the UI task — also confirmed unrelated to +Drive content or the
  eSDHC model (identical crash with `--card-image` omitted). **Corrects
  an earlier framing in this file and in `docs/findings/07-emulator.md`**:
  this is not a resource/glyph decode-cache miss. It is an uncaught C++
  exception (`std::logic_error("basic_string::_S_construct null not
  valid")`, i.e. `SampleManager::vfunc_40` builds a `std::string` from a
  null `const char*` with no null check) thrown because the selected
  `FileSystemDirectory` reports itself invalid (see above) and its
  "get display name" accessor (`FUN_4015996e`) returns a raw `NULL` for
  that case instead of the empty-string fallback it already uses for its
  other failure case. See `docs/findings/07-emulator.md`'s "Opening the
  sample-pool list or the +Drive browser panics the UI task" section for
  the full root-cause trace and why this most likely traces back to the
  mount gap above. It still blocks reading the live `Directory` through
  this screen; a direct RAM read of the mounted `Directory` object
  (bypassing the browser UI) remains the open path to confirming `hat.wav`
  is listed.
