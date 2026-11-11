// digikit/freqshift: a frequency shifter, an effect of the FX contract (<dk/fx/fx.h>), loaded at run time on the Digitakt II (docs/findings/55-fx-adapter.md,
// 61-fx-pages.md). It moves every frequency of the master up or down by the same number of Hz (single sideband modulation, not a pitch shift). The float32
// reference, which defines the arithmetic operation for operation, is modules/fx/freqshift/tests/freqshift_ref.py; the notes below say what the routine does.
//
// SHIFT, the one knob (p0, a float from -64 to +63), is mapped once per block: f = sign(v) 0.63 (2^(|v| / 5.5) - 1) Hz (about 0.08 Hz at |v| = 1, 35 Hz at 32,
// 2 kHz at 64). The increment of a 32-bit phase accumulator (2^32 is a turn) is f 2^32 / 48000, truncated to an integer. The increment goes from the last block's
// to this block's in a straight line over the 32 samples (d = (new - old) >> 5 added before each sample), so a turn of the knob is a sweep and the phase never jumps.
// sin and cos of the phase: the phase folds to +-a quarter turn (integer ops) and a degree-7 polynomial gives the sine (error 1.2e-6); cos is the sine of the phase
// plus a quarter turn. The 64 values of a block (cos, sin per sample) go to the state.
//
// Per channel, two chains of four allpass sections in z^-2, transposed form (t = c x; y = t + s; u = c y; s' = u - x, one section of the even samples and one of the
// odd ones: a polyphase pair, so the sections cost the same as first-order ones). The chain P is delayed by one sample (its output goes through the state word
// before the buffer: a[n - 1] is read at n); a (P) and b (Q) differ in phase by 90 degrees to within 0.55 degrees from 30 Hz to 18 kHz (image rejection 46 dB).
// out = a cos + b sin moves the input up by f (down for f < 0); it is clamped to +-7.9 (the effect must stay below the level bound, 8).
//
// State (664 bytes; the adapter zeroes it when the effect switches on): word 0 the phase, 1 the last block's increment, 2 the dk_fx_in pointer of this call,
// 4 to 35 the section states of chain P and chain Q of the left and the right channel (8 words each: section k, even samples 2k, odd 2k + 1), 36 to 68 and 69 to 101
// the one-sample delay buffers of the two channels (a carry word, then the 32 outputs of chain P), 102 to 165 cos and sin of the 32 samples.

#include "dk_fx.inc"
#include "freqshift.h"
#define FS_PHASE 0
#define FS_INC 1
#define FS_FXIN 2
#define FS_ST 4
#define FS_AB0 36
#define FS_AB1 69
#define FS_CS 102
#define FS_STATE_BYTES 664
// the same offsets in bytes (the index registers hold byte addresses)
#define FS_ST_B 16
#define FS_ST_Q0_B 48
#define FS_ST_P1_B 80
#define FS_ST_Q1_B 112
#define FS_AB0_B 144
#define FS_AB0_1_B 148
#define FS_CS_B 408

.SECTION/PM seg_pmco;
.GLOBAL _fs_process;

