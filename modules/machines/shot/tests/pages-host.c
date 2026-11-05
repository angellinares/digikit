/* Host test of the SHOT pages and the pool view: region draw, per-token peak failure cache, empty slot and zoom label. */
#include <assert.h>
#include <string.h>
#include "shot.h"

sample_view shot_samples;

static unsigned queries, current_token = 1;
static int current_state = DK_SAMPLE_READY, available, center_lines, empty;
static unsigned char bitmap[53][107];
static int start_x = -1, end_x = -1, two_digit_x = -1, range_draw;
static char zoom_label[8];

static int32_t status(uint32_t h, uint32_t i) { (void)h; (void)i; return current_state; }
static int32_t info(uint32_t h, uint32_t i, uint8_t *out) {
    (void)h; (void)i;
    if (current_state != DK_SAMPLE_READY) return DK_SAMPLE_E_NOT_FOUND;
    dk_sample_info value = {48000, 48000, 1, current_token};
    memcpy(out, &value, sizeof(value));
    return 0;
}
static int32_t peaks(uint32_t h, uint32_t i, uint32_t token, uint8_t *out, uint32_t cap) {
    (void)h; (void)i;
    assert(token == current_token);
    assert(cap == DK_SAMPLE_PEAK_BINS * sizeof(dk_sample_peak));
    queries++;
    if (!available) return DK_SAMPLE_E_READ;
    memset(out, 0, cap);
    return 512;
}
static void text(int32_t x, int32_t y, const char *s) {
    if (range_draw && y == 43) {
        if (!strcmp(s, "START")) assert(x == 8);
        if (!strcmp(s, "ZOOM")) assert(x == 58);
        if (!strcmp(s, "END")) assert(x == 89);
    }
    if (!strcmp(s, "EMPTY")) empty++;
    if (y == 51 && !strcmp(s, "0")) start_x = x;
    if (y == 51 && !strcmp(s, "127")) end_x = x;
    if (y == 51 && !strcmp(s, "10")) two_digit_x = x;
    if (y == 51 && x == 59) { assert(strlen(s) < sizeof(zoom_label)); strcpy(zoom_label, s); }
}
static void rect(int32_t a, int32_t b, int32_t c, int32_t d, int32_t color) {
    if (range_draw) assert(a >= 0 && c <= 94 && b >= 6 && d <= 36);
    for (int y = b; y <= d; y++) for (int x = a; x <= c; x++)
        if (x >= 0 && x < 107 && y >= 0 && y < 53) bitmap[y][x] = color != 0;
    if (a == 0 && b == 21 && c == 94 && d == 21) center_lines++;
}

int main(void) {
    dk_api api = {.version = 1, .size = sizeof(api),
        .sample_pool_info = info, .sample_pool_status = status, .sample_pool_peaks = peaks};
    dk_page_frame_v1 f = {.version = 1, .size = sizeof(f), .track = 0,
        .params = {[7] = 127 << 8}, .word_count = 5, .word_values = {[2] = 127 << 8},
        .canvas = {107, 53, text, rect}};
    sample_view_init(&shot_samples, &api, 1);
    range_draw = 1;
    shot_range_draw(&f); shot_range_draw(&f);
    assert(queries == 1 && center_lines == 2);
    assert(start_x == 16 && end_x == 88);
    /* Wide origin x17 preserves global E/H value positions x33/x105. */
    for (int y = 0; y < 53; y++) for (int x = 0; x < 107; x++)
        if (y < 6 || y > 36 || x > 94) assert(bitmap[y][x] == 0);
    /* A two-digit number is centred on x19: 19 - 2 * 6 / 2. */
    f.word_values[SHOT_P_START] = 10 << 8; shot_range_draw(&f);
    assert(two_digit_x == 13);
    f.word_values[SHOT_P_START] = 0;
    /* Another slot cannot cause this failed generation to be retried. */
    f.word_values[0] = 1 << 8; current_token = 2; shot_range_draw(&f);
    f.word_values[0] = 0; current_token = 1; shot_range_draw(&f);
    assert(queries == 2);
    available = 1; current_token = 3; shot_range_draw(&f); shot_range_draw(&f);
    unsigned bins = 0;
    assert(queries == 3 && sample_view_peaks(&shot_samples, &bins) != 0 && bins == 512);
    current_state = DK_SAMPLE_UNUSED; shot_range_draw(&f);
    range_draw = 0; shot_controls_draw(&f); range_draw = 1;
    /* Draws without peaks show a centre line; the two-digit draw above adds one to the count. */
    assert(empty == 0 && sample_view_peaks(&shot_samples, &bins) == 0 && bins == 0 && center_lines == 8);
    current_state = DK_SAMPLE_READY; current_token = 4; shot_range_draw(&f);
    assert(queries == 4);
    /* Zoom is observed in the ZOOM label at (59, 51). */
    dk_page_event_v1 event = {.version = 1, .size = sizeof(event), .track = 0, .control = 0, .delta = 1, .coarse = 0};
    shot_range_event(&event); shot_range_draw(&f); assert(!strcmp(zoom_label, "1.02"));
    event.delta = 1000; shot_range_event(&event); shot_range_draw(&f); assert(!strcmp(zoom_label, "4.00"));
    event.delta = -1000; shot_range_event(&event); shot_range_draw(&f); assert(!strcmp(zoom_label, "1.00"));
    /* Another control or event version leaves the zoom unchanged. */
    event.delta = 1; event.control = 1; shot_range_event(&event); shot_range_draw(&f);
    assert(!strcmp(zoom_label, "1.00"));
    event.control = 0; event.version = 2; shot_range_event(&event); shot_range_draw(&f);
    assert(!strcmp(zoom_label, "1.00"));
    return 0;
}
