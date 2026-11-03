// digikit/pluck: a Karplus-Strong plucked string, a machine of the SDK contract (<dk/machine.h>, docs/design/module-sdk.md 4.1). One source for
// both devices: the adapter of core/dsp-voice calls `_pk_note_on` once per note and `_pk_render` once per block with R4 = this voice's state,
// R8 = the `dk_voice_in` (pitch in semitones << 8 with the tune folded in, velocity << 8, the output block), R12 = the four parameter words
// DECAY, PICK, BRIGHT, COLOR (0 .. 0x7f00 = UI 0 .. 127, clamped by the adapter), and finds the note-on and the voice's end itself. State per voice
// (PK_STATE bytes, all cleared by the adapter at note-on: a row of 32 words, then the delay line of 4096 samples. The line starts silent because a later block may read further back than this note's N, when the pitch or a parameter moves the period). The module reads no frame word.
//
// Loop, per sample, with the write index w and the read index r = w - N (both in bytes, masked to the 16 KiB line):
//   x   = line[r]
//   lp  = (1 - b) x + b x'                  damping: one-zero lowpass, x' the previous x; phase delay ~ b
//   ap  = a (lp - ap') + lp'                first-order allpass, fractional delay delta = (1 - a) / (1 + a)
//   y   = g ap;  line[w] = y;  out = y
// The loop delay is N + tau_lp(b, w0) + tau_ap(delta, w0) = P = fs / f0. Per block:
//   x   = pitch / (256 * 12)                semitones / 12 from note 0 (the adapter added the tune word - 64 * 256)
//   inc = exp2(x) * INC0 (= f0 / fs);  P = exp2(-x) * P0;  w0^2 = 4 pi^2 inc^2
//   b   = 0.5 (1 - BRIGHT);  tau_lp = b - w0^2 b (1 - b)(1 - 2b) / 6
//   N   = trunc(P - tau_lp - 0.5);  R = P - tau_lp - N in [0.5, 1.5);  delta = R - w0^2 R (1 - R^2) / 12
//   a   = polynomial(delta - 1) (degree 7, error 1.4e-7)
//   g   = exp2(-GK P invT60), invT60 = 20 exp2(-8.64 DECAY): T60 = 0.05 s .. 20 s on the DECAY knob
// Note-on: the last N + 1 samples of the line are filled with xorshift32 noise through a one-pole lowpass of coefficient c = 0.9 COLOR, scaled
// by velocity (1 + 2 c) (an amplitude compensation), the mean is removed (sum * inc), and a pick-position comb y[k] -= y[k - p] with
// p = trunc(PICK / 2 N + 0.5) is applied when p >= 1. The coefficients of the note-on block are computed once, by note_on (word 5 of the row
// tells render). The voice ends (DK_DONE) after 8 consecutive blocks whose peak is below 2^-16.
//
// State row words: 0 w, 2 quiet blocks, 3 lp', 4 ap', 5 coefficients are fresh (set by note_on), 6 DECAY, 7 PICK, 8 BRIGHT, 9 COLOR, 10 velocity,
// 11 N, 12 1 - b, 13 b, 14 a, 15 g, 16 inc, 17 comb p, 18 c, 19 amplitude. The random state `_pk_rng` is shared by all voices (a note's noise
// continues the sequence of the last).

#include "dk_machine.inc"
#include "dsp/noise.inc"
#define PK_LINE 128
#define PK_STATE 16512
#define PK_VOICES 16

.SECTION/PM seg_pmco;
.GLOBAL _pk_render;
.GLOBAL _pk_note_on;
.EXTERN _dv_exp2;

