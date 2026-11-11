// test/dyn-guard-nan: a machine of the SDK contract (<dk/machine/machine.h>) whose render outputs NaN, one body for both devices (fixture of the runtime
// guard of core/dsp-voice, docs/findings/51-pluck-tune-guard.md). The adapter calls note_on once per note and render once per block while the
// voice runs; the guard zeroes the block, ends the voice and counts the fault.

#include "dk_machine.inc"

.SECTION/PM seg_pmco;
.GLOBAL _gn_render;
.GLOBAL _gn_note_on;

_gn_render:
    I1 = R8;
    R0 = DM(DK_IN_W_OUT, I1);
    I0 = R0;
    R1 = 0x7fc00000;                      // a quiet NaN
    LCNTR = 32, DO _gn_loop UNTIL LCE;
_gn_loop: DM(I0, M6) = R1;
    R0 = DK_OK;
    JUMP (M13, I13);

// (not at offset 0 of the image: an offset of 0 reads as "no note_on" in the manifest and the pack)
_gn_note_on:
    R0 = 0;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
// dk_machine_desc: magic, contract, n_params, voices, state_bytes, state_clear_bytes, max_cycles_block, max_cycles_note_on, max_call_depth, stack_bytes.
.GLOBAL _gn_desc;
.VAR _gn_desc = DK_MAGIC;
.VAR _gn_desc1 = DK_MACHINE_V1;
.VAR _gn_desc2 = 0;
.VAR _gn_desc3 = DK_MAX_VOICES;
.VAR _gn_desc4 = 8;
.VAR _gn_desc5 = 0;
.VAR _gn_desc6 = 100;
.VAR _gn_desc7 = 100;
.VAR _gn_desc8 = 0;
.VAR _gn_desc9 = 0;
