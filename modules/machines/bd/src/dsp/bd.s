// =====================================================================
//  bd : an FM bass drum for the DT2 SDK contract
// ---------------------------------------------------------------------
//  Phase modulation, not an 808. A carrier sine is modulated by a second sine whose *index* decays fast, so the strike
//  is a clang of sidebands that collapses into a pure sine as the note decays. The carrier also carries a pitch
//  envelope, so the whole thing punches downward while the clang rings out. A sample cannot do that; this is the
//  instrument people mean when they ask for an FM kick.
//
//    carrier   f(t) = f0 * (1 + 2.5 * BEND * env^2)       env = 1 - n/40: falls over 40 blocks, about 27 ms
//    modulator f(t) = carrier * (0.25 + 3.75 * RATIO)     1:1 smooth (RATIO 0.2), 1:2 bright, non-integer ratios metallic
//    index     I(t) = 3 * INDEX * (1 - n/112)^2           modulation depth in cycles, gone after 112 blocks (about 75 ms)
//    noise     white noise * NOISE * (1 - n/60)           gone after 60 blocks (40 ms)
//    out       sin(2 pi (carrier phase + I * sin(2 pi modulator phase)))
//
//  The amplitude decay is the track's AMP envelope, so the machine only has to sound right for the first 50 ms.
//
//  State: carrier phase, block counter, modulator phase (all zeroed at note-on, which is the envelope trigger), the
//  index, and the modulator's last value. Nothing survives a trigger, so there is nothing to seed.
//
//  Register map (F and R are one file):
//    F13 carrier phase   F15 modulator phase   F12 carrier inc   F14 modulator inc
//    F7 base inc   F10 ratio (4.0 in the loop) F9 level   F6 BEND   F5 INDEX   F11 1.0   F4 0.5   F8 0.25
//    per-sample scratch: F0 F1 F2 F3
// =====================================================================
#include "dk_machine.inc"
#include "dsp/oscillator.inc"
#include "dsp/noise.inc"

.SECTION/PM seg_pmco;
.GLOBAL _bd_render;
.GLOBAL _bd_note_on;
.EXTERN _dv_exp2;

