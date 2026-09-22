# SHARC+ ISA and decoding

The SHARC+ instruction tables built from the public ADI manuals, the run of Type-NN decode corrections made against the generated Ghidra language, and the selache cross-check.

## SHARC+ instruction tables from the public ADI manuals **[D][O]**

- `tools/sharcspec/` builds the SHARC+ instruction decode table from two
  public Analog Devices manuals: the bit-layout figures in the SHARC+ Core
  Programming Reference Rev 1.5, read from the PDF's vector drawings, checked
  bit by bit against the classic SHARC Processor Programming Reference Rev
  2.4. The method and the manual errata it found are in
  `tools/sharcspec/README.md`; the documents are listed in
  `docs/sharc/SOURCES.md`; the results are in `docs/sharc/SPEC-FINDINGS.md`.
  It came from Em's separate sharc-spec work. **[D]**
- On the 1.15C DSP main program (104,848 bytes at `0x28382670`) its decoder
  leaves 1.1% of words unknown. `tools/sharc_disasm.py` stops at its first
  unknown word, 4.4% in; forced to continue, it cannot name 9.6%. One cause is
  traced: our `15b` entry fixes 3 bits where the manual fixes 7, so 1,928
  words decode as `17b`. **[D]**
- `tools/sharc_visa_tables.py` now loads `tools/sharcspec/decode_table.json`
  and picks a form by the same rule as the sharc-spec decoder, and
  `tools/sharc_disasm.py` still stops at the first word it cannot classify.
  `tools/sharcldr.py --main` writes the final application's code, and
  `tools/sharccompare.py` sweeps a region with both decoders. The main
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
  `tools/ghidra/gen-sharc-slaspec.py` still expects the old form names
  (`8a`, `9a`, `9b`) and fails on the rebuilt table; the SHARC language is to
  be regenerated from `tools/sharcspec/ghidra/gen_sleigh.py`. A second
  check rebuilt both regions from the boot-stream blocks, counted the mask
  bits, and reproduced the sharc-spec decoder's counts, first unknown and
  last instruction with its own sweep. **[V]** The old decoder's numbers
  come from `tools/sharccompare.py` alone. **[D]**
- `tools/ghidra/install-sharc.sh` now generates the language from
  `decode_table.json` with `tools/sharcspec/ghidra/gen_sleigh.py` and
  installs `SHARC_VISA:LE:32:default`; the generated module equals the copy
  already installed from the sharc-spec work, except the stack pointer, now
  I7 as the call convention in `docs/sharc/structure-1.16.md` uses. The old
  generator and `tools/ghidra/SHARC/` are removed; the installed
  `Processors/SHARC` stays for the `dt2_SHARC` program in `~/ghidra-projects/dt2`.
  The code space has wordsize 2, so byte offset `0x382670` shows as
  short-word address `0x1c1338`. **[D]**
- `tools/sharc_import.py --seed-calls --analyze` imports section 7 into
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
  program starts at SW `0x1c12e2` and is 105,016 bytes (`tools/sharcldr.py
  --main`). It also loads 54,792 bytes at `0x8045a6c8` and 283,032 bytes at
  `0x80467cf4` in external memory; Digitakt II 1.16 loads 3,316 bytes at
  `0x8045a6c8`. `tools/sharc_import.py --seed-calls --analyze` imports it as
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
- `tools/sharcspec/ghidra/gen_sleigh.py` now emits Type25a as `call`. Its
  delay slots are not modelled: they follow the call in the listing.
  `tools/sharcflow.py` lists the calls, indirect calls and returns with their
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
- `tools/sharcspec/ghidra/gen_sleigh.py` now gives each of these forms two
  constructors: cond TRUE keeps the old p-code, and any other cond tests
  `condition(cond)`, a user-defined p-code op, and falls through when it
  does not hold. `tools/sharcpcode.py` measured the old and new language back
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

- `tools/sharcpcode.py measure --ghidra` writes `OUT/<image>.sqlite`: our
  decoder at every decodable offset of the main program (form, fields, depth,
  aligned, computed target, the pypcode lift) and Ghidra's instructions,
  references, bookmarks, functions, decompiled C and decompiler warnings. Two
  runs compare with sqlite's `ATTACH`; the queries below are
  `tools/sharcpcode.sql`. **[D]**
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
  crossing resolver in `tools/sharcspec/ghidra/gen_sleigh.py` against the PRM.
  Not checked. **[O]**

