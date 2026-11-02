// A machine of the SDK contract that shows its input: samples 0 to 7 are its parameter words times 2^-12 (below the guard's bound),
// the rest `in->gate`.
// tests/patchkit/test_dsp_voice_input.py runs it through core/dsp-voice in tests/support/machine_rig.py.

#include "dk_machine.inc"

.SECTION/PM seg_pmco;
.GLOBAL _pr_render;
.GLOBAL _pr_note_on;

_pr_note_on:
    R0 = 0;
    JUMP (M13, I13);

_pr_render:
    I0 = R8;
    R1 = DM(DK_IN_W_OUT, I0);
    I1 = R1;
    I2 = R12;
    M1 = 1;
    R2 = 0x39800000;                      // 2^-12
    LCNTR = DK_MAX_PARAMS, DO _pr_p UNTIL LCE;
        R0 = DM(I2, M1);
        F0 = FLOAT R0;
        F0 = F0 * F2;
_pr_p:  DM(I1, M1) = R0;
    R0 = DM(DK_IN_W_GATE, I0);
    F0 = FLOAT R0;
    LCNTR = DK_BLOCK - DK_MAX_PARAMS, DO _pr_g UNTIL LCE;
_pr_g:  DM(I1, M1) = R0;
    R0 = DK_OK;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
.GLOBAL _pr_desc;
.VAR _pr_desc = DK_MAGIC;
.VAR _pr_desc1 = DK_MACHINE_V1;
.VAR _pr_desc2 = 0;
.VAR _pr_desc3 = DK_MAX_VOICES;
.VAR _pr_desc4 = 4;
.VAR _pr_desc5 = 0;
.VAR _pr_desc6 = 300;
.VAR _pr_desc7 = 300;
.VAR _pr_desc8 = 0;
.VAR _pr_desc9 = 0;
