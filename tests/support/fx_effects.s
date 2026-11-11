// Test effects of the FX contract (<dk/fx/fx.h>) for tests/support/fx_rig.py: small routines that the adapter core/fx must call, check and
// crossfade. Entered with I13 = return; R4 = state, R8 = dk_fx_in, R12 = parameter words. One description for all of them.

#include "dk_fx.inc"

.SECTION/PM seg_pmco;
.GLOBAL _te_half;
.GLOBAL _te_add1;
.GLOBAL _te_nan;
.GLOBAL _te_big;
.GLOBAL _te_neg;
.GLOBAL _te_param;
.GLOBAL _te_trash;
.GLOBAL _te_stray;

// out = 0.5 in; state word 0 counts the calls since the state was zeroed
_te_half:
    I2 = R8;
    I3 = R4;
    R5 = DM(0, I3);
    R5 = R5 + 1;
    DM(0, I3) = R5;
    I0 = DM(DK_FX_IN_W_IN, I2);
    I1 = DM(DK_FX_IN_W_OUT, I2);
    R2 = 0x3f000000;
    LCNTR = DK_FX_BLOCK, DO _te_hl UNTIL LCE;
        R0 = DM(I0, M6);
        F0 = F0 * F2;
_te_hl: DM(I1, M6) = R0;
    R0 = DK_FX_OK;
    JUMP (M13, I13);

// out = in + 1.0; state word 0 counts the calls
_te_add1:
    I2 = R8;
    I3 = R4;
    R5 = DM(0, I3);
    R5 = R5 + 1;
    DM(0, I3) = R5;
    I0 = DM(DK_FX_IN_W_IN, I2);
    I1 = DM(DK_FX_IN_W_OUT, I2);
    R2 = 0x3f800000;
    LCNTR = DK_FX_BLOCK, DO _te_al UNTIL LCE;
        R0 = DM(I0, M6);
        F0 = F0 + F2;
_te_al: DM(I1, M6) = R0;
    R0 = DK_FX_OK;
    JUMP (M13, I13);

// out[0] = NaN, the rest 0
_te_nan:
    I2 = R8;
    I1 = DM(DK_FX_IN_W_OUT, I2);
    R0 = 0;
    LCNTR = DK_FX_BLOCK, DO _te_nl UNTIL LCE;
_te_nl: DM(I1, M6) = R0;
    I1 = DM(DK_FX_IN_W_OUT, I2);
    R0 = 0x7fc00000;
    DM(0, I1) = R0;
    R0 = DK_FX_OK;
    JUMP (M13, I13);

// out = 9.0 everywhere (above the level bound)
_te_big:
    I2 = R8;
    I1 = DM(DK_FX_IN_W_OUT, I2);
    R0 = 0x41100000;
    LCNTR = DK_FX_BLOCK, DO _te_bl UNTIL LCE;
_te_bl: DM(I1, M6) = R0;
    R0 = DK_FX_OK;
    JUMP (M13, I13);

// out = in, result -1 (a fault of the module itself)
_te_neg:
    I2 = R8;
    I0 = DM(DK_FX_IN_W_IN, I2);
    I1 = DM(DK_FX_IN_W_OUT, I2);
    LCNTR = DK_FX_BLOCK, DO _te_gl UNTIL LCE;
        R0 = DM(I0, M6);
_te_gl: DM(I1, M6) = R0;
    R0 = -1;
    JUMP (M13, I13);

// out = float(parameter word 0) everywhere; state word 1 = the block number
_te_param:
    I2 = R8;
    I3 = R4;
    R5 = DM(DK_FX_IN_W_BLOCK, I2);
    DM(1, I3) = R5;
    I0 = R12;
    R0 = DM(0, I0);
    F2 = FLOAT R0;
    I1 = DM(DK_FX_IN_W_OUT, I2);
    LCNTR = DK_FX_BLOCK, DO _te_pl UNTIL LCE;
_te_pl: DM(I1, M6) = R2;
    R0 = DK_FX_OK;
    JUMP (M13, I13);

// out = in, after the module clobbered every register its contract allows
_te_trash:
    I2 = R8;
    I0 = DM(DK_FX_IN_W_IN, I2);
    I1 = DM(DK_FX_IN_W_OUT, I2);
    LCNTR = DK_FX_BLOCK, DO _te_tl UNTIL LCE;
        R0 = DM(I0, M6);
_te_tl: DM(I1, M6) = R0;
    R0 = 0x1234;
    R1 = R0;
    R2 = R0;
    R3 = R0;
    R4 = R0;
    R5 = R0;
    R6 = R0;
    R7 = R0;
    R8 = R0;
    R9 = R0;
    R10 = R0;
    R11 = R0;
    R12 = R0;
    R13 = R0;
    R14 = R0;
    R15 = R0;
    I0 = R0;
    I1 = R0;
    I2 = R0;
    I3 = R0;
    I4 = R0;
    I5 = R0;
    M1 = R0;
    M2 = R0;
    M3 = R0;
    M4 = R0;
    B3 = R0;
    R0 = DK_FX_OK;
    JUMP (M13, I13);

// out = in, and a store into the input block (the master, read only for an effect): the write-set check must see it
_te_stray:
    I2 = R8;
    I0 = DM(DK_FX_IN_W_IN, I2);
    I1 = DM(DK_FX_IN_W_OUT, I2);
    LCNTR = DK_FX_BLOCK, DO _te_sl UNTIL LCE;
        R0 = DM(I0, M6);
_te_sl: DM(I1, M6) = R0;
    I0 = DM(DK_FX_IN_W_IN, I2);
    DM(3, I0) = R0;
    R0 = DK_FX_OK;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
// dk_fx_desc: magic, contract, n_params, state_bytes, max_cycles_block, max_call_depth, stack_bytes.
.GLOBAL _te_desc;
.VAR _te_desc = DK_FX_MAGIC;
.VAR _te_desc1 = DK_FX_V1;
.VAR _te_desc2 = 1;
.VAR _te_desc3 = 16;
.VAR _te_desc4 = 600;
.VAR _te_desc5 = 1;
.VAR _te_desc6 = 16;
.PKBSS _te_state_a, 16;
.PKBSS _te_state_b, 16;
