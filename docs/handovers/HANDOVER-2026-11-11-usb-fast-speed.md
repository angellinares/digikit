# Handover 2026-11-11: usb-fast speed work and the USB take-over

Branch `speed/usb-fast` in lane `../lanes/usb-bulk`, head `d735ed02` (29 commits over origin/main; not pushed). PR #404 (the first
usb-fast baseline) is already merged into main.

## State

- Done and on the branch: 4 KiB wedge fix (64-byte IN packets), wrong-USB-mode refusal, staging folder per session, 8 RX buffers,
  1 MiB write buffer + direct/movem copy, DN2 store single-pass BEGIN + 64 KiB pieces, DN2 device readback at close (CAP READBACK; host
  skips the SysEx verify), DT2 fast close (own hash + readback + firmware tail, byte-identical to the firmware close) + batched hash-table
  rebuild, fast-close fallback, cached RX reads (compiled OFF), the workbench take-over (usb-fast TAKEOVER/RELEASE, one re-enumeration,
  switch guard, restart-counter anchor, wb.mjs and desktop wiring), findings 121 and 122, AGENTS.md/TESTING.md full-tier rule.
- Hardware, merged build (DN2 image `29b91c34123022a6`, DT2 `85cc2533f2de4877`, runtime 0.5.10):
  - Take-over works on BOTH devices from USB AUDIO/MIDI: plain `wb samples put` takes over, uploads over bulk, releases; the stored USB
    CONFIG byte stays unchanged; USB audio checked by ear after (QuickTime record and playback).
  - DN2: 8 MB 3.10 s incl. device readback (was 2.92 s with the SysEx verify: the readback is slower: 16 KiB reads, slow hash32);
    100 small files 1.03 MB/s.
  - DT2: 100 small files 0.84 MB/s (was 0.51: batched rebuild); 8 MB 3.8-5.9 s (card write speed varies a lot after heavy use; our
    readback reads 32 KiB per call, so it barely beats the firmware re-read for large files).
  - Model: DT2 ~0.19 s/file + ~2.0 MB/s; DN2 ~0.18 s/file + ~2.9 MB/s. See docs/plans/usb-fast-hardware.md for all numbers and ceilings.

## In flight when the session ended (check their results first)

1. Integration test run of `d735ed02` (combined targeted list incl. `DIGIKIT_TIER=full` upload-faults on DN2 and DT2, smoke, conformance).
   Logs: `/private/tmp/claude-501/-Users-em-src-digi-digitakt2/66b53e39-72d5-45e2-acc7-fdcdc5231b84/scratchpad/` (merge2-ready.txt says
   MERGED d735ed02; device results in dev-summary.log). If anything is red, fix before the PR. RESULT: all green; the first conformance run timed out under load (no new boot within 90 s at load 40-80) and its rerun passed in 38 s.
2. Lane `../lanes/usb-host`, branch `speed/fast-hash` (from d735ed02, uncommitted): word-at-a-time hash32 (+ pdw_hash) proven identical,
   instruction counts, 64 KiB readback reads.
   DONE, all tests green (incl. full upload-faults DN2, plusdrive-write DT2): hash32 was already word-wise; the loop is now 20% cheaper
   (7.0 -> 5.6 insn/byte on DN2; the rotate-less ColdFire mix is the floor); pdw_hash.h restructured the same way (was ~10 insn/byte);
   readback uses a temporary 64 KiB fw_alloc buffer for files larger than the caller's buffer (not yet confirmed to run in the emulator;
   the allocator's real rounding is unknown). Expected ~0.2 s per 8 MB on the DN2; needs a quiet-host A/B and hardware.
3. Lane `../lanes/zero-dsp`, branch `exp/dn2-unreliable` (uncommitted, EXPERIMENT ONLY, never merge as is): DN2 patch set
   `device-usb-exp`: a RAM flag skips the eMMC reliable-write bit for file-data writes only (store table/headers keep it); cached RX
   reads switchable at run time; usb-fast bench selectors toggle both. Purpose: one flash, two hardware A/Bs.
   READY (emulator-proven: with the switch on only data CMD23s lose the bit, table/header keep it; existing DN2 store tests incl. full
   upload-faults pass). Image `out/patch/exp-unrel-dn2-1.12.syx`, module `out/patch/exp-usb-fast-dn2.dkm`. Switches: usb-fast STATUS 0x37
   body `55 <unrel> <cached>` (reply: unrel, cached, rx_inval_lines, n_cmd23_reliable, n_cmd23_unreliable); bench `42 21 08`, results
   `52`. Run each of the 4 combinations 3x (alternating), then 8 MB `wb samples put` stock / unrel / unrel+cached; check the END hash and
   that n_skip and rx_inval_lines grow. Static checks and DT2 tests were not run on that branch.

## Next

1. PR #407 is OPEN (pushed c189d9d0) but CONFLICTS with main (6 new commits): merge origin/main into speed/usb-fast (regenerate generated files, never hand-merge them), rerun the affected device tests, push; ask Em before merging; full tier after merge.
2. DN2 A/B with the experiment image (reliable write on/off, cached reads on/off), then decide.
3. Merge fast-hash; measure; DN2 readback must beat the old verify.
4. Larger card commands (firmware 64 KiB clamp in the eSDHC write/read, DN2 0x4012ca8a / DT2 0x4012e3c4; check the bounce buffer
   0x4e6f9300 size first) and DT2 raw extent writes (firmware reserves, we write data with large commands, readback + firmware tail).
   Target ~8-10 MB/s for large files.
5. Emulator gap: the DDR cached window and its +0x08000000 alias are separate pages (packages/emulator/machine/src/board.rs page_index);
   fix it before enabling cached RX by default (one full-tier run).
6. Clean up the bench folders on both devices (`wb samples rm`; DT2: bench2-*, bench3-*, takeover-dt2, ab*; DN2: samples/bench*,
   takeover1, ab*).

## Notes

- Flash from the lane with its own patchkit (`dist/native-target/nogen/release/patchkit --root <lane>`); auto mode blocks flashing, so Em
  runs it with `!`. Two SysEx clients on one port conflict (dklog follow vs dkupload). Lanes share `out/` (cache objects leak).
- Hardware-only facts are marked [D] in findings 42/120/121/122; no [V] without a second agent's byte check.
