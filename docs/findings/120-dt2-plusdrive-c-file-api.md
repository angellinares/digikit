# DT2 +Drive: the firmware's C-level file API for writes

Digitakt II OS 1.17. Static analysis (fw.py, cfdb, no Ghidra, no emulator run), 2026-11-10. All **[D]** unless marked. For the planned
`core/plusdrive-write` adapter (docs/plans/usb-fast.md section 3.1): our runtime I/O task writes samples through the firmware's own functions, so
Elektron's allocation, inode, flush and close-hash code runs unchanged. The sampler's recorder `FUN_40153c70` is a complete stock caller of this API
(delete, open "w", write, trailer, hash, close) and is the template.

## Handle

Caller-owned, 16 bytes, no heap: `{u32 id; u32 pos; u32 mode; u32 valid}` (mode 1 = "w", 0 = "r"; valid 1 = open). The same handle as the existing
`cf.storage.fn.path.open` / `read` anchors. The stock `File` object (vtable 0x40225c9c, 0x2c bytes, ctor 0x401590b2) and `FileOutputStream` (128 KiB
buffer) wrap it and are not needed.

## Functions

| Anchor (proposed) | Entry | Signature | Strict hash |
|---|---|---|---|
| `cf.storage.fn.path.open` (exists) | 0x40158902 | `i32 (const u8 *path, const u8 *mode, u32 *handle)` | f642319babf0cc8f26a18a17356a25df |
| `cf.storage.fn.file.write` | 0x4015866a | `i32 (const u8 *buf, u32 bytes, u32 *handle)` | a2c1df3d75289bf0303697618f4c92ee |
| `cf.storage.fn.file.reserve` | 0x40158810 | `i32 (u32 *handle, u32 offset, u32 bytes)` | 3da0b0458b2bd3bc920b7b28898bda58 |
| `cf.storage.fn.file.hash` | 0x4015b3aa | `i32 (u32 *handle)` | 0dfe3aa5cadab0e82643415ef4a6f0f9 |
| `cf.storage.fn.path.close` (exists) | 0x401588c0 | `i32 (u32 *handle)` | ad3982007ea053f7f633f28b1f0e5f59 |
| `cf.storage.fn.file.delete` | 0x40157078 | `i32 (const u8 *path)` | 4900c52f1741daeb3c6d135bbff647d6 |
| `cf.storage.fn.file.mkdir` | 0x4015746e | `i32 (const u8 *path)` | aa63f0091fff183d872b70744d4ec0fc |
| `cf.storage.fn.file.info` | 0x4015af4c | `i32 (const u8 *path, u32 *out)` | c43b399ff1b6f8a228646b583d335a8a |
| `cf.storage.fn.file.any_open` | 0x4015bfc4 | `i32 (void)` | 0d51b093da49feaf6446a8990289cd27 |
| `cf.storage.fn.file.rmdir` | 0x40157192 | `i32 (const u8 *path)` | d1767db87bc0ea8afc6d2721641c3786 |

All take the recursive filesystem mutex 0x46f4deb0 themselves and return 0 (write: the byte count) or -1. They block (mutex, eMMC, the hash read-back),
so task context only, never an ISR. No task-local state was seen; shared static scratch (0x4066d8c0, 0x406658c0, 0x44f2bc68) is used under the mutex.
Rename by path is the wrapper `FUN_40156f6c(old, new)` (anchor `file.rename`): it takes the mutex, calls `FUN_40156b00(old, new, 0)` to add the new directory entry for the same inode and `FUN_40156e94(old, 1)` to remove the old entry. `FUN_40156b00` refuses a source that is missing, a folder or open (the open bitmap), and its third argument only turns on a limit of 2000 entries in the destination folder; the sibling wrapper `FUN_40156ffc` passes 1 and adds the entry without removing the old one. Rehash by path `FUN_4015b32c`.

- **Open** (`path.open` with "w", detected by `strchr(mode, 'w')`): fails when the filesystem is not mounted, the path is over 255 bytes, the parent
  folder is missing or the leaf is a folder. A missing leaf gets a new inode and directory entry. An existing file opens at pos 0 **without
  truncation** (old size and data stay); stock deletes first (the `File` ctor calls `FUN_40158e30`, the recorder `FUN_40157078`), so a new write is
  delete then open. Open sets the inode's bit in the open-file bitmap 0x46f4ded0.
- **Write** (`FUN_4015866a`): writes at `handle.pos` (settable) and advances it. It reserves the 32 KiB pages first (`FUN_40158448`; -1 when the card
  is full or the file would pass 0xfff0 extents), writes a whole page aligned to 0x8000 directly from the source buffer (`FUN_40155e84`) and copies
  partial pages through the page cache, sets the inode size to max(size, pos+len) and writes the inode back on every call. Cheapest path: whole 0x8000
  chunks at pos multiples of 0x8000 from a 16-byte-aligned buffer, the short chunk last.
