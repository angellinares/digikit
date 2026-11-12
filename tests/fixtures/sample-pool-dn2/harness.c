/* Host harness for modules/core/adapters/sample-pool/src/cf/dn2 (pool.c, xfer.c and send.c, unchanged): the store, the DSP block clock, the
 * LP0 port, the SampleLoader worker and the runtime's completion call are stand-ins, and a small model of the DSP receiver (spw_dsp_rule)
 * writes the pages it decodes from the port into a DDR array and answers through the real reply hook. modules/core/adapters/sample-pool/tests/test_sample_pool_dn2.py
 * runs the scenarios. It tests our logic: the allocator, the page order, the job loop with its resend, error and cancel rules, the raw
 * sender's byte order and bounds, the hand-over between the UI task and the job, and the data that reaches DDR; not the firmware or the DSP
 * code. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "cf/dn2/xfer.h"

#define DDR_BASE 0x80531000u
#define DDR_BYTES 0x100000u /* 256 pages: small enough to fill */
const uint32_t sp_dev_ddr_base = DDR_BASE;
const uint32_t sp_dev_ddr_bytes = DDR_BYTES;

static uint8_t ddr[DDR_BYTES];

/* ---- time: one event loop for the block interrupt, the UI task and (inside the job's sleeps) the worker task ----
 * Time is simulated in microseconds. Every FRAME_US the DSP model delivers a reply (the block interrupt); every ui_period frames the UI task
 * runs dkpool_tick. The worker runs a posted job right after a tick; the job's sleeps, sends and card reads advance time through advance(),
 * which keeps the interrupt and the UI task running meanwhile (a nested call is the preemption of the worker). */
#define FRAME_US 667u
static uint32_t t_us, next_frame_us = FRAME_US, next_ui_us, frames_n, ui_period = 50,
                      ack_latency = 1, send_us = 300, read_us_base = 300, read_us_sector = 20;
static uint32_t clock_now = 1000, job_t0, job_t1;
uint32_t dkrt_blocks(void)
{
    return clock_now;
}
static int done_n, in_tick, done_outside_tick;
static uint32_t done_req[64];
static int32_t done_res[64];
static uint32_t done_clock;
void dkrt_pool_done(uint32_t req, int32_t result)
{
    if (!in_tick)
        done_outside_tick++;
    done_clock = clock_now;
    done_req[done_n] = req;
    done_res[done_n++] = result;
}
void dkrt_pool_lost(void)
{
    puts("LOST");
    exit(4);
}

/* ---- the worker: one posted job, run after the tick that posted it ---- */
/* The runtime's job queue (dkrt_job_post): step runs after the tick that posted it, done runs in the UI task at the next pass. */
static int32_t (*job_step)(uint8_t *);
static void (*job_done)(uint8_t *, int32_t);
static int32_t job_status;
static int job_pending, job_finished, worker_runs, queued, no_worker, name_ok = 1, layout_ok = 1;
int32_t dkrt_job_post(uintptr_t step, uintptr_t done, uint8_t *ctx)
{
    (void)ctx;
    if (no_worker)
        return -1;
    if (!step || !done || job_pending || job_finished)
        layout_ok = 0;
    job_step = (int32_t (*)(uint8_t *))step;
    job_done = (void (*)(uint8_t *, int32_t))done;
    job_pending = 1;
    queued++;
    return 1;
}

