/* Checks the sample view: marker clipping in a zoomed rectangle, full bar coverage of a short sample and the Q8 zoom ramp. */
#include <assert.h>
#include <string.h>
#include <dk/sample/view.h>

static unsigned calls, left_mark, right_mark;
static void viewport_rect(int32_t a,int32_t b,int32_t c,int32_t d,int32_t color) {
    assert(a>=7 && c<=87 && b>=9 && d<=39 && a<=c && b<=d);
    calls++;
    if(color && a==7 && c==7 && b==9 && d==39) left_mark++;
    if(color && a==87 && c==87 && b==9 && d==39) right_mark++;
}
static void check_viewport(void) {
    dk_page_canvas_v1 canvas={107,53,0,viewport_rect};
    dk_sample_view_rect plot={7,9,87,39};
    dk_sample_peak peaks[512];
    for(unsigned i=0;i<512;i++) { peaks[i].min=-32768; peaks[i].max=32767; }
    dk_sample_view_rect_q8(&canvas,peaks,512,48000,0,127,1024,plot);
    /* END (frame 48000) lies beyond the view [0, 12000): an arrow, no marker line at the right edge. */
    assert(calls>81 && left_mark==1 && right_mark==0);
    /* START-centred zoom also turns an END beyond the visible interval into an arrow. */
    dk_sample_view_rect_q8(&canvas,peaks,512,48000,64,127,1024,plot);
    assert(right_mark==0);
    unsigned before=calls;
    plot.right=plot.left;
    dk_sample_view_rect_q8(&canvas,0,0,0,0,127,256,plot);
    assert(calls==before);
}

static unsigned bars;
static unsigned char bitmap[53][107];
static void bar_text(int32_t x, int32_t y, const char *s) { (void)x; (void)y; (void)s; }
static void bar_rect(int32_t a,int32_t b,int32_t c,int32_t d,int32_t color) {
    for (int y=b; y<=d; y++) for (int x=a; x<=c; x++)
        if (x>=0 && x<107 && y>=0 && y<53) bitmap[y][x] = color != 0;
    if (a == c && b > 18 && b < 29 && d > 29 && d < 41) bars++;
}
static void check_bars_and_ramp(void) {
    dk_page_canvas_v1 canvas={99,53,bar_text,bar_rect};
    /* At 4x a 128-frame sample spans fewer frames than display pixels.
       Every pixel must repeat a source bin, with no gaps at bin boundaries. */
    dk_sample_peak short_peaks[128];
    for (unsigned i = 0; i < 128; i++) {
        short_peaks[i].min = -16000; short_peaks[i].max = 16000;
    }
    bars = 0;
    dk_sample_view_rect_q8(&canvas, short_peaks, 128, 128, 0, 127, DK_SAMPLE_ZOOM_ONE << 2, (dk_sample_view_rect){0, 18, 98, 41});
    assert(bars == 99);
    dk_sample_peak ramp[512]; unsigned char before[53][107];
    for (unsigned i=0;i<512;i++) { ramp[i].min=-(int16_t)(i*40); ramp[i].max=(int16_t)(i*40); }
    memset(bitmap,0,sizeof(bitmap));
    dk_sample_view_rect_q8(&canvas,ramp,512,51200,64,127,256,(dk_sample_view_rect){0,0,98,34});
    memcpy(before,bitmap,sizeof(before)); memset(bitmap,0,sizeof(bitmap));
    dk_sample_view_rect_q8(&canvas,ramp,512,51200,64,127,320,(dk_sample_view_rect){0,0,98,34});
    assert(memcmp(before,bitmap,sizeof(before)) != 0);
}

static void check_zoom_and_columns(void) {
    uint32_t first, span;
    /* Centred on the focus, clamped at both ends. */
    dk_sample_view_viewport(48000, 24000, 512, 95, &first, &span);
    assert(span == 24000 && first == 12000);
    dk_sample_view_viewport(48000, 100, 512, 95, &first, &span);
    assert(span == 24000 && first == 0);
    dk_sample_view_viewport(48000, 47900, 512, 95, &first, &span);
    assert(span == 24000 && first == 24000);
    /* Deep zoom keeps one frame per column; a short sample shows all frames. */
    dk_sample_view_viewport(48000, 24000, 256u * 4096u, 95, &first, &span);
    assert(span == 95 && first == 24000 - 47);
    dk_sample_view_viewport(50, 10, 256u * 64u, 95, &first, &span);
    assert(span == 50 && first == 0);
    dk_sample_view_viewport(48000, 0, 100, 95, &first, &span);
    assert(span == 48000 && first == 0);

    /* 64x of 51200 frames: 800 frames over 99 columns, no gaps. */
    dk_page_canvas_v1 canvas = {99, 53, bar_text, bar_rect};
    dk_sample_peak flat[512];
    for (unsigned i = 0; i < 512; i++) { flat[i].min = -16000; flat[i].max = 16000; }
    bars = 0;
    dk_sample_view_rect_q8(&canvas, flat, 512, 51200, 64, 127, 256 * 64, (dk_sample_view_rect){0, 18, 98, 41});
    assert(bars == 99);

    /* The columns path draws exactly the given columns. */
    dk_sample_peak cols[99];
    for (unsigned i = 0; i < 99; i++) {
        cols[i].min = 0; cols[i].max = (i & 1) ? 0 : 30000;
    }
    memset(bitmap, 0, sizeof(bitmap));
    dk_sample_view_draw(&canvas, (dk_sample_view_rect){0, 18, 98, 41}, 51200, 0, 800,
        flat, 512, cols, 99, 0, 51200);
    /* Odd columns have no waveform above the centre line (y 29); even columns do. */
    for (unsigned x = 8; x < 90; x++) {
        if (x & 1) assert(bitmap[22][x] == 0);
        else assert(bitmap[22][x] == 1);
    }
    /* A column count other than the width falls back to the overview. */
    memset(bitmap, 0, sizeof(bitmap));
    dk_sample_view_draw(&canvas, (dk_sample_view_rect){0, 18, 98, 41}, 51200, 0, 800,
        flat, 512, cols, 50, 0, 51200);
    /* The overview (+-16000) reaches about 5 px above the centre: both neighbours are drawn at y 26. */
    assert(bitmap[26][51] == 1 && bitmap[26][50] == 1);
}

