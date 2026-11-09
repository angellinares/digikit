# 102. RECIPS and RSQRTS on a Digitakt II: the seed tables, the exponent rules and the flags

Trigger: the core modelled the seed of `RECIPS` and `RSQRTS` with an approximation (`approx_recips`: the reciprocal rounded to float32, low mantissa bits cleared). On the probe of phase 1 (finding 101, `docs/plans/debug-mode-and-probes.md`) 2034 of 4624 `RECIPS` and 1901 of 2070 `RSQRTS` results of the emulator differed from the device. This finding has the measured rules, the tables and the check. Marks: **[D]** measured or read once, **[O]** open.

## 1. The measurement **[D]**

2026-10-31, real Digitakt II, OS 1.17, image id `83dbe826ff7f81b8`, probe module `test/sharc-probe` (`tests/fixtures/modules/sharc-probe`), collector `digikit/probe`. 6694 items (4624 `RECIPS`, 2070 `RSQRTS`): every table index with three low-mantissa patterns, boundary exponents, both signs, zero, a denormal, infinity and NaN. Before each item ASTAT bits 0-13 and all of STKY were cleared; MODE1 was the one the fx adapter sets (RND32 set, TRUNCATE, ALUSAT and PEYEN clear). The capture is complete: one run, no sequence or checksum errors, no conflicts between runs.

The device rows are stored in `tests/fixtures/sharc/recips-rsqrts-dt2-1.17.json` (op, input, result, ASTAT[13:0], STKY[9:0]).

## 2. The rules **[D]**

The model reproduces 6694 of 6694 items: the result word, ASTAT bits 0-13 and STKY bits 0-9 (`uv run python -m tools.sharc_dev.recip_tables verify out/probe/dt2-device.json`). No item needed a rule beyond the two tables and the rules below.

**RECIPS** (`F = RECIPS Fx`, e the unbiased exponent, m the mantissa):

