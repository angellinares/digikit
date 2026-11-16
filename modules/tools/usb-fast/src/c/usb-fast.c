/* digikit/usb-fast: file uploads on the USB bulk interface of core/usb-bulk (docs/plans/usb-fast.md section 5, usb-fast-hardware.md "Step 4").
 *
 * One I/O job (<dk/platform/io.h>) enables the interface, takes the received buffers, cuts the byte stream into frames by their own length field, runs each
 * request on file_create / file_write / file_close / file_abort (<dk/platform/file.h>) and sends the reply on EP2 IN. The numbers are those of sdk/sdk.toml
 * [protocol.bulk] (request: 'DKB1' | cmd u8 | seq u8 | flags u16 | len u32 | body; reply: 'DKB1' | cmd|0x40 | seq | status i16 | len u32 | body;
 * big-endian). SysEx stays the runtime's channel for everything else; this module adds one SysEx command: SET_NONCE (0x36), by which the host proves
 * which USB device is on its MIDI port. The job calls dk_usb_bulk_enable at its start and, while it waits on state (not enabled, a mode that is not a bulk mode, a
 * mode switch settling), again at every wake by the module's tick (about four a second): it enables when the user's USB CONFIG brings USB MIDI mode. A device that
 * stays in a mode without bulk for 2 s ends the session (disable, buffers freed); the next enable follows when the mode is usable (OUT-only: at the next nonce).
 *
 * Stream: a frame may span receive buffers and a buffer may hold several frames. Every frame is copied into one 16-byte-aligned frame buffer (header 12 +
 * DATA offset 4 + data_max), so a DATA request hands file_write an aligned pointer; each receive buffer is released as soon as its bytes are copied, so
 * the host is NAKed only while this job is busy (the flow control). A bad magic or a length over data_max + 4 + frame_slack is fatal for the stream: the
 * job aborts the open file and replies E_MSG only when the magic was valid (the seq can be trusted), then discards what is left of the current USB
 * transfer and parses headers again from the next one. The adapter hands over a transfer in buffers of 16 KiB: a buffer shorter than that ends the
 * transfer (a transfer of an exact multiple of 16 KiB ends with an empty packet that the adapter drops, so the next buffer is discarded too: the host
 * then times out one request and resyncs). A BEGIN while a file of ours is open aborts that file first: the module is the only bulk client, so the
 * host has given the old file up.
 *
 * Requests are handled one at a time and in order, one reply each (a reply that cannot be sent yet is kept; nothing is parsed meanwhile). A file_*
 * call already waits about 3 s for the store lock; one that answers E_BUSY is called again twice (DK_IO_MORE, the runtime waits 1 ms), then the host
 * gets E_BUSY and asks again. A change of the reset count of dk_usb_bulk_state (a bus reset or a disconnect) aborts the open file and clears the parser. A
 * dk_usb_bulk_send error other than busy or E_AGAIN drops the reply, the open file and the parser (counted in STATUS).
 *
 * Accumulation: at BEGIN the job tries to get a staging buffer (1 MiB, then 256 KiB, never more than the file needs; dk_alloc, then dk_fw_alloc) and
 * keeps the DATA bytes in it in order. A DATA reply means "accepted": when the buffer is full, one file_write of the whole buffer goes out and that DATA
 * carries its status. A failed flush is sticky: every later DATA and END return it (the host aborts). A flush that stays E_BUSY after its tries is not
 * reported (the bytes are accepted): the next DATA or END flushes first and answers E_BUSY while it cannot. END flushes the tail with one more
 * file_write (the store takes a length that is not a multiple of 512 only at the end of the file), then closes. pump() copies a DATA body straight
 * from the receive buffer into the staging buffer when it fits in the room left (one copy); else the body goes through `frame` and dispatch() copies
 * it. No buffer: one file_write per DATA, from `frame`. The buffer is freed at END, abort, bus reset, send error and unload. The END hash check
 * guards the result.
 *
 * Copy/write overlap (docs/plans/usb-fast-batch.md E5): a file larger than 512 KiB, with the 1 MiB reservation, stages in two halves of 512 KiB.
 * A half that fills becomes the pending write and the stream goes on in the other half; the write is made from the step loop (ov_service) and the helper task
 * of the runtime (dk_helper_run: pure compute, no import) copies the DATA bodies that the receive ring holds already into the other half while dk_files_write
 * blocks the I/O task. pump() records those copies (a plan) instead of making them and keeps the receive buffers until the helper is collected; frames are
 * finished in order, so the replies stay in order. The flushing DATA is answered at once; a failed write is sticky (acc_err) and shows on the next DATA or END.
 * An E_BUSY write is tried again like any file call; after its tries the pending half is written by the next DATA or END before anything else.
 *
 * Batch commit (docs/plans/usb-fast-batch.md B): BEGIN flag DEFER (0x0002) creates the file with DK_FILE_DEFER: END verifies it as ever, but the file is only
 * pending; one table commit names up to 16 files. The END reply body is hash4 | pending u16 | reserved u16: pending 0 means everything sent so far is committed.
 * COMMIT (0x07) calls dk_files_commit; its reply body is pending u16 | reserved u16. The runtime also commits the pending files 1 s after the last file call, so an
 * unplug or a host crash loses nothing that END reported verified. ABORT drops the open file only.
 *
 * VERIFY is reserved: the reply is E_PROTO (the host verifies over SysEx: both devices are CPU bound there with tiny replies). */
/* the code is placed as it is: no jump tables (data in the code) */
#pragma GCC optimize("no-jump-tables")
#include <dk/core/module.h>
#include <dk/core/bytes.h>
#include <dk/core/log.h>
#include <dk/core/mem.h>
#include <dk/core/time.h>
#include <dk/platform/debug.h>
#include <dk/platform/files.h>
#include <dk/platform/hash32.h>
#include <dk/platform/host.h>
#include <dk/platform/io.h>
#include <dk/platform/sdhc_large.h>
#include <dk/platform/usb_bulk.h>
#include "uf.h"

typedef uint8_t u8;
typedef uint16_t u16;
typedef uint32_t u32;
typedef int32_t i32;

/* sdk/sdk.toml [protocol.bulk] */
#define HDR 12u
#define DATA_MAX 65536u
#define ALIGN 512u
#define SLACK 64u
#define NONCE_LEN 8u
#define REPLY_FLAG 0x40u
#define FLAG_REPLACE 0x0001u
#define FLAG_DEFER 0x0002u
#define CMD_INFO 1u
#define CMD_BEGIN 2u
#define CMD_DATA 3u
#define CMD_END 4u
#define CMD_ABORT 5u
#define CMD_VERIFY 6u /* reserved: E_PROTO */
#define CMD_COMMIT 7u
#define BULK_VERSION 1u
/* the largest body is a DATA request: offset u32 + data_max, plus the slack that BEGIN's path needs */
#define FRAME_MAX (HDR + 4u + DATA_MAX + SLACK)
/* [protocol.service] USB_FAST_NONCE; [protocol.ranges] usb_fast = [0x36, 0x37] */
#define NONCE_CMD 0x36u
#define NONCE_RANGE 2u
/* sdk/sdk.toml [protocol.status] */
#define E_MSG (-50)
#define E_STATE (-53)
#define E_PROTO (-60)
/* staging buffer sizes: both multiples of 512; the 1 MiB write call is the fastest on the +Drive (10.3 MB/s against 2.6 with 64 KiB) */
#define ACC_BIG 1048576u
#define ACC_MID 262144u
/* a file_* call that answers E_BUSY (after waiting about 3 s for the store) is called again once or twice, then the host is told: tries 1 and 2 retry, the third replies */
#define BUSY_TRIES 3u
/* BEGIN waits longer: the store, or the +Drive's staging folder (removed once its upload session has been idle for 2 s), may be busy for a few hundred ms. A
 * file_* call that answers E_BUSY makes the I/O task sleep 1 ms before the step that returns DK_IO_MORE runs again (a timed wait, no spin), so 400 tries
 * cover about 0.4 s and more. */