_fs_process:                              // R4 = state, R8 = dk_fx_in, R12 = the parameter words
    I5 = R4;
    DM(FS_FXIN, I5) = R8;
    I1 = R12;
    R0 = DM(FSHIFT_SHIFT_WORD, I1);       // p0, the SHIFT word
    // ---- the increment of this block: f = KHZ (2^x - 1), x = min(|v|, 64) / 5.5 ----
    F9 = PASS F0;
    F0 = ABS F0;
    R1 = 0x42800000;
    F0 = MIN(F0, F1);
    R1 = 0x3e3a2e8c;
    F0 = F0 * F1;                         // x
    R1 = TRUNC F0;                        // i
    F2 = FLOAT R1;
    F0 = F0 - F2;                         // the fraction
    R2 = 0x3aeabeb6;
    F3 = F2 * F0;
    R2 = 0x3c16a34d;
    F3 = F3 + F2;
    F3 = F3 * F0;
    R2 = 0x3d63fc0a;
    F3 = F3 + F2;
    F3 = F3 * F0;
    R2 = 0x3e75f8b5;
    F3 = F3 + F2;
    F3 = F3 * F0;
    R2 = 0x3f31721f;
    F3 = F3 + F2;
    F3 = F3 * F0;                         // 2^fraction - 1
    F3 = SCALB F3 BY R1;                  // times 2^i
    R2 = 0x3f800000;
    F4 = SCALB F2 BY R1;                  // 2^i
    F4 = F4 - F2;
    F3 = F3 + F4;                         // 2^x - 1
    R2 = 0x3f2147ae;
    F3 = F3 * F2;                         // the shift in Hz
    R2 = 0;
    COMP(F9, F2);
    IF LT F3 = -F3;
    R2 = 0x47aec33e;
    F3 = F3 * F2;
    R3 = TRUNC F3;                        // the new increment
    // ---- the ramp and the oscillator ----
    R4 = DM(FS_INC, I5);
    DM(FS_INC, I5) = R3;
    R0 = R3 - R4;
    R1 = -5;
    R10 = ASHIFT R0 BY R1;                // d
    R8 = R4;                              // the increment
    R9 = DM(FS_PHASE, I5);
    R11 = 0x40000000;                     // a quarter turn
    R12 = 0x80000000;                     // half a turn
    R13 = 0x30800000;                     // 2^-30
    R15 = 0xbb8f3abb;
    R7 = 0x3da2d13d;
    R6 = 0xbf255b67;
    R5 = 0x3fc90fd1;
    R0 = I5;
    R1 = FS_CS_B;
    R0 = R0 + R1;
    I4 = R0;
    LCNTR = 32, DO _fs_osc UNTIL LCE;
        R8 = R8 + R10;
        R9 = R9 + R8;
        R0 = PASS R9;
        R1 = R0 + R0;
        R1 = R1 XOR R0;
        IF LT R0 = R12 - R0;
        F1 = FLOAT R0;
        F1 = F1 * F13;
        F2 = F1 * F1;
        F3 = F15 * F2;
        F3 = F3 + F7;
        F3 = F3 * F2;
        F3 = F3 + F6;
        F3 = F3 * F2;
        F3 = F3 + F5;
        F4 = F3 * F1;                     // sin
        R0 = R9 + R11;
        R1 = R0 + R0;
        R1 = R1 XOR R0;
        IF LT R0 = R12 - R0;
        F1 = FLOAT R0;
        F1 = F1 * F13;
        F2 = F1 * F1;
        F3 = F15 * F2;
        F3 = F3 + F7;
        F3 = F3 * F2;
        F3 = F3 + F6;
        F3 = F3 * F2;
        F3 = F3 + F5;
        F3 = F3 * F1;                     // cos
        DM(I4, M6) = R3;
_fs_osc: DM(I4, M6) = R4;
    DM(FS_PHASE, I5) = R9;
    // ---- chain P, left and right (the input advances from one channel to the other) ----
    I2 = DM(FS_FXIN, I5);
    I0 = DM(DK_FX_IN_W_IN, I2);
    R0 = I5;
    R1 = FS_AB0_1_B;
    R1 = R0 + R1;
    I1 = R1;
    R1 = FS_ST_B;
    R1 = R0 + R1;
    I3 = R1;
    CALL _fs_pass_p;
    R0 = DM(I1, M6);                      // the buffer of the right channel starts after the left one's: skip its carry word
    R0 = I5;
    R1 = FS_ST_P1_B;
    R1 = R0 + R1;
    I3 = R1;
    CALL _fs_pass_p;
    // ---- chain Q with the mix, left and right ----
    I2 = DM(FS_FXIN, I5);
    I0 = DM(DK_FX_IN_W_IN, I2);
    I1 = DM(DK_FX_IN_W_OUT, I2);
    R0 = I5;
    R1 = FS_AB0_B;
    R1 = R0 + R1;
    I2 = R1;
    R1 = FS_ST_Q0_B;
    R1 = R0 + R1;
    I3 = R1;
    R1 = FS_CS_B;
    R1 = R0 + R1;
    I4 = R1;
    CALL _fs_pass_q;
    R0 = DM(I2, M6);                      // the last output of chain P is the next block's first delayed sample
    DM(FS_AB0, I5) = R0;
    R0 = I5;
    R1 = FS_ST_Q1_B;
    R1 = R0 + R1;
    I3 = R1;
    R1 = FS_CS_B;
    R1 = R0 + R1;
    I4 = R1;
    CALL _fs_pass_q;
    R0 = DM(I2, M6);
    DM(FS_AB1, I5) = R0;
    R0 = DK_FX_OK;
    JUMP (M13, I13);

