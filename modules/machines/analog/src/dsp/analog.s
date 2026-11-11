// machinedev/analog: a virtual-analog synth voice of the SDK contract (<dk/machine/machine.h>).
//
// The machine contract supplies pitch (semitones << 8, the device's tune folded in), velocity and an output block, and the
// track's own AMP and FLTR stages run on what we write. So a subtractive voice only has to supply oscillators: the
// Digitakt's filter, envelopes, level, pan and track FX do the shaping. That is the point of this machine -- a basic voice
// without loading single-cycle waveforms into the sampler.
//
// Voice: two oscillators detuned against each other, both running the WAVE shape, plus a square sub one octave down;
// scaled by velocity. One voice per track (the engine runs the even voice and copies it to the odd one, so a track is mono).
//
// Knobs (SAMPLE's TUNE and LEV sit at the ends of the row):
//   1 DETUNE  0 = unison, 1 = 3 % apart (on two saws that is the rolling reese beat)
//   2 WAVE    0 saw -> 1/3 triangle -> 2/3 pulse (5 %) -> 1 square (50 %): one continuous slider, and it shapes the
//             oscillators themselves, so there is no second oscillator and no blend level
//   4 SUB     sub level, 0 .. 1 over the whole range (a square one octave down)
//   5 FOLD    wave-folder drive on the oscillators only, 1 .. 4 (0 = no fold); the sub is added after the folder
//
// The adapter maps module parameters by position in the device's param_slots [1, 2, 4, 5, 6]: the four parameters here
// take the first four (3 is the sample slot, 0 and 7 are the stock tune and level).
//
// No drive and no noise: the FX overdrive works on the oscillators as it does on a sample, and sample-rate and
// bit-rate reduction crunch them, so both knobs are better spent on the shape and the sub.
//
// State (6 words = 24 bytes; nothing is cleared at note-on): phase 1, phase 2, sub phase, fold drive, magic, note-on ramp counter.
//
// Register map. F and R are one file, so nothing may collide, and a constant must never land on a register that holds a
// signal. Persistent through the sample loop:
//   F13 phase 1   F14 phase 2   F12 increment 1   F7 increment 2
//   F9 level      F5 sub        F6 wave          F11 1.0
//   F0 t2   F1 t1   F15 pulse width (from the WAVE position; the same for both oscillators)
//   F3 carries the mix; per-oscillator scratch is F2, F4, F8 and nothing else may use them.

#include "dk_machine.inc"

.SECTION/PM seg_pmco;
.GLOBAL _va_render;
.GLOBAL _va_note_on;
.EXTERN _dv_exp2;

_va_render:
    I3 = R4;                              // state
    I2 = R8;                              // voice input
    I1 = R12;                             // parameter words

    // ---- one-time bootstrap. Nothing is cleared at note-on (the phases must free-run, see _va_note_on), so the first
    // call after load seeds the state itself: unwritten memory reads as NaN on the concrete core and a NaN phase never
    // wraps. Word 4 carries a magic number once that has happened. ----
    R2 = DM(4, I3);
    R3 = 0x56415f31;                      // "VA_1"
    COMP(R2, R3);
    IF EQ JUMP _va_booted;
    R0 = 0;
    DM(0, I3) = R0;                       // phase 1
    DM(1, I3) = R0;                       // phase 2
    DM(2, I3) = R0;                       // sub phase
    R0 = 0x3f800000;
    DM(3, I3) = R0;                       // fold drive 1.0: a no-op until render rewrites it
    R0 = 0x56415f31;
    DM(4, I3) = R0;                       // booted
