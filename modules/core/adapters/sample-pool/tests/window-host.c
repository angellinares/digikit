/* Host test of spp_window_feed (src/cf/pool_peaks.h). A ramp makes each column's min/max a known frame range. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "cf/pool_peaks.h"

#define MAXF 4096u
#define MAXC 64u

static uint8_t pcm[MAXF * 2u * 2u];

/* Frame f has the value f + 1 on channel 0 and -(f + 1) on channel 1 (never 0, so zero-inclusion is visible only as the initial 0). */
static void make(uint32_t frames, uint32_t channels)
{
    uint32_t f, c;
    for (f = 0; f < frames; f++)
        for (c = 0; c < channels; c++) {
            int16_t v = (int16_t)(c ? -(int32_t)(f + 1u) : (int32_t)(f + 1u));
            pcm[(f * channels + c) * 2u] = (uint8_t)((uint16_t)v >> 8);
            pcm[(f * channels + c) * 2u + 1u] = (uint8_t)((uint16_t)v & 0xffu);
        }
}

static int fail(const char *what, uint32_t a, uint32_t b)
{
    fprintf(stderr, "FAIL %s (%u, %u)\n", what, a, b);
    return 1;
}

/* Feed the window in pieces of `piece` frames (0: all at once). pcm holds frames from `window_first`, so frame f is at index f - window_first. */
static void run(int16_t *out, uint32_t columns, uint32_t first, uint32_t frames, uint32_t channels, uint32_t piece)
{
    uint32_t at = first;
    memset(out, 0, columns * 4u);
    while (at < first + frames) {
        uint32_t n = first + frames - at;
        if (piece && n > piece)
            n = piece;
        spp_window_feed(out, columns, first, frames, at, pcm + (at - first) * channels * 2u, n, channels);
        at += n;
        if (piece && piece > 1u)
            piece = piece % 7u + 1u;   /* odd, changing sizes */
    }
}

static int check(uint32_t columns, uint32_t first, uint32_t frames, uint32_t channels, uint32_t piece)
{
    int16_t out[MAXC * 2u], ref[MAXC * 2u];
    uint32_t col, f;
    /* the expected range of column col: frames whose (f * columns / frames) == col, by brute force */
    make(frames, channels);
    run(out, columns, first, frames, channels, piece);
    for (col = 0; col < columns; col++) {
        int32_t lo = 0, hi = 0;
        for (f = 0; f < frames; f++)
            if ((uint64_t)f * columns / frames == col) {
                int32_t a = (int32_t)(f + 1u), b = channels == 2 ? -a : a;
                if (a > hi) hi = a;
                if (b < lo) lo = b;
            }
        ref[col * 2u] = (int16_t)lo;
        ref[col * 2u + 1u] = (int16_t)hi;
        if (out[col * 2u] != ref[col * 2u] || out[col * 2u + 1u] != ref[col * 2u + 1u]) {
            fprintf(stderr, "column %u: got {%d, %d} want {%d, %d}\n", col, out[col * 2u], out[col * 2u + 1u], ref[col * 2u],
                    ref[col * 2u + 1u]);
            return fail("column", columns, frames);
        }
    }
    return 0;
}

int main(void)
{
    static const uint32_t cols[] = {1, 2, 3, 7, 10, 64};
    static const uint32_t frs[] = {64, 100, 1000, 4000, 4096};
    static const uint32_t pieces[] = {0, 1, 3, 100, 513};
    uint32_t i, j, k, ch;
    int16_t o[4];
    for (ch = 1; ch <= 2; ch++)
        for (i = 0; i < sizeof cols / sizeof *cols; i++)
            for (j = 0; j < sizeof frs / sizeof *frs; j++)
                for (k = 0; k < sizeof pieces / sizeof *pieces; k++) {
                    /* a window that does not start at frame 0 gives the same columns: only the frame indices shift */
                    if (check(cols[i], 0, frs[j], ch, pieces[k]) || check(cols[i], 12345u, frs[j], ch, pieces[k]))
                        return 1;
                }
    /* more columns than frames: every frame lands in one column, the others stay empty */
    if (check(64, 0, 10, 1, 0))
        return 1;
    /* frames outside the window are ignored */
    make(8, 1);
    memset(o, 0, sizeof o);
    spp_window_feed(o, 2, 10, 4, 9, pcm, 1, 1);
    spp_window_feed(o, 2, 10, 4, 14, pcm, 1, 1);
    if (o[0] || o[1] || o[2] || o[3])
        return fail("outside", 0, 0);
    puts("ok");
    return 0;
}