/* ---- DSP model ---- */
static uint32_t d_last = 0xd0000000u, d_count, d_status = 1, d_err, d_errcount;
static int sends, drop_sends, freeze_frames, force_err_tag, garble;
static uint32_t sent_tags[2000];
void *sp_stock_copy(void *d, const void *s, uint32_t n)
{
    memcpy(d, s, n);
    return d;
}
/* a page that was received completely waits `ack_latency` frames before the DSP's LP0 callback copies it and the reply says so */
static struct {
    int on;
    uint32_t wait, tag;
    uint8_t data[4096];
} pend;
static void dsp_apply(void)
{
    uint32_t r = force_err_tag ? SPW_ERR_TAG : spw_dsp_rule(d_last, pend.tag, DDR_BYTES / 4096u);
    force_err_tag = 0;
    pend.on = 0;
    if (r >= 4) {
        d_status = 2;
        d_err = r;
        d_errcount++;
        return;
    }
    memcpy(ddr + spw_tag_page(pend.tag) * 4096u, pend.data, 4096);
    if (r == 0) {
        d_count++;
        d_last = pend.tag;
    }
    d_status = 1;
    d_err = 0;
}
static void deliver(void)
{
    static uint8_t reply[0xabc + 16 + 16], dest[0xabc + 16];
    uint16_t *h = (uint16_t *)(reply + 0xabc);
    h[0] = 0x5350;
    h[1] = 2;
    h[2] = d_status;
    h[3] = d_err | (d_errcount << 8);
    h[4] = d_count;
    h[5] = d_count >> 16;
    h[6] = (spw_tag_seq(d_last) << 8) | (d_last & 0xff);
    h[7] = (spw_ack_sum(h) ^ 0x5a5a) & 0xffff;
    if (garble)
        h[7] ^= 1;
    sp_copy_reply(dest, reply, 0xabc);
}
static int in_worker, ui_blocked; /* the UI task is inside a card read: its tick waits */
static void advance(uint32_t us);
static void frame_event(void)
{
    clock_now++;
    frames_n++;
    if (pend.on && --pend.wait == 0)
        dsp_apply();
    if (freeze_frames > 0)
        freeze_frames--;
    else
        deliver();
}
static void ui_event(void)
{
    if (job_finished) {
        job_finished = 0;
        job_done(0, job_status);
    }
    in_tick = 1;
    dkpool_tick();
    in_tick = 0;
    if (job_pending) {
        int32_t rc;
        job_pending = 0;
        worker_runs++;
        job_t0 = t_us;
        in_worker = 1;
        do
            rc = job_step(0);
        while (rc == 1);
        in_worker = 0;
        job_t1 = t_us;
        job_status = rc;
        job_finished = 1;
    }
}
static void advance(uint32_t us)
{
    uint32_t target = t_us + us;
    for (;;) {
        uint32_t ui = ui_blocked ? 0xffffffffu : next_ui_us,
                 ev = next_frame_us < ui ? next_frame_us : ui;
        if (ev > target) {
            if (t_us < target)
                t_us = target;
            return;
        }
        if (t_us < ev)
            t_us = ev;
        if (ev == next_frame_us && ev <= ui) {
            next_frame_us += FRAME_US;
            frame_event();
        } else {
            next_ui_us += ui_period * FRAME_US;
            ui_event();
        }
    }
}
void sp_stock_sleep(uint32_t us)
{
    advance(us);
}

/* ---- the LP0 port: the raw sender writes halves; the model decodes them the way the stock helper's order defines ----
 * word = cmd 0x0080, poll bit 0 of the status, then b3 b2 b1 b0 (memory order b0 first) as `byte << 8 | 0x80`, `byte << 8`. */
static uint32_t words_got, polls_run, max_polls, status_reads, stuck_pages, port_words_total,
    cmd_seen, nh;
static uint16_t hv[8];
static uint32_t cur_tag;
static uint8_t cur_page[4096];
static int bad_port;
static int pages_sent_n;
static void (*on_page)(int n);
static uint16_t halves_log[16];
static int halves_n;
void sp_port_cmd(uint16_t v)
{
    if (v != 0x80)
        bad_port++;
    cmd_seen++;
    polls_run = 0;
}
uint16_t sp_port_status(void)
{
    status_reads++;
    if (stuck_pages && words_got >= 4) { /* the receiver stops answering in the middle of a page */
        if (++polls_run > max_polls)
            max_polls = polls_run;
        if (polls_run >= 8192) {
            stuck_pages--;
            words_got = 0;
            nh = 0;
        }
        return 0;
    }
    return 1;
}
void sp_port_data(uint16_t v)
{
    uint32_t k, m[4], b[4];
    if (halves_n < 16)
        halves_log[halves_n++] = v;
    if (!cmd_seen)
        bad_port++;
    hv[nh++] = v;
    if (nh < 8)
        return;
    nh = 0;
    cmd_seen = 0;
    for (k = 0; k < 4; k++) {
        if (hv[2 * k] != (uint16_t)(hv[2 * k + 1] | 0x80) || (hv[2 * k + 1] & 0xff))
            bad_port++;
        b[k] = hv[2 * k + 1] >> 8; /* the wire order: b3 b2 b1 b0 */
    }
    m[0] = b[3];
    m[1] = b[2];
    m[2] = b[1];
    m[3] = b[0];
    if (words_got == 0)
        cur_tag = (m[0] << 24) | (m[1] << 16) | (m[2] << 8) | m[3];
    else
        for (k = 0; k < 4; k++)
            cur_page[4 * (words_got - 1) + k] = (uint8_t)m[k];
    port_words_total++;
    if (++words_got < 1025)
        return;
    words_got = 0;
    advance(send_us); /* the send took this long; the interrupt and the UI task ran meanwhile */
    sent_tags[sends++] = cur_tag;
    if (drop_sends > 0)
        drop_sends--;
    else {
        memcpy(pend.data, cur_page, 4096);
        pend.tag = cur_tag;
        pend.wait = ack_latency;
        pend.on = 1;
    }
    pages_sent_n++;
    if (on_page)
        on_page(pages_sent_n);
}

