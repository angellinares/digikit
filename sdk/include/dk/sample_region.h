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
/* New first frame of a viewport after its span changes from span_old to span_new
   (both at most frames). The anchor frame keeps its screen fraction
   p = (anchor - first_old) / span_old; an anchor outside the old viewport uses
   p = 1/2. The result is clamped to [0, frames - span_new]. At span_new >= frames
   the view is the whole sample (0). Uses a 64-bit product, no 64-bit division. */
static inline uint32_t dk_sample_view_zoom(uint32_t frames, uint32_t first_old, uint32_t span_old,
    uint32_t span_new, uint32_t anchor)
{
    if (span_new >= frames) return 0;
    uint32_t offset;
    if (span_old && anchor >= first_old && anchor - first_old < span_old)
        offset = dk_region_fraction(anchor - first_old, span_new, span_old);
    else
        offset = span_new / 2;
    uint32_t first = anchor > offset ? anchor - offset : 0;
    return first > frames - span_new ? frames - span_new : first;
}
/* New first frame of the viewport [first, first + span) after the edited marker
   `frame` moved. A frame inside the view does not pan. Outside, the view pans just
   enough to put the frame span / 8 from the edge it came from (left edge when
   the frame is before the view, right edge when after: first = frame + span / 8 - span). Clamped to the sample. */
static inline uint32_t dk_sample_view_follow(uint32_t frames, uint32_t first, uint32_t span,
    uint32_t frame)
{
    if (span >= frames) return 0;
    uint32_t margin = span / 8;
    if (frame < first) first = frame > margin ? frame - margin : 0;
    else if (frame >= first + span) first = frame + margin > span ? frame + margin - span : 0;
    return first > frames - span ? frames - span : first;
}
/* Peak min/max of plot column x from exact columns, the previous window or the
   overview. Returns 0 when no source covers it. */
static inline int dk_sample_region_column(int use_columns, const dk_sample_peak *columns,
    const dk_sample_peak *overview, unsigned bins, uint32_t bin_width,
    int have_prev, const dk_sample_peak *prev, uint32_t pfirst, uint32_t pspan, unsigned pcols,
    uint32_t left, uint32_t span, uint32_t width, uint32_t x, int32_t *omin, int32_t *omax)
{
    int32_t min = 0, max = 0;
    if (use_columns) {
        min = columns[x].min; max = columns[x].max;
    } else {
        uint32_t f0 = left + dk_region_fraction(span, x, width);
        uint32_t f1 = left + dk_region_fraction(span, x + 1, width);
        /* Short samples/zoom spans repeat a source bin across pixels rather
           than leaving holes when the pixel's integer frame interval is empty. */
        if (f1 <= f0) f1 = f0 + 1;
        if (have_prev && f0 >= pfirst && f1 - pfirst <= pspan) {
            /* The previous detail window covers this pixel: reuse its columns. */
            uint32_t c0 = dk_region_fraction(f0 - pfirst, pcols, pspan);
            uint32_t c1 = dk_region_fraction(f1 - 1 - pfirst, pcols, pspan);
            if (c1 >= pcols) c1 = pcols - 1;
            if (c0 > c1) c0 = c1;
            for (uint32_t i = c0; i <= c1; i++) {
                if (prev[i].min < min) min = prev[i].min;
                if (prev[i].max > max) max = prev[i].max;
            }
        } else {
            if (!(overview && bins)) return 0;
            uint32_t lo = f0 / bin_width, hi = (f1 - 1) / bin_width;
            if (hi >= bins) hi = bins - 1;
            if (lo >= bins) lo = bins - 1;
            for (uint32_t i = lo; i <= hi; i++) {
                if (overview[i].min < min) min = overview[i].min;
                if (overview[i].max > max) max = overview[i].max;
            }
        }
    }
    *omin = min; *omax = max;
    return 1;
}
/* v * gain_q8 / 256, saturated to the int16 range. */
static inline int32_t dk_sample_region_gain(int32_t v, uint32_t gain_q8)
{
    v = (v * (int32_t)gain_q8) >> 8;
    return v > 32767 ? 32767 : v < -32768 ? -32768 : v;
}
/* Draws the waveform of the viewport [first, first + span) of a sample of
   `frames` frames. With columns != 0 and ncols == plot width, pixel x is
   columns[x]; otherwise each pixel is derived from the overview bins. Then the
   centre line and the START/END markers (frame positions). A marker inside
   [first, first + span] is a line with a flag (START's at the top, END's at the
   bottom); a marker outside the viewport is a solid 3 x 5 px arrow at that side of
   the plot, at the flag row, pointing outwards.
   The _detail form also takes a previous detail window prev (pcols columns over
   frames [pfirst, pfirst + pspan)): when no exact columns are given, each pixel
   whose frame range lies inside it takes min/max of the touched prev columns;
   other pixels use the overview.
   Vertical auto-scale: the peak amplitude amp of the visible columns gives
   gain_q8 = amp == 0 ? 256 : min(vmax_q8, max(256, 32767 * 256 / amp)). vmax_q8 256
   turns it off, 2048 allows up to 8x. Display only. vmax_q8 is limited to 8192. */
