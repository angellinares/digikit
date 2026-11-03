# 108. DT2 sample data and the SHARC data cache

Trigger: row 3 of the gap map in the sampler hardware-observation handover asked whether sample PCM can reach the SHARC voice code stale because of the data cache, and whether a device test is needed. All results are read once from the DT2 1.17 image and from earlier findings. No device run. Marks: **[D]** measured or read once, **[O]** open.

## 1. Cache configuration **[D]**

Values from [finding 103](103-dsp-facts-on-a-digitakt-ii.md):

- SHL1C_CFG `0x00010101` enables the instruction, DM and PM caches (16 KiB data cache, 64-byte lines).
- SHL1C_CFG2 `0xf0`: range 2 `[0x200fa000, 0x200fe000)` and range 3 `[0x28240000, 0x283a0000)` are non-cacheable. Stock startup programs them as non-cacheable (finding 95: `0xb8b93a` -> `0x1c00da` -> `0x1c008c`).
- Everything else in L2 and DDR is cached.
- The data-cache write policy is **[O]**.

## 2. The stock sample path **[D]**

1. ColdFire sends a 0x802-word frame over the link (finding 85).
2. DMA fills the staging descriptor `0x268220` (ADDRSTART `0x268240`, count 0x401). Its byte alias `0x28268240` is inside range 3, so it is non-cacheable (finding 81).
3. The SHARC CPU copies the page into DDR with its own stores. `0x1c403c` (called from `0x1c7e76`) forms I4 = (page<<12) + `0x8422b9c8`; the guard copy is `0x1c400f`. It is reached from the interrupt handler `0x1c8705` -> `0x1c85ba` -> receiver `0x1c7e1f` (finding 81).
4. PCM lives in cached DDR at byte `0x8422b9c8 + (page<<12)`, page < 0x19200 (findings 15, 81, 99).

## 3. Result **[D]**

No DMA writes cached memory on this path. The writer and the reader of the DDR PCM are the same core, so the cache cannot give stale PCM. Stock code needs and does no cache maintenance for PCM.

The maintenance helper `0x1c07e3` (in `0x1c075e`; writeback, or writeback and invalidate; no invalidate-only mode; finding 95) is not on the page-copy path. **[O]** The full receiver chain was not scanned for maintenance.

## 4. Consequence for the sampler

- An owned sample buffer filled the stock way (DMA into non-cacheable staging, then CPU copy) needs no device test and no maintenance.
- The DT2 candidate bank `[0x80800000, 0x808ea600)` (finding 85) is cached DDR. Its ownership is still **[O]**.
- A design that DMAs directly into cached L2 or DDR needs maintenance and one device test. The helper has no invalidate-only mode, so a dirty line could be written back over DMA data (finding 95).

## 5. Device test, only if direct DMA is chosen

- Module `test/dsp-cache`, same shape as `test/dsp-facts`.
- A 64-byte-aligned, line-exclusive L2 BSS buffer.
- DMA word A into it and read. DMA word B and read again (fresh = B, stale = A). Then DM invalidate and read as control (B).
- Two restarts with `dk:probe` (select, restore), one with `--keep`.
- The emulator has no cache model, so `--spawn dt2` proves only the probe mechanics.