// Chain P over one channel (32 samples): I0 = the input (advances 32), I1 = where the outputs go (advances 32), I3 = the 8 state words of this
// chain and channel (section k, even samples: word 2k; odd samples: word 2k + 1). Clobbers R0-R15.
_fs_pass_p:
    R0 = 0x3ee83a77;                      // c0
    R1 = 0x3f5b9207;                      // c1
    R2 = 0x3f787e57;                      // c2
    R3 = 0x3f7f281b;                      // c3
    R4 = DM(0, I3);
    R8 = DM(1, I3);
    R5 = DM(2, I3);
    R9 = DM(3, I3);
    R6 = DM(4, I3);
    R10 = DM(5, I3);
    R7 = DM(6, I3);
    R11 = DM(7, I3);
    LCNTR = 16, DO _fs_pp UNTIL LCE;
        R12 = DM(I0, M6);
        F14 = F0 * F12;        // section 0: t = c x
        F13 = F14 + F4;        // y = t + s
        F14 = F0 * F13;        // u = c y
        F4 = F14 - F12;        // s' = u - x
        F14 = F1 * F13;        // section 1: t = c x
        F12 = F14 + F5;        // y = t + s
        F14 = F1 * F12;        // u = c y
        F5 = F14 - F13;        // s' = u - x
        F14 = F2 * F12;        // section 2: t = c x
        F13 = F14 + F6;        // y = t + s
        F14 = F2 * F13;        // u = c y
        F6 = F14 - F12;        // s' = u - x
        F14 = F3 * F13;        // section 3: t = c x
        F12 = F14 + F7;        // y = t + s
        F14 = F3 * F12;        // u = c y
        F7 = F14 - F13;        // s' = u - x
        DM(I1, M6) = R12;
        R12 = DM(I0, M6);
        F14 = F0 * F12;        // section 0: t = c x
        F13 = F14 + F8;        // y = t + s
        F14 = F0 * F13;        // u = c y
        F8 = F14 - F12;        // s' = u - x
        F14 = F1 * F13;        // section 1: t = c x
        F12 = F14 + F9;        // y = t + s
        F14 = F1 * F12;        // u = c y
        F9 = F14 - F13;        // s' = u - x
        F14 = F2 * F12;        // section 2: t = c x
        F13 = F14 + F10;        // y = t + s
        F14 = F2 * F13;        // u = c y
        F10 = F14 - F12;        // s' = u - x
        F14 = F3 * F13;        // section 3: t = c x
        F12 = F14 + F11;        // y = t + s
        F14 = F3 * F12;        // u = c y
        F11 = F14 - F13;        // s' = u - x
_fs_pp: DM(I1, M6) = R12;
    DM(0, I3) = R4;
    DM(1, I3) = R8;
    DM(2, I3) = R5;
    DM(3, I3) = R9;
    DM(4, I3) = R6;
    DM(5, I3) = R10;
    DM(6, I3) = R7;
    DM(7, I3) = R11;
    RTS;

