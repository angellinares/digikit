# DSP recipes: tested SHARC+ code to copy

Every block below is copied from a source that passes its tests. Change constants and state words only; do not write new
assembly when a recipe fits. Contract, registers and entry: docs/MODULE-AUTHORING.md, section 7.1. Lines are `file:line`.
P = `modules/machines/pluck/src/dsp/pluck.s` (conformance, `test_pluck*.py`, ran on the device). D = `3b385ad6:modules/fx/master-drive/src/dsp/master_drive.s` (deleted; the quoted code stays, the D line numbers refer to the file on main at 3b385ad6).
Cost numbers are the ones the sources state or a count of instruction lines in the quoted loop. No other numbers are measured.
`#include "dk_machine.inc"` gives `DK_OK`, `DK_DONE`, `DK_BLOCK` (32), `DK_IN_W_PITCH`, `DK_IN_W_VELOCITY`, `DK_IN_W_OUT`, `DK_MAGIC`, `DK_MACHINE_V1`.

## 1. Parameter word to a float in 0..1 (P:158-163)

The adapter gives words 0 .. 0x7f00 (UI 0 .. 127). 1/32512 = 0x38010204 is 1/0x7f00 as a float.
```
    R0 = DM(6, I3);
    F0 = FLOAT R0;
    R1 = 0x38010204;
    F0 = F0 * F1;
    R1 = 0x3f800000;
    F0 = MIN(F0, F1);
```
Uses R0, R1, F0. Cost: 6 instructions. Adapt: load the word from `DM(i, I1)` with I1 = R12 (recipe 2), or from your state row.
Use `1.0 - x` as in P:76-82 (`F0 = F1 - F0;` after the MIN). The clamp to 1.0 is in the source every time; keep it.

## 2. Entry, parameter words and voice input (P:40-52, 54, 177-181, 294-310)

`note_on` and `render` start with `I3 = R4;` (the state row; R4 = state). `R8` = `dk_voice_in`, `R12` = the parameter words.
```
    I1 = R12;
    R0 = DM(0, I1);
    DM(6, I3) = R0;                       // DECAY
    I2 = R8;
    R0 = DM(DK_IN_W_VELOCITY, I2);
    DM(10, I3) = R0;
    R0 = DM(DK_IN_W_PITCH, I2);
```
Output block pointer (P:307-309):
```
    I1 = R8;
    R0 = DM(DK_IN_W_OUT, I1);
    I0 = R0;                              // the output block
```
Note-on: `I3 = R4; CALL _pk_coef; R0 = 1; DM(5, I3) = R0;` (P:178-181) computes the coefficients once and marks them fresh. `render` (P:296-303) reads word 5: if set, it clears it and skips `_pk_coef`; if not, it calls `_pk_coef` every block (parameters can move).
Returns: `JUMP (M13, I13);` after `R0 = DK_OK;` or `R0 = DK_DONE;`. A routine that calls (`CALL _dv_exp2`) ends its helper with `RTS;` (P:175).
Register classes: sdk/dsp/REGISTERS.md. M5 = 0, M6 = 1, M7 = -1 are the post-modifiers used below.

## 3. Block loop: DK_BLOCK samples, in to out (P:331-332, 355-358)

```
    R13 = 0;                              // peak
    LCNTR = 32, DO _pk_loop UNTIL LCE;
        ...32 samples of work, result in R8...
_pk_loop: DM(I0, M6) = R8;
    DM(0, I3) = R15;
```
The last instruction of the loop carries the label and stores the sample to `out` (I0, post-increment M6 = 1). Save state words after the loop.
Cost of pluck's loop body: 23 instruction lines per sample (counted in P:333-355).
Adapt: an effect reads its input with `R0 = DM(I4, M5);` and writes back with `DM(I4, M6) = R0;` (D:102, 115).

## 4. Pitch to frequency and period with `_dv_exp2` (P:53-73)

