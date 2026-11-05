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

int main(void) {
    check_viewport();
    check_bars_and_ramp();
    return 0;
}
