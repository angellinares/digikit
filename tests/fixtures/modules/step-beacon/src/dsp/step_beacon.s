// test/step-beacon: diagnostic effect. The audio passes unchanged. Each call increments one counter and stores one word in `DM(_dk_step)` (diagnostic
// word 25 of the DSP reply). The word cycles every two calls (only every second block's store is delivered) through: 0xBE000000 | (counter & 0xFFFFFF),
// then the initialised words _sb_k1, _sb_k2 and the first word of the descriptor, so the host can check that the module's initialised data arrived.
// No other state, no RECIPS, no MODE1 or ASTAT changes by design.

#include "dk_fx.inc"

.EXTERN _dk_step;

.SECTION/PM seg_pmco;
.GLOBAL _sb_process;

_sb_process:                              // R4 = state, R8 = dk_fx_in, R12 = parameter words (none)
    I2 = R8;
    I0 = DM(DK_FX_IN_W_IN, I2);
    I1 = DM(DK_FX_IN_W_OUT, I2);
    LCNTR = DK_FX_BLOCK, DO _sb_cp UNTIL LCE;
        R0 = DM(I0, M6);
_sb_cp: DM(I1, M6) = R0;
    R0 = DM(_sb_cnt);
    R0 = R0 + 1;
    DM(_sb_cnt) = R0;
    R2 = LSHIFT R0 BY -1;                 // the word changes every two calls
    R1 = 3;
    R2 = R2 AND R1;
    IF EQ JUMP _sb_c;
    R1 = 1;
    COMP(R2, R1);
    IF EQ JUMP _sb_m1;
    R1 = 2;
    COMP(R2, R1);
    IF EQ JUMP _sb_m2;
    R0 = DM(_sb_desc);                    // the descriptor magic
    JUMP _sb_out;
_sb_m1:
    R0 = DM(_sb_k1);
    JUMP _sb_out;
_sb_m2:
    R0 = DM(_sb_k2);
    JUMP _sb_out;
_sb_c:
    R1 = 0xFFFFFF;
    R0 = R0 AND R1;
    R1 = 0xBE000000;
    R0 = R0 OR R1;
_sb_out:
    DM(_dk_step) = R0;
    R0 = DK_FX_OK;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
// dk_fx_desc: magic, contract, n_params, state_bytes, max_cycles_block, max_call_depth, stack_bytes.
.GLOBAL _sb_desc;
.VAR _sb_desc = DK_FX_MAGIC;
.VAR _sb_desc1 = DK_FX_V1;
.VAR _sb_desc2 = 0;
.VAR _sb_desc3 = 4;
.VAR _sb_desc4 = 40000;
.VAR _sb_desc5 = 0;
.VAR _sb_desc6 = 0;
.VAR _sb_cnt = 0;
.VAR _sb_k1 = 0x5A5A1234;
.VAR _sb_k2 = 0xC0DE0001;