#define BEGIN_TRIES 400u
/* The runtime drops a write session that has had no BEGIN or write for 20 s (1500 blocks a second); in OUT-only mode the job ends its bulk session 1 s later.
 * In pair mode the staging buffer waits for the next write, which answers E_STATE. */
#define SESS_IDLE_BLOCKS (21u * 1500u)
/* the device stays in a mode without bulk for this long (2 s) before the job ends its session */
#define WRONG_MODE_BLOCKS (2u * 1500u)
/* the module's own timer: the job is woken about four times a second while it waits for a mode or a nonce */
#define TICK_BLOCKS (1500u / 4u)
/* steps of work (frames dispatched) before the job gives the other I/O jobs a turn; a staging flush ends a step at once */
#define STEP_BUDGET 8u

static dk_files *files;
static dk_usb_bulk *ub;
static dk_sdhc_large *sl; /* NULL without core/sdhc-large: dk_sdhc_large_ctl then returns 0 */
static i32 job_id;
static int enabled;
static int nomem_logged;
static u32 epoch;

static u8 nonce[NONCE_LEN];
static volatile u32
    nonce_gen; /* a seqlock: odd while SET_NONCE writes (MIDI task), INFO (I/O task) reads until it sees an even, unchanged value */

/* the stream parser */
static u8 frame_raw[FRAME_MAX + 16u];
static u8 *
    frame; /* frame_raw rounded up to 16 bytes at run time: the loader's bss alignment is not relied on */
static u32 have;  /* bytes of the frame in `frame` */
static u32 total; /* its length once the header is in, else 0 */
static void *cur; /* the receive buffer being copied, or 0 */
static u32 cur_len, cur_off;
static int frame_ready;
static int skipping; /* after a fatal error: discard buffers until the end of the transfer */
static u32 last_len; /* length of the last buffer taken */

/* The replies waiting for the engine: a queue of REPQ, each in a slot of REP_SLOT bytes, sent one per usb_bulk_send in order. The job keeps parsing while the send
 * slots are busy (the host may have a window of up to 12 DATA frames in flight and does not read replies while it is still sending: a single waiting reply would
 * stop the job, the receive ring would fill and the host's next frame would never leave). Nothing is parsed while the queue is full. */
#define REPQ 16u
#define REP_SLOT (HDR + 20u)
static u8 repq[REPQ][REP_SLOT];
static u8 rep_n[REPQ];
/* rep_head, rep_count and the slots are shared by the job (I/O task) and the MIDI task (the 'R' drain): every change is made with the interrupt mask on. */
static volatile u32 rep_head, rep_count;

static inline u32 sr_mask(void)
{
    u32 old;
    __asm__ volatile("move.w %%sr,%0\n\tmove.w #0x2700,%%sr" : "=d"(old) : : "memory", "cc");
    return old;
}

static inline void sr_restore(u32 old)
{
    __asm__ volatile("move.w %0,%%sr" : : "d"(old) : "memory", "cc");
}

/* the port is USB AUDIO/MIDI with only the bulk OUT endpoint: replies are not sent, the host takes them with `0x37 'R'` */
static int out_only(void)
{
    return (dk_usb_bulk_state(ub) & DK_USB_BULK_OUT_ONLY) != 0;
}

/* the file */
static i32 handle; /* > 0 while a file is open */
static u32 busy_tries;
static u32 st_steps, st_frames, st_tx_busy, st_last_rc, st_bufs, st_bytes, st_fatal, st_resets,
    st_send_err;
static i32 enable_rc;
static int wm_logged; /* the wrong-mode line is logged once per wait */
static int
    wm_on; /* an enabled session is in a port with no bulk route: wm_at is the block clock of the first step that saw it */
static int sess_oo; /* the session was enabled in OUT-only mode (USB AUDIO/MIDI) */
static u32 wm_at;
static u32 tick_at; /* the block clock of the last wake by tick */
static int flushed; /* a staging flush went out since the step last looked */
/* OUT-only mode: the bulk session (ring, staging buffer) exists only while a host works. SET_NONCE (MIDI task) counts a session request and wakes the job; the job
 * enables when the count differs from the one it served. The host sends SET_NONCE before INFO, and its INFO is a bulk OUT transfer that the controller NAKs until the
 * ring is primed by the enable, so INFO needs no retry. nonce_at is the block clock of the last request: the session expires 21 s after the later of it and the last
 * file activity (sess_at). */
static volatile u32 nonce_seq, nonce_at;
static u32 served_seq;
static u32
    sess_at; /* the block clock at the last file_create or successful file_write of the open file */
static u32 st_expired; /* sessions the job dropped for the runtime's idle expiry */

/* the staging buffer of the open file */
static u8 *acc_raw, *acc_buf; /* acc_buf: acc_raw rounded up to 16 bytes */
static int acc_fw;            /* acc_raw came from dk_fw_alloc */
static int acc_is_res; /* acc_raw is the session's reservation (never freed by acc_release) */
/* The staging reservation: taken before the adapter allocates its receive ring (the enable step), so the staging buffer gets the
 * heap first and the ring takes the largest tier that still fits; kept for the session and freed when the adapter's buffers are gone,
 * or at unload. acc_open hands the reservation to each file. */
static u8 *res_raw;
static int res_fw;
static u32 res_cap;
static u32 acc_cap;  /* 0: no buffer; with `ov`, the size of one half */
static u32 acc_fill; /* bytes in it */
static u32 acc_off;  /* file offset of its first byte */
/* Copy/write overlap (docs/plans/usb-fast-batch.md E5): `ov` = the reservation is two halves. `acc_buf` is the half the stream fills; a full half becomes the pending
 * write (wbuf, wlen, woff) and the halves swap. The write goes out from the step loop (ov_service), and the copies of the frames already received go to the other
 * half on the runtime's helper task (dk_helper_run) while dk_files_write blocks the I/O task. A DATA that fills a half replies at once (accepted); a failed write is
 * sticky in acc_err, as before, and shows on the next DATA or END. */
static int ov;
static u8 *acc_oth; /* the half that is not being filled */
static u8 *wbuf;    /* the pending write */
static u32 wlen, woff;
static int
    w_giveup; /* the pending write stayed E_BUSY after its tries: the next DATA or END writes it first (acc_flush) */
static i32 acc_err; /* sticky status of a failed flush */
static u32 file_size;
static u32 acc_limit =
    ACC_BIG; /* the largest buffer the next BEGIN asks for (a one-shot test knob, STATUS 'A' with UF_KNOBS; acc_open resets it) */
#if UF_KNOBS
static u32 acc_got; /* the size the last BEGIN got */
#endif
static int d_acc; /* the DATA frame in `frame` is in the buffer already (a flush is retried) */
static int
    dstage; /* pump: 1 = the DATA offset is still to come, 2 = the body goes straight into the buffer */
static int direct;  /* the DATA frame ready has its body in the buffer */
static i32 abort_h; /* a handle to abort after a reset (file_abort may need several steps) */

#if UF_TRACE || UF_BENCH
/* The only clock a module can call is dk_time_blocks(), the 1500 Hz block counter (1 unit = 0.667 ms). A short call often reads 0 and a long one is
 * rounded, so only the totals of whole calls (per file) are meaningful, not one call. Units: "blk" tags are blocks; uf.bytes and uf.datas are counts. */
#define UF_NOW() dk_time_blocks()
#define UF_KIND_MOD_LOG                                                                            \
    14u /* sdk/sdk.toml [[log.kind]] 14 MOD_LOG: args msg0..msg3 (16 ASCII chars, big-endian words) and value */
#define UF_FLAGS                                                                                   \
    (2u << 1) /* level 2 (info) in bits 1-3 (level_shift 1): passes the default log level */
