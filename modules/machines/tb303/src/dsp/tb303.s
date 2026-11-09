// machines/tb303: Open303 (rosic::Open303) as a voice of the SDK contract (<dk/machine.h>): its oscillator, its TB-303
// filter (TeeBeeFilter, mode TB_303) with the filter envelope and accent, its amp envelope and its output stage.
//
// Per block, at its last sample (Open303 does this per sample; these move over 2 ms or more, a block is 0.67 ms):
//   e32   = e ce^32                                         mainEnv (DecayEnvelope) at the block's end
//   r2    = a + rc2^32 (r2 - a),  a = e32 on an accented    rc2, the accent integrator; acc = ACC on an accented note, else 0
//           note
//   fx    = 2^clamp(u0 + es (e32 - eo) + acc r2)            the cutoff over fs sqrt 2: cutoff 2^(envScaler (e - envOffset) + accent)
//   b0, k, 2g                                               calculateCoefficientsApprox4 (b0's division: 1/d by Newton steps);
//                                                           the samples ramp them linearly from the previous block's values
// Per sample, as Open303's getSample at 48 kHz without its 4x oversampling (the filter is linear and the oscillator is
// band-limited below 24 kHz, so the 4x path and its anti-alias filter change the sound by about 0.1 dB; README.md):
//   f     = target + c (f - target)                         the slide (Open303's pitchSlewLimiter, 12 ms; c = 0 without)
//   e     = e ce                                            mainEnv, for the amp
//   a     = a + ka (ta - a)                                 ampEnv (AnalogEnvelope): ta = 1, ka = 1 on a note's first sample
//                                                           (attack 0), then ta = 0: ka decay 1230 ms while held, release
//                                                           1 ms (50 ms after an accent) once released
//   amp   = declick(a + (0.45 + 4 acc) e while held)        Open303's amp gain; ampDeClicker
//   x     = -osc                                            level = exponent(2048 f / 48000) + 1
//   y     = notch(hp2(allpass(teebee(hp1(x))))) amp 0.5
// Levels 4 and up are stored shorter than Open303's 2048 samples (gen_tables.py), so the index is phase x length.
//
// Knobs (params): 0 WAVE (saw / square), 1 ACC (Open303's accent amount), 2 CUT 314 .. 2394 Hz, 3 RES, 4 ENV, 5 DEC 200 .. 2000 ms
// (knob H, the stock LEV knob: core/dsp-voice gives TB303 tracks a fixed level of 100).
// The amp envelope is Open303's (set the track's AMP page to a gate: attack 0, sustain full): a slide (a tie) restarts neither
// it nor the filter envelope, as Open303's slideToNote; the end of the gate (`in->gate` 0) ends the held phase.
// Accent: a velocity above 100 at note-on (amount ACC, decay 200 ms, as Open303). Slide: a note that starts before the previous
// one was released (a trig longer than its step) glides to its pitch, as a 303's slide: a block with the gate down sets S_REL, the
// note-on reads and clears it.
//
// Abrupt knob changes (an LFO's square, a p-lock) make the filter blow up, Open303's own too (README.md), so u0, es, eo and
// the resonance glide to the knobs' values with a 2 ms time constant, a block per step (S_*T are the targets).
//
// State (256 bytes; the adapter clears none at note-on: the phase free-runs, a slide starts from the previous note, and the filters run on as in
// Open303): words S_* below. The adapter zeroes a track's state at its first note-on after the load (machine contract 1.6), so the first
// note-on finds no S_INIT mark and zeroes the state again; the check is kept (one compare) and holds without that zeroing.
// Constants: _tb_k at K_* (tables.inc).
//
// Loop registers: F9 wave, F10 frequency, F11 slide coefficient, F12 target, F13 phase, F6 k, F7 b0, F8 2g, F14 amp;
// F0-F5, F15 scratch; I1 constants, I3 state, I0 output, I4/I5 the tables.

#include "dk_machine.inc"
#include "tables.inc"

