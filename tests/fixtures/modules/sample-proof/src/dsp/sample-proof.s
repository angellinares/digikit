#include "dk_machine.inc"
#include "dsp/call.inc"
#include "dsp/sample.inc"
#include "playback.inc"

// Initialized READY=0 descriptor is sent with the module image; bank is BSS.
.PKDATA _sp_view, 16;
.PKBSS _sp_bank, 32768;

.SECTION/DM seg_dmda;
.GLOBAL _sp_desc;
.VAR _sp_desc = DK_MAGIC;
.VAR _sp_desc1 = DK_MACHINE_V1;
.VAR _sp_desc2 = 3;
.VAR _sp_desc3 = DK_MAX_VOICES;
.VAR _sp_desc4 = 28;
.VAR _sp_desc5 = 28;
.VAR _sp_desc6 = 1500;
// Note-on budget includes the following render (kernel ceiling1380 plus
// initialization and adapter margin); instruction ceiling, not hardware timing.
.VAR _sp_desc7 = 1700;
.VAR _sp_desc8 = 2;
.VAR _sp_desc9 = 128;