pitch word = semitones << 8 with tune added. `.EXTERN _dv_exp2;` computes F0 = 2^F0 and clobbers R0 to R3.
```
    // x = pitch / (256 * 12)
    R0 = DM(DK_IN_W_PITCH, I2);
    F10 = FLOAT R0;
    R1 = 0x39aaaaab;                      // 1 / (256 * 12)
    F10 = F10 * F1;                       // x
    R1 = 0x41333333;                      // 11.2: P = P0 2^-x stays at 2.5 or more (the loop has no shorter period)
    F10 = MIN(F10, F1);
    R1 = 0;                               // 0: no word makes x negative
    F10 = MAX(F10, F1);
    F0 = PASS F10;
    CALL _dv_exp2;
    R1 = 0x39329a62;                      // INC0 = 440 2^(-69/12) / 48000
    F11 = F0 * F1;                        // inc
    DM(16, I3) = R11;
    F0 = -F10;
    CALL _dv_exp2;
    R1 = 0x45b777e3;                      // P0 = 1 / INC0
    F12 = F0 * F1;                        // P
```
Result: F11 = inc = f0/fs (cycles per sample), F12 = P = period in samples. F10-F13 survive the calls. Adapt: use F11 as a phase step for an oscillator, P for a delay length.

## 5. Noise: xorshift32 with a per-note seed (P:234-254, 381)

The random state `_pk_rng` is shared by all voices (declared `.VAR _pk_rng = 0x2545f491;` in `seg_dmda`, P:381). Loop (13 instruction lines):
```
    R9 = DM(_pk_rng);
    R10 = -17;
    R11 = -31;
    R12 = 0;                              // lowpass state
    LCNTR = R0, DO _pk_fill UNTIL LCE;
        R1 = LSHIFT R9 BY 13;
        R9 = R9 XOR R1;
        R1 = LSHIFT R9 BY R10;
        R9 = R9 XOR R1;
        R1 = LSHIFT R9 BY 5;
        R9 = R9 XOR R1;
        F1 = FLOAT R9 BY R11;             // noise in [-1, 1)
```
After the loop: `DM(_pk_rng) = R9;` (P:254). Keep R9 nonzero (xorshift stays at 0 if it starts at 0). Uses R1, R9-R11; `R10 = -17` is the right-shift amount as a register.

## 6. One-pole low-pass (P:247-250) and one-zero damping (P:338-341)

Recursive one-pole on the noise (c in F13, 1 - c in F14, state F12):
```
        F1 = F1 * F14;
        F2 = F12 * F13;
        F12 = F1 + F2;                    // one-pole lowpass of the noise
```
y = (1-c) x + c y'. The coefficient is `c = 0.9 COLOR` (P:200-208). Damping in the string loop (x = F0, x' = F10, 1-b = F5, b = F6):
```
        F1 = F0 * F5;
        F8 = F10 * F6;
        F10 = PASS F0;
        F1 = F1 + F8;                     // lp
```
lp = (1-b) x + b x'. Adapt: any single-pole filter state lives in a register in the loop and in a state word between blocks (P:325, 356-358).

## 7. First-order all-pass for the fractional delay (P:342-347; coefficient P:116-155)