- **Trailer**: the C API does not add the 16-byte zero trailer. The recorder writes it itself (`FUN_4015866a(buf, 0x10, h)`) just before the hash;
  GetFileInfo reports size - 16. Who adds it on a MidiRpc close is **[O]**; the content hash of a stock file covers the bytes plus 16 zero bytes.
- **Close**: `file.hash` (`FUN_4015b3aa` -> `FUN_4015b1e8`: reads the whole file back in 32 KiB pieces, hashlittle with seed 0x43fa243a, writes the
  hash-table page at `0x5ee980 + (id >> 13) * 0x40` and the inode word +0x0C) while it holds the mutex for the whole read-back, then `path.close`
  (`FUN_401588c0`: clears valid and the open bit). Stock (`FUN_40158cc4`) hashes only for mode 1.
- **Hash back**: `file.info(path, out)` fills `{id, hash|1 (inode +0x0C), size incl. trailer (inode +4), record counter (inode +0x10)}`; -1 when the
  leaf is missing or bit 0 of the hash word is clear (not hashed: a half-written file).
- **Delete** (`FUN_40157078`): refuses an open file (open bitmap), frees the pages of a hashed or unhashed file; a missing path gives -1.
- **mkdir** (`FUN_4015746e`): one level; the parent must exist; -1 when the leaf exists or a name character is invalid.
- **Rename** (`file.rename`): **[D]** on the emulator (adapter tests): the inode id, size and stored hash stay, so a file written and hashed under a staging name keeps its hash at the destination. The open bit of one inode: `FUN_4015bf92(id)` (anchor `file.id_open`: tests bit `id & 31` of word `id >> 5` of 0x46f4ded0, no mutex; ids from `file.info` word 0).
- **Stock writer detection**: the MidiRpc `FileWriter` (vtable 0x4021f878, a member of heap-allocated dispatchers, idle timer `FUN_4012823e` closes it
  after 20 s) has no global pointer. Every open file, read or write, sets its inode's bit in the open bitmap: `file.any_open` reports any bit;
  a single path is tested by looking up its inode id and bit `id & 31` of word `id >> 5`.

## Power cut and crash

The directory entry and inode exist from open; each write updates the inode size and writes the inode back; pages come from the allocation bitmap.
A cut mid-transfer leaves a visible file with a partial size and no hash (bit 0 of the hash word clear). Mount (`FUN_4015a72c`: superblock "SFke" at
0x5d8000, then `FUN_4015a400` loads the bitmap and rebuilds the hash table) has no repair pass that was seen: half-written files stay **[O]** (not every
callee read). Whether the inode and bitmap reach the eMMC on every write is **[O]** (`FUN_4015b868` not decompiled).

## Use

Recommended sequence: create the parents (`file.mkdir`, -1 ignored); refuse when the destination's open bit is set; `file.delete` (-1 ignored);
`path.open(path, "w", h)`; optionally `file.reserve(h, 0, total)` to fail early on a full card; `file.write` per 32 KiB chunk; write the 16 zero bytes;
`file.hash(h)`; `path.close(h)`; `file.info` for the hash. Abort: `path.close(h)` (no hash) then `file.delete(path)`: never the other order.

## Durability, links and the mounted flag (static, 1.17)

