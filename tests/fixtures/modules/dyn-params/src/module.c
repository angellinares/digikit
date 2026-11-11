/* The machine declaration of test/dyn-params: one machine `dynp` with four parameters (words 0 .. 3), to check the parameter rows
   the registry writes. The ranges differ per word. The DSP is test/sine's. patchkit reads these records; the device never sees them. */
#include <dk/machine/machine.h>

DK_MACHINE(dynp, .name = "DynP", .short_name = "DYP", .render = "_sn_render",
           .note_on = "_sn_note_on", .desc = "_sn_desc");
DK_PARAM(dynp, 0, p0, "Param 0", "P0", 0, 127, 16);
DK_PARAM(dynp, 1, p1, "Param 1", "P1", (1 / 256.0), 126, (4369 / 256.0));
DK_PARAM(dynp, 2, p2, "Param 2", "P2", (2 / 256.0), 125, (4642 / 256.0));
DK_PARAM(dynp, 3, p4, "Param 4", "P4", (4 / 256.0), 123, (5188 / 256.0));
