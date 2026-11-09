// =====================================================================
//  pd : a phase-distortion voice (Casio CZ lineage) for the DT2 SDK contract
// ---------------------------------------------------------------------
//  Phase distortion is not FM: the oscillator reads a sine *through a warped phase*. A piecewise-linear curve runs the
//  first part of the cycle fast and the rest slow (or pausing in the middle), so the wave leans from a sine into
//  something else. The wave stays periodic, so the spectrum is still f0, 2f0, 3f0 ... with different amplitudes -- it
//  morphs along the harmonic series where FM spreads into inharmonic sidebands.
//
//  The warp is one general shape, and the CURVE knob picks its three numbers (all constants, no division -- the
//  assembler has none):
//
//      p' = p + AMOUNT * ( min(s1 * p, 0.5) + s2 * max(p - b2, 0) - p )
//
//      saw        s1 = 2    s2 = 2/3    b2 = 0.25      the CZ saw
//      square     s1 = 2    s2 = 2      b2 = 0.75      a plateau in the middle
//      resonance  s1 = 6    s2 = 6/11   b2 = 1/12      a steep run then a crawl: a moving formant, the CZ signature
//
//  The sine is the SDK fixture's odd polynomial (tests/fixtures/modules/sine): fold the phase to +-0.25 with CLIP, then
//  a Horner in y = fold^2. No table.
//
//  Register map (F and R are ONE file -- this has bitten this repo repeatedly):
//    F13 phase 1   F14 phase 2   F12 increment 1   F7 increment 2
//    F9 level      F6 amount     F11 1.0   F4 0.5   F8 0.25   F5 oscillator 1 (kept while 2 is computed)
//    a sample's scratch: F0 F1 F2 F3 F15, and nothing else may use them
//  The curve's three numbers live in state words 3, 4 and 5 so the loop does not need three more registers.
// =====================================================================
#include "dk_machine.inc"
#include "dsp/oscillator.inc"

.SECTION/PM seg_pmco;
.GLOBAL _pd_render;
.GLOBAL _pd_note_on;
.EXTERN _dv_exp2;

_pd_render:
    I3 = R4;                              // state
    I2 = R8;                              // voice input
    I1 = R12;                             // parameter words

    // ---- one-time bootstrap. Nothing is cleared at note-on (the phases must free-run, see _pd_note_on), so the first
    // call after load has to seed the state itself: unwritten memory reads as NaN on the concrete core, and a NaN
    // phase never wraps. Word 6 carries a magic number once that has happened. ----
    R2 = DM(6, I3);
    R3 = 0x50445f31;                      // "PD_1"
    COMP(R2, R3);
    IF EQ JUMP _pd_booted;
    R0 = 0;
    DM(0, I3) = R0;                       // phase 1
    DM(1, I3) = R0;                       // phase 2
    DM(2, I3) = R0;                       // spare
    DM(7, I3) = R0;                       // snap flag 0: the first block writes the curve numbers straight to the target
    R0 = 0x40000000;
    DM(3, I3) = R0;                       // s1 = 2
    R0 = 0x3f2aaaab;
    DM(4, I3) = R0;                       // s2 = 2/3
    R0 = 0x3e800000;
    DM(5, I3) = R0;                       // b2 = 0.25
    R0 = 0x50445f31;
    DM(6, I3) = R0;                       // booted
