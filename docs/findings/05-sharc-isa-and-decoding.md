# SHARC+ ISA and decoding

The SHARC+ instruction tables built from the public ADI manuals, the run of Type-NN decode corrections made against the generated Ghidra language, and the selache cross-check.

## SHARC+ instruction tables from the public ADI manuals **[D][O]**

- `tools/sharc_dev/sharcspec/` builds the SHARC+ instruction decode table from two
  public Analog Devices manuals: the bit-layout figures in the SHARC+ Core
  Programming Reference Rev 1.5, read from the PDF's vector drawings, checked
  bit by bit against the classic SHARC Processor Programming Reference Rev
  2.4. The method and the manual errata it found are in
  `tools/sharc_dev/sharcspec/README.md`; the documents are listed in
  `docs/sharc/SOURCES.md`; the results are in `docs/sharc/SPEC-FINDINGS.md`.
  It came from Em's separate sharc-spec work. **[D]**
- On the 1.15C DSP main program (104,848 bytes at `0x28382670`) its decoder
  leaves 1.1% of words unknown. `tools/sharc_dev/sharc_disasm.py` stops at its first
  unknown word, 4.4% in; forced to continue, it cannot name 9.6%. One cause is
  traced: our `15b` entry fixes 3 bits where the manual fixes 7, so 1,928
  words decode as `17b`. **[D]**
- `tools/sharc_dev/sharc_visa_tables.py` now loads `tools/sharc_dev/sharcspec/decode_table.json`
  and picks a form by the same rule as the sharc-spec decoder, and
  `tools/sharc_dev/sharc_disasm.py` still stops at the first word it cannot classify.
  `tools/sharc_dev/sharcldr.py --main` writes the final application's code, and
  `tools/sharc_dev/sharccompare.py` sweeps a region with both decoders. The main
  programs of 1.15C and 1.16 are both 104,848 bytes at `0x28382670`, from
  the same five blocks, with the entry at offset 0. The two decoders agree
  on the length and form of all 22,147 instructions that both decode:

  | | 1.15C | 1.16 |
  |---|---|---|
  | sharc-spec decoder: instructions, unknown words | 22,148, 249 (1.11%) | 22,147, 252 (1.12%) |
  | rebuilt decoder: instructions, unknown words | 22,147, 250 | 22,147, 252 |
  | first unknown word, both decoders | `0xbb0` (2.9% in) | `0xbb0` |
  | old decoder: walk stops at | `0x122c` (4.4% in) | `0x122c` |
  | old decoder: instructions at the spec decoder's offsets | 57.2% | 57.2% |
  | old decoder: instructions it cannot name | 1,962 (9.4%) | 1,950 (9.3%) |
  | old decoder: `17b` where the spec decoder has `15b` | 1,928 | 1,926 |

  The one difference on 1.15C is the last instruction, a `21a` at `0x1998e`
  whose 6 bytes run past the region: the rebuilt decoder refuses it, the
  sharc-spec decoder pads with zeros. After `15b`, the old decoder's largest
  confusions are `5a_move` for `5b_move` (797) and `18a` for `19a` (442).
  `tools/coldfire/ghidra/gen-sharc-slaspec.py` still expects the old form names
  (`8a`, `9a`, `9b`) and fails on the rebuilt table; the SHARC language is to
  be regenerated from `tools/sharc_dev/sharcspec/ghidra/gen_sleigh.py`. A second
  check rebuilt both regions from the boot-stream blocks, counted the mask
  bits, and reproduced the sharc-spec decoder's counts, first unknown and
  last instruction with its own sweep. **[V]** The old decoder's numbers
  come from `tools/sharc_dev/sharccompare.py` alone. **[D]**
- `tools/coldfire/ghidra/install-sharc.sh` now generates the language from
  `decode_table.json` with `tools/sharc_dev/sharcspec/ghidra/gen_sleigh.py` and
  installs `SHARC_VISA:LE:32:default`; the generated module equals the copy
  already installed from the sharc-spec work, except the stack pointer, now
  I7 as the call convention in `docs/sharc/structure-1.16.md` uses. The old
  generator and `tools/coldfire/ghidra/SHARC/` are removed; the installed
  `Processors/SHARC` stays for the `dt2_SHARC` program in `~/ghidra-projects/dt2`.
  The code space has wordsize 2, so byte offset `0x382670` shows as
  short-word address `0x1c1338`. **[D]**
- `tools/sharc_dev/sharc_import.py --seed-calls --analyze` imports section 7 into
  `~/ghidra-projects/elektron-sharc`:

  | | 1.16 | 1.15C |
  |---|---|---|
  | memory blocks (from 104 loader blocks) | 8 | 8 |
  | call targets seeded (of 546 recovered) | 282 | 285 |
  | functions after analysis | 347 | 352 |
  | instructions, all / in the main program | 5,490 / 4,684 | 5,541 / 4,735 |
  | Error bookmarks | 60 | 63 |
  | main-program instructions where our decoder starts one of the same length | 4,663 | 4,707 |
  | ... of a different length | 0 | 0 |
  | ... where our linear sweep starts none | 21 | 28 |

  The first 12 instructions at the entry match our decoder in form and
  length. Ghidra reaches about a fifth of the main program's 22,147
  instructions, so the rest needs more seeds. The Error bookmarks are
  branches into addresses outside the loaded blocks and undecodable words
  in the first application at `0x120xxx`. **[D][O]**
- `docs/sharc/structure-1.16.md` maps the 1.16 DSP program: FreeRTOS tasks, a
  task proposed as the command dispatcher for the ColdFire, and a command block
  at `0x82a00000`. These were found on 1.16 and are hypotheses. **[D][O]**
