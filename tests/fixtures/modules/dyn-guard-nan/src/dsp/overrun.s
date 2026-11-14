// Write-set fixture (rig only, not linked into the module): a machine of the SDK contract whose render writes one word past its own
// state block (8 bytes), into the next voice's state. The output is silence, so only the write-set check of the conformance rig
// (tests/support/machine_rig.py) fails it. pk_conform.NEGATIVE_VARIANTS names it as `test/dyn-guard-nan#overrun`.

#include "dk_machine.inc"

.SECTION/PM seg_pmco;
.GLOBAL _ov_render;
.GLOBAL _ov_note_on;

_ov_render:
    I1 = R8;
    R0 = DM(DK_IN_W_OUT, I1);
    I0 = R0;
    R1 = 0;
    LCNTR = 32, DO _ov_loop UNTIL LCE;
_ov_loop: DM(I0, M6) = R1;
    I2 = R4;
    DM(0, I2) = R1;                       // own state, word 0: allowed
    DM(1, I2) = R1;                       // own state, word 1: allowed
    DM(2, I2) = R1;                       // one word past the 8-byte state block: the next voice's state
    R0 = DK_OK;
    JUMP (M13, I13);

_ov_note_on:
    R0 = 0;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
.GLOBAL _ov_desc;
.VAR _ov_desc = DK_MAGIC;
.VAR _ov_desc1 = DK_MACHINE_V1;
.VAR _ov_desc2 = 0;
.VAR _ov_desc3 = DK_MAX_VOICES;
.VAR _ov_desc4 = 8;
.VAR _ov_desc5 = 0;
.VAR _ov_desc6 = 0;
.VAR _ov_desc7 = 0;
