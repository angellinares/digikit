/* test/intro-ticks: takes the boot animation for TICKS ticks. The frame callback (intro task, no dk_api call) folds each tick into
 * h = h * 31 + tick + 1 and counts the calls. `done` (UI task, first pass) logs the count and the hash, so a test can prove that every tick
 * ran once and in order: a skipped, repeated or swapped tick changes the hash. */

#include <dk/platform/intro.h>

#define TICKS 40u

static uint8_t blank[DK_INTRO_BYTES] __attribute__((aligned(4)));
static volatile uint32_t hash, calls;
static const dk_api *dkapi;

static const uint8_t *frame(uint32_t tick, void *ctx)
{
    (void)ctx;
    hash = hash * 31u + tick + 1u;
    calls = calls + 1u;
    return blank;
}

static void done(void *ctx)
{
    (void)ctx;
    dkapi->log((const uint8_t *)"ticks", calls);
    dkapi->log((const uint8_t *)"hash", hash);
}

int32_t dkm_init(const dk_api *api)
{
    int32_t rc;
    dkapi = api;
    rc = dk_intro_take(api, frame, done, 0, TICKS);
    api->log((const uint8_t *)"intro-ticks: take", (uint32_t)rc);
    return 0;
}