#if UF_TRACE
static u32 uf_write, uf_copy, uf_wait, uf_begin, uf_end, uf_bytes, uf_datas;
static u32 uf_wait_from;
static int
    uf_waiting; /* uf_wait_from is valid: the last request of this file was answered and no byte of the next one has been copied yet (the wait is the host's stall; the copy is uf_copy) */
#endif

/* `tag`: a string of at most 16 chars, padded with NULs */
void uf_log(const char *tag, u32 value)
{
    u32 a[5];
    u32 live = 1;
    for (u32 i = 0; i < 16u; i++) {
        u32 c = 0;
        if (live && tag[i] != 0)
            c = (u8)tag[i];
        else
            live = 0;
        if ((i & 3u) == 0u)
            a[i >> 2] = 0;
        a[i >> 2] |= c << (24u - 8u * (i & 3u));
    }
    a[4] = value;
    dk_log_event(UF_KIND_MOD_LOG, UF_FLAGS, a);
}
#endif

static void copy(u8 *d, const u8 *s, u32 n)
{
    if (n >= 32 &&
        (((u32)d | (u32)s) & 15u) == 0) { /* movem.l: bursts from the uncached receive buffers */
        for (u32 k = n >> 5; k; k--) {
            __asm__ volatile("movem.l (%1),%%d0-%%d3\n\t"
                             "movem.l 16(%1),%%d4-%%d7\n\t"
                             "movem.l %%d0-%%d3,(%0)\n\t"
                             "movem.l %%d4-%%d7,16(%0)"
                             :
                             : "a"(d), "a"(s)
                             : "d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7", "memory");
            d += 32;
            s += 32;
        }
        n &= 31u;
    }
    if (n >= 16 && (((u32)d ^ (u32)s) & 3u) == 0) {
        while ((u32)d & 3u) {
            *d++ = *s++;
            n--;
        }
        u32 *dw = (u32 *)d;
        const u32 *sw = (const u32 *)s;
        for (u32 w = n >> 2; w; w--)
            *dw++ = *sw++;
        d = (u8 *)dw;
        s = (const u8 *)sw;
        n &= 3u;
    }
    while (n--)
        *d++ = *s++;
}

static void acc_release(void)
{
    if (acc_raw && !acc_is_res) {
        if (acc_fw)
            dk_fw_free(acc_raw);
        else
            dk_free(acc_raw);
    }
    acc_raw = acc_buf = acc_oth = wbuf = 0;
    wlen = woff = 0;
    ov = w_giveup = 0;
    acc_is_res = 0;
    acc_cap = acc_fill = acc_off = 0;
    acc_err = 0;
    d_acc = 0;
}

static u8 *acc_try(u32 n)
{
    u8 *p = dk_alloc(n); /* 16-byte aligned (<dk/core/mem.h>): the exact size, no round up */
    acc_fw = 0;
    if (!p) {
        p = dk_fw_alloc(n);
        acc_fw = p != 0;
        if (p &&
            ((u32)p & 15u) !=
                0) { /* the firmware allocator promises no alignment: ask for room to round up */
            dk_fw_free(p);
            p = dk_fw_alloc(n + 16u);
            acc_fw = p != 0;
        }
    }
    return p;
}

static void res_free(void)
{
    acc_release();
    if (res_raw) {
        if (res_fw)
            dk_fw_free(res_raw);
        else
            dk_free(res_raw);
    }
    res_cap = 0;
    res_raw = 0;
}

/* Take the staging reservation (1 MiB, then 256 KiB) unless one is held. */
static void res_reserve(void)
{
    u32 want = ACC_BIG;
    if (res_raw)
        return;
    for (u32 t = 0; t < 2u; t++, want = ACC_MID) {
        u8 *p = acc_try(want);
        if (p) {
            res_fw = acc_fw;
            res_cap = want;
            res_raw = p;
            return;
        }
    }
}

/* A staging buffer for a file of `size` bytes: the size rounded up to 512, at most acc_limit and ACC_BIG; ACC_MID when that cannot be had; else none.
 * acc_limit applies to this BEGIN only: it is reset to ACC_BIG here. */
static void acc_open(u32 size)
{
    acc_release();
    file_size = size;
    u32 want = size > ACC_BIG ? ACC_BIG : (size + 511u) & ~511u;
    if (want > acc_limit)
        want = acc_limit & ~511u;
    acc_limit = ACC_BIG;
    if (res_raw && want >= 512u) { /* the reservation, as much of it as the file needs */
        acc_raw = res_raw;
        acc_fw = res_fw;
        acc_is_res = 1;
        if (want > res_cap)
            want = res_cap;
    }
    for (u32 t = 0; t < 2u && want >= 512u && !acc_raw; t++) {
        acc_raw = acc_try(want);
        if (!acc_raw) {
            if (want <= ACC_MID)
                break;
            want = ACC_MID;
        }
    }
    if (acc_raw) {
        acc_buf = (u8 *)(((u32)acc_raw + 15u) & ~15u);
        acc_cap = want;
        /* a file larger than half of the 1 MiB reservation: two halves of 512 KiB, so that the next frames are copied while the first half is written (the
         * 256 KiB tier keeps its one buffer: a 128 KiB write is slower per byte) */
        if (acc_is_res && res_cap == ACC_BIG && want > ACC_BIG / 2u) {
            acc_cap = ACC_BIG / 2u;
            acc_oth = acc_buf + acc_cap;
            ov = 1;
        }
    }
#if UF_KNOBS
    acc_got = acc_cap;
#endif
}

/* The copies of the overlap: dk_helper_run runs plan_job on the runtime's helper task while dk_files_write blocks the I/O task. */
#define PLAN_SEGS 48u
typedef struct {
    u8 *dst;
    const u8 *src;
    u32 n;
} plan_seg;
static struct {
    u32 n;
    plan_seg s[PLAN_SEGS];
} plan;
static void *plan_rel
    [PLAN_SEGS]; /* receive buffers whose bytes the plan reads: released after the helper is collected */
static u32 plan_rel_n;
static int plan_on;     /* pump() records the copies of DATA bodies instead of making them */
static int cur_planned; /* a segment reads the receive buffer `cur` */
static int plan_bad;    /* pump() met a bad header while planning: fatal_stream() after the write */

/* Runs on the helper task: pure compute, no import. */
static void plan_job(void *arg)
{
    (void)arg;
    for (u32 i = 0; i < plan.n; i++)
        copy(plan.s[i].dst, plan.s[i].src, plan.s[i].n);
}

static int pump(void);
static void fatal_stream(void);
static void lost_stream(i32 status);
static void reply(u8 cmd, u8 seq, i32 status, const u8 *body, u32 n);

/* Take the frames that are received already and carry a DATA body for the half that is being filled: pump() records their copies. A complete direct DATA frame is
 * finished here (accepted: acc_fill, reply); any other frame is left in `frame` for the loop. Stops when the half is full, the reply queue nears full or the plan is full. */
static void ov_plan_collect(void)
{
    plan.n = 0;
    plan_rel_n = 0;
    cur_planned = 0;
    plan_on = 1;
    while (rep_count < REPQ - 2u && acc_fill < acc_cap) {
        int p = pump();
        if (p < 0) {
            plan_bad = 1;
            break;
        }
        if (p == 0)
            break;
        if (frame[4] == CMD_DATA &&
            direct) { /* the checks of dispatch() were made when pump() decided `direct` */
            u32 n = total - (HDR + 4u);
            u8 seq = frame[5];
            acc_fill += n;
            st_frames++;
            frame_ready = 0;
            have = total = 0;
            dstage = direct = 0;
            reply(CMD_DATA, seq, 0, 0, 0);
            continue;
        }
        frame_ready = 1;
        st_frames++;
        break;
    }
    plan_on = 0;
}

