/* The effect page of test/dyn-fx-gain-b: one effect `gain` with one knob. patchkit pack reads these records; the device never sees them.
   Tab GNB. GAIN is p0, 0 to 1.27: the shown value 0 .. 127 is the gain in percent, so the default 25 is 0.25.
   The MIX knob starts at 64. */
#include <dk/fx/fx.h>

DK_FX(gain, .process = "_dgb_process", .desc = "_dgb_desc", .mix = 64);
DK_FX_PARAM(gain, 0, "Gain", "GAIN", DK_PERCENT, 0, 1.27, 0.25);
DK_PAGE(gain, "GNB", "Test Gain B", DK_KNOB_AT(0, 0));