// Chain Q over one channel with the mix: I0 = the input (advances 32), I1 = the output (advances 32), I2 = the delayed chain P output (advances 32),
// I3 = the 8 state words, I4 = the oscillator (cos, sin per sample, advances 64). out = a cos + b sin, within +-CLAMP. Clobbers R0-R15.
_fs_pass_q:
    R0 = 0x3e19a1c8;                      // c0
    R1 = 0x3f350f25;                      // c1
    R2 = 0x3f6f273d;                      // c2
    R3 = 0x3f7ce391;                      // c3
    R4 = DM(0, I3);
    R8 = DM(1, I3);
    R5 = DM(2, I3);
    R9 = DM(3, I3);
    R6 = DM(4, I3);
    R10 = DM(5, I3);
    R7 = DM(6, I3);
    R11 = DM(7, I3);
    R15 = 0x40fccccd;                      // CLAMP
    LCNTR = 16, DO _fs_pq UNTIL LCE;
        R12 = DM(I0, M6);
        F14 = F0 * F12;        // section 0: t = c x
        F13 = F14 + F4;        // y = t + s
        F14 = F0 * F13;        // u = c y
        F4 = F14 - F12;        // s' = u - x
        F14 = F1 * F13;        // section 1: t = c x
        F12 = F14 + F5;        // y = t + s
        F14 = F1 * F12;        // u = c y
        F5 = F14 - F13;        // s' = u - x
        F14 = F2 * F12;        // section 2: t = c x
        F13 = F14 + F6;        // y = t + s
        F14 = F2 * F13;        // u = c y
        F6 = F14 - F12;        // s' = u - x
        F14 = F3 * F13;        // section 3: t = c x
        F12 = F14 + F7;        // y = t + s
        F14 = F3 * F12;        // u = c y
        F7 = F14 - F13;        // s' = u - x
        R13 = DM(I2, M6);                 // a (the delayed chain P output)
        R14 = DM(I4, M6);                 // cos
        F13 = F13 * F14;
        R14 = DM(I4, M6);                 // sin
        F12 = F12 * F14;
        F12 = F12 + F13;
        F13 = -F15;
        F12 = MIN(F12, F15);
        F12 = MAX(F12, F13);
        DM(I1, M6) = R12;
        R12 = DM(I0, M6);
        F14 = F0 * F12;        // section 0: t = c x
        F13 = F14 + F8;        // y = t + s
        F14 = F0 * F13;        // u = c y
        F8 = F14 - F12;        // s' = u - x
        F14 = F1 * F13;        // section 1: t = c x
        F12 = F14 + F9;        // y = t + s
        F14 = F1 * F12;        // u = c y
        F9 = F14 - F13;        // s' = u - x
        F14 = F2 * F12;        // section 2: t = c x
        F13 = F14 + F10;        // y = t + s
        F14 = F2 * F13;        // u = c y
        F10 = F14 - F12;        // s' = u - x
        F14 = F3 * F13;        // section 3: t = c x
        F12 = F14 + F11;        // y = t + s
        F14 = F3 * F12;        // u = c y
        F11 = F14 - F13;        // s' = u - x
        R13 = DM(I2, M6);                 // a (the delayed chain P output)
        R14 = DM(I4, M6);                 // cos
        F13 = F13 * F14;
        R14 = DM(I4, M6);                 // sin
        F12 = F12 * F14;
        F12 = F12 + F13;
        F13 = -F15;
        F12 = MIN(F12, F15);
        F12 = MAX(F12, F13);
_fs_pq: DM(I1, M6) = R12;
    DM(0, I3) = R4;
    DM(1, I3) = R8;
    DM(2, I3) = R5;
    DM(3, I3) = R9;
    DM(4, I3) = R6;
    DM(5, I3) = R10;
    DM(6, I3) = R7;
    DM(7, I3) = R11;
    RTS;

.SECTION/DM seg_dmda;
// dk_fx_desc: magic, contract, n_params, state_bytes, max_cycles_block, max_call_depth, stack_bytes.
.GLOBAL _fs_desc;
.VAR _fs_desc = DK_FX_MAGIC;
.VAR _fs_desc1 = DK_FX_V1;
.VAR _fs_desc2 = FSHIFT_KNOBS;
.VAR _fs_desc3 = FS_STATE_BYTES;
.VAR _fs_desc4 = 4200;
.VAR _fs_desc5 = 1;
.VAR _fs_desc6 = 0;