_va_booted:

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
    R1 = 0x3e800000;                      // 0.25: two saws, a square and noise stay inside DK_LEVEL_BOUND
    F9 = F0 * F1;
    // ---- note-on ramp, six blocks (about 9 ms). The contract gives no voice index, so the oscillator state has to
    // live in the voice state block, and on hardware something below the adapter clears it at a trigger -- the phases
    // then jump and the waveform steps. This cannot be masked in the state, so the level is brought up over a few
    // milliseconds instead. A trig restarts the amp envelope, so the level is already near zero here and the ramp adds
    // nothing audible; with a long attack it is far below the envelope anyway. (The rig has no envelope, which is why
    // measuring this there made it look like a click of its own.) ----
    R0 = DM(5, I3);                       // blocks since note_on, counted here: DK_IN_W_BLOCK is not 0 on the
    F0 = FLOAT R0;                        // note-on block in practice, so the ramp never ran. Read before the
    R1 = R0 + 1;                          // increment, so the note-on block itself is silent.
    DM(5, I3) = R1;
    R2 = 0x3e2aaaab;                      // 1/6 per block  (R2 IS F2: the constant must be read from F2)
    NOP;
    F0 = F0 * F2;
    R2 = 0x3f800000;                      // 1.0
    NOP;
    F0 = MIN(F0, F2);
    NOP;
    F9 = F9 * F0;



    // ---- DETUNE -> increment 2 = increment 1 x (1 + detune x 0.03). Full range: 0 is two saws in unison and the top
    // is a few percent apart, which on two saws is the rolling reese beat rather than a mild chorus. ----
    R1 = 0x38010204;                      // 1 / 32512
    R0 = DM(0, I1);
    F0 = FLOAT R0;
    F0 = F0 * F1;                         // 0 .. 1
    NOP;
    F0 = F0 * F12;                        // x increment 1
    R1 = 0x3cf5c28f;                      // 0.03
    NOP;
    F0 = F0 * F1;
    NOP;
    F7 = F12 + F0;

    // ---- WAVE shape, the SUB level and the FOLD drive ----
    R1 = 0x38010204;                      // 1 / 32512
    R0 = DM(1, I1);
    F0 = FLOAT R0;
    F6 = F0 * F1;                         // wave: 0 .. 1 across saw, triangle and pulse

    R0 = DM(2, I1);
    F0 = FLOAT R0;
    F5 = F0 * F1;                         // SUB level, the whole range (v0.11 ended the 3/4 cap)

    R0 = DM(3, I1);
    F0 = FLOAT R0;
    F0 = F0 * F1;                         // FOLD 0 .. 1
    R1 = 0x40400000;                      // 3.0
    NOP;
    F0 = F0 * F1;
    R1 = 0x3f800000;                      // 1.0
    NOP;
    F0 = F0 + F1;                         // fold drive 1 .. 4
    R2 = F0;
    NOP;
    DM(3, I3) = R2;                       // state word 3: the drive the reflection folder reads

    R0 = DM(DK_IN_W_OUT, I2);
    I0 = R0;
    R13 = DM(0, I3);                      // phase 1
    R14 = DM(1, I3);                      // phase 2
    R10 = DM(2, I3);                      // sub phase (F10 would be increment 2: never use it here)
    R11 = 0x3f800000;                     // 1.0
    R1 = 0x40000000;                      // 2.0
    F0 = F6 * F1;                         // seg = 2 wave
    NOP;
    R15 = 0;                              // 0.0
    F1 = MIN(F0, F11);                    // t1: saw -> triangle, 0 .. 1
    NOP;
    F0 = F0 - F11;                        // seg - 1
    NOP;
    F0 = MAX(F0, F15);                    // t2: triangle -> pulse, 0 .. 1
    R2 = 0x3ee66666;                      // 0.45
    NOP;
    F15 = F0 * F2;                        // 0.45 t2
    R2 = 0x3d4ccccd;                      // 0.05
    NOP;
    F15 = F15 + F2;                       // pulse width = 0.05 + 0.45 t2
    LCNTR = DK_BLOCK, DO _va_last UNTIL LCE;
    // ---- the two oscillators, both running the WAVE shape ----
    // ---- the two oscillators, both running the WAVE shape ----
    // ---- oscillator 1 (phase 1): saw -> triangle by t1, then -> pulse by t2 ----
    F8 = F13 + F13;                     // 2 ph
    NOP;
    F8 = F8 - F11;                        // saw
    F4 = F11 - F13;                      // 1 - ph
    NOP;
    F4 = MIN(F13, F4);                   // min(ph, 1 - ph)
    NOP;
    F4 = F4 + F4;                         // 2 min
    NOP;
    F4 = F4 + F4;                         // 4 min
    NOP;
    F4 = F4 - F11;                        // triangle = 4 min - 1
    F4 = F4 - F8;                         // triangle - saw
    NOP;
    F4 = F4 * F1;                         // t1
    NOP;
    F8 = F8 + F4;                         // saw -> triangle
    R4 = 0x3e800000;                      // 0.25: the pulse rides a quarter turn on, so it adds to the saw in power
    NOP;
    F2 = F13 + F4;                       // phase + 0.25
    F4 = F2 - F11;                        // phase + 0.25 - 1
    IF GE F2 = PASS F4;                   // wrap to [0, 1) (F-to-F: the integer form does not work on this core)
    R4 = 0x447a0000;                      // 1000
    NOP;
    F2 = F15 - F2;                        // width - frac
    NOP;
    F2 = F2 * F4;                         // x 1000
    R4 = 0x3f800000;                      // +1
    NOP;
    F2 = MIN(F2, F4);
    R4 = 0xbf800000;                      // -1
    NOP;
    F4 = MAX(F2, F4);                     // the pulse, as a clamp rather than a conditional
    NOP;
    F2 = F15 + F15;                       // 2 w
    NOP;
    F2 = F2 - F11;                        // 2 w - 1 = the pulse mean
    F4 = F4 - F2;                         // DC-compensated pulse
    NOP;
    F4 = F4 - F8;                         // pulse - the morph so far
    NOP;
    F4 = F4 * F0;                         // t2
    NOP;
    F8 = F8 + F4;                         // shape 1
    F3 = F8;                              // keep it
    // ---- oscillator 2 (phase 2): saw -> triangle by t1, then -> pulse by t2 ----
    F8 = F14 + F14;                     // 2 ph
    NOP;
    F8 = F8 - F11;                        // saw
    F4 = F11 - F14;                      // 1 - ph
    NOP;
    F4 = MIN(F14, F4);                   // min(ph, 1 - ph)
    NOP;
    F4 = F4 + F4;                         // 2 min
    NOP;
    F4 = F4 + F4;                         // 4 min
    NOP;
    F4 = F4 - F11;                        // triangle = 4 min - 1
    F4 = F4 - F8;                         // triangle - saw
    NOP;
    F4 = F4 * F1;                         // t1
    NOP;
    F8 = F8 + F4;                         // saw -> triangle
    R4 = 0x3e800000;                      // 0.25: the pulse rides a quarter turn on, so it adds to the saw in power
    NOP;
    F2 = F14 + F4;                       // phase + 0.25
    F4 = F2 - F11;                        // phase + 0.25 - 1
    IF GE F2 = PASS F4;                   // wrap to [0, 1) (F-to-F: the integer form does not work on this core)
    R4 = 0x447a0000;                      // 1000
    NOP;
    F2 = F15 - F2;                        // width - frac
    NOP;
    F2 = F2 * F4;                         // x 1000
    R4 = 0x3f800000;                      // +1
    NOP;
    F2 = MIN(F2, F4);
    R4 = 0xbf800000;                      // -1
    NOP;
    F4 = MAX(F2, F4);                     // the pulse, as a clamp rather than a conditional
    NOP;
    F2 = F15 + F15;                       // 2 w
    NOP;
    F2 = F2 - F11;                        // 2 w - 1 = the pulse mean
    F4 = F4 - F2;                         // DC-compensated pulse
    NOP;
    F4 = F4 - F8;                         // pulse - the morph so far
    NOP;
    F4 = F4 * F0;                         // t2
    NOP;
    F8 = F8 + F4;                         // shape 2
    // ---- mix the two, then the sub, then the level ----
    F3 = F3 + F8;
    NOP;
    R2 = 0x3f000000;                      // 0.5
    NOP;
    F3 = F3 * F2;                         // the pair spans +-1
    // ---- WAVE FOLDING on the oscillators only: drive past 1 and *reflect* the overshoot rather than clipping it,
    // y = x - 2(x - clamp(x, +-1)). Each reflection is a discontinuity, so it adds harmonics (grit) instead of the
    // rounded peaks a saturator gives -- the DT2's overdrive already does saturating. The sub is added after, so the low
    // end stays clean, and it all runs before the FLTR page. ----
    R2 = DM(3, I3);                       // drive, refreshed each block (R2, not R0: R0 is F0 = t2)
    F8 = PASS R2;
    F2 = F3 * F8;                         // drive x
    NOP;
    R4 = 0x3f800000;                      // +1
    NOP;
    F4 = MIN(F2, F4);                     // clamp high
    R8 = 0xbf800000;                      // -1  (R8, not R4: F4 holds the clamp result)
    NOP;
    F4 = MAX(F4, F8);                     // c = clamp(drive x, +-1)
    NOP;
    F8 = F2 - F4;                         // how far past the edge
    NOP;
    F8 = F8 + F8;                         // twice
    NOP;
    F2 = F2 - F8;                         // fold: x - 2(x - c) reflects instead of clipping
    NOP;
    F3 = F2;                              // the folded signal
    R2 = 0x3f800000;                      // the +1 default goes to F4 and the constants through R2, so a constant
    F4 = PASS R2;                         // never lands on the register that holds the value
    R2 = 0x3f000000;                      // 0.5
    NOP;
    COMP(F10, F2);                        // the sub is a square from its half-rate phase
    R2 = 0xbf800000;                      // -1
    IF GE F4 = PASS R2;
    NOP;
    F4 = F4 * F5;                         // sub level
    F3 = F3 + F4;
    NOP;
    // (the fold itself is the reflection folder above, which already reads the gain from state word 3. This used to
    // carry a second, clamping folder on top of it -- two folds on one signal, the second multiplying a value the first
    // had consumed, which is why the knob appeared dead.)
    F3 = F3 * F9;                         // level

    // ---- advance the phases ----
    F13 = F13 + F12;
    F2 = F13 - F11;
    IF GE F13 = PASS F2;
    F14 = F14 + F7;
    F2 = F14 - F11;
    IF GE F14 = PASS F2;
    R2 = 0x3f000000;                      // half increment: the sub runs one octave down
    NOP;
    F4 = PASS R2;
    NOP;
    F2 = F12 * F4;
    NOP;
    F10 = F10 + F2;
    F2 = F10 - F11;
    IF GE F10 = PASS F2;
    DM(0, I0) = F3;