/* The pending write, with the helper copying the next frames into the other half meanwhile when `with_plan`. A status; on 0 nothing is pending. E_BUSY keeps it. */
static i32 ov_write(int with_plan)
{
    int helper = 0;
    if (with_plan) {
        ov_plan_collect();
        if (plan.n) {
            if (dk_helper_run(plan_job, 0) == 0)
                helper = 1;
            else
                plan_job(0); /* the helper is busy or missing: copy here, before the write */
        }
    }
#if UF_TRACE
    u32 t0 = UF_NOW();
#endif
    i32 r = dk_files_write(files, handle, woff, wbuf, wlen);
    if (helper)
        dk_helper_wait();
    for (u32 i = 0; i < plan_rel_n; i++)
        dk_usb_bulk_release(ub, plan_rel[i]);
    plan_rel_n = plan.n = 0;
    cur_planned = 0;
    if (plan_bad) {
        plan_bad = 0;
        fatal_stream();
        return r;
    }
#if UF_TRACE
    uf_write += UF_NOW() - t0;
    uf_datas++;
    if (r == 0)
        uf_bytes += wlen;
#endif
    st_last_rc = (u32)r;
    if (r == 0) {
        wlen = 0;
        flushed = 1;
        sess_at = dk_time_blocks();
    }
    return r;
}

/* The full half becomes the pending write; the stream goes on in the other half. */
static void ov_push(void)
{
    wbuf = acc_buf;
    wlen = acc_fill;
    woff = acc_off;
    acc_off += acc_fill;
    acc_fill = 0;
    acc_buf = acc_oth;
    acc_oth = wbuf;
    w_giveup = 0;
}

/* One file_write of what the buffer holds (with `ov`: the pending half first, then the filling one). A status; on 0 the buffer is empty. */
static i32 acc_flush(void)
{
    if (ov) {
        if (wlen) {
            i32 r = ov_write(0);
            if (r != 0)
                return r;
        }
        if (!acc_fill)
            return 0;
        ov_push();
        return ov_write(0);
    }
    if (!acc_fill)
        return 0;
#if UF_TRACE
    u32 t0 = UF_NOW();
#endif
    i32 r = dk_files_write(files, handle, acc_off, acc_buf, acc_fill);
#if UF_TRACE
    uf_write += UF_NOW() - t0;
    uf_datas++;
    if (r == 0)
        uf_bytes += acc_fill;
#endif
    st_last_rc = (u32)r;
    if (r == 0) {
        acc_off += acc_fill;
        acc_fill = 0;
        flushed = 1;
        sess_at = dk_time_blocks();
    }
    return r;
}

static void clear_stream(void);
static void drop_file(void);

static void reply(u8 cmd, u8 seq, i32 status, const u8 *body, u32 n)
{
    if (rep_count >=
        REPQ) { /* the loop never parses with a full queue: this cannot happen; the host's view of the stream is lost, so start over */
        st_send_err++;
        clear_stream();
        drop_file();
        return;
    }
    u32 sr =
        sr_mask(); /* the tail index, the copy (at most 32 bytes) and the count are one masked step: the MIDI task's 'R' drain pops and flushes too */
    u32 at = (rep_head + rep_count) % REPQ;
    u8 *rep = repq[at];
    rep[0] = 'D';
    rep[1] = 'K';
    rep[2] = 'B';
    rep[3] = '1';
    rep[4] = (u8)(cmd | REPLY_FLAG);
    rep[5] = seq;
    rep[6] = (u8)((u32)status >> 8);
    rep[7] = (u8)status;
    dk_bytes_put32(rep + 8, n);
    for (u32 i = 0; i < n; i++)
        rep[HDR + i] = body[i];
    rep_n[at] = (u8)(HDR + n);
    rep_count++;
    sr_restore(sr);
}