## Type21a was swallowing the two words after it **[C][D][O]**

- Corrects the open item above. `Type21a` matched any first word `0x0000` to
  `0x007f` and constrained nothing in the other two words, so it absorbed the
  short instructions that followed: 904 matches in 1.16, only 44 of them the
  all-zero word the PRM draws (1.11: 31 of 898). `tools/sharcspec/build_table.py`
  now takes every bit from the figure for Type21a and Type21c, and the
  left-over first words are the provisional 16-bit `Type21p_undoc16`
  (`docs/sharc/SPEC-FINDINGS.md` 3.7). **[D]**
- Measured with `tools/sharcpcode.py measure --ghidra` before and after, both
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
  `tools/sharcspec/audit_bits.py`: Type22a (idle/emuidle) and Type26a (`sync`)
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

- `tools/sharcspec/compare_sources.py` diffs two sources, the PRM figures and
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
  `tools/sharcspec/build_table.py` 273-275 declines to fix a bit the PRM shades
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

- `tools/sharcfields.py`, new here, is the mirror of
  `tools/sharcspec/audit_bits.py`: where that lists bits a PRM figure prints
  that the table does not fix, this tallies what values the fields the table
  *declares* actually take across the aligned instructions of both images, and
  flags any field sitting on bits the classic PGR grid fixes. Three fields
  qualify. One is large. **[D]**
- `Type19a` fixes six bits, `000101`, where its neighbours in the opcode space
  fix eight or nine: `Type18a` `00010100`, `Type19a_bitrev` `000101101`,
  `Type20a` `00010111`. The PRM's Figure 17-2 shades only six and declares bits
  41-40 a SHARC+ field `sc[1:0]` and bit 39 a field `w`, while the classic grid
  and all three older manuals fix bits 44-40 at `10110`
  (`tools/sharcspec/classic.json` "Type 19a", pattern `000101100...`). **[D]**
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
  `tools/sharcspec/build_table.py` -- an override there, not an edit to
  `figures.json`, which `extract_figures.py` regenerates. The provisional 48-bit
  `Type19p_undoc48` (`00010101`, eight fixed bits) takes the gap. **[V]**
- `tools/sharcpcode.py compare out/sharcpcode/t23 out/sharcpcode/t24` reports
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
  across the two images, the same 757 `tools/sharcfields.py` found. `18a`,
  `20a` and `19a_bitrev` are untouched, `19p_undoc48` has no undecoded words and
  no length mismatches, `tools/sharccompare.py` still has both decoders agreeing
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

- `tools/sharcfields.py` guessed a form's classic.json table from its name, and
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
  `tools/sharccompare.py` still has both decoders agreeing on all 22,886
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

- `tools/sharccompare.py` had them agreeing on all 22,886 offsets of 1.16 but
  differing by one on Digitone II 1.11: `unknown only in ours 1`. It is the last
  word of the image. `out/sharc/dn2-1.11-main.bin` is 105,016 bytes and ends on
  an all-zero word at byte offset `0x19a36`, where only two of the six bytes a
  48-bit instruction needs exist. **[V]**
- Both decoders zero-pad the missing words before matching, and an all-zero
  frame trivially satisfies `Type21a`, whose figure fixes every one of its 48
  bits. `tools/sharc_disasm.py` 108 checks the matched form's width against how
  many real words were read and refuses; `tools/sharcspec/sharc_decode.py`'s
  `linear()` had no such check and reported a phantom 48-bit instruction sitting
  on four bytes that are not in the file. It now makes the same check. **[V]**
- It predates today's work: the same single offset and form reproduce with
  HEAD's committed `decode_table.json` in a scratch tree, so none of
  `Type8a_abs`, `Type8a_rel`, `Type8p_undoc48`, `Type19a` or `Type19p_undoc48`
  is involved. 1.16 never showed it because its image does not end that way.
  **[V]**

## Every measurement here was of a stale language until it wasn't **[V][C]**

