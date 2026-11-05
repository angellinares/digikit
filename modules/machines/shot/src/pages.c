#include "shot.h"

/* UI-only state. The existing START/END words continue through native editing;
   zoom consumes its own turn and never writes a parameter or sample epoch. */
#define PLOT_WIDTH 95u
#define ZOOM_STEPS_MAX 128 /* 16 doublings */
static int32_t page_zoom[16]; /* per track step index z: ratio 2^(z/8), one detent = one step */
static uint32_t last_start[16], last_end[16]; /* words seen at the last draw */
static uint8_t focus_end[16]; /* the active marker is END (else START) */
static uint8_t left_range[16]; /* page 1 was drawn since the last page-2 draw: the next page-2 draw is an entry */
static uint32_t view_first[16], view_span[16], view_frames[16]; /* viewport kept per track, and the span/frames it was computed for */

/* Pixel width of a string in the firmware's proportional font. Measured from the DN2 1.12
   framebuffer: 1 px between glyphs. Replace it with a firmware-centred text call on the
   canvas (planned SDK change). */
static int text_width(const char *s)
{
    int width = 0, n = 0;
    for (; *s; s++, n++)
        width += *s == 'T' || *s == 'x' || *s == '1' ? 3 : *s == 'M' ? 5 : *s == '.' ? 1 : 4;
    return n ? width + n - 1 : 0;
}

/* Draws s centred on centre2 / 2 (doubled coordinate, so half pixels work), rounding down. */
static void text_centered(const dk_page_canvas_v1 *canvas, int centre2, int y, const char *s)
{
    int w = text_width(s);
    int left = (centre2 - (w ? w - 1 : 0)) / 2;
    canvas->text(left, y, s);
}

/* Canvas row between the value row (text y 46 covers rows 42..46) and the label row (y 52 covers rows 48..52). */
#define MARK_ROW 47

static void page_number(const dk_page_canvas_v1 *canvas, int centre2, int y, unsigned value, int underline)
{
    char text[12];
    char *at = text + sizeof(text) - 1;
    *at = 0;
    do { *--at = (char)('0' + value % 10); value /= 10; } while (value);
    text_centered(canvas, centre2, y, at);
    if (underline) {
        int w = text_width(at);
        int left = (centre2 - (w ? w - 1 : 0)) / 2;
        canvas->fill_rect(left, MARK_ROW, left + w - 1, MARK_ROW, 1);
    }
}

static unsigned unit(int32_t raw)
{
    unsigned value = raw <= 0 ? 0 : (unsigned)raw >> 8;
    return value > 127 ? 127 : value;
}

