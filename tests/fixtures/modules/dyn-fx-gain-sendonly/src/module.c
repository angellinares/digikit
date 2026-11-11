/* The effect page of test/dyn-fx-gain-sendonly: one effect `gain` with one knob. patchkit pack reads these records; the device never sees them.
   Tab GSO. GAIN is p0, 0 to 1.27: the shown value 0 .. 127 is the gain in percent, so the default 50 is 0.5.
   The MIX knob starts at 127 (wet). The effect starts as a send and can only be a send. */
#include <dk/fx/fx.h>

DK_FX(gain, .process = "_gso_process", .desc = "_gso_desc", .mix = 127, .route = DK_ROUTE_SEND,
      .routes = DK_ROUTE_SEND);
DK_FX_PARAM(gain, 0, "Gain", "GAIN", DK_PERCENT, 0, 1.27, 0.5);
DK_PAGE(gain, "GSO", "Test Gain Send Only", DK_KNOB_AT(0, 0));
