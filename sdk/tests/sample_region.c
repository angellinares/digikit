/* Checks the sample region: marker clipping in a zoomed rectangle, full bar coverage of a short sample and the Q8 zoom ramp. */
#include <assert.h>
#include <string.h>
#include <dk/sample_region.h>

static unsigned calls, left_mark, right_mark;
static void viewport_rect(int32_t a,int32_t b,int32_t c,int32_t d,int32_t color) {
    assert(a>=7 && c<=87 && b>=9 && d<=39 && a<=c && b<=d);
    calls++;
    if(color && a==7 && c==7 && b==9 && d==39) left_mark++;
    if(color && a==87 && c==87 && b==9 && d==39) right_mark++;
}
static void check_viewport(void) {
    dk_page_canvas_v1 canvas={107,53,0,viewport_rect};
    dk_sample_region_rect plot={7,9,87,39};
    dk_sample_peak peaks[512];
    for(unsigned i=0;i<512;i++) { peaks[i].min=-32768; peaks[i].max=32767; }
    dk_sample_region_rect_q8(&canvas,peaks,512,48000,0,127,1024,plot);
    assert(calls>81 && left_mark==1 && right_mark==1);
    /* START-centred zoom also clips an END beyond the visible interval. */
    dk_sample_region_rect_q8(&canvas,peaks,512,48000,64,127,1024,plot);
    assert(right_mark==2);
    unsigned before=calls;
    plot.right=plot.left;
    dk_sample_region_rect_q8(&canvas,0,0,0,0,127,256,plot);
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
    dk_sample_region(&canvas, short_peaks, 128, 128, 0, 127, 2, 18, 41);
    assert(bars == 99);
    dk_sample_peak ramp[512]; unsigned char before[53][107];
    for (unsigned i=0;i<512;i++) { ramp[i].min=-(int16_t)(i*40); ramp[i].max=(int16_t)(i*40); }
    memset(bitmap,0,sizeof(bitmap));
    dk_sample_region_q8(&canvas,ramp,512,51200,64,127,256,0,34);
    memcpy(before,bitmap,sizeof(before)); memset(bitmap,0,sizeof(bitmap));
    dk_sample_region_q8(&canvas,ramp,512,51200,64,127,320,0,34);
    assert(memcmp(before,bitmap,sizeof(before)) != 0);
}

static void check_zoom_and_columns(void) {
    uint32_t first, span;
    /* Centred on the focus, clamped at both ends. */
    dk_sample_viewport(48000, 24000, 512, 95, &first, &span);
    assert(span == 24000 && first == 12000);
    dk_sample_viewport(48000, 100, 512, 95, &first, &span);
    assert(span == 24000 && first == 0);
    dk_sample_viewport(48000, 47900, 512, 95, &first, &span);
    assert(span == 24000 && first == 24000);
    /* Deep zoom keeps one frame per column; a short sample shows all frames. */
    dk_sample_viewport(48000, 24000, 256u * 4096u, 95, &first, &span);
    assert(span == 95 && first == 24000 - 47);
    dk_sample_viewport(50, 10, 256u * 64u, 95, &first, &span);
    assert(span == 50 && first == 0);
    dk_sample_viewport(48000, 0, 100, 95, &first, &span);
    assert(span == 48000 && first == 0);

    /* 64x of 51200 frames: 800 frames over 99 columns, no gaps. */
    dk_page_canvas_v1 canvas = {99, 53, bar_text, bar_rect};
    dk_sample_peak flat[512];
    for (unsigned i = 0; i < 512; i++) { flat[i].min = -16000; flat[i].max = 16000; }
    bars = 0;
    dk_sample_region_q8(&canvas, flat, 512, 51200, 64, 127, 256 * 64, 18, 41);
    assert(bars == 99);

    /* The columns path draws exactly the given columns. */
    dk_sample_peak cols[99];
    for (unsigned i = 0; i < 99; i++) {
        cols[i].min = 0; cols[i].max = (i & 1) ? 0 : 30000;
    }
    memset(bitmap, 0, sizeof(bitmap));
    dk_sample_region_draw(&canvas, (dk_sample_region_rect){0, 18, 98, 41}, 51200, 0, 800,
        flat, 512, cols, 99, 0, 51200);
    /* Odd columns have no waveform above the centre line (y 29); even columns do. */
    for (unsigned x = 8; x < 90; x++) {
        if (x & 1) assert(bitmap[22][x] == 0);
        else assert(bitmap[22][x] == 1);
    }
    /* A column count other than the width falls back to the overview. */
    memset(bitmap, 0, sizeof(bitmap));
    dk_sample_region_draw(&canvas, (dk_sample_region_rect){0, 18, 98, 41}, 51200, 0, 800,
        flat, 512, cols, 50, 0, 51200);
    /* The overview (+-16000) reaches about 5 px above the centre: both neighbours are drawn at y 26. */
    assert(bitmap[26][51] == 1 && bitmap[26][50] == 1);
}

static void check_previous_detail(void) {
    dk_page_canvas_v1 canvas = {99, 53, bar_text, bar_rect};
    dk_sample_region_rect plot = {0, 18, 98, 41};
    dk_sample_peak zero[512], prev[99];
    memset(zero, 0, sizeof(zero));
    for (unsigned i = 0; i < 99; i++) { prev[i].min = 0; prev[i].max = 30000; }
    /* A covering previous window is the source: the overview is flat, so only the window draws above the centre. */
    memset(bitmap, 0, sizeof(bitmap));
    dk_sample_region_draw_detail(&canvas, plot, 51200, 0, 800, zero, 512, 0, 0,
        prev, 0, 800, 99, 0, 51200);
    for (unsigned x = 8; x < 90; x++) assert(bitmap[22][x] == 1);
    /* Partial coverage (frames 400..800): uncovered pixels use the overview, covered ones the window. */
    memset(bitmap, 0, sizeof(bitmap));
    dk_sample_region_draw_detail(&canvas, plot, 51200, 0, 800, zero, 512, 0, 0,
        prev, 400, 400, 99, 0, 51200);
    for (unsigned x = 8; x < 45; x++) assert(bitmap[22][x] == 0);
    for (unsigned x = 55; x < 90; x++) assert(bitmap[22][x] == 1);
    /* Zoom in: viewport 100..300 lies in prev columns 1..3 of 10 (80 frames each); no gaps. */
    dk_sample_peak coarse[10];
    for (unsigned i = 0; i < 10; i++) { coarse[i].min = 0; coarse[i].max = (i >= 1 && i <= 3) ? 30000 : 0; }
    memset(bitmap, 0, sizeof(bitmap));
    dk_sample_region_draw_detail(&canvas, plot, 51200, 100, 200, zero, 512, 0, 0,
        coarse, 0, 800, 10, 0, 51200);
    for (unsigned x = 8; x < 90; x++) assert(bitmap[22][x] == 1);
    /* An exact window wins over the previous one. */
    dk_sample_peak none[99];
    memset(none, 0, sizeof(none));
    memset(bitmap, 0, sizeof(bitmap));
    dk_sample_region_draw_detail(&canvas, plot, 51200, 0, 800, zero, 512, none, 99,
        prev, 0, 800, 99, 0, 51200);
    for (unsigned x = 8; x < 90; x++) assert(bitmap[22][x] == 0);
}

int main(void) {
    check_zoom_and_columns();
    check_previous_detail();
    check_viewport();
    check_bars_and_ramp();
    return 0;
}
