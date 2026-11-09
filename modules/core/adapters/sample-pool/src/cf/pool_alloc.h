/* The DN2 sample pool's allocator: 4 KiB pages of the DSP's DDR range, a sorted free-extent list, a table of blocks with the file identity,
 * a reference count and a cache of released blocks. No firmware access and no libc: tests/patchkit/test_sample_pool_dn2.py compiles it
 * on the host. docs/design/dn2-sample-pool.md section 1 is the specification.
 *
 * A block holds one sample: `channels` planes of `plane_pages` pages each, the right plane right after the left one. A mono block has
 * base_r == base_l. States: FREE (table entry unused), LOADING (pages reserved, being written), READY (refs > 0), CACHED (refs == 0, the
 * content is valid and stays until an allocation needs the room). PCM never moves. */
#ifndef SP_ALLOC_H
#define SP_ALLOC_H
#include <stdint.h>
#include "dkrt_pool.h"
#include "cf/pool_nw32.h"

#if defined(__GNUC__) && !defined(__clang__)
#pragma GCC optimize("no-tree-loop-distribute-patterns") /* a patched-in object has no libc: no memset or memcpy calls from loops */
#endif

#define SPA_PAGE_BYTES 4096u
#define SPA_PAGE_FRAMES 2048u                     /* mono PCM16 frames of one page */
#define SPA_MAX_PAGES (1u << 20)                  /* the page index of the wire tag has 20 bits */
#define SPA_BLOCKS 64u
#define SPA_EXTENTS (SPA_BLOCKS + 2u)             /* at most 64 blocks leave 65 holes; one spare */
#define SPA_STOCK_END 0x80531000u                 /* the end of the last stock write seen (finding 106); a range starts at or above it */

#define SPA_FREE 0u
#define SPA_LOADING 1u
#define SPA_READY 2u
#define SPA_CACHED 3u

/* Results of spa_init. */
#define SPA_OK 0
#define SPA_E_RANGE (-1)

/* Results of spa_open. */
#define SPA_NEW 0                                 /* a block was reserved: LOADING, write the pages */
#define SPA_HIT 1                                 /* an equal block is READY now: no transfer */
#define SPA_E_NOSPACE (-2)                        /* no contiguous run of pages, no table entry (after evicting all cached blocks) */
#define SPA_E_TOKENS (-3)                         /* the token counter is exhausted: it never wraps */

typedef struct spa_extent {
    uint32_t start; /* first page */
    uint32_t count; /* pages */
} spa_extent;

typedef struct spa_block {
    uint32_t state;
    uint32_t first_page;
    uint32_t plane_pages;
    uint32_t channels;
    uint32_t frames;
    uint32_t refs;
    uint32_t token;
    uint32_t lru;          /* CACHED: the clock value when it was released; the smallest is evicted first */
    uint32_t has_id;       /* the identity below is valid: the block may be shared */
    uint32_t size;
    uint32_t hash_kind;
    uint8_t hash[32];
} spa_block;

typedef struct spa_pool {
    uint32_t base;         /* physical byte address of page 0 */
    uint32_t pages;        /* pages in the range */
    uint32_t n_ext;
    uint32_t clock;        /* LRU clock */
    uint32_t tokens;       /* tokens issued so far */
    spa_extent ext[SPA_EXTENTS];
    spa_block blk[SPA_BLOCKS];
} spa_pool;

/* Pages of one plane of `frames` mono-or-stereo frames: PCM16 plane bytes are frames * 2, rounded up to whole pages. */
static inline uint32_t spa_plane_pages(uint32_t frames)
{
    return (frames + (SPA_PAGE_FRAMES - 1u)) / SPA_PAGE_FRAMES;
}

static inline void spa_zero(void *p, uint32_t n)
{
    volatile uint8_t *b = (volatile uint8_t *)p;
    while (n--)
        *b++ = 0;
}

/* Set up the allocator for the range [base, base + bytes). RANGE when the range is not 4 KiB aligned, starts below the stock writes, ends
 * above the DSP's NW32 alias (0xa0000000), or has more pages than the wire tag can address. */
static inline int32_t spa_init(spa_pool *p, uint32_t base, uint32_t bytes)
{
    uint32_t end = base + bytes;
    spa_zero(p, (uint32_t)sizeof *p);
    if (bytes == 0 || (base & 0xfffu) || (bytes & 0xfffu) || base < SPA_STOCK_END || end < base || end > SP_DDR_PHYS_END ||
        bytes / SPA_PAGE_BYTES > SPA_MAX_PAGES)
        return SPA_E_RANGE;
    p->base = base;
    p->pages = bytes / SPA_PAGE_BYTES;
    p->n_ext = 1;
    p->ext[0].start = 0;
    p->ext[0].count = p->pages;
    return SPA_OK;
}

