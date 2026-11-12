/* Module-owned mono float sample banks. API contract 1.3. */
#ifndef DK_SAMPLE_BANK_H
#define DK_SAMPLE_BANK_H
#include <dk/core/status.h>
#include <dk/page/page.h>

#define DK_SAMPLE_UNUSED 0
#define DK_SAMPLE_QUEUED 1
#define DK_SAMPLE_LOADING 2
#define DK_SAMPLE_READY 3
/* The sample errors are the runtime status codes of <dk/core/status.h>. */
#define DK_SAMPLE_E_NOT_FOUND DK_E_NOT_FOUND
#define DK_SAMPLE_E_READ DK_E_READ
#define DK_SAMPLE_E_SHA DK_E_SHA
#define DK_SAMPLE_E_FORMAT DK_E_FORMAT
#define DK_SAMPLE_E_NOMEM DK_E_NOMEM
#define DK_SAMPLE_E_LOADED DK_E_LOADED
#define DK_SAMPLE_E_BUSY DK_E_BUSY
#define DK_SAMPLE_E_TOO_BIG DK_E_TOO_BIG
#define DK_SAMPLE_E_NO_DSP DK_E_NO_DSP
#define DK_SAMPLE_E_DSP_XFER DK_E_DSP_XFER
#define DK_SAMPLE_E_SEL_PATH DK_E_SEL_PATH
#define DK_SAMPLE_E_CONTRACT DK_E_CONTRACT

/* The DSP descriptor is four initially zero 32-bit words: READY (0/1),
   normal-word source pointer, frame count, sample rate. Runtime publishes
   READY last. Reserve CAPACITY mono float frames in the module's own BSS.
   Exactly one bank is supported. Resolve its handle in init; queue its single
   load from init or a service callback (the runtime copies the path);
   the destination remains owned until restart once delivery begins. */
#define DK_SAMPLE_BANK(LOCAL_ID, BANK, DESCRIPTOR, CAPACITY)                                       \
    static const dk_desc_sample_bank DK_CAT(dk_sample_bank_, __LINE__)                             \
        DK_DESC = {.kind = DK_DESC_SAMPLE_BANK,                                                    \
                   .size = sizeof(dk_desc_sample_bank),                                            \
                   .local_id = (LOCAL_ID),                                                         \
                   .bank = #BANK,                                                                  \
                   .descriptor = #DESCRIPTOR,                                                      \
                   .capacity_frames = (CAPACITY)}
#endif