// The coefficients of this block from the pitch, the parameters and the velocity. In: I3 = the state row, R8 = the voice input, R12 = the
// parameter words. Writes the row (words 6 to 19 but 17 to 19). Clobbers R0 to R3, R9 to R15 (not R4, R5, R8).
_pk_coef:
    I1 = R12;
    R0 = DM(0, I1);
    DM(6, I3) = R0;                       // DECAY
    R0 = DM(1, I1);
    DM(7, I3) = R0;                       // PICK
    R0 = DM(2, I1);
    DM(8, I3) = R0;                       // BRIGHT
    R0 = DM(3, I1);
    DM(9, I3) = R0;                       // COLOR
    I2 = R8;
    R0 = DM(DK_IN_W_VELOCITY, I2);
    DM(10, I3) = R0;
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
    F0 = F11 * F11;
    R1 = 0x421de9e6;                      // 4 pi^2
    F13 = F0 * F1;                        // w0^2

    // b = 0.5 (1 - BRIGHT), 1 - b
    R0 = DM(8, I3);
    F0 = FLOAT R0;
    R1 = 0x38010204;                      // 1 / 32512
    F0 = F0 * F1;
    R1 = 0x3f800000;                      // 1.0
    F0 = MIN(F0, F1);
    F0 = F1 - F0;
    R2 = 0x3f000000;                      // 0.5
    F14 = F0 * F2;                        // b
    DM(13, I3) = R14;
    F15 = F1 - F14;                       // 1 - b
    DM(12, I3) = R15;

    // tau_lp = b - w0^2 b (1 - b)(1 - 2b) / 6
    F0 = F14 * F15;
    F1 = F14 + F14;
    R2 = 0x3f800000;
    F1 = F2 - F1;                         // 1 - 2b
    F0 = F0 * F1;
    F0 = F0 * F13;
    R1 = 0x3e2aaaab;                      // 1 / 6
    F0 = F0 * F1;
    F0 = F14 - F0;                        // tau_lp
    F9 = F12 - F0;                        // P - tau_lp
    // keep P - tau_lp where N = 2 .. 4090 serves it with R in [0.5, 1.5): a period outside (a pitch or TUNE word beyond the
    // string's range) plays the nearest period the line can hold. Unclamped, R leaves the range, the allpass coefficient
    // passes 1 and the string state becomes Inf/NaN until the next note-on.
    R1 = 0x40201062;                      // 2.501
    F9 = MAX(F9, F1);
    R1 = 0x457fb7d7;                      // 4091.49
    F9 = MIN(F9, F1);
    R1 = 0x3f000000;
    F0 = F9 - F1;                         // P - tau_lp - 0.5
    R0 = TRUNC F0;
    R1 = 2;
    R0 = MAX(R0, R1);
    R1 = 4090;
    R0 = MIN(R0, R1);                     // N
    DM(11, I3) = R0;
    F1 = FLOAT R0;
    F0 = F9 - F1;                         // R = P - tau_lp - N
    // delta = R - w0^2 R (1 - R^2) / 12
    F1 = F0 * F0;
    R2 = 0x3f800000;
    F1 = F2 - F1;
    F1 = F1 * F0;
    F1 = F1 * F13;
    R2 = 0x3daaaaab;                      // 1 / 12
    F1 = F1 * F2;
    F0 = F0 - F1;                         // delta
    R1 = 0x3f800000;
    F0 = F0 - F1;                         // y = delta - 1
    // a = c0 + y (c1 + y (... + y c7))
    R2 = 0xbc164b35;                      // c7
    F2 = F2 * F0;
    R3 = 0x3c91859a;                      // c6
    F2 = F2 + F3;
    F2 = F2 * F0;
    R3 = 0xbcfd8bca;                      // c5
    F2 = F2 + F3;
    F2 = F2 * F0;
    R3 = 0x3d7e9704;                      // c4
    F2 = F2 + F3;
    F2 = F2 * F0;
    R3 = 0xbe000677;                      // c3
    F2 = F2 + F3;
    F2 = F2 * F0;
    R3 = 0x3e80024a;                      // c2
    F2 = F2 + F3;
    F2 = F2 * F0;
    R3 = 0xbeffffed;                      // c1
    F2 = F2 + F3;
    F2 = F2 * F0;
    R3 = 0xb413dec9;                      // c0
    F2 = F2 + F3;
    R1 = 0x3f666666;                      // |a| < 1 is the allpass's stability limit; the valid range is -0.2 .. 0.33
    F2 = MIN(F2, F1);
    R1 = 0xbf666666;
    F2 = MAX(F2, F1);
    DM(14, I3) = R2;                      // a

    // g = exp2(-GK P invT60), invT60 = 20 exp2(-8.64 DECAY)
    R0 = DM(6, I3);
    F0 = FLOAT R0;
    R1 = 0x38010204;
    F0 = F0 * F1;
    R1 = 0x3f800000;
    F0 = MIN(F0, F1);
    R1 = 0xc10a3d71;                      // -8.64
    F0 = F0 * F1;
    CALL _dv_exp2;
    R1 = 0x41a00000;                      // 20.0
    F0 = F0 * F1;
    F0 = F0 * F12;
    R1 = 0x3959b4b5;                      // 3 log2(10) / 48000
    F0 = F0 * F1;
    F0 = -F0;
    CALL _dv_exp2;
    DM(15, I3) = R0;                      // g
    RTS;

