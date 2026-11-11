/* Neutral SDK fixture: the second native page reuses two saved words.
 * LOCAL zoom is UI-only state, independent per source track. No firmware calls. */
#include <dk/machine/machine.h>
#include <dk/core/api.h>

DK_MACHINE(native, .name = "NATIVE", .short_name = "NAT", .render = "_np_render",
           .note_on = "_np_note_on", .desc = "_np_desc");
DK_PARAM(native, 0, native_start, "Start", "START", 0, 127, 16);
DK_PARAM(native, 1, native_end, "End", "END", 0, 127, 112);
DK_PAGE(native, "NAT", "Native proof", DK_KNOB(0), DK_KNOB(1));
DK_NATIVE_PAGE_V1(native, "Native proof", native_draw, native_event,
                  DK_PAGE_PARAM(0, 0), DK_PAGE_LOCAL(0, 2), DK_PAGE_PARAM(1, 7));

static int32_t zoom[16];
/* Exported measurements let the harness check state without inferring numbers from pixels. */
uint32_t native_draws, native_events, native_track;
int32_t native_start_seen, native_end_seen, native_zoom_seen, native_level_seen;

/* Read-only test observation; no parameter or UI mutations through RPC. */
static int32_t status(uint32_t cmd, uint32_t seq, const uint8_t *body, uint32_t len, uint8_t *out)
{
    (void)cmd; (void)seq; (void)body;
    if (len) return -1;
    uint32_t words[] = { native_draws, native_events, native_track,
        (uint32_t)native_start_seen, (uint32_t)native_end_seen, (uint32_t)native_zoom_seen, (uint32_t)native_level_seen };
    for (unsigned i = 0; i < 7; i++) {
        out[4*i] = words[i] >> 24; out[4*i+1] = words[i] >> 16;
        out[4*i+2] = words[i] >> 8; out[4*i+3] = words[i];
    }
    return sizeof(words);
}

int32_t dkm_init(const dk_api *api)
{
    if (api->version != 1 || api->size < offsetof(dk_api, register_cmds) + sizeof(api->register_cmds)
        || !api->register_cmds) return -1;
    return api->register_cmds(0x3e, 1, status);
}

static void number(const dk_page_canvas_v1 *canvas, int x, int y, int value)
{
    char text[12];
    char *at = text + sizeof(text) - 1;
    *at = 0;
    unsigned mag = value < 0 ? (unsigned)-value : (unsigned)value;
    do { *--at = (char)('0' + mag % 10); mag /= 10; } while (mag);
    if (value < 0) *--at = '-';
    canvas->text(x, y, at);
}

void native_draw(const dk_page_frame_v1 *frame)
{
    const dk_page_canvas_v1 *canvas = &frame->canvas;
    if (frame->version != 1 || frame->size < sizeof(*frame) || frame->track >= 16)
        return;
    native_draws++;
    native_track = frame->track;
    native_start_seen = frame->params[0];
    native_end_seen = frame->params[7];
    native_zoom_seen = zoom[frame->track];
    native_level_seen = frame->level;
    canvas->text(0, 7, "LEVEL");
    number(canvas, 32, 7, frame->level);
    canvas->text(0, 18, "START");
    canvas->text(38, 18, "ZOOM");
    canvas->text(76, 18, "END");
    number(canvas, 0, 27, frame->params[0] >> 8);
    number(canvas, 42, 27, zoom[frame->track]);
    number(canvas, 76, 27, frame->params[7] >> 8);
    /* Deliberately cross the canvas edges: the adapter clips both shapes. */
    canvas->fill_rect(-12, 34, 30, 39, -1);
    canvas->fill_rect(66, 44, (int)canvas->width + 12, (int)canvas->height + 8, -1);
}

void native_event(const dk_page_event_v1 *event)
{
    if (event->version != 1 || event->size < sizeof(*event) || event->track >= 16 || event->control != 0)
        return;
    int next = zoom[event->track] + event->delta;
    zoom[event->track] = next < 0 ? 0 : next > 127 ? 127 : next;
    native_events++;
}
