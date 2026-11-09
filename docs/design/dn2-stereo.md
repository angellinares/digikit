# DN2 stereo: design and as built

Status: phase 1 accepted by Em on 2026-11-01 (section 7), phase 2 (every stage between the machine and the pan has an R twin, section 6) approved on 2026-11-02. Built in the lane `lane/dn2-stereo`; model tests only. The DN2 emulator proof (section 8) is not run. DN2 OS 1.12 (SHA-256 of the SYX in finding 96). Marks: **[D]** read from image bytes or measured on the model, **[O]** open, **[P]** proposal.
Addresses are SHARC short-word pcs. Offsets in `devices/fw/dsp/voice.toml` are `pc - 0x1c8ef1` (engine function entry) or `pc - 0x1c9b73` (its continuation fragment, hash `d72f9ef5...`); check: `0x1c94fd - 0x1c8ef1 = 0x60c`, the first-order site.

## 0. Idea in four lines

- A stereo machine returns `DK_STEREO` and writes R to a per-slot R buffer. `dsp-voice-dn2` checks R, scales it like the engine scales L, and stamps the slot "stereo in this block" with the block counter.
- Each pre-pan stage that has an R twin runs a second time on the R buffer, with a private R state record. L stays the stock, authoritative path. The two inline gain stages without a call (block ramp, fade) run on R in one hook before the pan loop.
- Every stage between the machine and the pan has a twin. A twin that meets a context it cannot run in (a guard) clears the stamp: the pan loop then plays R = L for that block, the guard counts it per stage and the health word shows it.
- The pan loop is patched once, per slot: a stamped slot gets `plane B = gain B * R`, every other slot runs the stock instructions.

## 1. Static answers (all [D] unless marked)

### 1.1 Loop-branch restriction at the hook site `0x1c9af6`

A jump at `0x1c9af6` is legal. The Programming Reference (SHARC+ Core, Rev 1.5, "Restrictions on Ending Loops" p. 4-41 and "VISA-Related Restrictions on Hardware Loops" p. 4-42) limits what may stand at the end of a loop: a call in the last 5 instructions needs a return with the loop-return modifier, no `IF NOT LCE` after a `CURLCNTR` write, no `IDLE` in the last 3, the last 11 instructions are 48-bit, and the loop and PC stacks must not be pushed, popped or written inside the loop.

