/* <dk/sample_picker.h>: the on-device sample picker of a sampler machine that owns a 16-entry sample pool (<dk/sample_track.h>).
   Shared source, not a runtime call: the module lists sdk/src/sample_picker.c and sdk/src/picker_logic.c in its [[object]] sources and
   needs no new manifest field (design: docs/design/sample-picker.md).

   The module calls three functions:

     dkm_init:   dk_picker_init(api, handle);   handle is the value of api->sample_tracks(local_id)
     click:      dk_picker_open_pool(cur, pick, ctx);   the SMP knob was clicked (released without a turn), see below
     dkm_fini:   dk_picker_fini();

   dk_picker_open_pool opens the pool list over the machine page: row 0 is "+ ADD", then the 16 entries, numbered 0 to 15 like the SMP
   knob, with name, mono/stereo, length or state; an empty entry shows "-". The entry the SMP value points at is marked with '>'.
     YES on + ADD     opens the folder browser of the samples source (<dk/files.h>); YES on a file adds it to the pool
                      (sample_pool_add) and calls pick(ctx, index) with its pool index, then shows the list again
     YES on an entry  calls pick(ctx, index) and closes the list (an empty entry is not chosen)
     RIGHT on entry   CLEAR (sample_pool_clear) and INFO; RIGHT on a file in the browser gives SELECT and INFO
     NO               up one folder in the browser, out of the browser at the root; in the list back to the machine page
   An add that the runtime refuses (pool full, bad path, no pool on this device) shows on the + ADD row; pick is not called.
   pick is how the machine sets its SMP value: the picker has no way to write a parameter itself. It runs in the UI task. The picker does
   nothing when the device has no core/ui lists, when it could not start, or when init was not called. */
#ifndef DK_SAMPLE_PICKER_H
#define DK_SAMPLE_PICKER_H
#include <dk/api.h>
#include <dk/ui.h>

/* Called with the pool index the machine should select: the entry just added, or the entry chosen with YES. */
typedef void dk_picker_pick_fn(void *ctx, int index);

/* Acquires the listing client; call in dkm_init. The picker keeps api; it must stay valid. 0 when the picker can run, -1 when not (the
   click then does nothing). */
int dk_picker_init(const dk_api *api, uint32_t handle);
/* Opens the pool list. current is the SMP value now (0 to 15, or -1) and is marked (-1: no row is marked); pick may be 0: YES on an entry then only closes the list and adding a file only adds it. Call from the UI task. */
void dk_picker_open_pool(int current, dk_picker_pick_fn *pick, void *ctx);
/* Stops the picker's timer and listing; call in dkm_fini. */
void dk_picker_fini(void);
#endif
