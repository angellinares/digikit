"""The native Shot page caches missing overviews per READY generation."""

import shutil
import subprocess

import pytest
from support.paths import REPO_ROOT


@pytest.mark.skipif(shutil.which("cc") is None, reason="no host C compiler")
def test_region_failure_cache_replacement_and_empty_slot(tmp_path):
    source = tmp_path / "region.c"
    source.write_text(
        r"""
#include <assert.h>
#include <string.h>
#include "modules/machines/shot/src/module.c"
static unsigned queries, current_token = 1;
static int current_state = DK_SAMPLE_READY, available, center_lines, bars, empty;
static unsigned char bitmap[53][107];
static int start_x=-1,end_x=-1,two_digit_x=-1,range_draw;
int dk_picker_init(const dk_api *api, uint32_t h) { (void)api; (void)h; return 0; }
void dk_picker_fini(void) {}
void dk_picker_open_pool(int c, dk_picker_pick_fn *p, void *ctx) { (void)c; (void)p; (void)ctx; }
int dk_ui_toast(const char *s) { (void)s; return 0; }
static int32_t tracks(uint32_t id) { (void)id; return 1; }
static int32_t status(uint32_t h, uint32_t i) { (void)h; (void)i; return current_state; }
static int32_t info(uint32_t h, uint32_t i, uint8_t *out) {
    (void)h; (void)i;
    if (current_state != DK_SAMPLE_READY) return DK_SAMPLE_E_NOT_FOUND;
    dk_sample_info value = {48000, 48000, 1, current_token};
    memcpy(out, &value, sizeof(value));
    return 0;
}
static int32_t peaks(uint32_t h, uint32_t i, uint32_t token, uint8_t *out, uint32_t cap) {
    (void)h; (void)i; assert(token == current_token); assert(cap == sizeof(page_peaks));
    queries++;
    if (!available) return DK_SAMPLE_E_READ;
    memset(out, 0, cap);
    return 512;
}
static void text(int32_t x, int32_t y, const char *s) {
    if (range_draw && y==43) {
        if (!strcmp(s,"START")) assert(x==8);
        if (!strcmp(s,"ZOOM")) assert(x==58);
        if (!strcmp(s,"END")) assert(x==89);
    }
    if (!strcmp(s, "EMPTY")) empty++;
    if (y==51 && !strcmp(s,"0")) start_x=x;
    if (y==51 && !strcmp(s,"127")) end_x=x;
    if (y==51 && !strcmp(s,"10")) two_digit_x=x;
}
static void rect(int32_t a, int32_t b, int32_t c, int32_t d, int32_t color) {
    if(range_draw) assert(a>=0 && c<=94 && b>=6 && d<=36);
    for (int y=b; y<=d; y++) for (int x=a; x<=c; x++)
        if (x>=0 && x<107 && y>=0 && y<53) bitmap[y][x] = color != 0;
    if (a == 0 && b == 21 && c == 94 && d == 21) center_lines++;
    if (a == c && b > 18 && b < 29 && d > 29 && d < 41) bars++;
}
int main(void) {
    dk_api api = {.version=1, .size=sizeof(api), .sample_tracks=tracks,
        .sample_pool_info=info, .sample_pool_status=status, .sample_pool_peaks=peaks};
    dk_page_frame_v1 f = {.version=1, .size=sizeof(f), .track=0,
        .params={[7]=127<<8}, .word_count=5, .word_values={[2]=127<<8},
        .canvas={107,53,text,rect}};
    assert(dkm_init(&api) == 0);
    range_draw=1;
    shot_range_draw(&f); shot_range_draw(&f);
    assert(queries == 1 && center_lines == 2);
    assert(start_x==16 && end_x==88);
    /* Wide origin x17 preserves global E/H value positions x33/x105. */
    for (int y=0;y<53;y++) for(int x=0;x<107;x++)
        if(y<6 || y>36 || x>94) assert(bitmap[y][x]==0);
    page_number(&f.canvas,11,51,10);assert(two_digit_x==5);
    /* Another slot cannot cause this failed generation to be retried. */
    f.word_values[0] = 1 << 8; current_token = 2; shot_range_draw(&f);
    f.word_values[0] = 0; current_token = 1; shot_range_draw(&f);
    assert(queries == 2);
    available = 1; current_token = 3; shot_range_draw(&f); shot_range_draw(&f);
    assert(queries == 3 && peak_count == 512);
    current_state = DK_SAMPLE_UNUSED; shot_range_draw(&f);
    range_draw=0; shot_controls_draw(&f); range_draw=1;
    assert(empty == 0 && peak_count == 0 && center_lines == 7);
    current_state = DK_SAMPLE_READY; current_token = 4; shot_range_draw(&f);
    assert(queries == 4);
    dk_page_event_v1 event = {1, sizeof(event), 0, 0, 1, 0};
    shot_range_event(&event); assert(page_zoom[0] == 4);
    event.delta = 1000; shot_range_event(&event); assert(page_zoom[0] == 768);
    event.delta = -1000; shot_range_event(&event); assert(page_zoom[0] == 0);
    range_draw=0;
    /* At 4x a 128-frame sample spans fewer frames than display pixels.
       Every pixel must repeat a source bin, with no gaps at bin boundaries. */
    dk_sample_peak short_peaks[128];
    for (unsigned i = 0; i < 128; i++) {
        short_peaks[i].min = -16000; short_peaks[i].max = 16000;
    }
    bars = 0;
    f.canvas.width=99;
    dk_sample_region(&f.canvas, short_peaks, 128, 128, 0, 127, 2, 18, 41);
    assert(bars == 99);
    dk_sample_peak ramp[512]; unsigned char before[53][107];
    for (unsigned i=0;i<512;i++) { ramp[i].min=-(int16_t)(i*40); ramp[i].max=(int16_t)(i*40); }
    memset(bitmap,0,sizeof(bitmap));
    dk_sample_region_q8(&f.canvas,ramp,512,51200,64,127,256,0,34);
    memcpy(before,bitmap,sizeof(before)); memset(bitmap,0,sizeof(bitmap));
    dk_sample_region_q8(&f.canvas,ramp,512,51200,64,127,320,0,34);
    assert(memcmp(before,bitmap,sizeof(before)) != 0);
    dkm_fini(&api);
    return 0;
}
"""
    )
    binary = tmp_path / "region"
    subprocess.run(
        [
            "cc",
            "-std=c99",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-I",
            str(REPO_ROOT),
            "-I",
            str(REPO_ROOT / "sdk/include"),
            str(source),
            "-o",
            str(binary),
        ],
        check=True,
    )
    subprocess.run([str(binary)], check=True)