/* The frame in `frame` is handled. Returns 0 when its reply is staged, 1 when a file_* call was busy and the same frame is run again. */
static int dispatch(void)
{
    u8 cmd = frame[4], seq = frame[5];
    u32 flags = ((u32)frame[6] << 8) | frame[7];
    u32 len = dk_bytes_get32(frame + 8);
    const u8 *body = frame + HDR;
    i32 r;

    if (cmd == CMD_INFO) {
        u8 b[20];
        u32 sr0 = sr_mask(); /* INFO starts a session: replies of an old one are dropped */
        rep_count = 0;
        sr_restore(sr0);
        served_seq =
            nonce_seq; /* the host's handshake is done: a later mode switch without a new SET_NONCE ends the session */
        b[0] = BULK_VERSION;
        b[1] = out_only() ? 1u : 0u; /* flags bit 0: replies over SysEx (`0x37 'R'` poll) */
        /* byte 2: the receive window in DATA frames of 64 KiB that the device takes before the USB controller NAKs (dk_usb_bulk_ring); 0: an older device, or the old
         * 8-slot buffers: the host keeps 2 in flight. At most REPQ - 2, so the reply queue always has room. */
        u32 win = (u32)DK_USB_BULK_RING_WINDOW(dk_usb_bulk_ring(ub, -1));
        b[2] = (u8)(win > REPQ - 2u ? REPQ - 2u : win);
        b[3] = 0;
        dk_bytes_put32(b + 4, DATA_MAX);
        dk_bytes_put32(b + 8, ALIGN);
        int stable = 0;
        for (u32 t = 0; t < 8 && !stable;
             t++) { /* SET_NONCE may be writing: read until an even, unchanged generation */
            u32 g = nonce_gen;
            if (g & 1u)
                continue;
            __asm__ volatile("" ::: "memory");
            for (u32 i = 0; i < NONCE_LEN; i++)
                b[12 + i] = nonce[i];
            __asm__ volatile("" ::: "memory");
            stable = g == nonce_gen;
        }
        if (!stable) {
            reply(cmd, seq, DK_FILE_E_BUSY, 0, 0);
            return 0;
        }
        reply(cmd, seq, 0, b, sizeof b);
        return 0;
    }
    if (cmd == CMD_BEGIN) {
        /* root u32, path (NUL-terminated), size u32, expected hash 4 bytes (all zero: none) */
        u32 p = 4;
        while (p < len && body[p] != 0)
            p++;
        if (len < 4 + 1 + 4 + 4 || p >= len || len - (p + 1) < 8) {
            reply(cmd, seq, E_MSG, 0, 0);
            return 0;
        }
        if (handle >
            0) { /* the module is the only bulk client: a new BEGIN means the host gave the old file up */
            r = dk_files_abort(files, handle);
            if (r == DK_FILE_E_BUSY) {
                if (++busy_tries < BEGIN_TRIES)
                    return 1;
                busy_tries = 0;
                reply(cmd, seq, DK_FILE_E_BUSY, 0, 0);
                return 0;
            }
            busy_tries = 0; /* 0, or E_STATE: the runtime had expired the session already */
            handle = 0;
            acc_release();
        }
        const u8 *h4 = body + p + 1 + 4;
        int none = (h4[0] | h4[1] | h4[2] | h4[3]) == 0;
#if UF_TRACE
        u32 t0 = UF_NOW();
        if (busy_tries == 0) /* a retry of the same BEGIN keeps the totals */
            uf_write = uf_copy = uf_wait = uf_begin = uf_end = uf_bytes = uf_datas = 0;
#endif
        r = dk_files_create(files, dk_bytes_get32(body), (const char *)(body + 4),
                            dk_bytes_get32(body + p + 1), none ? 0 : h4,
                            ((flags & FLAG_REPLACE) ? DK_FILE_REPLACE : 0u) |
                                ((flags & FLAG_DEFER) ? DK_FILE_DEFER : 0u));
#if UF_TRACE
        uf_begin += UF_NOW() - t0;
        uf_wait_from = UF_NOW();
        uf_waiting = 1;
#endif
        if (r == DK_FILE_E_BUSY && ++busy_tries < BEGIN_TRIES)
            return 1;
        busy_tries = 0;
        if (r > 0) {
            handle = r;
            r = 0;
            sess_at = dk_time_blocks();
            acc_open(dk_bytes_get32(body + p + 1));
#if UF_TRACE
            uf_log("uf.acc.kib", acc_cap >> 10);
#endif
        }
        reply(cmd, seq, r, 0, 0);
        return 0;
    }
    if (cmd == CMD_DATA) {
        if (len < 4) {
            reply(cmd, seq, E_MSG, 0, 0);
            return 0;
        }
        if (handle <= 0) {
            reply(cmd, seq, E_STATE, 0, 0);
            return 0;
        }
        if (acc_cap) {
            u32 n = len - 4, off = dk_bytes_get32(body);
            if (acc_err) {
                reply(cmd, seq, acc_err, 0, 0);
                return 0;
            }
            if (!d_acc) {
                if (off != acc_off + acc_fill) {
                    reply(cmd, seq, DK_FILE_E_OFFSET, 0, 0);
                    return 0;
                }
                if (off > file_size || n > file_size - off ||
                    ((n & 511u) != 0 && n != file_size - off)) {
                    reply(cmd, seq, DK_FILE_E_LENGTH, 0, 0);
                    return 0;
                }
                if (!direct &&
                    n > acc_cap -
                            acc_fill) { /* the frame does not fit: flush first (nothing of the frame is taken yet) */
                    r = acc_flush();
                    if (r == DK_FILE_E_BUSY && ++busy_tries < BUSY_TRIES)
                        return 1;
                    busy_tries = 0;
                    if (r != 0) {
                        if (r != DK_FILE_E_BUSY)
                            acc_err = r;
                        if (r == DK_FILE_E_STATE) {
                            handle = 0;
                            acc_release();
                        }
                        reply(cmd, seq, r, 0, 0);
                        return 0;
                    }
                }
                if (!direct)
                    copy(acc_buf + acc_fill, body + 4, n);
                acc_fill += n;
                d_acc = 1;
            }
            r = 0;
            /* With buffering the runtime's 20 s idle expiry counts from the last flush: a host slower than ~50 KB/s gets E_STATE on the flushing DATA
             * (handled: the file is dropped, nothing partial is placed). */
            if (!ov &&
                acc_fill ==
                    acc_cap) { /* full: this DATA carries the flush (with `ov` the loop writes the half: ov_service) */
                r = acc_flush();
                if (r == DK_FILE_E_BUSY && ++busy_tries < BUSY_TRIES)
                    return 1;
                busy_tries = 0;
                if (r == DK_FILE_E_BUSY)
                    r = 0; /* accepted; the next DATA or END flushes first */
                else if (r != 0)
                    acc_err = r;
            }
            d_acc = 0;
#if UF_TRACE
            uf_wait_from = UF_NOW();
            uf_waiting = 1;
#endif
            if (r == DK_FILE_E_STATE) { /* the runtime ended the session (no write for 20 s) */
                handle = 0;
                acc_release();
            }
            reply(cmd, seq, r, 0, 0);
            return 0;
        }
        /* frame is 16-byte aligned and the data starts at HDR + 4 = 16: file_write gets an aligned pointer */
#if UF_TRACE
        u32 t0 = UF_NOW();
#endif
        r = dk_files_write(files, handle, dk_bytes_get32(body), body + 4, len - 4);
#if UF_TRACE
        uf_write += UF_NOW() - t0;
        if (r == 0) {
            uf_bytes += len - 4;
            uf_datas++;
        }
        uf_wait_from = UF_NOW();
        uf_waiting = 1;
#endif
        st_last_rc = (u32)r;
        if (r == DK_FILE_E_BUSY && ++busy_tries < BUSY_TRIES)
            return 1;
        busy_tries = 0;
        if (r == 0)
            sess_at = dk_time_blocks();
        if (r == DK_FILE_E_STATE) /* the runtime ended the session (no write for 20 s) */
            handle = 0;
        reply(cmd, seq, r, 0, 0);
        return 0;
    }
    if (cmd == CMD_END) {
        u8 hash[4] = {0, 0, 0, 0};
        if (handle <= 0) {
            reply(cmd, seq, E_STATE, 0, 0);
            return 0;
        }
        if (acc_err) { /* the host aborts */
            reply(cmd, seq, acc_err, 0, 0);
            return 0;
        }
        if (acc_fill ||
            wlen) { /* the tail: the one write that may end in a part sector (with `ov`, a pending half first) */
            r = acc_flush();
            if (r == DK_FILE_E_BUSY && ++busy_tries < BUSY_TRIES)
                return 1;
            busy_tries = 0;
            if (r != 0) {
                if (r != DK_FILE_E_BUSY)
                    acc_err = r;
                if (r == DK_FILE_E_STATE) {
                    handle = 0;
                    acc_release();
                }
                reply(cmd, seq, r, 0, 0);
                return 0;
            }
        }
#if UF_TRACE
        u32 t0 = UF_NOW();
#endif
        r = dk_files_close(files, handle, hash);
#if UF_TRACE
        uf_end += UF_NOW() - t0;
#endif
        if (r == DK_FILE_E_BUSY && ++busy_tries < BUSY_TRIES)
            return 1;
        busy_tries = 0;
#if UF_TRACE
        if (r != DK_FILE_E_BUSY) {
            uf_log("uf.t.write.blk", uf_write);
            uf_log("uf.t.copy.blk", uf_copy);
            uf_log("uf.t.wait.blk", uf_wait);
            uf_log("uf.t.begin.blk", uf_begin);
            uf_log("uf.t.end.blk", uf_end);
            uf_log("uf.bytes", uf_bytes);
            uf_log("uf.datas", uf_datas);
        }
#endif
        if (r != DK_FILE_E_BUSY) { /* the session is closed whatever the result */
            handle = 0;
            acc_release();
        }
        /* body: hash4 | pending u16 | reserved u16. `pending` is the files not yet named in the store (0: everything sent so far is committed); an old device sends
         * the 4 bytes only, which the host reads as committed. */
        if (r == 0 || r == DK_FILE_E_HASH) {
            u8 body8[8] = {hash[0], hash[1], hash[2], hash[3], 0, 0, 0, 0};
            i32 pend = dk_files_pending(files);
            if (pend > 0) {
                body8[4] = (u8)((u32)pend >> 8);
                body8[5] = (u8)pend;
            }
            reply(cmd, seq, r, body8, 8u);
        } else {
            reply(cmd, seq, r, 0, 0);
        }
        return 0;
    }
    if (cmd == CMD_COMMIT) {
        /* name every pending file in one table commit; reply body: pending u16 | reserved u16 (0 on success). A COMMIT with nothing pending is ok. */
        u8 body4[4] = {0, 0, 0, 0};
        r = dk_files_commit(files);
        if (r == DK_FILE_E_BUSY && ++busy_tries < BUSY_TRIES)
            return 1;
        busy_tries = 0;
        i32 pend = dk_files_pending(files);
        if (pend > 0) {
            body4[0] = (u8)((u32)pend >> 8);
            body4[1] = (u8)pend;
        }
        reply(cmd, seq, r, body4, 4u);
        return 0;
    }
    if (cmd == CMD_ABORT) {
        if (handle <= 0) { /* nothing open: ABORT is the host's way to be sure */
            reply(cmd, seq, 0, 0, 0);
            return 0;
        }
        r = dk_files_abort(files, handle);
        if (r == DK_FILE_E_BUSY && ++busy_tries < BUSY_TRIES)
            return 1;
        busy_tries = 0;
        if (r == 0 ||
            r ==
                DK_FILE_E_STATE) { /* only then is the session gone; on any other error the host asks again */
            handle = 0;
            acc_release();
        }
        reply(cmd, seq, r == DK_FILE_E_STATE ? 0 : r, 0, 0);
        return 0;
    }
    /* CMD_VERIFY is reserved, and so is every other number */
    reply(cmd, seq, E_PROTO, 0, 0);
    return 0;
}

