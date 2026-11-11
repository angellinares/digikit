/* The machine declaration of fadedlimes/analog: one machine `analog` with four parameters. patchkit pack reads these records;
   the device never sees them. The DSP reads the words in this order: DETUNE, WAVE, SUB, FOLD (src/dsp/analog.s).
   TUNE (0xca) and LEV (0xd1) are the stock rows. The four parameters take the device list's first four positions: 1, 2, 4, 5. */
#include <dk/machine/machine.h>

DK_MACHINE(analog, .name = "Analog", .short_name = "ANA", .render = "_va_render",
           .note_on = "_va_note_on", .desc = "_va_desc");
DK_PARAM(analog, 0, analog_detune, "Detune", "DET", 0, 127, 40);
DK_PARAM(analog, 1, analog_wave, "Wave", "WAVE", 0, 127, 0);
DK_PARAM(analog, 2, analog_sub, "Sub", "SUB", 0, 127, 0);
DK_PARAM(analog, 3, analog_fold, "Fold", "FOLD", 0, 127, 0);