_va_last: MODIFY(I0, 4);                  // the DO loop's last instruction

    DM(0, I3) = R13;
    DM(1, I3) = R14;
    DM(2, I3) = R10;
    R0 = DK_OK;
    JUMP (M13, I13);

// not at offset 0 of the image: an offset of 0 reads as "no note_on" in the manifest and the pack
_va_note_on:
    // The phases are NOT reset -- a retrigger while the previous note sounds would jump the waveform. Only the
    // note-on ramp's own counter is cleared, which is what makes the ramp start at zero on every trigger.
    I3 = R4;
    R0 = 0;
    DM(5, I3) = R0;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
// magic, contract, n_params, voices, state_bytes, state_clear_bytes, max_cycles_block, max_cycles_note_on,
// max_call_depth, stack_bytes
.GLOBAL _va_desc;
.VAR _va_desc = DK_MAGIC;
.VAR _va_desc1 = DK_MACHINE_V1;
.VAR _va_desc2 = 4;
.VAR _va_desc3 = DK_MAX_VOICES;
.VAR _va_desc4 = 24;
.VAR _va_desc5 = 0;   // nothing is cleared at note-on: the phases free-run
.VAR _va_desc6 = 1200;
.VAR _va_desc7 = 200;
.VAR _va_desc8 = 2;
.VAR _va_desc9 = 0;
