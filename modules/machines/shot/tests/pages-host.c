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
static unsigned window_calls, window_ready_after = 2, window_columns, window_first, window_span;
static int window_hold; /* 1: new windows stay pending */
static int32_t peaks_window(uint32_t h, uint32_t i, uint32_t token, uint32_t first, uint32_t frames,
    uint32_t columns, uint8_t *out, uint32_t cap) {
    (void)h; (void)i;
    assert(token == current_token && columns == 95 && cap >= columns * sizeof(dk_sample_peak));
    window_calls++; window_first = first; window_span = frames; window_columns = columns;
    if (window_calls < window_ready_after || window_hold) return 0;
    dk_sample_peak *p = (dk_sample_peak *)out;
    for (unsigned k = 0; k < columns; k++) { p[k].min = -16384; p[k].max = 16384; }
    return (int32_t)columns;
}
static void text(int32_t x, int32_t y, const char *s) {
    if (range_draw && y == 52) {
        if (!strcmp(s, "START")) assert(x == 5);
        if (!strcmp(s, "ZOOM")) assert(x == 58);
        if (!strcmp(s, "END")) assert(x == 87);
    }
    if (!strcmp(s, "EMPTY")) empty++;
    if (y == 46 && !strcmp(s, "0")) start_x = x;
    if (y == 46 && !strcmp(s, "127")) end_x = x;
    if (y == 46 && !strcmp(s, "10")) two_digit_x = x;
    if (y == 46 && s[0] == 'x') { assert(strlen(s) < sizeof(zoom_label)); strcpy(zoom_label, s); }
}
static void rect(int32_t a, int32_t b, int32_t c, int32_t d, int32_t color) {
    if (range_draw && b != 47) assert(a >= 0 && c <= 94 && b >= 6 && d <= 36);
    for (int y = b; y <= d; y++) for (int x = a; x <= c; x++)
        if (x >= 0 && x < 107 && y >= 0 && y < 53) bitmap[y][x] = color != 0;
    if (a == 0 && b == 21 && c == 94 && d == 21) center_lines++;
}

