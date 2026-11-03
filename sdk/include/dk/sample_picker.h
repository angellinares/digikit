/* <dk/sample_picker.h>: the on-device sample picker of a sampler machine that owns a 16-entry sample pool (<dk/sample_track.h>): the
   chooser (<dk/chooser.h>) filled with the pool. Shared source, not a runtime call: the module lists sdk/src/sample_picker.c,
   sdk/src/chooser.c and sdk/src/picker_logic.c in its [[object]] sources (design: docs/design/sample-picker.md, docs/design/ui-rules.md).

   The module calls three functions:

     dkm_init:   dk_picker_init(api, handle);   handle is the value of api->sample_tracks(local_id)
     click:      dk_picker_open_pool(cur, pick, ctx);   the SMP knob was clicked (released without a turn), see below
     dkm_fini:   dk_picker_fini();

   dk_picker_open_pool opens the slot list (the generic slot list of <dk/chooser.h>) over the machine page: the 16 slots, numbered 0 to
   15 like the SMP knob, with name, mono/stereo, length or state; an empty slot shows "-". The slot the SMP value points at is marked with
   '>' and the cursor starts on it.
     YES on a slot    opens the folder browser of the samples source (<dk/files.h>) for that slot (empty or filled)
     YES on a file    loads it into that slot (sample_pool_set; a filled slot is replaced), calls pick(ctx, slot) and closes the whole
                      flow: one press, back on the page
     RIGHT on a slot  CLEAR (sample_pool_clear) and INFO; RIGHT on a file in the browser gives SELECT and INFO
     NO               up one folder in the browser, out of the browser at the root back to the slot list; in the list back to the page
   A load that the runtime refuses (bad path, no pool on this device) closes the flow and shows the error text as a toast on the
   page; pick is not called. pick is how the machine sets its SMP value: the picker has no way to write a parameter itself. It runs in the
   UI task, after the lists have closed. The picker does nothing when the device has no core/ui lists, when it could not start, or when
   init was not called. */
#ifndef DK_SAMPLE_PICKER_H
#define DK_SAMPLE_PICKER_H
#include <dk/api.h>
#include <dk/ui.h>

/* Called with the slot the machine should select: the slot a file was just loaded into. ctx stays in use after the
   click callback returns: static or module memory. */
typedef void dk_picker_pick_fn(void *ctx, int index);

/* Acquires the listing client; call in dkm_init. The picker keeps api; it must stay valid. 0 when the picker can run, -1 when not (the
   click then does nothing). */
int dk_picker_init(const dk_api *api, uint32_t handle);
/* Opens the slot list. current is the SMP value now (0 to 15, or -1) and is marked (-1: no row is marked); pick may be 0: loading a file then only loads it. Call from the UI task. */
void dk_picker_open_pool(int current, dk_picker_pick_fn *pick, void *ctx);
/* Stops the picker's timer and listing; call in dkm_fini. */
void dk_picker_fini(void);
#endif