#define S_PHASE 0
#define S_FREQ 1
#define S_E 2
#define S_EC 3
#define S_ACC 4
#define S_RC 5
#define S_HX1 6
#define S_HY1 7
#define S_FX1 8
#define S_FY1 9
#define S_Y1 10
#define S_Y2 11
#define S_Y3 12
#define S_Y4 13
#define S_DX1 14
#define S_DX2 15
#define S_DY1 16
#define S_DY2 17
#define S_AX1 18
#define S_AY1 19
#define S_GX1 20
#define S_GY1 21
#define S_NX1 22
#define S_NX2 23
#define S_NY1 24
#define S_NY2 25
#define S_U0 26
#define S_ES 27
#define S_EO 28
#define S_RS 29
#define S_AMPK 30
#define S_U0T 31
#define S_EST 32
#define S_EOT 33
#define S_RST 34
#define S_REL 35
#define S_SLIDE 36
#define S_AENV 37
#define S_AON 38
#define S_ARC 39
#define S_AT 40
#define S_AKS 41
#define S_AK 42
#define S_ACCON 43
#define S_EC32 44
#define S_B0 45
#define S_K 46
#define S_G 47
#define S_DB0 48
#define S_DK 49
#define S_DG 50
#define S_INIT 51
#define TB_MARK 0x54423033                    // 'TB03': S_INIT of a state the note-on has zeroed

.SECTION/PM seg_pmco;
.GLOBAL _tb_render;
.GLOBAL _tb_note_on;
.EXTERN _dv_exp2;
.EXTERN _tb_saw;
.EXTERN _tb_sq;
.EXTERN _tb_off;
.EXTERN _tb_len;
.EXTERN _tb_k;

_tb_render:
    I3 = R4;
    I2 = R8;
    I5 = R12;
    R0 = _tb_k;
    I1 = R0;

    // target = 8.1758 Hz x 2^(pitch / 3072), pitch = semitones << 8 with the tune in; at most 11.2 octaves (19.2 kHz)
    R0 = DM(DK_IN_W_PITCH, I2);
    F0 = FLOAT R0;
    R1 = 0x39aaaaab;                      // 1 / 3072
    F0 = F0 * F1;
    R1 = 0x41333333;                      // 11.2
    F0 = MIN(F0, F1);
    R1 = 0;
    F0 = MAX(F0, F1);
    CALL _dv_exp2;
    R1 = 0x4102d013;                      // 440 x 2^(-69/12)
    F12 = F0 * F1;

    // WAVE: a switch, saw (0) or square (1)
    R0 = DM(0, I5);
    R1 = 0x80;
    R9 = 0;
    R2 = 0x3f800000;
    COMP(R0, R1);
    IF GE R9 = PASS R2;

    // CUT: u0 = log2(cutoff / (fs sqrt 2)); c = Open303's expToLin(cutoff, c0, c1)
    R0 = DM(2, I5);
    CALL _tb_word01;
    R1 = DM(K_LC1, I1);
    F1 = F0 * F1;
    R2 = DM(K_LC0, I1);
    F1 = F1 + F2;
    DM(S_U0T, I3) = R1;
    R1 = DM(K_C1, I1);
    F4 = F0 * F1;
    R2 = DM(K_C0, I1);
    F4 = F4 + F2;                         // c

    // ENV: Open303's calculateEnvModScalerAndOffset (the measured mapping)
    R0 = DM(4, I5);
    CALL _tb_word01;
    R1 = DM(K_SLOF, I1);
    F5 = F0 * F1;
    R2 = DM(K_SLOC, I1);
    F5 = F5 + F2;                         // sLo
    R1 = DM(K_SHIF, I1);
    F2 = F0 * F1;
    R3 = DM(K_SHIC, I1);
    F2 = F2 + F3;                         // sHi
    R1 = DM(K_ONE, I1);
    F3 = F1 - F4;
    F3 = F3 * F5;
    F2 = F4 * F2;
    F3 = F3 + F2;
    DM(S_EST, I3) = R3;                   // envScaler = (1 - c) sLo + c sHi
    R1 = DM(K_OF, I1);
    F3 = F4 * F1;
    R2 = DM(K_OC, I1);
    F3 = F3 + F2;
    DM(S_EOT, I3) = R3;                   // envOffset = oF c + oC

    // RES: the skewed resonance (1 - e^-3r) / (1 - e^-3)
    R0 = DM(3, I5);
    CALL _tb_word01;
    R1 = DM(K_RX, I1);
    F0 = F0 * F1;
    CALL _dv_exp2;
    R1 = DM(K_ONE, I1);
    F0 = F1 - F0;
    R1 = DM(K_RN, I1);
    F0 = F0 * F1;
    DM(S_RST, I3) = R0;

    // the slide coefficient this note chose, and the gate: a block with the gate down is a release (S_REL, read at the next note-on)
    R11 = DM(S_SLIDE, I3);
    R0 = DM(DK_IN_W_GATE, I2);
    R0 = PASS R0;
    IF NE JUMP _tb_held;
    R1 = 1;
    DM(S_REL, I3) = R1;
    R0 = 0;
    DM(S_AON, I3) = R0;                   // released
