# SHARC+ cycle counter (EMUCLK, EMUCLK2) consumers on DT2 1.17 and DN2 1.12

Clock audit, item P7 of [profiling.md](../plans/profiling.md) section 8. Static evidence only (sharcdb `reguse`, `regdef`, `insn`, `functions`, `edges`; `fw.py` and `refscan` on the ColdFire side; this repo's sources). No emulator run. Marks: **[D]** read once here, **[O]** open. Nothing here is **[V]**: a second agent has not checked it against image bytes.

## Answer

**No EMUCLK read affects control. Every consumer reports.** Each image has exactly one reader of the counters in code the firmware can run: the per-block handler of the stock DSP firmware. It reads the low word at its start and at its end and stores the difference in the reply record. Nothing in the handler compares, branches on or computes with that value other than the subtraction and the store. On the ColdFire side, only `core/health` of this repo reads the stored word, and it feeds the HEALTH page and the log. **[D]**

## Method

- sharcdb: every `reguse` and `regdef` row with `EMUCLK` or `EMUCLK2`, joined to `insn` and `functions`. The registers are user-level read-only, so a `regdef` row is a decode of a write form (`15a`, `5a` with the counter as destination); none of them is a real write.
- Rows outside every function are data bytes decoded as code (see "Not consumers"). The check: no function covers the address, the decode shows `!!GAP!!`, `UREG_RESERVED_xx` or a `TCOUNT = EMUCLK2` pairing with an unrelated compute field.
- This repo: `grep -i emuclk` over the tree (no `out/`, no `firmware/`), then the readers of the ColdFire word.

## Consumers

| Image | CPU | Function / address | What it reads | Use | Reachability | Mark |
| --- | --- | --- | --- | --- | --- | --- |
| DT2 1.17 | SHARC, stock | `FUN_001c74cd` (`0x1c74cd..0x1c75d8`): start `0x1c74e9` EMUCLK2 (R15), `0x1c74eb` EMUCLK (R14); end `0x1c75a9` EMUCLK2 (R1), `0x1c75ab` EMUCLK (R0) | both words, 32-bit moves | the delta of the low words is stored (see below); reporting | called at `0x1c7756` (delayed call) from the block loop function `0x1c75d8` each block | [D] |
| DN2 1.12 | SHARC, stock | `FUN_001c9d6b` (`0x1c9d6b..0x1c9e76`): `0x1c9d87`/`0x1c9d89` start, `0x1c9e47`/`0x1c9e49` end | same | same | called at `0x1c9ff4` from `0x1c9e76` each block | [D] |
| both | ColdFire | `core/health` `health_block`, hooked at the entry of the block ISR (DT2 `0x4002dd0c`, DN2 `0x40025e36`; anchors `cf.link.fn.block_isr`, `cf.link.site.reply_copy_lit`) | word 0 of the record copy (DT2 `0x8000488c`, DN2 `0x800053a4`), once per block | window sum, maximum and count (1500 blocks), published as `dsp_avg`, `dsp_max` | the block ISR, 1500 per second | [D] |
| both | ColdFire | runtime `dkrt_health` (`modules/core/runtime/src/rs/src/lib.rs`): `dsp_avg_x10 = dsp_avg * 1000 / DSP_BLOCK_CYCLES` | the window figures | HEALTH page (`modules/core/apps/digikit-menu`), the `HEALTH` log record of `watch.rs` `snapshot` every 10 s, `dklog`, `dk-session` `runtime-log.ts` | on demand and every 10 s | [D] |
| both | SHARC, test | `tests/fixtures/modules/dsp-facts` (`dsp_facts.s`, `facts_seq.inc`), driven by `tools/device/dkprobe_facts.py` | `R2 = EMUCLK` pairs around snippets | instruction and block cycle measurements | probe module only, loaded on request | [D] |
| both | SHARC, adapters and machines of this repo | `core/dsp-link`, `core/dsp-voice`, `core/fx` and the machine and effect modules | no read | none | no `EMUCLK`, `EMUCLK2` in any `src/dsp` source | [D] |

### The handler, in detail (same code on both images)

- The two reads sit in the two delay slots of a delayed `JUMP`: EMUCLK2 first, then EMUCLK. The jump target is the next word, so they are plain reads.
- Start: `R14 = dec(R14)`. End: `R0 = dec(R0)`. Then `R2 = sub(R0, R14)`. The two `dec` cancel: the value is `end_low - start_low` modulo 2^32, in core cycles. **[D]**
- Width and wrap: 32 bits, unsigned, no wrap test. A block would have to take more than 2^32 cycles (4.3 s at 1 GHz) to give a wrong value.
- 64-bit pair: both high words are read and discarded. `R15` (start high) is saved at `0x1c74e2` and restored by the return frame; `R1` (end high) is overwritten by a load at `0x1c75b6`. The result is not a 64-bit delta. This matches profiling.md section 8.
- The value is rotated by 16 bits (`R0 = R2 >> 16`, `R2 = R2 << 16`, `or`), then stored through the record pointer at `0x1c75c4` (DT2) and `0x1c9e62` (DN2). The ColdFire reads it as a plain 32-bit word, so the rotation is how the DSP-to-ColdFire transfer orders the halves (finding 41 compares the window average with a direct read of the record: they agree **[D]**).
- Between the start and end reads, the only conditional branch (`0x1c7511`, `0x1c9daf`) tests a flag set by `lshift(R2, -2)` on the work-list word, not a clock value. `R14` and `R0` take no other part in any compare. The `btgl` of bit 0 of the state word (`DM(0x25f780)` DT2, `DM(0x268a38)` DN2) selects the reply record of the block and does not use the clock.
- Cost: four 32-bit moves and the subtraction, a few instructions per block.

## Not consumers

- DN2 1.12 has 48 more `regdef` rows and 8 more `reguse` rows, all at `0xb90880..0xb97377` (inside the code block that starts at `0xb80000`). None is in a function (no instruction of a function lies in `0xb90000..0xb98000`). The decode is noise: `!!GAP!!` compute fields, `UREG_RESERVED_62 = R2`, `TCOUNT = EMUCLK2` beside an unrelated compute field, and `PM(I15 - 0x3fa03f30) = EMUCLK` with 30-bit displacements. These words are table data. **[D]**
- DT2 1.17 has one `regdef` row (a write decode) at `0x16b961`, outside every function, in a tiny loader block. **[D]**
- `15a` and `5a` forms with the counter as destination are writes by decode only. No instruction in a function writes either counter.

## ColdFire side: who reads the record word

- The stock block ISR copies the DSP reply record with `FUN_400cd420(0x802, DAT_80005348, 0xabc, 0x8000488c)` (DT2): 0xabc bytes into the record copy.
- `refscan` over the DT2 1.17 main image (96.8 % byte coverage) finds three absolute references to `0x8000488c`: the `pea` in the ISR, a `lea` in `FUN_4002d36c`, and an `addi` in the ISR body. `FUN_4002d36c` reads its first 14 indexes from `0x4fe57100` and indexes 14 and up from halfwords at `0x800048a8` and later, so it does not read word 0. The `addi` at `0x4002f0dc` builds an address that reads 16 bytes at `+0xa9c` (not word 0). So the stock ColdFire code has no direct reader of word 0. This is a lower bound: a pointer in memory can reach it. **[D]/[O]**
- Only `core/health` reads word 0 (`health_rec_lit`, the literal of the `pea`).
- The runtime treats `dsp_avg_x10` only as an output: no module, no scheduler and no voice allocation reads `dsp_avg`, `dsp_max` or `cpu_x10` (grep over `modules`, `sdk`, `apps`, `packages`: runtime `lib.rs`, `watch.rs` snapshot, `digikit-menu`, `dk-session`). Throttling, voice stealing and budget decisions do not use the clock. The runtime's DSP stall guard uses the heartbeat word of `dl_diag` (`DIAG_HEARTBEAT`), a counter, not EMUCLK. **[D]**

## Emulator

- The engine sets EMUCLK, EMUCLK2 to one tick per completed instruction (`packages/emulator/sharc/src/lib.rs`: `instruction_clock`, `block_models_after`, the idle-replay path). It is explicitly not a cycle model, and it includes idle replay. `docs/CONTROL.md` tells readers to compare the HEALTH page of an emulator run with `model_pct`, not `real_pct`.
- `tools/sharc_dev/sharc_rsgen.py` `UNSAFE_READ_NAMES = ("EMUCLK", "EMUCLK2", "TCOUNT")` and `MODEL_REG_NAMES` keep any block that reads them out of the AOT block tier: the engine advances the clock only between instructions, so a block would read a stale value. So code that reads EMUCLK, such as the handler of both images, cannot run as an AOT block. Read from source, not run. **[O]**
- The measured DT2 1.17 hardware value (finding 47, rung C): 40.8 % average, 44.2 % maximum at the assumed 1 GHz block; the emulator model gives about 31 % on the same card. So the counter counts on the device. **[D]**

## Open

- **[O]** DN2 hardware: no device reading of the handler's delta is recorded here. DT2 has one (finding 47).
- **[O]** A reader of the DSP reply record word 0 on the SHARC side or through a ColdFire pointer: the static search finds none; indirect access was not excluded. If one exists, it would still be a reader of a stored value, not a read of the counter.
- **[O]** TCOUNT (core timer) and the other timing sources (PIT, DMA completion) were not audited; the question was EMUCLK.
- **[O]** Hardware properties of EMUCLK that the plan lists (pauses under a debugger, includes IDLE) rest on the public manual, not on a device test.
- **[O]** A check by a second agent against image bytes is needed before any of this is **[V]**.