/* ---- store ---- */
typedef struct {
    char path[32];
    uint32_t size, id;
    uint8_t *data;
} File;
static File files[8];
static int nfiles;
static int read_fail_at = -1, reads;
static void (*on_read)(int n);
static uint32_t read_log[64][2];
static uint32_t val(uint32_t id, uint32_t frame, uint32_t ch)
{
    return (id * 7919u + frame * 31u + ch * 1009u + 17u) & 0xffffu;
}

static void make_file(const char *path, uint32_t id, uint32_t frames, uint32_t channels,
                      int corrupt)
{
    File *f = &files[nfiles++];
    uint32_t pcm = frames * 2u * channels, i, c;
    f->size = 64u + pcm + 16u;
    f->id = id;
    f->data = calloc(1, f->size + 4096);
    strcpy(f->path, path);
    f->data[1] = (uint8_t)(channels - 1u);
    f->data[4] = pcm >> 24;
    f->data[5] = pcm >> 16;
    f->data[6] = pcm >> 8;
    f->data[7] = pcm;
    f->data[8] = 0;
    f->data[9] = 0;
    f->data[10] = 0xbb;
    f->data[11] = 0x80; /* 48000 */
    f->data[0x14] = 0x7f;
    if (corrupt)
        f->data[1] = 2; /* stereo byte above 1: spw_header rejects it */
    for (i = 0; i < frames; i++)
        for (c = 0; c < channels; c++) {
            uint32_t v = val(id, i, c), at = 64u + (i * channels + c) * 2u;
            f->data[at] = v >> 8;
            f->data[at + 1] = v;
        }
}
int32_t dkrt_store_stat(const uint8_t *path, DkrtStoreFile *o)
{
    int i;
    for (i = 0; i < nfiles; i++)
        if (!strcmp((const char *)path, files[i].path)) {
            o->first = (uint32_t)i * 0x10000u;
            o->size = files[i].size;
            memset(o->sha, files[i].id, 32);
            return 0;
        }
    return DKRT_E_NOT_FOUND;
}
/* leases (the runtime's dkrt_store_lease): ids from 1, `leases_live` counts the ones not ended; lease_full makes the next lease fail with E_NOMEM */
static int lease_next = 1, leases_live, lease_full, lease_bad_end;
static uint8_t lease_open[64];
int32_t dkrt_store_lease(const uint8_t *path, DkrtStoreFile *o)
{
    int32_t rc;
    if (lease_full)
        return DKRT_E_NOMEM;
    rc = dkrt_store_stat(path, o);
    if (rc != 0)
        return rc;
    lease_open[lease_next] = 1;
    leases_live++;
    return lease_next++;
}
void dkrt_store_unlease(int32_t id)
{
    if (id <= 0 || id >= 64 || !lease_open[id]) {
        lease_bad_end++;
        return;
    }
    lease_open[id] = 0;
    leases_live--;
}
int32_t dkrt_store_read(const DkrtStoreFile *f, uint32_t sector, uint32_t count, uint8_t *dst)
{
    File *file = &files[f->first / 0x10000u];
    uint32_t sectors = (file->size + 511u) / 512u;
    if (((uintptr_t)dst & 15u) || count < 1 || count > 128 || sector + count > sectors) {
        printf("badread %u %u\n", sector, count);
        exit(3);
    }
    if (reads < 64) {
        read_log[reads][0] = sector;
        read_log[reads][1] = count;
    }
    if (on_read)
        on_read(reads);
    if (reads++ == read_fail_at)
        return DKRT_E_READ;
    memcpy(dst, file->data + sector * 512u, count * 512u);
    if (!in_worker)
        ui_blocked++; /* a read of the UI task: the interrupt goes on, the UI task is busy */
    advance(read_us_base +
            count *
                read_us_sector); /* the card takes time: frames and (for the worker) ticks go on */
    if (!in_worker)
        ui_blocked--;
    return 0;
}