_pk_note_on:
    I3 = R4;
    CALL _pk_coef;
    R0 = 1;
    DM(5, I3) = R0;                       // render reads these coefficients in the note-on block

    // ---- note-on: excitation ----
    // comb offset p
    R0 = DM(11, I3);
    F1 = FLOAT R0;                        // N
    R0 = DM(7, I3);
    F0 = FLOAT R0;
    R2 = 0x38010204;
    F0 = F0 * F2;
    R2 = 0x3f800000;
    F0 = MIN(F0, F2);                     // PICK in 0 .. 1
    R2 = 0x3f000000;
    F0 = F0 * F2;
    F0 = F0 * F1;
    F0 = F0 + F2;                         // PICK N / 2 + 0.5
    R0 = TRUNC F0;
    DM(17, I3) = R0;
    // colour c = 0.9 COLOR, 1 - c, amplitude = velocity (1 + 2 c) / 2
    R0 = DM(9, I3);
    F0 = FLOAT R0;
    R2 = 0x38010204;
    F0 = F0 * F2;
    R2 = 0x3f800000;
    F0 = MIN(F0, F2);
    R2 = 0x3f666666;                      // 0.9
    F13 = F0 * F2;                        // c
    DM(18, I3) = R13;
    R2 = 0x3f800000;
    F14 = F2 - F13;                       // 1 - c
    F0 = F13 + F13;
    F0 = F0 + F2;                         // 1 + 2c
    R2 = 0x3f000000;
    F0 = F0 * F2;
    R1 = DM(10, I3);
    F1 = FLOAT R1;
    R2 = 0x38010204;
    F1 = F1 * F2;                         // velocity 0 .. 1
    F15 = F0 * F1;                        // amplitude
    DM(19, I3) = R15;

    // line base, start of the last N + 1 samples
    R3 = I3;
    R0 = PK_LINE;
    R3 = R3 + R0;
    R0 = DM(11, I3);
    R4 = LSHIFT R0 BY 2;                  // 4 N
    R6 = 0x4000;
    R6 = R6 - R4;
    R1 = 4;
    R6 = R6 - R1;
    R7 = R3 + R6;
    I2 = R7;
    R9 = DM(_pk_rng);
    R10 = -17;
    R11 = -31;
    R12 = 0;                              // lowpass state
    R8 = 0;                               // sum
    R0 = R0 + 1;
    LCNTR = R0, DO _pk_fill UNTIL LCE;
        DK_XORSHIFT32_STEP(R9, R1, R10);
        F1 = FLOAT R9 BY R11;             // noise in [-1, 1)
        F1 = F1 * F14;
        F2 = F12 * F13;
        F12 = F1 + F2;                    // one-pole lowpass of the noise
        F2 = F12 * F15;
        F8 = F8 + F2;