- Result mantissa: `RECIPS_SEED[m >> 16]` as the top 8 mantissa bits (the 7 MSBs of the mantissa index the table, as the manual says). The low 15 bits are zero. All 128 indices were measured and no index gave two values.
- Result exponent: biased `253 - (e + 127)`, that is `-e - 1` as the manual says. Result sign: the sign of the input.
- Biased exponent 253 or more (including infinity): result is +-zero. Zero (and a denormal input, flushed to zero) gives +-infinity. NaN gives `0xFFFFFFFF`.
- ASTAT: AF is set on every item (the manual's flag table does not list it); AC and AS are cleared. AZ for the +-zero result, AV for a zero input, AI for a NaN input, AN for the sign of a non-NaN input. **[C]** AN stays clear for a negative NaN, although the manual says "set if the input operand is negative".

**RSQRTS** (`F = RSQRTS Fx`):

- Result mantissa: `RSQRTS_SEED[(biased exponent LSB << 6) | (m >> 17)]` as the top 8 mantissa bits. Only a 4-bit seed is documented; the table has 8 bits. The low 15 bits are zero.
- Result exponent: unbiased `-1 - (e >> 1)` (arithmetic shift). The manual's formula has a lost operator. Result sign: positive (a negative nonzero input gives all ones).
- +infinity gives +zero (AZ). +-zero gives +-infinity (AV, and AN for -zero). NaN and every negative nonzero input (including -infinity) give `0xFFFFFFFF` (AI).
- AF is set on every item; AC and AS are cleared.

**Sticky bits.** STKY bits 0 (AUS), 1 (AVS) and 5 (AIS) latch: AVS with AV, AIS with AI. AUS latches only when a finite `RECIPS` input underflows (exponent above +125); not for +infinity and not for the +zero result of `RSQRTS`. **[D]** The other float ALU operations latch AVS, AIS and (add, subtract, FIX) AUS the same way, and the fixed-point adder operations latch AOS (finding 103).

STKY bits 24 and 26 (`0x05000000`) are set on every item. They are not an effect of the instruction: bit 24 is SSEM (status stack empty) and bit 26 is LSEM (loop stack empty) (PRM, status stack and loop resources). The check masks STKY to bits 0-9 and ASTAT to bits 0-13.

## 3. The change

- `tools/sharc_dev/sharc_core/seed_tables.py`: the two 128-entry tables, written by `tools/sharc_dev/recip_tables.py fit PROBE.json --write`. The tool also verifies the core against a probe JSON or the fixture (`verify`) and writes the fixture (`fixture`).
- `compute_alu._alu_recip_seed_impl`: the rules above; `compute._apply_sticky` latches the three sticky bits. The operation names are literals because the translator erases a string built at run time (the first regeneration lost every sticky bit this way: the latch compares the name).
- `approx_recips` is removed from the core, the translator tables, the native run configuration (option 11 is unused), the native runtime default, the tools and the tests. The fast-region lowering of `RECIPS` is replaced by an exact one for `RECIPS` and `RSQRTS` (section 7).
- Test: `tests/sharc/semantics/test_sharc_recips.py` runs the core over all 6694 fixture rows. The emulator check is in `tests/regression/runtime/sharc-probe.test.ts` (section 4).

## 4. The emulator **[D]**

The regression check is the third test of `tests/regression/runtime/sharc-probe.test.ts` ("RECIPS and RSQRTS give the measured result and flags for every probe item", Vitest project `runtime`, both devices). It installs the probe, lets the collector fill its ring (one run and a margin, about 70 to 90 s per device), places every frame in its run as `dkprobe.build_table` does, and compares the result word, ASTAT[13:0] and STKY[9:0] of all 6694 items with the fixture.

| Daemon | Result |
| --- | --- |
| DT2 1.17 (core regenerated) | 6694 of 6694 items equal, 86 s |
| DN2 1.12 (core regenerated) | 6694 of 6694 items equal (against the DT2 rows), 71 s |

The first regeneration failed this check on 1567 items (STKY only): the operation name of the underflow case was built with `"..." + name` and the translator erased it, so no sticky bit latched in the generated core while the Python core passed. The check found it; the names are literals now (section 3).

Regeneration (recipes in `out/native/{dt2,dn2}-audio/gen/PROVENANCE.txt`, work directories copied from the last generation, backups of the previous gens as `gen-pre-recips-20261031`): DT2 57 s (86 s the first time), DN2 107 s (118 s), shared core `mise run gen:core` 6 s. Daemon builds from the new gens: DT2 96 s, DN2 493 s (the core crate dominates). The generated cores of the first regeneration are not kept.

## 5. What else changed **[D]**

Stock firmware uses `RECIPS` in its division helper (DN2 0x1c06ba: a seed and three Newton iterations), so every scenario whose audio divides gets another last bit. One scenario compared sample by sample (`trig1-tap-release`, 816000 values): 18038 samples differ, the largest difference is 2.2e-8 on a peak of 0.056 (4e-7 of the peak). The divided result is within one float32 ulp with either seed; only the rounding moves.

- `tools/sharc_dev/sharc_core` golden hashes (`tests/sharc/integration/test_sharc_golden.py`): six of six unchanged.
- DT2 scenarios (`tests/scenarios/dt2.json`, 5): all PCM hashes unchanged (the DT2 stock scenarios run no DSP).
- DN2 scenarios (`tests/scenarios/dn2.json`, 10): the four pins `exactness-100m`, `trig1-noqa-40m`, `trig1-taps` and `trig1-tap-release` changed (the runs report `ok=False`); the unpinned `trig1-hold`, `kb-chord16` and `dn2-rec-play` changed; `idle`, `dn2-rec-noplay` and `dn2-led-sweep` are unchanged.
- DN2 FX scenarios (`tests/scenarios/dn2-fx.json`, 17): all 17 hashes changed. They have no pins; the 61 FX oracles of the quick tier (`mise run test:fx`) pass.

Em approved the four new values on 2026-11-01; `tests/scenarios/dn2.json` holds them:

| Pin (`tests/scenarios/dn2.json`) | Pinned | With the exact seeds |
| --- | --- | --- |
| `exactness-100m` | `826db0d1de8f7738ac5d0c4579198439bb5a0102b4b9d8bc36d93fa07ce15843` | `e84a9cdc9b8d433fbce500508df74ce27f48d32a3bbf64b09475fa0e6316b1b2` |
| `trig1-noqa-40m` | `d56d8f4bce4e5435a7f0c2ec16a48b1edfcba148356b574d3458c6ccab73f1c9` | `4460493f74f70f0500c01e168d914987c7409d74ec18c24d8281a25bcbb28e2b` |
| `trig1-taps` | `8b26c70cee443b75b90ad6d24f0f5e9e25178e9062e0237f4b839d08a8d3ae9b` | `6cadae1c8e08fefbf31bb4d501a6f1bef064f3e1894985379507a830562b17d9` |
| `trig1-tap-release` | `f4f6d19fe0b270e83a0c537be31b055f0101755a26a444ece322ec340ba6a507` | `2ec38cc9a7c2edb6096c2fbf1129fd53d985f2d7126fc20a4bdb605102779291` |


## 6. Open

- DN2 **[D]**: measured on a real Digitone II (OS 1.12, image `44c2d5d6d0341420`, 2026-11-01, `out/probe/dn2-device.json`): all 6694 results, ASTAT[13:0] and STKY[9:0] equal the Digitakt II rows; only ASTAT bits above 13 differ (`0x75000000` on DN2, `0x3f040000` on DT2), which is the state the surrounding firmware leaves, not an effect of the instruction. The DT2 fixture therefore holds for both devices.
- `RECIPS` and `RSQRTS` under other MODE1 settings (TRUNCATE, ALUSAT, RND32 clear) and in 40-bit mode are not measured; the core uses the same result for all. **[O]**

## 7. The fast tier **[D]**

The fast tier (`packages/emulator/sharc/src/fast/lower/float_alu.rs`) lowers `RECIPS` (opcode `0xc4`) and `RSQRTS` (`0xc5`) for the ordinary inputs and leaves the kernel before the instruction for every other input, as the other float operations do:

- `RECIPS`: exponent field 1 to 252. `RSQRTS`: positive input with exponent field 1 to 254. Zero, denormal, infinity, NaN, a negative `RSQRTS` input and the `RECIPS` underflow range go to the interpreter, which sets AV, AI and the sticky bits.
- In range the result is nonzero, so the flags are those of any float ALU result (`FlagKind::Falu`: AF set, AZ and AN from the result, the rest clear) and no sticky bit latches.
- The seed is a byte load from a 128-entry table at a host address held by the kernel. `packages/emulator/sharc/build.rs` writes both tables to `OUT_DIR` from `tools/sharc_dev/sharc_core/seed_tables.py`; there is no second copy. The wasm backend refuses a kernel with such an address (`Op::CI64`), as it does for a helper record above 4 GiB.

Before this change a kernel with either operation was refused. On stock DN2 (`trig1-hold`, `kb-chord16`), stock DT2 (`dt2-led-sweep`) and the patched DN2 and DT2 replays no kernel contained one: no refusal named opcode `0xc4` or `0xc5`, and the instruction counts of each tier (interpreter, blocks, fast tier) and the PCM and DSP state hashes are equal before and after (the stock division helper at `0x1c06ba` starts with a MODE1 bit test, which the fast tier does not run, and the generated blocks run the rest).

Exactness: `fast_diff forms` (random states, each result compared with the interpreter step: registers, flags, stacks, memory), `--filter recips` and `--filter rsqrts`, both backends, 20000 trials per form (150000 for `rsqrts`): 0 mismatches. On the five replays the PCM hash and the DSP state dump are byte-equal with the fast tier on and with `SHARC_FAST_REGIONS`, `SHARC_FAST_FALLBACK_REGIONS`, `SHARC_FAST_AUTO` empty and `SHARC_FAST_CHASE=0`. The third test of `tests/regression/runtime/sharc-probe.test.ts` passes on both devices with the fast tier on, but its probe code did not reach a kernel with either operation (the fast tier summary refuses the same number of entries as before), so `fast_diff` is the evidence for the lowering.