/* Forget what was half received: the parser starts a new stream (a bus reset, or a send error). */
/* `drain`: also release what the adapter holds for the job (the stream is lost mid-transfer). After a bus reset nothing is drained: the adapter dropped every buffer of
 * the old connection, so what is left came after the reset (the host's next INFO). */
static void clear_stream_from(int drain)
{
    void *b;
    u32 n;
    have = total = 0;
    frame_ready = 0;
    dstage = direct = 0;
    skipping = 0;
    if (cur) {
        dk_usb_bulk_release(ub, cur);
        cur = 0;
    }
    while (drain && dk_usb_bulk_take(ub, &b, &n) != 0)
        dk_usb_bulk_release(ub, b);
}

static void clear_stream(void)
{
    clear_stream_from(1);
}

/* Abort the open file (the job does it in a later step: file_abort may be busy). */
static void drop_file(void)
{
    acc_release();
    if (handle > 0) {
        abort_h = handle;
        handle = 0;
    }
}

/* The stream is lost (corrupt: bad magic, a length over the bound, E_MSG; or the session expired under a half-received frame, E_STATE): abort the open file, reply `status` only when the magic was valid (the seq can be trusted),
 * and discard the rest of the current USB transfer: a buffer of less than 16 KiB ends it. */
static void lost_stream(i32 status)
{
    if (have >= HDR && frame[0] == 'D' && frame[1] == 'K' && frame[2] == 'B' && frame[3] == '1')
        reply(frame[4], frame[5], status, 0, 0);
    have = total = 0;
    frame_ready = 0;
    dstage = direct = 0;
    skipping =
        last_len >=
        DK_USB_BULK_RX_BYTES; /* the buffer with the bad header was full: the transfer goes on in the next one */
    if (cur) {
        dk_usb_bulk_release(ub, cur); /* the rest of this buffer is garbage too */
        cur = 0;
    }
    drop_file();
}

static void fatal_stream(void)
{
    st_fatal++;
    lost_stream(E_MSG);
}

/* Copy bytes from the receive buffers into the frame. 1: a whole frame is in `frame`; 0: no more bytes now; -1: the stream is corrupt. */
static int pump(void)
{
    for (;;) {
        if (total && have == total)
            return 1;
        if (plan_on && (plan.n >= PLAN_SEGS - 2u || plan_rel_n >= PLAN_SEGS - 1u))
            return 0; /* the plan is full: the rest follows after the write */
        if (!cur) {
            if (dk_usb_bulk_take(ub, &cur, &cur_len) == 0) {
                cur = 0;
                return 0;
            }
            cur_off = 0;
            cur_planned = 0;
            last_len = cur_len;
            st_bufs++;
            st_bytes += cur_len;
        }
        if (skipping) {
            dk_usb_bulk_release(ub, cur);
            cur = 0;
            if (last_len < DK_USB_BULK_RX_BYTES)
                skipping = 0; /* a short buffer was the end of the transfer */
            continue;
        }
        u32 want = total ? total : HDR;
        u8 *dst = frame + have;
        if (dstage == 1)
            want = HDR + 4u; /* the offset first: it decides where the body goes */
        else if (dstage == 2)
            dst = acc_buf + acc_fill + (have - (HDR + 4u));
        u32 n = cur_len - cur_off;
        if (n > want - have)
            n = want - have;
#if UF_TRACE
        u32 t0 = UF_NOW();
        if (uf_waiting) { /* the host has delivered the first bytes of the next request: its stall ends here, the copy is counted apart */
            uf_wait += t0 - uf_wait_from;
            uf_waiting = 0;
        }
#endif
        if (plan_on &&
            dstage ==
                2) { /* the body goes to the other half on the helper task, while the pending write runs */
            plan.s[plan.n].dst = dst;
            plan.s[plan.n].src = (const u8 *)cur + cur_off;
            plan.s[plan.n].n = n;
            plan.n++;
            cur_planned = 1;
        } else {
            copy(dst, (const u8 *)cur + cur_off, n);
        }
#if UF_TRACE
        uf_copy += UF_NOW() - t0;
#endif
        have += n;
        cur_off += n;
        if (cur_off >= cur_len) {
            if (plan_on && cur_planned)
                plan_rel[plan_rel_n++] = cur; /* a segment still reads it */
            else
                dk_usb_bulk_release(ub, cur);
            cur = 0;
        }
        if (dstage == 1 && have == HDR + 4u) {
            u32 off = dk_bytes_get32(frame + HDR), body_n = total - (HDR + 4u);
            direct = body_n > 0 && off == acc_off + acc_fill && body_n <= acc_cap - acc_fill &&
                     off <= file_size && body_n <= file_size - off &&
                     ((body_n & 511u) == 0 || body_n == file_size - off);
            dstage = direct ? 2 : 0;
        }
        if (!total && have == HDR) {
            u32 len = dk_bytes_get32(frame + 8);
            if (frame[0] != 'D' || frame[1] != 'K' || frame[2] != 'B' || frame[3] != '1')
                return -1;
            if (len > DATA_MAX + 4u + SLACK)
                return -1;
            total = HDR + len;
            dstage =
                (frame[4] == CMD_DATA && acc_cap && handle > 0 && !acc_err && len >= 4u) ? 1 : 0;
        }
    }
}

/* The overlap's write, from the step loop: the pending half goes out (a full half is pushed first), with the copies of the received frames running on the helper meanwhile.
 * 1: E_BUSY, run the step again; 2: a half was written; 0: nothing to do. */
static int ov_service(void)
{
    if (!ov || handle <= 0 || acc_err || frame_ready)
        return 0;
    if (!wlen) {
        if (acc_fill != acc_cap)
            return 0;
        ov_push();
    }
    if (w_giveup)
        return 0;
    i32 r = ov_write(1);
    if (handle <= 0 || !ov) /* the stream was bad: fatal_stream dropped the file */
        return 0;
    if (r == DK_FILE_E_BUSY) {
        if (++busy_tries < BUSY_TRIES)
            return 1;
        busy_tries = 0;
        w_giveup = 1; /* accepted; the next DATA or END writes it first */
        return 0;
    }
    busy_tries = 0;
    if (r != 0) {
        acc_err = r;
        if (r == DK_FILE_E_STATE) { /* the runtime ended the session (no write for 20 s) */
            /* A DATA frame may be half received (dstage 2: the rest of its body goes into the buffer that is released here). Drop the frame and
             * skip the rest of its transfer, as a bad stream does: the frame gets E_STATE, the file is gone, the next frames get E_STATE too. */
            handle = 0;
            lost_stream(E_STATE);
        }
        return 0;
    }
    flushed = 0;
    return 2;
}

/* End the bulk session: the stream, the open file and the replies are void, the buffers go back and the next enable allocates them again. */
static void end_session(void)
{
    dk_usb_bulk_disable(ub, job_id);
    enabled = 0;
    wm_on = 0;
    clear_stream();
    u32 sr = sr_mask();
    rep_count = 0;
    sr_restore(sr);
    busy_tries = 0;
    drop_file();
    res_free();
}

