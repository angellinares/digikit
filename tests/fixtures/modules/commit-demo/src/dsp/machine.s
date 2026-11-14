// test/commit-demo: the machine's DSP routines (<dk/machine/machine.h> contract): they do nothing. `render` ends the voice at once; the symbols follow
// the rule `_<id>_render`, `_<id>_note_on`, `_<id>_desc` of the machine id `cmach`.

#include "dk_machine.inc"

.SECTION/PM seg_pmco;
.GLOBAL _cmach_render;
.GLOBAL _cmach_note_on;

_cmach_render:
    R0 = DK_DONE;
    JUMP (M13, I13);

// (not at offset 0 of the image: an offset of 0 reads as "no note_on" in the pack)
_cmach_note_on:
    R0 = 0;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
// dk_machine_desc: magic, contract, n_params, voices, state_bytes, state_clear_bytes, max_call_depth, stack_bytes.
.GLOBAL _cmach_desc;
.VAR _cmach_desc = DK_MAGIC;
.VAR _cmach_desc1 = DK_MACHINE_V1;
.VAR _cmach_desc2 = 3;
.VAR _cmach_desc3 = DK_MAX_VOICES;
.VAR _cmach_desc4 = 8;
.VAR _cmach_desc5 = 8;
.VAR _cmach_desc6 = 2;
.VAR _cmach_desc7 = 0;
