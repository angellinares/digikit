#include "dk_machine.inc"
#include "dsp/contract/call.inc"
#include "dsp/sample/float.inc"
#include "dsp/sample/pcm16.inc"
#include "playback.inc"

.SECTION/DM seg_dmda;
.GLOBAL _dd_play_desc;
.VAR _dd_play_desc = DK_MAGIC;
.VAR _dd_play_desc1 = DK_MACHINE_V1;
.VAR _dd_play_desc2 = 1;
.VAR _dd_play_desc3 = DK_MAX_VOICES;
.VAR _dd_play_desc4 = 28;
.VAR _dd_play_desc5 = 28;
// Conservative instruction budgets include the 2662 reader ceiling (measured 1854 without the loop option) and bridge.
// The contract's historical field names say cycles; no hardware timing claim.
.VAR _dd_play_desc6 = 2400;
.VAR _dd_play_desc7 = 2600;
.VAR _dd_play_desc8 = 2;
.VAR _dd_play_desc9 = 128;
.GLOBAL _dd_play_state;
.GLOBAL _dd_play_notes;
.GLOBAL _dd_play_blocks;
.GLOBAL _dd_play_done;
.VAR _dd_play_state = 0;
.VAR _dd_play_notes = 0;
.VAR _dd_play_blocks = 0;
.VAR _dd_play_done = 0;

.GLOBAL _dd_play_output;
.VAR _dd_play_output = 0;

.GLOBAL _dd_play_reads;
.VAR _dd_play_reads = 0;