static void step(void)
{
    advance(FRAME_US);
}
static void run(int n, int stop_after_done)
{
    int d0 = done_n;
    while (n-- > 0) {
        step();
        if (stop_after_done && done_n > d0)
            break;
    }
}

/* ---- helpers ---- */
static dkpool_handle H[8];
static dk_file_id fid(const char *path)
{
    dk_file_id id;
    int i;
    memset(&id, 0, sizeof id);
    for (i = 0; i < nfiles; i++)
        if (!strcmp(path, files[i].path)) {
            id.size = files[i].size;
            id.flags = 1;
            id.hash_kind = 1;
            memset(id.hash, files[i].id, 32);
        }
    return id;
}
static int32_t load(uint32_t req, const char *path, dkpool_handle *h)
{
    dk_file_id id = fid(path);
    return dkpool_load(req, &id, (const uint8_t *)path, h);
}
static uint32_t off_of(uint32_t nw)
{
    return 0x80000000u + (nw - 0x10000000u) * 4u - DDR_BASE;
}
/* the planes in DDR against the pattern, the zero padding of each plane's last page included: bit 0 L ok, bit 1 R ok */
static int planes_ok(const dkpool_handle *h, uint32_t id)
{
    int ok = 3;
    uint32_t ch, i, pp = (h->frames + 2047) / 2048;
    for (ch = 0; ch < h->channels; ch++) {
        uint32_t off = off_of(ch ? h->base_r : h->base_l);
        for (i = 0; i < pp * 2048; i++) {
            uint32_t want = i < h->frames ? val(id, i, ch) : 0,
                     got = ((uint32_t)ddr[off + 2 * i] << 8) | ddr[off + 2 * i + 1];
            if (got != want)
                ok &= ~(1 << ch);
        }
    }
    if (h->channels == 1)
        ok &= 1;
    return ok;
}
static uint32_t cancel_req, cancel_at_page;
static void page_cancel(int n)
{
    if ((uint32_t)n == cancel_at_page) {
        dkpool_cancel(cancel_req);
        on_page = 0;
    }
}
static void read_cancel(int n)
{
    (void)n;
    if (in_worker) {
        dkpool_cancel(cancel_req);
        on_read = 0;
    }
} /* the first read of the job, not the header read */
static void page_freeze(int n)
{
    if (n == 2) {
        freeze_frames = 120;
        on_page = 0;
    }
}
static void show(const char *k, const dkpool_handle *h)
{
    printf("%s id=%u frames=%u ch=%u rate=%u l=%x r=%x\n", k, h->id, h->frames, h->channels,
           h->rate, h->base_l, h->base_r);
}

