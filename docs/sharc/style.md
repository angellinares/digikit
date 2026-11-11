# SHARC assembly style for digikit modules

Status: draft for step 7. Marks: **[D]** read once in the repo, **[O]** proposal or guess. The assembler accepts what
`modules/machines/pluck/src/dsp/pluck.s` uses (`//` comments, `#include`, `#define`, `.SECTION`, `.VAR`, `.GLOBAL`,
`.EXTERN`, `LCNTR = n, DO label UNTIL LCE`). Anything beyond that (for example `#define` as a register alias, `.equ`)
must be tried once with the assembler before the guide makes it a rule **[O]**. Note from `sdk/dsp/contract/dk_machine.inc`: the
assembler upper-cases macro text, so write numbers as decimal after `+` (the generator already does this).

## 1. File layout

One `.s` file per module (later, one per SDK kernel in a DSP folder under `sdk`). Order:

1. Header comment: what the module is, the state layout, the block algorithm in plain maths (pluck does this well).
2. `#include "dk_machine.inc"` (or `dk_fx.inc`; never both in one file, they define `DK_BLOCK` twice).
3. Module `#define` constants: `PK_STATE`, `PK_VOICES` (prefix with the module's two or three letters).
4. `.SECTION/PM seg_pmco;` code, then `.GLOBAL` and `.EXTERN` lines.
5. Private helpers first, entry points last or first, each with a header comment: inputs, outputs, clobbers.
6. `.SECTION/DM seg_dmda;` data: `.VAR` tables, then the descriptor (`_xx_desc`) at the end.

Label rules:
- Public: `_xx_render`, `_xx_note_on`, `_xx_desc`. Private: `_xx_name`. Loop labels: `_xx_name_loop`, `_xx_name_end`.
- State words: define offsets once, `#define XX_W 0`, `#define XX_LP1 3`, and write `DM(XX_LP1, I3)`.
  Pluck uses bare numbers (`DM(11, I3)`) with a comment table in the header. **Difference:** I recommend names.

## 2. Registers: roles and aliases

Fix one role per register in a module, and say it in the header. Proposed default map for a block kernel **[O]**:

| Registers | Role |
| --- | --- |
| I3 | state record base (`DM(OFF, I3)` is the only use) |
| I0 | output pointer (post-modify M6 = +1) |
| I1, I2 | input or delay-line pointers |
| I4, I5 | scratch pointers |
| M5 = 0, M6 = +1, M7 = -1 | constants set by the adapter; use as post-modify, never write |
| R0-R3 | scratch and results |
| F4-F7 | coefficients (loaded once before the loop) |
| F8-F15 | running state and loop temporaries |

Alias with `#define` named after the role, in uppercase so they stand out (for example `#define FB F5`) and
undefine at the end of the routine. Do not alias M5/M6/M7. Use the F-name for float use and the R-name for integer or
raw loads of the same register (the instruction decides the type, as in pluck's `R5 = DM(12, I3)` then `F1 = F0 * F5`).

Rules:
- Load coefficients into registers before the loop. Keep state in registers across the loop and store it after.
- Prefer `DM(offset, Ireg)` (immediate offset, no modify) for fixed fields. Pluck also uses `DM(I1, M5)` as "read, modify by
  0"; both work because M5 = 0. The immediate form does not depend on M5 and is easier to read.
- Do not write I6 to I15, M0, M5 to M15, L0 to L7, or MODE1 (see section 5). B3 and M1-M4 may be changed.

## 3. Comments

- `//` only. One comment per line of intent, not per instruction. Put maths in the header.
- End-of-line comments name the value, not the operation: `F1 = F1 + F8;      // lp`, not `// add`.
- Mark the loop's pipeline stage when a loop is software pipelined: `// stage 1: load x[n+1]`.
- Cycle comments: `// 5 instructions per sample`. State count type: counted, or estimated.
- Units and ranges in constants: `R1 = 0x39aaaaab;   // 1 / (256 * 12)`. A float literal in hex needs its value
  in the comment (pluck does this). If the assembler takes float literals, prefer them **[O]**.
- A reference to the manual goes in the header of a routine that uses a special feature
  (`// SIMD: PRM, Processing Elements, SIMD Mode`).

## 4. Block loop layout

```
    // setup: pointers, coefficients, state; no loads in the loop that can be done before it
    LCNTR = DK_BLOCK, DO _xx_loop UNTIL LCE;
        ...                          // body, 1 to n instructions
_xx_loop: last body instruction;     // the label is on the last instruction of the loop
    // epilogue: store state back, peak or counters, return
```

- Count: use the constant `DK_BLOCK` (32) as an immediate. Use a register for a variable count.
- Last instruction: no jump, no call, and no other loop end in the last slot (Program Sequencer, "Restrictions on
  Ending Loops"). A 1-instruction or 2-instruction loop has more restrictions on some cores; test it first (see
  "VISA-Related Restrictions on Hardware Loops") **[O]**.
- Pack multifunction instructions: the multiplier and the ALU in one instruction, with a data move in the same
  instruction when it does not use the operands' registers in a conflicting way (Processing Elements,
  "Multifunction Computations"; "Multifunction Input Operand Constraints": the multiplier takes its operands
  from R0-R3 and R4-R7, the ALU from R8-R11 and R12-R15 in the multifunction forms).
- Overlap the load of the next input with the compute of this one (software pipelining). Put the prime
  load before the loop and the last stores after it. Write the stages in a comment.
- Use post-modify with M6 (+1) for streams. For a delay line use a circular buffer (B and L set, I post-modifies);
  restore L to 0 before return. Avoid index arithmetic with masks (pluck: 8 of 23 instructions are address work).
- Do not use `PASS` to rotate state when a different register can be used in the next iteration (unroll by 2 to
  swap names). Pluck uses `F10 = PASS F0` style copies: 3 of 23 instructions **[D]**.

## 5. The module register contract

From `<dk/machine/machine.h>`, `<dk/fx/fx.h>` and finding 45 **[D]**:

- Entry: the adapter sets `I13` to the return address and jumps through `JUMP (M13, I12)`. The module returns with
  `JUMP (M13, I13)`. Do not use `RTS` or the frame helpers unless the module builds its own frame.
- Arguments: R4 = state, R8 = input record (`dk_voice_in` or `dk_fx_in`), R12 = parameter words. Result R0
  (`DK_OK`, `DK_DONE` or a negative fault).
- May clobber: R0-R15, I0-I5, M1-M4, B3.
- Must keep: I6-I15 (frame and stack), M0, M5-M15, L0-L7, MODE1.
- Constants: M5 = 0, M6 = 1, M7 = -1, M13 = 0, M14 = 1, M15 = -1 hold at entry. The wrapper guarantees them (the
  stock code never writes them; finding 45). Never write them; use them for post-modify.
- MODE1 (entry value 0x39011cf8 on both devices): SIMD (PEYEN, bit 21) is off, TRUNCATE and ALUSAT are off,
  RND32 is on, circular buffering (CBUFEN) is on, and the six secondary register-bank selects are set. The module
  runs on the secondary register files, in a software-interrupt context.
  - To use SIMD: set PEYEN, wait the pipeline gap (a `NOP`-class instruction after a MODE1 write), run, clear PEYEN
    before return. Manual: "SIMD Mode"; the stock copy clears it before the return.
  - Do not clear RND32 (the float result rounding assumed by the rest of the DSP).
- Circular buffers: set B and L of the DAG register you use, restore L = 0 at the end. L0-L7 are preserved by contract,
  so save them if you change them **[O]**. **[C]** Earlier text said the stock code never writes B or L. The stock voice
  renderer sets B3/L3 and B4/L4 and restores L = 0 and B3 (DT2 1.17 around 0x1c508a-0x1c532b); only the FX units
  avoid them (finding 53).
- Hardware loops: no loop may be active at entry (H2/H3 hooks: no DO loop is active). Do not leave a loop with
  the loop stack unbalanced.
- Level: output `|x| <= DK_LEVEL_BOUND` (8.0); more is a fault.
- State: `state_bytes` per voice, the first `state_clear_bytes` zeroed at note-on. All state in the record; no
  hidden globals (pluck's shared RNG `_pk_rng` is an exception, noted in its header).

## 5a. Rules from the manuals

Read in the SHARC+ programming reference **[D]**. Items marked **[O]** are not measured in the emulator or on hardware.

- Loops: a block loop has 4 or more instructions **[D]**. Shorter loops have extra restrictions; test them first **[O]**.
- Circular buffers: set B/L only on I0-I5. Set L back to 0 on every exit. Do not touch L6, L7, B6, B7. Save B3 if the
  module writes it **[D]**.
- DAG load-to-use: do not use a DAG register within 4 instructions of loading it; each instruction short of that
  costs a stall, up to 4 **[D]**.
- Multiplier: if both inputs come from the previous instruction, 2 stalls; one input from one instruction back, 1
  stall **[D]**.
- A conditional store followed by a load costs 1 stall **[D]**.
- PM data access in a loop may cost a cycle each. Keep data in DM only **[D]**, **[O]** for the exact cost.
- MODE1 write then use: wait the pipeline gap before an instruction that depends on the new mode **[D]**.
- Stall counts are not measured: the emulator counts instructions, not stalls **[O]**.

## 6. Examples (written fresh from the manual's instruction descriptions; not measured in the emulator yet)

The counts below are instruction counts by reading **[O]**. Add 3 to 6 cycles for loop setup and pipeline effects.

### 6.1 Gain loop: `out[i] = in[i] * g`

Input pointer I1, output pointer I0, gain in F8. Plain form, 3 instructions per sample, easy to read:

```
    LCNTR = DK_BLOCK, DO _g_end UNTIL LCE;
        F1 = DM(I1, M6);          // x
        F0 = F1 * F8;             // y = g x
_g_end: DM(I0, M6) = F0;          // store
```

Expected: 3 x 32 + 2 = 98 instructions.

Pipelined form: load of the next sample and the store of the previous product share instructions with the
multiply. Two memory accesses on the data bus in one instruction are not allowed, so load and store stay in two
different instructions:

```
    F1 = DM(I1, M6);              // prime: x[0]
    LCNTR = DK_BLOCK, DO _g2_end UNTIL LCE;
        F0 = F1 * F8, F1 = DM(I1, M6);      // y[n] = g x[n]; load x[n+1]
_g2_end: DM(I0, M6) = F0;                   // store y[n]
```

Expected: 2 x 32 + 2 = 66. The loop reads one word past the block (x[32]); the caller must make that legal, or
shorten the loop by one and add a tail **[O]**. A 1-instruction-per-sample form needs the second bus (PM) for the
store and a one-sample lag **[O]**.

### 6.2 One-pole low-pass: `y += a (x - y)`

State y in F0 (loaded from the record), coefficient a in F8.

```
    F1 = DM(I1, M6);                        // prime: x[0]
    LCNTR = DK_BLOCK, DO _lp_end UNTIL LCE;
        F2 = F1 - F0, F1 = DM(I1, M6);      // d = x[n] - y; load x[n+1]
        F2 = F2 * F8;                       // a d
        F0 = F0 + F2;                       // y
_lp_end: DM(I0, M6) = F0;                   // store y[n]
```

Expected: 4 x 32 + 3 = 131. The dependent chain (subtract, multiply, add) is three instructions long, so 3 is the
floor for this form; the store is the fourth. Fold the multiply: `y' = y + a x - a y` does not shorten the chain.
For a stereo pair, SIMD with PEYEN (state of the left in F0, of the right in S0) gives the same 4 per sample pair
**[O]**. Do not forget the dependency stalls the manual lists ("Stalls Related to Compute Operations") **[O]**.

### 6.3 Block ramp across a block: parameter smoothing

`p[i] = p0 + i * (p1 - p0) / 32`. Step computed once; 32 is a power of two, so the divide is a multiply by 1/32.

```
    F2 = F1 - F0;                           // F0 = old, F1 = new
    R3 = 0x3d000000;                        // 1/32 as a raw word
    F2 = F2 * F3;                           // step
    LCNTR = DK_BLOCK, DO _rp_end UNTIL LCE;
        F0 = F0 + F2;                       // p += step
        F5 = F4 * F0;                       // use p, for example as a gain on a value in F4
_rp_end: DM(I0, M6) = F5;
```

(Shape only: the loop body uses p as the module needs.) The accumulator add costs one slot per sample but can
share an instruction with a multiply (`F4 = F5 * F0, F0 = F0 + F2`), so it is free in a multiply-bound loop.
After the loop, write F1 (not F0) to the state so rounding drift does not accumulate.

## 7. Where existing code differs

- `pluck.s` [D]: 394 lines. Comments and `#define`, sections, labels and entry points follow section 1. It does:
  - state offsets as numbers (`DM(11, I3)`), not names;
  - register roles per routine, not stated in one table (partly in the comments);
  - the string loop: 23 instructions per sample, with `R1 = R3 + R14; I1 = R1` address rebuilds (8 instructions),
    three `PASS` copies, no multifunction packing (a multiply and an add are never in one instruction), and no
    circular buffer (masking with `AND 0x3fff` on a byte index). A packed version with hardware circular buffer is
    about 8 to 10 instructions per sample **[O]**. Its `max_cycles_block` of 2200 is above the count 23 x 32 = 736
    plus setup, so the descriptor has a wide margin.
  - `DM(I1, M5)` as "read without modify" rather than `DM(0, I1)`.
  - the voice end check after the loop uses compare and conditional jumps; fine.
- `<dk/machine/machine.h>` / `<dk/fx/fx.h>`: the contract is specified for entry and return. It lists clobbered and preserved
  registers but not MODE1 bit values, SIMD state, or whether loops/stacks are clear at entry. Add one sentence each
  (section 5). It does not say that B registers other than B3 are preserved: say whether they are **[O]**.
- `dk_machine.inc` provides `DK_IN_W_*` (word offsets) and `DK_DESC_*`. It has no names for register roles or
  state words; that is left to each module.