_pd_booted:

    // ---- pitch -> increment 1 ----
    R0 = DM(DK_IN_W_PITCH, I2);
    F7 = FLOAT R0;
    R1 = 0x46fe0000;                      // 32512.0 = note 127
    F7 = MIN(F7, F1);
    R1 = 0x39aaaaab;                      // 1 / (256 * 12)
    F0 = F7 * F1;
    CALL _dv_exp2;
    R1 = 0x39329a62;                      // 440 * 2^(-69/12) / 48000
    F12 = F0 * F1;

    // ---- level from velocity ----
    R0 = DM(DK_IN_W_VELOCITY, I2);
    F0 = FLOAT R0;
    R1 = 0x38010204;                      // 1 / 32512
    F0 = F0 * F1;
    R1 = 0x3e800000;                      // 0.25
    F9 = F0 * F1;



    // ---- DETUNE -> increment 2 = increment 1 x (1 + detune x 0.03) ----
    R1 = 0x38010204;
    R0 = DM(3, I1);                       // FOLD, the fourth parameter
    F0 = FLOAT R0;
    F0 = F0 * F1;                         // 0 .. 1
    R1 = 0x40400000;                      // 3.0
    NOP;
    F0 = F0 * F1;
    R1 = 0x3f800000;                      // 1.0
    NOP;
    F0 = F0 + F1;                         // fold gain 1 .. 4
    R1 = F0;
    NOP;
    DM(8, I3) = R1;                       // word 7 is the snap flag, word 8 the fold gain

    R1 = 0x38010204;
    R0 = DM(2, I1);
    F0 = FLOAT R0;
    F0 = F0 * F1;                         // 0 .. 1
    NOP;
    F0 = F0 * F12;                        // x increment 1
    R1 = 0x3cf5c28f;                      // 0.03
    NOP;
    F0 = F0 * F1;
    NOP;
    F7 = F12 + F0;

    // ---- AMOUNT ----
    R1 = 0x38010204;                      // 1 / 32512
    R0 = DM(0, I1);
    F0 = FLOAT R0;
    F6 = F0 * F1;                         // 0 .. 1: 0 is a plain sine, 1 the full warp

    // ---- CURVE: pick the warp's three numbers, then park them in state words 3, 4, 5 ----
    R1 = 0x38010204;
    R0 = DM(1, I1);
    F0 = FLOAT R0;
    F0 = F0 * F1;                         // c 0 .. 1
    R1 = 0x40000000;
    F3 = PASS R1;                         // s1 = 2      (saw by default)
    R1 = 0x3f2aaaab;
    F15 = PASS R1;                        // s2 = 2/3
    R1 = 0x3e800000;
    F5 = PASS R1;                         // b2 = 0.25
    // square at c >= 1/3: s1 = 2, s2 = 2, b2 = 0.75
    R1 = 0x40000000;
    R2 = 0x3eaaaaab;
    NOP;
    F2 = F0 - F2;                         // c - 1/3 (the compare must be adjacent to the conditional)
    IF GE F15 = PASS F1;                  // s2 = 2
    R1 = 0x3f400000;
    R2 = 0x3eaaaaab;
    NOP;
    F2 = F0 - F2;
    IF GE F5 = PASS F1;                   // b2 = 0.75
    // resonance at c >= 2/3: s1 = 6, s2 = 6/11, b2 = 1/12
    R1 = 0x40c00000;
    R2 = 0x3f2aaaab;
    NOP;
    F2 = F0 - F2;
    IF GE F3 = PASS F1;                   // s1 = 6
    R1 = 0x3f0aaaab;
    R2 = 0x3f2aaaab;
    NOP;
    F2 = F0 - F2;
    IF GE F15 = PASS F1;                  // s2 = 6/11
    R1 = 0x3d2aaaab;
    R2 = 0x3f2aaaab;
    NOP;
    F2 = F0 - F2;
    IF GE F5 = PASS F1;                   // b2 = 1/12
    // ---- the three numbers. On the note-on block they are written straight to the target: the voice state does not
    // reliably survive a trigger, so a slew would otherwise restart from the bootstrap's saw defaults at every note
    // and the warp would re-converge over ~12 ms -- a timbre chirp on the onset, which no level ramp can hide. After
    // that they slew, because a curve change that snaps steps the waveform (the click at the square/resonance
    // boundary). ----
    R0 = DM(7, I3);                       // snap flag: 0 on the note-on block (note_on and the bootstrap write 0)
    R0 = PASS R0;
    IF EQ JUMP _pd_snap;
    R2 = DM(3, I3);
    F0 = PASS R2;                         // s1 now
    NOP;
    F1 = F3 - F0;                         // towards the target
    NOP;
    R2 = 0x3e000000;                      // 0.125: about 12 ms to settle, inaudible as a step
    NOP;
    F1 = F1 * F2;
    NOP;
    F0 = F0 + F1;
    NOP;
    R2 = F0;
    NOP;
    DM(3, I3) = R2;
    R2 = DM(4, I3);
    F0 = PASS R2;                         // s2 now
    NOP;
    F1 = F15 - F0;
    NOP;
    R2 = 0x3e800000;
    NOP;
    F1 = F1 * F2;
    NOP;
    F0 = F0 + F1;
    NOP;
    R2 = F0;
    NOP;
    DM(4, I3) = R2;
    R2 = DM(5, I3);
    F0 = PASS R2;                         // b2 now
    NOP;
    F1 = F5 - F0;
    NOP;
    R2 = 0x3e800000;
    NOP;
    F1 = F1 * F2;
    NOP;
    F0 = F0 + F1;
    NOP;
    R2 = F0;
    NOP;
    DM(5, I3) = R2;
    JUMP _pd_slewed;
_pd_snap:
    R2 = F3;
    NOP;
    DM(3, I3) = R2;                       // s1 straight to the target
    R2 = F15;
    NOP;
    DM(4, I3) = R2;                       // s2
    R2 = F5;
    NOP;
    DM(5, I3) = R2;                       // b2
