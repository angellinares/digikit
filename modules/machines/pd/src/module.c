/* The machine declaration of fadedlimes/pd: one machine `pd` with four parameters. patchkit pack reads these records;
   the device never sees them. The DSP reads the words in this order: AMOUNT, CURVE, DETUNE, FOLD (src/dsp/pd.s).
   SAMPLE's TUNE (0xca) and LEV (0xd1) are at the ends. The DSP reads params[i] from the knob at param_slots[i]
   (Digitakt II 1.17 and Digitone II 1.12: 1, 2, 4, 5). */
#include <dk/machine.h>

DK_MACHINE(pd, .name = "Phase Dist", .short_name = "PD", .render = "_pd_render",
           .note_on = "_pd_note_on", .desc = "_pd_desc");
DK_PARAM(pd, 0, pd_amount, "Distortion", "PD", 0, 127, 0);
DK_PARAM(pd, 1, pd_curve, "Curve", "CURVE", 0, 127, 0);
DK_PARAM(pd, 2, pd_detune, "Detune", "DET", 0, 127, 40);
DK_PARAM(pd, 3, pd_fold, "Fold", "FOLD", 0, 127, 0);
