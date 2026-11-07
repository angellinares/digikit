# The digikit module store

Status: built (builder and reader in `packages/emulator/plusdrive/src/store.rs`, runtime in `modules/core/runtime/sdk/src/store.rs`), 2026-10-16; version 2 (two header copies, for
module upload, `docs/design/digikit-upload.md`) 2026-10-18; **the settings record** (a fixed area of 32 sectors before the selection record: header copy B moved from `sectors - 3` to `sectors - 35`, runtime 0.5.1) 2026-10-25; **version 3** (a per-device area of 82 MiB / 22 GiB, folders, reusable space, 159-byte paths, a streamed table; runtime 0.3.0) 2026-10-22; **version 4** (the table's hash slot holds the +Drive content hash, not a SHA-256; capability FILEHASH) 2026-11-07.
Plan: `docs/plans/storage.md`. Area choice: `docs/findings/59-storage-regions.md` (the older range choice, `docs/findings/23-digikit-runtime-spikes.md` S6, is superseded).

The store holds the files of `plus_drive/<device>/digikit/` on the card (modules, and samples on the DN2), in sectors the firmware never writes, so the
runtime can read them with the firmware's block read (DT2 1.16 `FUN_4012deda`, DN2 1.11 `FUN_4012c59a`; arguments sector,
byte count, buffer; the buffer 16-byte aligned, the count a multiple of 512).

## Where

The area comes from the device profile (`devices/<device>.toml`, `[card.store]`): sectors `[first_sector, end_sector)`, different per device and always
above the MmcFs write guard at sector `0x457FFF` (finding 59):

| Device | `first_sector` | `end_sector` | Size |
| --- | --- | --- | --- |
| DT2 | `0x2DF1000`, after the +Drive | `0x2E1A000`, the card end | 82 MiB |
| DN2 | `0x800000`, above everything the Data API reaches | `0x2E1A000`, the card end | about 22 GiB |

`[card] total_sectors` is the card the firmware accepts (the pSLC 32 GB part, 0x2E1A000 sectors); the area ends with it. A card the runtime finds
smaller than the area (its last sector does not read) has **no store**: nothing is read or written there, uploads answer `E_NOSTORE`, and the boot goes on
(`CARD_TOO_SMALL`, a `CARD_CHECK` event). Sector numbers below are relative to `first_sector` unless said otherwise. A device with no `[card.store]` takes no
`digikit/` files (the builder reports them as skipped).

The area is not a path filesystem: **folders are in the paths.** The runtime's modules are in `modules/`, samples (DN2) in `samples/`.

## Format

All integers are big-endian. Sector size 512. Version 4 is the only version: a header of another version is not a store (pre-1.0: no migrations;
the version 1 and 2 stores lived in the old area `0x38000..0x3E000`, which no code uses any more).

```text
sector 0                       header copy A
sectors 1 .. header_b - 1      tables and file data, anywhere: free space is reused
sector header_b = sectors - 35 header copy B
sectors - 34 .. sectors - 3    the settings record, two copies of 16 sectors (below; not a store structure)
sectors - 2, sectors - 1       the selection record, two copies (below; not a store structure)

header (one sector)
  0   4   magic "DKST"
  4   4   u32 version = 4
  8   4   u32 entry_count
  12  4   u32 table_sectors      = ceil(entry_count / 2)
  16  4   u32 table_first        relative to the store; 1 when there is no table
  20  4   u32 table_crc          crc32 of the table's sectors, zero padding included (IEEE, as zlib: "123456789" gives 0xcbf43926); 0 for no table
  24  4   u32 seq                sequence number of this header
  28  4   u32 used_sectors       header A, the table and every file
  32  4   u32 area_sectors       the store's size in sectors when it was written (sectors, not limit): a store made for another area is not this one
  36  ..  zero
  508 4   u32 crc32 of bytes 0..508

table: entry_count entries of 256 bytes, sorted by (first_sector, path in byte order)
  0   160 path, NUL padded (relative to digikit/, "/" separated, at most 159 bytes)
  160 4   u32 first_sector       relative to the start of the store; 0 for an entry with no data
  164 4   u32 size_bytes
  168 32  file hash slot: the +Drive content hash of the file bytes `| 1` (`dksample::content_hash`, a lookup3 variant), 4 bytes big-endian, then 28 zero bytes; all zero for a folder
  200 1   u8 kind: 0 file, 1 folder with nothing in it
  201 55  zero
```

