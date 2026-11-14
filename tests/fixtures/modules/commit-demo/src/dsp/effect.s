// test/commit-demo: the effect's DSP routine (<dk/fx/fx.h> contract): it passes the block through. Symbols `_cfx_process` and `_cfx_desc` for the effect id `cfx`.

#include "dk_fx.inc"

.SECTION/PM seg_pmco;
.GLOBAL _cfx_process;

_cfx_process:                             // R4 = state, R8 = dk_fx_in, R12 = the row's parameter words
    R0 = DK_FX_OK;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
// dk_fx_desc: magic, contract, n_params, state_bytes, max_call_depth, stack_bytes.
.GLOBAL _cfx_desc;
.VAR _cfx_desc = DK_FX_MAGIC;
.VAR _cfx_desc1 = DK_FX_V1;
.VAR _cfx_desc2 = 1;
.VAR _cfx_desc3 = 4;
.VAR _cfx_desc4 = 0;
.VAR _cfx_desc5 = 0;
