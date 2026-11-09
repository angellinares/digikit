# 103. SHARC+ instruction facts of a Digitakt II: float ALU flags, ALUSAT, sticky bits, register values and cycles

Trigger: the `test/dsp-facts` probe (`tests/fixtures/modules/dsp-facts`, items defined in `tools/device/dkprobe_facts.py`) ran on a real Digitakt II. 76 value or register items and 39 cycle items differed from the emulator. This finding has the classification, the rules that now hold in the core, the register values read on silicon and the cycle table for later cycle-model work. Marks: **[D]** measured or read once, **[C]** corrects an earlier claim or the manual, **[O]** open.

## 1. The measurement **[D]**

2026-10-31, real Digitakt II, OS 1.17. Capture `out/probe/facts-dt2-device.json`: 143 items (59 value, 37 register, 39 cycle and 8 time items), 48 runs, complete, no sequence or checksum errors. Emulator captures: `facts-dt2-emulator.json` (before this work), `facts-dt2-emulator2.json` and `facts-dn2-emulator2.json` (after).

A value item runs one instruction on given operands with ASTAT bits 0-13 and all of STKY cleared before it, and reads the result, ASTAT and STKY after it. MODE1 was the one the fx adapter sets (RND32 set; TRUNCATE, ALUSAT and PEYEN clear), plus ALUSAT for the `*_sat` items. The device rows of the 59 value items are in `tests/fixtures/sharc/dsp-facts-values-dt2-1.17.json` (item index, op, operands, result, ASTAT[13:0], STKY[9:0]).

STKY bits 24 (SSEM, status stack empty) and 26 (LSEM, loop stack empty) are set on every item. They are not an effect of the instruction, so every comparison masks STKY to bits 0-9 and ASTAT to bits 0-13 (as finding 102 does).

## 2. Classification of the differing items

| Class | Items | What |
| --- | --- | --- |
| (a) instruction semantics | 44 of 59 value items | result, ASTAT[13:0] or STKY[9:0] differ: section 3 |
| (c) not an instruction effect | the other 15 value items | they differ only in STKY bits 24 and 26: the emulator clears them when STKY is written, the device keeps them (status of the empty stacks); not changed |
| (b) register reads | 17 of 37 register items | the emulator returns plain memory or a guess: section 4 |

No other class exists: all 143 items are covered.

## 3. The rules **[D]**

The core reproduces 59 of 59 value items (`tests/sharc/semantics/test_sharc_dsp_facts.py`, tool `tools/sharc_dev/dsp_facts_values.py`). The emulator reproduces them on both devices (section 5). Device against emulator before the change:

