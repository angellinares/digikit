/* The machine declaration of fadedlimes/bd: one machine `bd` with four parameters. patchkit pack reads these records;
   the device never sees them. The DSP reads the words in this order: BEND, INDEX, RATIO, NOISE (src/dsp/bd.s).
   TUNE (slot 0) is the kick's note. The parameters take the first positions of the device's param_slots list:
   1, 2 and 4 on the Digitakt II. */
#include <dk/machine/machine.h>

DK_MACHINE(bd, .name = "Bass Drum", .short_name = "BD", .render = "_bd_render",
           .note_on = "_bd_note_on", .desc = "_bd_desc");
DK_PARAM(bd, 0, bd_bend, "Bend", "BEND", 0, 127, 64);
DK_PARAM(bd, 1, bd_index, "FM Index", "INDEX", 0, 127, 32);
DK_PARAM(bd, 2, bd_ratio, "Ratio", "RATIO", 0, 127, 0);
DK_PARAM(bd, 3, bd_noise, "Noise", "NOISE", 0, 127, 0);