_tb_held:

    // the amp envelope's coefficient for this block, and its share of the filter envelope (0.45 + 4 accent while held)
    R0 = DM(S_AON, I3);
    R0 = PASS R0;
    R1 = DM(S_ARC, I3);
    R2 = 0;
    IF EQ JUMP _tb_aoff;
    R1 = DM(K_ADEC, I1);
    R0 = DM(S_ACC, I3);
    R2 = DM(K_AMPA, I1);
    F0 = F0 * F2;
    R2 = DM(K_AMP0, I1);
    F2 = F0 + F2;
_tb_aoff:
    DM(S_AK, I3) = R1;
    DM(S_AMPK, I3) = R2;

    // a frequency of 0 is the boot state: start on the target, and the glides on their targets
    R10 = DM(S_FREQ, I3);
    R0 = PASS R10;
    IF NE JUMP _tb_running;
    R10 = PASS R12;
    R0 = DM(S_U0T, I3);
    DM(S_U0, I3) = R0;
    R0 = DM(S_EST, I3);
    DM(S_ES, I3) = R0;
    R0 = DM(S_EOT, I3);
    DM(S_EO, I3) = R0;
    R0 = DM(S_RST, I3);
    DM(S_RS, I3) = R0;
_tb_running:
    // The filter's coefficients at the block's last sample; the samples ramp b0, k and 2g to them from the previous block's.
    // The glides, one block on
    R0 = DM(K_SM32, I1);
    R1 = DM(S_U0T, I3);
    R2 = DM(S_U0, I3);
    F1 = F1 - F2;
    F1 = F1 * F0;
    F2 = F2 + F1;
    DM(S_U0, I3) = R2;
    R1 = DM(S_EST, I3);
    R2 = DM(S_ES, I3);
    F1 = F1 - F2;
    F1 = F1 * F0;
    F2 = F2 + F1;
    DM(S_ES, I3) = R2;
    R1 = DM(S_EOT, I3);
    R2 = DM(S_EO, I3);
    F1 = F1 - F2;
    F1 = F1 * F0;
    F2 = F2 + F1;
    DM(S_EO, I3) = R2;
    R1 = DM(S_RST, I3);
    R2 = DM(S_RS, I3);
    F1 = F1 - F2;
    F1 = F1 * F0;
    F2 = F2 + F1;
    DM(S_RS, I3) = R2;

    // the filter envelope at the block's end (e ec^32), and the accent integrator one step of rc2^32
    R4 = DM(S_E, I3);
    R1 = DM(S_EC32, I3);
    F4 = F4 * F1;                         // e
    R1 = DM(S_ACC, I3);
    R2 = DM(S_ACCON, I3);
    F2 = F4 * F2;                         // a = e on an accented note
    R3 = DM(S_RC, I3);
    F3 = F3 - F2;
    R0 = DM(K_RC32, I1);
    F3 = F3 * F0;
    F3 = F3 + F2;
    DM(S_RC, I3) = R3;                    // r2
    F5 = F3 * F1;                         // acc r2
    R1 = DM(S_EO, I3);
    F0 = F4 - F1;
    R1 = DM(S_ES, I3);
    F0 = F0 * F1;
    F0 = F0 + F5;
    R1 = DM(S_U0, I3);
    F0 = F0 + F1;
    R1 = DM(K_ULO, I1);
    F0 = MAX(F0, F1);
    R1 = DM(K_UHI, I1);
    F0 = MIN(F0, F1);
    CALL _dv_exp2;
    F5 = PASS F0;                         // fx

    // b0 = (A0 + A1 fx) / d, d = 1 + B1 fx + B2 fx^2
    R1 = DM(K_B2, I1);
    F6 = F5 * F1;
    R1 = DM(K_B1, I1);
    F6 = F6 + F1;
    F6 = F6 * F5;
    R1 = DM(K_ONE, I1);
    F6 = F6 + F1;                         // d
    R1 = DM(K_RQ2, I1);
    F7 = F6 * F1;
    R1 = DM(K_RQ1, I1);
    F7 = F7 + F1;
    F7 = F7 * F6;
    R1 = DM(K_RQ0, I1);
    F7 = F7 + F1;
    R2 = DM(K_TWO, I1);
    F0 = F6 * F7;
    F0 = F2 - F0;
    F7 = F7 * F0;
    F0 = F6 * F7;
    F0 = F2 - F0;
    F7 = F7 * F0;
    F0 = F6 * F7;
    F0 = F2 - F0;
    F7 = F7 * F0;                         // 1 / d
    R1 = DM(K_A1, I1);
    F0 = F5 * F1;
    R1 = DM(K_A0, I1);
    F0 = F0 + F1;
    F7 = F0 * F7;                         // b0

    // k and g (TB_303)
    R1 = DM(K_P0, I1);
    F6 = F5 + F1;
    F6 = F6 * F5;
    R1 = DM(K_P1, I1);
    F6 = F6 + F1;
    F6 = F6 * F5;
    R1 = DM(K_P2, I1);
    F6 = F6 + F1;
    F6 = F6 * F5;
    R1 = DM(K_P3, I1);
    F6 = F6 + F1;
    F6 = F6 * F5;
    R1 = DM(K_P4, I1);
    F6 = F6 + F1;
    F6 = F6 * F5;
    R1 = DM(K_P5, I1);
    F6 = F6 + F1;
    R1 = DM(K_R17, I1);
    F8 = F6 * F1;
    R1 = DM(K_ONE, I1);
    F8 = F8 - F1;
    R2 = DM(S_RS, I3);
    F8 = F8 * F2;
    F8 = F8 + F1;
    F0 = F1 + F2;
    F8 = F8 * F0;                         // g
    F8 = F8 + F8;                         // 2g
    F6 = F6 * F2;                         // k

    // the ramps: from the previous block's end (none before the first block) to these, 1/32 of the way per sample
    R0 = DM(S_B0, I3);
    R0 = PASS R0;
    IF NE JUMP _tb_ramp;
    DM(S_B0, I3) = R7;
    DM(S_K, I3) = R6;
    DM(S_G, I3) = R8;
