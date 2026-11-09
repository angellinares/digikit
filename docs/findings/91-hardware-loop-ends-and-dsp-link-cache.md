# 91. Hardware loop ends must be 48-bit; the DSP link writes the data cache back

Trigger: on a real Digitakt II (OS 1.17), `fx/freqshift` on its default send route stopped the SHARC at boot, before the effect call mark (`fx_last` NONE, finding 77). The emulator does not show it. Marks as in AGENTS.md: **[D]** read or measured here, **[O]** open. Nothing here is **[V]**: a second agent must check image bytes. The device ran the fix on 2026-10-30 (section 5).

Manual pages are printed pages of the SHARC+ Core Programming Reference (`sc58x-2158x-prm`, docs/sharc/SOURCES.md).

## 1. The rule **[D]**

"VISA-Related Restrictions on Hardware Loops" (p. 4-42): "The last 11 instruction of a hardware loop must be encoded as legacy Instruction Set Architecture (ISA) instructions." They "may not be encoded as Variable Instruction Set Architecture (VISA) instructions", for two reasons: to take interrupts while the last instructions are fetched, and "to reliably detect the fetch of the last instruction". For a body shorter than 11 "all the instructions in the loop are encoded as ISA instructions (left uncompressed)". The same page says the assembler "automatically identifies the last eight instructions"; the number in the rule is 11, and we use 11.

## 2. The assembler defect **[D]**

Selache (`selas/src/assemble.rs`, the revision in docs/sharc/SOURCES.md) forces 48-bit encoding for the body of `LCNTR = n, DO end UNTIL LCE;` with one slot, `no_compress_label` (set at about lines 573-590, cleared once the instruction at the end label is encoded, about lines 429-442). A nested loop overwrites the slot with the inner end label, and the end of the inner loop clears it. The rest of the outer body, including its last instruction, is compressed.

In `modules/core/adapters/fx/src/dsp/fxsend-a.inc` (the 16 new gains of the send route, a 4x4 nested loop) the outer loop ends in `_fxs_cw: NOP;`, assembled as a 16-bit Type 21c instruction. That is the only violation in the repository: **[D]** 254 DO loops decoded in the DSP objects among 242 `.dko.json` files (committed core adapters, cached module and fixture builds, old copies under `out/`): two hits (plus their old copies under `out/`), the objects built from `fxsend-a.inc` (DT2 `fxsend-dsp`, DN2 `fxsend-dn2-dsp`; 7 and 11 loops). The freqshift object itself has no hit. A draft upstream issue is kept outside the repository.

The loop runs once when the send route is set up. A loop end the sequencer does not detect would run the body past its end, and the stop before the effect mark fits **[O]**: the device must confirm it.

## 3. The fix and the build check

- `fxsend-a.inc` wraps the nested loop in `.NOCOMPRESS;` and `.COMPRESS;` (supported by selas), so every instruction of the outer body is 48-bit. After the rebuild the two objects have no VISA instruction in the last 11 of any loop: 39 loops in the committed objects, 77 with the cached module and fixture objects (old copies under `out/variants` and `out/broken` left out).
- `patchkit` refuses the defect: `build::sharc_legal::loop_end_problems` decodes the objects (Type 12a and 13a `DO`, end = address + `reladdr`) and refuses a VISA instruction in the last 11 instructions of a body, or anywhere in a shorter body. `sharc_obj::verify` runs it for every module build (error text names this finding and the `.NOCOMPRESS` workaround); `dspscan` runs it on the code blocks of an image, so `patchkit preflight` and `patchkit flash` check patched images too. Tests: a counted loop with a VISA instruction at each position, and the nested loop above, refused as written and accepted with `.NOCOMPRESS;`.
- **[D]** The stock DT2 1.16, 1.17 and DN2 1.11, 1.12 images have zero hits (`patchkit isa-scan`; 240 loops judged on DT2 1.17, 286 on DN2 1.12). The rule applies to stock code, and a loop whose end is not at an instruction start of the scanned run is not judged.

## 4. The DSP link cache sequence **[D]**

`core/dsp-link` copies the module code into `_dl_area` (free L2) with DSP stores. The L1 data cache is write allocate and write back by default (Table 8-2, p. 8-2), and the DSP start-up enables it (statically read: `SHL1C_CFG` 0x414141, finding 06, **[O]**). Before this change the hook ran `FLUSH CACHE` (instruction-conflict cache only, p. 4-50) and set `ICAINV` (p. 8-12). Dirty data cache lines were not written back, and instruction fetch reads L2, so the copy could be stale in L2 **[O]**: whether `_dl_area` is cacheable on the device is not known.

