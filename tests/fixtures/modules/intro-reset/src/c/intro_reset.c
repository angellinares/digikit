/* test/intro-reset: takes the boot animation and resets the machine in the first frame callback (on the intro task): it writes SOFTRST (0x80)
 * into RCM.RCR (0xEC090000), as test/dyn-fault does. A frame callback makes no dk_api call. */

#include <dk/platform/intro.h>

static uint8_t blank[DK_INTRO_BYTES] __attribute__((aligned(4)));

static const uint8_t *frame(uint32_t tick, void *ctx)
{
    (void)tick;
    (void)ctx;
    *(volatile unsigned char *)0xEC090000 = 0x80;
    for (;;) {
    }
    return blank;
}

int32_t dkm_init(const dk_api *api)
{
    int32_t rc = dk_intro_take(api, frame, 0, 0, 30);
    api->log((const uint8_t *)"intro-reset: take", (uint32_t)rc);
    return 0;
}