- **Card or RAM [V]**: the inode write-back `FUN_4015b868` and the page write-back `FUN_40155c34` write through the block write `FUN_4012e39c` (ADMA transfer, pends on its semaphores, so synchronous and in call order) only when the inode id or page number is below 0x1000000. For ids and page numbers from 0x1000000 they copy into RAM tables (inodes at `id * 0x80 - 0x3828c130`, i.e. 0x47d73ed0 + n * 0x80; pages at `n * 0x8000 + 0x46f63ed0`) and nothing reaches the card. Both callers ignore the block write's return value, so a failed card write is silent and the in-RAM state (inode cache, page cache) goes on as if it had succeeded. `FUN_4015bb7e` (inode free) and the allocators write their bitmap pages through the same block write, also unchecked.
- **What is RAM-backed [D]**: only the firmware's ROM tree `/factory` (its sample folders and files). `FUN_4002cd6a` (called by the mount `FUN_4015a400` when its flag bit 0 is clear) links a RAM folder `/factory` into the root with `FUN_4015c1ba`, and every factory file with `FUN_4015c13a`/`FUN_4015c21c`; their inodes come from `FUN_4015bfe0` (ids 0x1000000 + n, counter 0x44f33f70, at most 0x800) and are marked read-only (inode byte +1 = 7; mkdir, rename source and delete refuse read-only inodes). RAM state is rebuilt at every mount. User folders and files are always card-backed: inode ids come from `FUN_4015ba88` (bit index in a 0x2c00-word bitmap, below 0x58000), data pages from `FUN_40155974` (bitmap of 0xa0080 pages), and `FUN_40158448` refuses ids above 0xffffff. So `/.dkstage`, `Samples/...` and every folder the adapter creates are card-backed. The adapter refuses any path under `/factory` (`E_PATH`), because a card file linked into a RAM directory would vanish from its name at the next boot.
- **Link count [V]**: the inode has a 16-bit link count at +2. Adding an entry (`FUN_40156610` with its 4th argument 1: rename and `FUN_40156ffc`, anchor `file.link`) sets the parent, increments the count, writes the inode back, then writes the directory page, then the index pages. Deleting a name (`FUN_40156e94`, then `FUN_40156e00`) removes the directory entry first, then `FUN_4015614e` decrements the count and frees the data only at 0. The second argument of `FUN_40156e94` is not "entry only": it only lets a folder inode be removed (rmdir passes 1). So one name of a file with two names can be deleted without freeing the data, a cut inside a rename leaves two names with count 2, and a cut between the entry removal and the decrement of a delete leaks pages and never frees shared ones.
- **Index pages [O window, D facts]**: a directory has three sorted index pages next to its entry pages: 0x10000 (by name hash, used by the lookup `FUN_401575bc` for folders of up to 0xffe entries), 0x10001 (the order `directory.next` `FUN_4015734e` walks), 0x10002 (by inode id). Both the lookup and the listing read these index pages, not the raw entry pages: an entry that is missing from the index is neither found nor listed. The mount does not rebuild them: `FUN_4015b0ee` at the end of the mount rebuilds the sorted content-hash table (hash word, inode id) from the hash pages and the RAM inodes, which is a different table. **[O]** A cut after the entry page and before the index pages (in `FUN_40156610` for every add, and in the matching removal `FUN_40156bf6`) leaves an entry that is invisible, or an index slot that points at a removed entry. The stock rename has the same window; the adapter does not add to it, and its recovery (which finds the destination through the index) would add a second entry for a destination whose first entry was never indexed (a leaked entry and link count, not a lost file). No test cuts there.
- **Names** are hashed and compared byte for byte (`FUN_40156272`): case-sensitive.
- **Mounted flag [V]**: `0x44f2bd68` (set by the mount `FUN_4015a72c`, tested first by every path function): anchor `cf.storage.site.mounted` (the literal at `file.info` + 0x12).
- **Existence**: `file.info` is -1 for a folder, for an unhashed file and for a missing one. The adapter scans the parent folder with `directory.open/next` instead: the entry has the inode id (+0) and the type (u16 +4, 1 = folder). The path resolver `FUN_4015a22c` reports a found leaf as 6 (folder) or 10 (file).
- **Open [O]**: what the eMMC itself buffers is not under the firmware's control.

## As built: `core/plusdrive-write` (docs/plans/usb-fast.md step 2)

**[D]** Everything below ran on the DT2 1.17 emulator (`modules/core/adapters/plusdrive-write/tests`, the card read back with `plusdrive fs ls` / `fs cat`).

- Files are written to `/.dkstage/s<8 hex>`; the number is above every name of ours in the folder (never a reused or deleted leftover name). The folder is removed whenever it is empty, so it exists only during a transfer (or while foreign entries are in it). Whether the stock sample browser would list it was **not** shown (the raw directory iterator `FUN_4015734e` and the `dirent2` list builder have no name filter, the browser views were not traced) **[O]**. The runtime refuses paths inside it (`E_PATH`).
- The adapter writes the 16 zero bytes itself before `file.hash`: the stored hash covers data + 16 zeros and `fs ls` reports size N+16 **[D]**. `file.rename` keeps the hash **[D]**.
- Destination checks use the folder scan: a folder at the destination, a file in the path or a path under `/factory` is `E_PATH`; a file, hashed or not, is `E_EXISTS` without REPLACE and is replaced through the journal with it **[D]**; an open destination is `E_FILE_IN_USE` before any data is written **[D]** (the test sets bitmap words in guest memory; a real stock reader was not used).
- Replace: a hashed journal file `<staged>.dest` (the destination path), `file.delete` of the destination, `file.rename`, delete the journal. A rename that fails after the delete leaves the journal and the staged file; the runtime marks the recovery pending and its UI pass finishes the replace before the next file is accepted **[D]** (test knob step 7).
- Boot recovery (`dkpw_recover` from the runtime's UI pass once the mounted flag is set): a whole journal with a hashed staged file and a missing destination is renamed into place; every other name of ours is deleted (a staged name that shares its inode with the destination loses only its name); names not ours are left alone and logged (event 0xb7, word 4); more than 24 names are handled in rounds.
- Cuts at: mid-data, after the hash, after the journal, between the delete and the rename, after the rename, and inside the rename (two names, with and without a journal) (test knob `DKRT_PD_CUT`): after the next boot the destination holds the old file (steps up to the journal) or the new one (from the delete on), whole, the folder is gone, the destination's data is still allocated (another file is written and it reads as before) and no other file changed **[D]**.
- A stock-writer leftover (an unhashed file at the destination) survives a boot and is handled as above **[D]**; a foreign file in `/.dkstage` survives a boot and a transfer **[D]**.
- **[O]** The stock sample browser's view of `/.dkstage`; a real stock MidiRpc reader or writer on the destination; the eMMC's own write cache.