_pk_fill: DM(I2, M6) = R2;
    DM(_pk_rng) = R9;
    // mean removal: mean = sum * inc
    R2 = DM(16, I3);
    F8 = F8 * F2;
    R0 = DM(11, I3);
    R0 = R0 + 1;
    I1 = R7;
    I2 = R7;
    LCNTR = R0, DO _pk_mean UNTIL LCE;
        R2 = DM(I1, M6);
        F2 = F2 - F8;
_pk_mean: DM(I2, M6) = R2;
    // pick-position comb
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
    R0 = 0;
    DM(0, I3) = R0;                       // w
    DM(2, I3) = R0;                       // quiet blocks
    DM(3, I3) = R0;                       // lp'
    DM(4, I3) = R0;                       // ap'
    R0 = 0x4b50;
    DM(1, I3) = R0;    R0 = 0;
    JUMP (M13, I13);

_pk_render:
    I3 = R4;
    R0 = DM(5, I3);
    R0 = PASS R0;
    IF NE JUMP _pk_fresh;
    CALL _pk_coef;
    JUMP _pk_run;
_pk_fresh:
    R0 = 0;
    DM(5, I3) = R0;

    // ---- the block ----
_pk_run:
    I1 = R8;
    R0 = DM(DK_IN_W_OUT, I1);
    I0 = R0;                              // the output block
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
    R1 = R14 - R4;
    R1 = R1 AND R2;
    R1 = R3 + R1;
    I1 = R1;
    R10 = DM(I1, M5);                     // x' = the sample before the first read
    R11 = DM(3, I3);                      // lp'
    R12 = DM(4, I3);                      // ap'
    R5 = DM(12, I3);                      // 1 - b
    R6 = DM(13, I3);                      // b
    R7 = DM(14, I3);                      // a
    R9 = DM(15, I3);                      // g
    R13 = 0;                              // peak
    LCNTR = 32, DO _pk_loop UNTIL LCE;
        R1 = R3 + R14;
        I1 = R1;
        R0 = DM(I1, M5);                  // x
        R14 = R4 + R14;
        R14 = R14 AND R2;
        F1 = F0 * F5;
        F8 = F10 * F6;
        F10 = PASS F0;
        F1 = F1 + F8;                     // lp
        F8 = F1 - F12;
        F8 = F8 * F7;
        F8 = F8 + F11;                    // ap
        F11 = PASS F1;
        F12 = PASS F8;
        F8 = F8 * F9;                     // y
        R1 = R3 + R15;
        I2 = R1;
        DM(I2, M5) = R8;
        R15 = R4 + R15;
        R15 = R15 AND R2;
        F0 = ABS F8;
        F13 = MAX(F13, F0);
_pk_loop: DM(I0, M6) = R8;
    DM(0, I3) = R15;
    DM(3, I3) = R11;
    DM(4, I3) = R12;
    // quiet counter: 8 blocks below 2^-16 end the voice
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

.SECTION/DM seg_dmda;
.VAR _pk_rng = 0x2545f491;
// dk_machine_desc (<dk/machine.h>): magic, contract, n_params, voices, state_bytes, state_clear_bytes, max_cycles_block, max_cycles_note_on,
// max_call_depth, stack_bytes.
.GLOBAL _pk_desc;
.VAR _pk_desc = DK_MAGIC;
.VAR _pk_desc1 = DK_MACHINE_V1;
.VAR _pk_desc2 = 4;
.VAR _pk_desc3 = PK_VOICES;
.VAR _pk_desc4 = PK_STATE;
.VAR _pk_desc5 = PK_STATE;
.VAR _pk_desc6 = 2200;
.VAR _pk_desc7 = 9000;
.VAR _pk_desc8 = 2;
.VAR _pk_desc9 = 0;
