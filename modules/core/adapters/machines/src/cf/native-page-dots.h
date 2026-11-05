/* Native Bitmap coordinates start at the bottom. Page zero is the visually
 * upper dot; page one is the lower dot. Keep this separate from canvas-local
 * drawing, whose top-origin coordinates are transformed by canvas_fill. */
#ifndef NATIVE_PAGE_DOTS_H
#define NATIVE_PAGE_DOTS_H
#include <stdint.h>
static inline void native_page_dots(uint8_t *canvas, uint32_t page,
    void (*rect)(uint8_t *, int32_t, int32_t, int32_t, int32_t, int32_t))
{
    if (page > 1) return;
    if (page == 0) {
        rect(canvas, 124, 31, 127, 34, -1);
        rect(canvas, 125, 24, 126, 25, -1);
    } else {
        rect(canvas, 125, 32, 126, 33, -1);
        rect(canvas, 124, 23, 127, 26, -1);
    }
}
#endif