_pd_slewed:
    R0 = 1;
    DM(7, I3) = R0;                       // later blocks slew (R0 is free here: the next line reloads it)

    // ---- constants and state ----
    R0 = DM(DK_IN_W_OUT, I2);
    I0 = R0;
    R13 = DM(0, I3);                      // phase 1
    R14 = DM(1, I3);                      // phase 2
    R11 = 0x3f800000;                     // 1.0
    R4 = 0x3f000000;                      // 0.5
    R8 = 0x3e800000;                      // 0.25
    LCNTR = DK_BLOCK, DO _pd_last UNTIL LCE;

    // ---- oscillator 1: warp the phase, then read the sine ----
    R2 = DM(3, I3);
    F0 = PASS R2;                         // s1
    NOP;
    F0 = F0 * F13;                        // s1 p
    NOP;
    F0 = MIN(F0, F4);                     // first segment, clamped at 0.5
    R2 = DM(4, I3);
    F1 = PASS R2;                         // s2
    R2 = DM(5, I3);
    F2 = PASS R2;                         // b2
    NOP;
    F3 = F13 - F2;                        // p - b2
    NOP;
    R2 = 0;                               // 0.0
    NOP;
    F3 = MAX(F3, F2);                     // the second segment's input
    NOP;
    F3 = F3 * F1;                         // s2 x
    NOP;
    F0 = F0 + F3;                         // the warped phase
    NOP;
    F0 = F0 - F13;                        // how far the warp moves it
    NOP;
    F0 = F0 * F6;                         // x AMOUNT
    NOP;
    F0 = F0 + F13;                        // p'
    NOP;
    F1 = F0 - F4;                         // x = p' - 0.5
    NOP;
    F2 = CLIP F1 BY F8;                   // fold to +-0.25
    F2 = F2 + F2;
    F0 = F2 - F1;
    NOP;
    DK_SINE_POLY_STEP(F2, F0, F1, F3, R2, R3);
    F5 = F2;                              // keep oscillator 1

    // ---- oscillator 2 (same warp, phase 2) ----
    R2 = DM(3, I3);
    F0 = PASS R2;
    NOP;
    F0 = F0 * F14;
    NOP;
    F0 = MIN(F0, F4);
    R2 = DM(4, I3);
    F1 = PASS R2;
    R2 = DM(5, I3);
    F2 = PASS R2;
    NOP;
    F3 = F14 - F2;
    NOP;
    R2 = 0;
    NOP;
    F3 = MAX(F3, F2);
    NOP;
    F3 = F3 * F1;
    NOP;
    F0 = F0 + F3;
    NOP;
    F0 = F0 - F14;
    NOP;
    F0 = F0 * F6;
    NOP;
    F0 = F0 + F14;
    NOP;
    F1 = F0 - F4;
    NOP;
    F2 = CLIP F1 BY F8;
    F2 = F2 + F2;
    F0 = F2 - F1;
    NOP;
    DK_SINE_POLY_STEP(F2, F0, F1, F3, R2, R3);

    // ---- mix the pair and take the level ----
    F3 = F5 + F2;
    NOP;
    F3 = F3 * F4;                        // x 0.5: the pair spans +-1 (F4 stays 0.5)
    R0 = DM(8, I3);                       // fold drive (1 .. 4)
    F0 = PASS R0;
    NOP;
    F2 = F3 * F0;                         // drive x
    NOP;
    F1 = MIN(F2, F11);                   // clamp high at +1
    R0 = 0xbf800000;                      // -1
    NOP;
    F0 = PASS R0;
    NOP;
    F1 = MAX(F1, F0);                    // c = clamp(drive x, +-1)
    F0 = F2 - F1;                        // overshoot
    NOP;
    F0 = F0 + F0;                        // twice
    NOP;
    F2 = F2 - F0;                        // reflect instead of clipping
    NOP;
    F3 = F2;                             // the folded signal
    NOP;
    F3 = F3 * F9;                        // level

    // ---- advance the phases ----
    F13 = F13 + F12;
    F2 = F13 - F11;
    IF GE F13 = PASS F2;
    F14 = F14 + F7;
    F2 = F14 - F11;
    IF GE F14 = PASS F2;
    DM(0, I0) = F3;
_pd_last: MODIFY(I0, 4);

    DM(0, I3) = R13;
    DM(1, I3) = R14;
    R0 = DK_OK;
    JUMP (M13, I13);

// not at offset 0 of the image: an offset of 0 reads as "no note_on" in the manifest and the pack
_pd_note_on:
    // The phases ARE reset (the adapter zeroes state_clear_bytes, the two leading words), so a note starts at
    // sin(0) = 0 -- the same starting condition a sample has. Free-running phases were measured starting a retriggered
    // sine at full amplitude (-0.216), which is a step with a fast amp attack and reads as a click on anything
    // sine-like. The curve numbers past the cleared words are left alone; only the snap flag (word 7, outside the
    // cleared bytes) is zeroed, so render writes the curve numbers straight to the target on the note-on block.
    I3 = R4;
    R0 = 0;
    DM(7, I3) = R0;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
.GLOBAL _pd_desc;
.VAR _pd_desc = DK_MAGIC;
.VAR _pd_desc1 = DK_MACHINE_V1;
.VAR _pd_desc2 = 4;                       // n_params
.VAR _pd_desc3 = DK_MAX_VOICES;
.VAR _pd_desc4 = 36;                      // state bytes (9 words: 2 phases, spare, 3 curve numbers, magic, snap flag, fold gain)
.VAR _pd_desc5 = 8;                       // the two phases are zeroed at note-on (see _pd_note_on)
.VAR _pd_desc6 = 1200;                    // max cycles per block
.VAR _pd_desc7 = 200;
.VAR _pd_desc8 = 2;
.VAR _pd_desc9 = 0;
