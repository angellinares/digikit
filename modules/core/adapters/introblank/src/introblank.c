/* core/introblank: a black intro (see README.md). ColdFire, one source for the Digitakt II and the Digitone II.
 *
 * The stock intro calls `callback(index, count, bitmap)` once per tick. The bitmap persists between frames and the tick does not clear it, so the
 * callback clears it with the stock clear function (value 0, flag 1). It takes no input, reads no storage and allocates nothing. */
#include <stdint.h>
#include "dk_fw_draw.h"

typedef struct IntroScene {
    uint32_t callback;
    int32_t count;
} IntroScene;

/* One scene and the terminator: 16 bytes, one cache line of their own (a data line that shares a line with code is a flush hazard). The hook in
 * hook.s fills it at the start of the intro; the runtime writes count 0 into `introblank_table[0]` to end the intro early. */
IntroScene introblank_table[2] __attribute__((aligned(16)));

void introblank_black(uint32_t index, uint32_t count, uint8_t *bitmap)
{
    (void)index;
    (void)count;
    dk_fw_draw_bitmap_clear(bitmap, 0, 1);
    /* Not a tail call: a jump would reuse the three argument slots on the stack for the clear call, and the caller (and the emulator's intro
     * frame capture, which reads index, count and bitmap there when the callback returns) expects them unchanged. */
    __asm__ volatile("" ::: "memory");
}
