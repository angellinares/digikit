#include "shot.h"

/* UI-only state. The existing START/END words continue through native editing;
   zoom consumes its own turn and never writes a parameter or sample epoch. */
static int32_t page_zoom[16]; /* Q8 ratio offset from 1x: 0..768, 1/64x per detent */

static void page_number(const dk_page_canvas_v1 *canvas, int center, int y, unsigned value)
{
    char text[12];
    char *at = text + sizeof(text) - 1;
    *at = 0;
    do { *--at = (char)('0' + value % 10); value /= 10; } while (value);
    canvas->text(center - (int)(text + sizeof(text) - 1 - at) * 6 / 2, y, at);
}

static unsigned unit(int32_t raw)
{
    unsigned value = raw <= 0 ? 0 : (unsigned)raw >> 8;
    return value > 127 ? 127 : value;
}

static void region_values(const dk_page_frame_v1 *f)
{
    const dk_page_canvas_v1 *c = &f->canvas;
    c->text(8, 43, "START"); c->text(89, 43, "END");
    page_number(c, 19, 51, unit(f->word_values[SHOT_P_START]));
    page_number(c, 97, 51, unit(f->word_values[SHOT_P_END]));
}

static unsigned sample_visual(int ready, int status)
{
    return ready == 0 ? DK_WIDGET_SAMPLE_READY :
        status == DK_SAMPLE_UNUSED ? DK_WIDGET_SAMPLE_EMPTY :
        status < 0 ? DK_WIDGET_SAMPLE_MISSING : DK_WIDGET_SAMPLE_LOADING;
}

void shot_controls_draw(const dk_page_frame_v1 *f)
{
    if (f->version != 1 || f->size < sizeof(*f) || f->track >= 16 || f->word_count < SHOT_P_COUNT) return;
    const dk_page_canvas_v1 *c = &f->canvas;
    unsigned slot = unit(f->word_values[SHOT_P_SMP]);
    dk_sample_info info;
    int ready = sample_view_update(&shot_samples, slot, &info, 0);
    /* Only top A/B/D glyph rectangles are replaced. Native labels, E/H
       encoders and the separate physical LEV fader stay untouched. */
    c->fill_rect(0, 0, 22, 19, 0);
    dk_page_tune(c, 0, 0, ((int)unit(f->word_values[SHOT_P_PITCH])-64)*3);
    c->fill_rect(24, 0, 47, 19, 0);
    dk_sample_play_glyph(c, 29, 7, unit(f->word_values[SHOT_P_PLAY]));
    c->fill_rect(76, 0, 98, 19, 0);
    int status = ready == 0 ? DK_SAMPLE_READY : sample_view_status(&shot_samples, slot);
    dk_page_sample(c, 76, 0, slot, sample_visual(ready, status));
}

static void zoom_number(const dk_page_canvas_v1 *c, int x, int y, unsigned ratio)
{
    unsigned value = (ratio * 100u + 128u) / 256u;
    char text[5] = {(char)('0' + value / 100), '.', (char)('0' + value / 10 % 10), (char)('0' + value % 10), 0};
    c->text(x, y, text);
}

void shot_range_draw(const dk_page_frame_v1 *f)
{
    if (f->version != 1 || f->size < sizeof(*f) || f->track >= 16 || f->word_count < SHOT_P_COUNT) return;
    unsigned slot = unit(f->word_values[SHOT_P_SMP]);
    dk_sample_info info;
    int ready = sample_view_update(&shot_samples, slot, &info, 1);
    unsigned ratio = DK_SAMPLE_ZOOM_ONE + (unsigned)page_zoom[f->track];
    dk_sample_region_rect plot = {0, 6, 94, 36};
    unsigned bins = 0;
    const dk_sample_peak *peaks = ready == 0 ? sample_view_peaks(&shot_samples, &bins) : 0;
    dk_sample_region_rect_q8(&f->canvas, peaks, bins, ready == 0 ? info.frames : 0,
        unit(f->word_values[SHOT_P_START]), unit(f->word_values[SHOT_P_END]), ratio, plot);
    region_values(f);
    f->canvas.text(58, 43, "ZOOM");
    zoom_number(&f->canvas, 59, 51, ratio);
}

void shot_range_event(const dk_page_event_v1 *event)
{
    if (event->version != 1 || event->size < sizeof(*event) || event->track >= 16 || event->control != 0)
        return;
    int delta = event->delta;
    if (delta < -192) delta = -192;
    if (delta > 192) delta = 192;
    int next = page_zoom[event->track] + delta * 4;
    page_zoom[event->track] = next < 0 ? 0 : next > 768 ? 768 : next;
}
