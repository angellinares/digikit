/* The effect of test/dsp-facts: one master effect with no knob and no page. patchkit pack reads these records; the device never sees them.
   The audio passes unchanged (the routine copies its input), so MIX does not matter. */
#include <dk/fx.h>

DK_FX(facts, .process = "_fc_process", .desc = "_fc_desc", .mix = 127);
