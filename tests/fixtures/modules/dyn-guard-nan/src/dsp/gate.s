// A machine of the SDK contract that shows its input's gate: every sample of its block is the float `in->gate`.
// tests/patchkit/test_dsp_voice_gate.py runs it through core/dsp-voice in tests/support/machine_rig.py.

#include "dk_machine.inc"

.SECTION/PM seg_pmco;
.GLOBAL _gp_render;
.GLOBAL _gp_note_on;

_gp_note_on:
    R0 = 0;
    JUMP (M13, I13);

_gp_render:
    I0 = R8;
    R0 = DM(DK_IN_W_GATE, I0);
    F0 = FLOAT R0;
    R1 = DM(DK_IN_W_OUT, I0);
    I1 = R1;
    M1 = 1;
    LCNTR = DK_BLOCK, DO _gp_w UNTIL LCE;
_gp_w: DM(I1, M1) = R0;
    R0 = DK_OK;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
.GLOBAL _gp_desc;
.VAR _gp_desc = DK_MAGIC;
.VAR _gp_desc1 = DK_MACHINE_V1;
.VAR _gp_desc2 = 0;
.VAR _gp_desc3 = DK_MAX_VOICES;
.VAR _gp_desc4 = 4;
.VAR _gp_desc5 = 0;
.VAR _gp_desc6 = 200;
.VAR _gp_desc7 = 200;
.VAR _gp_desc8 = 0;
.VAR _gp_desc9 = 0;