/* Put `count` pages from `start` back into the free list, merged with the neighbours. */
static inline void spa_free_pages(spa_pool *p, uint32_t start, uint32_t count)
{
    uint32_t i = 0, k;
    if (!count)
        return;
    while (i < p->n_ext && p->ext[i].start < start)
        i++;
    if (i > 0 && p->ext[i - 1].start + p->ext[i - 1].count == start) {   /* joins the extent before */
        p->ext[i - 1].count += count;
        if (i < p->n_ext && p->ext[i - 1].start + p->ext[i - 1].count == p->ext[i].start) {
            p->ext[i - 1].count += p->ext[i].count;
            for (k = i; k + 1 < p->n_ext; k++)
                p->ext[k] = p->ext[k + 1];
            p->n_ext--;
        }
        return;
    }
    if (i < p->n_ext && start + count == p->ext[i].start) {   /* joins the extent after */
        p->ext[i].start = start;
        p->ext[i].count += count;
        return;
    }
    for (k = p->n_ext; k > i; k--)
        p->ext[k] = p->ext[k - 1];
    p->ext[i].start = start;
    p->ext[i].count = count;
    p->n_ext++;
}

/* First fit: the first page of a run of `count` free pages, or -1. */
static inline int32_t spa_take_pages(spa_pool *p, uint32_t count)
{
    uint32_t i, k, start;
    for (i = 0; i < p->n_ext; i++) {
        if (p->ext[i].count < count)
            continue;
        start = p->ext[i].start;
        p->ext[i].start += count;
        p->ext[i].count -= count;
        if (p->ext[i].count == 0) {
            for (k = i; k + 1 < p->n_ext; k++)
                p->ext[k] = p->ext[k + 1];
            p->n_ext--;
        }
        return (int32_t)start;
    }
    return -1;
}

/* Drop a block: its pages go back to the free list and the entry is free. */
static inline void spa_drop(spa_pool *p, uint32_t idx)
{
    spa_block *b = &p->blk[idx];
    spa_free_pages(p, b->first_page, b->channels * b->plane_pages);
    spa_zero(b, (uint32_t)sizeof *b);
}

/* Evict the oldest CACHED block; 1 when one was dropped, 0 when none is cached. */
static inline int spa_evict_oldest(spa_pool *p)
{
    uint32_t i, best = SPA_BLOCKS;
    for (i = 0; i < SPA_BLOCKS; i++)
        if (p->blk[i].state == SPA_CACHED && (best == SPA_BLOCKS || p->blk[i].lru < p->blk[best].lru))
            best = i;
    if (best == SPA_BLOCKS)
        return 0;
    spa_drop(p, best);
    return 1;
}

static inline int spa_same_id(const spa_block *b, uint32_t size, uint32_t hash_kind, const uint8_t *hash)
{
    uint32_t i;
    if (!b->has_id || b->size != size || b->hash_kind != hash_kind)
        return 0;
    for (i = 0; i < 32; i++)
        if (b->hash[i] != hash[i])
            return 0;
    return 1;
}

static inline int32_t spa_find_free_entry(const spa_pool *p)
{
    uint32_t i;
    for (i = 0; i < SPA_BLOCKS; i++)
        if (p->blk[i].state == SPA_FREE)
            return (int32_t)i;
    return -1;
}

/* An equal sample that is READY or CACHED, if any: the identity is (size, hash_kind, 32 hash bytes).
 * READY: the block gains a reference and keeps its token (a second holder of the same backing; the first handle stays valid).
 * CACHED: it becomes READY with refs 1 and a new token (a new life: handles of the earlier life stay stale).
 * Returns 1 and the table index in *out, or 0 (also when the token counter is exhausted, which spa_open then reports). */
static inline int spa_hit(spa_pool *p, uint32_t size, uint32_t hash_kind, const uint8_t *hash, uint32_t *out)
{
    uint32_t i;
    spa_block *b;
    for (i = 0; i < SPA_BLOCKS; i++) {
        b = &p->blk[i];
        if ((b->state != SPA_READY && b->state != SPA_CACHED) || !spa_same_id(b, size, hash_kind, hash))
            continue;
        if (b->state == SPA_READY) {
            b->refs++;
        } else {
            if (p->tokens == 0xffffffffu)
                return 0;
            b->state = SPA_READY;
            b->refs = 1;
            b->token = ++p->tokens;
        }
        *out = i;
        return 1;
    }
    return 0;
}

/* Take a block for a sample of `frames` frames and `channels` planes.
 *  - With an identity an equal block is shared (spa_hit): HIT, nothing is transferred.
 *  - Otherwise the pages are reserved first fit; when no run fits, CACHED blocks are evicted oldest first, one at a time, and the search
 *    repeats. A block table entry is found the same way. The block is LOADING with refs 1 and a new token: NEW.
 * *out is the table index. A failure changes nothing except the cached blocks that were evicted. */
