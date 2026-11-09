/* The effect of test/step-beacon: one master effect with no knob and no page. patchkit pack reads these records; the device never sees them.
   The audio passes unchanged (the routine copies its input), so MIX does not matter. */
#include <dk/fx.h>

DK_FX(beacon, .process = "_sb_process", .desc = "_sb_desc", .mix = 127);
