# 101. The module step word on a Digitakt II: 3995 of 4000 words were 0

Trigger: on a real Digitakt II (OS 1.17) the SHARC probe transport did not work. The module step word (diagnostic word 25 of the DSP reply, finding 77) was 0 in almost every record while the effect ran. This finding has the measurement, the cause and the new design. Marks: **[D]** measured or read once, **[O]** open.

## 1. The measurement **[D]**

2026-10-31, real Digitakt II, OS 1.17, branch `debug/mode-and-probes`, image id `bd8b1d4105942639`. Modules: `digikit/remote`, `test/step-beacon` and `digikit/probe`. The runtime logged `STEP_PATCH outcome=patched record_addr=0x2666c8`.
The collector kept 4000 raw values of word 25 from that record. 3995 were 0. The 5 others were correct: `0x5a5a1234` and `0xc0de0001` (the initialised words of the beacon) and a counter value `0xbe......`.
So the values are right when they arrive, and nearly all of them do not arrive. In the emulator the same `.dkm` delivered every value that the old scheme could deliver (every second record).

## 2. The old scheme

The module stores its step into the import `_dk_step`. The runtime (`dsp.rs patch_steps`, now deleted) pointed those stores at word 25 of one fixed reply record, the older of the two rings. The sites that call modules (`core/fx` `_fx_mark`, `core/dsp-voice`) cleared word 25 in both records before each module call, so that a step belonged to the call that stored it. The block tail copied the static `_dk_step` into the record only when no store was patched.

## 3. The cause **[D]**

The emulator with `test/step-beacon` and `digikit/probe` on the DN2 (1.12), with `patch_steps` bypassed (the static path):

- The module's store held the address of `_dk_step` after relocation (searched in the module's image in the DSP area: the 32-bit address field of the store was the plan's address of `_dk_step`). The import and its relocation are right.
- With the static path and the sites' clear of the records as they were, word 25 was 0 in every record (collector ring: all 0).
- With the same static path and the sites' clear of the records taken out (a test build; the tail unchanged), every record carried the beacon's word: counters, `0x5a5a1234`, `0xc0de0001` and the descriptor magic, each held for two calls, one word per block.

So the tail copy, the static word and the relocation work. What destroyed the word was the clear in the records. The two reply records alternate, one per block. `_fx_rec` and `_dv_rec` hold the record that the tail filled at the end of the previous block and the one before. The clear in block B + 1 therefore ran on the record of block B, the record that the ColdFire reads next, after the tail had copied the step into it. The old scheme worked for patched stores only because the module stored into that record after the clear. Only every second record has that address, and the probe stored each word twice to get one through. On a Digitakt II the reply DMA also sends the record while the clear runs, which explains why even the few words that were stored got lost. **[O]** The DMA timing on silicon is not measured. The emulator's DMA is instant, so the emulator shows the clear alone.

An earlier open question (why a static path delivered nothing in the emulator) is answered by the clear. The order of the hooks was not the problem.

## 4. The new design

- The module stores its step in the static word `_dk_step` (as before; no patching of the image).
- `core/fx` and `core/dsp-voice` clear the static word before each module call. They do not touch the records' word 25. The block tail gives each site the address of `_dk_step` (`_fx_rec[2]`, `_dv_rec[2]`, 0 until the first block with the link), because the sites do not require `core/dsp-link`.
- The block tail copies `_dk_step` into word 25 of the record of the block, after all module calls of the block (the hooks run before the tail in one block, finding 77), and clears it. Every record carries a word: 1500 words a second. A block in which the last module stored nothing carries 0.
- Deleted: `patch_steps`, `record_step_addr`, the step lists, the `STEP_PATCH` log kind, and the capacity tool's step-list term. `digikit/probe` keeps word 25 of every record; `test/sharc-probe` stores each word once; one run of 23,436 words takes about 16 s.

Check in the emulator (`tests/regression/runtime/sharc-probe.test.ts`, the beacon): no zero words, consecutive counter values, and the initialised constants exact, on every block, with no records missed.

## 5. What the design gives up

A step stored in the call that stops the DSP is in no record: the tail does not run again. The fault record shows the step of the last block that ended (`dlog.rs merge_stop`). The fixture `dyn-fx-hang` stores the steps 1 and 2 on every call and 3 on the call that hangs; the record now shows 2 (`tests/regression/runtime/fault-trace.test.ts`). A module that wants a mark in the stopping call must have stored it on the block before. **[O]** If a precise stop mark matters more, the old scheme (stores patched into the record) has to come back, with the clear moved out of the records and the double-store removed.

**[O]** The new design is not yet run on a Digitakt II.
