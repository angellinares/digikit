// test/sine: a pitched sine machine of the SDK contract (<dk/machine/machine.h>), one body for both devices. It is the template of a machine module:
// `note_on` has nothing to do, `render` writes one block and returns DK_OK; after 96,000 samples (2 s) it returns DK_DONE.
// State per voice (8 bytes, cleared at note-on): word 0 the phase (float, 0 .. 1), word 1 the sample count.
// pitch (semitones << 8, the tune folded in): f = 440 * 2^((n - 69) / 12). Level: velocity / (127 << 8) * parameter 0 / 0x7f00 * 0.5.
//
// sin(2 pi p) is -sin(2 pi (p - 0.5)); the argument is folded into [-0.25, 0.25] (2 clip(x, 0.25) - x) and a
// degree-9 odd Taylor polynomial gives |error| < 4e-6.

#include "dk_machine.inc"

.SECTION/PM seg_pmco;
.GLOBAL _sn_render;
.GLOBAL _sn_note_on;
.EXTERN _dv_exp2;

_sn_render:
    I3 = R4;                              // the state
    I2 = R8;                              // the voice input
    R5 = DM(1, I3);
    R0 = DK_BLOCK;
    R5 = R0 + R5;
    DM(1, I3) = R5;                      // count += 32
    R0 = DM(DK_IN_W_PITCH, I2);
    F10 = FLOAT R0;
    R1 = 0x46fe0000;                      // 32512.0 = note 127: above it the phase would advance more than a cycle per sample
    F10 = MIN(F10, F1);
    R1 = 0x39aaaaab;                      // 1 / (256 * 12)
    F0 = F10 * F1;
    CALL _dv_exp2;
    R1 = 0x39329a62;                      // 440 * 2^(-69/12) / 48000
    F4 = F0 * F1;                        // phase increment
    R0 = DM(DK_IN_W_VELOCITY, I2);
    F0 = FLOAT R0;
    R2 = 0x38010204;                      // 1 / 32512
    F0 = F0 * F2;
    I1 = R12;
    R1 = DM(0, I1);                       // parameter 0
    F1 = FLOAT R1;
    F1 = F1 * F2;
    F0 = F0 * F1;
    R1 = 0x3f000000;                      // 0.5
    F9 = F0 * F1;                         // level
    R0 = DM(DK_IN_W_OUT, I2);
    I0 = R0;
    R13 = DM(0, I3);                      // phase
    R14 = 0x3f000000;                     // 0.5
    R15 = 0x3e800000;                     // 0.25
    R11 = 0x3f800000;                     // 1.0
    LCNTR = 32, DO _sn_samples UNTIL LCE;
        F0 = F13 - F14;                   // x = p - 0.5
        F1 = CLIP F0 BY F15;              // clamp to +-0.25
        F2 = F1 + F1;
        F0 = F2 - F0;                     // x' = 2 clip(x) - x
        F1 = F0 * F0;                     // y = x'^2
        R2 = 0x42283c1a;                  // a9
        F2 = F2 * F1;                     // Horner in y: a9 y + a7, ...
        R3 = 0xc2996966;                  // a7
        F2 = F2 + F3;
        F2 = F2 * F1;
        R3 = 0x42a335e3;                  // a5
        F2 = F2 + F3;
        F2 = F2 * F1;
        R3 = 0xc2255de7;                  // a3
        F2 = F2 + F3;
        F2 = F2 * F1;
        R3 = 0x40c90fdb;                  // a1
        F2 = F2 + F3;
        F2 = F2 * F0;                     // sin(2 pi x')
        F2 = F2 * F9;                     // level
        F2 = -F2;
        F13 = F13 + F4;
        F3 = F13 - F11;                   // wrap p at 1
        IF GE F13 = PASS F3;
_sn_samples: DM(I0, M6) = R2;
    DM(0, I3) = R13;
    R0 = 96000;
    COMP(R5, R0);
    IF GE JUMP _sn_done;
    R0 = DK_OK;
    JUMP (M13, I13);
_sn_done:
    R0 = DK_DONE;
    JUMP (M13, I13);

// (not at offset 0 of the image: an offset of 0 reads as "no note_on" in the manifest and the pack)
_sn_note_on:
    R0 = 0;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
// dk_machine_desc: magic, contract, n_params, voices, state_bytes, state_clear_bytes, max_cycles_block, max_cycles_note_on, max_call_depth, stack_bytes.
.GLOBAL _sn_desc;
.VAR _sn_desc = DK_MAGIC;
.VAR _sn_desc1 = DK_MACHINE_V1;
.VAR _sn_desc2 = 1;
.VAR _sn_desc3 = DK_MAX_VOICES;
.VAR _sn_desc4 = 8;
.VAR _sn_desc5 = 8;
.VAR _sn_desc6 = 1600;
.VAR _sn_desc7 = 1700;
.VAR _sn_desc8 = 2;
.VAR _sn_desc9 = 0;
