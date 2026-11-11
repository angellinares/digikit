/* The machine declaration of test/dyn-dn2: machine `dyn_pluck` with one page of four knobs. patchkit pack reads these records;
   the device never sees them. The DSP is test/sine's: it reads parameter word 0 only, the other three words are not read. */
#include <dk/machine/machine.h>

DK_MACHINE(dyn_pluck, .name = "DynSyn", .short_name = "DSY", .render = "_sn_render",
           .note_on = "_sn_note_on", .desc = "_sn_desc");
DK_PARAM(dyn_pluck, 0, dn_a, "Dyn Alpha", "ALPH", 0, 127, 32);
DK_PARAM(dyn_pluck, 1, dn_b, "Dyn Beta", "BETA", 0, 63, 16);
DK_PARAM(dyn_pluck, 2, dn_c, "Dyn Gamma", "GAMA", 1, 127, 112);
DK_PARAM(dyn_pluck, 3, dn_d, "Dyn Delta", "DELT", 0, 127, 64);
DK_PAGE(dyn_pluck, "DYN", "Dyn Synth", DK_KNOB(0), DK_KNOB(1), DK_KNOB(2), DK_KNOB(3));
