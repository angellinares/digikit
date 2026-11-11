/* Checks the page widgets (encoder, level, play glyph, tune, sample box): geometry, value changes and clipping to the canvas. */
#include <assert.h>
#include <string.h>
#include <dk/page/widgets.h>
#include <dk/sample/view.h>
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
        clear();dk_sample_view_play_glyph(&canvas,4,8,mode);
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
