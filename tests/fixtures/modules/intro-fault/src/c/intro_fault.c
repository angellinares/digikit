/* test/intro-fault: takes the boot animation and faults in the first frame callback (on the intro task). */

#include <dk/platform/intro.h>

static uint8_t blank[DK_INTRO_BYTES] __attribute__((aligned(4)));

static const uint8_t *frame(uint32_t tick, void *ctx)
{
    (void)tick;
    (void)ctx;
    __asm__ volatile(".short 0x4afc"); /* illegal instruction */
    return blank;
}

int32_t dkm_init(const dk_api *api)
{
    int32_t rc = dk_intro_take(api, frame, 0, 0, 30);
    api->log((const uint8_t *)"intro-fault: take", (uint32_t)rc);
    return 0;
}
