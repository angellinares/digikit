/* Generic native callback page adapter. All firmware details stay here.
 * The runtime validates owner, callback offsets and bindings before registration.
 * Content sees an ephemeral bounded canvas and LOCAL events, never a Bitmap or view. */
#include <stdint.h>
#include <dk/page/page.h>
#include "dk_fw_page.h"
#include "dk_fw_draw.h"
#include "dk_fw_text.h"
#include "dk_fw_view.h"
#include "native-page-dots.h"
#include "native-page-mask.h"

enum { TYPES = 32, WIDTH = 99, WIDE_WIDTH = 107, HEIGHT = 53, ORIGIN_X = 25 };
typedef struct text_rep {
    uint32_t len, cap;
    int32_t refs;
    char text[24];
} text_rep;
typedef struct native_page {
    uint32_t draw, event, flags;
    uint32_t bindings[8][3];
    uint32_t word_count, word_codes[8];
    text_rep name;
    uint32_t descriptor[11];
} native_page;
static native_page pages[TYPES][2];
static uint8_t modes[TYPES]; /* 1 append-only v1, 2 replacement pair v2 */
static uint32_t armed;

/* Bitmap has two working planes; each native constructor allocates
 * width * ceil(height/32) * 16 bytes per plane. Retain that full extent. */
typedef struct bitmap {
    uint32_t vptr, width, height, bands;
    uint32_t *plane0, *plane1;
} bitmap;
static uint32_t pixels0[WIDE_WIDTH * 2 * 4], pixels1[WIDE_WIDTH * 2 * 4];
static bitmap clipped;
static bitmap *drawing;
static uint32_t overlay_mask[WIDE_WIDTH * 2];
static uint32_t overlay;

/* The generated header exposes data anchors as byte symbols. This anchor is a
 * four-byte address operand, so use its actual width when loading the pointer. */
extern const uint32_t native_font_address __asm__("cf.page.data.native_font_operand");

extern int32_t mf_native_type(uint8_t *view);
#if !NP_DT2
extern uint32_t mf_reader_real(uint8_t *sound);
int32_t mf_native_type(uint8_t *view)
{
    uint32_t index = dk_fw_page_native_sound_index(view);
    uint8_t *sound;
    if (index < 0x80)
        sound = dk_fw_page_native_library_sound(*(uint8_t **)(view + 0x68), index);
    else {
        uint32_t track = dk_fw_page_view_track(*(uint8_t **)(view + 0x74));
        sound = dk_fw_page_native_track_sound(*(uint8_t **)(view + 0x78), track);
    }
    return (int32_t)mf_reader_real(sound);
}
#endif

static native_page *selected(uint8_t *view)
{
    uint32_t index = *(uint32_t *)(view + 0x90);
    if (!armed || index >= 2)
        return 0;
    int32_t type = mf_native_type(view);
    return type >= 0 && type < TYPES && pages[type][index].draw ? &pages[type][index] : 0;
}

/* Bindings are (kind, resolved PARAM code / LOCAL id, encoder position).
 * Copies are adapter-owned; removal clears callbacks before the runtime frees code. */
static int32_t register_page(uint32_t type, uint32_t index, uint32_t mode, uint32_t flags,
                             const char *title, uint32_t draw, uint32_t event,
                             const uint32_t *bindings, const uint32_t *words, uint32_t word_count)
{
    if (flags & ~(DK_PAGE_STOCK_OVERLAY | DK_PAGE_WIDE_CANVAS) || (flags && mode != 2) ||
        (flags & DK_PAGE_STOCK_OVERLAY && index != 0) ||
        (flags & DK_PAGE_WIDE_CANVAS && index != 1) || type >= TYPES || index >= 2 || !draw ||
        !event || pages[type][index].draw || (modes[type] && modes[type] != mode) ||
        word_count > 8 || (word_count && !words))
        return -1;
    native_page *page = &pages[type][index];
    for (unsigned i = 0; i < 24; i++)
        page->name.text[i] = 0;
    unsigned len = 0;
    while (title[len] && len < 23) {
        page->name.text[len] = title[len];
        len++;
    }
    page->name.len = len;
    page->name.cap = 23;
    page->name.refs = -1;
    page->descriptor[0] = page->descriptor[1] = (uint32_t)page->name.text;
    for (unsigned i = 2; i < 11; i++)
        page->descriptor[i] = 0;
    page->descriptor[10] = 10; /* native track level code, not a new parameter */
    for (unsigned i = 0; i < 8; i++) {
        for (unsigned j = 0; j < 3; j++)
            page->bindings[i][j] = bindings[3 * i + j];
        if (page->bindings[i][0] == 1)
            page->descriptor[2 + page->bindings[i][2]] = page->bindings[i][1];
    }
    page->word_count = word_count;
    for (unsigned i = 0; i < word_count; i++)
        page->word_codes[i] = words[i];
    page->event = event;
    page->flags = flags;
    modes[type] = (uint8_t)mode;
    page->draw = draw; /* publication last */
    armed++;
    return 0;
}