static void check_previous_detail(void) {
    dk_page_canvas_v1 canvas = {99, 53, bar_text, bar_rect};
    dk_sample_view_rect plot = {0, 18, 98, 41};
    dk_sample_peak zero[512], prev[99];
    memset(zero, 0, sizeof(zero));
    for (unsigned i = 0; i < 99; i++) { prev[i].min = 0; prev[i].max = 30000; }
    /* A covering previous window is the source: the overview is flat, so only the window draws above the centre. */
    memset(bitmap, 0, sizeof(bitmap));
    dk_sample_view_draw_detail(&canvas, plot, 51200, 0, 800, zero, 512, 0, 0,
        prev, 0, 800, 99, 0, 51200, 256);
    for (unsigned x = 8; x < 90; x++) assert(bitmap[22][x] == 1);
    /* Partial coverage (frames 400..800): uncovered pixels use the overview, covered ones the window. */
    memset(bitmap, 0, sizeof(bitmap));
    dk_sample_view_draw_detail(&canvas, plot, 51200, 0, 800, zero, 512, 0, 0,
        prev, 400, 400, 99, 0, 51200, 256);
    for (unsigned x = 8; x < 45; x++) assert(bitmap[22][x] == 0);
    for (unsigned x = 55; x < 90; x++) assert(bitmap[22][x] == 1);
    /* Zoom in: viewport 100..300 lies in prev columns 1..3 of 10 (80 frames each); no gaps. */
    dk_sample_peak coarse[10];
    for (unsigned i = 0; i < 10; i++) { coarse[i].min = 0; coarse[i].max = (i >= 1 && i <= 3) ? 30000 : 0; }
    memset(bitmap, 0, sizeof(bitmap));
    dk_sample_view_draw_detail(&canvas, plot, 51200, 100, 200, zero, 512, 0, 0,
        coarse, 0, 800, 10, 0, 51200, 256);
    for (unsigned x = 8; x < 90; x++) assert(bitmap[22][x] == 1);
    /* An exact window wins over the previous one. */
    dk_sample_peak none[99];
    memset(none, 0, sizeof(none));
    memset(bitmap, 0, sizeof(bitmap));
    dk_sample_view_draw_detail(&canvas, plot, 51200, 0, 800, zero, 512, none, 99,
        prev, 0, 800, 99, 0, 51200, 256);
    for (unsigned x = 8; x < 90; x++) assert(bitmap[22][x] == 0);
}

static void check_view_zoom_follow(void) {
    /* Anchor 15000 in [12000, 24000): p = 3000 / 12000 = 0.25; span 6000 gives offset 1500, first 13500. */
    assert(dk_sample_view_zoom(48000, 12000, 12000, 6000, 15000) == 13500);
    /* Anchor 13500: p = 1500 / 12000 = 0.125; offset 750, first 12750. */
    assert(dk_sample_view_zoom(48000, 12000, 12000, 6000, 13500) == 12750);
    /* Anchor outside the old view uses p = 1/2: 1000 - 3000 clamps to 0; 47000 - 3000 clamps to 42000. */
    assert(dk_sample_view_zoom(48000, 12000, 12000, 6000, 1000) == 0);
    assert(dk_sample_view_zoom(48000, 12000, 12000, 6000, 47000) == 42000);
    /* Inside, near the end: 11900 / 12000 of 6000 = 5950, first 47900 - 5950 = 41950 (below the limit 42000). */
    assert(dk_sample_view_zoom(48000, 36000, 12000, 6000, 47900) == 41950);
    /* Inside, at the end of the view [42000, 48000): offset 5999 * 3000 / 6000 = 2999, first 47999 - 2999 = 45000 (the limit). */
    assert(dk_sample_view_zoom(48000, 42000, 6000, 3000, 47999) == 45000);
    /* Zooming out to the whole sample gives 0. */
    assert(dk_sample_view_zoom(48000, 12000, 12000, 48000, 15000) == 0);

    /* Span 8000: margin 1000. A frame inside the view does not pan. */
    assert(dk_sample_view_follow(48000, 20000, 8000, 21000) == 20000);
    /* Before the view: the frame sits 1000 from the left edge. */
    assert(dk_sample_view_follow(48000, 20000, 8000, 10000) == 9000);
    /* After the view: the frame sits 1000 from the right edge, first = 30000 + 1000 - 8000. */
    assert(dk_sample_view_follow(48000, 20000, 8000, 30000) == 23000);
    /* Clamped to the sample at both ends. */
    assert(dk_sample_view_follow(48000, 20000, 8000, 100) == 0);
    assert(dk_sample_view_follow(48000, 20000, 8000, 47999) == 40000);
}