- `tools/sharcpcode.py measure` compiles the slaspec sitting in
  `tools/sharcspec/ghidra/SHARC_VISA/data/languages/` (`sharcpcode.py` 66,
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
  `tools/sharcfields.py` and `tools/sharccompare.py` all read
  `decode_table.json` directly. Lengths agreed by luck -- the stale `Type19a`
  and `Type8a_rel` are 48 bits wide, like the forms that replaced them -- which
  is why nothing tripped. **[V]**
- `tests/test_sharc_pcode.py` 56-67 regenerates into a temp directory on every
  run, excluding the `SHARC_VISA` output tree from its copy. That is why the
  test caught the unreferenced-table warning and four measurements did not.
  **[V]**
- Measured properly, after `tools/ghidra/install-sharc.sh` and a fresh
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
`tools/sharc_disasm.py` are **little-endian per 16-bit parcel** (the boot-stream
convention). Fed unswapped, selache desyncs after the first instruction. Swapped,
it tracks our decoder for hundreds of consecutive instructions.

Measured over two 4000-byte windows at `0x1c24e9` and `0x1c71ec` (~1660
instructions, all three VISA widths present: 16/32/48-bit): ~1.4% unrecognised
by selache, 22 of 24 hand-tabulated instructions agree byte-for-byte (the two
non-agreements are one mutual "uncertain" and one selache table gap, neither a
contradiction). Assembling 15 hand-transcribed instructions with `selas`
(`-proc ADSP-21569`) and comparing to the image: **14/15 byte-identical**.

This is meaningful three-way corroboration of `tools/sharc_visa_tables.py` --
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
fused compute+DM form, a delayed jump) decodes in `tools/sharc_disasm.py`
to the same eleven instruction boundaries, and runs in
`tools/sharc_trace.py` with all seven expected results (the jump target was
patched by hand, as `selas` leaves relocations unresolved).
`tools/selasm.py` assembles a file, swaps parcels and prints both listings.
Two more Selache decode bugs, both checked against the bits of code we
assembled: `decode_32_group4()` merges two adjacent 16-bit Type3c
instructions (`90 34 90 15`) into one bogus 32-bit instruction and drops
the store; `decode_type3()` reads the fused form's M register as
`bits(39,38)+4`, where the field is bits 40:38 with no offset, so it
reports M5 for M1. **[V]**

## Only 22% of the image has semantics, and the emulator runs the rest as no-ops **[D][O]**

The generated SLEIGH emits an empty `{}` body, never `unimpl;`, for every
constructor without hand-written semantics
(`tools/sharcspec/ghidra/gen_sleigh.py`, `Constructor.emit`). pypcode therefore
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
byte-addressed space in the language, measured with `tools/sharcpcode.py`.
**[O]**

## DM literals now reach their own bytes **[V][C]**

**[C]** The doubling in the previous section is fixed. `gen_sleigh.py`'s
`dm_byte_addr_to_ram_unit` translates a DM byte address to a `ram` unit
before each DM access: `unit = (addr >> 1) + isl2 * 0xF0B80000`, where `isl2`
is 1 inside the L2 byte window `0x20000000..0x20020000`, so L2 lands on the
importer's short-word alias (`0x20000010` -> unit `0xB80008`). Both current
DM sites use it, the Type14a scalar forms and the exact Type3b reader; PM
accesses are unchanged. `tools/sharc_import.py` now shifts only the L1 alias
window `0x28000000..0x28400000` down by `0x28000000`, so external memory sits
at its own address (`0x82a00008` stays `0x82a00008`, unit `0x41500004`).
All 646 Type14a DM literals in the DT2 1.16 main program are even, so the
shift loses nothing. `tools/sharcpcode.py compare` of the language before and
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
from the public manual. pypcode and `tools/sharc_disasm.py` agree on the
length of all 22,668 aligned instructions. 105 instances load into the I
register they address through (`I4 = DM(M5, I4)`); all are pre-modify, so no
update competes with the load. In the emulator a 15b load, a 15b store and a
4a post-modify store behaved exactly as predicted. **[V]**

Coverage measured with `tools/sharc_worklist.py` rose from 22.09% to 51.02%
of the image and from 31.19% to 72.94% of `FUN_001c18a6`.
`tools/sharcpcode.py compare` shows no decompile failure or timeout;
decompile time rose from 1.6 s to 6.9 s, and the constant-folded guards
produce 6,534 "Removing unreachable block" warnings. Stepping `FUN_001c18a6`
now runs 210 instructions and stops mostly at `compute` (139 of 301 faults)
rather than at missing forms. **[D]**

Every indexed store is now visible to the dataflow queries. A whole-image
`ghidraq stores 0x252658` returns 3,692 `computed-or-unresolved` candidates in
396 functions, where it returned none before. By address root they are led by
I6 (2,021), addresses that come through a load (1,338), I4 (135) and I5 (73).
None is resolved to `0x252658` yet. **[D][O]**
