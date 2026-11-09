/* <dk/intro.h>: a module's own boot animation (API 1.9: api->intro_take). The firmware's intro runs at 30 ticks a second; the module gives one
   128 x 64 native bitmap per tick. It runs after the boot checks (REC, safe mode, boot counter) and uses no storage: read the frames in dkm_init.

     frame:   `const uint8_t *frame(uint32_t tick, void *ctx)` returns the 1024-byte native bitmap of this tick (0 .. ticks - 1), or NULL to end the
              animation (the last frame stays on the screen). It runs on the intro task, not on the UI task: it must not block and must not call
              any dk_api function. The bitmap must stay valid until the next call of frame.
     done:    `void done(void *ctx)`, may be NULL. Called once on the UI task at the first UI pass, after the intro has ended. Free the
              buffers here: never allocate or free inside frame.
     ticks:   1 to 300.

   Only inside dkm_init; the last call of dkm_init is the best place, so that a later failure does not leave the intro with a module that
   was rolled back. Results: 0, E_NOT_FOUND (outside dkm_init), E_CONTRACT (no frame, or ticks out of range), E_BUSY (another module took the
   intro) or E_BOOT_ONLY (the intro is over already). A refusal changes nothing: the stock intro runs.

   A fault inside frame reaches the firmware's own exception handler. The boot counter stays raised until the session is stable, so repeated
   failed boots end in safe mode, where no module loads.

   A module that also runs on a runtime older than API 1.9 checks `api->size >= sizeof(dk_api)` before it calls this. */
#ifndef DK_INTRO_H
#define DK_INTRO_H
#include <dk/api.h>

#define DK_INTRO_TICKS_MAX 300u
#define DK_INTRO_BYTES 1024u /* the native bitmap of one tick */

/* The runtime's results (sdk/sdk.toml [load.status]) named above. */
#define DK_INTRO_E_NOT_FOUND (-1)  /* E_NOT_FOUND: outside dkm_init */
#define DK_INTRO_E_BUSY      (-12) /* E_BUSY: taken already */
#define DK_INTRO_E_BOOT_ONLY (-45) /* E_BOOT_ONLY: the intro is over */
#define DK_INTRO_E_CONTRACT  (-48) /* E_CONTRACT: no frame, or ticks out of range */

typedef dk_intro_frame *dk_intro_frame_fn;
typedef dk_intro_done *dk_intro_done_fn;

/* Takes over the boot animation; call in dkm_init. 0 or a negative error. */
static inline int32_t dk_intro_take(const dk_api *api, dk_intro_frame_fn frame, dk_intro_done_fn done, void *ctx, uint32_t ticks) {
    return api->intro_take(frame, done, ctx, ticks);
}
#endif
