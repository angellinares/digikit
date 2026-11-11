# DT2 +Drive fast close and the measured transfer ceilings

Code: `modules/core/adapters/plusdrive-write` (`hash_fast`), `modules/tools/usb-fast`. Hardware runs of 2026-11-11 (DN2 1.12 and DT2 1.17, branch `speed/usb-fast` at 6760f16c), from [usb-fast-hardware.md](../plans/usb-fast-hardware.md). Marks: **[D]** measured once on one device, **[O]** open. Nothing here is **[V]**.

## The close hash

The firmware closes a +Drive file with `FUN_4015b1e8`: it reads the whole file back through the page cache in 32 KiB pieces and hashes it (4.2 MB/s **[D]**). `hash_fast` reads the same pages from the card with the firmware's block read (the card sector of a file page is `data base + phys * 0x40`, `phys` from the extent lookup `FUN_4015813c`), hashes them with the adapter's own code (the dksample hash), compares the result with the host's hash before it changes anything, and then runs the rest of `FUN_4015b1e8` in its order with the firmware's functions under its mutex: the hash page, the table rebuild, the inode word, the event, the inode write-back. A hash mismatch changes nothing. If the card cannot be read the way `hash_fast` needs (not mounted, no inode, a size that is not what was written, a page the extent lookup misses, a failed block read), nothing has changed yet and the close runs the firmware's own hash. The emulator tests compare the card sectors and the RAM hash table of the fast close, of the fallback and of the stock close: equal. **[D]**

## Ceilings (storage bench, UF_BENCH build, 8 MiB, no USB) **[D]**

| what | rate |
|---|---|
| DT2 +Drive `file_write`, 64 KiB calls | 2.6 MB/s |
| DT2 +Drive `file_write`, 256 KiB calls | 7.8 MB/s |
| DT2 +Drive `file_write`, 1 MiB calls | 10.3 MB/s |
| DT2 firmware hash re-read at close | 4.2 MB/s |
| DT2 store | about 7.2 MB/s |
| DN2 store | 6.3 MB/s |
| raw card reads, both devices | 15.3 MB/s |
| CPU hash | 16 to 18.7 MB/s |
| USB echo, each way at once (64-byte IN packets) | DN2 about 2.3 MB/s, DT2 about 2.9 MB/s |

The 64-byte IN packets are the controller's TX buffer (64 bytes per endpoint); usb-fast replies are at most 32 bytes, so the IN rate does not limit uploads.

## Merged build on hardware **[D]**

- A 1 MiB staging buffer (a flush of 1 MiB per `file_write`) took a DT2 8 MB upload from 4.20 s to 3.32 s: the card writes went from 1.83 s to 0.77 s (9 calls instead of 129). The DN2 did not change.
- DT2 +Drive: 8 MB in 2.99 to 6.15 s over three runs (best 2.80 MB/s; the first runs are slower), 100 small files (28 MB) 0.51 MB/s (about 0.55 s a file; the hash table rebuild of about 300 ms a file remains). SysEx: 0.11 and 0.10 MB/s.
- DN2 store: 8 MB upload 1.75 s (4.8 MB/s), with SysEx verify 2.92 s (2.87 MB/s); 100 small files 1.17 MB/s with verify. SysEx: 0.62 and 0.57 MB/s.
- The +Drive rate varies between runs: measure several. **[O]** whether the first-run slowness is card warm-up or the staging folder's first mkdir.