_tb_ramp:
    R1 = DM(K_B32, I1);
    R0 = DM(S_B0, I3);
    F2 = F7 - F0;
    F2 = F2 * F1;
    DM(S_DB0, I3) = R2;
    DM(S_B0, I3) = R7;
    F7 = PASS F0;
    R0 = DM(S_K, I3);
    F2 = F6 - F0;
    F2 = F2 * F1;
    DM(S_DK, I3) = R2;
    DM(S_K, I3) = R6;
    F6 = PASS F0;
    R0 = DM(S_G, I3);
    F2 = F8 - F0;
    F2 = F2 * F1;
    DM(S_DG, I3) = R2;
    DM(S_G, I3) = R8;
    F8 = PASS F0;

    R13 = DM(S_PHASE, I3);
    R0 = DM(DK_IN_W_OUT, I2);
    I0 = R0;

    LCNTR = DK_BLOCK, DO _tb_last UNTIL LCE;
    // the slide
    F0 = F10 - F12;
    F0 = F0 * F11;
    F10 = F0 + F12;

    // the coefficient ramps
    R0 = DM(S_DB0, I3);
    F7 = F7 + F0;                         // b0
    R0 = DM(S_DK, I3);
    F6 = F6 + F0;                         // k
    R0 = DM(S_DG, I3);
    F8 = F8 + F0;                         // 2g

    // the filter envelope, for the amp
    R4 = DM(S_E, I3);
    R1 = DM(S_EC, I3);
    F4 = F4 * F1;                         // e
    DM(S_E, I3) = R4;

    // the amp envelope, then the amp gain through the declicker (a 12 dB low-pass at 200 Hz)
    R0 = DM(S_AENV, I3);
    R1 = DM(S_AT, I3);
    R2 = DM(S_AKS, I3);
    F1 = F1 - F0;
    F1 = F1 * F2;
    F5 = F0 + F1;
    DM(S_AENV, I3) = R5;
    R1 = DM(S_AK, I3);
    DM(S_AKS, I3) = R1;
    R1 = 0;
    DM(S_AT, I3) = R1;
    R1 = DM(S_AMPK, I3);
    F0 = F4 * F1;
    F0 = F0 + F5;
    R1 = DM(K_DCB0, I1);
    F1 = F0 * F1;
    R2 = DM(S_DX1, I3);
    R3 = DM(K_DCB1, I1);
    F2 = F2 * F3;
    F1 = F1 + F2;
    R2 = DM(S_DX2, I3);
    R3 = DM(K_DCB2, I1);
    F2 = F2 * F3;
    F1 = F1 + F2;
    R2 = DM(S_DY1, I3);
    R3 = DM(K_DCA1, I1);
    F2 = F2 * F3;
    F1 = F1 + F2;
    R2 = DM(S_DY2, I3);
    R3 = DM(K_DCA2, I1);
    F2 = F2 * F3;
    F14 = F1 + F2;
    R2 = DM(S_DX1, I3);
    DM(S_DX2, I3) = R2;
    DM(S_DX1, I3) = R0;
    R2 = DM(S_DY1, I3);
    DM(S_DY2, I3) = R2;
    DM(S_DY1, I3) = R14;

    // the oscillator; level from the table increment's exponent: f >= 8.17 Hz keeps it at -1 or more, f <= 19.2 kHz at 10 or less
    R1 = DM(K_INCT, I1);
    F1 = F10 * F1;
    R2 = FEXT R1 BY 23:8;
    R3 = -126;
    R4 = R2 + R3;
    R3 = 0;
    R2 = MAX(R4, R3);
    R2 = LSHIFT R2 BY 2;                  // byte offset into the level tables
    R3 = _tb_off;
    R4 = R3 + R2;
    I4 = R4;
    R0 = DM(0, I4);                       // the level's first word
    R3 = _tb_len;
    R4 = R3 + R2;
    I4 = R4;
    R3 = DM(0, I4);                       // its length, as a float
    F1 = F13 * F3;
    R2 = TRUNC F1;
    F3 = FLOAT R2;
    F1 = F1 - F3;                         // frac
    R4 = R0 + R2;
    R4 = LSHIFT R4 BY 2;
    R3 = _tb_saw;
    R2 = R3 + R4;
    I4 = R2;
    R3 = _tb_sq;
    R2 = R3 + R4;
    I5 = R2;
    R2 = DM(0, I4);
    R3 = DM(1, I4);
    F3 = F3 - F2;
    F3 = F3 * F1;
    F2 = F2 + F3;                         // saw
    R3 = DM(0, I5);
    R4 = DM(1, I5);
    F4 = F4 - F3;
    F4 = F4 * F1;
    F3 = F3 + F4;                         // square
    R4 = DM(K_HALF, I1);
    F3 = F3 * F4;
    F3 = F3 - F2;
    F3 = F3 * F9;
    F2 = F2 + F3;
    F15 = -F2;                            // Open303's -osc
    R1 = DM(K_INVFS, I1);
    F0 = F10 * F1;
    F13 = F13 + F0;
    R1 = DM(K_ONE, I1);
    F0 = F13 - F1;
    IF GE F13 = PASS F0;

    // the pre-filter high-pass (44.5 Hz)
    R1 = DM(K_H1B0, I1);
    F0 = F15 * F1;
    R2 = DM(S_HX1, I3);
    R3 = DM(K_H1B1, I1);
    F2 = F2 * F3;
    F0 = F0 + F2;
    R2 = DM(S_HY1, I3);
    R3 = DM(K_H1A1, I1);
    F2 = F2 * F3;
    F0 = F0 + F2;
    DM(S_HX1, I3) = R15;
    DM(S_HY1, I3) = R0;

    // the feedback high-pass (150 Hz) on k y4
    R4 = DM(S_Y4, I3);
    F4 = F6 * F4;
    R1 = DM(K_FBB0, I1);
    F1 = F4 * F1;
    R2 = DM(S_FX1, I3);
    R3 = DM(K_FBB1, I1);
    F2 = F2 * F3;
    F1 = F1 + F2;
    R2 = DM(S_FY1, I3);
    R3 = DM(K_FBA1, I1);
    F2 = F2 * F3;
    F1 = F1 + F2;
    DM(S_FX1, I3) = R4;
    DM(S_FY1, I3) = R1;
    F0 = F0 - F1;                         // y0

    // the four stages
    R1 = DM(S_Y1, I3);
    R2 = DM(S_Y2, I3);
    R3 = DM(S_Y3, I3);
    R4 = DM(S_Y4, I3);
    F15 = F7 + F7;
    F5 = F0 - F1;
    F5 = F5 + F2;
    F5 = F15 * F5;
    F1 = F1 + F5;
    F5 = F2 + F2;
    F5 = F1 - F5;
    F5 = F5 + F3;
    F5 = F7 * F5;
    F2 = F2 + F5;
    F5 = F3 + F3;
    F5 = F2 - F5;
    F5 = F5 + F4;
    F5 = F7 * F5;
    F3 = F3 + F5;
    F5 = F4 + F4;
    F5 = F3 - F5;
    F5 = F7 * F5;
    F4 = F4 + F5;
    DM(S_Y1, I3) = R1;
    DM(S_Y2, I3) = R2;
    DM(S_Y3, I3) = R3;
    DM(S_Y4, I3) = R4;
    F0 = F8 * F4;

    // the all-pass (14 Hz), the high-pass (24.2 Hz) and the notch (7.5 Hz)
    R1 = DM(K_APB0, I1);
    F1 = F0 * F1;
    R2 = DM(S_AX1, I3);
    R3 = DM(K_APB1, I1);
    F2 = F2 * F3;
    F1 = F1 + F2;
    R2 = DM(S_AY1, I3);
    R3 = DM(K_APA1, I1);
    F2 = F2 * F3;
    F1 = F1 + F2;
    DM(S_AX1, I3) = R0;
    DM(S_AY1, I3) = R1;
    R0 = DM(K_H2B0, I1);
    F0 = F1 * F0;
    R2 = DM(S_GX1, I3);
    R3 = DM(K_H2B1, I1);
    F2 = F2 * F3;
    F0 = F0 + F2;
    R2 = DM(S_GY1, I3);
    R3 = DM(K_H2A1, I1);
    F2 = F2 * F3;
    F0 = F0 + F2;
    DM(S_GX1, I3) = R1;
    DM(S_GY1, I3) = R0;
    R1 = DM(K_NB0, I1);
    F1 = F0 * F1;
    R2 = DM(S_NX1, I3);
    R3 = DM(K_NB1, I1);
    F2 = F2 * F3;
    F1 = F1 + F2;
    R2 = DM(S_NX2, I3);
    R3 = DM(K_NB2, I1);
    F2 = F2 * F3;
    F1 = F1 + F2;
    R2 = DM(S_NY1, I3);
    R3 = DM(K_NA1, I1);
    F2 = F2 * F3;
    F1 = F1 + F2;
    R2 = DM(S_NY2, I3);
    R3 = DM(K_NA2, I1);
    F2 = F2 * F3;
    F1 = F1 + F2;
    R2 = DM(S_NX1, I3);
    DM(S_NX2, I3) = R2;
    DM(S_NX1, I3) = R0;
    R2 = DM(S_NY1, I3);
    DM(S_NY2, I3) = R2;
    DM(S_NY1, I3) = R1;
    F1 = F1 * F14;
    R2 = DM(K_GAIN, I1);
    F1 = F1 * F2;
