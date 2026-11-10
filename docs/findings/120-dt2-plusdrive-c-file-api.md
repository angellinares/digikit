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
Rename is `FUN_40156b00(old, new, flag)`; rehash by path `FUN_4015b32c`.

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

Because a cut leaves a half-written file, the adapter should write under a temporary name in a hidden folder and rename into place after the hash,
and remove leftovers from that folder at boot. Whether rename keeps the inode and its hash is **[O]**: check on the emulator.
