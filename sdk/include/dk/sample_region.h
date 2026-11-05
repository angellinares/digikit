/* Reusable sample-region drawing for native pages. Inputs are immutable overview
 * peaks or windowed columns, never PCM. Q8 zoom 256 is 1x; the viewport keeps at
 * least one frame per column. dk_sample_region_rect_q8 follows START (0..127). Null peaks/zero bins still draw the center
 * line and trim handles. Coordinates and labels remain the caller's policy. */
#ifndef DK_SAMPLE_REGION_H
#define DK_SAMPLE_REGION_H
#include <dk/page.h>
#include <dk/sample_track.h>
static inline uint32_t dk_region_fraction(uint32_t value, uint32_t n, uint32_t d)
{
    while (n && (value % d) > UINT32_MAX / n && d > 1) {
        value >>= 1; d >>= 1;
    }
    return (value / d) * n + (value % d) * n / d;
}
#define DK_SAMPLE_ZOOM_ONE 256u /* Q8 zoom: 256 = 1x, no upper limit */
/* Inclusive plot rectangle. All waveform and marker writes clip to it. */
typedef struct dk_sample_region_rect { int32_t left, top, right, bottom; } dk_sample_region_rect;
static inline void dk_sample_region_fill(const dk_page_canvas_v1 *c, dk_sample_region_rect r,
    int32_t x0, int32_t y0, int32_t x1, int32_t y1, int32_t color)
{
    if (x0 < r.left) x0 = r.left;
    if (x1 > r.right) x1 = r.right;
    if (y0 < r.top) y0 = r.top;
    if (y1 > r.bottom) y1 = r.bottom;
    if (x0 <= x1 && y0 <= y1) c->fill_rect(x0, y0, x1, y1, color);
}
/* Clips the plot to the canvas; returns its width in pixels, 0 when too small. */
static inline uint32_t dk_sample_region_clip(const dk_page_canvas_v1 *c, dk_sample_region_rect *r)
{
    if (r->left < 0) r->left = 0;
    if (r->top < 0) r->top = 0;
    if (r->right >= (int32_t)c->width) r->right = (int32_t)c->width - 1;
    if (r->bottom >= (int32_t)c->height) r->bottom = (int32_t)c->height - 1;
    if (r->right <= r->left || r->bottom - r->top < 4) return 0;
    return (uint32_t)(r->right - r->left + 1);
}
/* Viewport of a zoomed sample: span = frames * 256 / zoom_q8, but at least
   min(width, frames) (one frame per column); first is centred on focus and
   clamped so [first, first + span) lies inside [0, frames). */
static inline void dk_sample_viewport(uint32_t frames, uint32_t focus, uint32_t zoom_q8,
    uint32_t width, uint32_t *first, uint32_t *span)
{
    if (!frames) { *first = 0; *span = 0; return; }
    if (zoom_q8 < DK_SAMPLE_ZOOM_ONE) zoom_q8 = DK_SAMPLE_ZOOM_ONE;
    uint32_t least = width < frames ? width : frames;
    uint32_t s = dk_region_fraction(frames, DK_SAMPLE_ZOOM_ONE, zoom_q8);
    if (s < least) s = least;
    if (!s) s = 1;
    if (s > frames) s = frames;
    uint32_t f = focus > s / 2 ? focus - s / 2 : 0;
    if (f > frames - s) f = frames - s;
    *first = f; *span = s;
}
/* Draws the waveform of the viewport [first, first + span) of a sample of
   `frames` frames. With columns != 0 and ncols == plot width, pixel x is
   columns[x]; otherwise each pixel is derived from the overview bins. Then the
   centre line and the START/END markers (frame positions; outside the
   viewport they sit at its edge). */
