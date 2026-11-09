// test/dyn-fx-hang: an effect of the FX contract (<dk/fx.h>) that stops the SHARC on purpose. The process routine copies its input to its output, stores
// the module step 1 (entered) and 2 (copied) in `DK_STEP` (sdk/asm/dk_trace.inc) and counts its calls in state word 0.
// From the HANG_AFTER-th call on it stores step 3 and enters an endless loop: the core is halted for the engine, the heartbeats freeze, and the fault
// table in the reply record holds the site (FX send), the row, the core step CALL and the module step 2: the step of the last block that ended (the block tail
// copies `_dk_step` into the record), because the step 3 of the call that stops the core is lost with it.
// State (4 bytes): word 0 the calls so far.

#include "dk_fx.inc"
// The test needs the steps in every build, not only in `pack --trace`.
#define DK_TRACE
#include "dk_trace.inc"

#define HANG_AFTER 5000

.SECTION/PM seg_pmco;
.GLOBAL _hf_process;

_hf_process:                              // R4 = state, R8 = dk_fx_in, R12 = the row's parameter words
    I4 = R4;
    R0 = DM(0, I4);
    R0 = R0 + 1;
    DM(0, I4) = R0;                       // calls so far
    DK_STEP(1, R1);                     // step 1: entered
    I2 = R8;                              // the dk_fx_in
    I0 = DM(DK_FX_IN_W_IN, I2);
    I1 = DM(DK_FX_IN_W_OUT, I2);
    LCNTR = DK_FX_BLOCK, DO _hf_loop UNTIL LCE;
        R3 = DM(I0, M6);
_hf_loop: DM(I1, M6) = R3;
    DK_STEP(2, R1);                     // step 2: copied
    R2 = HANG_AFTER;
    COMP(R0, R2);
    IF GE JUMP _hf_stuck;
    R0 = DK_FX_OK;
    JUMP (M13, I13);
_hf_stuck:
    DK_STEP(3, R1);                     // step 3: the endless loop
_hf_spin:
    NOP;
    JUMP _hf_spin;

.SECTION/DM seg_dmda;
// dk_fx_desc: magic, contract, n_params, state_bytes, max_cycles_block, max_call_depth, stack_bytes.
.GLOBAL _hf_desc;
.VAR _hf_desc = DK_FX_MAGIC;
.VAR _hf_desc1 = DK_FX_V1;
.VAR _hf_desc2 = 1;
.VAR _hf_desc3 = 4;
.VAR _hf_desc4 = 220;
.VAR _hf_desc5 = 0;
.VAR _hf_desc6 = 0;
