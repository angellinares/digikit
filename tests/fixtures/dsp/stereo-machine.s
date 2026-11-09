// Test-only stereo machine (machine contract 1.2) for the core/dsp-voice tests: L is the constant 0.25, R is chosen by parameter word 0
// (UI units): 0 gives 0.5, 1 gives 16.0 (over the level bound), 2 gives a NaN. The status is DK_OK | DK_STEREO.
#include "dk_machine.inc"

.SECTION/PM seg_pmco;
.GLOBAL _sm_render;
.GLOBAL _sm_note_on;

_sm_render:
    I2 = R8;
    I4 = R12;
    NOP; NOP; NOP;
    R0 = DM(0, I4);
    R0 = LSHIFT R0 BY -8;
    R1 = 0x3f000000;                      // 0.5
    R2 = 1;
    COMP(R0, R2);
    R3 = 0x41800000;                      // 16.0
    IF EQ R1 = PASS R3;
    R2 = 2;
    COMP(R0, R2);
    R3 = 0x7fc00000;                      // NaN
    IF EQ R1 = PASS R3;
    R0 = DM(DK_IN_W_OUT, I2);
    I0 = R0;
    R0 = DM(DK_IN_W_OUT_R, I2);
    I1 = R0;
    R2 = 0x3e800000;                      // 0.25
    NOP; NOP; NOP;
    LCNTR = 32, DO _sm_loop UNTIL LCE;
        DM(I1, M6) = R1;
_sm_loop: DM(I0, M6) = R2;
    R0 = DK_OK;
    R1 = DK_STEREO;
    R0 = R0 OR R1;
    JUMP (M13, I13);

_sm_note_on:
    R0 = 0;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
.GLOBAL _sm_desc;
.VAR _sm_desc = DK_MAGIC;
.VAR _sm_desc1 = 257;                     // DK_MACHINE_V1 | DK_MACHINE_STEREO
.VAR _sm_desc2 = 1;
.VAR _sm_desc3 = DK_MAX_VOICES;
.VAR _sm_desc4 = 4;
.VAR _sm_desc5 = 4;
.VAR _sm_desc6 = 200;
.VAR _sm_desc7 = 200;
.VAR _sm_desc8 = 0;
.VAR _sm_desc9 = 0;
