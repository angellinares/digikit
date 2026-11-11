// test/dyn-dsp-hang: a machine of the SDK contract (<dk/machine/machine.h>) that stops the SHARC on purpose (docs/findings/48-dsp-breadcrumbs.md).
// `render` writes silence for HANG_AFTER - 1 blocks of a note; on the HANG_AFTER-th block it enters an endless loop and never returns: the core is
// halted for the engine, the block handler never reaches its tail, the heartbeats freeze. The point of the module is the diagnosis: the ColdFire's
// debug log must name the step (routine called), the voice and the machine type. A negative conformance fixture (tools/patchkit/pk_conform.py NEGATIVE).
//
// State per voice (8 bytes, cleared at note-on): word 0 the blocks rendered since the note-on.

#include "dk_machine.inc"

#define HANG_AFTER 3

.SECTION/PM seg_pmco;
.GLOBAL _hg_render;
.GLOBAL _hg_note_on;

_hg_render:
    I3 = R4;                              // the state
    R1 = DM(0, I3);
    R1 = R1 + 1;
    DM(0, I3) = R1;
    I1 = R8;
    R0 = DM(DK_IN_W_OUT, I1);
    I0 = R0;
    R0 = 0;
    LCNTR = 32, DO _hg_zloop UNTIL LCE;
_hg_zloop: DM(I0, M6) = R0;
    R2 = HANG_AFTER;
    COMP(R1, R2);
    IF GE JUMP _hg_stuck;
    R0 = DK_OK;
    JUMP (M13, I13);
_hg_stuck:
    NOP;
    JUMP _hg_stuck;

// (not at offset 0 of the image: an offset of 0 reads as "no note_on" in the manifest and the pack)
_hg_note_on:
    R0 = 0;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
// dk_machine_desc: magic, contract, n_params, voices, state_bytes, state_clear_bytes, max_cycles_block, max_cycles_note_on, max_call_depth, stack_bytes.
.GLOBAL _hg_desc;
.VAR _hg_desc = DK_MAGIC;
.VAR _hg_desc1 = DK_MACHINE_V1;
.VAR _hg_desc2 = 0;
.VAR _hg_desc3 = DK_MAX_VOICES;
.VAR _hg_desc4 = 8;
.VAR _hg_desc5 = 8;
.VAR _hg_desc6 = 100;
.VAR _hg_desc7 = 100;
.VAR _hg_desc8 = 0;
.VAR _hg_desc9 = 0;
