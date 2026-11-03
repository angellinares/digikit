/* The part of the DT2 sample pool adapter that does not touch the firmware: slot choice, PCM geometry and the free-space figure, on plain
 * data the caller passes in (the stock slot model; the device-neutral address helpers are in pool_nw32.h). tests/patchkit/test_sample_pool_core.py compiles it on the host. Finding 109 and 81 are the sources. */
#ifndef SP_CORE_H
#define SP_CORE_H
#include <stdint.h>
#include "dkrt_pool.h"
#include "cf/pool_nw32.h"

#define SP_SLOTS 0x400u           /* stock sample slots 0 .. 0x3ff */
#define SP_KEY_WORDS 4u           /* key: 16 bytes per slot */
#define SP_KEY_FREE 0xffffffffu   /* word 0 of the key of a released or reset slot */
#define SP_REC_BYTES 0x34u        /* record stride of the stock record vector */
#define SP_REC_BLOCK 0x10u        /* record +0x10: pool block offset */
#define SP_REC_EXTENT 0x14u       /* record +0x14: rounded extent A */
#define SP_POOL_LIMIT 0x19000400u /* the loader's append limit on the block offset space (finding 109 section 2) */

/* Fill the handle's planes and frame count for a loaded block (finding 81 "Stereo storage"):
 * block = record +0x10, extent = record +0x14 (A, the loader's own rounded size), pcm = P from the file header (bytes 4..8; the frames
 * come from it, the 16-byte trailer is optional), file_bytes = key word 2 (must hold 64 + P), pool_base = the DSP view of the pool origin.
 * Mono: PCM at block + 64, both planes the same. Stereo: left at block + 32, right at block + A/2 + 32. Frames = P / (2 * channels).
 * Also sets rate (SP_NATIVE_RATE) and channels. Returns 0, or DKPOOL_E_FORMAT when the numbers do not describe a PCM16 block that lies inside its extent. */
static inline int32_t sp_geometry(uint32_t pool_base, uint32_t block, uint32_t extent, uint32_t stereo, uint32_t pcm, uint32_t file_bytes,
                                  dkpool_handle *out)
{
    uint32_t chan = stereo ? 2u : 1u, plane, frame_bytes = 2u * chan, l, r;
    if (pcm == 0 || pcm % frame_bytes || file_bytes < 64u || pcm > file_bytes - 64u)
        return DKPOOL_E_FORMAT;
    plane = pcm / chan;
    /* the left plane ends before the right one starts, and the right one ends inside the extent */
    if (extent == 0 || (stereo ? (plane > extent / 2u || extent / 2u + 32u + plane > extent) : (64u + plane > extent)))
        return DKPOOL_E_FORMAT;
    l = pool_base + block + (stereo ? 32u : 64u);
    r = stereo ? pool_base + block + extent / 2u + 32u : l;
    out->base_l = sp_nw32(l);
    out->base_r = sp_nw32(r);
    out->frames = pcm / frame_bytes;
    out->rate = SP_NATIVE_RATE;
    out->channels = chan;
    return (out->base_l && out->base_r && out->frames) ? 0 : DKPOOL_E_FORMAT;
}

/* The free slot to take: from 0x3ff down to the floor, skipping slots with (slot & 0x7f) == 0 (the loader refuses them), slots this
 * adapter holds (busy[slot] != 0) and slots whose key word 0 is not free (stock uses them). Returns 0 when there is none. */
static inline uint32_t sp_pick_slot(uint32_t floor, const uint32_t *keys, const uint8_t *busy)
{
    uint32_t s;
    if (floor == 0 || floor >= SP_SLOTS)
        return 0;
    for (s = SP_SLOTS - 1u; s >= floor; --s) {
        if ((s & 0x7fu) == 0)
            continue;
        if (!busy[s] && keys[s * SP_KEY_WORDS] == SP_KEY_FREE)
            return s;
    }
    return 0;
}

/* Is a loaded slot gone? A reset or release writes -1 into key word 0. The slot map reads 0 after a reset and must not be used. */
static inline int sp_slot_lost(const uint32_t *keys, uint32_t slot)
{
    return keys[slot * SP_KEY_WORDS] == SP_KEY_FREE;
}

/* Free space in mono PCM16 frames: what is left below the loader's limit after the highest record end. Each record is the block offset
 * (+0x10) plus the extent (+0x14). A little headroom (one extent granule of 8 KiB) stays out; it is not proven that the loader needs
 * none. [open] the exact limit (finding 109 section 2 reads it as 0x19000400 on the offset). */
static inline uint32_t sp_free_frames(const uint32_t *begin, const uint32_t *end)
{
    uint32_t top = 0, headroom = 8192u;
    const uint8_t *p = (const uint8_t *)begin;
    while (p + SP_REC_BYTES <= (const uint8_t *)end) {
        const uint32_t *w = (const uint32_t *)p;
        uint32_t e = w[SP_REC_BLOCK / 4] + w[SP_REC_EXTENT / 4];
        if (e > top)
            top = e;
        p += SP_REC_BYTES;
    }
    if (top + headroom >= SP_POOL_LIMIT)
        return 0;
    return (SP_POOL_LIMIT - top - headroom) / 2u;
}
/* What dkpool_tick reports for a finished load job (`cancelled`: dkpool_cancel flagged it). A job that was cancelled before it started
 * has result DKPOOL_E_LOST already (nothing was loaded). A cancelled job that loaded anyway (it was running) reports DKPOOL_E_LOST too and
 * sets *release: its slot goes back to the stock pool, as no handle will ever name it. Without cancel the result is reported as it is. */
static inline int32_t sp_finish(int32_t result, uint32_t cancelled, uint32_t *release)
{
    *release = 0;
    if (!cancelled)
        return result;
    if (result == 0) {
        *release = 1;
        return DKPOOL_E_LOST;
    }
    return result;
}

#endif