| Rule | Items (device, emulator before) |
| --- | --- |
| Float ALU invalid result: an operation with a NaN input gives `0xFFFFFFFF`, AI set, **AN clear**. [C] the manual says AN is set for a negative result, and the all-ones word is negative. | `fadd` NaN + 1, `fmax`/`fmin` with a NaN, `fabs` NaN, `fcomp` with a NaN, FIX of a NaN: ASTAT 0x420, emulator 0x424. |
| Infinity minus infinity gives `0xFFFFFFFF` with AI (the old core kept the host NaN `0x7FC00000`). | `fadd` +inf + -inf: device `0xFFFFFFFF`, emulator `0x7FC00000`. |
| Sticky AIS latches with AI and AVS with AV for every float ALU operation. | all items above with AI or AV: STKY 0x20 (AIS), 0x22 for FIX of an infinity or overflow without ALUSAT (AIS and AVS), 0x2 for FIX with ALUSAT. |
| Sticky AUS latches when a float add or subtract returns a flushed denormal (a zero from a nonzero exact result). Not for a denormal input that is flushed (`fadd` denormal + 0, `fmax`, `fabs` of a denormal give STKY 0). | `fadd` 0x00800000 + 0x80800001: result -0, STKY 0x1. |
| FIX has an integer result: AZ means all zeros and AN is the sign bit (the float rule made 0x80000000 a zero). With ALUSAT, -infinity or a negative overflow returns `0x80000000` with AV and AN, AZ clear, AI clear. Without ALUSAT an overflow or infinity gives `0xFFFFFFFF` with AI and AV and **AN clear**. | `fix_sat` -inf: device 0x406, emulator 0x407; `fix` +-inf, +-2^32: device 0x422, emulator 0x426. |
| FIX of +-0.5 returns 0 and latches AUS (STKY 0x1); 1.5 gives 2 without AUS. | `fix` 0x3f000000, 0xbf000000. |
| MIN(+0, +0) is +0 (a denormal operand is a zero of its sign). Two zeros give -0 if either is negative for MIN, +0 if either is positive for MAX (the manual's -0 for MIN(+0, -0) and +0 for MAX(+0, -0) stay). | `fmin` 0x1, 0x0: device `0x00000000`, emulator `0x80000000`. |
| Fixed-point add, subtract, negate and abs set the sticky AOS (STKY bit 2, 0x4) with AV, with or without ALUSAT. | `add`, `sub`, `neg`, `abs` of the overflow cases: STKY 0x4. |
| With MODE1.ALUSAT set the same operations saturate: a positive overflow returns `0x7FFFFFFF`, a negative one `0x80000000`. AN and AZ describe the returned word; AC and AV stay those of the adder. | `add_sat` 0x7fffffff + 1: `0x7FFFFFFF`, ASTAT 0x2 (emulator wrapped, 0x6). `add_sat` 0x80000000 + 0x80000000: `0x80000000`, 0xe. `sub_sat`, `neg_sat`, `abs_sat` (0x12: AV and AS) the same way. |
| The float multiplier's invalid result (a NaN operand, infinity times zero) is `0x7FFFFFFF` with MI set and **MN clear** (not all ones). Measured with positive operands only. | `fmul` +inf * 0, NaN * 1: ASTAT 0x200, STKY 0x200; emulator `0x7FC00000` and `0xFFFFFFFF` with MN. |
| Reading an MR word into a register leaves the multiplier flags. [C] PRM p.493 says MU, MN, MI and MV are cleared for every direction. | `MRF = R1 * R2 (SSFR); R0 = MR1F;` with -1.0 * -1.0: ASTAT 0x80 (MV), emulator 0. |

Operation names: the sticky latch compares the operation name of a compute. The underflow cases have their own literal names (`float-add-underflow`, `float-subtract-underflow`, `fix-underflow`, `fix-scaled-underflow`) and `float-min`/`float-max` are literals now; the translator erases a name built at run time (the lesson of finding 102).

## 4. Registers read on silicon **[D]**

Two reads of each register gave the same value (the registers are stable while the probe runs). Emulator values are from the capture before the change; "same" means equal on the DT2 and DN2 emulators.

| Register | Device | Emulator | Note |
| --- | --- | --- | --- |
| IMASK | `0x8040801b` | `0x80408018` | bits 0 and 1 are set on the device |
| IMASKP | `0x00000000` | same | |
| IRPTL | `0x07a00800` | `0x00000800` | pending bits change with timing; not worth modelling |
| LIRPTL | `0x8040801b` | `0x80408018` | equals IMASK on both |
| MODE2 | `0x42000021` | `0x00000021` | the device has bits 25 and 30 set |
| MMASK | `0x00000000` | same | |
| TPERIOD, TCOUNT | `0x0025f280`, `0x2009ce58` | same on the DT2 emulator | the timer does not run in the probe (TCOUNT is the same in both reads) |
| SHDBG_REVID (0x30026) | `0x00000020` | `0x00000021` | |
| SHDBG_CORE_ID (0x300ED) | `0x00000000` | same | |
| SHDBG_OSPID (0x30023) | `0x00224601` | the TPERIOD value | the emulator aliases the address to the timer period |
| CMMR_SYSCTL (0x30024) | `0x00006004` | `0x00016004` | bit 16 is set in the emulator |
| SHL1C_CFG, SHL1C_CFG2 | `0x00010101`, `0x000000f0` | same | |
| SHL1C_RANGE_START/END 0 and 1 | all `0x00000000` | garbage words (plain memory) | |
| SHL1C_RANGE 2 | start `0x200fa000`, end `0x200fe000` | `0x00000028`, `0x00000000` | |
| SHL1C_RANGE 3 | start `0x28240000`, end `0x283a0000` | start `0x3a000000`, end same | |
| SHL1C_RANGE 4 to 7 | all `0x00000000` | garbage for 4 and 5, 0 for 6 and 7 | |
| CMMR_PFB_NOCHRT0_START, _END, CMMR_GPERR_STAT, CMMR_PWR_L1_SD_CTL, _GLB_CTL, _L1_LS_CTL, PFM_CFG | all `0x00000000` | same | |

What matters for the model: the non-cacheable ranges (range registers 2 and 3, each from start up to but not including end: `0x200fa000`-`0x200fe000` and `0x28240000`-`0x283a0000`; SHL1C_CFG2 `0xf0` sets RR2EN, RR2SEL, RR3EN and RR3SEL, and RRnSEL = 1 means non-cacheable, SHARC+ Core Programming Reference, Table 32-3; L2 and DDR outside them are cached), the ID registers (REVID `0x20`, OSPID `0x00224601`), IMASK and MODE2. A register read costs 2 to 5 cycles, IMASK and IRPTL 27, TPERIOD 22, CMMR_PWR_L1_SD_CTL 28 (the emulator: 2 for all).

## 5. The emulator against the device **[D]**

`mise run dk:probe -- --spawn dt2|dn2 --modules test/dsp-facts --capture out/probe/facts-DEV-emulator2.json --compare out/probe/facts-dt2-device.json` with the regenerated core: 59 of 59 value items equal the device (masked as above) on the DT2 emulator and on the DN2 emulator (against the DT2 rows, which section 10 shows hold on a Digitone II too). The probe took 114 s (DT2) and 66 s (DN2).

The regression check is the fourth test of `tests/regression/runtime/sharc-probe.test.ts` ("the instruction facts of the Digitakt II hold for every value item of the dsp-facts probe", Vitest project `runtime`, both devices, about 27 s each). It runs three runs of the facts stream and compares result, ASTAT[13:0] and STKY[9:0] of the 59 items with the fixture. With the daemons built before this change it fails with 44 of 59 items differing (the class (a) items), so the check sees the change.

## 6. The change

- `sharc_core`: `floats._float_binary`/`_float_unary` return all ones with AI for a NaN made by the operation; `_float_multiply` returns `0x7FFFFFFF`; `_float_min`/`_float_max` zero signs; `flags._float_alu_updates` (AN clear with AI, `integer_result`), `flags._saturate_alu_result`; `compute_alu` (saturation in add, subtract, carry forms, increment, decrement, negate, abs; underflow operation names; FIX and TRUNC integer flags); `compute._apply_sticky` (AVS, AIS, AUS, AOS tables); `compute_mult._mr_data_move` (reads keep the flags). CORE_FINGERPRINT `9f38fe79...`.
- Rust runtime: `sharc-rt/src/rt/bnd.rs` (the slow path of the float add and subtract fast path and of the multiply, and their reference implementations in the unit tests).
- Fast tier (`packages/emulator/sharc/src/fast/lower`): it leaves the kernel before an instruction when the interpreter would latch a sticky bit or saturate: a fixed-point add, subtract, increment, decrement, negate or abs that overflows; a float add or subtract with a denormal result; a FIX that returns zero from a nonzero value. `fmin`/`fmax` use the new zero signs. `fast_diff` knows the new exits (`Check::IAdd` and the others).
- Regeneration (work directories copied from `out/recips`, the recipes of finding 102): shared core 5 s, DT2 57 s (transpile and rsgen), DN2 105 s. New gens in `out/dspfacts/{dt2,dn2}-gen`, not promoted; `out/native/{dt2,dn2}-audio/gen` is unchanged and backed up as `gen-pre-dspfacts-20261031`. Daemon builds: DT2 97 s, DN2 386 s. The first generation (before a ruff format and a variable rename) gave the same code except that rename.

## 7. What else changed **[D]**

Against the state after finding 102 (the RECIPS and RSQRTS core):

- `tools/sharc_dev/sharc_core` golden hashes (`tests/sharc/integration/test_sharc_golden.py`): six of six unchanged.
- DT2 scenarios (`tests/scenarios/dt2.json`, 5): all PCM hashes unchanged.
- DN2 scenarios (`tests/scenarios/dn2.json`, 10): all PCM hashes unchanged, including the four pins that finding 102 left changed (`exactness-100m`, `trig1-noqa-40m`, `trig1-taps`, `trig1-tap-release`; they still report `ok=False`). No new pin change; no pin was updated.
- DN2 FX scenarios (`tests/scenarios/dn2-fx.json`, 17): all 17 hashes unchanged.
- DN2 FX oracles of the quick tier (`mise run test:fx`, 61 assertions) pass on the regenerated core (SHARC_GEN_DIR, the DN2 daemon built from it).

The unchanged hashes show that these scenarios do not reach a changed case (a saturating overflow, a NaN or denormal float result, a FIX that returns zero) in a way that moves the PCM.


## 8. Cycles of the device against the emulator **[D]**

For later cycle-model work. Cycles are net of the cost of the pair of cycle-counter reads (1 cycle). `min` is the smallest of 8 repeats over 48 runs, `cold` the extra cost of the first repeat (cold caches and predictors). The emulator column is the capture before the change (the emulator counts one cycle per instruction plus the jump costs it models).

| Snippet | Units | Device (cycles per unit) | Cold extra | Emulator |
| --- | --- | --- | --- | --- |
| NOP, independent ALU ops, 48-bit encodings | 16 or 64 | 1.0 | 0 to 150 | 1.0 |
| float multiply, independent | 32 | 1.0 | 42 | 1.0 |
| float multiply, dependent chain | 32 | **1.5** | 35 | 1.0 |
| fixed MAC, float multiply with parallel add | 32 | 1.0 | 43, 66 | 1.0 |
| hardware loop of 1 instruction, per pass | 64 or 256 | 1.0, start cost 12 | 0 to 25 | 1.0, start cost 1 |
| hardware loop of 4 and of 16 instructions | 64 passes | 4.19 and 16.19 per pass (slope 16.0, start cost 12) | 18, 41 | 4.02 and 16.02 |
| conditional jump not taken | 32 | 1.0 | 72 | 1.0 |
| conditional jump taken (to the next instruction) | 16 | **6.8** | 43 | 1.0 |
| unconditional jump | 16 | **6.75** | 22 | 1.0 |
| delayed jump with two NOPs | 8 | **6.5** | 22 | 3.0 |
| CALL and RTS pair | 16 | **13.9** | 42 | 2.0 |
| RECIPS alone (seed) | 16 | 1.0 | 24 | 1.0 |
| divide: RECIPS, two Newton steps, multiply | 1 or 4 | **15** | 25 | 8 |
| RSQRTS with three Newton steps | 1 | **31** | 0 | 16 |
| 32 loads from consecutive L2 words | 32 | 1.09 | 110 | 1.0 |
| 32 loads from the same L2 words, warm | 32 | 1.09 | 48 | 1.0 |
| 32 stores to L2, load-store mix | 32 or 16 | 1.09 | 26, 48 | 1.0 |
| 32 loads from L1 (sequential, mixed) | 32 or 16 | 1.0 | 21, 18 | 1.0 |
| 48 loads over 6 cache lines, no conflict | 48 | 1.25 (60 cycles) | 230 | 1.02 |
| 48 loads over 6 lines 16 KiB apart (one set) | 48 | **58.4** (2802 cycles; 46.7 times the line case, about 57 times the emulator's 49) | 26 | 1.02 |
| 32 loads from consecutive DDR words | 32 | 1.0 | 315 | 1.0 |

One block (the cycle counter between consecutive module calls, 768 samples): median **666582** cycles, range 648702 to 683890 (that is 2.7 % below and 2.6 % above), standard deviation 3459 (0.5 %). The emulator: 666633, standard deviation 36. The device block time jitters; the emulator's does not.

The L2 conflict case shows that L2 data is cached in a 16 KiB cache that maps lines 16 KiB apart to one set (the 6 lines do not fit in one set), and that a miss costs about 58 cycles. The conflict, branch, call and divide figures are the ones the emulator does not model.

## 9. Open **[O]**

- AUS: TRUNC (any form), FIX with a nonzero input between 0.5 and 1 (the rule "a nonzero input that returns zero" is read from the +-0.5 pair), average, abs of a sum or difference, scalb and FLOAT BY, and the ALU half of a multifunction compute do not latch AUS, AVS or AIS in the core. The manual's Table 3-3 gives AUS to most of them.
- ALUSAT is applied to the single-function ALU forms. The short compute forms (Type 2c), the multifunction ALU halves and the fixed dual add/subtract do not read MODE1 and still wrap. Their AOS latches with AV through the shared operation names where the name is shared (`add`, `subtract`, `increment`, `decrement`).
- The multiplier's invalid result for a negative operand (the sign may follow the operands: the manual says the sign of NaN x NaN is the OR of the input signs), and the invalid result of the multiplier half of a multifunction compute (`compute_multi._float_multiply_parts` still keeps the host NaN).
- The write direction of an MR data move (register to MR): the flags are still cleared as the manual says.
- ALUSAT on the float ALU beyond FIX and LOGB, and the 40-bit mode (RND32 clear): not measured.
- STKY bits 24 and 26 (SSEM, LSEM) are written as zero by an STKY write in the emulator; they read as the stack status on the device.
- The emulator register reads of section 4 are not changed: REVID, OSPID, SYSCTL bit 16, IMASK bits 0 and 1, MODE2 bits 25 and 30 and the SHL1C ranges.
- Cycle model: nothing in section 8 is modelled yet.
- The fast tier now leaves a kernel on every fixed-point overflow and on a denormal float add or subtract result; the cost of the added guards is not measured (no A/B).

## 10. The same probe on a Digitone II **[D]**

2026-11-01, real Digitone II, OS 1.12, image `44c2d5d6d0341420` (runtime with `digikit/debug`): `out/probe/facts-dn2-device.json`, 143 of 143 items, 48 runs, no sequence or checksum errors, compared with the Digitakt II capture.

- Value items: all 59 equal the Digitakt II (masked as in section 1).
- Registers: equal except three that the firmware sets, not the core: the core timer period `0x00268538` (DT2 `0x0025f280`), the timer count `0x200da318` (DT2 `0x2009ce58`) and OSPID `0x07674700` (DT2 `0x00224601`).
- Cycles: every snippet costs the same as on the Digitakt II (the L2 conflict loads 2769 against 2803 cycles for 4096 loads).
- One block: median 666784 cycles, standard deviation 3730 (DT2: 666582, 3459).

The same capture also proved the step-word transport of [finding 101](101-module-step-word-on-hardware.md) on DN2 silicon, and the health adapter reported DSP 57 % average (61 % peak) and ColdFire 26 % with only `digikit/remote` and `digikit/debug` loaded.
