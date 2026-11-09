#include "dk_machine.inc"
.SECTION/PM seg_pmco;
.GLOBAL _np_render;
.GLOBAL _np_note_on;
_np_render:
    I1 = R8;
    R0 = DM(DK_IN_W_OUT, I1);
    I0 = R0;
    R0 = 0;
    LCNTR = DK_BLOCK, DO _np_zero UNTIL LCE;
_np_zero: DM(I0, M6) = R0;
    R0 = DK_OK;
    JUMP (M13, I13);
_np_note_on:
    R0 = DK_OK;
    JUMP (M13, I13);
.SECTION/DM seg_dmda;
.GLOBAL _np_desc;
.VAR _np_desc = DK_MAGIC;
.VAR _np_d1 = DK_MACHINE_V1;
.VAR _np_d2 = 2;
.VAR _np_d3 = DK_MAX_VOICES;
.VAR _np_d4 = 0;
.VAR _np_d5 = 0;
.VAR _np_d6 = 100;
.VAR _np_d7 = 100;
.VAR _np_d8 = 0;
.VAR _np_d9 = 0;