- The body `0x1c9aea..0x1c9b30` has 29 instructions: 18 before the inner loop, the 10 of the inner `DO 0x1c9b2d`, and the loop end `0x1c9b30`. `0x1c9af6` is the 7th, the 23rd from the end: outside every window of the rules above.
- The patch is a plain `JUMP` out and back. It touches neither stack. The wrapper's own `DO` nests as the stock inner `DO` does (stack depth 2 of 6); the stereo path ends with a `JUMP` to the loop-end instruction `0x1c9b30`, as the stock `JUMP IF EQ` at `0x1c9b07` does. The mono path returns to `0x1c9af9`.
- The hook site is a 48-bit instruction (Type 7a), as the replacement `JUMP` is. The next instruction `0x1c9af9` is 16 bits.
- Model check: the full block `0x1c99de..0x1c9b33` runs 16 slots with the hook; all 16 slots are processed (loop count intact) and a mono slot is bit-identical to the stock block. No hardware claim: the model does not model the sequencer's pipeline restrictions **[O]**, the emulator test covers the integrated run.
- The slot index is not read from `LCNTR`: the hook takes it from `I3` (the slot's entry of the table of voice buffers, `DM(I6 - 24)` is its base): `4 v = I3 - DM(I6 - 24)`. The twins take it from the voice buffer `R8` in the same table.

### 1.2 Steal and retrigger: trigger edge and reset site

- Allocator `0x4002a6a2` (called from the frame builder `0x40025e36`, vector-191 handler) reuses a slot in place when it steals or retriggers: it overwrites the slot's record (`0x40288300 + 0x14 v`: track, note, age), clears the old owner's bitmaps and returns the slot in the allocated mask (low half of the 64-bit result). Steal mode `_DAT_4059da30` 0 (oldest), 1, 2 and 3 differ only in which slot is chosen. The high half is the voices of the same note that were not reused; the builder frees them and sets their release bit.
- The builder ORs the bit of every returned slot into `local_b0` (one OR for each slot of the allocated mask, in the loop over the result), and writes it to `_DAT_80005e82` (`0x40027498`), the frame mask `0x11` that is `ADM_TRIG_HW`. `local_b0` is zeroed at every entry of the builder (every frame): the bit is a one-frame pulse.
- So a steal or retrigger gives the same pulse in the same slot as a fresh note-on: a rising edge of bit v, unless the slot was also triggered in the previous frame (two note-ons in consecutive frames; then the bit stays set and the adapter sees no edge, as for every module today). The adapter's primary R clear at the note-on block is therefore the steal and retrigger reset. **[D]** static reading of the decompiled builder and allocator (second check pending); the emulator test covers it with a real steal.
- The stock reset of the L stage states happens in the control part of the same block as the pulse (before the audio chain), at `0x1c9642..0x1c96d1` (entered from `0x1c9343`): first-order reset `0xb80b93` at `0x1c96cb`, U/Y reset `0xb8135a` at `0x1c96aa`, case 5 reset `0xb818c7` at `0x1c9673`, case 3 `0xb811f1` at `0x1c9692`, case 2 `0xb81c8d` at `0x1c9684`, case 1 `0xb81d6d` at `0x1c964b`, ring reset `0xb805d8` at `0x1c96b3`. The note-on resets are covered by the adapter's clear of the R records at the first block of the machine (the same state, one block later). The resets in the middle of a note (a filter, SRR or ring-delay change) are mirrored (section 2.4).

### 1.3 The constant gain of the chain (`0x1c9448..0x1c94fd`)

The loop `0x1c9478..0x1c94c5` multiplies every voice buffer by the constant `0x3fcade01` (about 1.5898, loaded at `0x1c946b` and `0x1c948c`) in SIMD pairs (`MODE1` PEYEN set at `0x1c9484`, cleared at `0x1c94b0`). It runs for all 16 buffers right after the render-hook loop (`0x1c9453..0x1c9467`), skipped only for a block count of zero. Its multiplications are plain 32-bit products, so a scalar multiply of R gives the same bits. `_gs_post` scales R by `0x3fcade01` after the guard.

### 1.4 Two inline stages the first spec did not list

`0xb80345` is the envelope generator, not a "final gain ramp". After the gain loop `0x1c99bd..0x1c99d6` two more inline stages multiply the L buffers, with no call, so the twins cannot reach them:

- **Block ramp** `0x1c99ed..0x1c9a31`: for every slot, `out[k] = r_k * in[k]`, `r_0 = DM(rec)`, `r_{k+1} = r_k + DM(rec + 2)`; the record is `engine + 0x1395c + 28 v` (7 words). Unconditional.
- **Conditional fade** `0x1c9a59..0x1c9aac`: for slot v when byte v of the array `engine + 0x138fc` (a byte access, Type 3b with `(l, x, w) = (0, 0, 0)`) and the word `engine + 0x1351c + 44 v` (word 5 of the `0xb80345` record) are both nonzero: `r` runs from 1.0 with the step `-1/31` (`0xbd042108`, the value of the stock division `0x1c06ba` of -1.0 by 31.0; tested against the stock block).
- The metadata clear `0x1c9aaf..0x1c9acf` zeroes the fade flags before the pan loop. A pan-loop hook cannot read them; the R fade needs a hook before the clear (section 2.3).

Both stages are linear per-sample gains, so R needs the same multiplications in the same order.

### 1.5 Exact call sites of the stages

The dispatch table at `0x8052dbc0` holds `0x1c98b9` (case 0 and above 6: skip), `0x1c98ab` (1), `0x1c9ba9` (2), `0x1c9bc4` (3), `0x1c9bdf` (4), `0x1c9bf7` (5), `0x1c9c45` (6). The first spec called `0x1c9c4f` the case 5 continuation: it is case 6's ring call.

| Stage | Call site (pc) | Rule: function + offset | Kind |
| --- | --- | --- | --- |
| first-order `0xb80c39` | `0x1c94fd` | engine `0x60c` | twin (existing site) |
| first-order reset `0xb80b93` | `0x1c96cb` | engine `0x7da` | mirror |
| U/Y `0xb81368` early | `0x1c959f` | engine `0x6ae` | twin |
| U/Y late | `0x1c9b7a` | fragment `0x7` | twin |
| U/Y reset `0xb8135a` | `0x1c96aa` | engine `0x7b9` | mirror |
| held sample `0xb8265b` early / late | `0x1c95b4` / `0x1c9b8f` | engine `0x6c3` / fragment `0x1c` | twin |
| shaper `0x1cdb56` early / late | `0x1c95c9` / `0x1c9935` | engine `0x6d8` / `0xa44` | twin (late with the scalings) |
| case 1 `0xb82342` | `0x1c98af` | engine `0x9be` | twin |
| case 2 `0xb81a16` | `0x1c9bb6` | fragment `0x43` | twin |
| case 3 `0xb80f2e` | `0x1c9bd1` | fragment `0x5e` | twin |
| case 4 ring `0xb806f5` | `0x1c9be9` | fragment `0x76` | twin |
| case 5 `0xb81526` | `0x1c9c08` | fragment `0x95` | twin + blend |
| case 6 ring `0xb806f5` | `0x1c9c4f` | fragment `0xdc` | twin |
| gain `0xb80345` | `0x1c99cc` | engine `0xadb` | twin |
| pre-pan hook | `0x1c9aaf` | engine `0xbbe` (len 6; next `0x1c9ab2`, `0xbc1`, len 4) | replace |
| pan hook | `0x1c9af6` | engine `0xc05` (next `0x1c9af9`, `0xc08`, len 2; end `0x1c9b30`, `0xc3f`) | replace |

All of them are call instructions with both delay slots, retargeted as the existing sites. The stages `0xb8265b` (entry inside the function at `0xb8258f`, offset `0xcc`) and the dispatch stages have one caller each in the table above; the other callers of the shared functions are not retargeted. Every call has R8 = the voice buffer, the same table entry as the slot's pointer in the pan loop. The sites of the resets in the middle of a note are in section 2.4.

### 1.6 Two static answers of phase 2

- **The stage on the default track [D] for the gates, [O] for the stored default.** Filter type 0 (multi-mode) is dispatch case 0, which skips the whole dispatch, so the default track reaches a stage without a twin only through the shaper (overdrive, `0x1cdb56`) or the held-sample stage (SRR, `0xb8265b`). Both are entered whenever their routing byte is 0 (early, `LEFTZ` + `IF SV` at `0x1c94d9`, `0x1c9517`) or 1 (late, `BTGL` bit R11 = 0 at `0x1c98e4`, `0x1c998f`); the UI offers exactly the routing values 0 and 1 ("OVR routing", "SRR routing", findings 03 and 53), so the stages are entered at every setting, whatever the amount (at amount 0 they change the block little or not at all, but they run). So both stages, in this order (shaper before the first-order stage, held after it), reached the fallback of phase 1 on every default stereo voice; `_gs_nbad` counted only the first because the first wrapper cleared the stamp. The writer of the routing bytes (`0x254aa4`, `0x254ab4`) is a store through a frame pointer from frame bytes and was not found as a direct store, so the stored default value stays **[O]**; the counters of section 2.1 name any stage that is still refused in the next run. Both stages have twins now.
- **The gain stage `0xb80345` reads audio only as the multiplicand [D].** The listing (`0xb803d9..0xb803f7`) loads each input sample, negates and multiplies it by the running gain, and stores the product; no word of the 11-word record is computed from a sample (word 10 is the gain level, word 5 the fade flag, word 7 the phase, word 9 the counter, all driven by the control words and the rate tables `0x26a570`, `0x26a770`, `0x26a970`). So its state could be shared by the two channels. The twin does not share the record, it copies all 11 words before the L call and runs the stock function on the R buffer, so R follows L's gain trajectory and applies it to its own audio (DT2 runs the stage per voice). No change was needed.

## 2. As built

### 2.1 Adapter (`dsp-voice-dn2.s`, `_gs_post`, `_gs_block` in `dsp-voice-dn2-stereo.s`)

- `out_r = _gs_rbuf + 128 v` (16 x 32 floats). The status bit `DK_STEREO` is kept in `_dv_c + 48`.
- After the guard of L: `CALL _gs_post`. For a mono return it is 4 instructions. For a stereo return: the latch `_gs_seen[v] = heartbeat` (every stereo slot, whatever the checks say; never cleared by a stage), then the guard of R (finite, at most 8.0; else no stamp, no fault: the slot plays R = L for the block), the cap, R times `0x3fcade01`, the stamp.
- Cap `_GS_MAX_STEREO` = 8: the first 8 stereo slots of a block are stamped (counter `_gs_ns` of the block `_gs_nhb`); the others play R = L. A slot that stops being stereo frees its place in the next block.
- Stamp: `_gs_stamp[v] = heartbeat` (`_dv_diag` word 0, once per block, slot 0). It expires with the next block.
- R stage records (first-order 28 words, U/Y 8, gain 11, case 5 53, held 5, case 1 51, case 2 75, case 3 59, ring 18; the shaper keeps none) are cleared at the note-on block and whenever the stamp of the previous block is not the previous heartbeat (a gap). The ring record starts as the stock reset leaves L: history and cursor 0, fill status (word 17) 1. R buffers are never cleared: a stereo machine writes all 32 samples.
- Memory (`.PKDATA`, loaded as zeros): stamps 64 B, latch 64, counters 128, R buffers 2048, first-order 1792, U/Y 512, gain 704, case 5 3392, case 5 R scratch 2048, shaper 1536, held 320, case 1 3264, case 2 4800, case 3 3776, ring 1152, 12 words of counters: about 29 KiB for 16 slots, plus the R rings in DDR (section 2.5).
- **Counters and health (visible to the host by symbol).** `_gs_cnt` (32 words): 0 held early, 1 held late, 2 shaper early, 3 shaper late, 4 case 1, 5 case 2, 6 case 3, 7 ring (case 4), 8 ring (case 6), 9 first-order, 10 U/Y, 11 gain, 12 case 5 count the refusals of an unsupported context by that twin (the guard); 16 bad R; 17 cap; 18 a gap (R restarted in a running voice). `_gs_nbad` is the total of stamps the guards cleared. `_gs_health` is the word of the last completed block, written by `_gs_block` from the heartbeat code of slot 0 (`_gs_hacc` collects the events of the running block): bit 0 some stereo slot played R = L (any reason), bit 1 a twin refused a context, bit 2 bad R, bit 3 cap. The stamp is cleared by the first refusing twin of a slot; a later twin of the same slot sees an unstamped slot and counts nothing, but the reason (the context) is the same for all twins of the block. The word is not in the dsp-link record yet: read it by symbol (`dspWords('_gs_health', 1)`); a spare word of `_dv_diag` words 0 to 13 could carry it into the record later.

### 2.2 Twins

Rule (replay-proven in finding 96): copy the shared control words L to R before the stock L call, run the L call, run the same stock function on the R record and the R buffer. The R pass never publishes control state to L. Only the words the stage reads are copied (the audited read sets of finding 96); the model tests poison every other word of the R record.

| Stage (symbols) | L record (stride) | Copied before L (shared) | R private (kept, cleared at note-on) |
| --- | --- | --- | --- |
| first-order `_gs_fo` | `engine + 0xe088 + 0x70 v` | 21 to 23 | 24, 25 (histories) |
| U/Y `_gs_uy` | `engine + 0x12148 + 0x20 v` | 2 to 5 | 6, 7 |
| gain `_gs_gain` | `engine + 0x13508 + 0x2c v` | all 11 | none |
| case 5 `_gs_c5` | `engine + 0x127c8 + 0xd4 v` | 11 to 48 | 9, 10 |
| shaper `_gs_shaper_early/late` | `engine + 0xe788 + 0x60 v` | 0, 1, 2, 5, 12 | none (no audio state) |
| held sample `_gs_held_early/late` | `engine + 0xeec8 + 0x14 v` | 0, 3, 4 | 1, 2 (held audio) |
| case 1 `_gs_case1` | `engine + 0xf308 + 0xcc v` | 5, 6, 7, 12, 14, 26, 28, 33, 35, 40, 42 | 3, 4, 48, 49, 50 |
| case 2 `_gs_case2` | `engine + 0xffc8 + 0x12c v` | 12 + 7k, 14 + 7k (k = 0 to 8) | 10, 11 |
| case 3 `_gs_case3` | `engine + 0x11288 + 0xec v` | 0, 1, 2, 3, 4, 14, 18, 25, 32, 39, 46, 53 | 5, 9, 10, 11, 12 |
| ring `_gs_ring4/6` | `engine + 0x12348 + 0x48 v` | 2, 3, 4, 5, 10, 15 | 0, 13, 14, 17 and the ring (word 16 is written, section 2.5) |

- A wrapper starts with the stamp check; an unstamped slot costs 13 instructions and reaches the stock function with R4, R8, R12 and the stack unchanged. The stereo path needs L3, L4, L5 = 0 and a block count of 32 (the guard: else it clears the stamp, counts the refusal and calls the stock function: R = L). MODE1 PEYEN is clear at every call site: the four SIMD sections of the engine between `0x1c9440` and `0x1c9ad2` hold no call and are cleared before the next one.
- Registers: the wrappers clobber only R0, R1, R2, R4, R8, R12, I4, I12 and the flags (the C convention, `sdk/dsp/call.inc`); the callee-saved ones are kept and tested.
- The shaper at its late call (`0x1c9935`) sits between the engine's own scalings of L (x16 before, x1/16 after, `0x1c98f9..0x1c9975`); `_gs_shaper_late` scales the R buffer by 16 before the R pass and by 1/16 after it, scalar multiplications by powers of two, which give the bits of the SIMD pairs. The early call has no scaling.
- Case 3: the call itself advances the shared words (current coefficients `{17, 18, 19} + 7k` and the prior values 46 and 53), so R starts from the state L saw before its call; replaying the stage on advanced state would change its ramps (finding 96).
- Case 5: the scratch block R12 of the L call is the firmware's (`DM(engine + 0x13808)`); the R pass writes the slot's R scratch. The blend that follows (`0x1c9c12..0x1c9c44`) runs for R inside the wrapper, reading the same ramp record `I0` and writing nothing to it. The L blend stays the stock continuation.

### 2.3 Hooks in the pan section

- Pre-pan hook `_gs_prepan` at `0x1c9aaf` (before the metadata clear): for every stamped slot, the block ramp and the fade on its R buffer, in place, as in section 1.4. Live in: I0, M13, M5, M7, R9, R11, I6; everything else is dead (`regs` in `voice.toml`). It needs R9 = 32.
- Pan hook `_gs_slot` at `0x1c9af6`, once per slot. Mono path (no stamp of this block, or R9 not 32): `I1 = 0; R0 = R9 - 1` (the register and the flags of the hook), the displaced `F4 = F2 * F8; modify(I1, M4)` as two instructions, jump to `0x1c9af9`. Stereo path: plane A as the stock loop, plane B from R, 31 trips of a 5-instruction loop; I3 is advanced by one word, as the stock `modify(I3, M6)` does (written `MODIFY(I3, 4)`: the assembler's `MODIFY(I3, M6)` is a raw 1-byte step, the stock instruction is the word-scaled Type 7a form); jump to `0x1c9b30`. Dead registers used: R1, R3 to R7, R10 to R15, I0, I1, I3 (a DM access takes I0 to I7).
- The pan gains are the stock gA, gB records (`TABLE + 0x354 + 28 v`, words 0, 2 and 112, 114). A stamped slot with R = L reproduces the stock pair bit for bit (tested).

### 2.4 Reset mirrors

The stock resets run in two places of the engine's control part: at the trigger of a note (`0x1c9642..0x1c96d1`) and, in the middle of a note, after a change of a stage's setting. The R mirror of the first is the adapter's clear of the R records at the first block of the machine. The second needs a mirror at the call: the wrapper sets the private words of the R record of the slot as the stock reset does for L, whatever the stamp says, then jumps to the stock reset. A pointer that is not the start of a slot's record only reaches the stock reset (the slot number is `(R4 - (engine + offset)) / stride`, checked exactly).

| Stage | Site (call of the reset) | Cause | R words set |
| --- | --- | --- | --- |
| first-order `0xb80b93` | `0x1c96cb` | note-on | 24 to 27 = 0 |
| U/Y `0xb8135a` | `0x1c96aa` | note-on | 6, 7 = 0 |
| held sample `0xb82440` | `0x1c91c5` | SRR routing byte changed | 0 to 4 = 0 |
| case 1 `0xb81d6d` | `0x1c9716` | filter change | 3, 4 = 0 (the snapshots 48 to 50 survive a stock reset) |
| case 2 `0xb81c8d` | `0x1c9747` | filter change | 10, 11 = 0 |
| case 3 `0xb811f1` | `0x1c976f` | filter change | 5, 9, 10, 11, 12 = 0 |
| ring `0xb805d8` | `0x1c97cd`, `0x1c989f` | delay change (case 4, case 6) | 0, 14 = 0, 17 = M14 (1); 13, 16 and the backing stay |
| case 5 `0xb818c7` | `0x1c9842` | filter change | 9, 10 = 0 |

Seven sites are new in `devices/fw/dsp/voice.toml` (rules by hash and offset in the engine function `0x1c8ef1`), with six function anchors for the stock resets; `devices/ports/dn2-1.12.toml` holds the resolved addresses. The other callers of the stock resets (`0x1c8d07`, `0x1c8d10` for the held reset, functions outside the engine) are not retargeted. The note-on sites of the stages other than the first-order and U/Y stage are not retargeted either: the adapter's clear gives the same R state one block later, and R does not run in the edge block.

**Accepted difference from DT2 (4c of the comparison with the stock DT2 engine).** DT2 resets each voice from its own record, so a reset whose cause depends on audio could hit one voice only. Here R is never reset on its own: R resets exactly when L does (mid-note mirror) or when its stamp restarts (note-on, gap). A reset caused by a control change is the same in both models; no reset in the stages read here is caused by audio.

### 2.5 The R rings (`_gs_ring4`, `_gs_ring6`)

- The stock ring stage keeps a 32 KiB delay ring per slot behind the byte pointer in word 16 of its record (`0x804ad790 + 0x8000 v`, finding 106; the 16 rings are `[0x804ad790, 0x8052d790)`, cleared at boot by `0xb80575`). A copied record would share the ring, so word 16 is never copied.
- R has 16 fixed rings, one per physical slot (no pool, no owner table, no ownership change on a steal): `ADM_RING_R_BASE + 0x8000 v`, where `ADM_RING_R_BASE` is `ring_r_base` of `[adapter.sample_pool]` in `devices/digitone-ii.toml` (generated into `build/gen/adapter-dn2.inc`). The twin writes that pointer into word 16 of the R record before every R pass.
- Placement: `ring_r_base = 0x904b1000`, the top 512 KiB of the former pool range `[0x80531000, 0x90531000)`. The pool range is 512 KiB shorter: `ddr_bytes = 0x0ff80000` (256 MiB minus 512 KiB, `SP_DDR_PAGES` 65536 to 65408; the pool is a placeholder until the extended soak, finding 106). Reason: finding 106 proves no stock write in `[0x80531000, 0xa0000000)`; the gap after the stock rings (14 KiB) is too small; L2 is not an option (its free size is not established). `patchkit gen` refuses rings below the stock writes, above the alias end, unaligned, or overlapping the pool range.
- Coherence: the DSP alone reads and writes the R rings, as it does the stock rings. No flush, no ColdFire access; the pool allocator never hands the bytes out (outside its range).
- Initial state and reset: the stage treats a ring whose fill status (record word 17, byte 0) is nonzero as not yet written and reads no wet signal until the write cursor has wrapped once; the stock reset leaves the backing dirty and sets that status. R starts the same way (the note-on clear and the mid-note mirror set word 17 to 1, history and cursor to 0), so no backing clear is needed. Model tests: a dirty R ring against a clean reference ring gives the same R after a reset (fill status 1); with the status of a filled ring (0) the dirty ring is audible, so the test can tell.

## 3. Cost (instruction counts on the model, 1 instruction = 1 cycle, no stalls), budget in cycles

Measured with the stock image on the model rig (`tests/support/dn2_stereo_rig.py`). Per call of a stereo slot, the stamped twin against the unstamped wrapper call (the stock call plus the 13 instructions of the check):

| Part | Stock call | Twin call | Twin minus stock (R pass + wrapper) |
| --- | --- | --- | --- |
| first-order | 657 | 1386 | 716 |
| U/Y (per site; early or late) | 204 | 480 | 263 |
| gain `0xb80345` | 237 | 616 | 366 |
| shaper early | 524 | 1123 | 586 |
| shaper late (with the two scalings) | 524 | 1323 | 786 |
| held sample (early or late) | 516 | 1103 | 574 |
| case 1 | 608 | 1303 | 682 |
| case 2 | 1140 | 2420 | 1267 |
| case 3 | 2208 | 4505 | 2284 |
| ring (case 4 or 6), fresh / near the wrap | 1514 / 1450 | 3109 / 2981 | 1582 / 1518 |
| case 5 + blend | 489 | 1553 | 1051 |
| block ramp (pre-pan, per stamped slot) | | | 128 (+ 128 with the fade) |
| pan hook: stereo slot against a mono slot | | | +15 |
| `_gs_post` (latch, guard, scale, stamp) | | | 241; 603 at a note-on block or a gap |

- Per stereo slot, default sound (first-order, U/Y, gain, shaper early, held early, ramp, pan, `_gs_post`; no dispatch case): 716 + 263 + 366 + 586 + 574 + 128 + 15 + 241 = about 2,900 instructions. Worst case (the same with the shaper late and case 3, the largest, plus the fade): 716 + 263 + 366 + 786 + 574 + 2284 + 256 + 15 + 241 = about 5,500. Case 3 and the ring stage are the heavy ones: their stock calls are 2,208 and about 1,500 instructions, more than the first estimate of the phase 2 spec (900 and 600).
- Budget in cycles (`devices/cycle-factors.json`: DN2 nominal x1.138, high bound x1.326; a DSP block is 666,667 cycles; the stock block is 334.5k instructions, 57 % nominal to 67 % at the high bound): 16 worst-case stereo slots are 88k instructions, 100k cycles nominal (15 %) and 117k at the high bound (17.5 %), so about 72 % to 84 % of a block with the stock engine. With the cap of 8 stereo slots: 44k instructions, 7.5 % to 8.8 %; the default sound on 8 slots is about 4.0 % to 4.6 %. The cap of 8 stays until the paired A/B on the emulator (`tools/devtools/coupled_ab.py`); the estimate does not need it, and 16 worst-case slots leave 16 % (high bound) to 28 % (nominal) of the block free.
- Unstamped slot: 13 instructions per wrapper call, 14 for the pan hook and about 12 for the pre-pan loop: a block with no stereo slot costs 423 instructions for the 16 slots (6.3 % of the stock block `0x1c99de..0x1c9b33`, 0.06 % of a DSP block), plus the 8 instructions of `_gs_block` once per block and 6 of the latch per stereo slot.

## 4. What breaks, and the guards

- **Sends, FX:** they read A/B after the loop and already take two planes. A stereo slot writes both planes in full; tested with L != R.
- **Steal and retrigger:** section 1.2. R records and rings are per physical slot, never keyed by track; no history sharing (tested: records of other slots are untouched).
- **Polyphony:** unchanged, 16 mono slots; a stereo voice uses one slot.
- **Stage state shared by two paths:** the R pass runs inside the call site of its stage, so it runs only when L runs.
- **Exceptional floats:** a bad R block (not finite, over 8.0) gives no stamp: R = L for that block, no fault; counted (`_gs_cnt` 16).
- **The guard:** an unsupported context (L3, L4 or L5 nonzero, a block count other than 32) clears the stamp and calls the stock function (R = L); counted per stage (`_gs_cnt` 0 to 12), in `_gs_nbad` and in `_gs_health`. The cap (`_gs_cnt` 17) and a gap in a running voice (18) are counted too. On the default track every counter must stay 0.
- **Registers and flags:** the pan hook uses only registers the epilogue reloads (section 2.3); the mono path restores R0 and the flags as at the hook.
- **Other callers of the stock functions:** only the sites of sections 1.5 and 2.4 are retargeted.
- **Module and DT2:** nothing changes on DT2 (the new ops and claims are `devices = ["dn2"]`; `adapter-dt2.inc` has no `ADM_RING_R_BASE`); `dsp-voice-dn2.s` and `dsp-voice-dn2-stereo.s` are the only DN2 files with stereo behaviour.
- **`core/dsp-stereo-proof`:** stays the private observer. It claims the first-order and U/Y sites too, so patchkit refuses both modules together (tested).

## 5. Files and tests

- `modules/core/adapters/dsp-voice/src/dsp/dsp-voice-dn2.s`, `dsp-voice-dn2-stereo.s`: one object, `module.toml` lists both sources.
- `devices/fw/dsp/voice.toml`, `modules/core/adapters/dsp-voice/module.toml`, `devices/ports/dn2-1.12.toml`, `modules/core/fw/reference.json`, `build/dsp-voice-dn2-dsp.dko.json`. The DN2 1.11 port was not regenerated: it was already stale for other anchors.
- `devices/digitone-ii.toml` (`ring_r_base`, `ddr_bytes`), `packages/patchkit/src/devgen.rs` (the key, its checks and `ADM_RING_R_BASE`), the generated `adapter-dn2.inc` and the sample pool's `sample-pool-dn2.inc`, `sample-pool-dsp-dn2.inc`.
- `tests/support/dn2_stereo_rig.py` (stock image plus the stereo object, patch overlays, synthetic engine memory), `tests/support/dn2_stereo_stages.py` (the stage table and the harness), `tests/support/machine_rig.py`.

Model tests:

1. `tests/patchkit/test_dsp_stereo_gain_slot.py`: the stock block `0x1c99de..0x1c9b33` with both hooks against the stock block on L and on R: mono slots bit-identical, stereo slots A = stock(L).A and B = stock(R).B, with block ramp and fade, loop counts, flags, live-out registers.
2. `tests/patchkit/test_dsp_stereo_twins.py`: each twin against two isolated mono channels over 3 to 9 blocks with changing controls, callee-saved registers, unstamped slot is the stock call; negative controls (shared history, controls copied after the L call, no scaling on R for the late shaper); the R ring is its own, at the profile base; the dirty-ring decision with its negative control.
3. `tests/patchkit/test_dsp_stereo_identity.py`: with the same audio in L and R, R equals L bit for bit through every twin over seven blocks with changing controls and a reset in the middle of the note (the reset wrappers); negative control: the stock reset without the mirror breaks it.
4. `tests/patchkit/test_dsp_stereo_lifecycle.py`: the reset mirrors against the real stock resets (L is the stock reset, R's private words are set, no other slot is touched), a pointer that is no slot, the guard (one refusal counted per stage, `_gs_health`), a mono slot goes straight to the stock stage.
5. `tests/patchkit/test_dsp_voice_stereo_dn2.py`: the adapter on the machine rig: stamp, latch, R scaling, bad R, cap, counters and health word, note-on clear of every R record, gap, retrigger, mono unchanged, the proof module cannot be installed with it.

## 6. Phase 2 as built

Approved by Em on 2026-11-02 after the static analysis of the default sound (section 1.6) and the comparison with the stock DT2 engine (the twin model confirmed; edits: the budget in cycles in section 3, R is never reset on its own in section 2.4, the identity test in section 5, the envelope check in section 1.6).

- Counters, latch and health word first (section 2.1), then the twins of the shaper and the held sample, of cases 1 to 3 and of the ring stage with its R rings (sections 2.2 and 2.5), the seven mid-note reset mirrors (section 2.4), and the guard: the old `_gs_bad_*` wrappers and their macro are gone; a stage no longer clears the stamp because it has no twin, only because the context is unsupported.
- Not done: raising the cap of 8 (needs the A/B), the health word in the dsp-link record, the note-on reset sites of the stages beyond the first-order and U/Y stage, a finding for the routing bytes (the stored defaults need a peek on the device or the emulator: 16 bytes each at `0x254aa4`, `0x254ab4`, `0x254ac4` on the default track).
- Optional: a patch of the pair gate / balance mapping if the DN2 pan differs from gate-1 on DT2.

## 7. Decisions of Em

2026-11-01:

1. The twins live in the `dsp-voice` DN2 object set (one adapter, one claim owner); `dsp-stereo-proof` stays a private observer.
2. Cap of 8 stereo slots at first, lifted after the A/B.
3. Steal edge resolved statically before the reset code (section 1.2).
4. Per-slot gain hook at `0x1c9af6` (section 1.1).
5. Static cycle count first (section 3), one A/B later.

2026-11-02: a stereo sample must play stereo, so every remaining stage between the machine and the pan is twinned (replaces "R = L whenever a non-twinned stage is active"); the fallback stays only as a guard, counted per stage and visible in the health word; 16 fixed R rings carved from the top of the sample pool range; the seven mid-note reset mirrors.

## 8. What the DN2 emulator test must check after integration

The model has the stock image's engine bytes but not the real engine state, the allocator or the audio path.

- The patched image boots and plays a stock DN2 voice with the hooks installed: the audio is bit-identical to the unpatched image (golden hash), 16 voices included. The sample pool still works with its 512 KiB shorter range (`SP_DDR_PAGES` 65408).
- A stereo machine on a track (`tests/fixtures/dsp/stereo-machine.s`) on the default track: tone A in L and tone B in R, the other tone at least 20 dB down in each channel; `_gs_nbad` = 0, `_gs_cnt` words 0 to 12, 16 and 17 = 0, bit 0 of `_gs_health` = 0, `_gs_seen` equal to the heartbeat for the slot.
- The same with the pan at the extremes and the centre: L != R in the master and send planes; hard-left: plane B carries the R tone only.
- Two voices on different slots with different R tones; retrigger one slot and steal the other (a 17th note): each slot's R history restarts at its note-on block, the other slot's output is unchanged (section 1.2 on the real allocator).
- **Measure the split before the stock Delay echo [D].** The default track returns its signal through the stock Delay: first echo about 1.09 s after the key press (default TIME 64), both tones in both channels at about -18 dB, then a tail that outlives the voice (run 4: a window at 1.0 to 1.2 s read 0.0077 of the other tone; the same note read 0.0023 before 1.0 s, with no knob turned). Split windows end before press + 0.95 s; rows measured later read only the counters.
- Filter types of FLTR page 1 (dispatch cases 1 to 6) and the OVR and SRR amounts and routings (0 and 1), where the panel encoders are known: the split stays at least 20 dB and `_gs_nbad` stays 0; a ring type (case 4 or 6) for a few seconds, past the first wrap of the ring (8192 samples of delay line, 256 blocks), and a mid-note filter change (a reset mirror).
- The fade: end a stereo voice (release) and check that R fades with L in the last block (no click). The block ramp: a level change on a held stereo note moves R and L together.
- The busy-cycle ceiling with 16 sounding voices, and one paired A/B (`tools/devtools/coupled_ab.py`) with 8 stereo voices on the default sound and with case 3: read the busy-cycle delta; compare with section 3 (about 23k and 44k instructions per block).
- The pan hook on the real core: all 16 slots are processed with the hook (loop count), no sequencer stop.
- Hardware only after all of the above.
