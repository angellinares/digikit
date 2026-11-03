/* core/sample-pool on the Digitone II: the page transport (docs/design/dn2-sample-pool.md section 2).
 *
 * One load runs at a time, as a job on the stock SampleLoader worker task (the task stock Digitakt II sends its pages from). The job reads the
 * store file in 64 KiB ranges, converts one chunk into one page (mono) or two (stereo), and sends each page with our own bounded sender
 * (`sp_raw_send`, send.c). A page counts as written when the receiver's acknowledgement names it (stop and wait, bounded by wall time in
 * DSP blocks and by a count of sleeps, resent up to three times). The acknowledgement rides in a 16-byte extension of the DSP's reply record;
 * the hook `sp_copy_reply` on the stock reply copy decodes it in the interrupt into a snapshot that the job reads (pool_wire.h has the
 * format). The job sleeps 100 us between looks at the snapshot, so the UI task runs meanwhile.
 *
 * Who runs what: the UI-loop task (sp_xfer_tick, every dkpool_* call) starts a job, sets `cancelled`, and delivers the result (dkrt_pool_done)
 * and all allocator calls; the worker task runs the job and touches only its own buffers, the snapshot and the card read; the interrupt only
 * writes the snapshot. `job.state` hands the job over: the UI task sets RUN last, the job sets DONE last. */

#include <stdint.h>
#include "cf/dn2/xfer.h"

typedef uint32_t u32;
typedef int32_t i32;
typedef uint8_t u8;
typedef uint16_t u16;

extern void *sp_stock_copy(void *destination, const void *source, u32 length);
extern void sp_stock_sleep(u32 us);                                                         /* task context; blocks the task */
extern u32 sp_worker_get(void);                                                             /* the SampleLoader worker singleton */
extern u32 sp_worker_queue(u32 worker, const char *name, void *job, void *done);

#define SP_STOCK_REPLY 0xabcu  /* the stock reply copy is this long; the extension follows it (the wire is 0xacc bytes long) */
#define SP_SLEEP_US 100u       /* between two looks at the acknowledgement (stock sleeps 100 after each page) */
#define SP_WAIT_SLEEPS 6000u   /* sleeps in one wait (an armed receiver, one page's acknowledgement with its resends): a hard bound when the block clock stands still */

#define J_IDLE 0u
#define J_RUN 1u    /* posted to the worker, or running there */
#define J_DONE 2u   /* the job ended; sp_xfer_tick delivers `result` */

static u8 rbuf[SPW_RBUF_SECTORS * SPW_SECTOR] __attribute__((aligned(16)));   /* one card read: whole chunks as they are in the file */
static u8 pages[2][SPA_PAGE_BYTES] __attribute__((aligned(16)));              /* the converted pages of one chunk */

static spw_snap snap;           /* the last good acknowledgement; written by sp_copy_reply in the interrupt */

volatile u32 sp_stuck_sends;    /* pages whose send ended on the port-ready bound (a diagnostic; xfer.h) */

static struct {
    volatile u32 state;         /* J_*: the hand-over between the UI task and the job */
    volatile i32 result;        /* valid when state == J_DONE */
    SpReq *r;
    spw_x x;                    /* the page state machine: the job only */
    u32 armed;                  /* the receiver was seen armed and idle (set on READY, kept by an acknowledgement) */
    u32 channels, plane_pages, first_page, frames;
    uintptr_t fn[4];            /* the std::function of the job: storage (2 words), manager, invoker (words on the ColdFire; pointers on the host) */
    u32 done[8];                /* an empty std::function */
} job;

/* ---- The two hooks on the stock link ---------------------------------------------------------------------------------------------------- */

/* Boot-only replacement for the wire length getter: the DSP builds all its SPI2 descriptors with this length too (patched in the same
 * op list, `dsp.link.site.boot_wire_size`). */
u32 sp_wire_size(void) { return SP_STOCK_REPLY + 16u; }

/* The stock reply copy (0xabc bytes) with the extension read after it. Interrupt context; the only writer of `snap`. */
void *sp_copy_reply(void *destination, const void *source, u32 length)
{
    void *result = sp_stock_copy(destination, source, length);
    spw_ack a;
    int ok = 0;
    if (length == SP_STOCK_REPLY && source && destination)
        ok = spw_ack_decode((const volatile u16 *)((const u8 *)source + SP_STOCK_REPLY), &a);
    if (!ok) {
        if (snap.valid)   /* fail closed, once */
            spw_snap_store(&snap, 0, &a);
        return result;
    }
    if (snap.valid && snap.status == a.status && snap.error == a.error && snap.errcount == a.errcount && snap.count == a.count &&
        snap.last == a.last)
        return result;   /* the same observation again keeps the sequence */
    spw_snap_store(&snap, 1, &a);
    return result;
}

/* ---- The job, in the worker task ------------------------------------------------------------------------------------------------------- */

static int cancelled(void) { return job.r->cancelled != 0; }

static i32 map_error(i32 poll)
{
    if (poll == SPW_E_DEAD)
        sp_link_dead = 1;
    return poll == SPW_E_TAG ? DKPOOL_E_FORMAT : DKPOOL_E_READ;
}

/* One look at the link. */
static i32 look(void)
{
    spw_ack a;
    int have = spw_snap_read(&snap, &a);
    return spw_poll(&job.x, dkrt_blocks(), have ? &a : 0);
}

/* Send one page and wait for its acknowledgement; 0, a DKPOOL_E_* code, or DKPOOL_E_LOST when the load was cancelled before the page went
 * out. A page in flight is always settled first (acknowledged or failed), so the receiver's counters are known when the next load starts. */
