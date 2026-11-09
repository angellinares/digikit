// Source-track fixture: observe both callbacks and render the supplied track / 16.
#include "dk_machine.inc"
.SECTION/PM seg_pmco;
.GLOBAL _tk_render;
.GLOBAL _tk_note_on;
_tk_render:
    R0 = DM(_tk_renders);
    R0 = R0 + 1;
    DM(_tk_renders) = R0;
    I1 = R8;
    R2 = DM(DK_IN_W_TRACK, I1);
    DM(_tk_render_track) = R2;
    F2 = FLOAT R2;
    R3 = 0x3d800000;                      // 1/16
    F2 = F2 * F3;
    R0 = DM(DK_IN_W_OUT, I1);
    I0 = R0;
    LCNTR = 32, DO _tk_loop UNTIL LCE;
_tk_loop: DM(I0, M6) = R2;
    R0 = DK_OK;
    JUMP (M13, I13);
_tk_note_on:
    R0 = DM(_tk_notes);
    R0 = R0 + 1;
    DM(_tk_notes) = R0;
    I1 = R8;
    R0 = DM(DK_IN_W_TRACK, I1);
    DM(_tk_note_track) = R0;
    R0 = 0;
    JUMP (M13, I13);
.SECTION/DM seg_dmda;
.GLOBAL _tk_desc;
.VAR _tk_desc = DK_MAGIC;
.VAR _tk_desc1 = DK_MACHINE_V1;
.VAR _tk_desc2 = 1;
.VAR _tk_desc3 = DK_MAX_VOICES;
.VAR _tk_desc4 = 8;
.VAR _tk_desc5 = 0;
.VAR _tk_desc6 = 200;
.VAR _tk_desc7 = 300;
.VAR _tk_desc8 = 0;
.VAR _tk_desc9 = 0;
.VAR _tk_notes = 0;
.VAR _tk_renders = 0;
.VAR _tk_note_track = 0;
.VAR _tk_render_track = 0;