@pytest.mark.skipif(shutil.which("cc") is None, reason="no host C compiler")
def test_reusable_page_widgets_geometry_and_clipping(tmp_path):
    source = tmp_path / "widgets.c"
    source.write_text(
        r"""
#include <assert.h>
#include <string.h>
#include <dk/widgets.h>
#include <dk/sample_region.h>
static unsigned char pixels[53][99], before[53][99];
static unsigned calls;
static void rect(int32_t a,int32_t b,int32_t c,int32_t d,int32_t color) {
    assert(a>=0 && b>=0 && c<99 && d<53 && a<=c && b<=d && color==1);
    calls++;
    for(int y=b;y<=d;y++) for(int x=a;x<=c;x++) pixels[y][x]=1;
}
static void clear(void) { memset(pixels,0,sizeof(pixels)); calls=0; }
static char last_text[8];
static void widget_text(int32_t x,int32_t y,const char *s) { assert(x>=0&&y>=0&&x<99&&y<53&&s);strcpy(last_text,s); }
int main(void) {
    dk_page_canvas_v1 canvas={99,53,widget_text,rect};
    dk_page_encoder(&canvas,2,0,0);
    assert(calls<100 && pixels[8][2] && pixels[8][18] && pixels[0][10] && pixels[16][10]);
    assert(pixels[13][5]); /* minimum needle points bottom-left */
    memcpy(before,pixels,sizeof(before));clear();dk_page_encoder(&canvas,2,0,64);
    assert(pixels[1][10] && memcmp(before,pixels,sizeof(before))!=0);
    clear();dk_page_encoder(&canvas,2,0,127);assert(pixels[13][15]);
    memcpy(before,pixels,sizeof(before));clear();dk_page_encoder(&canvas,2,0,999);
    assert(memcmp(before,pixels,sizeof(before))==0);
    clear();dk_page_encoder(&canvas,-8,-8,64);assert(calls<100);
    clear();dk_page_encoder(&canvas,95,49,127);assert(calls<100);
    clear();dk_page_level(&canvas,85,0,0);assert(calls<100 && pixels[15][85]);
    memcpy(before,pixels,sizeof(before));clear();dk_page_level(&canvas,85,0,127);
    assert(pixels[1][85] && memcmp(before,pixels,sizeof(before))!=0);
    clear();dk_page_level(&canvas,-4,-8,64);assert(calls<100);
    clear();dk_page_level(&canvas,95,49,64);assert(calls<100);
    for(unsigned mode=0;mode<4;mode++) {
        clear();dk_sample_play_glyph(&canvas,4,8,mode);
        assert(pixels[5][7]==(mode<2));
        assert(pixels[5][13]==(mode>=2));
        assert(pixels[18][16]==(mode==1 || mode==2));
    }
    clear();dk_page_tune(&canvas,0,0,0);assert(calls<130 && pixels[0][0] && pixels[16][21] && !strcmp(last_text,"0"));
    clear();dk_page_tune(&canvas,0,0,3);assert(calls<130 && pixels[14][3]);
    clear();dk_page_tune(&canvas,0,0,-189);assert(calls<130 && !strcmp(last_text,"-23"));
    clear();dk_page_sample(&canvas,76,0,0,DK_WIDGET_SAMPLE_EMPTY);
    assert(pixels[0][76] && !pixels[0][77] && !pixels[8][87]);
    clear();dk_page_sample(&canvas,76,0,15,DK_WIDGET_SAMPLE_READY);assert(calls<100 && pixels[0][77] && !strcmp(last_text,"15"));
    clear();dk_page_sample(&canvas,76,0,0,DK_WIDGET_SAMPLE_LOADING);assert(pixels[9][82]);
    clear();dk_page_sample(&canvas,76,0,0,DK_WIDGET_SAMPLE_MISSING);assert(pixels[8][86]);
    dk_page_encoder(0,0,0,0);dk_page_level(0,0,0,0);
    return 0;
}
"""
    )
    binary = tmp_path / "widgets"
    subprocess.run(
        [
            "cc",
            "-std=c99",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-I",
            str(REPO_ROOT / "sdk/include"),
            str(source),
            "-o",
            str(binary),
        ],
        check=True,
    )
    subprocess.run([str(binary)], check=True)


@pytest.mark.skipif(shutil.which("cc") is None, reason="no host C compiler")
def test_sample_region_rectangle_clips_zoomed_markers(tmp_path):
    source = tmp_path / "viewport.c"
    source.write_text(
        r"""
#include <assert.h>
#include <dk/sample_region.h>
static unsigned calls, left_mark, right_mark;
static void rect(int32_t a,int32_t b,int32_t c,int32_t d,int32_t color) {
    assert(a>=7 && c<=87 && b>=9 && d<=39 && a<=c && b<=d);
    calls++;
    if(color && a==7 && c==7 && b==9 && d==39) left_mark++;
    if(color && a==87 && c==87 && b==9 && d==39) right_mark++;
}
int main(void) {
    dk_page_canvas_v1 canvas={107,53,0,rect};
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
    return 0;
}
"""
    )
    binary = tmp_path / "viewport"
    subprocess.run(
        [
            "cc",
            "-std=c99",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-I",
            str(REPO_ROOT / "sdk/include"),
            str(source),
            "-o",
            str(binary),
        ],
        check=True,
    )
    subprocess.run([str(binary)], check=True)
