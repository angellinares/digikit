/* Small code-native page widgets; no firmware assets or Bitmap access.
 * Coordinates use the clipped dk_page_canvas_v1 top-left origin. Encoder is
 * 17x17 pixels and level is 9x17. Values clamp to 0..127. Both are display-only:
 * bindings decide input behavior (a level readout does not bind an encoder).
 * Geometry and drawing internals may change while these function contracts stay.
 * Encoder/level draw fewer than 100 bounded calls each; tune fewer than 130
 * and sample fewer than 100. No helper allocates. */
#ifndef DK_WIDGETS_H
#define DK_WIDGETS_H
#include <dk/page.h>
static inline void dk_widget_pixel(const dk_page_canvas_v1 *c, int x, int y)
{
    if (x >= 0 && y >= 0 && (unsigned)x < c->width && (unsigned)y < c->height)
        c->fill_rect(x, y, x, y, 1);
}
static inline void dk_page_encoder(const dk_page_canvas_v1 *c, int x, int y, unsigned value)
{
    if (!c || !c->fill_rect) return;
    if (value > 127) value = 127;
    int cx = x + 8, cy = y + 8, a = 8, b = 0, error = -7;
    do {
        dk_widget_pixel(c,cx+a,cy+b); dk_widget_pixel(c,cx+b,cy+a);
        dk_widget_pixel(c,cx-b,cy+a); dk_widget_pixel(c,cx-a,cy+b);
        dk_widget_pixel(c,cx-a,cy-b); dk_widget_pixel(c,cx-b,cy-a);
        dk_widget_pixel(c,cx+b,cy-a); dk_widget_pixel(c,cx+a,cy-b);
        b++;
        if (error < 0) error += 2*b+1;
        else { a--; error += 2*(b-a)+1; }
    } while (a >= b);
    /* 270-degree sweep: minimum bottom-left, midpoint up, maximum bottom-right. */
    static const int8_t dx[17] = {-5,-6,-7,-7,-7,-6,-5,-3,0,3,5,6,7,7,7,6,5};
    static const int8_t dy[17] = {5,3,0,-2,-4,-5,-6,-7,-7,-7,-6,-5,-4,-2,0,3,5};
    unsigned phase = value * 16, i = phase / 127, fraction = phase % 127;
    int ex = dx[i], ey = dy[i];
    if (i < 16) { ex += (dx[i+1]-ex)*(int)fraction/127; ey += (dy[i+1]-ey)*(int)fraction/127; }
    int steps = ex < 0 ? -ex : ex, ay = ey < 0 ? -ey : ey;
    if (ay > steps) steps = ay;
    for (int n=0;n<=steps;n++) dk_widget_pixel(c,cx+ex*n/steps,cy+ey*n/steps);
}
static inline void dk_page_level(const dk_page_canvas_v1 *c, int x, int y, unsigned value)
{
    if (!c || !c->fill_rect) return;
    if (value > 127) value = 127;
    for (int n=0;n<17;n++) { dk_widget_pixel(c,x+2,y+n); dk_widget_pixel(c,x+6,y+n); }
    for (int n=2;n<=6;n++) { dk_widget_pixel(c,x+n,y); dk_widget_pixel(c,x+n,y+16); }
    int thumb = y+15-(int)((value*14+63)/127);
    for (int n=0;n<9;n++) { dk_widget_pixel(c,x+n,thumb); dk_widget_pixel(c,x+n,thumb-1); }
}
/* 22x17 tune box. eighths is a signed number of 1/8 semitone, clamped to
 * +/-24 semitones. Whole semitones use the native canvas font; nonzero exact
 * fractions (.125..875) use our 3x5 numerals below. No parameter editing/scaling. */
static inline void dk_page_tune(const dk_page_canvas_v1 *c, int x, int y, int eighths)
{
    if (!c || !c->fill_rect || !c->text) return;
    if (eighths < -192) eighths = -192;
    if (eighths > 192) eighths = 192;
    for (int n=0;n<22;n++) { dk_widget_pixel(c,x+n,y); dk_widget_pixel(c,x+n,y+16); }
    for (int n=1;n<16;n++) { dk_widget_pixel(c,x,y+n); dk_widget_pixel(c,x+21,y+n); }
    unsigned value = (unsigned)(eighths < 0 ? -eighths : eighths), whole = value/8, fraction=value%8;
    char text[4]; unsigned at=0;
    if (eighths < 0) text[at++]='-';
    if (whole >= 10) text[at++]=(char)('0'+whole/10);
    text[at++]=(char)('0'+whole%10);text[at]=0;
    c->text(x+(22-(int)at*6)/2,y+(fraction?8:11),text);
    if (fraction) {
        static const uint8_t digits[10][5] = {
            {7,5,5,5,7},{2,6,2,2,7},{7,1,7,4,7},{7,1,7,1,7},{5,5,7,1,1},
            {7,4,7,1,7},{7,4,7,5,7},{7,1,1,1,1},{7,5,7,5,7},{7,5,7,1,7}
        };
        unsigned decimal=fraction*125;
        dk_widget_pixel(c,x+3,y+14);
        for (int col=0;col<3;col++) {
            unsigned divisor=col==0?100:col==1?10:1;
            unsigned digit=decimal/divisor%10;
            for (int row=0;row<5;row++) for (int bit=0;bit<3;bit++)
                if (digits[digit][row]&(4>>bit)) dk_widget_pixel(c,x+6+col*4+bit,y+10+row);
        }
    }
}

enum { DK_WIDGET_SAMPLE_EMPTY, DK_WIDGET_SAMPLE_READY, DK_WIDGET_SAMPLE_LOADING, DK_WIDGET_SAMPLE_MISSING };
/* 22x17 slot box: empty is dotted; READY shows a local slot number 0..99.
 * Loading uses three dots and missing uses a cross. These states are display
 * conventions only, independent of firmware sample banks and parameter codes. */
static inline void dk_page_sample(const dk_page_canvas_v1 *c, int x, int y, unsigned slot, unsigned state)
{
    if (!c || !c->fill_rect || !c->text) return;
    for (int n=0;n<22;n++) if (state!=DK_WIDGET_SAMPLE_EMPTY || !(n&1)) {
        dk_widget_pixel(c,x+n,y);dk_widget_pixel(c,x+n,y+16);
    }
    for (int n=1;n<16;n++) if (state!=DK_WIDGET_SAMPLE_EMPTY || !(n&1)) {
        dk_widget_pixel(c,x,y+n);dk_widget_pixel(c,x+21,y+n);
    }
    if (state==DK_WIDGET_SAMPLE_READY) {
        if (slot>99) slot=99;
        char text[3];unsigned at=0;
        if (slot>=10) text[at++]=(char)('0'+slot/10);
        text[at++]=(char)('0'+slot%10);text[at]=0;
        c->text(x+(22-(int)at*6)/2,y+11,text);
    } else if (state==DK_WIDGET_SAMPLE_LOADING) {
        for (int n=0;n<3;n++) dk_widget_pixel(c,x+6+n*4,y+9);
    } else if (state==DK_WIDGET_SAMPLE_MISSING) {
        for (int n=0;n<9;n++) { dk_widget_pixel(c,x+6+n,y+4+n);dk_widget_pixel(c,x+14-n,y+4+n); }
    }
}
#endif
