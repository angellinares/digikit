/* The machine declaration of test/sine: one machine `sine` with one level parameter and a page of one knob.
   patchkit pack reads these records; the device never sees them. */
#include <dk/machine/machine.h>

DK_MACHINE(sine, .name = "Sine", .short_name = "SIN", .render = "_sn_render",
           .note_on = "_sn_note_on", .desc = "_sn_desc");
DK_PARAM(sine, 0, sine_level, "Level", "LEVEL", 0, 127, 127);
DK_PAGE(sine, "SINE", "Sine", DK_KNOB(0));
