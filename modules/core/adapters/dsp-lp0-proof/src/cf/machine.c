/* Explicit opt-in, private two-track diagnostic. */
#include <dk/machine.h>
DK_MACHINE(lp_sampler, .name = "LP0 sampler", .short_name = "LP0",
           .render = "_lp_play_render", .note_on = "_lp_play_note_on",
           .desc = "_lp_play_desc");
DK_PARAM(lp_sampler, 0, lp_reverse, "Reverse", "REV", 0, 1, 0);
DK_PAGE(lp_sampler, "LP0PROOF", "LP0 sampler", DK_KNOB(0));
