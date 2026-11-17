# Fast USB: batch commit, cached receive, DT2 per-file cost

Goal: faster sample-pack uploads over Fast USB on both devices, with the readback hash check kept on every file and replace kept atomic.

## Status

Built, measured on both devices (finding 134, "Result of the speed round") and always on. The run-time switches that carried the levers for the hardware A/B (STATUS `'S'` bits 4 to 8 and `wb.mjs --no-batch`) are removed.

| Item | Status | Measured |
|---|---|---|
| B batch commit | done | DN2 pack 2.42 → 3.25 MB/s |
| C cached receive | dropped: slower on the DN2 (finding 134) | |
| D1 read-back in runs, D2 trailer in the last write | done | DT2 pack 2.32 → 2.40, 9.8 MB 3.53 → 3.68 |
| D3 page allocation without the free page count | done | DT2 pack → 2.45 |
| D4 new files at their final name, no boot cleanup | done | DT2 pack → 2.49 |
| E5 copy/write overlap | done | DN2 9.8 MB 3.67 → 4.37, DT2 3.53 → 4.01 |
| E6 prepare the next file on the host | done | not measured alone; DN2 pack 3.56 with batch and overlap, DT2 pack 3.07 with all levers |

D4 decision: a new file is written at its final name and the boot cleanup (an intent record in the staging folder and a scan after boot) is not built. After a power cut during an upload, one partial file without a hash may stay visible; the next upload of the same path replaces it. A folder that a failed upload created stays.

Follow-up (separate PR): remove the speed bits 0 to 3, the STATUS `'S'` command, `wb.mjs --speed` and `dk_debug_speed_flags`.

## Why not parallel writes

Details and evidence: [finding 133](../findings/133-storage-concurrency.md).

- The eSDHC runs one command and one data transfer at a time: no command queue, no packed commands, 4-bit bus only, no DDR mode (RM eSDHC chapter).
- The firmware block read and write (DT2 0x4012e1b6 / 0x4012e39c, DN2 0x4012c87a / 0x4012ca60) run under one global mutex (0x47de3cac on DT2). Every +Drive file call also runs under the file-system mutex 0x46f4deb0.
- So several writers only take turns. The card is not the limit:
  - card write plus readback: 7.8 MB/s measured, about 9.3 MB/s at the wire;
  - one large file end to end: 3.1 MB/s (DT2) and 4.0 MB/s (DN2);
  - a 200-file pack: 1.5 MB/s (DT2) and 2.1 MB/s (DN2).