Rules: a path is 1 to 159 bytes of printable text (0x20 to 0x7E, no NUL), with no empty, `.` or `..` part; two entries may not have the same path; a path that is a
file may not also be the folder of another entry (`a` and `a/b`). A **folder entry** (kind 1, size 0) exists only for a folder with nothing in it (made by the
host with MKDIR; the commit that stores a file or a folder inside it drops it, and a folder whose last file is deleted is gone, since folders with files are implied); a folder that has files is implied by their paths. A file or folder with no data (an empty file, a folder) has `first_sector` 0 and takes no sector; it sorts before every file with data. A file is at
most 4 GiB - 1 bytes; each file starts on a sector boundary and takes `ceil(size / 512)` sectors.

The tail, `store::TAIL` = 35 sectors (header copy B, the settings record's 32, the selection record's 2), is never a file's: `limit` = `sectors - 35` (`store::Layout::limit`, `header_b`), the allocator
(`gaps`, `first_fit`, `write_sectors`) works inside `[1, limit)` and reserves `[header_b, sectors)`, and the free-space figures count the tail as used.

**A store written before 2026-10-25** has its header B at `sectors - 3` and its allocator ended there, so a file or the table of a nearly full store may lie where header B and the settings copies are now.
`store::open` (with the table check, once per header change) therefore refuses a header copy whose table or any file reaches into the tail (`store::reaches_tail`), as it refuses a table that fails its CRC; a header
that says `used` or the table past `limit` is not usable (`plausible`). The result: an old store that does not reach the tail works as before (header A is usable, header B is not read at its old sector and the next commit
or the repair at BEGIN writes it at the new place, over free space); an old store that does reach the tail is **not used at all** (no usable copy: uploads answer `E_NOSTORE`, nothing loads from it, CLEAR still works and makes a
new store) and **the settings store is off** (below). The runtime never writes header B or the settings area over a file: a commit needs a verified store, and a verified store has nothing in the tail. The old header B at
`sectors - 3` is the last sector of settings copy B; nothing reads it as a header, and a settings record of the full 8 KiB overwrites it. This case needs about 82 MiB of files on a Digitakt II (finding 63).

A header copy is **usable** when its magic, version, CRC, area and counts hold (`table_sectors` = ceil(count / 2), the table and `used_sectors` inside the area). A reader
takes the usable copy with the higher `seq` (equal: A), and, when it checks the table's CRC (once per header: a runtime that wrote the table itself skips it), skips a copy whose table
does not check for the other one. An empty store (CLEAR, or every file deleted) is a valid store with `entry_count` 0 and no table.