- Checked against the main programs of 1.16 and 1.15C (short-word address
  SW = `0x1c1338` + file offset / 2), with a second check:
  - A software call is the triple `3c` (raw `0x9ff2`, push R2 through
    I7/M7), `16a` (stores the goto's short-word address minus 1) and
    `25a_direct` (the goto): 45 in each version. A return is `9b_abs` (raw
    `0x083f343f`, indirect jump through I4/M6) then `25c_rframe` (`0x1901`):
    45 in 1.16, 47 in 1.15C. **[V]**
  - The RPC dispatcher task is created at SW `0x1c3f5a`-`0x1c3f6a`,
    byte-identical in both: `17a` ureg2=`0x257800`, `17b` ureg12=1000, `17a`
    ureg8=`0x2577f0`, `17a` ureg4=`0x1c3bf0`, then `25a_direct` to
    `0xb8615d` (1.16) / `0xb86159` (1.15C). ureg8 is the name "RPC
    dispatcher" at loader byte address `0x282577f0`, so data pointers add
    `0x28000000` without doubling, while ureg4, the entry, is a short-word
    address. The call target lies outside every loaded section-7 block. The
    same target creates "Audio Task" at SW `0x1c7775` in 1.16 (entry value
    `0x1c7749`) and at SW `0x1c7708` in 1.15C (`0x1c76dc`). **[V]**
  - Exactly 30 instructions in each main program carry a value in
    `0x82a00000`-`0x82a001ff` (forms `14a`, `14d`, `16a`, `17a`), at the
    same 30 short-word addresses, all in `0x1c361a`-`0x1c48c3`. **[V]**
  - Ghidra has no function at `0x1c3bf0`-`0x1c3c3e` in either program: the
    body calls through the software-call idiom, which the language models as
    plain gotos. The SPORT/DAI setup writes 34 registers at SW
    `0x1cb28e`-`0x1cb31a` in 1.16 and `0x1cb222`-`0x1cb2ae` in 1.15C. **[D]**
  - The two main programs differ in four parts: up to SW `0x1c7501` only
    single words differ; SW `0x1c74fb`-`0x1c7781` (1.16) against
    `0x1c74fb`-`0x1c7715` (1.15C), around the Audio Task creation, has
    several edits that add 108 words; the next 26,733 words are the same
    code, `0x6c` words later in 1.16; the last 18 words of 1.16 and 126 of
    1.15C are different code, so both stay 104,848 bytes. **[V]**
  - Still open: how the dispatcher selects a command (the indirect `9a_abs`
    through I6/M3 at SW `0x1c46c4` is a candidate; the `9a_rel` at
    `0x1c551a` has a fixed target), the Audio Task entry (`0x1c7749` is not
    an instruction boundary in our decode), and how the ColdFire reaches
    `0x82a00000`; the only confirmed link is the DSPI2 frame. **[O]**

## The DSP programs of Digitakt II 1.16 and Digitone II 1.11 in Ghidra **[V][C][O]**

- Digitone II 1.11's section 7 is a boot stream of 95 blocks. Its main
  program starts at SW `0x1c12e2` and is 105,016 bytes (`tools/sharc_dev/sharcldr.py
  --main`). It also loads 54,792 bytes at `0x8045a6c8` and 283,032 bytes at
  `0x80467cf4` in external memory; Digitakt II 1.16 loads 3,316 bytes at
  `0x8045a6c8`. `tools/sharc_dev/sharc_import.py --seed-calls --analyze` imports it as
  `/dn2-1.11_SHARC` in `~/ghidra-projects/elektron-sharc`, with 257
  functions. **[D]**
- A software call is CJUMP followed by its two delay slots, not a push and
  store before a goto. `25a` is CJUMP, a call that "should always use the
  DB modifier" and does `R2=I6, I6=I7` (SHARC+ Core Programming Reference,
  Type 25a, p.416); a delayed branch executes the two instructions after it
  before the target, and a delayed call returns to "the seventh address
  after the branch instruction" (pp.121-122). The classic SHARC Programming
  Reference Rev 2.4 (p.238) gives the compiler's call as `CJUMP (DB);
  DM(I7,M0)=R2; DM(I7,M0)=PC` (the PC store holds the return address - 1).
  In the images, every aligned `25a_direct` is followed by a push of R2
  through I7/M7 and a `16a` through I7/M7 that stores its own short-word
  address + 2, so the return lands after the store: 589 of 589 in Digitakt
  II 1.16 (490 with `3c` raw `0x9ff2`, 99 with a 48-bit `3a`) and 551 of 551
  in Digitone II 1.11 (479 and 72). An indirect call moves I6 to R2 and I7 to
  I6, then jumps through `9b_abs` with M5 (DB) with the same push and store:
  12 sites in each image. **[V]**
- **[C]** The "seventh address" passage counts pipeline instruction slots,
  not short-word addresses. A CJUMP returns to the instruction after its
  second delay slot: call + 7 short words after a 16-bit `3c` push, call + 9
  after a 48-bit `3a` push. See "Tracer decode and call-model corrections"
  below.
- This corrects the call triple above (`3c`, `16a`, `25a_direct`, "stores
  the goto's short-word address minus 1"): the push and store belong to the
  CJUMP before them. The triple looked right because calls often follow each
  other. **[C]**
- A return is the delayed `9b_abs` jump through I4/M6 (raw `0x083f343f`); its
  two delay slots hold `25c_rframe` and one epilogue instruction, mostly `15b`
  then `25c_rframe`: 390 of 391 returns in 1.16 and 276 of 277 in 1.11
  contain `25c_rframe` in the slots. **[D]**
- **[C]** Type 9 indirect branches use DAG2 registers, so this return is
  `JUMP (M14, I12) (DB)`, not I4/M6. The same correction applies to the other
  "I4/M6" mentions in this file. See "Tracer decode and call-model
  corrections" below.
- `tools/sharc_dev/sharcspec/ghidra/gen_sleigh.py` now emits Type25a as `call`. Its
  delay slots are not modelled: they follow the call in the listing.
  `tools/sharc_dev/sharcflow.py` lists the calls, indirect calls and returns with their
  delay slots; with `--cover --analyze --save` it sets FlowOverride.CALL on
  the indirect calls, disassembles every aligned instruction Ghidra has not
  reached, starts a function after each return's delay slots and at each run
  of code no function holds, and re-runs analysis. Both programs were
  re-imported with the new language and the pass applied; copies from before
  any pass are in `~/ghidra-projects/backup-2026-09-15-sharc`. **[D]**

  | | DT2 1.16 | DN2 1.11 |
  |---|---|---|
  | aligned instructions (our decoder) | 21,270 | 20,805 |
  | main-program instructions after import / after the pass | 6,575 / 20,468 | 6,191 / 20,234 |
  | ... inside a function, after the pass | 20,443 | 19,994 |
  | aligned instructions that clash with Ghidra's instructions or data | 36 | 60 |
  | main-program functions after import / after the pass | 234 / 1,875 | 154 / 1,666 |
  | call references after import / after the pass | 287 / 914 | 225 / 866 |

- Function boundaries are still too fine: in 1.16, 168 main-program
  functions have one instruction and 806 have two to five. Two causes are
  seen, and no-return marks are not one (clearing them and recomputing the
  bodies changes nothing). The delay slots after a return become their own
  function, such as `0x1c1498` (`17b` R0=`0xabc`, `25c_rframe`). Code reached
  only through an indirect jump becomes separate functions: the RPC
  dispatcher's first piece `0x1c3bef`-`0x1c3c3e` ends in a `9b` jump at
  `0x1c3c3c`, which the language models as a return, and its cases, such as
  `0x1c3c4f`, `0x1c3d34` and `0x1c3f16`, jump back to `0x1c3c1d`. Both need
  the language: delay slots and indirect jump targets. **[D][O]**

## Delay slots as one Ghidra instruction **[D][O]**

- A throwaway copy of the generated SHARC+ language, installed as
  `SHARC_SPIKE`, models a delayed branch and its two delay-slot instructions
  as one instruction: two identical slot subtables, each a copy of every
  constructor with an empty body, follow the branch pattern, and the body
  builds both before its `call`, `return` or `goto`. It was applied to
  CJUMP, delayed `9a_abs` and `9b_abs` jumps, and delayed `8a_abs` jumps. On
  the 1.16 main program, SW `0x1c80a2` is one 14-byte instruction (CJUMP,
  `3c`, `16a`) with p-code `CALL` to `0x1c7bd4` and fall-through
  `0x1c80a9`; `0x1c2c9a` is 18 bytes (CJUMP, `3a`, `16a`); and the return at
  `0x1c1496` is 10 bytes (`9b_abs`, `17b` R0=`0xabc`, `25c_rframe`) with
  p-code `RETURN`. **[D]**
- SLEIGH limits met: one subtable cannot appear twice in a pattern; a
  subtable must be defined before it is used; bare mnemonic words in
  subtable constructors must be quoted; a field cannot be both displayed and
  constrained in one constructor. `delayslot(n)` counts bytes, so it cannot
  express two SHARC+ instructions of varying length. **[D]**
- Not yet in the generator, and function boundaries were not measured: the
  test program was seeded only from the entry and the CJUMP targets. Its
  `FUN_001c136a` stops at SW `0x1c13bc`, where our decoder and Ghidra both
  read an `8a_rel` jump to a nonsensical target (raw `00 07 3e 02 30 00`),
  with the current language as well. **[O]**

## Conditional jumps, calls and returns in the generated language **[D][O]**

- The generated language lifted every jump, call and return that has a
  `cond` field as unconditional: with cond EQ, an `8a_abs` jump was a `goto`
  with no fall-through. In the 1.16 main program 499 such instructions have a
  cond other than TRUE (`8a_rel` 404, `11a` 37, `8a_abs` 22, `9a_abs` 15,
  `9a_rel` 15, `11c` 5, `9b_rel` 1); in Digitone II 1.11, 478. **[D]**
- `tools/sharc_dev/sharcspec/ghidra/gen_sleigh.py` now gives each of these forms two
  constructors: cond TRUE keeps the old p-code, and any other cond tests
  `condition(cond)`, a user-defined p-code op, and falls through when it
  does not hold. `tools/sharc_dev/sharcpcode.py` measured the old and new language back
  to back, each in a throwaway project (`sharc_import.py --seed-calls
  --analyze`, then `sharcflow.py --cover --analyze`):

| | DT2 1.16 old / new | DN2 1.11 old / new |
|---|---|---|
| main-program functions after the pass | 1,875 / 1,211 | 1,666 / 1,084 |
| ... with one instruction | 168 / 114 | 124 / 94 |
| ... with two to five | 806 / 496 | 691 / 448 |
| functions the decompiler truncates at bad instruction data | 309 / 280 | 346 / 335 |
| Error bookmarks, whole program | 106 / 165 | 113 / 172 |

- The new Error bookmarks are Bad Instruction bookmarks at addresses that had
  none before, outside the main program (Ghidra shows `0012058c`, `001208fa`),
  reached through the new fall-throughs. The decompiler's "Instruction
  overlaps" warning in 1.16 went from 5 to 70. Neither is examined yet. **[O]**
- `FUN_001c136a` now has 55 instructions (38 before) and still stops at the
  `8a_rel` decode at SW `0x1c13bc`. **[O]**

## Where the new Error bookmarks and overlap warnings come from **[D][O]**

- `tools/sharc_dev/sharcpcode.py measure --ghidra` writes `OUT/<image>.sqlite`: our
  decoder at every decodable offset of the main program (form, fields, depth,
  aligned, computed target, the pypcode lift) and Ghidra's instructions,
  references, bookmarks, functions, decompiled C and decompiler warnings. Two
  runs compare with sqlite's `ATTACH`; the queries below are
  `tools/sharc_dev/sharcpcode.sql`. **[D]**
- The conditional-flow change does not create the new Error bookmarks. Ghidra
  reaches more code, and that code meets decode problems the old language
  never walked into. Of the 53 new bookmarks inside the 1.16 main program: 23
  flow into memory the image does not load (17 are CJUMP calls to SW
  `0xb8xxxx`, which `FUN_001c136a` makes as well; 3 are `8a_abs` calls; 3 are
  `8a_rel` jumps whose target comes out negative), 24 are "unable to resolve
  constructor" on the fall-through of a three-word instruction (mostly `21a`,
  `1a`, `2a`), where our own linear sweep stops too, and 6 are branch targets
  inside an existing instruction. Digitone II 1.11 has 52 in the same
  proportions. **[D]**
- 65 of the 70 "overlaps instruction" warnings in 1.16 are `8a_rel` or
  `9a_rel` targets one or two words inside an aligned instruction. By the form
  they land inside: `21a` 62 (54 two words in, 8 one word in), `2a` 14,
  `6a_mem` 11; in 1.11, `21a` 70, `2a` 12, `6a_mem` 8. **[D]**
- Of the 552 aligned `8a_rel` instructions in 1.16 whose target is inside the
  main program, 434 land on an instruction start; 104 of the 118 misses land
  inside one. `25a_direct` lands on a start 314 times of 318. **[D]**
- Open: `21a` decodes as a 48-bit nop matching any first word `0x0000` to
  `0x007f`, 904 of them in 1.16, and the instructions those targets land
  inside carry nonzero later words (`0010 9b52 aa01`, `0069 4dfe 0e3f`). If
  those words are shorter instructions, `21a` swallows the one or two that
  follow, which would explain both the mid-instruction targets and the
  fall-throughs that do not decode. Check Type21a and the `21a`/`22c`
  crossing resolver in `tools/sharc_dev/sharcspec/ghidra/gen_sleigh.py` against the PRM.
  Not checked. **[O]**

## Type21a was swallowing the two words after it **[C][D][O]**

- Corrects the open item above. `Type21a` matched any first word `0x0000` to
  `0x007f` and constrained nothing in the other two words, so it absorbed the
  short instructions that followed: 904 matches in 1.16, only 44 of them the
  all-zero word the PRM draws (1.11: 31 of 898). `tools/sharc_dev/sharcspec/build_table.py`
  now takes every bit from the figure for Type21a and Type21c, and the
  left-over first words are the provisional 16-bit `Type21p_undoc16`
  (`docs/sharc/SPEC-FINDINGS.md` 3.7). **[D]**
- Measured with `tools/sharc_dev/sharcpcode.py measure --ghidra` before and after, both
  images, same machine: **[D]**

| | DT2 1.16 | DN2 1.11 |
|---|---|---|
| `8a_rel` branches landing on an instruction start | 434/552 -> 520/590 | 533/649 -> 625/695 |
| decompiler "overlaps instruction" warnings | 70 -> 46 | 66 -> 48 |
| "call to offcut address" warnings | 13 -> 9 | not recorded |
| aligned instructions our decoder finds | 21,270 -> 21,792 | 20,805 -> 21,361 |
| main-program functions | 1,211 -> 1,170 | 1,084 -> 1,049 |
| ... with one instruction | 114 -> 95 | not recorded |
| functions the decompiler truncates at bad data | 280 -> 296 | 335 -> 343 |
| import and analysis / the sharcflow pass, seconds | 4.2 -> 6.0 / 7.5 -> 10.0 | 4.3 -> 5.9 / 8.2 -> 11.1 |

- Aligned counts in 1.16: `21a` 904 -> 49, `21c` 336 -> 175, `23p_undoc16`
  366 -> 407, and 883 of the new `21p_undoc16`. The branch targets that landed
  two words inside a `21a` are gone; what is left lands inside `2a`, `6a_mem`,
  `14a` and `4a`, in counts of three to ten. **[D]**
- Open: Digitone II's `25a_direct` landing fell, 239/269 -> 217/276, with no
  explanation yet. Both images decode more bytes, so Ghidra meets more bad
  instruction data and analysis takes about 35% longer. 1.16 keeps 165 Error
  bookmarks but 53 of them are different ones; 1.11 goes 172 -> 182. Twelve of
  the twenty worst mid-instruction branch targets are still unexplained: their
  container decodes cleanly and the chain still misses the target, so a
  neighbouring form is wrong too. **[O]**

## Type22a is idle, and neither image contains one **[D][O]**

- The same merge rule over every other form, audited with the new
  `tools/sharc_dev/sharcspec/audit_bits.py`: Type22a (idle/emuidle) and Type26a (`sync`)
  print every bit in their figures but matched on a nine- and sixteen-bit
  prefix, taking 220 and 2 instructions across the two images, while their
  strict words appear in neither. Both are tightened; the words they took are
  now the provisional `Type22p_undoc48` and `Type26p_undoc48`
  (`docs/sharc/SPEC-FINDINGS.md` 3.8). **[D]**
- This buys a correct name, not a better decode: measured before and after on
  both images, every flow metric is identical -- 1.16 keeps 296 functions
  truncated at bad instruction data, 165 Error bookmarks, 1,170 main-program
  functions, the same branch-target landing and the same form counts, and both
  probes keep their earlier verdicts. **[D]**
- A 16-bit reading of the same words was measured and rejected: it split the
  RPC dispatcher at SW `0x1c3c2e`, added 33 truncated functions in 1.16 and
  shifted the alignment sweep enough to change unrelated forms' counts. **[D]**
- Restoring the dropped bits on the compute forms destroys real matches
  (Type2a: every one of its 1,089 instances in 1.16, and 22% of all aligned
  instructions), so the merge rule stands for them. Type3d, Type4d and Type20a
  are neutral. **[D]**
- Open: what the Type22p and Type26p words do. 220 instructions in two images
  are decoded at a length that has never been checked against anything but
  their neighbours' alignment. **[O]**

## Instruction Types 23 and 24, and why the modern manuals skip them **[D]**

- The SHARC+ Core Programming Reference Rev 1.5 and the classic SHARC Processor
  Programming Reference Rev 2.4 both number straight from Type 22 to Type 25
  with no note explaining the gap, so `docs/sharc/SPEC-FINDINGS.md` 3.6 recorded
  Types 23 and 24 as undocumented. Three older ADI manuals, fetched into
  `docs/refs/` on 2026-09-16, close it. **[D]**
- ADSP-2106x SHARC Processor User's Manual Rev 2.1 (March 2004) and ADSP-21065L
  SHARC DSP Technical Reference Rev 2.0 (July 2003) each document **Type 23 =
  `IDLE16`** and **Type 24 = `CJUMP`/`RFRAME`** in full -- syntax, prose and
  opcode bit maps (2106x Appendix A-53 and A-54; 21065L pages 99 and 101). **[D]**
- ADSP-21160 SHARC DSP Instruction Set Reference Rev 2.1 (April 2013) records
  the renumbering that made them vanish. Table 1-21 prints
  `Type 23: Idle16 -- Not supported on ADSP-21160`,
  `Type 24: creg<->ureg -- Not documented on ADSP-21160` and
  `Type 25: Cjump/Rframe  0001 1000 0000 0100 0000 0`
  (`out/refs/ADSP-21160_isr_rev2.1/all.txt` 2390-2401). **[D]**
- So the gap is a renumbering, not an omission. CJUMP/RFRAME moved from Type 24
  to Type 25 and kept its encoding: the classic Type 24 direct-branch word
  `0001 1000 0000 0100` is bit-for-bit the `Type25a_direct` the table already
  carries. Type numbers are ADI's labels, not a field, so the vacated number
  says nothing about encoding space and there is no hole to search. **[D]**
- `IDLE16` is an ordinary 48-bit instruction. The 16 is the clock divisor --
  "the internal clock continues to run at 1/16th the rate of CLKIN"
  (`out/refs/3789835185494138226006565l_book_tr/all.txt` 3826-3830) -- not a
  16-bit encoding, so it is not a candidate for the provisional 16-bit forms.
  Its word is `000 00000 1 01` where Type 22 `idle` is `000 00000 1`, which puts
  it inside the nine-bit prefix `Type22p_undoc48` now covers. Whether any of
  those 220 words is an `IDLE16` has not been checked. **[O]**
- `creg<->ureg` is an instruction ADI acknowledges and declines to document.
  The same manual's glossary defines `creg` as "One of 32 cache entries, an
  entry consisting of a CH, CL, & CA" (`all.txt` 1423), so it moves between an
  instruction-cache entry and a universal register -- systems code, not
  something to expect hundreds of times in an audio program. Its Table 1-21 row
  carries no opcode bits at all. **[O]**

## A third opcode source: Type 7a confirmed, Type 19a's bit 39 settled **[D][C]**

- `tools/sharc_dev/sharcspec/compare_sources.py` diffs two sources, the PRM figures and
  the classic PGR grid. The three manuals added on 2026-09-16 give a third for
  every form the classic core had, which is enough to break the ties the two-way
  diff left open in `docs/sharc/SPEC-FINDINGS.md` 3.2 and 3.3. **[D]**
- Type 7a: the PRM figure shades nine fixed bits, `000001001`, for Type 7a and
  Type 7d alike, which is why the two are indistinguishable there. Both new
  manuals print `000 00100` over bits 47-40 and make bit 39 the `G` field,
  "Selects DAG1 or DAG2" (`out/refs/3789835185494138226006565l_book_tr/all.txt`
  2414-2454; `out/refs/ADSP-21160_isr_rev2.1/all.txt` 3712-3745). Eight fixed
  bits, not nine. **[D]**
- `decode_table.json` already has this right: `Type7a` is `mask
  0xff0000000000`, `fixed_bits 8`, bit 39 unconstrained, because
  `tools/sharc_dev/sharcspec/build_table.py` 273-275 declines to fix a bit the PRM shades
  where the PGR leaves a blank. The third source confirms that merge rule rather
  than correcting it. Type 7a decodes 311 times in 1.16 and 369 in 1.11. **[D]**
- Type 19a: bit 39 is the bit-reverse flag, `0` for `MODIFY` and `1` for
  `BITREV`, printed explicitly as a `0`/`1` pair in both new manuals
  (`out/refs/ADSP-21160_isr_rev2.1/all.txt` 5723-5745;
  `out/refs/3789835185494138226006565l_book_tr/all.txt` 3617-3670). Bits 41-39
  are `100` without bit-reverse and `101` with. That is the PGR value the PRM
  figure contradicts with `000`, so 3.2's open item closes in the PGR's favour.
  **[C][D]**

## Type19a's mask is two bits short, and 757 instructions live in the gap **[V][O]**

- `tools/sharc_dev/sharcfields.py`, new here, is the mirror of
  `tools/sharc_dev/sharcspec/audit_bits.py`: where that lists bits a PRM figure prints
  that the table does not fix, this tallies what values the fields the table
  *declares* actually take across the aligned instructions of both images, and
  flags any field sitting on bits the classic PGR grid fixes. Three fields
  qualify. One is large. **[D]**
- `Type19a` fixes six bits, `000101`, where its neighbours in the opcode space
  fix eight or nine: `Type18a` `00010100`, `Type19a_bitrev` `000101101`,
  `Type20a` `00010111`. The PRM's Figure 17-2 shades only six and declares bits
  41-40 a SHARC+ field `sc[1:0]` and bit 39 a field `w`, while the classic grid
  and all three older manuals fix bits 44-40 at `10110`
  (`tools/sharc_dev/sharcspec/classic.json` "Type 19a", pattern `000101100...`). **[D]**
- So `Type19a` claims the whole `000101` block and, by longest-leading-prefix,
  keeps whatever its tighter neighbours leave: its own `10110` with `w=0`, and
  the whole of `10101`. Of its 1,935 matches across the two images, 1,178 are
  `10110 0`, the documented `MODIFY`; **748 are `10101 1` and 9 are
  `10101 0`**. **[V]**
- Bits 44-40 = `10101` is documented by nothing. The sequence runs Type 18
  `10100` (system register bit manipulation), the gap, Type 19 `10110`, Type 20
  `10111` -- in the PGR Rev 2.4 (`all.txt` 19877-19942), the ADSP-21160 ISR Rev
  2.1 (`all.txt` 5637-5741 and Table 1-19 at 2280-2321), the 21065L and the
  2106x alike. Four manual generations, no instruction at `10101`. **[D]**
- Checked against the bytes by a second agent that used no project decoder: in
  1.16, SW `0x1c14a0` is `87 15 ff ff fc ff`, frame `0x1587fffffffc`, bits 47-39
  `000101011`; in 1.11, SW `0x1c1445` is `87 15 ff ff ee ff`, frame
  `0x1587ffffffee`. A naive scan of every even offset finds 851 frames with
  `10101 1` across the two images, of which the aligned sweep takes 748 -- a
  subset, as it must be. **[V]**
- Read through `Type19a`'s field layout, those two frames give `g=DAG1,
  idis=0, is=I7, data=-4` and `data=-18`: an index register and a small negative
  immediate, the shape of a `MODIFY`. The words are structurally a sibling of
  Type 19, not noise, and they sit in the aligned sweep without disturbing it.
  **[D]**
- Two readings, disagreeing about the table rather than the bytes. Either
  SHARC+ really did widen Type 19 with a two-bit `sc` selector, as its own
  figure says, and `sc=01` is a new variant; or `10101` is a separate
  instruction `Type19a` is swallowing. Against the first: within `sc=01`, bit 39
  is `1` in 748 of 757, so if `w` were still the bit-reverse flag then almost
  every one of them would be a `BITREV`, while the documented bit-reverse form
  occurs 8 times in both images combined. **[O]**
- (`figures.json`'s `Type19a_bitrev` pattern `000101000` is not a new problem:
  bits 41-39 = `000` is exactly the PRM erratum `docs/sharc/SPEC-FINDINGS.md`
  3.2 already records, and `build_table.py` already takes the PGR value.) **[D]**
- Done, and it holds. `Type19a` now fixes the nine bits the classic grid fixes,
  `000101100` (`mask 0xff8000000000`), with its `sc[1:0]` and `w` field
  declarations removed through a new `DROP_FIELDS` table in
  `tools/sharc_dev/sharcspec/build_table.py` -- an override there, not an edit to
  `figures.json`, which `extract_figures.py` regenerates. The provisional 48-bit
  `Type19p_undoc48` (`00010101`, eight fixed bits) takes the gap. **[V]**
- `tools/sharc_dev/sharcpcode.py compare out/sharcpcode/t23 out/sharcpcode/t24` reports
  **0 regressions**. Aligned and decoded counts identical (1.16 21,792 of
  21,792; 1.11 21,361 of 21,361); 1,170 and 1,049 main-program functions with
  the same size histogram; 296 and 343 functions truncated at bad instruction
  data; both probes keeping their earlier verdicts; and every Error and warning
  bookmark count byte-identical -- "Bad instruction - Truncating control flow
  here" 337, "Control flow encountered bad instruction data" 296, overlapping
  instructions 46, offcut calls 9. The only movement is timing noise of a few
  per cent in both directions. **[V]**
- Only the per-form split moved, and both sums are exact: 1.16 `19a` 969 ->
  559 plus `19p_undoc48` 410; 1.11 `19a` 966 -> 619 plus 347. That is 757 words
  across the two images, the same 757 `tools/sharc_dev/sharcfields.py` found. `18a`,
  `20a` and `19a_bitrev` are untouched, `19p_undoc48` has no undecoded words and
  no length mismatches, `tools/sharc_dev/sharccompare.py` still has both decoders agreeing
  on every offset of 1.16, and `sharcfields` no longer flags Type19a. **[V]**
- So 757 words keep their length, their neighbours and every flow metric, and
  only stop being called Type 19. That is the strongest evidence available short
  of a manual that the split is right, and it says nothing about what they
  do. **[D]**
- Still open: what `10101` is, and what bit 39 selects within it -- 748 of the
  757 have it set. Two of the mid-instruction branch targets the handover counts
  as unexplained still land inside a `19a` (1.16 SW `0x1c866b` inside
  `0x1c866a`, and `0x1c87a0` inside `0x1c879e`), so a neighbouring form is wrong
  as well. **[O]**

## The decode table now records which classic tables it merged **[V][C]**

- `tools/sharc_dev/sharcfields.py` guessed a form's classic.json table from its name, and
  the guess is wrong for every split form: `Type8a_rel` resolves to `Type 8a`
  where the merge really used `Type 8a #2`. `build_table.py` knew the answer and
  threw it away, so every emitted form now carries `classic_keys`, and
  `sharcfields` reads it instead of guessing. **[V]**
