/* Bounded PCM16 overview, built by loader workers, never by a draw callback.
 * Zero-inclusive extrema combine both channels. At most 512 bins / 2048 bytes
 * per backing; bin width is ceil(frames / 512). No PCM pointers escape. */
#ifndef SP_PEAKS_H
#define SP_PEAKS_H
#include <stdint.h>
#define SPP_BINS 512u
typedef struct spp_peak { int16_t min, max; } spp_peak;
static inline uint32_t spp_width(uint32_t frames) { return (frames + SPP_BINS - 1u) / SPP_BINS; }
static inline void spp_clear(spp_peak *p)
{
    uint32_t i;
    for (i = 0; i < SPP_BINS; i++) {
        ((volatile spp_peak *)p)[i].min = 0;
        ((volatile spp_peak *)p)[i].max = 0;
    }
}
static inline void spp_feed(spp_peak *p, uint32_t total, uint32_t first,
                            const uint8_t *pcm, uint32_t frames, uint32_t channels)
{
    uint32_t i, c, width = spp_width(total);
    if (!width) return;
    for (i = 0; i < frames; i++) {
        uint32_t bin = (first + i) / width;
        if (bin >= SPP_BINS) break;
        for (c = 0; c < channels; c++) {
            const uint8_t *at = pcm + (i * channels + c) * 2u;
            int16_t v = (int16_t)(((uint32_t)at[0] << 8) | at[1]);
            if (v < p[bin].min) p[bin].min = v;
            if (v > p[bin].max) p[bin].max = v;
        }
    }
}
static inline int32_t spp_copy(const spp_peak *p, uint32_t frames, uint8_t *out, uint32_t cap)
{
    uint32_t i, n;
    if (!frames) return -48;
    n = (frames + spp_width(frames) - 1u) / spp_width(frames);
    if (!out || cap < n * sizeof(*p)) return -48;
    for (i = 0; i < n; i++) {
        int16_t *dst = (int16_t *)out;
        dst[i * 2] = p[i].min;
        dst[i * 2 + 1] = p[i].max;
    }
    return (int32_t)n;
}
#endif
