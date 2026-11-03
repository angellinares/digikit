/* The machine declaration of tom/tb303: one machine `tb303` with six parameters. patchkit pack reads these records; the device never
   sees them. The DSP reads the words in this order: WAVE, ACC, CUT, RES, ENV, DEC (src/dsp/tb303.s). On the Digitakt II they take
   knobs 1, 2, 4, 5, 6 and 7; DEC on knob 7 (LEV) gives the track a fixed level. */
#include <dk/machine.h>

DK_MACHINE(tb303, .name = "TB303", .short_name = "303", .render = "_tb_render",
           .note_on = "_tb_note_on", .desc = "_tb_desc");
DK_PARAM(tb303, 0, tb303_wave, "Wave", "WAVE", 0, 1, 0);
DK_PARAM(tb303, 1, tb303_acc, "Accent", "ACC", 0, 127, 127);
DK_PARAM(tb303, 2, tb303_cut, "Cutoff", "CUT", 0, 127, 64);
DK_PARAM(tb303, 3, tb303_res, "Resonance", "RES", 0, 127, 64);
DK_PARAM(tb303, 4, tb303_env, "Env Mod", "ENV", 0, 127, 64);
DK_PARAM(tb303, 5, tb303_dec, "Decay", "DEC", 0, 127, 64);