Per sample (lp = F1 from recipe 6, a = F7, state ap' = F12, lp' = F11, gain g = F9):
```
        F8 = F1 - F12;
        F8 = F8 * F7;
        F8 = F8 + F11;                    // ap
        F11 = PASS F1;
        F12 = PASS F8;
        F8 = F8 * F9;                     // y
```
ap = a (lp - ap') + lp'. Coefficient from the fractional part R in [0.5, 1.5) in F0. Copy P:116-127 whole (delta from R, then y = delta - 1); the start of the Horner chain is:
```
    // delta = R - w0^2 R (1 - R^2) / 12   (P:117-125, then R1 = 1.0 and F0 = F0 - F1: y = delta - 1)
    // a = c0 + y (c1 + y (... + y c7))
    R2 = 0xbc164b35;                      // c7
    F2 = F2 * F0;
    R3 = 0x3c91859a;                      // c6
    F2 = F2 + F3;
```
Continue Horner with c5 `0xbcfd8bca`, c4 `0x3d7e9704`, c3 `0xbe000677`, c2 `0x3e80024a`, c1 `0xbeffffed`, c0 `0xb413dec9`, each as `F2 = F2 * F0; R3 = c; F2 = F2 + F3;`
(P:131-150). Then clamp (P:151-155, see Traps). Needs w0^2 in F13 (P:71-73: inc squared times 4 pi^2 `0x421de9e6`).
Degree 7, error 1.4e-7 (P:17). Adapt: P:89-125 is the full tuning chain (tau_lp, N, R, delta); keep it whole and change only the damping if you drop recipe 6.

## 8. Delay line with a wrap: read at w - N, write at w (P:310-324, 332-337, 348-352)

The line starts at byte PK_LINE of the voice state: a 16 KiB ring (4096 words), indices in bytes, mask `0x3fff`.
```
    R3 = I3;
    R0 = PK_LINE;
    R3 = R3 + R0;                         // base of this voice's line
    R4 = 4;
    R2 = 0x3fff;
    R15 = DM(0, I3);                      // w
    R0 = DM(11, I3);
    R0 = LSHIFT R0 BY 2;
    R14 = R15 - R0;
    R14 = R14 AND R2;                     // r = (w - 4 N) mod 16 KiB
        R1 = R3 + R14;
        I1 = R1;
        R0 = DM(I1, M5);                  // x
        R14 = R4 + R14;
        R14 = R14 AND R2;
        ...
        R1 = R3 + R15;
        I2 = R1;
        DM(I2, M5) = R8;
        R15 = R4 + R15;
        R15 = R15 AND R2;
```
R3 line base, R14 read index, R15 write index (bytes), R4 = 4 (word step), R2 = mask.
Save `DM(0, I3) = R15;` after the loop. The line must start silent (cleared by `state_clear_bytes`). Constants: `PK_LINE 128`, `PK_STATE 16512` = 128 + 16384 (P:29-30).
Adapt: change the mask and `PK_STATE` together; the mask is a power of two minus 1 in bytes.

## 9. Comb at a pick position (P:185-198, 266-283)

p = trunc(PICK N / 2 + 0.5) is computed in note-on (P:185-198: PICK word times 1/32512, MIN 1.0, times 0.5, times N, plus 0.5, `R0 = TRUNC F0;`, stored in state word 17).
Apply y[k] -= y[k - p] over the excitation, from the end backward, only when p >= 1:
```
    R4 = DM(17, I3);
    R4 = PASS R4;
    IF EQ JUMP _pk_nocomb;
    R0 = DM(11, I3);
    R0 = R0 + 1;
    R0 = R0 - R4;                         // N + 1 - p samples
    R1 = 16380;
    R2 = R3 + R1;
    I2 = R2;                              // the last sample of the line
    R1 = LSHIFT R4 BY 2;
    R2 = R2 - R1;
    I1 = R2;                              // p samples earlier
    LCNTR = R0, DO _pk_comb UNTIL LCE;
        R0 = DM(I2, M5);
        R1 = DM(I1, M7);
        F0 = F0 - F1;
_pk_comb: DM(I2, M7) = R0;
_pk_nocomb:
```
Needs R3 = line base (recipe 8). Cost: 4 instruction lines per sample.

## 10. Silence detection: end the voice with DK_DONE (P:331, 353-354, 359-378)

Peak in R13 (`F0 = ABS F8; F13 = MAX(F13, F0);` in the loop, with `R13 = 0;` before it). After the loop, 8 blocks in a row below 2^-16 return DK_DONE:
```
    R0 = 0x37800000;                      // 2^-16
    R1 = DM(2, I3);
    COMP(F13, F0);
    IF GE JUMP _pk_loud;
    R2 = 1;
    R1 = R2 + R1;
    JUMP _pk_counted;
_pk_loud:
    R1 = 0;
_pk_counted:
    DM(2, I3) = R1;
    R2 = 8;
    COMP(R1, R2);
    IF LT JUMP _pk_return;
    R0 = DK_DONE;
    JUMP (M13, I13);
_pk_return:
    R0 = DK_OK;
    JUMP (M13, I13);
```
The block is already written when DK_DONE returns. Note: a NaN peak never counts as quiet (finding 51), so keep the clamps (Traps).

## 11. State words, clear bytes and `desc` (P:284-292, 380-394)

State row words are numbered in the file header (P:24-26): 0 write index, 2 quiet blocks, 3 and 4 filter states, 5 fresh flag, 6 to 10 parameters and velocity, 11 onward coefficients.
Note-on resets (all state is also zeroed by the adapter up to `state_clear_bytes`):
```
    R0 = 0;
    DM(0, I3) = R0;                       // w
    DM(2, I3) = R0;                       // quiet blocks
    DM(3, I3) = R0;                       // lp'
    DM(4, I3) = R0;                       // ap'
```
Then P:290-292 sets word 1 to `0x4b50` (the source does not explain it) and returns with `R0 = 0;`. Descriptor, in `.SECTION/DM seg_dmda;`:
```
.GLOBAL _pk_desc;
.VAR _pk_desc = DK_MAGIC;
.VAR _pk_desc1 = DK_MACHINE_V1;
.VAR _pk_desc2 = 4;
.VAR _pk_desc3 = PK_VOICES;
.VAR _pk_desc4 = PK_STATE;
.VAR _pk_desc5 = PK_STATE;
.VAR _pk_desc6 = 2;
.VAR _pk_desc7 = 0;
```
Order: magic, contract, params, voices, state bytes, clear bytes, call depth (2 here: `_pk_coef` calls `_dv_exp2`), stack bytes. The cost of a block is not declared: `module:check` measures it.
Here the clear bytes equal the state bytes, so the line starts silent. Code sections use `.SECTION/PM seg_pmco;`.

## 12. Per-sample linear ramp across a block (D:66-115)

Coefficients move from the last block's end value to this block's end value in 32 equal steps, so a knob move does not click.
```
    R1 = DM(_md_g);
    R2 = DM(_md_a);
    R3 = DM(_md_b);
    DM(_md_g) = R5;
    DM(_md_a) = R7;
    DM(_md_b) = R10;
    R12 = 0x3d000000;                     // 1/32
    F6 = F5 - F1;
    F6 = F6 * F12;                        // dG
    F5 = PASS F1;                         // start values
```
(F5 = end value, F1 = start value; the same lines repeat for A and B at D:87-93.) In the loop, add the delta each sample:
```
        F1 = F0 * F5;                     // t = G x
        F5 = F5 + F6;
```
(D:103, 111; the full loop is D:101-115.) 1/32 is exact in float, so the ramp ends on the end value. A second channel restarts the ramp from the saved start values (D:94-96, 116-118).
Cost stated (D:22-24): 20 instructions per block with no drive, 968 with the effect fully on.

## 13. Slewed parameter read in an effect (D:34-50)

An effect hook reads a frame word directly, scales it to 0..1 (subtract the neutral word, multiply by `_md_inv_max` = 1/(0x7f00 - 0x5000), MAX 0, MIN 1) and limits its move per block:
```
    R0 = DM(_fw_drive_word) (SWSE);
    F0 = FLOAT R0;
    R1 = 0x46a00000;                      // 20480.0 = 0x5000, the stock default word: no drive at or below it
    F0 = F0 - F1;
    // slew: d = d_prev + clip(target - d_prev, 1/128)
    R3 = DM(_md_d);
    F2 = F0 - F3;
    R1 = DM(_md_step);
    F2 = CLIP F2 BY F1;
    F0 = F3 + F2;                         // F0 = d, F3 = d_prev
    DM(_md_d) = R0;
```
(Full text D:34-50; the quote leaves out the scale and the clamps.) `(SWSE)` sign-extends the 16-bit frame word. The slew limit is `_md_step = 0x3c000000` (1/128 per block). An effect that saves registers, as D:56-64 and D:135-142 do,
keeps R5-R7 and R9-R13 in `.VAR` words; a machine does not need this (it may use all free registers, docs/MODULE-AUTHORING.md, section 7.1).

## Traps (named in the source comments)

- Unclamped pitch: `x` unbounded makes P outside the line. Keep the clamps `F10 = MIN(F10, 11.2)` and `MAX(F10, 0)` (P:58-61).
- Clamp the loop period before it is split: `R1 = 0x40201062` (2.501) with `MAX`, `R1 = 0x457fb7d7` (4091.49) with `MIN` (P:100-106), and N to 2 .. 4090 (P:110-113). Without them R leaves [0.5, 1.5), the all-pass coefficient passes 1 and the line becomes Inf/NaN until the next note-on (finding 51).
- Clamp the all-pass coefficient to `+-0x3f666666` (0.9) (P:151-154). The valid range is -0.2 .. 0.33; |a| < 1 is the stability limit.
- The guard zeroes a block above 8.0 or not finite, ends the voice, and 8 faults in 1500 blocks switch the machine off (finding 51; docs/MODULE-AUTHORING.md, section 7.1).
- A NaN peak never counts as quiet, so a voice with NaN state never ends (finding 51).