_tb_last: DM(I0, M6) = R1;

    DM(S_PHASE, I3) = R13;
    DM(S_FREQ, I3) = R10;
    R0 = DK_OK;
    JUMP (M13, I13);

// At note-on: the accent (a velocity above 100) and the filter envelope's restart, as Open303's triggerNote: decay 200 ms
// with an accent, DEC (200 .. 2000 ms) without; e = 1 / ce, so its first sample is 1.
_tb_note_on:
    I3 = R4;
    I2 = R8;
    I5 = R12;
    R0 = _tb_k;
    I1 = R0;
    // a state that no note-on has zeroed yet: whatever the runtime's memory held
    R0 = DM(S_INIT, I3);
    R1 = TB_MARK;
    COMP(R0, R1);
    IF EQ JUMP _tb_inited;
    I0 = I3;
    M1 = 1;
    R0 = 0;
    LCNTR = S_INIT, DO _tb_clr UNTIL LCE;
_tb_clr: DM(I0, M1) = R0;
    DM(S_INIT, I3) = R1;
_tb_inited:
    // a slide when the previous note was not released (and there was one)
    R0 = 0;
    R1 = DM(S_REL, I3);
    R2 = DM(S_FREQ, I3);
    R3 = DM(K_SLIDE, I1);
    R1 = PASS R1;
    IF NE JUMP _tb_noslide;
    R2 = PASS R2;
    IF NE R0 = PASS R3;
