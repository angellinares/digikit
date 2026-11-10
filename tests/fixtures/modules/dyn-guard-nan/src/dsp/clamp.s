// Guard fixture (rig only, not linked into the module): a machine of the SDK contract whose block is the first parameter word times 2^-12, in every
// sample. With the parameter inside 0 .. 0x7f00 the level stays below 8.0 (the clamp of the adapter keeps a frame word beyond it out of the
// module); a row that declares a wider range lets 0xffff through (16.0), and the guard steps in. Not a conformance fixture: tests/patchkit/test_dsp_guard_contract.py.

#include "dk_machine.inc"

.SECTION/PM seg_pmco;
.GLOBAL _cp_render;
.GLOBAL _cp_note_on;

_cp_render:
    I1 = R12;
    R2 = DM(0, I1);                       // the first parameter word
    F2 = FLOAT R2;
    R3 = 0x39800000;                      // 2^-12
    F2 = F2 * F3;
    I1 = R8;
    R0 = DM(DK_IN_W_OUT, I1);
    I0 = R0;
    LCNTR = 32, DO _cp_loop UNTIL LCE;
_cp_loop: DM(I0, M6) = R2;
    R0 = DK_OK;
    JUMP (M13, I13);

// (not at offset 0 of the image: an offset of 0 reads as "no note_on" in the manifest and the pack)
_cp_note_on:
    R0 = 0;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
.GLOBAL _cp_desc;
.VAR _cp_desc = DK_MAGIC;
.VAR _cp_desc1 = DK_MACHINE_V1;
.VAR _cp_desc2 = 1;
.VAR _cp_desc3 = DK_MAX_VOICES;
.VAR _cp_desc4 = 8;
.VAR _cp_desc5 = 0;
.VAR _cp_desc6 = 100;
.VAR _cp_desc7 = 100;
.VAR _cp_desc8 = 0;
.VAR _cp_desc9 = 0;
