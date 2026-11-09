/* Module-owned mono float sample banks. API contract 1.3. */
#ifndef DK_SAMPLE_H
#define DK_SAMPLE_H
#include <dk/page.h>

#define DK_SAMPLE_UNUSED 0
#define DK_SAMPLE_QUEUED 1
#define DK_SAMPLE_LOADING 2
#define DK_SAMPLE_READY 3
/* Existing runtime errors (sdk/sdk.toml [load.status]). */
#define DK_SAMPLE_E_NOT_FOUND (-1)
#define DK_SAMPLE_E_READ (-3)
#define DK_SAMPLE_E_SHA (-4)
#define DK_SAMPLE_E_FORMAT (-5)
#define DK_SAMPLE_E_NOMEM (-8)
#define DK_SAMPLE_E_LOADED (-10)
#define DK_SAMPLE_E_BUSY (-12)
#define DK_SAMPLE_E_TOO_BIG (-13)
#define DK_SAMPLE_E_NO_DSP (-30)
#define DK_SAMPLE_E_DSP_XFER (-35)
#define DK_SAMPLE_E_SEL_PATH (-40)
#define DK_SAMPLE_E_CONTRACT (-48)

/* The DSP descriptor is four initially zero 32-bit words: READY (0/1),
   normal-word source pointer, frame count, sample rate. Runtime publishes
   READY last. Reserve CAPACITY mono float frames in the module's own BSS.
   Exactly one bank is supported. Resolve its handle in init; queue its single
   load from init or a service callback (the runtime copies the path);
   the destination remains owned until restart once delivery begins. */
#define DK_SAMPLE_BANK(LOCAL_ID, BANK, DESCRIPTOR, CAPACITY)                      \
    static const dk_desc_sample_bank DK_CAT(dk_sample_bank_, __LINE__) DK_DESC = { \
        .kind = DK_DESC_SAMPLE_BANK, .size = sizeof(dk_desc_sample_bank),         \
        .local_id = (LOCAL_ID), .bank = #BANK, .descriptor = #DESCRIPTOR,          \
        .capacity_frames = (CAPACITY)}
#endif
