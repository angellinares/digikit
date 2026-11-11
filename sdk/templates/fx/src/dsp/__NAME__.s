// __ID__: an effect of the FX contract (<dk/fx/fx.h>): out = in * gain, and one state word that counts the calls.
// The effect is loaded at run time: the runtime places and relocates this object, reserves the state behind it and
// publishes a free row of the chain with the knob at its default.

#include "dk_fx.inc"
#include "__NAME__.h"
// Trace (optional, nothing in the object without `pack --trace`): add `#include "dk_trace.inc"` above, then write `DK_STEP(1, R0);` where a fault
// record should say "STEP 1". a register that is free there. See sdk/README.md, "Tracing".

.SECTION/PM seg_pmco;
.GLOBAL ___NAME___process;

___NAME___process:                        // R4 = state, R8 = dk_fx_in, R12 = the row's parameter words
    I4 = R4;
    R0 = DM(0, I4);
    R0 = R0 + 1;
    DM(0, I4) = R0;                       // calls so far
    I2 = R8;                              // the dk_fx_in
    I0 = DM(DK_FX_IN_W_IN, I2);
    I1 = DM(DK_FX_IN_W_OUT, I2);
    I3 = R12;
    R2 = DM(__UPPER___GAIN_WORD, I3);     // the GAIN word
    LCNTR = DK_FX_BLOCK, DO ___NAME___loop UNTIL LCE;
        R0 = DM(I0, M6);
        F0 = F0 * F2;
___NAME___loop: DM(I1, M6) = R0;
    R0 = DK_FX_OK;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
// dk_fx_desc: magic, contract, n_params, state_bytes, max_cycles_block, max_call_depth, stack_bytes.
.GLOBAL ___NAME___desc;
.VAR ___NAME___desc = DK_FX_MAGIC;
.VAR ___NAME___desc1 = DK_FX_V1;
.VAR ___NAME___desc2 = __UPPER___KNOBS;
.VAR ___NAME___desc3 = 4;
.VAR ___NAME___desc4 = 220;
.VAR ___NAME___desc5 = 0;
.VAR ___NAME___desc6 = 0;