_bd_render:
    I3 = R4;
    I2 = R8;
    I1 = R12;

    // ---- pitch -> base increment ----
    R0 = DM(DK_IN_W_PITCH, I2);
    F7 = FLOAT R0;
    R1 = 0x46fe0000;                      // 32512.0 = note 127
    F7 = MIN(F7, F1);
    R1 = 0x39aaaaab;                      // 1 / (256 * 12)
    F0 = F7 * F1;
    CALL _dv_exp2;
    R1 = 0x39329a62;                      // 440 * 2^(-69/12) / 48000
    F0 = F0 * F1;
    F7 = F0;

    // ---- level from velocity ----
    R0 = DM(DK_IN_W_VELOCITY, I2);
    F0 = FLOAT R0;
    R1 = 0x38010204;                      // 1 / 32512
    F0 = F0 * F1;
    R1 = 0x3eb33333;                      // 0.35
    F9 = F0 * F1;

    // ---- BEND, INDEX, RATIO ----
    R1 = 0x38010204;
    R0 = DM(0, I1);
    F0 = FLOAT R0;
    F6 = F0 * F1;                         // BEND
    R0 = DM(1, I1);
    F0 = FLOAT R0;
    F5 = F0 * F1;                         // INDEX
    R0 = DM(2, I1);
    F0 = FLOAT R0;
    F0 = F0 * F1;                         // RATIO 0 .. 1
    R1 = 0x40700000;                      // 3.75
    NOP;
    F0 = F0 * F1;
    R1 = 0x3e800000;                      // 0.25
    NOP;
    F10 = F0 + F1;                        // ratio multiplier 0.25 .. 4

    // ---- envelopes, from the block counter (zeroed at note-on) ----
    R11 = 0x3f800000;                     // 1.0
    R0 = DM(1, I3);
    R1 = R0 + 1;
    DM(1, I3) = R1;
    F0 = FLOAT R0;                        // n

    // carrier pitch: env = (1 - n/40)^2, inc = base * (1 + 2.5 BEND env)
    R1 = 0x3ccccccd;                      // 0.025 = 1/40
    NOP;
    F2 = F0 * F1;
    R1 = 0x3f800000;                      // 1.0
    NOP;
    F2 = F11 - F2;
    R1 = 0;                               // 0.0
    NOP;
    F2 = MAX(F2, F1);
    NOP;
    F2 = F2 * F2;                         // squared: quickest at the strike
    F1 = F2 * F6;                         // BEND * env
    R3 = 0x40200000;                      // 2.5
    NOP;
    F1 = F1 * F3;                         // 2.5 BEND env
    F1 = F1 + F11;                        // 1 + 2.5 BEND env
    NOP;
    F12 = F7 * F1;                        // carrier increment
    F14 = F12 * F10;                      // modulator increment, by ratio

    // index: I = 3 INDEX (1 - n/112)^2
    R1 = 0x3c124925;                      // 0.0089 = 1/112: the index lasts 112 blocks, about 75 ms. At 1/20 it was
    NOP;                                  // gone in 13 ms, so every sideband sat inside the transient and the body the
    F2 = F0 * F1;                         // ear hears afterwards was an unmodulated sine -- heard as 'a click'
    R1 = 0x3f800000;                      // 1.0
    NOP;
    F2 = F11 - F2;
    R1 = 0;                               // 0.0
    NOP;
    F2 = MAX(F2, F1);
    NOP;
    F2 = F2 * F2;
    F2 = F2 * F5;                         // x INDEX
    R1 = 0x40400000;                      // 3.0: up to three cycles of excursion, which is real FM. The phase is
    NOP;                                  // wrapped before the sine (any value in -3 .. 4 is reduced to 0 .. 1), so
    F2 = F2 * F1;                         // the polynomial is always inside its range
    R1 = F2;
    DM(3, I3) = R1;                       // the index for this block

    // noise envelope: (1 - n/60), 40 ms, so NOISE turns the drum into a snare or a hat
    R1 = 0x3c888889;                      // 1/60
    NOP;
    F2 = F0 * F1;
    R1 = 0x3f800000;                      // 1.0
    NOP;
    F2 = F11 - F2;
    R1 = 0;                               // 0.0
    NOP;
    F2 = MAX(F2, F1);
    R1 = F2;
    DM(6, I3) = R1;
    R1 = 0x38010204;                      // 1 / 32512
    R0 = DM(3, I1);
    F0 = FLOAT R0;
    F0 = F0 * F1;                         // NOISE 0 .. 1
    R1 = F0;
    DM(7, I3) = R1;

    // ---- constants and state ----
    R0 = DM(DK_IN_W_OUT, I2);
    I0 = R0;
    R13 = DM(0, I3);                      // carrier phase
    R15 = DM(2, I3);                      // modulator phase
    R4 = 0x3f000000;                      // 0.5
    R8 = 0x3e800000;                      // 0.25
    R10 = 0x40800000;                     // 4.0: whole cycles added before the floor of the modulated phase
    LCNTR = DK_BLOCK, DO _bd_last UNTIL LCE;

    // ---- modulator: sin(2 pi modulator phase) ----
    F0 = F15 - F4;
    NOP;
    F2 = CLIP F0 BY F8;
    F2 = F2 + F2;
    F0 = F2 - F0;
    NOP;
    DK_SINE_POLY_STEP(F2, F0, F1, F3, R2, R3);
    R0 = F2;
    NOP;
    DM(4, I3) = R0;                       // the modulator value

    // ---- carrier: sin(2 pi (carrier phase + I * modulator)) ----
    R0 = DM(3, I3);
    F0 = PASS R0;                         // I. The modulator goes through R2: R0 IS F0, so reading it into R0 a
    NOP;                                  // second time would overwrite the index and the multiply becomes m x m
    R2 = DM(4, I3);
    F2 = PASS R2;                         // modulator
    NOP;
    F0 = F0 * F2;                         // I m
    NOP;
    F0 = F0 + F13;                        // the modulated phase
    NOP;
    // wrap it into [0, 1) before the sine, or the polynomial reads outside its range and gives a folded wave. The
    // phase is F13 (0 .. 1) + I m (|I m| < 3), so it lies in (-3, 4). Add 4.0 to make it positive, then take the
    // fraction with TRUNC (truncation is the floor for a positive value). The 4.0 is a whole number of cycles.
    F0 = F0 + F10;                        // + 4.0: now in (1, 8)
    R1 = TRUNC F0;                        // floor
    F1 = FLOAT R1;
    F0 = F0 - F1;                         // fraction, 0 <= F0 < 1
    NOP;
    F0 = F0 - F4;
    NOP;
    F2 = CLIP F0 BY F8;
    F2 = F2 + F2;
    F0 = F2 - F0;
    NOP;
    DK_SINE_POLY_STEP(F2, F0, F1, F3, R2, R3);

    // ---- noise layer: xorshift, long envelope ----
    R0 = DM(5, I3);
    DK_XORSHIFT32_LOAD_STEP(R0, R1);
    DM(5, I3) = R0;
    F3 = FLOAT R0;
    R1 = 0x30000000;                      // 2^-31
    F1 = PASS R1;
    NOP;
    F3 = F3 * F1;                         // noise -1 .. 1
    NOP;
    R1 = DM(6, I3);
    F1 = PASS R1;                         // envelope
    NOP;
    F3 = F3 * F1;
    NOP;
    R1 = DM(7, I3);
    F1 = PASS R1;                         // NOISE
    NOP;
    F3 = F3 * F1;
    NOP;
    F2 = F2 + F3;

    // ---- level and bound ----
    F2 = F2 * F9;
    NOP;
    R0 = 0x3f800000;                      // +1
    NOP;
    F3 = MIN(F2, F0);
    R1 = 0xbf800000;                      // -1
    NOP;
    F2 = MAX(F3, F1);
    DM(0, I0) = F2;

    // ---- advance ----
    F13 = F13 + F12;
    F0 = F13 - F11;
    IF GE F13 = PASS F0;
    F15 = F15 + F14;
    R1 = TRUNC F15;                       // F15 >= 0, so this is the floor; F14 can exceed 1
    F1 = FLOAT R1;
    F15 = F15 - F1;                       // 0 <= F15 < 1
_bd_last: MODIFY(I0, 4);

    DM(0, I3) = R13;
    DM(2, I3) = R15;
    R0 = DK_OK;
    JUMP (M13, I13);

_bd_note_on:
    I3 = R4;
    R0 = 0x2545f491;                      // seed the noise: a zero xorshift state stays zero
    DM(5, I3) = R0;
    R0 = 0;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
.GLOBAL _bd_desc;
.VAR _bd_desc = DK_MAGIC;
.VAR _bd_desc1 = DK_MACHINE_V1;
.VAR _bd_desc2 = 4;                       // n_params: BEND, INDEX, RATIO, NOISE
.VAR _bd_desc3 = DK_MAX_VOICES;
.VAR _bd_desc4 = 32;                      // 8 words: carrier phase, counter, mod phase, index, mod value, noise state, noise env, noise level
.VAR _bd_desc5 = 12;                      // phase, counter, modulator phase zeroed at note-on
.VAR _bd_desc6 = 1200;
.VAR _bd_desc7 = 200;
.VAR _bd_desc8 = 2;
.VAR _bd_desc9 = 0;
