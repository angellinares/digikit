// test/dyn-fx-gain-sendonly: the FX contract's smallest run-time effect (<dk/fx/fx.h>): out = in * p0, and one state word that counts the calls. The effect is
// loaded at run time (a .dkm with a DSP section of version 4): the runtime places and relocates this object, reserves the state behind it and
// publishes a free row of the chain with p0 = 0.5 (the DK_FX knob default).

#include "dk_fx.inc"

.SECTION/PM seg_pmco;
.GLOBAL _gso_process;

_gso_process:                              // R4 = state, R8 = dk_fx_in, R12 = the row's parameter words
    I4 = R4;
    R0 = DM(0, I4);
    R0 = R0 + 1;
    DM(0, I4) = R0;                       // calls so far
    I2 = R8;                              // the dk_fx_in
    I0 = DM(DK_FX_IN_W_IN, I2);
    I1 = DM(DK_FX_IN_W_OUT, I2);
    I3 = R12;
    R2 = DM(0, I3);                       // p0: the gain
    LCNTR = DK_FX_BLOCK, DO _gso_loop UNTIL LCE;
        R0 = DM(I0, M6);
        F0 = F0 * F2;
_gso_loop: DM(I1, M6) = R0;
    R0 = DK_FX_OK;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
// dk_fx_desc: magic, contract, n_params, state_bytes, max_cycles_block, max_call_depth, stack_bytes.
.GLOBAL _gso_desc;
.VAR _gso_desc = DK_FX_MAGIC;
.VAR _gso_desc1 = DK_FX_V1;
.VAR _gso_desc2 = 1;
.VAR _gso_desc3 = 4;
.VAR _gso_desc4 = 220;
.VAR _gso_desc5 = 0;
.VAR _gso_desc6 = 0;
