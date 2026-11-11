/* The effect page of test/dyn-fx-hang: one effect `hang` with one knob (unused by the routine). Starts as a send. patchkit pack reads these records; the device never sees them. */
#include <dk/fx/fx.h>

DK_FX(hang, .process = "_hf_process", .desc = "_hf_desc", .mix = 127, .route = DK_ROUTE_SEND);
DK_FX_PARAM(hang, 0, "Gain", "GAIN", DK_PERCENT, 0, 1.27, 0.5);
DK_PAGE(hang, "HNG", "Test FX Hang", DK_KNOB_AT(0, 0));
