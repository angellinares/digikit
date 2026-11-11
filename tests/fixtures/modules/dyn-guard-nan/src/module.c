/* The machine declaration of test/dyn-guard-nan: one machine `guardnan` with one parameter and a page of one knob
   (a machine without a page has nothing to show). patchkit pack reads these records; the device never sees them.
   The render is the same body on both devices. */
#include <dk/machine/machine.h>

DK_MACHINE(guardnan, .name = "NaN fixture", .short_name = "NAN", .render = "_gn_render",
           .note_on = "_gn_note_on", .desc = "_gn_desc");
DK_PARAM(guardnan, 0, nan_a, "Nan alpha", "ALPH", 0, 127, 32);
DK_PAGE(guardnan, "NAN", "NaN fixture", DK_KNOB(0));
