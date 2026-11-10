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
DkrtStoreFile sp_files[SPA_BLOCKS];
volatile u32 sp_link_dead;

static u8 win_buf[8 * SPW_SECTOR] __attribute__((aligned(16)));   /* one read of dkpool_window_step */
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
    /* the lease keeps the file as it is until the load ends: an upload cannot replace or delete it, and no file takes its sectors, so
     * the extent in r->file stays the one this file was found at (docs/plans/usb-fast.md 1b). It ends in sp_unlease, whatever the result. */
    rc = dkrt_store_lease(path, &r->file);
    if (rc <= 0)
        return rc == DKRT_E_NOT_FOUND ? DKPOOL_E_NOT_FOUND : rc == DKRT_E_NOMEM ? DKPOOL_E_POOL : DKPOOL_E_READ;
    r->lease = rc;
    if (r->file.size < SPW_HEADER_BYTES || dkrt_store_read(&r->file, 0, 1, hbuf) != 0) {
        sp_unlease(r);
        return DKPOOL_E_READ;
    }
    rc = spw_header(hbuf, r->file.size, &frames, &channels);
    if (rc != 0) {
        sp_unlease(r);
        return rc;
    }
    rc = spa_open(&sp_pool, frames, channels, id->size, hash ? id->hash_kind : 0, hash, &idx);
    if (rc == SPA_E_TOKENS) {
        sp_unlease(r);
        return DKPOOL_E_CONTRACT;
    }
    if (rc < 0) {
        sp_unlease(r);
        return DKPOOL_E_POOL;
    }
    r->state = RQ_QUEUED;
    r->req = req;
    r->blk = idx;
    r->age = ++arrivals;
    r->cancelled = 0;
    r->out = out;
    if (rc == SPA_HIT) {   /* only reached when the identity was hashed after all; handled above, kept for safety */
        r->state = RQ_FREE;
        sp_unlease(r);
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

/* One step of a peak window (dkrt_pool.h), in the worker task: reads the store file of the block, at most DKPOOL_WINDOW_CHUNK bytes of
 * PCM from frame *cursor on. The PCM starts at byte 64 and a frame is 2 or 4 bytes, so a frame never crosses a sector border: each
 * read feeds whole frames and no partial frame is carried. The load's lease ended with the load, so a sample can be replaced while it is loaded: the step
 * leases `path` for the length of its reads and compares the leased file (extent, size, hash) with sp_files. A different file, or none, ends the window
 * with DKPOOL_E_CHANGED and nothing is read at the old extent (it may belong to another file now). */
i32 dkpool_window_step(const dkpool_handle *h, const u8 *path, u32 first, u32 frames, u32 columns, int16_t *out, u32 *cursor)
{
    u32 fb, end, at, total_sectors, i;
    i32 lease, result = 0;
    DkrtStoreFile cur;
    const DkrtStoreFile *f;
    if (dkpool_check(h) != 0)
        return DKPOOL_E_LOST;
    if (!path || !out || !cursor || !columns || !frames || h->id >= SPA_BLOCKS || (h->channels != 1 && h->channels != 2))
        return DKPOOL_E_CONTRACT;
    if (first > h->frames || frames > h->frames - first || *cursor < first || *cursor > first + frames)
        return DKPOOL_E_CONTRACT;
    f = &sp_files[h->id];
    fb = h->channels * 2u;
    at = *cursor;
    end = first + frames;
    if (end - at > DKPOOL_WINDOW_CHUNK / fb)
        end = at + DKPOOL_WINDOW_CHUNK / fb;
    if (f->size < SPW_HEADER_BYTES || (f->size - SPW_HEADER_BYTES) / fb < end)
        return DKPOOL_E_FORMAT;
    lease = dkrt_store_lease(path, &cur);
    if (lease <= 0)
        return lease == DKRT_E_NOT_FOUND ? DKPOOL_E_CHANGED : lease == DKRT_E_NOMEM ? DKPOOL_E_POOL : DKPOOL_E_READ;
    if (cur.first != f->first || cur.size != f->size)
        result = DKPOOL_E_CHANGED;
    for (i = 0; i < 32; i++)
        if (cur.sha[i] != f->sha[i])
            result = DKPOOL_E_CHANGED;
    if (result) {
        dkrt_store_unlease(lease);
        return result;
    }
    total_sectors = (f->size + SPW_SECTOR - 1u) / SPW_SECTOR;
    while (at < end) {
        u32 byte = SPW_HEADER_BYTES + at * fb, sector = byte / SPW_SECTOR, skip = byte % SPW_SECTOR, count = total_sectors - sector, n;
        if (count > 8u)
            count = 8u;
        if (dkrt_store_read(f, sector, count, win_buf) != 0) {
            dkrt_store_unlease(lease);
            return DKPOOL_E_READ;
        }
        n = (count * SPW_SECTOR - skip) / fb;
        if (n > end - at)
            n = end - at;
        spp_window_feed(out, columns, first, frames, at, win_buf + skip, n, h->channels);
        at += n;
    }
    dkrt_store_unlease(lease);
    *cursor = at;
    return at < first + frames ? 1 : 0;
}