int main(int argc, char **argv)
{
    const char *s = argc > 1 ? argv[1] : "";
    int32_t rc;
    printf("wire %u\n", sp_wire_size());
    if (!strcmp(s, "stereo")) {
        make_file("a", 1, 3000, 2, 0);
        rc = load(1, "a", &H[0]);
        run(2000, 1);
        printf("rc %d done %d %d sends %d\n", rc, done_n, done_res[0], sends);
        show("h", &H[0]);
        printf("planes %d check %d free %u\n", planes_ok(&H[0], 1), dkpool_check(&H[0]),
               dkpool_free_frames());
        printf("tags %x %x %x %x\n", sent_tags[0], sent_tags[1], sent_tags[2], sent_tags[3]);
    } else if (!strcmp(s, "mono")) {
        make_file("a", 2, 5000, 1, 0);
        rc = load(7, "a", &H[0]);
        run(2000, 1);
        printf("rc %d done %d %d req %u sends %d\n", rc, done_n, done_res[0], done_req[0], sends);
        show("h", &H[0]);
        printf("planes %d\n", planes_ok(&H[0], 2));
        printf("tags %x %x %x\n", sent_tags[0], sent_tags[1], sent_tags[2]);
    } else if (!strcmp(s, "exact")) { /* frames a multiple of 2048, and one frame */
        make_file("a", 3, 4096, 2, 0);
        make_file("b", 4, 1, 1, 0);
        load(1, "a", &H[0]);
        run(3000, 1);
        load(2, "b", &H[1]);
        run(3000, 1);
        printf("done %d %d %d sends %d planes %d %d\n", done_n, done_res[0], done_res[1], sends,
               planes_ok(&H[0], 3), planes_ok(&H[1], 4));
    } else if (!strcmp(s, "dedup")) {
        make_file("a", 5, 3000, 2, 0);
        load(1, "a", &H[0]);
        run(2000, 1);
        int before = sends;
        rc = load(2, "a", &H[1]);
        printf("second rc %d sends %d same %d token %d %d\n", rc, sends - before,
               H[0].base_l == H[1].base_l && H[0].base_r == H[1].base_r, H[0].token == H[1].token,
               dkpool_check(&H[0]));
        dkpool_release(&H[0]);
        printf("after one release check %d %d\n", dkpool_check(&H[0]), dkpool_check(&H[1]));
        dkpool_release(&H[1]);
        printf("after two release check %d free %u\n", dkpool_check(&H[1]), dkpool_free_frames());
    } else if (!strcmp(s, "cache")) {
        make_file("a", 6, 3000, 2, 0);
        load(1, "a", &H[0]);
        run(2000, 1);
        dkpool_release(&H[0]);
        int before = sends;
        rc = load(2, "a", &H[1]);
        printf("rc %d sends %d same %d tokens differ %d old %d new %d planes %d\n", rc,
               sends - before, H[0].base_l == H[1].base_l, H[0].token != H[1].token,
               dkpool_check(&H[0]), dkpool_check(&H[1]), planes_ok(&H[1], 6));
        dkpool_release(&H[0]); /* a stale handle changes nothing */
        printf("still ready %d\n", dkpool_check(&H[1]));
    } else if (!strcmp(s, "evict")) {
        make_file("a", 7, 140000, 2, 0);
        make_file("b", 8, 140000, 2, 0); /* 69 pages a plane, 138 pages each */
        load(1, "a", &H[0]);
        run(20000, 1);
        printf("a %d free %u\n", done_res[0], dkpool_free_frames());
        rc = load(2, "b", &H[1]);
        printf("b while a is ready: %d\n", rc);
        dkpool_release(&H[0]);
        printf("free with a cached %u\n", dkpool_free_frames());
        rc = load(3, "b", &H[1]);
        run(20000, 1);
        printf("b rc %d done %d planes %d a stale %d\n", rc, done_res[1], planes_ok(&H[1], 8),
               dkpool_check(&H[0]));
        int before = sends;
        dkpool_release(&H[1]);
        rc = load(4, "a", &H[0]);
        printf("a again rc %d\n", rc);
        run(20000, 1);
        printf("a sends %d planes %d\n", sends - before, planes_ok(&H[0], 7));
    } else if (!strcmp(s, "resend")) {
        make_file("a", 9, 3000, 2, 0);
        drop_sends = 1;
        load(1, "a", &H[0]);
        run(3000, 1);
        printf("done %d %d sends %d planes %d tag0 %x tag1 %x\n", done_n, done_res[0], sends,
               planes_ok(&H[0], 9), sent_tags[0], sent_tags[1]);
    } else if (
        !strcmp(
            s,
            "lostack")) { /* the page is accepted but the acknowledgement is late: the resend is a repeat */
        make_file("a", 10, 3000, 2, 0);
        on_page =
            page_freeze; /* the replies stop for 120 frames (80 ms) while the second page is in flight */
        load(1, "a", &H[0]);
        run(3000, 1);
        printf("done %d %d sends %d count %u planes %d\n", done_n, done_res[0], sends, d_count,
               planes_ok(&H[0], 10));
    } else if (!strcmp(s, "dsperr")) {
        make_file("a", 11, 3000, 2, 0);
        make_file("b", 12, 3000, 2, 0);
        force_err_tag = 1;
        load(1, "a", &H[0]);
        run(3000, 1);
        printf("a %d free %u\n", done_res[0], dkpool_free_frames());
        load(2, "b", &H[1]);
        run(3000, 1);
        printf("b %d planes %d\n", done_res[1], planes_ok(&H[1], 12));
    } else if (!strcmp(s, "dead")) {
        make_file("a", 13, 3000, 2, 0);
        d_status = 3;
        d_err = 9;
        load(1, "a", &H[0]);
        run(3000, 1);
        printf("a %d sends %d\n", done_res[0], sends);
        d_status = 1;
        rc = load(2, "a", &H[0]);
        printf("next %d\n", rc);
    } else if (!strcmp(s, "timeout")) {
        make_file("a", 14, 3000, 2, 0);
        drop_sends = 100;
        load(1, "a", &H[0]);
        run(3000, 1);
        /* the job gave up 4 x 75 blocks after its first send; the result reaches the runtime at the next UI pass, at most ui_period later */
        printf("a %d sends %d job blocks %u free %u\n", done_res[0], sends,
               (job_t1 - job_t0) / FRAME_US, dkpool_free_frames());
    } else if (!strcmp(s, "unarmed")) {
        make_file("a", 15, 3000, 2, 0);
        d_status = 0;
        load(1, "a", &H[0]);
        run(3000, 1);
        printf("a %d sends %d job blocks %u\n", done_res[0], sends, (job_t1 - job_t0) / FRAME_US);
    } else if (!strcmp(s, "cancel")) {
        make_file("a", 16, 3000, 2, 0);
        make_file("b", 17, 3000, 2, 0);
        make_file("c", 18, 30000, 2, 0);
        load(1, "a", &H[0]);
        load(2, "b", &H[1]);
        dkpool_cancel(2); /* b has not started */
        run(3000, 0);
        printf("done %d: %u %d, %u %d\n", done_n, done_req[0], done_res[0], done_req[1],
               done_res[1]);
        printf("a planes %d b blocks %u\n", planes_ok(&H[0], 16), sp_pool.blk[1].state);
        int before = sends;
        load(3, "c", &H[2]);
        cancel_req = 3;
        cancel_at_page = pages_sent_n + 2;
        on_page = page_cancel; /* the UI task cancels while the second page is in flight */
        run(3000, 0);
        printf("c done %d: %u %d sends %d\n", done_n, done_req[2], done_res[2], sends - before);
        printf("free %u\n", dkpool_free_frames());
    } else if (!strcmp(s, "cancelread")) { /* cancelled during a card read: no page goes out */
        make_file("a", 23, 30000, 2, 0);
        cancel_req = 1;
        on_read = read_cancel;
        load(1, "a", &H[0]);
        run(3000, 0);
        printf("done %d %d sends %d reads %d free %u\n", done_n, done_res[0], sends, reads,
               dkpool_free_frames());
        /* the next load starts at the receiver's counters as they are */
        make_file("b", 24, 3000, 2, 0);
        on_read = 0;
        load(2, "b", &H[1]);
        run(3000, 1);
        printf("b %d planes %d\n", done_res[1], planes_ok(&H[1], 24));
    } else if (
        !strcmp(
            s,
            "lease")) { /* a lease from dkpool_load to the end of the request, whatever the end is */
        make_file("a", 40, 3000, 2, 0);
        make_file("b", 41, 3000, 2, 0);
        make_file("c", 42, 30000, 2, 0);
        make_file("bad", 43, 3000, 2, 1);
        rc = load(1, "a", &H[0]);
        printf("queued rc %d live %d\n", rc, leases_live);
        run(3000, 1);
        printf("done %d live %d\n", done_res[0], leases_live);
        rc = load(2, "nothing", &H[1]);
        printf("missing rc %d live %d\n", rc, leases_live);
        rc = load(3, "bad", &H[1]);
        printf("bad rc %d live %d\n", rc, leases_live);
        load(4, "b", &H[1]);
        load(5, "c", &H[2]);
        printf("two queued live %d\n", leases_live);
        dkpool_cancel(5); /* c has not started */
        run(3000, 0);
        printf("after cancel of a queued one live %d\n", leases_live);
        make_file("d", 44, 30000, 2, 0);
        cancel_req = 6;
        cancel_at_page = pages_sent_n + 2;
        on_page = page_cancel;
        load(6, "d", &H[3]);
        run(10, 0);
        printf("running live %d\n", leases_live);
        run(3000, 0);
        printf("cancelled running live %d\n", leases_live);
        make_file("e", 45, 3000, 2, 0);
        reads = 0;
        read_fail_at = 1;
        on_read = 0;
        load(7, "e", &H[4]);
        run(3000, 0);
        printf("read fail %d live %d\n", done_res[done_n - 1], leases_live);
        lease_full = 1;
        make_file("f", 46, 3000, 2, 0);
        rc = load(8, "f", &H[5]);
        printf("table full rc %d live %d\n", rc, leases_live);
        lease_full = 0;
        rc = load(9, "f", &H[5]);
        run(3000, 1);
        printf("then ok rc %d done %d live %d bad ends %d\n", rc, done_res[done_n - 1], leases_live,
               lease_bad_end);
    } else if (
        !strcmp(
            s,
            "window")) { /* a peak window after the store file changed reads nothing at the old extent */
        int16_t out[16];
        uint32_t cursor = 0;
        int before, rc2;
        make_file("a", 50, 3000, 2, 0);
        load(1, "a", &H[0]);
        run(3000, 1);
        memset(out, 0, sizeof out);
        rc = dkpool_window_step(&H[0], (const uint8_t *)"a", 0, 3000, 8, out, &cursor);
        printf("same file rc %d cursor %u live %d\n", rc, cursor, leases_live);
        before = reads;
        cursor = 0;
        files[0].id = 99; /* replaced: another hash at the same path */
        rc = dkpool_window_step(&H[0], (const uint8_t *)"a", 0, 3000, 8, out, &cursor);
        printf("replaced rc %d reads %d live %d\n", rc, reads - before, leases_live);
        files[0].id = 50;
        files[0].path[0] = 'z'; /* deleted */
        rc2 = dkpool_window_step(&H[0], (const uint8_t *)"a", 0, 3000, 8, out, &cursor);
        printf("deleted rc %d reads %d live %d\n", rc2, reads - before, leases_live);
        printf("pool still ready %d\n", dkpool_check(&H[0]));
    } else if (
        !strcmp(
            s,
            "settle")) { /* cancelled with a page in flight and a late acknowledgement: the page is settled before the load ends */
        make_file("a", 25, 30000, 2, 0);
        freeze_frames = 40;
        cancel_req = 1;
        cancel_at_page = 1;
        on_page = page_cancel;
        load(1, "a", &H[0]);
        run(3000, 0);
        printf("done %d %d sends %d count %u\n", done_n, done_res[0], sends, d_count);
    } else if (
        !strcmp(
            s,
            "worker")) { /* the hand-over: posted once per load, completion only from the UI tick, one job at a time */
        make_file("a", 26, 3000, 2, 0);
        make_file("b", 27, 3000, 1, 0);
        load(1, "a", &H[0]);
        load(2, "b", &H[1]);
        run(3000, 0);
        printf("queued %d runs %d name %d layout %d done %d outside %d planes %d %d\n", queued,
               worker_runs, name_ok, layout_ok, done_n, done_outside_tick, planes_ok(&H[0], 26),
               planes_ok(&H[1], 27));
        printf("bad port %d stuck %u\n", bad_port, sp_stuck_sends);
    } else if (!strcmp(s, "noworker")) {
        make_file("a", 28, 3000, 2, 0);
        no_worker = 1;
        load(1, "a", &H[0]);
        run(200, 1);
        printf("a %d queued %d free %u\n", done_res[0], queued, dkpool_free_frames());
    } else if (!strcmp(s, "slowui")) { /* the page rate does not depend on the UI pass rate */
        make_file("a", 29, 144000, 2, 0);
        ui_period = atoi(argc > 2 ? argv[2] : "50");
        ack_latency = (uint32_t)atoi(argc > 3 ? argv[3] : "1");
        send_us = (uint32_t)atoi(argc > 4 ? argv[4] : "300");
        load(1, "a", &H[0]);
        run(200000, 1);
        printf("done %d pages %d reads %d planes %d\n", done_res[0], sends, reads,
               planes_ok(&H[0], 29));
        printf("job us %u\n", job_t1 - job_t0);
    } else if (
        !strcmp(
            s,
            "wire")) { /* the 16 first halves of a send, and the bound of a port that never answers */
        static uint8_t page[4096];
        int i;
        for (i = 0; i < 4096; i++)
            page[i] = (uint8_t)(i * 5 + 1);
        printf("send %d\n", sp_raw_send(0xd0123456u, page));
        for (i = 0; i < 16; i++)
            printf("%04x ", halves_log[i]);
        printf("\nsent tag %x page first %02x %02x %02x %02x last %02x bad %d words %u\n",
               sent_tags[0], cur_page[0], cur_page[1], cur_page[2], cur_page[3], cur_page[4095],
               bad_port, port_words_total);
        stuck_pages = 1;
        printf("stuck send %d max polls %u reads\n", sp_raw_send(0xd0123457u, page), max_polls);
    } else if (
        !strcmp(
            s,
            "stuck")) { /* the port does not answer in the middle of a page: bounded, the page is lost and sent again */
        make_file("a", 31, 3000, 2, 0);
        stuck_pages = 1;
        load(1, "a", &H[0]);
        run(3000, 1);
        printf("done %d %d stuck %u max polls %u sends %d planes %d\n", done_n, done_res[0],
               sp_stuck_sends, max_polls, sends, planes_ok(&H[0], 31));
    } else if (!strcmp(s,
                       "ranges")) { /* the card reads: 64 KiB covers 7 stereo or 15 mono chunks */
        int i;
        make_file("a", 32, 144000, 2, 0);
        make_file("b", 33, 5000, 1, 0);
        make_file("c", 34, 60000, 1, 0);
        load(1, "a", &H[0]);
        run(200000, 1);
        /* read_log[0] is the header read of dkpool_load; the others are the job's */
        printf("a reads %d first", reads - 1);
        for (i = 1; i < 3; i++)
            printf(" %u/%u", read_log[i][0], read_log[i][1]);
        printf(" last %u/%u planes %d\n", read_log[reads - 1][0], read_log[reads - 1][1],
               planes_ok(&H[0], 32));
        reads = 0;
        load(2, "b", &H[1]);
        run(200000, 1);
        printf("b reads %d %u/%u planes %d\n", reads - 1, read_log[1][0], read_log[1][1],
               planes_ok(&H[1], 33));
        reads = 0;
        load(3, "c", &H[2]);
        run(200000, 1);
        printf("c reads %d %u/%u %u/%u planes %d\n", reads - 1, read_log[1][0], read_log[1][1],
               read_log[2][0], read_log[2][1], planes_ok(&H[2], 34));
    } else if (!strcmp(s, "readfail")) {
        make_file("a", 19, 30000, 2,
                  0); /* 3 reads of the job: the second one fails, after 14 pages */
        read_fail_at = 2;
        load(1, "a", &H[0]);
        run(3000, 1);
        printf("a %d sends %d free %u\n", done_res[0], sends, dkpool_free_frames());
    } else if (!strcmp(s, "badfile")) {
        make_file("bad", 20, 3000, 2, 1);
        printf("missing %d bad %d\n", load(1, "nope", &H[0]), load(2, "bad", &H[0]));
        printf("free %u\n", dkpool_free_frames());
    } else if (!strcmp(s, "tokens")) {
        make_file("a", 21, 3000, 2, 0);
        dkpool_free_frames();
        sp_pool.tokens = 0xffffffffu;
        printf("rc %d\n", load(1, "a", &H[0]));
    } else if (
        !strcmp(
            s,
            "many")) { /* the sequence number wraps (more than 256 pages), pages of all loads land right */
        char name[8];
        int i, bad = 0;
        for (i = 0; i < 5; i++) {
            name[0] = 'f';
            name[1] = '0' + i;
            name[2] = 0;
            make_file(name, 30 + i, 80 * 2048, 1, 0);
        }
        for (i = 0; i < 5; i++) {
            name[0] = 'f';
            name[1] = '0' + i;
            name[2] = 0;
            load(1 + i, name, &H[0]);
            run(20000, 1);
            if (done_res[i] != 0 || planes_ok(&H[0], 30 + i) != 1)
                bad++;
            dkpool_release(&H[0]);
        }
        printf("sends %d bad %d last seq %u\n", sends, bad, spw_tag_seq(sent_tags[sends - 1]));
    } else if (!strcmp(s, "garble")) {
        make_file("a", 22, 3000, 2, 0);
        garble = 1;
        load(1, "a", &H[0]);
        run(3000, 1);
        printf("a %d sends %d\n", done_res[0], sends);
    } else {
        puts("unknown scenario");
        return 2;
    }
    return 0;
}
