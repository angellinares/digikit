/* test/dyn-fault-late: a module that fails some time into a session, not in `init`. The runtime's boot counter must still count the
 * boot as bad when this happens before its stable period is over, and must not when it happens after (docs/findings/37-boot-load.md,
 * "Safe mode after the first UI pass").
 *
 * The delay and the kind of failure come from the store file `cfg/fault-after.bin` (5 bytes: u32 big endian blocks, then 0 = reset the
 * machine, as the firmware's exception path ends, 1 = hang in the UI task with the other tasks running); no file: never fails. */

#include <dk/core/api.h>

typedef uint32_t u32;

static u32 after = 0xffffffffu;
static u32 mode;
static u32 start;
static u32 started;

int32_t dkm_init(const dk_api *api)
{
    unsigned char cfg[16];
    int n = api->store_read((const uint8_t *)"cfg/fault-after.bin", cfg, sizeof cfg);
    if (n >= 5) {
        after = ((u32)cfg[0] << 24) | ((u32)cfg[1] << 16) | ((u32)cfg[2] << 8) | cfg[3];
        mode = cfg[4];
    }
    api->log((const uint8_t *)"fault-late after", after);
    return 0;
}

void dkm_slot_0(const dk_api *api)
{
    u32 now = dkrt_blocks();
    (void)api;
    if (!started) {
        started = 1;
        start = now;
        return;
    }
    if (now - start < after) {
        return;
    }
    if (mode == 0) {
        *(volatile unsigned char *)0xEC090000 = 0x80;
    }
    for (;;) {
    }
}
