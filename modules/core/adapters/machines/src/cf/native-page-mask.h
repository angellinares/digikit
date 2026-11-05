/* Logical Bitmap mask: x-major bands, MSB is the bottom-origin band's row0.
 * Only explicit SDK fill rectangles become opaque in an overlay. */
#ifndef NATIVE_PAGE_MASK_H
#define NATIVE_PAGE_MASK_H
#include <stdint.h>
static void native_page_mask_rect(uint32_t *mask, int width, int height,
    int x0, int y0, int x1, int y1)
{
    if (x0 < 0) x0 = 0;
    if (y0 < 0) y0 = 0;
    if (x1 >= width) x1 = width - 1;
    if (y1 >= height) y1 = height - 1;
    int bands = (height + 31) / 32;
    for (int x = x0; x <= x1; x++)
        for (int y = y0; y <= y1; y++) {
            int row = height - 1 - y;
            mask[x * bands + row / 32] |= UINT32_C(1) << (31 - row % 32);
        }
}
#endif
