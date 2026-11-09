/* A bounded stdin/stdout adapter for the module's own decoder (src/dkbs.c), not a second codec.
 * Output on success (exit 0): u32 records, u32 ticks, u32 decoded records, then per tick u32 decoded records so far and 1024 bytes.
 * Exit 1: the file was refused (and `dkbs_next` then returns 0). Exit 2: a decoder rule broke (a guard, the pointer, the tick count). */
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include "dkbs.h"

static struct {
    uint8_t before[16];
    uint8_t work[DKBS_FRAME];
    Dkbs state;
    uint8_t after[16];
} guarded;
static uint8_t input[DKBS_FILE_MAX + 1], output[12 + DKBS_TICKS_MAX * (4 + DKBS_FRAME)];

static int guards(void)
{
    for (unsigned i = 0; i < 16; i++)
        if (guarded.before[i] != 0xa5 || guarded.after[i] != 0x5a)
            return 0;
    return 1;
}

static void be32(uint8_t *p, uint32_t value)
{
    for (unsigned i = 0; i < 4; i++)
        p[i] = (uint8_t)(value >> (24 - 8 * i));
}

int main(void)
{
    memset(guarded.before, 0xa5, 16);
    memset(guarded.after, 0x5a, 16);
    uint32_t size = (uint32_t)fread(input, 1, sizeof(input), stdin);
    if (!dkbs_open(&guarded.state, input, size, guarded.work))
        return guards() && !dkbs_next(&guarded.state) ? 1 : 2;
    if (!guards() || guarded.state.tick || guarded.state.decoded_records)
        return 2;
    uint32_t ticks = 0, at = 12;
    const uint8_t *frame;
    while ((frame = dkbs_next(&guarded.state)) != 0) {
        if (!guards() || ticks >= DKBS_TICKS_MAX || frame != guarded.work)
            return 2;
        be32(output + at, guarded.state.decoded_records);
        memcpy(output + at + 4, frame, DKBS_FRAME);
        at += 4 + DKBS_FRAME;
        ticks++;
    }
    if (!guards() || !guarded.state.valid || ticks != guarded.state.total_ticks || dkbs_next(&guarded.state))
        return 2;
    be32(output, guarded.state.records);
    be32(output + 4, ticks);
    be32(output + 8, guarded.state.decoded_records);
    return fwrite(output, 1, at, stdout) == at ? 0 : 2;
}