**The table is never held whole.** It is read and written through small buffers (the runtime's: 8 KiB), so the number of entries is bounded only by the room on the card
(two entries per sector). A lookup is one pass over the table; a list starts at the sector of its first entry.

**Space is reused.** Free space is every gap between used extents: header A, the table, the files' data and the tail (header B, the settings record and the selection record). The runtime
allocates a file's extent in the first gap that holds it (first fit) and, on every commit, writes the new table into the first gap that holds it, never over the table in use. A host's
free-space figure (`used_sectors`, INFO's free bytes) is the sum of the gaps less the room for the table of one more entry; **a file larger than the largest gap is refused with
`E_FULL` even when the sum is larger** (the free space is not one piece).

## Commits and power cuts

A commit (the end of an upload, a delete, a clear) streams the old table through two small buffers into a gap, with the entry inserted, replaced or removed, reads the new
table back and checks its CRC, and then writes header B and header A with `seq + 1`. Nothing in use is overwritten before the headers move, so a cut at any point leaves the old store or
the new one, never a mix (`dkrt_sdk::store` tests cut a commit after every write). A file's data is written to its extent before the commit; an upload that is cut is a gap that nothing names.

**Both copies agree before space is reused.** After a cut between header B and header A the copies differ: A lags and still names the previous table. That is harmless while nothing is
overwritten, but the next commit writes a table into free space, which may be exactly the sectors of that old table; if its header B then tears, the lagging A would be the only usable copy and
name overwritten sectors. So BEGIN, DELETE and CLEAR first rewrite the copy that lost the `seq` comparison (missing, torn or different) from the winner, and refuse if that write fails
(`docs/findings/43-digikit-upload.md`, "The CLEAR double fault").

**A reader that runs while an upload commits** (the menu's, in the UI task, against the MIDI task) reads a fingerprint of the two header sectors before and after it reads the table, and
starts again when it changed (`dkrt_sdk::store::stable`); no lock. A table is never overwritten in place, so a reader that began with the old header reads a complete old table, and only a
second commit during one read could reuse its sectors (the fingerprint catches that too).

### A store of another version

A header sector with our magic and a version other than 4 (a version 3 store, which has SHA-256 in the hash slot) is **not a store**, and nothing reads or migrates it: `store::find`, LIST, the boot's loads and a sample load see an empty store, the
card check still calls the card ours, and the first upload writes version 4 headers into both header sectors and reuses the old table and file sectors as free space (the old bytes are overwritten; files of the old store
are uploaded again). What stays on the card: the **selection record** and the **settings record** are not part of the store structure, so they are kept as they are, and the next boot loads the modules the kept selection names once their files are uploaded again.
Foreign bytes in a header sector are still refused (`E_NOSTORE` for the upload commands, the area is somebody's data), and a version 4 header that fails its checks is a damaged store, refused as before (`E_NOSTORE`, CLEAR works).
`patchkit store dump` and `plusdrive store ls` refuse a version 3 store. Test: `modules/core/runtime/tests/store-format.device.test.ts` (both devices), `dkrt_sdk::store` unit test `a_store_of_another_version_is_no_store_a_damaged_current_one_is_refused`.

## Who writes and who reads

- **The builder** (`plusdrive build`, the desktop card job): every regular file under `digikit/` (recursive; symbolic links and `.DS_Store` skipped; an empty folder is not built: folder entries are made by the runtime and exported). It writes both header copies (`seq` 1) and lays the data out in path order after the table. An image that holds store files or a selection reaches the end of the area (a sparse file).
- **The exporter** (`card.export`, the end of a session) reads the store and writes its files back to `digikit/` (a folder with nothing in it too). The firmware never writes these
  sectors; only the runtime's upload does. A path-filesystem entry named `digikit` that the firmware's user made on the device is exported as `digikit~<id>` so it cannot land in the store folder.
- **A reader** for tests: `plusdrive store ls IMAGE [--device dt2|dn2] [--cat PATH]` prints `path first_sector size hash` by path (a folder with nothing in it as `path/ - 0 folder`; `--cat` writes one file's bytes).
  The runtime reads it with the firmware's block read; `mise run plusdrive:check` calls that function on a booted DT2 and DN2 and compares the header, table and every file with the image and the folder.
- **The runtime** (`modules/core/runtime`, Rust): `dkrt_sdk::store` is the format, the reader and the one writer; `upload.rs` is its commands (docs/design/digikit-upload.md); the device's area comes
  from its device object (`dkrt_store_first`, `dkrt_store_sectors`, written by `patchkit gen` from the device file). The runtime's one card-write function refuses a write outside the area (`E_RANGE`).

## `digikit/` and the path filesystem

`digikit/` is not also copied into the DT2 path filesystem. Reasons: the folder would show in the sample picker (finding 23 S3), which lists every file with no filter; two copies would need two places to keep equal;
the DN2 has no path filesystem, so the runtime needs the store anyway; and a path-filesystem copy is read by file name through the mutex-guarded C++ layer where the block read is a single call. The cost is that a
`digikit/` file is not visible on the device itself. **DT2 samples** are different: they belong in the sample browser, so they go to the +Drive (through the stock MidiRpc file commands, `docs/MIDI-SYSEX-RPC.md` section 12);
**DN2 samples** go to the store's `samples/` folder (the firmware cannot play them yet).
A sample in `samples/` is the DT2 native sample file, byte for byte what the +Drive holds (`packages/dksample`: a 64-byte header, the 48 kHz 16-bit PCM, a 16-byte zero trailer; the header's +4 is the PCM length), so a sampling engine reads one format on both devices. The host sends it through the same upload commands as a module (`packages/dk-session/src/samples.ts`, exported as `@digi/dk-session/samples`, backend "digikit"): `samples/<folder>/<name>`, no extension, and checks it with VERIFY (the host's file hash of the bytes it sent against the runtime's file hash of a fresh card read).

## The selection record (boot-time loading, plan decision 9)

The runtime keeps one writable record in the store range: which modules to load at the next boot, and the boot counter of safe
mode. It is the only thing the runtime ever writes to the card (finding 37). Layout fixed 2026-10-17; the +Drive round trip
(lane D) and the runtime (`modules/core/runtime`, `dkrt_sdk::DkrtSelection`) both rely on it.

**Where.** The last `selection_sectors` (2) sectors of the area (`devices/<device>.toml` `[card.store] selection_sectors`):
copy A at `end_sector - 2`, copy B at `end_sector - 1` (absolute sectors `0x2E19FFE` and `0x2E19FFF` on both devices: the area ends with the card). The store builder
keeps its files out of them (its size limit is `area - 35`: header copy B and the settings record are before them); a reader of the store ignores them. They are
zero (no record) on a card the builder made.

**Record** (one sector, all integers big endian):

```text
  0   4   magic "DKSL" (0x444b534c)
  4   4   u32 version = 1
  8   4   u32 seq             sequence number of this write
  12  4   u32 boot_counter    boots begun that did not stay up for the stable period (safe mode: 2 or more; below)
  16  4   u32 flags           bit 0: start in safe mode once (set by the menu, cleared by the boot that honours it)
  20  4   u32 count           selected modules, 0 to 8
  24  4   u32 stable_blocks    the stable period of this card in DSP blocks; 0 = the runtime's default, 45,000 (30 s)
  28  4   sticky type map, entry 0 (see below)
  32  448 8 paths of 56 bytes: store paths ("modules/dyn-pluck.dkm"), NUL padded, at most 55 characters, load order
  480 28  sticky type map, entries 1 to 7 (4 bytes each)
  508 4   u32 crc32 of bytes 0..508 (IEEE, as the store table)
```

**The sticky type map** (since 2026-10-20, runtime 0.2.0, docs/design/module-sdk.md 11.5 D7): up to 8 entries, each 4 bytes big endian, `hash24 << 8 | type`: the low 24 bits of FNV-1a 32 of the
machine's global id `<module id>:<machine id>` and the run-time machine type (16 to 23) the runtime gave it; all zero = empty. Entry 0 is bytes 28 to 31, entries 1 to 7 are bytes 480 to 507. A card
that was written before the map existed has zeros there (an empty map). The boot's assignment reads the map, the boot hook writes it back once after the loads when it changed (`docs/design/digikit-registry.md`,
"Machine keys and sticky type numbers"), and every other writer of the record (the menu's save, `SELECT`, the DEBUG and counter updates) carries it over. The builder writes a zero map.

**Reading.** Read both sectors; a copy is valid when the magic, version, count (<= 8), path NULs and CRC hold. Use the valid copy with
the higher `seq` (equal: A). No valid copy is an empty selection with counter 0.

**Writing** (`dkrt_selection_set`, the boot counter): the runtime writes with the firmware's block write (DT2 1.16 `FUN_4012e0c0`,
DN2 1.11 `FUN_4012c780`; anchor `cf.storage.fn.block.write`, hash-found on both ports; it takes the eMMC mutex itself) and only to these two
sectors. A new selection writes both copies with `seq + 1`, A first, then B, so a power cut between the two writes leaves one valid
copy (A new, B old: the higher seq wins; a torn A fails its CRC and B still holds the old record). **The boot counter** is raised at the
start of a boot that has modules selected and cleared when the session has been stable: `stable_blocks` (default 45,000, 30 s) DSP blocks
after the first pass of the UI loop, or at that first pass in a safe boot (no module runs there). The clearing re-reads the record first (the
menu may have saved a selection since the boot). A boot that dies earlier leaves the counter raised; two such boots in a row make the third a safe
boot (`docs/findings/37-boot-load.md`, "Safe mode after the first UI pass"). `stable_blocks` is a setting for tests and for people who debug a
module; nothing in the menu writes it, the builder writes 0, and the runtime carries it over when it writes the record. The boot counter changes write one
copy only: the one that is not the current record, with `seq + 1`.

**+Drive round trip.** The exporter reads both copies and writes the winner back as `plus_drive/<device>/digikit/selection.txt`
(one store path per line, load order); the builder turns that file into both copies (`seq` 1, counter 0); lines may be blank or start with `#`, the exporter keeps the user's file when it holds the same list, and the counter and flags are not exported (finding 40).

## The settings record (digikit's own settings, 2026-10-25)

Principle: **Elektron's files stay stock; every piece of digikit data that is not a module lives in our own store on the card.** The settings record holds that data in one place: a small key/value
record, not a store file (a module or a user cannot list, replace or delete it), in two copies like the selection record. Its first user is FX routing (finding 63): whether a run-time effect is a master
insert or a send effect, and the 16 send amounts of a send effect, which cannot live in the kit (the kit is Elektron's file).

**Where.** `settings_sectors` (32) sectors of `[card.store]` before the selection record: copy A at `end_sector - 34`, copy B at `end_sector - 18` (16 sectors, 8 KiB each; absolute DT2 and DN2:
`0x2E19FDE` and `0x2E19FEE`, the area ends with the card on both). The size of one copy is `[settings] sectors` of `sdk/sdk.toml`; `patchkit gen` refuses a device file whose `settings_sectors` is not two copies of it.
The runtime reads the placement from the layout (`store::Layout::set_a`, `set_b`), never from a literal. The store builder keeps its files out of the area (`StoreLayout::store_limit`).

**Record** (version 1; all integers big-endian; the decoder and encoder are `dkrt_sdk::settings`, shared with the host readers):

```text
  0   4   magic "DKST"            (the store header's magic; the store's version is a u32 at byte 4, so the two cannot be taken for each other: 3 against 0x0001xxxx)
  4   2   u16 version = 1
  6   4   u32 seq                sequence number of this write
  10  4   u32 used               bytes of the record: this header and the entries (18 to 8192)
  14  4   u32 crc32              zlib's crc32 (IEEE; "123456789" gives 0xcbf43926) over bytes 0..14 and 18..used: the header without this field, then the entries
  18  ..  entries                u8 key_len | key (1 to 47 bytes, printable ASCII 0x21 to 0x7e) | u16 value_len (0 to 1024) | value
```

The bytes after `used` are not part of the record (a shorter record written over a longer one leaves old bytes there; the crc32 ignores them). A copy is **valid** when its magic, version, `used` (18 to 8192),
crc32 and entries hold (every entry inside `used`, every key legal, no key twice). A reader takes the valid copy with the higher `seq` (equal: A); no valid copy is an empty record. The shared vectors
`sdk/vectors/settings-vectors.json` (valid records with their decoded entries and a refused record for each rule, with the first rule it breaks) are checked by the Rust SDK tests, by `patchkit store dump` and by the Python reader.

**Keys by owner.** A key is `<owner prefix><module id>` or, later, `<module id>/...`. Now: `fx/route/<module id>` (1 byte: 0 master, 1 send; absent, or a route the effect's manifest does not allow: the manifest's default `route`; written by DIGIKIT > FX ROUTES) and `fx/send/<module id>` (16 bytes: the amount 0 to 127 of each track). The
prefixes and how a reader prints their values are `[[settings.key]]` of `sdk/sdk.toml` (generated). A key of another shape is stored and printed as text or hex. **Reserved, not built:** a run-time module (a `.dkm`) will
reach only the keys that start with its own module id and a `/`; there is no module API yet (the runtime-internal functions below and the exported C functions for `core/*` modules are all there is).
Unloaded modules keep their entries: nothing removes a key but `remove`.

**Runtime rules** (`modules/core/runtime/src/rs/src/settings.rs`).

0. **Off:** at the boot the runtime opens the store first (`store::open`). A card with no store header has no files: the settings store is on. A store that opens is clean (nothing of it reaches the tail). A store whose headers do not read, or
   that reaches into the tail (see above), turns the settings store **off**: `SETTINGS_LOAD` state 4 (an error event, argument 3 is the store's status), CAPACITY `SETTINGS` limit 0 and used 0, `put`, `get` and `remove` and the SETTINGS command answer
   `E_SET_OFF` (-70), nothing is read from or written to the settings area until the next boot.
1. **Boot:** the boot hook, after the card check, reads the first sector of both copies, then the copy with the higher `seq` in full (the other one when the first fails its check), and keeps the record in RAM (`DKRT_SETTINGS`, 8 KiB).
   The log event `SETTINGS_LOAD` says what it found (state 0 none valid, 1 a copy, 2 a copy damaged and the other taken, 3 a read error, 4 off; a damaged copy is an error event). A card with no store has an empty record that is never written.
2. **Change:** `put` and `remove` change the RAM record at once (a `put` of the value the key has changes nothing and starts no write) and mark it dirty.
3. **Write:** every UI-loop pass the runtime checks whether nothing has changed for 2 s (3000 DSP blocks of the firmware's counter; 240 UI-loop passes when the image has no counter; a counter that never moves is given up after 1800 passes). Then it writes the record to
   the copy that is **not** current, with `seq + 1`, through `block_write` (the range and ownership checks of finding 46: a foreign card refuses, a blank card is claimed first), and only the sectors the record needs. A power cut in the write leaves the current copy valid and the next boot reads it. A failed
   write (not a refusal) is tried again after 2 s, three times; a refused one (`E_CARD_RO`, `E_RANGE`) is not: the record then holds in RAM until the next boot. `SETTINGS_WRITE` logs every write.
4. **Limits:** one copy is 8 KiB in all; `put` answers `E_SET_FULL` when the record would not fit (the record is left as it was), `E_SET_KEY` for a key that breaks the rules, `E_SET_VALUE` for a value over 1024 bytes. CAPACITY kind `SETTINGS` reports the area in bytes (limit 8192, used: the record's
   bytes, header included). Room for a share per module is left in the key rule (prefix by module id), not in code.
5. **Task:** the UI task (the menu, `core/fxpages`) and the mailbox test commands; not an interrupt or the MIDI task (the record is one buffer).

**API.** Runtime-internal Rust: `settings::get(key, &mut buf) -> len | error`, `put(key, value) -> 0 | error`, `remove(key) -> 0 | error`, `flush()`. Exported C for `core/*` modules (task context, the module links them by symbol like the menu links the selection functions):
`int dkrt_settings_get(const char *key, u8 *buf, u32 cap)` (the value's length, or `E_SET_NOKEY` -68, `E_SET_KEY` -66, `E_SET_VALUE` -69 when `cap` is too small), `int dkrt_settings_put(const char *key, const u8 *value, u32 len)` (0, `E_SET_KEY`, `E_SET_VALUE`,
`E_SET_FULL` -67) and `int dkrt_settings_remove(const char *key)` (0, also when the key was not there) and `int dkrt_settings_flush(void)` (write the record now when it has changes the card does not hold; `dkrt_restart` does the same before it resets, so no restart path loses a change younger than the 2 s delay; a menu calls it to show a write error first: 0, also when nothing changed, or `E_WRITE`, `E_RANGE`, `E_CARD_RO`, `E_SET_OFF`). Mailbox test commands: `CMD_SETTINGS_PUT` 9, `_GET` 10, `_REMOVE` 11, `_FLUSH` 12.

**Reading it from a device** (read only): the SysEx command `SETTINGS` 0x19 (capability bit 11): body `u32 offset, u16 length` (1 to 768); reply `u8 flags` (bit 0: changes not on the card yet), `u8 copy` (the card's current copy: 0 A, 1 B, 2 none), `u32 seq` (the card's),
`u32 used`, then the record's bytes from `offset`. It answers from the RAM record (sealed with the card's `seq` when it has changes), so a host reads one valid record in pieces; `E_SET_NOKEY` when the card has no record and nothing was put. Host: `uv run python -m tools.device.dkupload --emu|--port NAME settings [-o FILE] [--json]`.

## Reading our data locally

`patchkit store dump IMAGE [--device dt2|dn2] [--overlay FILE] [--base-sector N] [--json]` prints, from a card image, everything of ours: the store header (the copy in use, `seq`, sectors used), the file table (path, size, first sector, file hash), the selection record (both
copies, the one in use, boot counter, flags, the paths), and the settings record (both copies with their state, `seq` and size, the copy in use, every entry; `fx/route/...` as `master` or `send`, `fx/send/...` as 16 numbers; other keys as text or hex). `--json` prints the same as one object.
`IMAGE` is a card image of the emulator (a sparse file of 512-byte sectors: the DT2 +Drive card, the DN2 card, `out/cache/digikit/cards/*.img`, the card a desktop run keeps), optionally with a `card.overlay` file of the guest's writes laid over it (as `plusdrive export --overlay`), or a raw dump
of part of a real card whose first byte is the sector `--base-sector` (a dump of the area from `first_sector` to the end: `--base-sector 0x2df1000` on a DT2). `patchkit store dump --record FILE` decodes one settings record (the file `dkupload settings -o FILE` saves from a device) and needs no card.
The decoder is the runtime's own (`dkrt_sdk::settings`), so what the command prints is what the runtime reads. The Python reader `tools/device/dksettings.py` (`python -m tools.device.dksettings FILE`, also what `dkupload settings` prints) follows the same vectors.

```text
$ patchkit store dump out/cache/digikit/cards/dt2-<key>.img
card dt2: store sectors 0x2df1000..0x2e1a000, header B 0x2e19fdd, settings 0x2e19fde and 0x2e19fee, selection 0x2e19ffe and 0x2e19fff
store: header A in use, version 4, seq 1, 3 of 167901 sectors used, 1 entries
  modules/a.dkm  6 bytes  first sector 0x2  hash <8 hex digits, 56 zeros>
selection: copy A valid, seq 1, boot counter 0, flags 0, 1 modules; copy B valid, seq 1, boot counter 0, flags 0, 1 modules
  modules/a.dkm
settings: copy B in use
  copy A: sector 0x2e19fde, valid, seq 2, 47 bytes, 1 entries
  copy B: sector 0x2e19fee, valid, seq 7, 90 bytes, 2 entries  <- in use
  entries of copy B (seq 7):
    fx/route/test/dyn-fx-gain  send (1)
    fx/send/test/dyn-fx-gain  [0, 127, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 100]
```

## The folder mirror (while the emulator runs)

The exporter above runs at the end of a session (and on `card.export`). Since 2026-10-22 the emulator also mirrors the store while it runs
(`plusdrive_format::mirror`, `docs/plans/card-folder.md`): about 2 s after the guest's last card write the files of the store go to
`plus_drive/<device>/digikit/<path>` (the DN2 too: it has no path filesystem, so only these files), `card.sync` does it at once, and a file the
card no longer has is removed from the folder (to the backup), so a DT2 card built from the folder at the next start does not bring it back. One way
only: nothing is read from the folder except by the DT2 startup build. The mirror removes only paths it has seen on the card in the session, so a session
that starts on an empty card (the DN2 has none at boot) leaves the files an earlier session mirrored. The selection record is not a store file and is not mirrored.

## Open

- **[O]** The settings area has not been written on a real card (the emulator proves the rules, finding 63); the write cost (up to 16 sectors in one block write, every 2 s of changes at most) is not measured on a device.
- **[O]** The area was chosen from a static study of the firmware (finding 59), not measured on a device. The write-watch of the emulator proofs sees no firmware write in it.
- **[O]** The ownership check reads the two headers, the two selection sectors and the first 1 MiB of the area; it cannot scan 22 GiB at every boot. Foreign data further in is not noticed.
- **[O]** The emulator's state snapshots keep at most 131072 written card sectors (64 MiB) in the card overlay, so a store with more data than that cannot be saved with a state file.
- **[O]** The folder mirror and the exporter read every store file (and hash the folder's) at every pass; with samples in the store they need a cheaper comparison (step 6 of the storage plan).
- The format has a version field and no compression.
