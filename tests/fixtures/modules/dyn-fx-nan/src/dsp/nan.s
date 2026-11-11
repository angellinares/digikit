// test/dyn-fx-nan: an effect of the FX contract (<dk/fx/fx.h>) whose output block is NaN in every sample. core/fx's output check counts it as a fault
// (the master or send block stays as it was), the 8th fault within 1500 blocks switches the row off.

#include "dk_fx.inc"

.SECTION/PM seg_pmco;
.GLOBAL _fnn_process;

_fnn_process:                             // R4 = state, R8 = dk_fx_in, R12 = the row's parameter words
    I2 = R8;                              // the dk_fx_in
    I1 = DM(DK_FX_IN_W_OUT, I2);
    R3 = 0x7fc00000;                      // a quiet NaN
    LCNTR = DK_FX_BLOCK, DO _fnn_loop UNTIL LCE;
_fnn_loop: DM(I1, M6) = R3;
    R0 = DK_FX_OK;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
// dk_fx_desc: magic, contract, n_params, state_bytes, max_cycles_block, max_call_depth, stack_bytes.
.GLOBAL _fnn_desc;
.VAR _fnn_desc = DK_FX_MAGIC;
.VAR _fnn_desc1 = DK_FX_V1;
.VAR _fnn_desc2 = 1;
.VAR _fnn_desc3 = 4;
.VAR _fnn_desc4 = 220;
.VAR _fnn_desc5 = 0;
.VAR _fnn_desc6 = 0;