static inline void dk_sample_region_draw_detail(const dk_page_canvas_v1 *c, dk_sample_region_rect r,
    uint32_t frames, uint32_t first, uint32_t span, const dk_sample_peak *overview, unsigned bins,
    const dk_sample_peak *columns, unsigned ncols,
    const dk_sample_peak *prev, uint32_t pfirst, uint32_t pspan, unsigned pcols,
    uint32_t start_frame, uint32_t end_frame, unsigned vmax_q8)
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
    int have_prev = prev && pcols && pspan;
    uint32_t gain = DK_SAMPLE_ZOOM_ONE;
    if (vmax_q8 > 8192) vmax_q8 = 8192;
    if (vmax_q8 > DK_SAMPLE_ZOOM_ONE && (use_columns || have_prev || (overview && bins))) {
        uint32_t amp = 0;
        for (uint32_t x = 0; x < width; x++) {
            int32_t min, max;
            if (!dk_sample_region_column(use_columns, columns, overview, bins, bin_width,
                    have_prev, prev, pfirst, pspan, pcols, left, span, width, x, &min, &max)) continue;
            uint32_t m = (uint32_t)(-min), p = (uint32_t)max;
            if (m > amp) amp = m;
            if (p > amp) amp = p;
        }
        if (amp) {
            gain = 32767u * DK_SAMPLE_ZOOM_ONE / amp;
            if (gain < DK_SAMPLE_ZOOM_ONE) gain = DK_SAMPLE_ZOOM_ONE;
            if (gain > vmax_q8) gain = vmax_q8;
        }
    }
    for (uint32_t x = 0; (use_columns || have_prev || (overview && bins)) && x < width; x++) {
        int32_t min, max;
        if (!dk_sample_region_column(use_columns, columns, overview, bins, bin_width,
                have_prev, prev, pfirst, pspan, pcols, left, span, width, x, &min, &max)) continue;
        min = dk_sample_region_gain(min, gain); max = dk_sample_region_gain(max, gain);
        dk_sample_region_fill(c, r, r.left + (int32_t)x, mid - max * radius / 32768,
                     r.left + (int32_t)x, mid - min * radius / 32768, 1);
    }
    /* Selection rail and handles stay distinct from the waveform. Markers beyond
       the viewport are arrows; the rail runs to the plot edge on that side. */
    int a_in = a >= left && a - left <= span, b_in = b >= left && b - left <= span;
    int32_t ax = a < left ? 0 : a - left > span ? (int32_t)width - 1 :
        (int32_t)dk_region_fraction(a - left, width - 1, span);
    int32_t bx = b < left ? 0 : b - left > span ? (int32_t)width - 1 :
        (int32_t)dk_region_fraction(b - left, width - 1, span);
    ax += r.left; bx += r.left;
    if (b >= a) dk_sample_region_fill(c, r, ax, bottom, bx, bottom, 1);
    if (a_in) {
        dk_sample_region_fill(c, r, ax - 1, top, ax + 1, bottom, 0);
        dk_sample_region_fill(c, r, ax, top, ax, bottom, 1);
    }
    if (b_in) {
        dk_sample_region_fill(c, r, bx - 1, top, bx + 1, bottom, 0);
        dk_sample_region_fill(c, r, bx, top, bx, bottom, 1);
    }
    for (int i = 0; i < 5; i++) {
        int32_t d = i < 2 ? 2 - i : i - 2; /* distance from the arrow's middle row; the tip is at the plot edge */
        if (a_in) dk_sample_region_fill(c, r, ax, top + i, ax + 4 - i, top + i, 1);
        else if (a < left) dk_sample_region_fill(c, r, r.left + d, top + i, r.left + 2, top + i, 1);
        else dk_sample_region_fill(c, r, r.right - 2, top + i, r.right - d, top + i, 1);
        if (b_in) dk_sample_region_fill(c, r, bx - 4 + i, bottom - i, bx, bottom - i, 1);
        else if (b < left) dk_sample_region_fill(c, r, r.left + d, bottom - i, r.left + 2, bottom - i, 1);
        else dk_sample_region_fill(c, r, r.right - 2, bottom - i, r.right - d, bottom - i, 1);
    }
}
/* As dk_sample_region_draw_detail without a previous window and without auto-scale. */
static inline void dk_sample_region_draw(const dk_page_canvas_v1 *c, dk_sample_region_rect r,
    uint32_t frames, uint32_t first, uint32_t span, const dk_sample_peak *overview, unsigned bins,
    const dk_sample_peak *columns, unsigned ncols, uint32_t start_frame, uint32_t end_frame)
{
    dk_sample_region_draw_detail(c, r, frames, first, span, overview, bins, columns, ncols,
        0, 0, 0, 0, start_frame, end_frame, DK_SAMPLE_ZOOM_ONE);
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
