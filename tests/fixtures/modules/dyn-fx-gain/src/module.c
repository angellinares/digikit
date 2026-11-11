/* The effect page of test/dyn-fx-gain: one effect `gain` with one knob. patchkit pack reads these records; the device never sees them.
   Tab GAN. GAIN is p0, 0 to 1.27: the shown value 0 .. 127 is the gain in percent, so the default 50 is 0.5.
   The MIX knob starts at 127 (wet). */
#include <dk/fx/fx.h>

DK_FX(gain, .process = "_dg_process", .desc = "_dg_desc", .mix = 127);
DK_FX_PARAM(gain, 0, "Gain", "GAIN", DK_PERCENT, 0, 1.27, 0.5);
DK_PAGE(gain, "GAN", "Test Gain", DK_KNOB_AT(0, 0));
