/* core/sample-pool on the Digitone II: the sample backing (dkrt_pool.h) owns a range of the DSP's DDR (docs/design/dn2-sample-pool.md).
 *
 * The range is [sp_dev_ddr_base, + sp_dev_ddr_bytes) of the device file. pool_alloc.h hands out 4 KiB pages first fit, shares a block of an
 * equal file identity (size and sha256 of the store entry), and keeps released blocks as a cache until an allocation needs the room.
 * A load reads the 64-byte header of the store file, reserves the block and queues the transfer; dkpool_tick starts it, one load at a time,
 * as a job on the stock SampleLoader worker task, which sends the pages over LP0 (xfer.c, send.c). Nothing else resets this range, so
 * dkrt_pool_lost is never called.
 *
 * Tasks: every dkpool_* call and xfer.c's tick run in the runtime's UI-loop task, and so do the allocator and dkrt_pool_done. The job runs in
 * the worker task. Only sp_copy_reply (xfer.c) runs in an interrupt, and it only writes the acknowledgement snapshot. */

#include <stdint.h>
#include "cf/dn2/xfer.h"

typedef uint32_t u32;
typedef int32_t i32;
typedef uint8_t u8;

extern const u32 sp_dev_ddr_base;    /* device.s, from [adapter.sample_pool] ddr_base */
extern const u32 sp_dev_ddr_bytes;   /* ... and ddr_bytes */

spa_pool sp_pool;
spp_peak sp_peaks[SPA_BLOCKS][SPP_BINS];
SpReq sp_reqs[SP_REQS];
volatile u32 sp_link_dead;

static u8 hbuf[SPW_SECTOR] __attribute__((aligned(16)));   /* sector 0 of the file being loaded (xfer.c has its own buffers: a transfer may run) */
static u32 ready;       /* spa_init succeeded */
static u32 arrivals;    /* orders the queue; wrapping is harmless (compared as a difference) */

u32 dkpool_version(void) { return DKPOOL_VERSION; }

static int pool_ready(void)
{
    if (!ready && spa_init(&sp_pool, sp_dev_ddr_base, sp_dev_ddr_bytes) == SPA_OK)
        ready = 1;
    return ready;
}

static SpReq *take_req(void)
{
    u32 i;
    for (i = 0; i < SP_REQS; i++)
        if (sp_reqs[i].state == RQ_FREE)
            return &sp_reqs[i];
    return 0;
}

i32 dkpool_load(u32 req, const dk_file_id *id, const u8 *path, dkpool_handle *out)
{
    SpReq *r;
    u32 idx, frames, channels;
    i32 rc;
    const u8 *hash = 0;
    if (!path || !out || !id || !pool_ready())
        return DKPOOL_E_CONTRACT;
    if ((id->flags & DKPOOL_ID_HASH_VALID) && id->hash_kind != 0)
        hash = id->hash;
    /* an equal sample that is READY or CACHED needs no file read and no transfer */
    if (hash && spa_hit(&sp_pool, id->size, id->hash_kind, hash, &idx)) {
        spa_fill(&sp_pool, idx, out);
        return DKPOOL_DONE;
    }
    r = take_req();
    if (!r)
        return DKPOOL_E_CONTRACT;
    if (sp_link_dead)
        return DKPOOL_E_READ;
    rc = dkrt_store_stat(path, &r->file);
    if (rc != 0)
        return rc == DKRT_E_NOT_FOUND ? DKPOOL_E_NOT_FOUND : DKPOOL_E_READ;
    if (r->file.size < SPW_HEADER_BYTES ||dkrt_store_read(&r->file, 0, 1, hbuf) != 0)
        return DKPOOL_E_READ;
    rc = spw_header(hbuf, r->file.size, &frames, &channels);
    if (rc != 0)
        return rc;
    rc = spa_open(&sp_pool, frames, channels, id->size, hash ? id->hash_kind : 0, hash, &idx);
    if (rc == SPA_E_TOKENS)
        return DKPOOL_E_CONTRACT;
    if (rc < 0)
        return DKPOOL_E_POOL;
    r->state = RQ_QUEUED;
    r->req = req;
    r->blk = idx;
    r->age = ++arrivals;
    r->cancelled = 0;
    r->out = out;
    if (rc == SPA_HIT) {   /* only reached when the identity was hashed after all; handled above, kept for safety */
        r->state = RQ_FREE;
        spa_fill(&sp_pool, idx, out);
        return DKPOOL_DONE;
    }
    return DKPOOL_QUEUED;
}

/* Optional symbol (dkrt_pool.h): the runtime no longer waits for load `req`. A load that has not started is dropped at the next tick; one
 * that runs sends no further page once its page in flight has been settled. dkrt_pool_done follows once, with DKPOOL_E_LOST. */
void dkpool_cancel(u32 req)
{
    u32 i;
    for (i = 0; i < SP_REQS; i++)
        if (sp_reqs[i].state != RQ_FREE && sp_reqs[i].req == req)
            sp_xfer_cancel(&sp_reqs[i]);
}

void dkpool_release(const dkpool_handle *h)
{
    if (h && pool_ready())
        spa_release(&sp_pool, h->id, h->token);
}

i32 dkpool_check(const dkpool_handle *h)
{
    /* Content that is READY stays valid even when the link died later: only loads need it. */
    if (!h || !pool_ready() || !spa_current(&sp_pool, h->id, h->token))
        return DKPOOL_E_LOST;
    return 0;
}

u32 dkpool_free_frames(void)
{
    return pool_ready() ? spa_free_frames(&sp_pool) : 0;
}

void dkpool_tick(void)
{
    if (ready)
        sp_xfer_tick();
}

/* READY backing only; the runtime separately validates the public pool token. */
i32 dkpool_peaks(const dkpool_handle *h, u8 *out, u32 cap)
{
    if (dkpool_check(h) != 0) return DKPOOL_E_LOST;
    return spp_copy(sp_peaks[h->id], h->frames, out, cap);
}
