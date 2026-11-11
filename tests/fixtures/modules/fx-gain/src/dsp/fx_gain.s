// test/fx-gain: the FX contract's smallest effect (<dk/fx/fx.h>): out = in * gain, gain 0.5 as built. It is the fixture of core/fx's tests: the
// emulator proof checks that the master block, the DAC ring and the USB ring all carry the scaled signal. `_fg_gain` is a data word (an IEEE
// float) that the emulator's dsp.patch can change at run time, like a parameter.

#include "dk_fx.inc"

.SECTION/PM seg_pmco;
.GLOBAL _fg_process;

_fg_process:
    I2 = R8;                              // the dk_fx_in
    I0 = DM(DK_FX_IN_W_IN, I2);
    I1 = DM(DK_FX_IN_W_OUT, I2);
    R2 = DM(_fg_gain);
    LCNTR = DK_FX_BLOCK, DO _fg_loop UNTIL LCE;
        R0 = DM(I0, M6);
        F0 = F0 * F2;
_fg_loop: DM(I1, M6) = R0;
    R0 = DK_FX_OK;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
.GLOBAL _fg_gain;
.VAR _fg_gain = 0x3f000000;
// dk_fx_desc: magic, contract, n_params, state_bytes, max_cycles_block, max_call_depth, stack_bytes.
.GLOBAL _fg_desc;
.VAR _fg_desc = DK_FX_MAGIC;
.VAR _fg_desc1 = DK_FX_V1;
.VAR _fg_desc2 = 0;
.VAR _fg_desc3 = 4;
.VAR _fg_desc4 = 210;
.VAR _fg_desc5 = 0;
.VAR _fg_desc6 = 0;
.PKBSS _fg_state, 4;