- It also now mirrors the merge rule: a bit counts as fixed only where every
  merged variant prints the same digit. That corrects two false positives this
  tool reported on its first run. `Type11a` merges both `Type 11a` (`00001010`)
  and `Type 11a #2` (`00001011`), whose patterns differ at bit 40 -- so bit 40
  is the digit that picks between the two tables, `build_table.py` is right to
  leave it free, and the PRM is right to call it the field `x`. Its `lr` at bit
  24 is a second such selector. Neither was a conflict; the count of fields
  sitting on bits the classic grid fixes goes 3 to 0. **[V][C]**
- No form's decode changed: masks, values, widths and fields are identical, and
  `tools/sharc_dev/sharccompare.py` still has both decoders agreeing on all 22,886
  offsets of 1.16. **[V]**

## The Type 8a branch forms were taking words that are not branches **[V][C]**

- `audit_bits.py` reports `Type8a_abs` and `Type8a_rel` dropping seven PRM bits
  (32-27 and 25), which the handover named as where to start on the `8a_rel`
  with the nonsensical target. Restoring all of them is wrong, and measurably:
  it costs 225 aligned instructions in 1.16 and 279 in 1.11, and takes
  `Type8a_abs` from 29 matches to 6. **[V]**
- Measured per bit over both images instead, only one bit is a real
  discriminator. Bit 25 is set in 13 of `8a_rel`'s 1,382 instructions and 19 of
  `8a_abs`'s 54, and 30 of those 32 carry a target that is not a plausible
  address. The other six gap bits are each set in 7 to 20 instances, and bit 23
  -- which the wholesale fill would also have fixed, on `Type9a` -- is set in 18
  of `Type9a_abs`'s 98 and 14 of `Type9a_rel`'s 34. Those are fields in use, not
  zeros, so the PRM's digits in the branch figures' gaps are the same stale
  template values as on the compute forms (`docs/sharc/SPEC-FINDINGS.md` 3.8).
  **[V]**
- Fixing bit 25 to zero and leaving the words homeless still regressed: decoded
  instructions fell 50 in 1.16 and 71 in 1.11 and `halt_baddata` went 296 to
  298, because an undecoded word strands the alignment sweep. Giving them a
  48-bit home instead is flat. **`Type8p_undoc48`** takes bits 47-41 = `0000011`
  with bit 25 set -- one prefix for both halves, since bit 40 is Type8a's own
  abs/rel selector, kept as the field `r` -- and holds 16 instructions per
  image. **[V]**
- Measured: `compare` reports **0 regressions** against both `out/sharcpcode/t24`
  and `out/sharcpcode/t23`. Aligned counts are identical to baseline (21,792 and
  21,361), and `8a_rel` + `8a_abs` + `8p_undoc48` sums exactly to the old
  `8a_rel` + `8a_abs`: 636 + 19 + 16 = 671 in 1.16, 733 + 16 + 16 = 765 in 1.11.
  The nonsense `jump 0x3e0030` at 1.16 SW `0x1c13bc` and 1.11 SW `0x1c1366` --
  the same six bytes `00 07 3e 02 30 00` in both images -- is gone from the
  listing. **[V]**
- **Corrects the handover.** The probe `returns 2748` does not fail because of
  that word, and `FUN_001c136a` does not stop there: it decompiles to the same
  57 instructions before and after, and removing the bogus branch leaves the
  probe exactly as it was. 2748 is `0xabc`, loaded by the `17b` at SW
  `0x1c1498`, which sits in the delay slot of the `9b_abs` return at
  `0x1c1496` (`0x083f343f`, the I4/M6 idiom). The probe cannot pass until the
  language models delay slots. It is blocked on stage 1, not on the decoder.
  **[V][C]**
- Open: what the `Type8p` words are. 32 across two images, several of them
  byte-identical in both (`0x062207100000`, `0x062207300000`, `0x06220f8e0002`,
  `0x07110f840003`), so they are real shared code, not misalignment. **[O]**

## A generated subtable with no user **[V]**

- Adding a fixed bit to a split branch form made `sleighc` warn "Unreferenced
  table `target_pcrel_6b_w0`", and `tests/test_sharc_pcode.py` caught it.
  `gen_sleigh.py` calls `get_target_subtable()` once per branch form before it
  knows whether that form's constructors will need an `extra_by_word` variant
  instead; when every sharer of a shape takes a variant, the plain one is left
  with no user and nothing prunes it. The generator now emits only the
  subtables its constructors reference, matching on whole identifiers because
  `target_pcrel_6b_w0` is a substring of `target_pcrel_6b_w0_v2`. **[V]**

## The two decoders disagreed on one word, past the end of the file **[V]**

- `tools/sharc_dev/sharccompare.py` had them agreeing on all 22,886 offsets of 1.16 but
  differing by one on Digitone II 1.11: `unknown only in ours 1`. It is the last
  word of the image. `out/sharc/dn2-1.11-main.bin` is 105,016 bytes and ends on
  an all-zero word at byte offset `0x19a36`, where only two of the six bytes a
  48-bit instruction needs exist. **[V]**
- Both decoders zero-pad the missing words before matching, and an all-zero
  frame trivially satisfies `Type21a`, whose figure fixes every one of its 48
  bits. `tools/sharc_dev/sharc_disasm.py` 108 checks the matched form's width against how
  many real words were read and refuses; `tools/sharc_dev/sharcspec/sharc_decode.py`'s
  `linear()` had no such check and reported a phantom 48-bit instruction sitting
  on four bytes that are not in the file. It now makes the same check. **[V]**
- It predates today's work: the same single offset and form reproduce with
  HEAD's committed `decode_table.json` in a scratch tree, so none of
  `Type8a_abs`, `Type8a_rel`, `Type8p_undoc48`, `Type19a` or `Type19p_undoc48`
  is involved. 1.16 never showed it because its image does not end that way.
  **[V]**

## Every measurement here was of a stale language until it wasn't **[V][C]**

- `tools/sharc_dev/sharcpcode.py measure` compiles the slaspec sitting in
  `tools/sharc_dev/sharcspec/ghidra/SHARC_VISA/data/languages/` (`sharcpcode.py` 66,
  215-235). Nothing regenerates it, and it is git-ignored (`.gitignore` 34), so
  editing `decode_table.json` leaves it behind with no sign. **[V]**
- The guard that exists cannot catch this. `sharcpcode.py` 791-796 compares the
  compiled `.sla` against the one installed in Ghidra -- but a stale slaspec
  compiles to the stale `.sla` that is installed, so the two agree and the run
  proceeds. Runs `t24`, `t25`, `t26` and `t27` all recorded
  `slaspec_sha256 93c238502b…`, written at 11:17:10, before `Type19p_undoc48`
  and `Type8p_undoc48` existed. Their Ghidra numbers -- function counts,
  bookmarks, decompiler warnings, probe verdicts -- are of a language without
  those forms. **[C]**
- Their decoder numbers are unaffected: `aligned`, the per-form counts,
  `tools/sharc_dev/sharcfields.py` and `tools/sharc_dev/sharccompare.py` all read
  `decode_table.json` directly. Lengths agreed by luck -- the stale `Type19a`
  and `Type8a_rel` are 48 bits wide, like the forms that replaced them -- which
  is why nothing tripped. **[V]**
- `tests/test_sharc_pcode.py` 56-67 regenerates into a temp directory on every
  run, excluding the `SHARC_VISA` output tree from its copy. That is why the
  test caught the unreferenced-table warning and four measurements did not.
  **[V]**
- Measured properly, after `tools/coldfire/ghidra/install-sharc.sh` and a fresh
  generation (slaspec `ca2362aa…`, 80 constructors), against the session's
  starting point `out/sharcpcode/t23`: **0 regressions**, and the flow metrics
  improve. **[V]**