int32_t mf_native_register(uint32_t type, const char *title, uint32_t draw, uint32_t event,
                           const uint32_t *bindings, const uint32_t *words, uint32_t word_count)
{
    return register_page(type, 1, 1, 0, title, draw, event, bindings, words, word_count);
}

int32_t mf_native_register_v2(uint32_t type, uint32_t index, uint32_t flags, const char *title,
                              uint32_t draw, uint32_t event, const uint32_t *bindings,
                              const uint32_t *words, uint32_t word_count)
{
    return register_page(type, index, 2, flags, title, draw, event, bindings, words, word_count);
}

void mf_native_remove(uint32_t type)
{
    if (type >= TYPES)
        return;
    for (unsigned i = 0; i < 2; i++) {
        if (pages[type][i].draw) {
            pages[type][i].draw = pages[type][i].event = 0;
            armed--;
        }
    }
    modes[type] = 0;
}

uint32_t mf_native_page_count(uint32_t type, uint32_t ordinary)
{
    if (type >= TYPES || !modes[type])
        return ordinary;
    return modes[type] == 2 ? 2 : ordinary + 1;
}

uint32_t mf_native_descriptor(uint32_t type, uint32_t page)
{
    return type < TYPES && page < 2 && pages[type][page].draw
               ? (uint32_t)pages[type][page].descriptor
               : 0;
}

/* -1 resumes the original mapper; zero is a valid consumed mapping for LOCAL/unused. */
int32_t mf_native_code(uint8_t *view, uint32_t encoder)
{
    native_page *page = selected(view);
    if (!page || encoder >= 8)
        return -1;
    for (unsigned i = 0; i < 8; i++)
        if (page->bindings[i][0] == 1 && page->bindings[i][2] == encoder)
            return (int32_t)page->bindings[i][1];
    return 0;
}

static void canvas_text(int32_t x, int32_t y, const char *text)
{
    if (!drawing || x < 0 || x >= (int32_t)drawing->width || y < 0 || y >= HEIGHT || !text)
        return;
    char bounded[33];
    unsigned i = 0;
    while (i < 32 && text[i]) {
        bounded[i] = text[i];
        i++;
    }
    bounded[i] = 0;
    dk_fw_text_draw_fmt((uint8_t *)drawing, (uint8_t *)native_font_address, x, HEIGHT - 1 - y, 0,
                        "%s", bounded);
}

static void canvas_fill(int32_t x0, int32_t y0, int32_t x1, int32_t y1, int32_t color)
{
    if (!drawing)
        return;
    if (x0 > x1) {
        int t = x0;
        x0 = x1;
        x1 = t;
    }
    if (y0 > y1) {
        int t = y0;
        y0 = y1;
        y1 = t;
    }
    if (x1 < 0 || y1 < 0 || x0 >= (int32_t)drawing->width || y0 >= HEIGHT)
        return;
    if (x0 < 0)
        x0 = 0;
    if (y0 < 0)
        y0 = 0;
    if (x1 >= (int32_t)drawing->width)
        x1 = (int32_t)drawing->width - 1;
    if (y1 >= HEIGHT)
        y1 = HEIGHT - 1;
    if (overlay)
        native_page_mask_rect(overlay_mask, drawing->width, HEIGHT, x0, y0, x1, y1);
    dk_fw_draw_fill_rect((uint8_t *)drawing, x0, HEIGHT - 1 - y1, x1, HEIGHT - 1 - y0,
                         color ? 1 : 0);
}