static i32 step(void *arg)
{
    (void)arg;
    st_steps++;
    u32 budget = STEP_BUDGET;
    /* A USB mode change voids what was half done; the adapter keeps its buffers until the disable. A port with no bulk route for WRONG_MODE_BLOCKS ends the
     * session here; so does a switch between the pair and OUT-only (the host link is another one) and any loss of the buffers, at once. */
    u32 st0 = dk_usb_bulk_state(ub);
    int flip = enabled && ((st0 & DK_USB_BULK_OUT_ONLY) != 0u) != sess_oo;
    if (flip && nonce_seq != served_seq) {
        /* the host sent SET_NONCE for the new link and its INFO may be in the ring already: the session goes on in the new mode (the switch's bus reset voided
         * the rest). With no handshake pending the ring holds nothing of the new host, so the session ends. */
        sess_oo = !sess_oo;
        flip = 0;
    }
    if (enabled && (!(st0 & DK_USB_BULK_LIVE) || (st0 & DK_USB_BULK_WRONG_MODE) || flip)) {
        u32 now = dk_time_blocks();
        if (!wm_on) {
            wm_on = 1;
            wm_at = now;
            clear_stream();
            u32 sr = sr_mask();
            rep_count = 0;
            sr_restore(sr);
            busy_tries = 0;
            drop_file();
        }
        if ((st0 & DK_USB_BULK_WRONG_MODE) && (st0 & DK_USB_BULK_LIVE) &&
            now - wm_at < WRONG_MODE_BLOCKS)
            return DK_IO_WAIT; /* tick wakes the job */
        wm_logged = 1;         /* the wait for the next bulk mode is not news */
        end_session(); /* served_seq stays: a nonce sent for the new mode (OUT-only) opens the next session */
    } else {
        wm_on = 0;
    }
    if (!enabled) {
        u32 want = nonce_seq;
        if (out_only() &&
            want == served_seq) /* OUT-only: nothing is allocated until a host asks (SET_NONCE) */
            return DK_IO_WAIT;
        st0 = dk_usb_bulk_state(ub);
        if (!(st0 & DK_USB_BULK_WRONG_MODE))
            res_reserve(); /* before the adapter allocates its ring */
        else
            res_free(); /* wrong mode: nothing to keep */
        i32 rc = dk_usb_bulk_enable(ub, job_id);
        if (rc == DK_USB_BULK_E_NOMEM &&
            res_raw) { /* the staging reservation may be what the ring lacks: enable without it (acc_open takes a buffer per file when memory allows) */
            res_free();
            rc = dk_usb_bulk_enable(ub, job_id);
        }
        enable_rc = rc;
        if (rc ==
            DK_USB_BULK_E_NOMEM) { /* no memory for the buffers (the adapter's ST_NOMEM): try again at the next wake */
            if (!nomem_logged) {
                nomem_logged = 1;
                dk_log(DK_LOG_WARN, "usb-fast: no memory for the USB buffers", (u32)rc);
            }
            return DK_IO_WAIT;
        }
        nomem_logged = 0;
        if (rc == DK_USB_BULK_E_STATE) {
            /* Not in a bulk mode, or a mode switch is settling. tick wakes the job about four times a second: it enables as soon as the device is in a bulk
             * mode (the user's USB CONFIG). Only a real wrong mode is logged, once. */
            u32 st = dk_usb_bulk_state(ub);
            if ((st & DK_USB_BULK_WRONG_MODE) && !wm_logged) {
                wm_logged = 1;
                dk_log(DK_LOG_INFO, "usb-fast: not in a bulk USB mode; bulk waits", (u32)rc);
            }
            return DK_IO_WAIT;
        }
        if (rc < 0) {
            dk_log(DK_LOG_ERROR, "usb-fast: enable failed", (u32)rc);
            return rc;
        }
        enabled = 1;
        sess_oo = out_only();
        served_seq = want;
        wm_logged = 0;
        epoch = DK_USB_BULK_RESETS(dk_usb_bulk_state(ub));
        return DK_IO_WAIT;
    }
    for (;;) {
        u32 now = DK_USB_BULK_RESETS(dk_usb_bulk_state(ub));
        if (now != epoch) { /* a bus reset: what was half done is void, the host starts again */
            epoch = now;
            st_resets++;
            clear_stream_from(0);
            u32 sr = sr_mask();
            rep_count = 0;
            sr_restore(sr);
            busy_tries = 0;
            drop_file();
        }
        if (out_only() &&
            dk_time_blocks() !=
                0u) { /* OUT-only: an idle session is closed, so nothing stays allocated while the user only plays audio */
            u32 act = nonce_at;
            if ((i32)(sess_at - act) > 0)
                act = sess_at;
            if (dk_time_blocks() - act >= SESS_IDLE_BLOCKS) {
                st_expired++;
                served_seq =
                    nonce_seq; /* a nonce seen during the session does not open it again: the next session is for the next nonce */
                end_session(); /* clear_stream, then drop_file */
                return DK_IO_WAIT;
            }
        }
        if (abort_h > 0) {
            i32 r = dk_files_abort(files, abort_h);
            if (r ==
                DK_FILE_E_BUSY) { /* the abort stays pending: it is tried again at the next step */
                if (++busy_tries < BUSY_TRIES)
                    return DK_IO_MORE;
            } else
                abort_h = 0;
            busy_tries = 0;
        }
        while (
            rep_count &&
            !out_only()) { /* in OUT-only mode the replies wait for the host's 'R' poll, which wakes the job */
            i32 r = dk_usb_bulk_send(ub, repq[rep_head], rep_n[rep_head]);
            if (r ==
                DK_FILE_E_BUSY) { /* both send slots in flight: the adapter wakes the job; meanwhile the stream goes on */
                st_tx_busy++;
                break;
            }
            if (r ==
                DK_USB_BULK_E_AGAIN) /* the engine refused and nothing is in flight: the adapter's tick wakes the job */
                break;
            if (r <
                0) { /* the reply cannot go out: the host's view of the stream is lost, so start over */
                st_send_err++;
                u32 sr = sr_mask();
                rep_count = 0;
                sr_restore(sr);
                clear_stream();
                drop_file();
                break;
            }
            u32 sr = sr_mask();
            if (rep_count) { /* re-checked under the mask: never below 0 */
                rep_head = (rep_head + 1u) % REPQ;
                rep_count--;
            }
            sr_restore(sr);
        }
        if (rep_count >=
            REPQ -
                1u) /* full: nothing is parsed until a reply has gone out (the completion of a send wakes the job) */
            return DK_IO_WAIT;
        if (budget == 0)
            return DK_IO_MORE; /* the other I/O jobs get a turn; the replies are sent above */
        if (ov) {
            int sv = ov_service();
            if (sv == 1)
                return DK_IO_MORE;
            if (sv ==
                2) { /* a write ended the step; the replies of the frames it took go out first */
                budget = 0;
                continue;
            }
        }
        if (frame_ready) {
            if (dispatch())
                return DK_IO_MORE;
            frame_ready = 0;
            have = total = 0;
            dstage = direct = 0;
            budget = flushed ? 0u : budget - 1u;
            flushed = 0;
            continue; /* send the reply first */
        }
        int p = pump();
        if (p < 0) {
            fatal_stream();
            continue;
        }
        if (p == 0)
            return DK_IO_WAIT;
        frame_ready = 1;
        st_frames++;
    }
}

static void finished(void *arg, i32 status)
{
    (void)arg;
    (void)status;
}

/* STATUS (0x37) sub-commands by the first body byte: one line per tag (the speed levers' lanes each add theirs). A handler returns the reply length, or a status. */
typedef i32 (*status_fn)(const u8 *body, u32 len, u8 *out);

/* 'S' [bit [0|1]]: the four speed switches (RAM, all on at boot, never saved): bit 0 hash once and bit 2 read-back hash overlap (the runtime's speed word),
 * bit 1 the USB receive ring (core/usb-bulk, `dk_usb_bulk_ring`), bit 3 large card commands (core/sdhc-large, `dk_sdhc_large_ctl`). Each adapter owns its switch; the
 * mask is composed here. Reply: the mask now (u32; a switch of an adapter that is not in the image reads 0). With no bit, only the read. E_MSG for a bit
 * above 3 or a value other than 0 or 1; E_STATE when a switch of an adapter is set and the adapter is not in the image. */
