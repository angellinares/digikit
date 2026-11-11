# SHARC+ cycle counter (EMUCLK, EMUCLK2) consumers on DT2 1.17 and DN2 1.12

Clock audit, item P7 of [profiling.md](../plans/profiling.md) section 8. Static evidence only (sharcdb `reguse`, `regdef`, `insn`, `functions`, `edges`; `fw.py` and `refscan` on the ColdFire side; this repo's sources). No emulator run. Marks: **[V]** checked by a second agent against the image bytes (2026-11-12: words decoded from `out/sections/<image>/section_7_BLOB.bin`, section hashes matching the `.syx` files), **[D]** read once here, **[O]** open.

## Answer

**No EMUCLK read affects control. Every consumer reports.** In the decoded code of each image there is exactly one reader of the counters: a handler of the stock DSP firmware, called from the DSP task loop. It reads the low word at its start and at its end and stores the difference in the reply record. Nothing in the handler compares, branches on or computes with that value other than the subtraction and the store. **[V]** On the ColdFire side, only `core/health` of this repo reads the stored word, and it feeds the HEALTH page and the log. **[D]**

## Method

- sharcdb: every `insn` row whose fields name EMUCLK or EMUCLK2 (aligned or not), and every `reguse` and `regdef` row, joined to `functions`. The registers are user-level read-only, so a `regdef` row is a decode of a write form (`15a`, `5a` with the counter as destination); none of them is a real write.
- Rows outside every function were checked as data (see "Not consumers").
- This repo: `grep -i emuclk` over the tree (no `out/`, no `firmware/`), then the readers of the ColdFire word.

## Consumers

| Image | CPU | Function / address | What it reads | Use | Reachability | Mark |
| --- | --- | --- | --- | --- | --- | --- |
| DT2 1.17 | SHARC, stock | `FUN_001c74cd`: start `0x1c74e9` R15 = EMUCLK2 (`bf76bf07`), `0x1c74eb` R14 = EMUCLK (`be763f87`); end `0x1c75a9` R1 = EMUCLK2 (`bf76bf00`), `0x1c75ab` R0 = EMUCLK (`be763f80`) | both words, 32-bit moves | the delta of the low words is stored (see below); reporting | `CALL 0x1c74cd` at `0x1c7756` (`04181c00cd74`) in the loop of the RTOS task at `0x1c7749`, skipped unless a counter equals 1 (see below) | [V]; call frequency [D] |
| DN2 1.12 | SHARC, stock | `FUN_001c9d6b`: `0x1c9d87`/`0x1c9d89` start, `0x1c9e47`/`0x1c9e49` end, same words as DT2 | same | same | `CALL 0x1c9d6b` at `0x1c9ff4` (`04181c006b9d`) in the loop of the RTOS task at `0x1c9fe7`, same gate | [V]; call frequency [D] |
| both | ColdFire | `core/health` `health_block`, hooked at the entry of the block ISR (DT2 `0x4002dd0c`, DN2 `0x40025e36`; anchors `cf.link.fn.block_isr`, `cf.link.site.reply_copy_lit`) | word 0 of the record copy (DT2 `0x8000488c`, DN2 `0x800053a4`), once per block | window sum, maximum and count (1500 blocks), published as `dsp_avg`, `dsp_max` | the block ISR, 1500 per second | [V] for the source; [D] for the rest |
| both | ColdFire | runtime `dkrt_health` (`modules/core/runtime/src/rs/src/lib.rs`): `dsp_avg_x10 = dsp_avg * 1000 / DSP_BLOCK_CYCLES` | the window figures | HEALTH page (`modules/core/apps/digikit-menu`), the `HEALTH` log record of `watch.rs` `snapshot` every 10 s, `dklog`, `dk-session` `runtime-log.ts` | on demand and every 10 s | [D] |
| both | SHARC, test | `tests/fixtures/modules/dsp-facts` (`dsp_facts.s`, `facts_seq.inc`), driven by `tools/device/dkprobe_facts.py` | `R2 = EMUCLK` pairs around snippets | instruction and block cycle measurements | probe module only, loaded on request | [D] |
| both | SHARC, adapters and machines of this repo | `core/dsp-link`, `core/dsp-voice`, `core/fx` and the machine and effect modules | no read | none | no `EMUCLK`, `EMUCLK2` in any `src/dsp` source | [D] |

### How the handler is reached

- The DSP task loop is the RTOS task at `0x1c7749..0x1c775d` (DT2, `JUMP 0x1c7749` closes it) and `0x1c9fe7` (DN2). The databases' function boundaries at `0x1c75d8` (DT2) and `0x1c9e76` (DN2) are not the loop: that region holds handler fragments that jump back to `0x1c758b` inside the handler. **[V]**
- Before the call, `CALL 0xb86b1e` returns a counter in R0, loaded from `DM[*0x2ca3e0 + 0x50]`; then `R2 = btgl(R0, 0)` (`0x1c7750`) and `JUMP IF NOT SZ` (`0x1c7753`) skip the handler unless R0 equals 1 (DN2: `0x1c9fee`, `0x1c9ff1`). **[V]**
- That this means "once per block" is inferred from the HEALTH figures (finding 41), not decoded: what the counter counts is open. **[D]**

### The handler, in detail (same code on both images)

- The two reads sit in the two delay slots of a delayed `JUMP` to the next word, so they are plain reads: EMUCLK2 first, then EMUCLK. **[V]**
- Start: `R14 = dec(R14)` (`0x1c74ed`). End: `R0 = dec(R0)` (`0x1c75ad`). Then `R2 = sub(R0, R14)` (`0x1c75ae`). The two `dec` cancel: the value is `end_low - start_low` modulo 2^32, in core cycles. **[V]**
- Width and wrap: 32 bits, unsigned, no wrap test. A block would have to take more than 2^32 cycles (4.3 s at 1 GHz) to give a wrong value.
- 64-bit pair: both high words are read and unused. `R15` (start high) is saved at `0x1c74e2` and restored at `0x1c75d5` in the return's delay slot; on the flagged path it is overwritten at `0x1c7519` before its only read. `R1` (end high) is overwritten at `0x1c75b6` (`R1 = DM(0x25f780)`) with no read in between. The result is not a 64-bit delta. This matches profiling.md section 8. **[V]**
- The value is rotated by 16 bits (`0x1c75b3` `R0 = lshift(R2, -16)`, `0x1c75b9` `R2 = lshift(R2, 16)`, `0x1c75bf` `or`), then stored with `DM(I4, M5) = R2` at `0x1c75c4` (DT2) and `0x1c9e62` (DN2). I4 comes from `DM(I6 - 2)`, which the callee `0x1c778a` (DN2 `0x1ca020`) fills with a record address. **[V]** The ColdFire reads it as a plain 32-bit word, so the rotation is how the DSP-to-ColdFire transfer orders the halves (finding 41 compares the window average with a direct read of the record: they agree). **[D]**
- The only conditional branch in the handler (`0x1c7511`, DN2 `0x1c9daf`) tests a flag from `lshift(R2, -2)` of the work-list word, not a clock value. The `btgl` of bit 0 of the state word (`DM(0x25f780)` DT2, `DM(0x268a38)` DN2) selects the reply record and does not use the clock. **[V]**
- An indirect `JUMP` through I12 (`0x1c7521`, DN2 `0x1c9dbf`) goes to the work-list handler loaded at `0x1c7514`. Its fragments return to `0x1c758b`, so the end read is reached, and no fragment uses R14 or R0 as a compare operand. **[V]** Whether their callees (`0x1c7462`, `0x1c2b24` and others) preserve R14 was not followed. **[O]**
- Cost: four 32-bit moves and the subtraction, a few instructions per block.

## Not consumers

- Every other row that names EMUCLK or EMUCLK2 lies outside every function: 26 on DT2, 218 on DN2. **[V]**
- DN2 `0xb90000..0xb98000` holds 26759 decoded rows, none in a function, with no incoming edge and no function entry at or above `0xb90000`. The words (from `0xb90870`: `0b61 c962 7a64 1e66 b567 3f69 bb6a 296c ...`) have steadily rising high bytes: a lookup table, not code. **[V]**
- DT2 `0x16b961` is ASCII text. **[V]**
- This covers decoded code only. The database's decoder decides what is code, so undecoded code cannot be excluded. **[D]**
- Sections `2_DSP` and `8_SECTION` hold ColdFire code (no loader blocks; section 2 starts with `4e75`), not SHARC code. **[V]**

## ColdFire side: who reads the record word

- The stock block ISR copies the DSP reply record with `FUN_400cd420(0x802, DAT_80005348, 0xabc, 0x8000488c)` (DT2): 0xabc bytes into the record copy.
- `refscan --base 0x40000400` over the DT2 1.17 main image finds exactly three references to `0x8000488c`: the `pea` at `0x4002dd60` in the ISR, a `lea` at `0x4002d3a8`, and an `addi` at `0x4002f0dc`. The `lea` reads at `+0x1c` and up; the `addi` reads 16 bytes at `+0xa9c` or `+0xaac`. Other references into the copy (`0x800048a2`, `0x800048a8`) are not word 0. So the stock ColdFire code has no direct reader of word 0. **[V]** This is a lower bound: a pointer in memory can reach it, and DN2 was not scanned. **[D]/[O]**
- In this repo only `core/health` reads word 0 (`health_rec_lit`, the literal of the `pea`); `dl_diag` in `core/dsp-link` uses the same literal at other offsets. **[V]**
- The runtime treats `dsp_avg_x10` only as an output: no module, no scheduler and no voice allocation reads `dsp_avg`, `dsp_max` or `cpu_x10` (grep over `modules`, `sdk`, `apps`, `packages`: runtime `lib.rs`, `watch.rs` snapshot, `digikit-menu`, `dk-session`). Throttling, voice stealing and budget decisions do not use the clock. The runtime's DSP stall guard uses the heartbeat word of `dl_diag` (`DIAG_HEARTBEAT`), a counter, not EMUCLK. **[D]**

## Emulator

- The engine sets EMUCLK, EMUCLK2 to one tick per completed instruction (`packages/emulator/sharc/src/lib.rs`: `instruction_clock`, `block_models_after`, the idle-replay path). It is explicitly not a cycle model, and it includes idle replay. `docs/CONTROL.md` tells readers to compare the HEALTH page of an emulator run with `model_pct`, not `real_pct`. **[D]**
- `tools/sharc_dev/sharc_rsgen.py` `UNSAFE_READ_NAMES = ("EMUCLK", "EMUCLK2", "TCOUNT")` and `MODEL_REG_NAMES` keep any block that reads them out of the AOT block tier: the engine advances the clock only between instructions, so a block would read a stale value. So code that reads EMUCLK, such as the handler of both images, cannot run as an AOT block. Read from source, not run. **[O]**
- The measured DT2 1.17 hardware value (finding 47, rung C): 40.8 % average, 44.2 % maximum at the assumed 1 GHz block; the emulator model gives about 31 % on the same card. So the counter counts on the device. **[D]**

## Open

- **[O]** What the gate counter at `DM[*0x2ca3e0 + 0x50]` counts, and so how often the handler runs per audio block.
- **[O]** Whether the callees of the work-list fragments preserve R14 between the start and end reads.
- **[O]** DN2 hardware: no device reading of the handler's delta is recorded here. DT2 has one (finding 47).
- **[O]** A reader of the DSP reply record word 0 on the SHARC side, through a ColdFire pointer, or on the DN2 ColdFire side: the static search finds none; indirect access was not excluded. If one exists, it would still be a reader of a stored value, not a read of the counter.
- **[O]** TCOUNT (core timer) and the other timing sources (PIT, DMA completion) were not audited; the question was EMUCLK.
- **[O]** Hardware properties of EMUCLK that the plan lists (pauses under a debugger, includes IDLE) rest on the public manual, not on a device test.
