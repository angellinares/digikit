/* The effect of test/sharc-probe: one master effect with no knob and no page. patchkit pack reads these records; the device never sees them.
   The audio passes unchanged (the routine copies its input), so MIX does not matter. */
#include <dk/fx.h>

DK_FX(probe, .process = "_pr_process", .desc = "_pr_desc", .mix = 127);