static u32 speed_mask(void)
{
    u32 m = dk_debug_speed_flags(0, 0) & 5u;
    if (dk_usb_bulk_ring(ub, -1) & 1)
        m |= 2u;
    if (dk_sdhc_large_ctl(sl, 0, 2))
        m |= 8u; /* the value 2 changes nothing: it reads the switch */
    return m;
}

static i32 status_speed(const u8 *body, u32 len, u8 *out)
{
    if (len >= 3) {
        u32 bit = body[1], v = body[2];
        if (bit > 3u || v > 1u)
            return E_MSG;
        if (bit == 1u) {
            if (dk_usb_bulk_ring(ub, (i32)v) == 0) /* the SDK's stub: this adapter has no ring */
                return E_STATE;
        } else if (bit == 3u) {
            /* the stub without the module returns 0 for every call: the switch is there only if it reads 1 after it was set to 1 (then it gets the value asked) */
            if (!dk_sdhc_large_ctl(sl, 0, 2)) {
                dk_sdhc_large_ctl(sl, 0, 1);
                if (!dk_sdhc_large_ctl(sl, 0, 2))
                    return E_STATE;
            }
            dk_sdhc_large_ctl(sl, 0, v);
        } else if (v) {
            dk_debug_speed_flags(0, 1u << bit);
        } else {
            dk_debug_speed_flags(1u << bit, 0);
        }
    }
    dk_bytes_put32(out, speed_mask());
    return 4;
}

/* 'R': the host's poll for replies in OUT-only mode. Reply: the oldest queued reply frames, whole and in order, as many as fit in 879 bytes (the SysEx
 * reply body); length 0 when none is queued. They are removed from the queue; the job is woken, as it may have waited with the queue nearly full. */
#define REPLY_POLL_MAX 879u
static i32 status_replies(const u8 *body, u32 len, u8 *out)
{
    (void)body;
    (void)len;
    u32 n = 0;
    if (!out_only())
        return 0;
    for (;;) {
        u32 sr = sr_mask();
        if (!rep_count || n + rep_n[rep_head] > REPLY_POLL_MAX) {
            sr_restore(sr);
            break;
        }
        u32 h = rep_head, m = rep_n[h];
        for (u32 i = 0; i < m; i++)
            out[n + i] = repq[h][i];
        rep_head = (h + 1u) % REPQ;
        rep_count--;
        sr_restore(sr);
        n += m;
    }
    if (n)
        dk_io_wake(job_id);
    return (i32)n;
}

static const struct {
    u8 tag;
    status_fn fn;
} status_tab[] = {
    {'S', status_speed},
    {'R', status_replies},
};

/* SysEx service command USB_FAST_NONCE (0x36): body nonce[8], the host's proof of which USB device is on its MIDI port; reply empty. The INFO reply of the
 * bulk link returns it. 0x37 STATUS (a debug aid: twenty u32 words). */
static i32 command(u32 cmd, u32 seq, const void *request, u32 len, void *reply_out)
{
    const u8 *body = request;
    u8 *out = reply_out;
    (void)seq;
    if (cmd == NONCE_CMD + 1 && len > 0) {
        for (u32 i = 0; i < sizeof status_tab / sizeof status_tab[0]; i++)
            if (body[0] == status_tab[i].tag)
                return status_tab[i].fn(body, len, out);
    }
#if UF_KNOBS
    if (cmd == NONCE_CMD + 1 && len > 0 &&
        body[0] ==
            'A') { /* test knob: 'A' [max bytes u32] sets the largest staging buffer of the next BEGIN (0: none); replies the size of the last one */
        if (len >= 5)
            acc_limit = dk_bytes_get32(body + 1);
        dk_bytes_put32(out, acc_got);
        return 4;
    }
#endif
#if UF_BENCH
    if (cmd == NONCE_CMD + 1 && len > 0)
        return uf_bench_command(body, len, out);
#endif
    if (cmd == NONCE_CMD + 1 && len > 0)
        return E_MSG; /* an unknown tag */
    if (cmd ==
        NONCE_CMD + 1) { /* STATUS (a debug aid): what the job has done and where it stands */
        dk_bytes_put32(out, st_steps);
        dk_bytes_put32(out + 4, st_frames);
        dk_bytes_put32(out + 8, st_tx_busy);
        dk_bytes_put32(out + 12, (u32)handle);
        dk_bytes_put32(out + 16, rep_count);
        dk_bytes_put32(out + 20, have);
        dk_bytes_put32(out + 24, total);
        dk_bytes_put32(out + 28, (frame_ready ? 1u : 0u) | (cur ? 2u : 0u) | (enabled ? 4u : 0u));
        dk_bytes_put32(out + 32, dk_usb_bulk_state(ub));
        dk_bytes_put32(out + 36, st_last_rc);
        dk_bytes_put32(out + 40, st_bufs);
        dk_bytes_put32(out + 44, st_bytes);
        dk_bytes_put32(out + 48, st_fatal);
        dk_bytes_put32(out + 52, st_resets);
        dk_bytes_put32(out + 56, (u32)enable_rc);
        dk_bytes_put32(out + 60, st_send_err);
        dk_bytes_put32(out + 64, acc_cap);
        dk_bytes_put32(out + 68, st_expired);
        dk_bytes_put32(out + 72,
                       res_cap); /* the staging reservation (0: none), for the hardware A/B */
        dk_bytes_put32(
            out + 76,
            (u32)(dk_usb_bulk_ring(ub, -1) >> 16) &
                0xffu); /* receive ring slots of the live block (0: none live, or no ring) */
        return 80;
    }
    if (cmd != NONCE_CMD)
        return E_PROTO;
    if (len != NONCE_LEN)
        return E_MSG;
    nonce_gen++; /* odd: INFO does not trust the bytes */
    __asm__ volatile("" ::: "memory");
    for (u32 i = 0; i < NONCE_LEN; i++)
        nonce[i] = body[i];
    __asm__ volatile("" ::: "memory");
    nonce_gen++;
    nonce_at = dk_time_blocks();
    nonce_seq++;
    dk_io_wake(
        job_id); /* OUT-only: the job enables the session before the host's INFO arrives (the controller NAKs it until then) */
    return 0;
}

static void fini(dk_module_ctx *ctx)
{
    (void)ctx;
    dk_usb_bulk_disable(ub, job_id);
#if UF_BENCH
    uf_bench_fini();
#endif
    dk_io_cancel(job_id); /* the runtime aborts a file session the job leaves open */
    res_free();
}

static i32 init(dk_module_ctx *ctx)
{
    i32 rc = dk_files_open(ctx, &files);
    if (rc != DK_OK)
        return rc;
    rc = dk_usb_bulk_open(ctx, &ub);
    if (rc != DK_OK)
        return rc;
    if (dk_sdhc_large_open(ctx, &sl) != DK_OK)
        sl = 0; /* no core/sdhc-large: the call stub returns 0 and the switch reads off */
    frame = (u8 *)(((u32)frame_raw + 15u) & ~15u);
    job_id = dk_io_post(step, finished, 0);
    if (job_id <= 0)
        return job_id ? job_id : -1;
#if UF_BENCH
    i32 bench = uf_bench_init(files);
    if (bench != 0) {
        dk_io_cancel(job_id);
        return bench;
    }
#endif
    rc = dk_host_register(NONCE_CMD, NONCE_RANGE, command);
    if (rc != DK_OK)
        dk_io_cancel(job_id);
    return rc;
}

/* The module's own timer (once per UI pass): while the job waits for a bulk mode, a nonce or a retry, wake it about four times a second. */
static void tick(dk_module_ctx *ctx)
{
    (void)ctx;
    u32 now = dk_time_blocks();
    if ((enabled && !wm_on) || now - tick_at < TICK_BLOCKS)
        return;
    tick_at = now;
    dk_io_wake(job_id);
}

const dk_module_def dk_module = {.init = init, .fini = fini, .tick = tick};
