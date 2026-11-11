/* The machine declaration of digikit/pluck: one machine `pluck` with four parameters. patchkit pack reads these records;
   the device never sees them. The DSP reads the words in this order: DECAY, PICK, BRIGHT, COLOR (src/dsp/pluck.s).
   The twin test/patched-pluck (tests/fixtures/modules) has its own copy of the DSP source; keep the two equal. */
#include <dk/machine/machine.h>

DK_MACHINE(pluck, .name = "Pluck", .short_name = "PLK", .render = "_pk_render",
           .note_on = "_pk_note_on", .desc = "_pk_desc");
DK_PARAM(pluck, 0, pluck_decay, "Decay", "DECAY", 0, 127, 64);
DK_PARAM(pluck, 1, pluck_pick, "Pick position", "PICK", 0, 127, 32);
DK_PARAM(pluck, 2, pluck_bright, "Brightness", "BRIGHT", 0, 127, 80);
DK_PARAM(pluck, 3, pluck_color, "Excitation colour", "COLOR", 0, 127, 40);
DK_PAGE(pluck, "PLUCK", "Pluck", DK_KNOB(0), DK_KNOB(1), DK_KNOB(2), DK_KNOB(3));