static i32 send_page(const u8 *page, u32 index)
{
    u32 n = 0, tag;
    i32 p;
    while (!job.armed) {
        if (cancelled())
            return DKPOOL_E_LOST;
        p = look();
        if (p == SPW_READY)
            job.armed = 1;
        else if (p < 0)
            return map_error(p);
        else if (++n > SP_WAIT_SLEEPS)
            return DKPOOL_E_READ;
        else
            sp_stock_sleep(SP_SLEEP_US);
    }
    if (cancelled())
        return DKPOOL_E_LOST;
    tag = spw_next_tag(&job.x, index);
    if (sp_raw_send(tag, page))
        sp_stuck_sends++;   /* the page is lost: the acknowledgement times out and the page is sent again */
    spw_sent(&job.x, tag, dkrt_blocks());
    for (n = 0; n < SP_WAIT_SLEEPS; n++) {
        sp_stock_sleep(SP_SLEEP_US);
        p = look();
        if (p == SPW_ACKED)
            return 0;
        if (p == SPW_RESEND) {
            if (sp_raw_send(job.x.tag, page))
                sp_stuck_sends++;
            spw_resent(&job.x, dkrt_blocks());
        } else if (p < 0) {
            return map_error(p);
        }
    }
    return DKPOOL_E_READ;
}

/* The whole load. */
static i32 run_job(void)
{
    u32 c = 0, j, k, ch = job.channels, chunk_bytes = SPA_PAGE_BYTES * ch;
    spw_begin(&job.x, dkrt_blocks());
    job.armed = 0;
    while (c < job.plane_pages) {
        spw_span s;
        if (cancelled())
            return DKPOOL_E_LOST;
        spw_span_of(ch, job.frames, c, &s);
        if (dkrt_store_read(&job.r->file, s.sector, s.sectors, rbuf) != 0)
            return DKPOOL_E_READ;
        for (j = 0; j < s.chunks; j++, c++) {
            spw_chunk ck;
            const u8 *pcm = rbuf + s.skip + j * chunk_bytes;
            i32 rc;
            spw_chunk_of(ch, job.frames, c, &ck);
            if (ch == 1)
                spw_page_mono(pcm, ck.frames, pages[0]);
            else
                spw_page_stereo(pcm, ck.frames, pages[0], pages[1]);
            for (k = 0; k < ch; k++) {
                rc = send_page(pages[k], job.first_page + k * job.plane_pages + c);
                if (rc != 0)
                    return rc;
            }
        }
    }
    return 0;
}

/* std::function plumbing for the worker (the same layout the DT2 adapter queues): the invoker gets the function's storage, whose first word
 * is unused here: there is one job. */
static void invoker(const void *storage)
{
    (void)storage;
    job.result = run_job();
    __asm__ volatile("" ::: "memory");
    job.state = J_DONE;
}

static int manager(u32 *dest, const u32 *src, int op)
{
    if (op == 1 || op == 2)
        dest[0] = src[0];
    return 0;
}

/* ---- The UI task ------------------------------------------------------------------------------------------------------------------------ */

static void finish(i32 result)
{
    SpReq *r = job.r;
    u32 req = r->req;
    if (result == 0) {
        spa_ready(&sp_pool, r->blk);
        spa_fill(&sp_pool, r->blk, r->out);
    } else {
        spa_abort(&sp_pool, r->blk);
        if (r->cancelled)
            result = DKPOOL_E_LOST;
    }
    r->state = RQ_FREE;
    job.r = 0;
    job.state = J_IDLE;
    dkrt_pool_done(req, result);
}

/* Post the load of `r` to the worker. */
static void start_job(SpReq *r)
{
    const spa_block *b = &sp_pool.blk[r->blk];
    u32 worker, i;
    job.r = r;
    job.channels = b->channels;
    job.plane_pages = b->plane_pages;
    job.first_page = b->first_page;
    job.frames = b->frames;
    job.fn[0] = 0;
    job.fn[1] = 0;
    job.fn[2] = (uintptr_t)manager;
    job.fn[3] = (uintptr_t)invoker;
    for (i = 0; i < 8; i++)
        ((volatile u32 *)job.done)[i] = 0;   /* volatile: the compiler would turn a plain loop into a memset call */
    r->state = RQ_ACTIVE;
    worker = sp_worker_get();
    if (!worker) {   /* no worker: nothing was posted */
        job.state = J_RUN;
        job.result = DKPOOL_E_READ;
        job.state = J_DONE;
        return;
    }
    job.state = J_RUN;   /* last: the worker may run the job before the queue call returns */
    sp_worker_queue(worker, "SamplePool", job.fn, job.done);
}

/* The oldest queued request, or 0. A queued request nobody waits for is finished here without touching the link. */
static SpReq *next_queued(void)
{
    SpReq *best = 0;
    u32 i;
    for (i = 0; i < SP_REQS; i++) {
        SpReq *r = &sp_reqs[i];
        if (r->state != RQ_QUEUED)
            continue;
        if (r->cancelled) {
            spa_abort(&sp_pool, r->blk);
            r->state = RQ_FREE;
            dkrt_pool_done(r->req, DKPOOL_E_LOST);
            continue;
        }
        if (!best || (int32_t)(r->age - best->age) < 0)
            best = r;
    }
    return best;
}

void sp_xfer_tick(void)
{
    if (job.state == J_DONE)
        finish(job.result);
    if (job.state == J_IDLE) {
        SpReq *r = next_queued();
        if (r)
            start_job(r);
    }
}

/* dkpool_cancel found the request: nothing to do here beyond the flag, which the job reads between pages. */
void sp_xfer_cancel(SpReq *r) { r->cancelled = 1; }
