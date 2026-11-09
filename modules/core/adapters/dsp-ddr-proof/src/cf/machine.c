/* Patched-in diagnostic only: fixed immutable residency, no public sample API. */
#include <dk/machine.h>

DK_MACHINE(ddr_proof, .name = "DDR proof", .short_name = "DDR",
           .render = "_dd_play_render", .note_on = "_dd_play_note_on",
           .desc = "_dd_play_desc");
DK_PARAM(ddr_proof, 0, dd_reverse, "Reverse", "REV", 0, 1, 0);
DK_PAGE(ddr_proof, "DDRPROOF", "DDR proof", DK_KNOB(0));