- The driver waits on semaphores during DMA and card busy, so other tasks get the CPU while the card works. A pipeline is safe. The mutexes have no priority inheritance.
- Closed:
  - the eMMC cache and the reliable-write bit (no effect within 2%, finding 126);
  - a faster card clock (37.7 MHz against the RM's 40 MHz limit).

## Expected gains

| Item | Device | Today | Expected | Confidence |
|---|---|---|---|---|
| B batch commit | DN2 | pack 2.1 MB/s | 3.5-3.8 MB/s | medium-high: commit cost measured (95 ms + 0.46 ms per stored file, finding 126) |
| C cached receive | both | copy 18 MB/s (0.42 s per 8 MB on DN2) | copy several times faster; large file DN2 4.0 → about 4.5 MB/s | medium: CPU reads of the uncached alias are measured slow (hash 9.3 vs 14.6 MB/s), the copy gain is not measured |
| D DT2 per-file cuts | DT2 | pack 1.5 MB/s, 195 ms per file | unknown until item A | low |

## E5 and E6. Copy/write overlap and prepare-ahead

**Status: done and measured (see Status above).** E5: SDK imports `dk_helper_run` and `dk_helper_wait` (I/O-step context; the runtime's helper task, E_BUSY while a run is uncollected, so the close read-back hash and a module never overlap); usb-fast stages a file above 512 KiB in two halves of the 1 MiB reservation and the helper copies the received frames into one half while `dk_files_write` of the other blocks the I/O task; `usb-fast.device.test.ts` case "copy/write overlap" (sizes below, at and above the half, with and without DEFER, same hash and same card contents). Hardware: DN2 9.8 MB 3.67 → 4.37 MB/s, DT2 3.53 → 4.01. E6: `uploadItems` of the desktop UI and `wb.mjs samples put` read and prepare file i+1 while file i is sent; the `prepared` event is still emitted when the file's turn comes, a failed read or prepare is reported on that file only, a stop or cancel discards the work ahead. Host tests in `packages/workbench-core/tests/sample-upload.test.ts`.

## A. Card command count per phase (emulator, DT2)

The 195 ms per DT2 file has never been split by phase.

1. Emulator: behind an env flag (like `DIGI_WRITE_WATCH`), `Board::service_dma59` (packages/emulator/machine/src/board.rs) logs one record per card data command (instruction count, command, argument/LBA, bytes) to the watch JSONL. CMD23 and CMD12 come from the eSDHC command decode.
2. Phase boundaries come from a `PDW_TRACE=1` + `UF_TRACE=1` build of plusdrive-write and usb-fast (measurement build only, not shipped).
3. A tool, `tools/devtools/card_phases.py`, reads the JSONL and the runtime log and prints, per phase, the number of commands and bytes read and written. It is documented in docs/INSTRUMENTATION.md and linked from DEVELOPMENT.md.
4. Run once for a 1.5 MB file and once for a 200-file pack folder (this also measures how the cost grows with folder size, open item 126:75). Multiply by the per-command costs measured on hardware (findings 124, 126) to estimate the breakdown.
5. Result: a finding with the table, then a short design for item D, which needs a second OK.

No kept tests for the tool.

## B. Batch commit

**Status: done and measured (see Status above).** Ticked: store crate `InsertMany` and its cut test, store_writer pending files and auto-commit, SDK `DK_FILE_DEFER`, `dk_files_commit` and `dk_files_pending`, DT2 `dkpw_commit`, usb-fast DEFER, 8-byte END, COMMIT, `BulkSession.put({ defer })` and `commit()`, `uploadItems` batch states, the docs. Device tests: `file-write.device.test.ts` (4 cases), `file-write-cut.device.test.ts` (1 case), `usb-fast.device.test.ts` (1 case per mode). Open: the scheduling of the automatic commit is polled by the UI pass (about one UI pass of delay); a SysEx MKDIR or RENAME commits the pending files first, a CLEAR drops them.

### Runtime (modules/core/runtime)

- SDK: new flag `DK_FILE_DEFER` (2u) on `dk_files_create`.
  - A deferred file is written, read back and hash-checked at `dk_files_close` as today.
  - It is not named in the store table: it becomes a pending file.
  - `dk_files_close` returns the hash as today.
- New SDK calls (io-step context, runtime 0.8.2 → 0.8.3, additions only):
  - `dk_files_commit(void)` names all pending files of the calling module in one table commit. It returns 0 or a negative status.
  - `dk_files_pending(void)` returns the number of pending files of the calling module.
- Limits: at most `DK_FILE_PENDING_MAX` = 16 pending files and 16 MiB of pending data. A deferred create (`dk_files_create` with `DK_FILE_DEFER`) that would pass either limit (16 pending files, or pending bytes plus its size over 16 MiB) commits all pending files first; if that commit fails, the create returns the error before any data is sent. A deferred close only adds the file and never commits, so END never fails for a file that stays pending. A pending file whose replaced target is leased stays pending while the others commit; the automatic commit retries it and the pending count reports it.
- The runtime commits pending files, as an I/O-task step, never in the UI task:
  - 1 s after the last file call of the owner;
  - at `end_job` and `retire` of the owner;
  - instead of the 20 s idle drop.
  Pending files are verified data, so they are committed, never dropped.
- A power cut loses only the pending files. Nothing names their extents, so the space is free after boot, and the old table stays valid. Each loss is bounded by the limits above.
- store crate (`modules/core/runtime/sdk/src/store.rs`):
  - new `Edit::InsertMany(&[*const u8])` with entries sorted by `before`;
  - the same-path replace check runs per entry;
  - `need = table_sectors(count + n)`.
  - The scan merge already handles N sorted adds. One table write, one readback and two header writes per batch.
- store_writer (`store_writer.rs`):
  - The single session stays (one open file).
  - New: a pending array of up to 16 entries (entry, extent).
  - `extras()` and `begin_fit` keep all pending extents out of allocation (`EXTRA_MAX` grows).
  - `begin` checks pending paths for conflicts.
  - `free_bytes` counts the pending table entries.
  - The `Hint` is dropped for `InsertMany` (it is valid only for one Insert).
  - A REPLACE of a path that is pending replaces the pending entry.
  - Delete of a pending path removes it from the array.
- DT2 +Drive route:
  - A deferred close does what it does today: the file is placed at END, and the hash-table rebuild stays deferred by `rb_pending`.
  - `dk_files_commit` calls a new adapter export `dkpw_commit` → `rebuild_table()` under `pd_enter()`.
  - The 2 s idle recovery stays as the fallback.
  - `dk_files_pending` returns 0 on DT2 (files are named at END).

### usb-fast protocol (sdk/sdk.toml [protocol.bulk])

- BEGIN flag `DEFER` = 0x0002 (beside `REPLACE` = 0x0001): the device creates the file with `DK_FILE_DEFER`. An old device ignores the unknown flag and commits at END as today.
- END reply body grows from 4 to 8 bytes: `hash4 | pending u16 | reserved u16`.
  - `pending` = pending files after this END; 0 means everything sent so far is committed.
  - An old device returns 4 bytes; the host reads that as committed.
- New command COMMIT 0x07: `dk_files_commit`, reply body `pending u16 | reserved u16` (0 on success).
  - An old device answers E_PROTO; the host reads that as nothing to commit.
  - No capability bit or version check is needed.
- ABORT aborts the open file only; pending files stay.
- The runtime's 1 s idle commit covers an unplug, a USB reset and a host crash.
- No STATUS 'S' bit: the device always honours DEFER (a switch existed for the hardware A/B and is removed).

### Host

- `BulkSession.put(..., { defer })` sets DEFER and returns `{ hash, pending }`.
- New `BulkSession.commit()`.
- `uploadItems` (packages/workbench-core/src/transfer/sample-upload.ts):
  - It sends files with DEFER.
  - A file whose END reply has `pending > 0` stays in state `verifying`, labelled "Saving…".
  - When a reply shows `pending == 0`, all files waiting in that state become `ok`.
  - After the last file it sends COMMIT.
  - On a COMMIT error, or a link loss with files waiting, those files become `failed` ("not saved, send again").
  - The existing re-run skip (`sameOnDevice`: name, size, listing hash) then sends only the lost files.
- The web app has no multi-file upload path. Nothing changes there beyond the shared dk-session code.

### Tests (host first, few device tests)

- store crate:
  - a many-insert twin of `a_power_cut_at_any_write_leaves_the_old_store_or_the_new_one` (every cut: old table, or new table with all N files);
  - same-path replace inside a batch.
- dk-session `bulk.test.ts`: DEFER flag, 8-byte and 4-byte END replies, COMMIT and its E_PROTO fallback.
- `packages/workbench-core/tests/sample-upload.test.ts`:
  - waiting files become `ok` only on `pending == 0`;
  - a COMMIT failure marks them `failed`;
  - the re-run skip after a loss.
- Device (DN2):
  - `file-write.device.test.ts`: a defer-and-commit case, the auto-commit at the limit, the 1 s idle commit.
  - `file-write-cut.device.test.ts`: a cut during an N-file commit leaves the old store or the new one with all N files; a cut before the commit leaves no pending file visible and the space free.
  - `usb-fast.device.test.ts`: one multi-file batch upload in both modes (pair and OUT-only).

The single-file path and its write count (`upload-cut` ≥ 6 writes) stay unchanged.

Docs:
- docs/plans/usb-fast.md sections 5 and 7;
- modules/tools/usb-fast/README.md;
- the header comments of usb-fast.c and bulk.ts;
- sdk/include/dk/platform/files.h;
- docs/design/digikit-store.md ("Commits and power cuts");
- the AGENTS.md rules, if any change.

## C. Cached receive

1. Emulator: fold the uncached DDR alias (0x48000000-0x4fffffff) onto the cached window in `Board::page_index` (board.rs:1084). Check its direct users:
   - `map_zeroed_ram_page`
   - `import_ram_page`
   - the `oracle_sdram_faults` range checks
   - the DMA written-range drain
   - boot/src/common.rs range checks

   No snapshot format change. The board memory unit tests get one alias case.
2. `core/usb-bulk`: turn the compile-time `UB_CACHED_RX` into a run-time switch, default off. The rules in usb_bulk.c:261-271 stay: the CPU never writes a receive buffer through its cached address, and ub_take invalidates the lines before the first read.
3. The emulator proves the data path. It cannot prove the cache invalidate (caches are not modelled, finding 128). Only the hardware A/B can.
4. If the hardware copy is still a large share after C, a double-buffered staging helper in sdk/ (the worker copies one half while the card writes the other) is the next step. It needs its own OK and a RAM check: the second buffer comes from the same heap as the receive ring.

## D. DT2 per-file cuts

After item A. Candidates from the code (pdw.c), each keeping replace-by-rename and the readback check:
- one folder scan instead of 2-3 (BEGIN `lookup`, `parent_is_file`, `place` `lookup`);
- skip the `parents()` mkdir attempts when BEGIN proved the folder exists;
- drop the `file_info` call after `hash_fast` (it repeats the hash and size);
- fold the 16-byte zero tail into the last staging flush (one inode write-back less).

Ranked by item A's numbers.

## Hardware

One image per device at the end, with every lever behind a switch (done; the switches are removed after the measurement):
- DN2 first: 8 MB file and a 200-file pack, batch on/off, cached receive on/off, 3 runs each, alternating.
- DT2: the same, plus the `PDW_TRACE` build for item A's hardware check.

Flashing needs Em.

## Work order and lanes

| Lane | Work | Effort (agent time) |
|---|---|---|
| usb-batch (`usb-fast/batch`) | B runtime + store crate + SDK, then the B protocol and host | 1-1.5 days |
| second lane | A emulator card log + tool + DT2 run, then C emulator alias + usb-bulk switch (both touch board.rs) | 1 day |
| after A | D design (second OK), build | about 1 day |

Each lane runs host checks only while building. One targeted device gate at the end (list and time shown to Em first), then the hardware session.

## Open

- Measured RAM headroom for a second staging buffer: needed only if C is not enough.
- Whether the DT2 per-file cost grows inside a large folder: item A answers it.