static inline void dk_sample_region_draw(const dk_page_canvas_v1 *c, dk_sample_region_rect r,
    uint32_t frames, uint32_t first, uint32_t span, const dk_sample_peak *overview, unsigned bins,
    const dk_sample_peak *columns, unsigned ncols, uint32_t start_frame, uint32_t end_frame)
{
    if (!c) return;
    uint32_t width = dk_sample_region_clip(c, &r);
    if (!width) return;
    int32_t top = r.top, bottom = r.bottom;
    /* Empty/unavailable data still has a normalized region and visible zero. */
    if (!frames) frames = 127;
    if (!span) span = 1;
    uint32_t a = start_frame, b = end_frame, left = first;
    uint32_t bin_width = (frames + DK_SAMPLE_PEAK_BINS - 1) / DK_SAMPLE_PEAK_BINS;
    int32_t mid = (top + bottom) / 2, radius = (bottom - top) / 2 - 2;
    dk_sample_region_fill(c, r, r.left, mid, r.right, mid, 1);
    int use_columns = columns && ncols == width;
    for (uint32_t x = 0; (use_columns || (overview && bins)) && x < width; x++) {
        int32_t min = 0, max = 0;
        if (use_columns) {
            min = columns[x].min; max = columns[x].max;
        } else {
            uint32_t f0 = left + dk_region_fraction(span, x, width);
            uint32_t f1 = left + dk_region_fraction(span, x + 1, width);
            /* Short samples/zoom spans repeat a source bin across pixels rather
               than leaving holes when the pixel's integer frame interval is empty. */
            if (f1 <= f0) f1 = f0 + 1;
            uint32_t lo = f0 / bin_width, hi = (f1 - 1) / bin_width;
            if (hi >= bins) hi = bins - 1;
            if (lo >= bins) lo = bins - 1;
            for (uint32_t i = lo; i <= hi; i++) {
                if (overview[i].min < min) min = overview[i].min;
                if (overview[i].max > max) max = overview[i].max;
            }
        }
        dk_sample_region_fill(c, r, r.left + (int32_t)x, mid - max * radius / 32768,
                     r.left + (int32_t)x, mid - min * radius / 32768, 1);
    }
    /* Selection rail and handles stay distinct from the waveform. Markers beyond
       a zoomed viewport sit at its edge. */
    int32_t ax = a <= left ? 0 : a >= left + span ? (int32_t)width - 1 :
        (int32_t)dk_region_fraction(a - left, width - 1, span);
    int32_t bx = b <= left ? 0 : b >= left + span ? (int32_t)width - 1 :
        (int32_t)dk_region_fraction(b - left, width - 1, span);
    ax += r.left; bx += r.left;
    if (b >= a) dk_sample_region_fill(c, r, ax, bottom, bx, bottom, 1);
    dk_sample_region_fill(c, r, ax - 1, top, ax + 1, bottom, 0);
    dk_sample_region_fill(c, r, bx - 1, top, bx + 1, bottom, 0);
    dk_sample_region_fill(c, r, ax, top, ax, bottom, 1);
    dk_sample_region_fill(c, r, bx, top, bx, bottom, 1);
    for (int i = 0; i < 5; i++) {
        dk_sample_region_fill(c, r, ax, top + i, ax + 4 - i, top + i, 1);
        dk_sample_region_fill(c, r, bx - 4 + i, bottom - i, bx, bottom - i, 1);
    }
}
/* Viewport follows START (0..127 units). Zoom is Q8 with no fixed upper limit;
   the viewport keeps at least one frame per column. */
static inline void dk_sample_region_rect_q8(const dk_page_canvas_v1 *c,
    const dk_sample_peak *peaks, uint32_t bins, uint32_t frames,
    uint32_t start, uint32_t end, uint32_t zoom_q8, dk_sample_region_rect r)
{
    if (!c) return;
    uint32_t width = dk_sample_region_clip(c, &r);
    if (!width) return;
    if (!frames) frames = 127;
    uint32_t a = dk_region_fraction(frames, start, 127);
    uint32_t b = dk_region_fraction(frames, end, 127);
    uint32_t first, span;
    dk_sample_viewport(frames, a, zoom_q8, width, &first, &span);
    dk_sample_region_draw(c, r, frames, first, span, peaks, bins, 0, 0, a, b);
}
/* Full-width compatibility viewport. */
static inline void dk_sample_region_q8(const dk_page_canvas_v1 *c,
    const dk_sample_peak *peaks, uint32_t bins, uint32_t frames,
    uint32_t start, uint32_t end, uint32_t zoom_q8, int32_t top, int32_t bottom)
{
    if (!c) return;
    dk_sample_region_rect r = {0, top, (int32_t)c->width - 1, bottom};
    dk_sample_region_rect_q8(c, peaks, bins, frames, start, end, zoom_q8, r);
}
/* Compatibility helper for callers using the original three zoom steps. */
static inline void dk_sample_region(const dk_page_canvas_v1 *c,
    const dk_sample_peak *peaks, uint32_t bins, uint32_t frames,
    uint32_t start, uint32_t end, uint32_t zoom, int32_t top, int32_t bottom)
{
    if (zoom > 2) zoom = 2;
    dk_sample_region_q8(c, peaks, bins, frames, start, end, DK_SAMPLE_ZOOM_ONE << zoom, top, bottom);
}

/* Our portable direction/loop mark. Uses only canvas primitives, no stock assets.
   Modes share the sample PLAY convention: REV, REV.L, FWD.L, FWD. */
static inline void dk_sample_play_glyph(const dk_page_canvas_v1 *c, int x, int y, unsigned mode)
{
    int forward = mode >= 2;
    c->fill_rect(x, y, x + 12, y, 1);
    for (int i = 0; i < 4; i++) {
        int at = forward ? x + 12 - i : x + i;
        c->fill_rect(at, y - i, at, y + i, 1);
    }
    if (mode == 1 || mode == 2) {
        c->fill_rect(x, y + 6, x + 12, y + 6, 1);
        c->fill_rect(x, y + 6, x, y + 10, 1);
        c->fill_rect(x + 12, y + 6, x + 12, y + 10, 1);
        c->fill_rect(x + 4, y + 10, x + 12, y + 10, 1);
        c->fill_rect(x + 2, y + 9, x + 4, y + 11, 1);
    }
}
#endif
