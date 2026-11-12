/* The effect page of freqshift: one effect `fshift` with one knob. patchkit pack reads this record; the device never sees it.
   Saved kits keep the effect by module id + effect id: never rename either.
   The page comes after CHO in the FX view, with the framework's MIX knob. The effect starts as a send effect:
   a send has no dry path of its own, so MIX is the return level and starts at 127 (0 would add nothing).
   The user may move it to the master; there MIX 127 is the fully shifted sound. MIX is one value for both routes.
   SHIFT is bipolar: the shown value -64 .. +63 is the float the routine reads. 0 is no shift; |v| = 1 is about 0.1 Hz,
   the ends are about +-2 kHz, exponential in between (the table is in docs/findings/54-module-cookbook.md). */
#include <dk/fx/fx.h>
#include "freqshift.h"

DK_FX(fshift, .process = "_fs_process", .desc = "_fs_desc", .mix = 127, .route = DK_ROUTE_SEND,
      .routes = DK_ROUTE_SEND | DK_ROUTE_MASTER);
DK_FX_PARAM(fshift, FSHIFT_SHIFT_WORD, "Freq Shift", "SHIFT", DK_BIPOLAR, -64, 63, 0);
DK_PAGE(fshift, "FSH", "Freq Shifter", DK_KNOB_AT(FSHIFT_SHIFT_WORD, 0));
