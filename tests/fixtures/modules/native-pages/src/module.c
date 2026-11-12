/* Neutral replacement pair. A's click action follows its new binding; E/H are
 * saved words, G on page one is local, and LEVEL stays independent. */
#include <dk/core/api.h>
#include <dk/machine/machine.h>
DK_MACHINE(twin, .name = "TWIN", .short_name = "TWN", .render = "_np_render",
           .note_on = "_np_note_on", .desc = "_np_desc");
DK_PARAM(twin, 0, twin_smp, "Sample", "SMP", 0, 15, 0);
DK_PARAM(twin, 1, twin_start, "Start", "START", 0, 127, 16);
DK_PARAM(twin, 2, twin_end, "End", "END", 0, 127, 112);
DK_PAGE(twin, "TWN", "Twin", DK_KNOB(0), DK_KNOB(1), DK_KNOB(2));
DK_PARAM_ACTION(twin, 0, twin_click);
DK_NATIVE_PAGE_V2(twin, 0, "TWIN ZERO", twin_zero, twin_event, DK_PAGE_PARAM(0, 0),
                  DK_PAGE_PARAM(1, 4), DK_PAGE_PARAM(2, 7));
DK_NATIVE_PAGE_V2(twin, 1, "TWIN ONE", twin_one, twin_event, DK_PAGE_PARAM(1, 4),
                  DK_PAGE_LOCAL(0, 6), DK_PAGE_PARAM(2, 7));
static uint32_t draws[2], clicks, last_page;
static int32_t seen[4], zoom;
void twin_click(uint32_t track)
{
    if (track < 16)
        clicks++;
}
static int32_t status(uint32_t cmd, uint32_t seq, const uint8_t *body, uint32_t len, uint8_t *out)
{
    (void)cmd;
    (void)seq;
    (void)body;
    if (len)
        return -1;
    uint32_t words[] = {draws[0],          draws[1],          clicks,
                        last_page,         (uint32_t)seen[0], (uint32_t)seen[1],
                        (uint32_t)seen[2], (uint32_t)seen[3], (uint32_t)zoom};
    for (unsigned i = 0; i < 9; i++) {
        out[4 * i] = words[i] >> 24;
        out[4 * i + 1] = words[i] >> 16;
        out[4 * i + 2] = words[i] >> 8;
        out[4 * i + 3] = words[i];
    }
    return sizeof(words);
}
int32_t dkm_init(const dk_api *api)
{
    if (api->version != 1 ||
        api->size < offsetof(dk_api, register_cmds) + sizeof(api->register_cmds))
        return -1;
    return api->register_cmds(0x3f, 1, status);
}
static void number(const dk_page_canvas_v1 *c, int x, int y, unsigned value)
{
    char text[12], *at = text + sizeof(text) - 1;
    *at = 0;
    do {
        *--at = (char)('0' + value % 10);
        value /= 10;
    } while (value);
    c->text(x, y, at);
}
static void draw(const dk_page_frame_v1 *f, unsigned page)
{
    if (f->version != 1 || f->size < sizeof(*f) || f->track >= 16 || f->word_count < 3)
        return;
    draws[page]++;
    last_page = page;
    for (unsigned i = 0; i < 3; i++)
        seen[i] = f->word_values[i];
    seen[3] = f->level;
    const dk_page_canvas_v1 *c = &f->canvas;
    c->text(0, 7, page ? "CUSTOM ONE" : "CUSTOM ZERO");
    c->text(0, 19, "SMP");
    number(c, 20, 19, (unsigned)seen[0] >> 8);
    c->text(50, 19, "LEV");
    number(c, 72, 19, (unsigned)seen[3] >> 8);
    c->text(0, 42, "START");
    c->text(50, 42, "LOCAL");
    c->text(76, 42, "END");
    number(c, 0, 51, (unsigned)seen[1] >> 8);
    number(c, 50, 51, (unsigned)zoom);
    number(c, 76, 51, (unsigned)seen[2] >> 8);
}
void twin_zero(const dk_page_frame_v1 *f)
{
    draw(f, 0);
}
void twin_one(const dk_page_frame_v1 *f)
{
    draw(f, 1);
}
void twin_event(const dk_page_event_v1 *e)
{
    if (e->version == 1 && e->size >= sizeof(*e) && e->track < 16 && e->control == 0)
        zoom += e->delta;
}