static void check_arrows_and_scale(void) {
    dk_page_canvas_v1 canvas = {99, 53, bar_text, bar_rect};
    dk_sample_view_rect plot = {0, 18, 98, 41};
    dk_sample_peak flat[512];
    for (unsigned i = 0; i < 512; i++) { flat[i].min = 0; flat[i].max = 0; }
    /* View [10000, 10800): START (0) is left of it, END (51200) right of it. Left arrow rows
       (tip at x 0, y 20): x 2, 1..2, 0..2, 1..2, 2. Right arrow at the END row (y 39 is the middle): x 96..98 there. */
    memset(bitmap, 0, sizeof(bitmap));
    dk_sample_view_draw(&canvas, plot, 51200, 10000, 800, flat, 512, 0, 0, 0, 51200);
    assert(bitmap[18][2] == 1 && bitmap[18][1] == 0 && bitmap[18][0] == 0);
    assert(bitmap[19][1] == 1 && bitmap[19][0] == 0);
    assert(bitmap[20][0] == 1 && bitmap[20][1] == 1 && bitmap[20][2] == 1 && bitmap[20][3] == 0);
    assert(bitmap[22][2] == 1 && bitmap[22][1] == 0);
    assert(bitmap[39][96] == 1 && bitmap[39][97] == 1 && bitmap[39][98] == 1 && bitmap[39][95] == 0);
    assert(bitmap[40][96] == 1 && bitmap[40][97] == 1 && bitmap[40][98] == 0);
    /* No marker lines at the edges. */
    assert(bitmap[25][0] == 0 && bitmap[25][98] == 0 && bitmap[30][0] == 0 && bitmap[30][98] == 0);

    /* Auto-scale on exact columns; the plot radius is 9 px, so a +-16000 column is 4 px high unscaled. */
    dk_sample_peak cols[99];
    for (unsigned i = 0; i < 99; i++) { cols[i].min = -2000; cols[i].max = 2000; }
    /* vmax 256: unchanged, +-2000 * 9 / 32768 = 0: a flat line. */
    memset(bitmap, 0, sizeof(bitmap));
    dk_sample_view_draw_detail(&canvas, plot, 51200, 0, 800, 0, 0, cols, 99, 0, 0, 0, 0, 0, 51200, 256);
    assert(bitmap[28][50] == 0 && bitmap[30][50] == 0);
    /* vmax 2048: 32767 / 2000 is about 16, capped to 8: 16000 -> 4 px each side (y 25..33). */
    memset(bitmap, 0, sizeof(bitmap));
    dk_sample_view_draw_detail(&canvas, plot, 51200, 0, 800, 0, 0, cols, 99, 0, 0, 0, 0, 0, 51200, 2048);
    assert(bitmap[25][50] == 1 && bitmap[24][50] == 0 && bitmap[33][50] == 1 && bitmap[34][50] == 0);
    /* +-16000: gain 32767 * 256 / 16000 = 524 (about 2x): 32750 -> 8 px each side (y 21..37). */
    for (unsigned i = 0; i < 99; i++) { cols[i].min = -16000; cols[i].max = 16000; }
    memset(bitmap, 0, sizeof(bitmap));
    dk_sample_view_draw_detail(&canvas, plot, 51200, 0, 800, 0, 0, cols, 99, 0, 0, 0, 0, 0, 51200, 2048);
    assert(bitmap[21][50] == 1 && bitmap[20][50] == 0 && bitmap[37][50] == 1 && bitmap[38][50] == 0);
    /* Silence stays a flat line (centre y 29). */
    for (unsigned i = 0; i < 99; i++) { cols[i].min = 0; cols[i].max = 0; }
    memset(bitmap, 0, sizeof(bitmap));
    dk_sample_view_draw_detail(&canvas, plot, 51200, 0, 800, 0, 0, cols, 99, 0, 0, 0, 0, 0, 51200, 2048);
    assert(bitmap[29][50] == 1 && bitmap[28][50] == 0 && bitmap[30][50] == 0);
}

int main(void) {
    check_view_zoom_follow();
    check_arrows_and_scale();
    check_zoom_and_columns();
    check_previous_detail();
    check_viewport();
    check_bars_and_ramp();
    return 0;
}
