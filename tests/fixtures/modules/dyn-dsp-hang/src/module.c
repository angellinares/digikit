/* The machine declaration of test/dyn-dsp-hang: one machine `hang` without parameters. patchkit pack reads this record;
   the device never sees it. */
#include <dk/machine/machine.h>

DK_MACHINE(hang, .name = "Hang", .short_name = "HNG", .render = "_hg_render",
           .note_on = "_hg_note_on", .desc = "_hg_desc");
