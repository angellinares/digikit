/* The effect page of test/dyn-fx-nan: one effect `nan` with one knob (unused by the routine). Starts as a send. patchkit pack reads these records; the device never sees them. */
#include <dk/fx/fx.h>

DK_FX(nan, .process = "_fnn_process", .desc = "_fnn_desc", .mix = 127, .route = DK_ROUTE_SEND);
DK_FX_PARAM(nan, 0, "Gain", "GAIN", DK_PERCENT, 0, 1.27, 0.5);
DK_PAGE(nan, "NAN", "Test FX NaN", DK_KNOB_AT(0, 0));
