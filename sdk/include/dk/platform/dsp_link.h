/* <dk/platform/dsp_link.h>: the per-block callback of the DSP link (needs core/dsp-link). Hand-written. Include <dk/core/module.h> first.

   `dk_dsp_link_open(ctx, &dl)` in `init`; without core/dsp-link it returns DK_E_MISSING. `dk_dsp_link_set_tick(dl, cb)` sets the one callback
   `void cb(const volatile u16 *rec)` (0: off) that runs every block in the block interrupt with the copy of the previous DSP reply record. It must
   be a short C function with no allocation and no logging. Clear it in `fini`: the runtime drops a callback that still points into the module after
   `fini`, but a module should not rely on that. */
#ifndef DK_DSP_LINK_H
#define DK_DSP_LINK_H
#include <dk/core/module.h>
#endif