static inline int32_t spa_open(spa_pool *p, uint32_t frames, uint32_t channels, uint32_t size, uint32_t hash_kind, const uint8_t *hash,
                               uint32_t *out)
{
    uint32_t i, plane = spa_plane_pages(frames), need;
    int32_t page, ent;
    spa_block *b;
    if (channels < 1 || channels > 2 || !frames || plane > p->pages / channels)
        return SPA_E_NOSPACE;
    need = plane * channels;
    if (hash && spa_hit(p, size, hash_kind, hash, out))
        return SPA_HIT;
    if (p->tokens == 0xffffffffu)
        return SPA_E_TOKENS;
    for (;;) {
        page = spa_take_pages(p, need);
        if (page >= 0)
            break;
        if (!spa_evict_oldest(p))
            return SPA_E_NOSPACE;
    }
    for (;;) {
        ent = spa_find_free_entry(p);
        if (ent >= 0)
            break;
        if (!spa_evict_oldest(p)) {
            spa_free_pages(p, (uint32_t)page, need);
            return SPA_E_NOSPACE;
        }
    }
    b = &p->blk[ent];
    spa_zero(b, (uint32_t)sizeof *b);
    b->state = SPA_LOADING;
    b->first_page = (uint32_t)page;
    b->plane_pages = plane;
    b->channels = channels;
    b->frames = frames;
    b->refs = 1;
    b->token = ++p->tokens;
    if (hash) {
        b->has_id = 1;
        b->size = size;
        b->hash_kind = hash_kind;
        for (i = 0; i < 32; i++)
            b->hash[i] = hash[i];
    }
    *out = (uint32_t)ent;
    return SPA_NEW;
}

/* The pages are written: LOADING becomes READY. */
static inline void spa_ready(spa_pool *p, uint32_t idx)
{
    if (idx < SPA_BLOCKS && p->blk[idx].state == SPA_LOADING)
        p->blk[idx].state = SPA_READY;
}

/* The load failed or was cancelled: the block goes, with its pages (nothing reads an unpublished block). */
static inline void spa_abort(spa_pool *p, uint32_t idx)
{
    if (idx < SPA_BLOCKS && p->blk[idx].state == SPA_LOADING)
        spa_drop(p, idx);
}

/* Is (idx, token) a READY block's current handle? */
static inline int spa_current(const spa_pool *p, uint32_t idx, uint32_t token)
{
    return idx < SPA_BLOCKS && p->blk[idx].state == SPA_READY && p->blk[idx].token == token;
}

/* Give a reference back. At 0 the block stays as CACHED. A stale handle is ignored. */
static inline void spa_release(spa_pool *p, uint32_t idx, uint32_t token)
{
    spa_block *b;
    if (!spa_current(p, idx, token))
        return;
    b = &p->blk[idx];
    if (b->refs > 1) {
        b->refs--;
        return;
    }
    b->refs = 0;
    b->state = SPA_CACHED;
    b->lru = ++p->clock;
}

/* Mono frames of the largest contiguous run of free and CACHED pages, as dkpool_free_frames reports it (a stereo sample needs one run
 * for both planes and PCM never moves, so total free space would promise too much). */
static inline uint32_t spa_free_frames(const spa_pool *p)
{
    uint32_t start[SPA_EXTENTS + SPA_BLOCKS], count[SPA_EXTENTS + SPA_BLOCKS], n = 0, i, j, best = 0, run_start, run_end;
    for (i = 0; i < p->n_ext; i++) {
        start[n] = p->ext[i].start;
        count[n++] = p->ext[i].count;
    }
    for (i = 0; i < SPA_BLOCKS; i++)
        if (p->blk[i].state == SPA_CACHED) {
            start[n] = p->blk[i].first_page;
            count[n++] = p->blk[i].channels * p->blk[i].plane_pages;
        }
    for (i = 1; i < n; i++) {   /* insertion sort by start */
        uint32_t s = start[i], c = count[i];
        for (j = i; j > 0 && start[j - 1] > s; j--) {
            start[j] = start[j - 1];
            count[j] = count[j - 1];
        }
        start[j] = s;
        count[j] = c;
    }
    run_start = run_end = 0;
    for (i = 0; i < n; i++) {
        if (i == 0 || start[i] != run_end) {
            run_start = start[i];
            run_end = start[i];
        }
        run_end = start[i] + count[i];
        if (run_end - run_start > best)
            best = run_end - run_start;
    }
    return best * SPA_PAGE_FRAMES;
}

/* The handle words of a block (all seven; id is the table index). */
static inline void spa_fill(const spa_pool *p, uint32_t idx, dkpool_handle *h)
{
    const spa_block *b = &p->blk[idx];
    uint32_t l = p->base + b->first_page * SPA_PAGE_BYTES;
    uint32_t r = b->channels == 2 ? l + b->plane_pages * SPA_PAGE_BYTES : l;
    h->id = idx;
    h->token = b->token;
    h->base_l = sp_nw32(l);
    h->base_r = sp_nw32(r);
    h->frames = b->frames;
    h->rate = SP_NATIVE_RATE;
    h->channels = b->channels;
}

#endif
