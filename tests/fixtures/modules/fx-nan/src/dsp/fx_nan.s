// test/fx-nan: a negative fixture of the FX contract (<dk/fx/fx.h>). It passes the input on and writes a NaN into sample 5 of its output. The
// adapter of core/fx must refuse the block (it plays dry and counts a fault); the conformance suite fails this module on purpose
// (pk_conform.FX_NEGATIVE) to show that the validator catches it.

#include "dk_fx.inc"

.SECTION/PM seg_pmco;
.GLOBAL _fn_process;

_fn_process:
    I2 = R8;
    I0 = DM(DK_FX_IN_W_IN, I2);
    I1 = DM(DK_FX_IN_W_OUT, I2);
    LCNTR = DK_FX_BLOCK, DO _fn_loop UNTIL LCE;
        R0 = DM(I0, M6);
_fn_loop: DM(I1, M6) = R0;
    I1 = DM(DK_FX_IN_W_OUT, I2);
    R0 = 0x7fc00000;
    DM(5, I1) = R0;
    R0 = DK_FX_OK;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
// dk_fx_desc: magic, contract, n_params, state_bytes, max_cycles_block, max_call_depth, stack_bytes.
.GLOBAL _fn_desc;
.VAR _fn_desc = DK_FX_MAGIC;
.VAR _fn_desc1 = DK_FX_V1;
.VAR _fn_desc2 = 0;
.VAR _fn_desc3 = 4;
.VAR _fn_desc4 = 210;
.VAR _fn_desc5 = 0;
.VAR _fn_desc6 = 0;
.PKBSS _fn_state, 4;
