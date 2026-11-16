# usb-fast on hardware: time per phase, cached receive, pack uploads

Code: `modules/tools/usb-fast`, `modules/core/adapters/usb-bulk`, `modules/core/adapters/plusdrive-write`, `modules/core/runtime` (`store_writer`). Hardware runs of 2026-11-16 by the main session. Earlier ceilings: [126](126-usb-fast-transfer-ceilings.md). Marks: **[D]** measured or read once, **[O]** open. Nothing here is **[V]**.

## Setup **[D]**

- DN2 OS 1.12 (image `ec5cac23ca9cc85d`) and DT2 OS 1.17 (image `67b49e8b3ab26847`). Set `device-audio-bulk`, runtime 0.8.2, built from branch `usb-fast/card-phases`. DT2 also runs `plusdrive-write` with `PDW_TRACE=1`.
- `usb-fast` is built with `UF_TRACE=1` (12332 bytes).
- USB CONFIG is USB AUDIO/MIDI, Fast USB through `usb-bulk-audio`.
- `wb.mjs samples put`, speed mask 0xf (levers A to D on), 1 MiB staging, 60 ring slots.
- Trace times are sums of 0.667 ms blocks per file, from the debug module ring (last 60 files on DN2, last 20 on DT2). The hash of every upload was checked on the device at END.
- Files: one 9.8 MB file (16-bit 48 kHz stereo WAV, 9 786 984 bytes stored) and a pack of 309 files (325 732 556 bytes stored, 1.05 MB per file).
- The raw data is in `out/runs/meas/` (not committed).

## Large file (9.8 MB) **[D]**

| | DN2 | DT2 |
|---|---|---|
| speed | 3.66 to 3.73 MB/s | 3.46 to 3.52 MB/s |
| BEGIN | small | 141 ms |
| card writes | 710 ms (13.8 MB/s) | 988 ms (9.9 MB/s) |
| copy from USB buffers | 710 ms | 558 ms |
| END (readback, hash, commit or place) | 1100 ms | 1009 ms (readback 940 ms, 10.4 MB/s) |

## Cached receive **[D]**

`UB_CACHED_RX` of `usb-bulk` was a run-time switch for this test (DN2, 3 runs each, alternating).

- Off: 3.66 to 3.73 MB/s. On: 3.47 to 3.50 MB/s.
- The copy time is the same (about 0.71 s in both). The extra 0.19 s is in no trace phase. The likely cause is the invalidate of each cache line (about 600 000 lines per 9.8 MB).
- The copy is not limited by uncached reads. The switch is removed; `UB_CACHED_RX` stays a compile-time switch, default 0.
- Why the copy runs at only about 14 MB/s is open. **[O]**

## Pack of 309 files, DN2 **[D]**

| | run 1 | run 2 |
|---|---|---|
| speed including host prepare | 1.81 MB/s (180 s) | 1.41 MB/s (230 s) |
| upload only | 2.05 MB/s (159 s) | 1.53 MB/s (213 s) |
| host prepare | 21 s | 18 s |
| BEGIN per file | 22 ms | 31 ms |
| copy per file | 71 ms | 69 ms |
| card writes per file | 77 ms | 75 ms |
| END per file | 386 ms | 550 ms |

- Run 2 started with 309 more files in the store. The END grows with the store size: the table commit rewrites and reads back the whole table (finding 126).
- The readback of a 1.05 MB file takes about 70 ms, so about 300 to 480 ms of each END is the table commit. Plan item B (batch commit) targets it.

## Pack of 309 files, DT2 **[D]**

Run 1 reached 1.82 MB/s and run 2 1.84 MB/s, including a host prepare of 10.6 s and 9.5 s. There is no growth between runs. Per file (last 20 files, 0.75 MB on average):

| phase | ms |
|---|---|
| BEGIN `file_reserve` | 94 (flat: 90 ms for the 9.8 MB file) |
| BEGIN `path_open` "w" | 52 |
| BEGIN lookup, parent, mkdir | about 5 |
| card writes (1.45 calls) | 95 |
| copy | 43 |
| END readback hash (32 KiB pages) | 92 |
| END place: rename | 56 |
| END tail (16-byte trailer write) | 13 |
| END close, info, journal | under 1 |

- The fixed cost (reserve, open and rename, about 200 ms) matches the 195 ms of finding 126 and is flat with the folder size. This closes the open item of finding 126 (growth inside one large folder) for folders up to 618 files.
- The scan, mkdir and file_info candidates of plan item D cost about 5 ms in total.

## Levers this ranks **[D]**

Plan: `docs/plans/usb-fast-batch.md`. In order of expected gain:

1. DN2 batch commit of the table (largest).
2. DT2 `file_reserve` and open plus rename (about 200 ms per file).
3. DT2 readback in larger reads (10 to 15 MB/s).
4. Copy and write overlap for large files (about 0.55 to 0.7 s per 10 MB).
5. The host prepares the next file during the current upload (10 to 21 s per pack run).

## Result of the speed round

Hardware runs of 2026-11-16 on both devices, branch `usb-fast/meas` (head 2d17fcb1c), every upload hash-checked at END. Each lever had a run-time switch for the run; the switches are removed and the levers are always on. Raw outputs: `out/runs/meas2/` (not committed). All numbers are **[D]**.

Levers: B batch commit (`DK_FILE_DEFER`, one table commit per up to 16 files), D1 read-back in runs of up to 128 KiB, D2 the trailer in the last write, D3 page allocation without the free page count, D4 new files at their final name (no rename), E5 copy/write overlap (two staging halves and the helper task), E6 the host prepares the next file during the upload. Plan item C (cached receive) was dropped (above).

DN2 (OS 1.12), MB/s:

| Upload | Before | Batch | Batch and overlap | Overlap only |
|---|---|---|---|---|
| 100 pack files | 2.42 | 3.25 | 3.56 | |
| one 9.8 MB file | 3.67 | | | 4.37 |

DT2 (OS 1.17), MB/s:

| Upload | Before | D1 and D2 | D1 to D3 | D1 to D4 | All levers |
|---|---|---|---|---|---|
| pack | 2.32 | 2.40 | 2.45 | 2.49 | 3.07 |

| Upload | Before | D1 and D2 | Overlap | D1, D2 and overlap |
|---|---|---|---|---|
| one 9.8 MB file | 3.53 | 3.68 | 4.01 | 4.13 |

- Every lever helped on its own and none cost anything, so all are kept.
- The DT2 pack reaches 2.49 MB/s with D1 to D4 and 3.07 MB/s with all levers on. The last step (0.58 MB/s) is not split by lever. **[O]**
- The DN2 pack gain is mostly the batch commit (0.83 of 1.14 MB/s): the table commit is paid once per batch.