static void region_values(const dk_page_frame_v1 *f, int end_active)
{
    const dk_page_canvas_v1 *c = &f->canvas;
    /* Names sit on page 1's label row; values one row above, centred on the names.
       The active marker's value is underlined. */
    c->text(5, 52, "START"); c->text(87, 52, "END");
    page_number(c, 31, 46, unit(f->word_values[SHOT_P_START]), !end_active);
    page_number(c, 187, 46, unit(f->word_values[SHOT_P_END]), end_active);
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
    left_range[f->track] = 1;
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

/* "x" + ratio: one decimal below 10 (x1.4), none from 10 (x512), xMAX above 9999. 5 characters at most. */
static void zoom_number(const dk_page_canvas_v1 *c, int centre2, int y, unsigned ratio)
{
    char text[6];
    unsigned tenths = (ratio * 10u + 128u) / 256u;
    text[0] = 'x';
    if (tenths < 100) {
        text[1] = (char)('0' + tenths / 10); text[2] = '.'; text[3] = (char)('0' + tenths % 10); text[4] = 0;
    } else {
        unsigned value = (ratio + 128u) / 256u;
        if (value > 9999) {
            text[1] = 'M'; text[2] = 'A'; text[3] = 'X'; text[4] = 0;
        } else {
            char *at = text + sizeof(text) - 1;
            *at = 0;
            do { *--at = (char)('0' + value % 10); value /= 10; } while (value);
            char *out = text + 1;
            while (*at) *out++ = *at++;
            *out = 0;
        }
    }
    text_centered(c, centre2, y, text);
}

/* Q8 ratio of zoom step z: 2^(z/8). */
static unsigned zoom_ratio(unsigned z)
{
    static const uint16_t eighth[8] = {256, 279, 304, 332, 362, 395, 431, 470};
    return (unsigned)eighth[z % 8] << (z / 8);
}

/* The DSP maps a START/END word to a frame: frames * word / 32512, word = unit << 8. */
static uint32_t word_frame(uint32_t frames, unsigned unit_value)
{
    uint32_t frame = dk_region_fraction(frames, unit_value << 8, 32512);
    return frame > frames ? frames : frame;
}

void shot_range_draw(const dk_page_frame_v1 *f)
{
    if (f->version != 1 || f->size < sizeof(*f) || f->track >= 16 || f->word_count < SHOT_P_COUNT) return;
    unsigned slot = unit(f->word_values[SHOT_P_SMP]);
    dk_sample_info info;
    int ready = sample_view_update(&shot_samples, slot, &info, 1);
    uint32_t real = ready == 0 ? info.frames : 0;
    uint32_t frames = real ? real : 127; /* no sample: a normalized region with markers and the zero line */
    unsigned ratio = zoom_ratio(page_zoom[f->track]);
    if (real) {
        /* At most one frame per column. */
        uint32_t least = frames < PLOT_WIDTH ? frames : PLOT_WIDTH;
        uint32_t cap = dk_region_fraction(frames, DK_SAMPLE_ZOOM_ONE, least);
        if (ratio > cap) ratio = cap;
    }
    dk_sample_region_rect plot = {0, 6, PLOT_WIDTH - 1, 36};
    unsigned bins = 0;
    const dk_sample_peak *peaks = ready == 0 ? sample_view_peaks(&shot_samples, &bins) : 0;
    unsigned start_unit = unit(f->word_values[SHOT_P_START]), end_unit = unit(f->word_values[SHOT_P_END]);
    uint32_t start_word = start_unit << 8, end_word = end_unit << 8;
    /* Entering the page (after page 1 was drawn, or the first draw) resets the active marker to START and
       takes the current words as seen. Then the active marker is the one turned between two page-2 draws
       (START wins when both changed). */
    int edited = 0;
    int entering = left_range[f->track] || !last_start[f->track]; /* stored +1: 0 means not seen yet */
    left_range[f->track] = 0;
    if (entering) {
        focus_end[f->track] = 0;
    } else {
        if (start_word + 1 != last_start[f->track]) { focus_end[f->track] = 0; edited = 1; }
        else if (end_word + 1 != last_end[f->track]) { focus_end[f->track] = 1; edited = 1; }
    }
    last_start[f->track] = start_word + 1; last_end[f->track] = end_word + 1;
    uint32_t start_frame = word_frame(frames, start_unit), end_frame = word_frame(frames, end_unit);
    uint32_t anchor = focus_end[f->track] ? end_frame : start_frame;
    uint32_t first, span;
    dk_sample_viewport(frames, 0, ratio, PLOT_WIDTH, &first, &span); /* span only */
    /* A zoom or a new sample keeps the active marker at its screen position (a new sample
       starts from the whole-sample view). Then an edited marker pans the view into sight. */
    if (view_frames[f->track] != frames) {
        view_first[f->track] = 0; view_span[f->track] = frames;
    }
    if (view_span[f->track] != span || view_frames[f->track] != frames)
        view_first[f->track] = dk_sample_view_zoom(frames, view_first[f->track], view_span[f->track], span, anchor);
    if (edited) view_first[f->track] = dk_sample_view_follow(frames, view_first[f->track], span, anchor);
    view_frames[f->track] = frames; view_span[f->track] = span;
    if (entering && span >= frames) view_first[f->track] = 0; /* 1x: fit the whole sample */
    first = view_first[f->track];
    const dk_sample_peak *window = 0;
    unsigned count = 0;
    /* A pixel that covers less than one overview bin needs the detail window; until it is ready the overview is stretched. */
    if (real && span < (frames + DK_SAMPLE_PEAK_BINS - 1) / DK_SAMPLE_PEAK_BINS * PLOT_WIDTH)
        window = sample_view_window(&shot_samples, slot, info.token, first, span, PLOT_WIDTH, &count);
    const dk_sample_peak *prev = 0;
    uint32_t pfirst = 0, pspan = 0;
    unsigned pcols = 0;
    /* Exact window not ready: keep drawing the last ready one (rescaled) where it covers the viewport. */
    if (real && !window) prev = sample_view_last_window(&shot_samples, slot, info.token, &pfirst, &pspan, &pcols);
    dk_sample_region_draw_detail(&f->canvas, plot, frames, first, span, peaks, bins, window, count,
        prev, pfirst, pspan, pcols, start_frame, end_frame, 2048);
    region_values(f, focus_end[f->track]);
    f->canvas.text(58, 52, "ZOOM");
    zoom_number(&f->canvas, 135, 46, ratio);
}

void shot_range_event(const dk_page_event_v1 *event)
{
    if (event->version != 1 || event->size < sizeof(*event) || event->track >= 16 || event->control != 0)
        return;
    int delta = event->delta;
    if (delta < -16) delta = -16;
    if (delta > 16) delta = 16;
    int next = page_zoom[event->track] + delta;
    page_zoom[event->track] = next < 0 ? 0 : next > ZOOM_STEPS_MAX ? ZOOM_STEPS_MAX : next;
}