_tb_noslide:
    DM(S_SLIDE, I3) = R0;
    R0 = PASS R0;
    IF NE JUMP _tb_tie;
    R1 = DM(K_ONE, I1);                   // a new note: the amp envelope's attack on its first sample
    DM(S_AT, I3) = R1;
    DM(S_AKS, I3) = R1;
    DM(S_B0, I3) = R0;                    // and its first block starts on its own filter coefficients (no ramp)
_tb_tie:
    R1 = DM(K_ONE, I1);
    DM(S_AON, I3) = R1;
    R0 = 0;
    DM(S_REL, I3) = R0;
    R0 = DM(DK_IN_W_VELOCITY, I2);
    R1 = 0x6400;                          // 100 << 8
    R2 = 0;
    R3 = 0x3f800000;
    COMP(R0, R1);
    IF GT R2 = PASS R3;                   // an accented note
    DM(S_ACCON, I3) = R2;
    R0 = DM(1, I5);
    CALL _tb_word01;
    F0 = F0 * F2;
    DM(S_ACC, I3) = R0;                   // its amount (ACC)
    R1 = DM(K_AREL, I1);
    R3 = DM(K_ARELA, I1);
    R2 = PASS R2;
    IF NE R1 = PASS R3;
    DM(S_ARC, I3) = R1;                   // the release, set at each note as Open303's
    R4 = DM(K_KDEC, I1);                  // m = k_env / 200 ms
    R2 = PASS R2;
    IF NE JUMP _tb_env;
    R0 = DM(5, I5);
    CALL _tb_word01;
    R1 = DM(K_DX, I1);
    F0 = F0 * F1;
    CALL _dv_exp2;
    F4 = F0 * F4;                         // m = k_env / tau, tau = 200 x 10^x ms
