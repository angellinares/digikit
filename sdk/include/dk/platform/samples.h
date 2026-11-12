/* <dk/platform/samples.h>: the module's sample pool (dk_sample_pool_open and the dk_sample_pool_* calls). Hand-written. Include <dk/core/module.h> first.

   The pool is the 16-entry stereo sample store a module declares with DK_SAMPLE_TRACKS (<dk/sample/pool.h> has the entry layout and the rules of the
   states, paths and peaks). `dk_sample_pool_open(ctx, &pool)` in `init` gives the handle; each call takes it where the Api table took a number.
   The drawing helpers and the picker stay in <dk/sample/view.h> and <dk/sample/picker.h>. Today the service is in the runtime image; it
   moves to a `samples` service module without a change to the module (docs/plans/runtime-and-services.md). */
#ifndef DK_PLATFORM_SAMPLES_H
#define DK_PLATFORM_SAMPLES_H
#include <dk/core/module.h>
#include <dk/sample/pool.h>
#endif