int main(void) {
    dk_api api = {.version = 1, .size = sizeof(api),
        .sample_pool_info = info, .sample_pool_status = status, .sample_pool_peaks = peaks,
        .sample_pool_peaks_window = peaks_window};
    dk_page_frame_v1 f = {.version = 1, .size = sizeof(f), .track = 0,
        .params = {[7] = 127 << 8}, .word_count = 5, .word_values = {[2] = 127 << 8},
        .canvas = {107, 53, text, rect}};
    sample_view_init(&shot_samples, &api, 1);
    range_draw = 1;
    shot_range_draw(&f); shot_range_draw(&f);
    assert(queries == 1 && center_lines == 2);
    /* Values are centred on the labels (canvas 15.5 and 93.5): "0" is 4 wide, "127" is 13 wide. */
    assert(start_x == 14 && end_x == 87);
    /* Wide origin x17 preserves global E/H value positions x33/x105. */
    for (int y = 0; y < 53; y++) for (int x = 0; x < 107; x++)
        if ((y < 6 || y > 36 || x > 94) && y != 47) assert(bitmap[y][x] == 0);
    /* "10" is 3 + 4 + 1 = 8 wide, centred on 15.5: floor((31 - 7) / 2) = 12. */
    f.word_values[SHOT_P_START] = 10 << 8; shot_range_draw(&f);
    assert(two_digit_x == 12);
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
    /* Zoom is observed in the ZOOM label on the value row (y 46); "x1.0" is 14 wide, left 61. */
    dk_page_event_v1 event = {.version = 1, .size = sizeof(event), .track = 0, .control = 0, .delta = 1, .coarse = 0};
    /* One detent is one step of 2^(1/8); a delta clamps to 16 steps per event. */
    shot_range_event(&event); shot_range_draw(&f); assert(!strcmp(zoom_label, "x1.1"));
    event.delta = 1000; shot_range_event(&event); shot_range_draw(&f); assert(!strcmp(zoom_label, "x4.4"));
    event.delta = -1000; shot_range_event(&event); event.delta = -1000; shot_range_event(&event);
    shot_range_draw(&f); assert(!strcmp(zoom_label, "x1.0"));
    /* No window is requested at 1x or 4.4x (a pixel covers more than one overview bin). */
    assert(window_calls == 0);
    /* Another control or event version leaves the zoom unchanged. */
    event.delta = 1; event.control = 1; shot_range_event(&event); shot_range_draw(&f);
    assert(!strcmp(zoom_label, "x1.0"));
    event.control = 0; event.version = 2; shot_range_event(&event); shot_range_draw(&f);
    assert(!strcmp(zoom_label, "x1.0"));
    /* Deep zoom (40 steps = 32x) asks for a 95-column window of the viewport at START. */
    event.version = 1; event.delta = 16; shot_range_event(&event); shot_range_event(&event);
    event.delta = 8; shot_range_event(&event);
    memset(bitmap, 0, sizeof(bitmap));
    shot_range_draw(&f);
    assert(!strcmp(zoom_label, "x32") && window_calls == 1);
    assert(window_columns == 95 && window_first == 0 && window_span == 1500);
    assert(bitmap[15][50] == 0); /* pending: the zeroed overview is stretched */
    shot_range_draw(&f);
    assert(window_calls == 2 && bitmap[15][50] == 1 && bitmap[27][50] == 1);
    /* One more detent whose window stays pending: the previous detail (not the zeroed overview) is drawn. */
    window_hold = 1;
    event.delta = 1; shot_range_event(&event);
    memset(bitmap, 0, sizeof(bitmap));
    shot_range_draw(&f);
    assert(window_span < 1500 && bitmap[15][50] == 1 && bitmap[27][50] == 1);
    event.delta = -1; shot_range_event(&event);
    window_hold = 0;
    shot_range_draw(&f);
    assert(window_span == 1500);
    /* Zoom is anchored on the active marker (START by default, frame 0): first stays 0.
       Step 41 is ratio 279 << 5 = 8928, span = 48000 * 256 / 8928 = 1376 (5 * 256 + 96). */
    event.delta = 1; shot_range_event(&event); shot_range_draw(&f);
    assert(window_span == 1376 && window_first == 0);
    event.delta = -1; shot_range_event(&event); shot_range_draw(&f);
    assert(window_span == 1500 && window_first == 0);
    /* START turned to unit 10 (frame 48000 * 2560 / 32512 = 3779) is right of the view [0, 1500).
       It pans so START sits span / 8 = 187 from the right edge: first = 3779 + 187 - 1500 = 2466. */
    f.word_values[SHOT_P_START] = 10 << 8; shot_range_draw(&f);
    assert(window_first == 2466);
    /* One detent in: START is at offset 3779 - 2466 = 1313 of 1500, so offset = 1313 * 1376 / 1500 = 1204
       in the new view: first = 3779 - 1204 = 2575. */
    event.delta = 1; shot_range_event(&event); shot_range_draw(&f);
    assert(window_span == 1376 && window_first == 2575);
    /* One detent out: offset 1204 of 1376 gives 1204 * 1500 / 1376 = 1312: first = 3779 - 1312 = 2467. */
    event.delta = -1; shot_range_event(&event); shot_range_draw(&f);
    assert(window_span == 1500 && window_first == 2467);
    /* END turned to unit 64 (frame 48000 * 16384 / 32512 = 24188) is far right of the view [2467, 3967).
       END is active now: first = 24188 + 187 - 1500 = 22875. */
    f.word_values[SHOT_P_END] = 64 << 8; shot_range_draw(&f);
    assert(window_first == 22875);
    /* One detent in keeps END's position: offset 24188 - 22875 = 1313 of 1500 gives 1204 of 1376,
       first = 24188 - 1204 = 22984. */
    event.delta = 1; shot_range_event(&event); shot_range_draw(&f);
    assert(window_span == 1376 && window_first == 22984);
    /* END to unit 63 (frame 23811) lies inside [22984, 24360): the view does not pan. */
    f.word_values[SHOT_P_END] = 63 << 8; shot_range_draw(&f);
    assert(window_first == 22984);
    /* Entering page 2 after page 1 was drawn: END changed meanwhile (frame 48000, far outside the view)
       is no turn. START stays the anchor, so the view does not pan and START's value is underlined
       (canvas row 47, between the value row and the label row). "10" is 8 wide at x12..19;
       "127" is 13 wide at x87..99. */
    range_draw = 0; shot_controls_draw(&f); range_draw = 1;
    f.word_values[SHOT_P_END] = 127 << 8;
    memset(bitmap, 0, sizeof(bitmap));
    shot_range_draw(&f);
    assert(window_first == 22984);
    for (int x = 12; x <= 19; x++) assert(bitmap[47][x] == 1);
    for (int x = 87; x <= 99; x++) assert(bitmap[47][x] == 0);
    /* END turned between two consecutive page-2 draws makes END active: its value "64" (9 wide, x89..97) is underlined. */
    f.word_values[SHOT_P_END] = 64 << 8;
    memset(bitmap, 0, sizeof(bitmap));
    shot_range_draw(&f);
    for (int x = 89; x <= 97; x++) assert(bitmap[47][x] == 1);
    for (int x = 12; x <= 19; x++) assert(bitmap[47][x] == 0);
    return 0;
}