static void paint(uint8_t *view, uint8_t *canvas, native_page *page, uint32_t track)
{
    uint32_t width = page->flags & DK_PAGE_WIDE_CANVAS ? WIDE_WIDTH : WIDTH;
    uint32_t origin_x = ORIGIN_X - (width - WIDTH);
    overlay = page->flags & DK_PAGE_STOCK_OVERLAY;
    for (unsigned i = 0; i < width * 2; i++)
        overlay_mask[i] = 0;
    /* Field copies avoid a compiler-emitted memcpy in this freestanding adapter. */
    clipped.vptr = ((bitmap *)canvas)->vptr;
    clipped.width = width;
    clipped.height = HEIGHT;
    clipped.bands = 2;
    clipped.plane0 = pixels0;
    clipped.plane1 = pixels1;
    for (unsigned i = 0; i < width * 2 * 4; i++)
        pixels0[i] = pixels1[i] = 0;
    /* blit uses plane1 as a write mask. Keep the final band's eleven rows
     * transparent so the rounded-up storage cannot write outside HEIGHT. */
    for (unsigned x = 0; x < width; x++) {
        pixels1[2 * x] = UINT32_MAX;
        pixels1[2 * x + 1] = UINT32_MAX << (64 - HEIGHT);
    }
    dk_page_frame_v1 frame;
    frame.version = 1;
    frame.size = sizeof(frame);
    frame.track = track;
    frame.canvas.width = width;
    frame.canvas.height = HEIGHT;
    frame.canvas.text = canvas_text;
    frame.canvas.fill_rect = canvas_fill;
    /* Preserve explicit stores rather than a freestanding memset dependency. */
    for (unsigned i = 0; i < 8; i++)
        ((volatile int32_t *)frame.params)[i] = 0;
    uint32_t *vtable = *(uint32_t **)view;
    int32_t (*value)(uint8_t *, uint32_t, int32_t) = (void *)vtable[0x54 / 4];
    for (unsigned i = 0; i < 8; i++)
        if (page->bindings[i][0] == 1)
            frame.params[page->bindings[i][2]] = value(view, page->bindings[i][1], -1);
    frame.level = value(view, page->descriptor[10], -1);
    frame.word_count = page->word_count;
    for (unsigned i = 0; i < 8; i++)
        frame.word_values[i] = i < page->word_count ? value(view, page->word_codes[i], -1) : 0;
    drawing = &clipped;
    ((void (*)(const dk_page_frame_v1 *))page->draw)(&frame);
    drawing = 0;
    if (overlay) {
        for (unsigned i = 0; i < width * 2; i++)
            pixels1[i] = overlay_mask[i];
    } else
        dk_fw_draw_fill_rect(canvas, origin_x, 0, 127, 52, 0);
    dk_fw_draw_blit(canvas, (uint8_t *)&clipped, origin_x, 0, 0);
    if (!overlay)
        native_page_dots(canvas, *(uint32_t *)(view + 0x90), dk_fw_draw_fill_rect);
}

/* 0 resumes stock, 1 handled, 2 requests stock then overlay. The hook invokes
 * stock only after this C frame has unwound; never nest stock draw in paint. */
uint32_t mf_native_draw(uint8_t *view, uint8_t *canvas)
{
    native_page *page = selected(view);
    if (!page)
        return 0;
    uint32_t track = dk_fw_page_view_track(*(uint8_t **)(view + 0x74));
    if (track >= 16)
        return 0;
    if (page->flags & DK_PAGE_STOCK_OVERLAY)
        return 2;
    paint(view, canvas, page, track);
    return 1;
}

void mf_native_overlay(uint8_t *view, uint8_t *canvas)
{
    /* Stock callbacks may change the view: revalidate before invoking content. */
    native_page *page = selected(view);
    if (!page || !(page->flags & DK_PAGE_STOCK_OVERLAY))
        return;
    uint32_t track = dk_fw_page_view_track(*(uint8_t **)(view + 0x74));
    if (track < 16)
        paint(view, canvas, page, track);
}

uint32_t mf_native_turn(uint8_t *view, const uint8_t *event)
{
    native_page *page = selected(view);
    uint32_t encoder = *(const uint32_t *)(event + 0xc) - 1;
    if (!page || encoder >= 8)
        return 0;
    for (unsigned i = 0; i < 8; i++) {
        const uint32_t *binding = page->bindings[i];
        if (binding[0] && binding[2] == encoder) {
            if (binding[0] == 1)
                return 0; /* exactly one native PARAM edit */
            dk_page_event_v1 local = {1,
                                      sizeof(local),
                                      dk_fw_page_view_track(*(uint8_t **)(view + 0x74)),
                                      binding[1],
                                      *(const int32_t *)(event + 0x10),
                                      event[0x14] != 0};
            ((void (*)(const dk_page_event_v1 *))page->event)(&local);
            dk_fw_view_invalidate(view);
            return 1;
        }
    }
    return 1; /* an unused encoder on this page must not edit a hidden parameter */
}