SHL1C_CFG bits (Table 33-2): enables ICAEN 0, DMCAEN 8, PMCAEN 16; operations ICAINV 6, DMCAINV 14, DMCAWB 15, PMCAINV 22, PMCAWB 23; sizes, way locks and FISREN. The stock start-up (DT2 1.17 routine at 0xb89426) writes the INV bits together with the enables: it builds 0x414141 and writes `read-back | 0x414141` (it never writes a WB bit). The manual does not say whether the operation bits clear themselves, whether they are edge or level triggered, or what a read returns; the emulator stores the written value, which says nothing about silicon. **[O]**

**[C]** The first version of this change wrote `R0 OR 0xC0C000` with R0 the value read back. If the INV bits read back as 1, that write carries ICAINV with the write-back invalidation, which p. 8-11 forbids ("Invalidation and WBI should not be mixed"), and the old `BSET R0 BY 6` for ICAINV changes nothing, so an edge trigger would never fire; the "register as it was" writes would also repeat the invalidation. The earlier text said only that the register is restored, and hid this.

New order, after the last chunk (`dsp-link.inc`): let C be the value read with the operation bits cleared (`R0 AND 0xFF3F3FBF`). `FLUSH CACHE`; write `C | 0xC0C000` (write-back invalidation of the DM and PM caches together: "the WB and Invalidation bits to be simultaneously set", p. 8-11; the core is stalled until it is done, so there is no poll); write C; write `C | 0x40` (ICAINV alone) and keep it for the 40-iteration wait; write C. The 12 instructions after every `SHL1C_CFG` write are 48-bit (`.NOCOMPRESS`): "Twelve instructions after any cache MMR access should be unrelated and uncompressed (48-bit)" (p. 8-9); before, they were 16-bit NOPs. Each operation is a 0-to-1 change of its own bits, so it works for an edge or a level trigger, and for bits that clear themselves or not. The register ends as C, which differs from the stock 0x414141 only in the operation bits. The two extra instructions cost nothing measurable (the smoke cells show +70 and +68 busy instructions per block, the same as before).

The full write-back invalidation is used because the range form needs the `CAINV` range registers and a loop that must run from L1 (p. 8-12, footnote on p. 8-9), and it runs once per module load. Its cost depends on the dirty lines (p. 8-12, "depends on number of dirty lines"); with 16 KB of D-cache it is small against a load. **[O]** The manual asks that cache configuration is not accessed "when executing from external memory"; the hook runs from L2, as before.

The emulator has no cache model: `FLUSH CACHE` and the `SHL1C_CFG` write are accepted and do nothing (finding 31), so the new sequence is a no-op there. No `sharc_core` change.

## 5. Open

- **[D]** Device retry (Em's DT2, OS 1.17, 2026-10-30, `device` set from this branch): freqshift boots and runs on the send route and on the master route, with no stop; its page shows under [FUNC]+[FX].
- **[D]** Selache fix (ElekLabs fork, branch `fix/nested-do-loop-end`, commit 926bfac9): a nested `DO` no longer ends the outer loop's 48-bit mode early. We pin the fork in `packages/patchkit/Cargo.toml` and `tools/patchkit/pk_toolchains.py`. The `.NOCOMPRESS`/`.COMPRESS` workaround is removed from `fxsend-a.inc`; the rebuilt objects of all `core/*` modules have the same code, symbols and relocations as before (only `selache_rev` and, for the two fxsend objects, `source_sha256` differ). Not yet filed with the original project.
- **[D]** Three more Selache encoding fixes in the fork branch `fix/encodings` (on top of 926bfac9): immediate shifts (PRM Table 17-9),
  `(BW)/(BWSE)/(SW)/(SWSE)` on a direct load or store (Type 14d), and the 32-bit `MODIFY(Ii, Mm)` (Type 7b fixed bits). patchkit no longer has
  `hazards`, `sw_prepare`/`sw_apply` or `fix_shift_counts`; the rebuilt objects of all modules have the same code, symbols and relocations as
  before. The compact `ADD` tie of finding 20 is not an encoding bug and stays checked.
- **[O]** Which of the two changes removed the stop: the loop end is the likely one; a build with only the loop-end fix would confirm it.
- **[O]** Edge or level trigger and read-back of the operation bits (the form above does not depend on them), and what the stock write helper at 0x1c0272 does (it is not decoded in the database; it may poll).
- **[O]** `_dl_area` cacheability and the effect of the write-back on a device load (finding 31).