_tb_env:
    F0 = -F4;
    CALL _dv_exp2;
    DM(S_EC, I3) = R0;
    F0 = F0 * F0;
    F0 = F0 * F0;
    F0 = F0 * F0;
    F0 = F0 * F0;
    F0 = F0 * F0;
    DM(S_EC32, I3) = R0;                  // ec^32, a block's decay
    R0 = DM(S_SLIDE, I3);
    R0 = PASS R0;
    IF NE JUMP _tb_on_end;                // a tie keeps the filter envelope running (its decay changes)
    F0 = PASS F4;
    CALL _dv_exp2;
    DM(S_E, I3) = R0;
_tb_on_end:
    R0 = 0;
    JUMP (M13, I13);

// F0 = min(R0 / 32512, 1) for a knob word in R0; clobbers R1
_tb_word01:
    F0 = FLOAT R0;
    R1 = DM(K_W01, I1);
    F0 = F0 * F1;
    R1 = DM(K_ONE, I1);
    F0 = MIN(F0, F1);
    RTS;

.SECTION/DM seg_dmda;
// magic, contract, n_params, voices, state_bytes, state_clear_bytes, max_cycles_block, max_cycles_note_on,
// max_call_depth, stack_bytes
.GLOBAL _tb_desc;
.VAR _tb_desc = DK_MAGIC;
.VAR _tb_desc1 = 513;  // DK_MACHINE_V1 | DK_MACHINE_GATE (an expression is not accepted here)
.VAR _tb_desc2 = 6;
.VAR _tb_desc3 = DK_MAX_VOICES;
.VAR _tb_desc4 = 256;
.VAR _tb_desc5 = 0;
.VAR _tb_desc6 = 7500;
.VAR _tb_desc7 = 400;
.VAR _tb_desc8 = 2;
.VAR _tb_desc9 = 0;