| | DT2 1.16 | DN2 1.11 |
|---|---|---|
| "Bad instruction - Truncating control flow here" | 337 -> 323 | 414 -> 399 |
| functions truncated at bad instruction data | 296 -> 289 | 343 -> 336 |
| Error bookmarks | 165 -> 160 | 182 -> 178 |
| main-program functions | 1,170 -> 1,169 | 1,049 -> 1,048 |

- `FUN_001c136a` loses its `halt_baddata()` call and both warning comments, so
  removing the bogus branch did repair that function -- it still cannot satisfy
  the `returns 2748` probe, which needs the delay slot. `8p_undoc48` now emits
  no p-code where those same words used to emit `goto`/`call` as `8a_rel`.
  **[V]**
- `measure` now regenerates into a temp directory and refuses when the slaspec
  on disk is not what `gen_sleigh.py` produces from the current table, and
  records `decode_table_sha256` in `lint.json` so a past run can be audited.
  **[V]**

## selache encodes and decodes DT2 SHARC+ code, once parcel order is fixed **[D][O]**

selache (GPLv3, `https://github.com/js216/selache`, commit `2b26d3b7`, targets
ADSP-21569 = the DT2's family) builds clean with `cargo build --release`.
Binaries: `selar selas selcc seld seldump selhex selinit selload selmem selpatch
selsyms`. Correction to the earlier note: there is no `selinstr` *binary* -- it
is a library crate consumed by `selas` and `seldump`; feeding raw firmware bytes
to it needs a small harness, because `seldump` only accepts ELF.

The blocking detail: **selache reads VISA parcels big-endian** (its
`selinstr/src/visa.rs` `read_16_be`, documented as "VISA parcels are always
big-endian ... regardless of ELF endianness") while `section_7_BLOB.bin` and
`tools/sharc_dev/sharc_disasm.py` are **little-endian per 16-bit parcel** (the boot-stream
convention). Fed unswapped, selache desyncs after the first instruction. Swapped,
it tracks our decoder for hundreds of consecutive instructions.

Measured over two 4000-byte windows at `0x1c24e9` and `0x1c71ec` (~1660
instructions, all three VISA widths present: 16/32/48-bit): ~1.4% unrecognised
by selache, 22 of 24 hand-tabulated instructions agree byte-for-byte (the two
non-agreements are one mutual "uncertain" and one selache table gap, neither a
contradiction). Assembling 15 hand-transcribed instructions with `selas`
(`-proc ADSP-21569`) and comparing to the image: **14/15 byte-identical**.

This is meaningful three-way corroboration of `tools/sharc_dev/sharc_visa_tables.py` --
an independently authored decoder built from the public PRM reaches the same
instruction boundaries. Two selache bugs found, both isolated, and in both cases
our reading is the one consistent with the image bytes:

1. Decode width bug at `0x1c7229` (`be6a0234b098`): a type-4a fused
   compute + dual-memory 48-bit form is truncated to a 32-bit move, dropping two
   bytes; it resyncs at the next instruction.
2. Encode round-trip bug: `LSHIFT ... BY -<n>` re-encodes with the `dataex`
   sign-extension nibble cleared (`023e0000f091` vs the image's `023e7800f091`),
   though both decode to the same text under selache's own decoder.

Verdict: usable as a cross-check and as a base for eventual code emission,
**provided the parcel byte-order fix is applied**. This does not address the
Tier-2 injection-seam problem, which remains the hard part.

Round trip re-run on 2026-09-22 with parcels swapped: a `selas` snippet
(16-bit computes, compressed DM store/load, a 48-bit immediate load, a
fused compute+DM form, a delayed jump) decodes in `tools/sharc_dev/sharc_disasm.py`
to the same eleven instruction boundaries, and runs in
`tools/sharc_dev/sharc_trace.py` with all seven expected results (the jump target was
patched by hand, as `selas` leaves relocations unresolved).
`tools/sharc_dev/selasm.py` assembles a file, swaps parcels and prints both listings.
Two more Selache decode bugs, both checked against the bits of code we
assembled: `decode_32_group4()` merges two adjacent 16-bit Type3c
instructions (`90 34 90 15`) into one bogus 32-bit instruction and drops
the store; `decode_type3()` reads the fused form's M register as
`bits(39,38)+4`, where the field is bits 40:38 with no offset, so it
reports M5 for M1. **[V]**

## Only 22% of the image has semantics, and the emulator runs the rest as no-ops **[D][O]**

The generated SLEIGH emits an empty `{}` body, never `unimpl;`, for every
constructor without hand-written semantics
(`tools/sharc_dev/sharcspec/ghidra/gen_sleigh.py`, `Constructor.emit`). pypcode therefore
decodes such an instruction and returns zero p-code ops rather than raising.
Two forms are correctly empty and are not gaps: Type21a (NOP) and
Type9a/9b_abs with `b==1` (register-indirect call, left empty on purpose).
Lifting all 22,668 aligned instructions of the DT2 1.16 main program
(`sharcflow.aligned`, `min_depth=8`; 47 forms; the language decoded every one)
finds p-code for 5,007, or 22.09%. The largest forms with none are `15b`
(4528, 19.98%), `3c` (1488), `5b_move` (1412), `2a_short` (1226), `4a`
(1024) and `3a` (983); `3b` has semantics for 5 of 1349 and `14a` for 615 of
648. In `FUN_001c18a6` 204 of 654 instructions (31.19%) have p-code. **[D]**

Ghidra 12.1.3's `EmulatorHelper` executes this language. Stepping
`FUN_001c18a6` from its entry ran 525 instructions, and a run started at
`sw 0x1c18ed` ran 200 of 200 through both known stores, `sw 0x1c191d` and
`sw 0x1c1928`. `EmulatorHelper.readMemory`/`writeMemory` observe live
emulated memory: a write of `0xabcd1234` to `0x254d9c` read back unchanged.
The PC register takes the short-word PC; writing the displayed entry
`0x38314c` into PC made `getExecutionAddress()` return `0x706298` and fail.
Data addresses are covered in the next section. **[D]**

Most of that execution is not real. A form with no semantics runs as a silent
no-op instead of faulting, so an emulator watchpoint would miss a write made
through `15b`, `3a`, `4a` or `3c` rather than stop at it. The only fault seen
was `Unimplemented CALLOTHER pcodeop (condition)` on conditional jumps. The
same gap blinds the decompiler dataflow queries in
`06-sharc-engine-and-startup.md` to indexed stores. `MemoryAccessFilter`
cannot be subclassed from Python ("Java classes cannot be extended in
Python"), so catching same-value writes needs a small Java shim. **[O]**

## The language puts every data literal at twice its address **[V][C][O]**

The generated language gives the `ram` space a word size of 2, so short-word
code addresses land on the right bytes. Ghidra scales every LOAD and STORE
offset by the space's word size, so a data literal is accessed at twice its
value: `R2 = DM(0x252658)` at `sw 0x1c18ed` builds `0x252658` and loads from
byte offset `0x4a4cb0`. No memory block of the Ghidra program covers
`0x4a4cb0`: `read 0x4a4cb0` fails as unmapped, while `read 0x252658` lands in
block `mem01_282403f0`. Ghidra's reference table already carries the
doubling: its only reference to `0x4a4cb0` comes from this instruction, at
`0x3831da`. **[V]**

The literal is a byte address. Ghidra places loader byte `0x28000000 + X` at
offset `X` for every block, and code at `sw S` at `2*S`. Of the 646 Type14a DM
accesses in the DT2 1.16 main program, all 583 on-chip literals (`0x25xxxx`,
`0x26xxxx`, `0x2dxxxx`) are covered by the loader at `0x28000000 + addr` and
none at `0x28000000 + 2*addr`, which falls outside every loader range. 558 of
those hits are in the zero-FILL block 18, so the stronger evidence is the 25
in payload blocks: `0x256800..0x256830`, read from `sw 0x1c1d8d` to
`0x1c2203`, hold a table of little-endian pointers into the same range
(`0x00253e78`, `0x00254178`, `0x00254278`, ...). The 17 external literals
(`0x82a0xxxx`) are covered at the literal itself. The SHARC+ core reference
states that the byte address space is universal for the core and SoC, and
Type14a's syntax has no access-size suffix, unlike Type14d's. A second agent
recomputed all of this independently. **[V]**

**[C]** The emulator therefore never sees the image's initialised data at a
DM literal and reads zero there, and an external literal such as
`0x82a00008` doubles past the 32-bit space. `ghidraq` `loads`, `stores` and
`read` compare against the caller's value and stay self-consistent, but
`xrefs`, `callers` and `range`'s referenced addresses read Ghidra's reference
table and are wrong for DM literals. Fixing this means giving data its own
byte-addressed space in the language, measured with `tools/sharc_dev/sharcpcode.py`.
**[O]**

## DM literals now reach their own bytes **[V][C]**

**[C]** The doubling in the previous section is fixed. `gen_sleigh.py`'s
`dm_byte_addr_to_ram_unit` translates a DM byte address to a `ram` unit
before each DM access: `unit = (addr >> 1) + isl2 * 0xF0B80000`, where `isl2`
is 1 inside the L2 byte window `0x20000000..0x20020000`, so L2 lands on the
importer's short-word alias (`0x20000010` -> unit `0xB80008`). Both current
DM sites use it, the Type14a scalar forms and the exact Type3b reader; PM
accesses are unchanged. `tools/sharc_dev/sharc_import.py` now shifts only the L1 alias
window `0x28000000..0x28400000` down by `0x28000000`, so external memory sits
at its own address (`0x82a00008` stays `0x82a00008`, unit `0x41500004`).
All 646 Type14a DM literals in the DT2 1.16 main program are even, so the
shift loses nothing. `tools/sharc_dev/sharcpcode.py compare` of the language before and
after shows no regression; decompiler warnings about globals overlapping
smaller symbols fell from about 90 to about 11 per image. **[V]**

Checked live on a fresh import, `~/ghidra-projects/sharc-dm-dt2-116`. The
Type14a load at `sw 0x1c21ee`, `R2 = DM(0x256820)`, emulated for one step
leaves `R2 = 0x00254878`, the payload bytes `78 48 25 00` of loader block 19.
With `--poke 0x256820=0x11223344` it leaves `0x11223344` instead.
`xrefs 0x252658` finds the read at `sw 0x1c18ed`, and `xrefs 0x254d9c` the
write at `sw 0x1c191d`. A second agent re-walked the loader headers, decoded
the instruction and repeated the emulation independently. **[V]**

Data addresses are now passed to `ghidraq` and `sharcemu` as plain byte
addresses. Ghidra's GUI still names data by `ram` unit, so DM `0x252658` is
labelled `DAT_0012932c`. Projects imported before this change use the old
language and are stale. The 31 Type14a DM accesses with `l = 1` (long word)
still have no semantics. **[O]**

## Indexed DM forms 15b, 4a and 3a now have semantics **[V][D][O]**

`gen_sleigh.py` gives the DM (`g = 0`) cases of Type15b, Type4a and Type3a
real loads and stores, each address built through `dm_byte_addr_to_ram_unit`.
Under the firmware's 32-bit normal words, 15b addresses
`I + sext7(data) * 4` and never updates I. 4a scales `sext6(data)` by 4 and
3a scales `M` by 4; pre-modify (`u = 0`) adds the offset without updating I,
and post-modify (`u = 1`) accesses the old I and then adds the offset. What
the language cannot model yet becomes a named unimplemented p-code op instead
of being dropped: `compute(field)` for a nonzero 23-bit compute field, the
existing `condition(code)` gating the whole instruction when cond is not
TRUE, and `circular(i)` in place of the update when `L[i] != 0`, since the
reference allows circular buffering only with post-modify addressing. PM
(`g = 1`) and long-word (`l = 1`) cases stay empty. None of the 4528 15b or
983 3a instances in DT2 1.16 targets PC, the PC stack or a loop register.
**[V]**

A second agent lifted all 6,535 DT2 1.16 instances of the three forms with
the installed language, ran them through its own p-code interpreter under
several register scenarios, and found no mismatch against a reference written
from the public manual. pypcode and `tools/sharc_dev/sharc_disasm.py` agree on the
length of all 22,668 aligned instructions. 105 instances load into the I
register they address through (`I4 = DM(M5, I4)`); all are pre-modify, so no
update competes with the load. In the emulator a 15b load, a 15b store and a
4a post-modify store behaved exactly as predicted. **[V]**

Coverage measured with `tools/sharc_dev/sharc_worklist.py` rose from 22.09% to 51.02%
of the image and from 31.19% to 72.94% of `FUN_001c18a6`.
`tools/sharc_dev/sharcpcode.py compare` shows no decompile failure or timeout;
decompile time rose from 1.6 s to 6.9 s, and the constant-folded guards
produce 6,534 "Removing unreachable block" warnings. Stepping `FUN_001c18a6`
now runs 210 instructions and stops mostly at `compute` (139 of 301 faults)
rather than at missing forms. **[D]**

Every indexed store is now visible to the dataflow queries. A whole-image
`ghidraq stores 0x252658` returns 3,692 `computed-or-unresolved` candidates in
396 functions, where it returned none before. By address root they are led by
I6 (2,021), addresses that come through a load (1,338), I4 (135) and I5 (73).
None is resolved to `0x252658` yet. **[D][O]**

## Type3d, Type4d and Type14d promoted to confident; Type15a's mem_access/dataref address bug fixed **[V]**

- Types 3d, 4d and 14d were on `tools/sharc_dev/sharcspec/README.md`'s "SHARC+-only
  encodings with no second source" list: the PRM (`sc58x-2158x-prm.pdf`) is
  the only manual that documents them at all, so `decode_table.json` carried
  `unconfirmed_bits` equal to every one of their fixed bits (3d 7, 4d 8, 14d
  7) and `tools/sharc_dev/sharc_coverage.py`'s tracer/runner refused every instance as
  "decode confidence uncertain". On the render path from `0x1c2b24`: 14d 56,
  4d 16, 3d 15 instances (`tools/sharc_dev/sharc_coverage.py dt2-1.16 --root
  0x1c2b24`); whole-image dt2-1.16: 14d 80, 4d 20, 3d 58.
- Checked two ways, independently, then re-checked by a second agent with no
  access to this session's own numbers:
  - **From the manual's bit layout, against the raw bytes.** The PRM prints
    each form's fixed bits as gray-filled cells in its opcode figure (Type3d
    Figure 14-9 p.326, Type4d Figure 14-12 p.337, Type14d Figure 16-2 p.388,
    `out/refs/sc58x-2158x-prm/`). Converting each figure's fixed-bit pattern
    to a mask/value by hand reproduces `decode_table.json`'s `mask`/`value`
    exactly (3d `0xe00000780000`/`0x400000300000`, 4d
    `0xf00000780000`/`0x600000300000`, 14d `0xfe0000000000`/`0x1a0000000000`).
    Reading the raw 48-bit word directly from `section_7_BLOB.bin` (3
    little-endian 16-bit parcels) for one instance of each form and
    extracting every field by the manual's bit positions alone (not through
    `sharc_isa.py`) reproduces the database's decoded `fields` and mnemonic
    exactly, e.g. sw `0x1c8119`: word `0x1b4200269454` ->
    d=1,ex=0,l=1,w=0,x=0,dreg=2,addr=`0x269454`, matching `DM(0x269454) = R2
    (sw)`. The PRM's own syntax tables for each form give a direct mnemonic
    rule (Type3d: `w=0,cond=31` -> `ACCESS`, else `IFCOND ACCESS`; Type4d:
    same by `cond` alone; Type14d: `(l,x,d)` -> the BH/BHSE/BHEX/BHSEEX
    suffix tables), and every real dt2-1.16 instance's rendered mnemonic
    matches that table with no exceptions (all 80 Type14d instances'
    `bw`/`sw`/`bwse`/`swse` suffixes checked). A second agent independently
    re-extracted the same three figures from rendered PDF pages and 18
    firmware instances (its own read of `section_7_BLOB.bin`, not this
    session's), and reported every field position and every decoded value
    matching `decode_table.json` and the database exactly, with one caveat:
    the PRM prints Type3d/Type4d's low 16 bits as an unlabelled all-zero row
    that `decode_table.json`'s mask leaves unconstrained rather than
    fixed-zero. That is the same 16 bits `tools/sharc_dev/sharcspec/audit_bits.py`
    already found and the "Type22a is idle" section above already resolved:
    restoring them as fixed changes nothing measurable on real code, so the
    merge rule (ignore a PRM figure's stale template defaults) correctly
    leaves them unconstrained. **[V]**
  - **From dataflow, against neighbouring code.** `0x269454` is a genuine
    counter: `R2=0x40; DM(0x269454)=R2` initialises it, later code
    `R2=DM(0x269454); ...; R2=sub(R2,R4); DM(0x269454)=R2` is a
    read-decrement-store loop (`img.listing`, function `0x1c80f2`).
    `0x82a001b8` (Type14d loads at `0x1c4720`/`0x1c4855`) sits inside the
    `0x82a00000`-`0x82a001ff` SoC command block already independently
    verified above; a Type17a at `0x1c47e5` loads the same address into an I
    register right beside them (`img.refs`). The Type3d cluster at function
    `0x1c42b8` is a textbook prologue: `I7=modify(I7,-10)` then `DM(I6-N) =`
    M2/M3/I3/I5/R9..R15 -- the same I7-as-SP register-save idiom the call
    convention above documents. The Type4d cluster at `0x1c5615` reads
    `DM(I1-8)`, `DM(I1-9)`, `DM(I1-14)`, `DM(I1-15)` -- small negative
    stack-relative offsets, the shape a compiler's spilled-argument reads
    take. Every one of the 158 whole-image instances (87 on the render path)
    has a clean, aligned, non-gap successor instruction starting exactly 3
    short words later (`tools/sharc_dev/sharc.py`'s `insn` table, `aligned=1`); none
    lands on a form the decoder had to guess at. **[V]**
  - Both checks agree for all three forms, so `decode_table.json` now sets
    `unconfirmed_bits` to 0 for Type3d/Type4d/Type14d (`source` records the
    manual pages and the corroboration). `sharc_coverage.py`'s "provisional
    14d/4d/3d: decode confidence uncertain" gaps are gone (see counts below).
- **Database bug, form 15a: `mem_access.abs_address` was the I-register
  offset, not an address.** `extract_mem_access()`'s `DIRECT_MEM_FORMS`
  branch (`tools/sharc_dev/sharcdb/`) treated Type15a the same as the genuinely
  absolute Type14a/14d, storing its raw `addr` field straight into
  `abs_address`. The PRM is explicit that Type15a is `DM(<data32>,Ia) =
  Ureg` / `PM(<data32>,Ic) = Ureg` (pp.387-390, Figure 16-3, worked example
  `DM(24,I5)=TCOUNT;`): an I-register-relative access the core never updates
  I for, not a direct address -- `tools/sharc_dev/sharcfn/`'s `render_mem_direct`
  already documents this correction for the disassembler's own mnemonic
  rendering and `_ptr_mem_form` already models it correctly for the `ptr`
  table; only `extract_mem_access` (mem_access) and the `extract_literal`
  caller's role classification (dataref) still had the old assumption. A
  second agent's independent read of the same PRM pages confirmed the
  citation. **[V]**
  - Before the fix, all 892 dt2-1.16 Type15a `mem_access` rows carried a
    bogus `abs_address` (429 of them a small negative offset stored as
    `0xfffexxxx`-`0xffffxxxx`, indistinguishable from a real top-of-space
    address; the rest small positive offsets indistinguishable from a real
    low DM/PM address), and the matching `dataref` rows carried role
    `abs_load`/`abs_store` (446/446) as if the raw offset were a resolved
    address.
  - Fixed: Type15a now gets its own branch in `extract_mem_access`, folding
    the DAG bank into the I-register number exactly as `render_mem_direct`
    does (`g=1` -> `+8`, e.g. `i=4,g=1` -> `I12`, matching the mnemonic
    `PM(I12 - 0x54) = L0, long`) and putting the sign-extended offset in
    `modifier` (the same column `INDEXED_MEM_FORMS`/`IMMOFF_MEM_FORMS` use
    for their own register-relative accesses); `abs_address` is now NULL for
    every Type15a row. The `extract_literal`/`dataref` caller now only
    assigns `abs_load`/`abs_store` for Type14a; Type15a's literal falls
    through to role `literal` (a modify delta, like 16a/18a/19a*), matching
    892 rows. After the fix: 0 `mem_access` rows anywhere in the image carry
    an `abs_address` in the `0xffff0000`-`0xffffffff` range; the only forms
    with `abs_address` set at all are 14a (1,610 rows) and 14d (80 rows),
    both genuinely absolute.
  - `INDEXED_MEM_FORMS` (3a/3b/3d/6a_mem) and `IMMOFF_MEM_FORMS`
    (4a/4b/4d/15b) were checked and never set `abs_address` in the first
    place -- their branches in `extract_mem_access` hard-code it `None`;
    only Type15a's presence in the `DIRECT_MEM_FORMS` grouping (alongside
    the genuinely-absolute 14a/14d) caused this bug, and it was isolated to
    that one form. **[V]**
- `DB_VERSION` 11 -> 12 (`tools/sharc_dev/sharcdb/`, both changes as one bump).
  Rebuilt `out/sharcdb/dt2-1.16.sqlite` (private per-lane copy) before/after,
  same `section_7_BLOB.bin` (image sha256 unchanged):

  | | before | after |
  |---|---|---|
  | `sharc_coverage.py --root 0x1c2b24`: gap instructions | 90 | 3 |
  | ... of which `provisional 14d/4d/3d: ... uncertain` | 87 | 0 |
  | `sharc_coverage.py --all-roots`: gap instructions | 166 | 11 |
  | ... of which `provisional 14d/4d/3d: ... uncertain` | 155 | 0 |
  | `mem_access` rows with `abs_address` in `0xfffe0000..0xffffffff` | 429 | 0 |
  | Type15a `mem_access` rows with any `abs_address` set (all bogus) | 892 | 0 |
  | Type15a `dataref` rows with role `abs_load`/`abs_store` (bogus) | 892 | 0 |
  | Type15a `dataref` rows with role `literal` (correct) | 0 | 892 |

  The remaining `sharc_coverage.py` gaps (26a `decode confidence uncertain`,
  a handful of unsupported full-compute opcodes, `21p_undoc16`/`8p_undoc48`)
  are untouched -- they are separate, already-open items.
- `tests/sharc/integration/test_sharc_golden.py`'s six cases were compared output-for-output
  (`output(name)`), not just by hash, both before (original code, original
  database) and after: `run_voice`/`trace_voice` (root `0x1c4ecf`, a
  different function) are byte-identical, since that path never touches
  3d/4d/14d/15a. `cov_render`/`cov_all` differ only by the removal of the
  14d/4d/3d gap rows above; every other gap entry (`1a`/`2a_short`/
  `5a_move` full-compute, `26a`, `21p_undoc16`, `8p_undoc48`) is
  byte-identical before and after. `run_frame`/`trace_frame` (root
  `0x1c2b24`) now run past the point the confidence gate used to stop them:
  the concrete runner (`tools/sharc_dev/sharc_run.py dt2-1.16 --start 0x1c2b24
  --json`) went from halting after 130 instructions on a refused Type14d at
  sw `0x1c2c41` to running 13,895 instructions (touching 14d x97, 4d
  x1,024, and every other form along the way -- deeper execution reaches
  more of everything, not just the newly-confident forms) before halting on
  an unrelated cause, an `8a_rel` branch whose predicate depends on an
  unseeded/unknown register value ("fork (2 successors): a predicate or
  address went Unknown despite concrete input"); the symbolic tracer
  (`--allow-provisional-form 14d`, now a no-op since 14d is no longer
  provisional) still ends in exactly 2 states, both an `external-call` stop
  at the same site (sw `0x1c2ca0`): one unchanged at 158 steps (the branch
  that never reached the Type4d instruction), the other -- which used to
  stop early at 155 steps on the refused Type4d loop-setup at sw `0x1c2c7c`
  ("uncertain or undecodable form: source: prm") -- now runs the Type4d
  instruction and continues 1,037 more steps to the same external-call site
  the other branch already reached, landing at 1,192 steps. No `15a` bug fix
  effect appears in any of the six golden cases: `mem_access`/`dataref`/`ptr` are
  read only by `tools/sharc_dev/sharc.py`'s `Image.refs`/`writers`/`readers`, not by
  the live tracer/runner, so that fix is silent here and only changes
  `tools/sharc_dev/sharc.py` query results. Updated with `uv run python
  tests/sharc/integration/test_sharc_golden.py --update`. **[V]**

## One decode path: `decode_at()` now makes the database's own width/form choice **[V]**

- Instruction decoding used to happen two ways that disagreed. The program
  database (`tools/sharc_dev/sharcdb/ build`, via `tools/sharc_dev/sharcimm.py`'s
  `decode_all()`) walks a whole code block and re-checks a wide form's own
  width choice against its successors (`_prefer_confident_widths`, see the
  "Type22a is idle"-era `_WIDTH_LOOKAHEAD` comment this section's history
  carries): a wide form that only wins `tools/sharc_dev/sharc_isa.py`'s
  `select_frame()` on a leading-fixed-bits tie is rejected in favour of a
  narrower form when the narrower reading decodes confidently for
  `WIDTH_LOOKAHEAD` (8) more instructions where the wide reading does not,
  or independently rejoins one of the wide reading's own successor
  boundaries. `tools/sharc_dev/sharc_core/sequencer.decode_at()` -- the tracer's and
  concrete runner's only decode entry -- instead called `select_frame()`
  once, with no successor context, so it kept the wide form in exactly the
  cases the database's second pass exists to reject.
- **Measured first** (`tools/sharc_dev/sharc_coverage.py <image> --compare-decode-at`,
  now a permanent, reusable check -- `compare_decode_at()` in that file):
  for every `aligned=1` row of the `insn` table in all four images, decode
  at that row's `sw` with `decode_at()` against this image's own
  `LoadedMemory` (no database access), and compare width, form and fields.
  Before the fix:

  | image | aligned rows | stale-bytes (excluded, see below) | decode_at mismatches | all classified as |
  |---|---:|---:|---:|---|
  | dt2-1.16 | 51,032 | 2,269 | 49 | width |
  | dn2-1.11 | 65,372 | 12,661 | 46 | width |
  | dt2-1.15C | 50,983 | 2,269 | 49 | width |
  | dn2-1.10E | 64,586 | 12,075 | 47 | width |

  Every mismatch was one of exactly two `(db_form, decode_at_form, width)`
  patterns, on every image: `2c`(2 bytes) vs `2b`(4 bytes) -- 44-47 of them
  -- and `22c`(2 bytes) vs `22p_undoc48`(6 bytes) -- 2 of them. Both are
  the wide-wins-a-leading-bits-tie pattern the database's second pass
  already corrects; none was a form choice among same-width candidates or
  a fields-only difference. DT2 1.16's two `2c`/`2b` instances at `sw
  0x1c502b` and `sw 0x1c59ce` are exactly `tools/sharc_dev/sharc_harness/`'s
  `_KNOWN_BAD_DECODE_PCS` workaround (that module's own docstring already
  named this "a systematic decode-table/disassembler bug, not two isolated
  ones" and asked for it to be reported rather than patched locally) -- the
  fix below makes that workaround unnecessary, though `sharc_harness.py`
  is lane A2's file and was not edited here. **[V]**
- **Fix: one function.** `tools/sharc_dev/sharc_disasm.py` gained
  `resolve_confident_width(pos, insn, raw_at, narrow_at, lookahead)`, the
  width/form decision itself, position- and source-agnostic (a byte offset
  into a flat buffer, or twice a short-word PC for loader-backed memory);
  `NEVER_ALIGNED_FORMS` and `WIDTH_LOOKAHEAD` moved there with it as the
  decoder's own policy constants. Two entry points call it:
  `decode_confident(data, offset)` (flat buffer) and
  `decode_confident_loaded(reader, pc_sw)` (loader-backed memory), both
  failing closed exactly as `disassemble()`/`decode_loaded_at()` already do
  when nothing decodes at all. `NEVER_ALIGNED_FORMS` is excluded only from
  a *successor* in the width-lookahead chain (matching `decode_all()`'s
  `table.get()`, which never records one as "aligned" real code either),
  not from the PC the caller asked to decode directly: an earlier version
  of this fix also refused to decode a `NEVER_ALIGNED_FORMS` pattern landed
  on directly, which broke `tests/test_sharc_trace.py`'s
  `PhaseAOpcodeRegressionTest` (it decodes a synthetic Type10a_rel at PC 0
  on purpose, to check the form's own split `reladdr` fields -- a
  legitimate use `decode_at()` never promised to refuse). The database has
  no opinion on such a PC at all, not a "definitely not code" one, so
  `decode_confident()`/`decode_confident_loaded()` do not either.
  `tools/sharc_dev/sharc_core/sequencer.decode_at()` (the only file this task
  was allowed to change in `sharc_core/`) now calls these instead of a bare
  `disassemble()`/`decode_loaded_at()` call -- no database access at decode
  time, exactly the emulator's own constraint. `tools/sharc_dev/sharcimm.py`'s
  `decode_all()` was refactored to call `resolve_confident_width()` too,
  passing its own memoized `table.get` as `raw_at` so a whole-image walk
  stays O(n) instead of re-decoding each lookahead step; `_NEVER_ALIGNED_
  FORMS`/`_narrow_candidates`/`_confident_reach`/`_prefer_confident_widths`
  no longer exist as a second, separately-maintained copy of the same
  logic. This is a pure refactor of `decode_all()`: rebuilding
  `out/sharcdb/dt2-1.16.sqlite` (private per-lane copy) before and after
  gives byte-identical `insn` rows (107,911 of them) and identical row
  counts in every other table (`functions`, `edges`, `mem_access`,
  `dataref`, `regdef`, `reguse`, `reach`, `ptr`, ...) -- `DB_VERSION` stays
  at 12, no rebuild is required for anyone reading an existing database.
  **[V]**
- **After the fix**, the same `--compare-decode-at` check gives 0
  mismatches on all four images (stale-bytes counts unchanged, see below):

  | image | aligned rows | stale-bytes | decode_at mismatches |
  |---|---:|---:|---:|
  | dt2-1.16 | 51,032 | 2,269 | 0 |
  | dn2-1.11 | 65,372 | 12,661 | 0 |
  | dt2-1.15C | 50,983 | 2,269 | 0 |
  | dn2-1.10E | 64,586 | 12,075 | 0 |

  **[V]**
- **"Stale-bytes" is a separate, real thing this check surfaces, not a
  decode bug.** For 2,269 of DT2 1.16's/1.15C's aligned rows (12,661 /
  12,075 for DN2 1.11/1.10E) -- all of DT2 1.16's in loader block 1, `sw
  0x120203`-`0x12161b` -- the database's own recorded `raw` bytes for that
  row do not match what `LoadedMemory.read_sw()` returns at the same `sw`
  today. Cause: block 1 is a `code` loader block (`base_sw` `0x1201f8`,
  10,312 bytes), but its whole byte range is overwritten in the same boot
  stream by later blocks (mostly one large 86,216-byte `FILL` of zeros,
  block 18, plus a few small `data` patches) before `LoadedMemory`'s
  documented last-write-wins resolution reaches a final answer -- so the
  bytes `tools/sharc_dev/sharcdb/`'s per-block scan decoded were never resident in
  the image the emulator (or `decode_at()`) actually runs from. DN2 1.11/
  1.10E's larger counts are the same block-1 pattern (2,269 rows) plus a
  second, larger, not-yet-characterized block (block 57, 10,392/9,806
  rows) -- open. `compare_decode_at()` treats this as a third outcome
  (`stale_bytes`), skipped rather than compared, so it cannot masquerade
  as a decode mismatch; the check first appends `raw` bytes to the query
  (`_raw_le_bytes()`, the 16-bit-little-endian-word packing `tools/sharc_dev/sharc_isa.py`'s
  `frame_of()`/`DecodedInstruction.raw` use) and compares against
  `read_sw()` before ever calling `decode_at()`. **[V][C]** (real and
  reproducible; resolved below, "Loader block 1 is not code: decoding from
  LoadedMemory" and "Loader block 57 was never stale: a too-small
  L2_BYTE_LIMIT" -- block 1 is dead scratch code, never entered in any of
  the four final images, and DN2's "not-yet-characterized" block 57 turned
  out to be a mapping bug, not a real overwrite, both DB_VERSION v13).
- **Golden.** Only `trace_voice` changed (`cov_render`, `cov_all`,
  `run_voice`, `run_frame`, `trace_frame` are byte-identical). Compared
  old vs. new `output("trace_voice")` directly (not just the hash, by
  running the pre-fix code from a checked-out copy of the four changed
  files): 9 of 76 states differ, all in the same way, all downstream of
  the `sw 0x1c502b` fix -- the old run stopped there
  ("unsupported full compute cu=0x0 opcode=0x0", the wrong `2b` decode's
  compute field), the new run executes the correct `2c` (`R0 =
  add(R0, R0)`), advances to `sw 0x1c502c`, and keeps going until it hits
  this case's own `--max-states 64` cap instead. No other state, and no
  other case, differs. Updated with `uv run python
  tests/sharc/integration/test_sharc_golden.py --update`. **[V]**
- **Gap triage** (`tools/sharc_dev/sharc_coverage.py dt2-1.16 --root 0x1c2b24` /
  `--all-roots`, unchanged counts before and after the fix -- render path 3
  gap instructions, all roots 11):
  - **`sw 0x1c32b0` (`21p_undoc16`, 2 bytes) and `sw 0x1c32b4`
    (`8p_undoc48`, 6 bytes).** Real code: bytes match the database exactly
    (not stale), `decode_at()` agrees with the database's width and form
    (successor-confident -- both sit in a dense, ordinary compiled
    sequence of `2a_short`/`14a`/`17a`/`8a_rel` around them, `sw
    0x1c32a0`-`0x1c32c8`). The one-decode-path fix does not touch either
    -- they were already correct before it, since neither is a
    wide-vs-narrow width dispute. What they need: real semantics.
    `decode_table.json` marks both `unconfirmed_bits` (their `source`
    reads "firmware only" territory, the same status Type3d/4d/14d had
    before that section's two-source promotion above) -- someone still
    needs to find what SHARC+ instruction these bit patterns actually
    encode, from a manual page not yet cross-referenced or from more
    firmware corroboration, the way the Type3d/4d/14d section above did.
    **[O]**
  - **`sw 0x1c32b1` (`1a`, `cu=3 opcode=0xe0`) and, all-roots only, `sw
    0xb88e8b` (`2a_short`, `cu=2 opcode=0x40`) / `sw 0xb88edb`
    (`5a_move`, `cu=2 opcode=0x40`, both in loader block 69, the L2
    decimator region -- `tools/sharc_dev/sharc_harness/`'s `ADDRESSES["decimator"]
    = 0xb80000`).** Real code, bytes match, decode already confident and
    already agrees with the database. `sharc_coverage.py`'s "compute" gap
    kind means exactly what it says: `sharc_trace._compute`/`sharc_core.
    forms_compute` has no handler for these `cu`/`opcode` combinations. Not
    a decode-path issue at all -- these three need a compute-opcode
    handler, in files this task does not own (`sharc_core/forms_compute.py`,
    `compute_alu.py`, `compute_mult.py`). **[O]**
  - **`sw 0x1208e4`, `0x1208ff`, `0x12096d`, `0x12099c`, `0x1209b5`,
    `0x1209c4` (`26a`, uncertain, all-roots only).** All six are inside
    loader block 1 -- the exact stale-bytes region above. The database
    decodes block 1's own (overwritten-before-boot) bytes as `26a` (SYNC,
    still `unconfirmed_bits`); `decode_at()`, reading the same `sw`
    through `LoadedMemory`, sees the all-zero bytes actually resident
    there and confidently decodes `21a` (the idle/NOP-like catch-all) --
    exactly what `--compare-decode-at` reports as `stale_bytes` for these
    six rows, not a decode mismatch. Verdict: **not real code** in the
    image the emulator boots into, and **not a live decode disagreement**
    either -- the database's per-block scan simply analyzed input bytes
    that never reach the final image. The one-decode-path fix cannot
    "resolve" this (there is no live candidate to choose differently
    between; the input itself differs), and fixing it for real means
    teaching `tools/sharc_dev/sharcdb/`'s block scan to skip -- or teaching
    `sharc_coverage.py`'s gap report to exclude -- a `code` block a later
    block in the same stream fully overwrites, which is a database/
    lifecycle change, not a decoder one. **[V][C]** (resolved below, "Loader
    block 1 is not code: decoding from LoadedMemory", DB_VERSION v13 -- the
    lifecycle change this bullet asked for).
- **Tests.** `tests/sharc/analysis/test_sharc_coverage.py` (new): `_raw_le_bytes()`
  against a real DT2 1.16 example; `compare_decode_at()`'s four
  classifications (agreeing, stale-bytes, width, form, fields mismatch) on
  hand-built images; `RealImageAgreementTest` asserts 0 mismatches on the
  real DT2 1.16 database (skipped when the blob is absent -- the
  acceptance fact behind this section). `tests/sharc/analysis/test_sharc_disasm.py`
  gained `ConfidentDecodeTest`: `decode_confident()`/`decode_confident_
  loaded()` reproduce `tests/sharc/analysis/test_sharcimm.py`'s existing
  `WidthPreferenceTest` fixture; both still decode a `NEVER_ALIGNED_FORMS`
  pattern landed on directly (`_raw_at_flat`/`_raw_at_loaded` exclude it
  only from a successor chain, checked directly too); `decode_confident_
  loaded()` fails closed on a genuinely unmapped loader PC.
  `tests/sharc/analysis/test_sharcimm.py`'s own tests (unchanged) still pass, since
  `decode_all()`'s behaviour is unchanged. **[V]**

## One decode INPUT, not just one decode function: DB_VERSION v13 **[V]**

The previous section's own "Stale-bytes" note left two things open: block 1's
role, and DN2's larger, uncharacterized stale-bytes count on block 57. Both
are now resolved. The model decided on: the database describes the program
**as loaded** -- one code block's `insn`/`functions`/edges rows are decoded
from `tools/sharc_dev/sharcldr.py`'s `LoadedMemory` (the whole boot stream's merged,
last-write-wins final image), not from that block's own raw stream payload.
This is the same input `tools/sharc_dev/sharc_core/sequencer.decode_at()` already used
(no database access, `LoadedMemory` only) -- the "One decode path" section
above unified the decode *function*; this unifies the decode *input* the
same way, for the same reason: a block whose own bytes never persist into
the image the emulator boots from was never real code, however confidently
it decodes. A block that genuinely ran as a boot stub before being
overwritten would need to stay distinguishable (a separate table or a flag
naming the overwriting block), not merged into the final-program tables --
no such case was found in this firmware (see below), so no such table was
added; `blocks.overwritten_bytes` (new column) is the general mechanism that
would surface one if a future image had it.

### Loader block 1 is not code: decoding from LoadedMemory **[V]**

Loader block 1 (target `0x282403f0`, `byte_count` 10312, base_sw `0x1201f8`)
is identical across all four images and is **not resident in any of the four
final boot images**: 10304 of its own 10312 declared bytes (99.9224%) are
overwritten by later blocks in the *same* boot stream -- mostly one large
zero `FILL` block, the rest small `data` blocks -- before `LoadedMemory`'s
last-write-wins resolution is reached. Only two 4-byte spans of its own
declared range still belong to block 1 in the final image: `[0x28240458,
0x2824045c)` (all four images) and `[0x282412bc, 0x282412c0)` (DT2
1.16/1.15C) / `[0x2824128c, 0x28241290)` (DN2 1.11/1.10E) -- 8 bytes total,
inconsequential. Every overlapping block has a *higher* stream index than
block 1 (13 of them on DT2, 11 on DN2 -- DN2 packs the same address range
into fewer, larger `FILL` blocks), so this is unambiguously "a later write
in the same stream overwrites an earlier one", not a parsing artifact.
**Second-agent check (this task's required independent verification):** a
fresh agent, given no access to this finding's numbers or to
`LoadedMemory.owner_runs()`, wrote its own parser and two independent
ownership computations (a byte-by-byte scan and an interval sweep) directly
against `tools/sharc_dev/sharcldr.parse_blocks()` on the raw `section_7_BLOB.bin` of
all four images. Its numbers matched exactly: same overlapping-block lists,
same 99.9224% (10304/10312), same two surviving 4-byte ranges including the
DT2-vs-DN2 difference in the second range, and confirmed every overlapping
block's index is higher than block 1's (no contradiction of "only a later
write can override"). **[V]**

What block 1 actually is: the boot stream's docstring convention already
established (see `tools/sharc_dev/sharcldr.py`'s module docstring) that a
`BFLAG_FIRST` block's `target_address` names the start of "an application"
and that a stream can carry more than one, with only the *last* one being
the application the core actually boots into (`0x1c1338` for DT2, matching
this repo's `render_frame` root). Block 1's own declared range covers sw
`0x120230`, DT2's *first* `BFLAG_FIRST` entry point -- so block 1 is (part
of) an earlier-build application the same combined boot stream still
carries, whose own memory range the final build's loader blocks reuse for
parameter/coefficient data (hence their `data`/`FILL` kind) before the core
ever starts running. No root, no IVT slot (`_detect_interrupt_vector_roots`,
`IVT_DISPATCH_TABLES`) and no `BFLAG_FIRST` used by the *final* application
enters block 1's range; the ~82 "functions" and dense `dataref_code_pointer`
roots the pre-v13 database found inside it were themselves artifacts of
decoding these dead, self-overlapping bytes as if they were live code
(literal fields that happen to encode addresses back inside the same dead
range). `tools/sharc_dev/sharcinv.py`'s `CODE_BLOCKS` and this file's
`KNOWN_CODE_BLOCKS` no longer include it on any of the four images; its
`blocks` row now reports `kind='data'` (matching what the final image
actually holds there) with `overwritten_bytes=10304`. **[V]**

Collateral: `docs/findings/06-sharc-engine-and-startup.md` describes block 1
as "the loader" and cites function counts, literal cross-references and hash
matches drawn from it (e.g. "blk1 82" functions, "literal 0x60 occurs in
blk1, blk69 and blk88", reload counts "across blk1/..."). Those specific
blk1-derived claims no longer have a `blocks`/`insn`/`functions` row to point
to after this fix and should be reviewed by whoever owns finding 06 -- not
done here (docs/findings/06 and `tools/sharc_dev/sharcinv.py`'s own module docstring
are outside this task's file ownership; only the `CODE_BLOCKS` line and the
`--blocks` usage example were corrected in `tools/sharc_dev/sharcinv.py`, since the bug
lives in the code that file defines). **[O]**

### Loader block 57 was never stale: a too-small L2_BYTE_LIMIT **[V]**

DN2 1.11/1.10E's other stale-bytes contributor, loader block 57 (target
`0x2001e888`, `byte_count` 67652), is a *genuine, unoverwritten* code block:
byte-for-byte identical between its own declared range and `LoadedMemory` at
every address (checked directly with `mem.read()` against the raw
byte-address, not through the sw-based accessor), independently confirmed
with `LoadedMemory.owner_runs()` showing 0 bytes owned by anything but block
57 itself. It was reported "stale" only because `tools/sharc_dev/sharcldr.py`'s
`L2_BYTE_LIMIT` (`0x20020000`, one 128 KB L2 SRAM bank) was too small:
`read_sw()`'s L2 fallback bounds-checks against it and returned `None` for
any sw past the bank boundary, while block 57 itself extends to
`0x2002f1c4` -- 61,900 bytes (91.5% of the block) past that old bound. The
ADSP-2156x hardware reference (`out/refs/adsp-2156x-hwr`) documents the real
size: "L2CTL0 contains 1M byte of RAM grouped into eight banks, 128K bytes
each and 96K bytes of boot ROM" -- the old bound modeled one bank of eight.
`L2_BYTE_LIMIT` is now `0x20100000` (the full 1 MB), and `tools/sharc_dev/sharcfn/`'s
`base_sw_note()` (which quoted the old bound in a diagnostic string) now
formats it from the live constants instead of a hardcoded literal. Block 57
stays in `KNOWN_CODE_BLOCKS`, now resolved end to end (0 stale bytes,
below). DN2's other out-of-window block, blk59 (target `0x200779f8`), now
has its *start* address inside the widened window too (it was previously
excluded for being past the old `L2_BYTE_LIMIT`), but it has no prior
characterization as code the way block 57 did (docs/findings/11's
correspondence method never named it) and is left out of
`KNOWN_CODE_BLOCKS` for that reason alone -- a known, unverified gap, not
re-added by this task. **[V][O]**

**Collateral, not fixed here:** `tools/sharc_dev/sharcspec/ghidra/gen_sleigh.py`
(SLEIGH language generation, not owned by this task) independently
hardcodes the *old* `0x20020000` L2 boundary in generated code (its own
`isl2` predicate, `addr_name < 0x20020000`) rather than reading
`sharcldr.L2_BYTE_LIMIT`; it is now inconsistent with the widened Python-side
bound. This affects only the Ghidra SLEIGH import path
(`tools/sharc_dev/sharc_import.py`'s own `ghidra_addr()`/`mapped_segments()` already
read the live constants and needed no change, confirmed by
`tests/sharc/analysis/test_sharc_import.py`), not this database/decode pipeline. Flagged
for whoever owns `tools/sharc_dev/sharcspec/ghidra/`. **[O]**

### DB_VERSION 12 -> 13, before/after **[V]**

Rebuilt all four private per-lane databases
(`uv run python -m tools.sharc_dev.sharcdb build out/sections/*/section_7_BLOB.bin`).
"Before" below is a faithful v12 rebuild from a clean checkout of this
task's changed files (not a stale number): its aligned-instruction/
stale-bytes counts reproduce the "One decode path" section's own already-`[V]`
table above exactly (e.g. dt2-1.16 51,032/2,269), confirming it is the same
baseline this section's "after" is measured against.

| image | aligned insns (before -> after) | functions (before -> after) | roots (before -> after) | reach (before -> after) | stale-bytes (before -> after) |
|---|---:|---:|---:|---:|---:|
| dt2-1.16 | 51,032 -> 48,760 | 1,228 -> 1,147 | 1,952 -> 1,872 | 67,137 -> 65,280 | 2,269 -> 0 |
| dt2-1.15C | 50,983 -> 48,711 | 1,227 -> 1,146 | 1,952 -> 1,872 | 67,058 -> 65,201 | 2,269 -> 0 |
| dn2-1.11 | 65,372 -> 63,100 | 1,157 -> 1,076 | 1,901 -> 1,821 | 59,454 -> 57,597 | 12,661 -> 0 |
| dn2-1.10E | 64,586 -> 62,314 | 1,151 -> 1,070 | 1,916 -> 1,836 | 58,965 -> 57,108 | 12,075 -> 0 |

`tools/sharc_dev/sharc_coverage.py --compare-decode-at` now reports 0 stale-bytes *and*
0 decode_at mismatches on all four images (previously 0 mismatches, nonzero
stale-bytes -- see the "One decode path" table above). Every image's drop in
aligned instructions is a few rows larger than its own stale-bytes count
alone (dt2-1.16: -2,272 vs. 2,269 stale rows; dt2-1.15C: -2,272 vs. 2,269;
dn2-1.11: -2,272 vs. 2,269 of block 1's contribution, block 57's own count
unchanged since it was never really stale; dn2-1.10E: -2,272 vs. 2,269) -- a
consistent -3-row edge effect from the two tiny 4-byte survivor spans no
longer contributing their own boundary-adjacent aligned rows once block 1
is not scanned at all, not a new discrepancy.

Coverage gaps (`tools/sharc_dev/sharc_coverage.py <image> --root 0x1c2b24` /
`--all-roots`):

| image | scope | gaps (before -> after) | not-executable instructions (before -> after) |
|---|---|---:|---:|
| dt2-1.16 | `--root 0x1c2b24` | 3 -> 3 (unchanged) | 3 -> 3 |
| dt2-1.16 | `--all-roots` | 6 -> 5 | 11 -> 5 |
| dt2-1.15C | `--all-roots` | 6 -> 5 | 11 -> 5 |
| dn2-1.11 | `--all-roots` | 10 -> 9 | 18 -> 12 |
| dn2-1.10E | `--all-roots` | 10 -> 9 | 18 -> 12 |

In every image, the removed gap is exactly the `provisional 26a` group at
`sw 0x1208e4/0x1208ff/0x12096d/0x12099c/0x1209b5/0x1209c4` (6 instructions)
-- block 1's dead bytes, gone because block 1 is no longer scanned at all.
The `--root 0x1c2b24` scope (DT2 1.16's render path) never reached block 1
and is byte-for-byte unchanged. All remaining gaps (`1c32b0`/`1c32b1`/
`1c32b4`, the L2-decimator `compute` gaps, DN2's extra `compute`/`form` gaps)
are real code, unrelated to this fix, and stay open per the existing gap
triage above. **[V]**

`uv run python -m tools.sharc_dev.sharc_symbols dt2-1.16` still resolves every symbol
to the same value as before this change (no `FAILED`/`missing`/`mismatch`
output) -- none of the profile's required symbols live in block 1's or
block 57's address ranges, so this is an unsurprising but checked pass.
**[V]**

**Golden.** Only `cov_all` (`sharc_coverage dt2-1.16 --all-roots --json`)
changed hash: `instructions` 46,010 -> 43,738, `gap_instructions` 11 -> 5,
distinct gaps 6 -> 5 (the removed `26a` group) -- exactly the coverage-gap
table above. `cov_render`, `run_frame`, `run_voice`, `trace_frame` and
`trace_voice` are byte-identical (none of their roots or traced paths ever
reached block 1). Updated with
`PYTHONPATH=tests uv run python -m tests.sharc.integration.test_sharc_golden --update`. **[V]**

**Files touched outside this task's listed ownership.** `tools/sharc_dev/sharcinv.py`
(`analyze_block()`, `build_inventory()`, `CODE_BLOCKS`) was edited because
the stale-input bug lives in its `analyze_block()`/`build_inventory()`
functions, which `tools/sharc_dev/sharcfn/`'s `load_context()` and
`tools/sharc_dev/sharcdb/`'s `_fill_database()` both call into for function
boundaries and call/jump/return sites -- fixing only this task's own files'
decode loop would have left `functions`/`edges`/`bblocks` still built from
block 1's stale bytes, an incoherent database. The edit is minimal (the
payload source inside `analyze_block()`, and removing block 1 from
`CODE_BLOCKS`); the module's own docstring and a comment referencing
`blk1@0x1201f8` as an example were left mostly alone (see the "Collateral"
note above). `tests/sharc/analysis/test_sharc_import.py` needed one test fixed (a
hardcoded `0xFFFF` L2-window-size literal tied to the old, narrower
`L2_BYTE_LIMIT`) to keep passing after the widening; `tools/sharc_dev/sharc_import.py`
itself needed no change (already reads the live constants). **[O]** for a
maintainer check that these out-of-list edits don't conflict with other
lanes' work on `tools/sharc_dev/sharcinv.py` (last touched by unrelated commits
`a286b20`/`8cccd2e`, well before this wave's work).

**Tests.** `uv run python -m pytest tests/sharc/integration/test_sharc_golden.py
tests/devtools/test_lint.py tests/devtools/test_types.py tests/sharc/analysis/test_sharcdb.py
tests/sharc/analysis/test_sharcfn.py tests/sharc/analysis/test_sharcimm.py tests/sharc/analysis/test_sharcldr.py
tests/sharc/analysis/test_sharc_disasm.py tests/sharc/analysis/test_sharc_coverage.py
tests/sharc/analysis/test_sharc_symbols.py tests/sharc/analysis/test_sharc_contract.py
tests/sharc/analysis/test_sharc_index.py tests/sharc/analysis/test_sharc_discover.py
tests/sharc/analysis/test_sharcwriters.py tests/sharc/analysis/test_sharcinv.py tests/sharc/analysis/test_sharc_import.py
tests/sharc/analysis/test_sharcflow.py -q --slow`: 444 passed, 5 subtests passed. **[V]**

## Type 7a MODIFY was missing its `(sw)`/`(nw)` scale bits **[V][C]**

Found while tracing the voice render loop `FUN_1c4f81` (finding 06, "One
voice renders correctly"): a plain `I = MODIFY(I, imm)` should leave the
loop's sample and coefficient pointers 2 and 24 bytes apart per iteration,
but the decoded immediates were all 4x too large.

Type 7a `MODIFY` has two scale bits the decode table lacked: `w` at bit 39
and `l` at bit 23. The PRM's "BH (Type 7a)" encode table (p.13-48) gives
`(w,l) = (0,1)` -> `(sw)` (the modify value is scaled x2, half-word/int16
addressing) and `(w,l) = (1,0)` -> `(nw)` (unscaled, normal-word
addressing); `(0,0)` is the plain, undecorated form our table already
covered, scaling every modify by 4 (normal-word units) regardless of these
two bits. In the render loop, `0x1c50af`, `0x1c50ca`, `0x1c50cf` and
`0x1c50d4` are `(sw)`; `0x1c50b6` is `(nw)`.

Checked against the raw bytes independently of the tracer: decoding each of
the five sites' 48-bit words by the PRM's bit positions alone reproduces the
`(sw)`/`(nw)` suffix and the corrected immediate scale, and matches what
execution needs for the sample pointer (2-byte int16 steps) and the
coefficient-table pointer (24-byte row steps) to land where the "One voice
renders correctly" section shows they must. **[V]**

## Conditional Type 7a: the refusal, the fix and the audit (2026-10-13) **[D]** **[C]**

Stock DT2 1.17 halted the DSP on `PTN` + `TRIG 3`/`4` then PLAY: `native-trap: sharc_core.forms_dag._type_7a:63: stop:
unsupported Type7a predicate at 0x1cbc93` (finding 17). The instruction is `IF EQ modify(I4, M4)`, cond 0, the only
conditional Type 7a with a condition other than `NOT SV` (0x17) in the audio code.

- **Cause.** `_type_7a` accepted cond `0x1F` (always) and `0x17` only, and refused any compute in a conditional
  form. The generated cores are transpiled from this function, so every core stopped at the first other condition.
- **Rule (PRM p.14-47, "Type 7a ISA/VISA (cond + comp + index modify)").** "If a condition is specified, it affects the
  entire instruction": compute and index modify alike. SISD tests PEx's condition. In SIMD the compute runs in each
  PE whose own condition is true; "the index register modify ... occurs based on the logical ORing of the outcome of
  the conditions tested on both PEs". Implemented in `tools/sharc_dev/sharc_core/forms_dag.py` for every condition the core
  evaluates (`_predicate`, `_predicate_pe`): a modify whose condition is false in every active PE is skipped (no
  circular wrap, no compute); PEx unresolved with SIMD unresolved keeps the old behaviour (Unknown outcome); a
  conditional compute with SIMD unresolved is refused (`unresolved SIMD conditional Type7a compute`). Tests:
  `tests/sharc/semantics/test_sharc_type7a_cond.py` (SISD true/false, `NE`, SIMD OR of the two PEs, per-PE compute, unconditional SIMD
  compute) and the changed fail-closed cases in `tests/test_sharc_trace.py`. Golden hashes
  (`tests/sharc/integration/test_sharc_golden.py`) did not change.
- **Audit of the other refusals reachable in the images.** `forms_dag` and the sibling forms refuse conditions the
  core cannot evaluate (`_predicate` has no FLAGx_IN, BM or LCE codes: cond 9 to 12, 14, 15, 25 to 28, 30). The
  sharcdb instruction table (`insn`, aligned, inside a function) for dt2-1.16, dt2-1.17, dn2-1.11 and dn2-1.12 holds
  no instruction with any of those conditions, in any form: the codes that occur outside `0x1F` are 0 to 4, 6 to 8,
  13, 16 to 18, 20, 23, 24 and 29, all evaluated by `_predicate`/`_predicate_pe`. Conditional Type 7a in the four
  images: one `IF EQ` (`0x1cbc93`) and `NOT SV` eleven times (DT2) or twice (DN2); none carries a compute.
  The refusals "unsupported Type7a circular modify" (L nonzero and B, I or M not concrete) and "unsupported Type6a
  conditional SIMD" (SIMD unresolved) remain; they are reachable only from a symbolic trace, not from the concrete
  driver. **[D]**
- **Proof.** Cores regenerated into `gen-next` dirs (DT2 `out/native/dt2-audio/gen-next`, DN2
  `out/native/dn2-audio/gen-next`, shared `out/native/shared-core/gen-next`, each with PROVENANCE.txt); the generated
  lowering `packages/emulator/sharc/src/fast/lower/gen_forms.rs` regenerated (`g_7a` only), and the workspace Rust tests pass
  (they run the fast tier against the interpreter on random encodings of every generated form, conditions included).
  `tests/patchkit_smoke` `test_play_in_patterns_3_and_4_runs_clean`: DT2 1.17 and 1.16, PLAY after `PTN` + `TRIG 3`
  and `TRIG 4`, no halt. Control run with the old core: the same test fails with the old message. A deliberately
  undecodable word at `0x1cbc93` (`dsp.patch`) still halts, so the detector keeps its teeth
  (`test_a_dsp_halt_is_a_failure`). DN2 pins, 61 scenario oracles, no-AOT oracles, tier-quick (20 of 20 equal) and the
  DT2 tone, oracles and tier-quick (9 of 9 equal) all pass against the `gen-next` cores (`SHARC_GEN_DIR=...
  tools/devtools/verify.py --only ...`, one device per run because `SHARC_GEN_DIR` is global). PyPy is not installed on this
  machine, so `tests/devtools/test_pypy.py` (which includes `test_sharc_trace.py`) could not run here **[O]**; the change uses
  only constructs the transpiler already accepts (it transpiled with 0 untranslated functions).

## A normal-word access through a byte-address pointer matches the manual **[D]**

Question from finding 20 (selcc): a default (no `bw`/`sw`/`lw`) `DM` access with an index register in byte space
(`0x2xxxxxxx` and the other byte aliases) is a normal-word access in our core, and a post-modify by M adds `M * 4`.
Does silicon do the same? Public programming reference (`sc58x-2158x-prm`, printed pages):

- Table 6-12 "Accesses Causing Illegal Address Space Interrupt" (pp.6-31, 6-32): the normal-word access row is Legal
  in long-word, normal-word, short-word and byte space, and footnote 1 says: "Normal word is the default access size.
  No interrupt will be raised for any address space. Normal word sized access would be done in byte address space."
- Table 6-2 "Legal and Illegal Accesses to Byte Space With or Without Address Scaling" (pp.6-10 to 6-12), rows
  `Rm = dm(In, mod)` and `Rm = dm(mod, In)`: with the I register in byte space, the access is at `In` and the
  modifier is `scaled_mod = mod << 2` (and `scaled_len = Ln << 2` for a circular buffer); in non-byte space it is
  unscaled. The text on p.6-9: "Scaling occurs automatically for pointers in the byte-addressed space."
- Table 6-13 (p.6-33): a SISD normal-word access has no alignment restriction; in SIMD it needs a normal-word
  boundary.
- Our core: `_access_modifier_scale` returns 4 for a normal-word access when the address is concrete and not in a
  normal-word alias (`tools/sharc_dev/sharc_core/addressing.py`, from the product datasheet tables) and `assume_nw32` holds (it
  is true in `tools/sharc_dev/sharc_run.py` and the generated cores), 1 for an address inside a normal-word alias; the circular
  length is scaled alike (`_post_modify_index`). That is the manual's rule. selcc's `0x2xxxxxxx` pointers with index
  steps of 4 therefore address the same words on silicon.
- Not covered: banks with the IMDW (40-bit) bit set scale by 6 (PRM p.7-7); no DT2/DN2 code path in this work sets it
  (the voice and master code runs on 32-bit blocks), but the register is not read by anything we emulate **[O]**.
  Misaligned long-word accesses are forced to the lower aligned address on silicon (p.6-32) and are not modelled.

## Type2b and the 16-bit Type2c: the decoder no longer takes Type2b **[V][C][O]**

The PRM's Type2b figure fixes bits 47..39 at `110000000` (mask `0xff8000000000`, value
`0xc00000000000`). Every first parcel in `0xc000..0xc07f` matches it, and every one of those
parcels is also a 16-bit Type2c short compute (`Rn = Rn + Rx`, opcode `0000`). The table
gave Type2b more fixed bits, so the raw pick was the 32-bit reading. The confidence
lookahead had to pull each case back to 2c.

The lookahead did not catch every case. selache's selas assembled a 16-bit `R2 = R2 + R3`
(`0xc023`) into a dn2_firmware DSP routine. The executor ran it as a 32-bit 2b, with the next
parcel as its low compute half. Every later instruction lost its boundary.

**What the silicon does** **[V]**:

- Three parcels sit in code that runs every audio block on the instrument: DN2 1.11
  `sw 0x1c0e13` (`0xc020`) and `sw 0x1c4f4a` (`0xc029`), and DT2 1.16 `sw 0x1c32ab` (`0xc011`).
  The source is dn2_firmware's `docs/for-digikit-sharc-runner-dn2.md`, section 10. A 32-bit reading desyncs the
  stream at each one. The instrument plays.
- On a Digitone II, one build rewrote a 16-bit `0xc018` in a mod's DSP code as a 32-bit
  form. It behaved the same as the build with the 16-bit form (dn2_firmware
  `docs/waverider-dsp-compare.md`, `stages-fix`).
- selache's selas, an independent assembler, never emits a 2b at this prefix. It encodes a
  32-bit full compute with the PGR prefix `000000011` (`r1 = r0 + r1` is `0x0180 0x1101`).
- In DN2 1.11, all 31 decoded 2b were either the two misreads above or sat in one block
  (`0xb8f69a..0xb96bc4`) that decodes like data.

**The change** **[V]**: `build_table.py` has `NOT_VISA = {"Type2b"}`. Type2b stays in
`decode_table.json` with `"visa": false`, so neither the Python decoder (`sharc_isa`, mode
`visa`) nor the generated Rust table (`sharc_decode_rsgen.py`, `sharc-rt`) takes it. A
`0xc000..0xc07f` first parcel is now a 2c on the raw pick, with no lookahead. The 2b arms of
`decode.rs`'s width resolution no longer run, but they stay in the code.

**Checked** **[V]**: `sharc-rt` 17 of 17. The Python SHARC tests on Windows: 1,645 passed, and 22
failed exactly as they fail on unmodified main (paths and missing peers). The six tests built
around the 2b/2c collision now assert the 2c on the raw pick, and they keep the firmware
shapes (DT2 1.16 `sw 0x1c4b99`, `sw 0x1c4e10`). selache and the native decoder now agree on
`0xc001`.

**Open** **[O]**: whether the PGR prefix `000000011` is the real Type2b. No firmware instance
of it is known, so the table does not take it.

## Selache shift-immediate encodings (2026-10-21) **[D]** **[C]**

A Type 6 shift-immediate instruction (forms `6a_mem`, `6a_nomem`, `6b_shiftimm`) carries its operation in the ShiftImm
opcode (`shiftimm[22:16]`; PRM Table 17-9 "SHIFTOP/SHIFTIMM Encode Table", bits 21-16) and, for a signed shift, an 8-bit
count in `shiftimm[15:8]` whose sign extension is the 4-bit field `dataex[3:0]` (frame bits 30-27: `0x78` in byte 2 of the
48-bit word in Selache parcel order). It extends bug 2 of the Selache cross-check above, which found the LSHIFT case only.

**Stock rule.** In every aligned instruction inside a function of the stock DSP images, `dataex` is 0xF for a negative count
and 0 for a positive one, for the opcodes 0x00 LSHIFT, 0x01 ASHIFT, 0x08 OR LSHIFT and 0x09 OR ASHIFT: DT2 1.17 721 `6b`
and 21 `6a_mem`, DN2 1.12 651 `6b` and 18 `6a_mem`, no exception. Every opcode in use is in Table 17-9 (DT2: 0x00, 0x01, 0x08,
0x10, 0x12, 0x14, 0x19, 0x1D, 0x1F, 0x30-0x33). The exceptions in the database (3 on DT2, 2 on DN2) are unaligned decodes
outside any function. `tests/patchkit/test_pk_sharc.py::test_every_stock_shift_immediate_passes_the_check` re-checks this.

**Selache at the pinned revision (`2b26d3b`), one line each:**

| Source | Selache writes | Table 17-9 |
| --- | --- | --- |
| `R0 = LSHIFT R11 BY -16;` | `023e0000f00b`: opcode 0, `dataex` 0 | `dataex` 0xF: `023e7800f00b` (stock form) |
| `R0 = LSHIFT R11 BY 16;`, `FEXT R11 BY 4:8`, `BSET`/`BCLR`/`BTGL`/`BTST R11 BY 5` | correct | |
| `R0 = ASHIFT R11 BY ±3;` | opcode 0x20 (100000) | ASHIFT is 000001; 100000 is not in the table |
| `R0 = R0 OR LSHIFT R11 BY 3;` | opcode 0x00, a plain LSHIFT | 001000 |
| `R0 = R0 OR ASHIFT R11 BY -3;` | opcode 0x20 | 001001 |
| `R0 = FDEP R11 BY 4:8;` | opcode 0x13, FDEP (SE) | 010001 |
| `R0 = R0 OR FDEP R11 BY 4:8;` | opcode 0x11, FDEP | 011011 |
| `R0 = FEXT R11 BY 4:8 (SE);`, `FDEP ... (SE)` | `0f0000000000` (another form) | 010010 / 010011 |
| `R0 = ROT R11 BY 3;` | refused by Selache | |

Each wrong word still decodes to a valid-looking form, so the re-decode gate of `pk_sharc.verify` does not see it, and the
emulator reads only the low 8 bits of a count.

**Silicon evidence (external, [M] there).** A public project, `angellinares/dn2_firmware_explore`
(`docs/waverider-dsp-compare.md`, section 11, 2026-09-29), ran its own Selache-assembled code on a Digitone II 1.11: the DSP
stopped on the first call (ILOPI, raised by look-ahead), and passed every emulator gate, including digikit's runner. The cause
was four `LSHIFT ... BY -n` without `dataex`; setting the six bytes to the stock form made it play. Not measured here: what the
silicon does with opcode 0x20 **[O]** (undefined; our core raises "unsupported ShiftImm opcode").

**The gate (`packages/patchkit/src/build/sharc_obj.rs`; before phase 6a `tools/patchkit/pk_sharc.py`).** `fix_shift_counts` sets `dataex` of every signed shift in an object after Selache
(assembly and selcc alike), so `LSHIFT ... BY -n` is allowed again. `ENCODE_HAZARDS` refuses the source forms Selache gets
wrong (ASHIFT, OR LSHIFT and FDEP by an immediate, FEXT by an immediate with (SE)); selcc's assembly (`-S`) passes the same
check. `verify` fails an object with an undefined ShiftImm opcode or a signed count without its sign extension. A shift count
in a register assembles correctly: in the 16-bit compute (`2a_short`), selas writes the shiftop (compute bits 19-12) as
0x00 LSHIFT, 0x04 ASHIFT, 0x08 ROT, 0x20 OR LSHIFT, 0x44 FDEP, the values of Table 17-9's shiftop column (2026-10-21);
selcc uses that form. None of the 35 tracked assembly files uses a
refused form or a negative count.

**Fixed in the fork [D]** (2026-10-30, ElekLabs/selache branch `fix/encodings`, commit "selas: encode immediate shifts per PRM Table 17-9"):
the parser now passes the Table 17-9 opcode (ASHIFT 0x01, ROT 0x02, OR LSHIFT 0x08, OR ASHIFT 0x09, FDEP 0x11, FEXT/FDEP (SE) 0x12/0x13,
OR FDEP 0x1B, OR FDEP (SE) 0x1D), the encoder sets `dataex` 0xF for a negative LSHIFT/ASHIFT/OR count, and `(SE)` is no longer taken for a
label. patchkit dropped `fix_shift_counts` and the `ENCODE_HAZARDS` refusals; its `verify` still checks every object against Table 17-9.
The 35 tracked sources assemble to the same bytes as before (none uses a refused form), so only the unit tests exercise the new words.

**Open [O].** The other Selache forms with an immediate have not been compared byte for byte with stock. The external
project's rule fits: "decodes the same" is not proof of an encoding; compare the exact bytes with stock for every instruction
form that carries an immediate.
