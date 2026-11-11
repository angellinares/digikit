/* digikit/usb-fast: file uploads on the USB bulk interface of core/usb-bulk (docs/plans/usb-fast.md section 5, usb-fast-hardware.md "Step 4").
 *
 * One I/O job (<dk/io.h>) enables the interface, takes the received buffers, cuts the byte stream into frames by their own length field, runs each
 * request on file_create / file_write / file_close / file_abort (<dk/file.h>) and sends the reply on EP2 IN. The numbers are those of sdk/sdk.toml
 * [protocol.bulk] (request: 'DKB1' | cmd u8 | seq u8 | flags u16 | len u32 | body; reply: 'DKB1' | cmd|0x40 | seq | status i16 | len u32 | body;
 * big-endian). SysEx stays the runtime's channel for everything else; this module adds three SysEx commands: SET_NONCE (0x36), by which the host proves
 * which USB device is on its MIDI port, and TAKEOVER (0x1e) and RELEASE (0x1f), by which the host has core/usb-bulk switch a device in USB AUDIO/MIDI to USB MIDI
 * for a transfer and back (docs/plans/usb-fast-takeover.md). The job calls usb_bulk_enable at its start and, while the device is not in USB MIDI mode (or a mode
 * switch is settling), again at every wake of the adapter (about four a second): it enables when the take-over, or the user's USB CONFIG, brings USB MIDI mode.
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
 * gets E_BUSY and asks again. A change of the reset count of usb_bulk_state (a bus reset or a disconnect) aborts the open file and clears the parser. A
 * usb_bulk_send error other than busy or E_AGAIN drops the reply, the open file and the parser (counted in STATUS).
 *
 * Accumulation: at BEGIN the job tries to get a staging buffer (1 MiB, then 256 KiB, never more than the file needs; sdk->alloc, then fw_alloc) and
 * keeps the DATA bytes in it in order. A DATA reply means "accepted": when the buffer is full, one file_write of the whole buffer goes out and that DATA
 * carries its status. A failed flush is sticky: every later DATA and END return it (the host aborts). A flush that stays E_BUSY after its tries is not
 * reported (the bytes are accepted): the next DATA or END flushes first and answers E_BUSY while it cannot. END flushes the tail with one more
 * file_write (the store takes a length that is not a multiple of 512 only at the end of the file), then closes. pump() copies a DATA body straight
 * from the receive buffer into the staging buffer when it fits in the room left (one copy); else the body goes through `frame` and dispatch() copies
 * it. No buffer: one file_write per DATA, from `frame`. The buffer is freed at END, abort, bus reset, send error and unload. The END hash check
 * guards the result.
 *
 * VERIFY is reserved: the reply is E_PROTO (the host verifies over SysEx: both devices are CPU bound there with tiny replies). */
/* the code is placed as it is: no jump tables (data in the code) */
#pragma GCC optimize("no-jump-tables")
#include <dk/api.h>
#include <dk/file.h>
#include <dk/hash32.h>
#include <dk/io.h>
#include <dk/usb_bulk.h>
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
#define CMD_INFO 1u
#define CMD_BEGIN 2u
#define CMD_DATA 3u
#define CMD_END 4u
#define CMD_ABORT 5u
#define CMD_VERIFY 6u /* reserved: E_PROTO */
#define BULK_VERSION 1u
/* the largest body is a DATA request: offset u32 + data_max, plus the slack that BEGIN's path needs */
#define FRAME_MAX (HDR + 4u + DATA_MAX + SLACK)
/* [protocol.service] USB_FAST_NONCE; [protocol.ranges] usb_fast = [0x36, 0x37] */
#define NONCE_CMD 0x36u
#define NONCE_RANGE 2u
/* [protocol.service] USB_FAST_TAKEOVER and USB_FAST_RELEASE; [protocol.ranges] usb_fast_takeover = [0x1e, 0x1f] */
#define TAKEOVER_CMD 0x1eu
#define RELEASE_CMD 0x1fu
#define TAKEOVER_RANGE 2u
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
/* The runtime drops a write session that has had no BEGIN or write for 20 s (1500 blocks a second); the job frees its staging buffer 1 s later. */
#define SESS_IDLE_BLOCKS (21u * 1500u)

static const dk_api *sdk;
static i32 job_id;
static int enabled;
static int nomem_logged;
static u32 epoch;

static u8 nonce[NONCE_LEN];
static volatile u32 nonce_gen; /* a seqlock: odd while SET_NONCE writes (MIDI task), INFO (I/O task) reads until it sees an even, unchanged value */

/* the stream parser */
static u8 frame_raw[FRAME_MAX + 16u];
static u8 *frame; /* frame_raw rounded up to 16 bytes at run time: the loader's bss alignment is not relied on */
static u32 have;  /* bytes of the frame in `frame` */
static u32 total; /* its length once the header is in, else 0 */
static u8 *cur;   /* the receive buffer being copied, or 0 */
static u32 cur_len, cur_off;
static int frame_ready;
static int skipping; /* after a fatal error: discard buffers until the end of the transfer */
static u32 last_len; /* length of the last buffer taken */

/* the reply waiting for the engine */
static u8 rep[HDR + 20u];
static u32 rep_len;

/* the file */
static i32 handle; /* > 0 while a file is open */
static u32 busy_tries;
static u32 st_steps, st_frames, st_tx_busy, st_last_rc, st_bufs, st_bytes, st_fatal, st_resets, st_send_err;
static i32 enable_rc;
static int wm_logged;  /* the wrong-mode line is logged once per wait */
static u32 sess_at;    /* the block clock at the last file_create or successful file_write of the open file */
static u32 st_expired; /* sessions the job dropped for the runtime's idle expiry */

/* the staging buffer of the open file */
static u8 *acc_raw, *acc_buf; /* acc_buf: acc_raw rounded up to 16 bytes */
static int acc_fw;            /* acc_raw came from fw_alloc */
static u32 acc_cap;           /* 0: no buffer */
static u32 acc_fill;          /* bytes in it */
static u32 acc_off;           /* file offset of its first byte */
static i32 acc_err;           /* sticky status of a failed flush */
static u32 file_size;
static u32 acc_limit = ACC_BIG; /* the largest buffer the next BEGIN asks for (a one-shot test knob, STATUS 'A' with UF_KNOBS; acc_open resets it) */
#if UF_KNOBS
static u32 acc_got; /* the size the last BEGIN got */
#endif
static int d_acc;               /* the DATA frame in `frame` is in the buffer already (a flush is retried) */
static int dstage;              /* pump: 1 = the DATA offset is still to come, 2 = the body goes straight into the buffer */
static int direct;              /* the DATA frame ready has its body in the buffer */
static i32 abort_h; /* a handle to abort after a reset (file_abort may need several steps) */

#if UF_TRACE || UF_BENCH
/* The only clock a module can call is dkrt_blocks(), the 1500 Hz block counter (1 unit = 0.667 ms). A short call often reads 0 and a long one is
 * rounded, so only the totals of whole calls (per file) are meaningful, not one call. Units: "blk" tags are blocks; uf.bytes and uf.datas are counts. */
#define UF_NOW() dkrt_blocks()
#define UF_KIND_MOD_LOG 14u    /* sdk/sdk.toml [[log.kind]] 14 MOD_LOG: args msg0..msg3 (16 ASCII chars, big-endian words) and value */
#define UF_FLAGS (2u << 1)     /* level 2 (info) in bits 1-3 (level_shift 1): passes the default log level */
#if UF_TRACE
static u32 uf_write, uf_copy, uf_wait, uf_begin, uf_end, uf_bytes, uf_datas;
static u32 uf_wait_from;
static int uf_waiting; /* uf_wait_from is valid: the last request of this file was answered and no byte of the next one has been copied yet (the wait is the host's stall; the copy is uf_copy) */
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
    dkrt_log_ev(UF_KIND_MOD_LOG, UF_FLAGS, a);
}
#endif

static u32 get32(const u8 *p) { return ((u32)p[0] << 24) | ((u32)p[1] << 16) | ((u32)p[2] << 8) | p[3]; }
static void put32(u8 *p, u32 v)
{
    p[0] = (u8)(v >> 24);
    p[1] = (u8)(v >> 16);
    p[2] = (u8)(v >> 8);
    p[3] = (u8)v;
}

static void copy(u8 *d, const u8 *s, u32 n)
{
    if (n >= 32 && (((u32)d | (u32)s) & 15u) == 0) { /* movem.l: bursts from the uncached receive buffers */
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
    if (acc_raw) {
        if (acc_fw)
            fw_free(acc_raw);
        else
            sdk->free(acc_raw);
    }
    acc_raw = acc_buf = 0;
    acc_cap = acc_fill = acc_off = 0;
    acc_err = 0;
    d_acc = 0;
}

static u8 *acc_try(u32 n)
{
    u8 *p = sdk->alloc(n + 16u);
    acc_fw = 0;
    if (!p) {
        p = fw_alloc(n + 16u);
        acc_fw = p != 0;
    }
    return p;
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
    }
#if UF_KNOBS
    acc_got = acc_cap;
#endif
}

/* One file_write of what the buffer holds. A status; on 0 the buffer is empty. */
static i32 acc_flush(void)
{
    if (!acc_fill)
        return 0;
#if UF_TRACE
    u32 t0 = UF_NOW();
#endif
    i32 r = sdk->file_write(handle, acc_off, acc_buf, acc_fill);
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
        sess_at = dkrt_blocks();
    }
    return r;
}

static void reply(u8 cmd, u8 seq, i32 status, const u8 *body, u32 n)
{
    rep[0] = 'D';
    rep[1] = 'K';
    rep[2] = 'B';
    rep[3] = '1';
    rep[4] = (u8)(cmd | REPLY_FLAG);
    rep[5] = seq;
    rep[6] = (u8)((u32)status >> 8);
    rep[7] = (u8)status;
    put32(rep + 8, n);
    for (u32 i = 0; i < n; i++)
        rep[HDR + i] = body[i];
    rep_len = HDR + n;
}

/* The frame in `frame` is handled. Returns 0 when its reply is staged, 1 when a file_* call was busy and the same frame is run again. */
static int dispatch(void)
{
    u8 cmd = frame[4], seq = frame[5];
    u32 flags = ((u32)frame[6] << 8) | frame[7];
    u32 len = get32(frame + 8);
    const u8 *body = frame + HDR;
    i32 r;

    if (cmd == CMD_INFO) {
        u8 b[20];
        b[0] = BULK_VERSION;
        b[1] = 0;
        b[2] = b[3] = 0;
        put32(b + 4, DATA_MAX);
        put32(b + 8, ALIGN);
        int stable = 0;
        for (u32 t = 0; t < 8 && !stable; t++) { /* SET_NONCE may be writing: read until an even, unchanged generation */
            u32 g = nonce_gen;
            if (g & 1u)
                continue;
            for (u32 i = 0; i < NONCE_LEN; i++)
                b[12 + i] = nonce[i];
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
        if (handle > 0) { /* the module is the only bulk client: a new BEGIN means the host gave the old file up */
            r = sdk->file_abort(handle);
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
        r = sdk->file_create(get32(body), (const char *)(body + 4), get32(body + p + 1), none ? 0 : h4,
                             (flags & FLAG_REPLACE) ? DK_FILE_REPLACE : 0);
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
            sess_at = dkrt_blocks();
            acc_open(get32(body + p + 1));
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
            u32 n = len - 4, off = get32(body);
            if (acc_err) {
                reply(cmd, seq, acc_err, 0, 0);
                return 0;
            }
            if (!d_acc) {
                if (off != acc_off + acc_fill) {
                    reply(cmd, seq, DK_FILE_E_OFFSET, 0, 0);
                    return 0;
                }
                if (off > file_size || n > file_size - off || ((n & 511u) != 0 && n != file_size - off)) {
                    reply(cmd, seq, DK_FILE_E_LENGTH, 0, 0);
                    return 0;
                }
                if (!direct && n > acc_cap - acc_fill) { /* the frame does not fit: flush first (nothing of the frame is taken yet) */
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
            if (acc_fill == acc_cap) { /* full: this DATA carries the flush */
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
        r = sdk->file_write(handle, get32(body), body + 4, len - 4);
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
            sess_at = dkrt_blocks();
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
        if (acc_fill) { /* the tail: the one write that may end in a part sector */
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
        r = sdk->file_close(handle, hash);
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
        reply(cmd, seq, r, hash, r == 0 || r == DK_FILE_E_HASH ? 4u : 0u);
        return 0;
    }
    if (cmd == CMD_ABORT) {
        if (handle <= 0) { /* nothing open: ABORT is the host's way to be sure */
            reply(cmd, seq, 0, 0, 0);
            return 0;
        }
        r = sdk->file_abort(handle);
        if (r == DK_FILE_E_BUSY && ++busy_tries < BUSY_TRIES)
            return 1;
        busy_tries = 0;
        if (r == 0 || r == DK_FILE_E_STATE) { /* only then is the session gone; on any other error the host asks again */
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
static void clear_stream(void)
{
    u8 *b;
    u32 n;
    have = total = 0;
    frame_ready = 0;
    dstage = direct = 0;
    skipping = 0;
    if (cur) {
        usb_bulk_release(cur);
        cur = 0;
    }
    while (usb_bulk_take(&b, &n) != 0)
        usb_bulk_release(b);
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

/* The stream is corrupt (bad magic, a length over the bound): abort the open file, reply E_MSG only when the magic was valid (the seq can be trusted),
 * and discard the rest of the current USB transfer: a buffer of less than 16 KiB ends it. */
static void fatal_stream(void)
{
    st_fatal++;
    if (have >= HDR && frame[0] == 'D' && frame[1] == 'K' && frame[2] == 'B' && frame[3] == '1')
        reply(frame[4], frame[5], E_MSG, 0, 0);
    have = total = 0;
    frame_ready = 0;
    dstage = direct = 0;
    skipping = last_len >= DK_USB_BULK_RX_BYTES; /* the buffer with the bad header was full: the transfer goes on in the next one */
    if (cur) {
        usb_bulk_release(cur); /* the rest of this buffer is garbage too */
        cur = 0;
    }
    drop_file();
}

/* Copy bytes from the receive buffers into the frame. 1: a whole frame is in `frame`; 0: no more bytes now; -1: the stream is corrupt. */
static int pump(void)
{
    for (;;) {
        if (total && have == total)
            return 1;
        if (!cur) {
            if (usb_bulk_take(&cur, &cur_len) == 0) {
                cur = 0;
                return 0;
            }
            cur_off = 0;
            last_len = cur_len;
            st_bufs++;
            st_bytes += cur_len;
        }
        if (skipping) {
            usb_bulk_release(cur);
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
        copy(dst, cur + cur_off, n);
#if UF_TRACE
        uf_copy += UF_NOW() - t0;
#endif
        have += n;
        cur_off += n;
        if (cur_off >= cur_len) {
            usb_bulk_release(cur);
            cur = 0;
        }
        if (dstage == 1 && have == HDR + 4u) {
            u32 off = get32(frame + HDR), body_n = total - (HDR + 4u);
            direct = body_n > 0 && off == acc_off + acc_fill && body_n <= acc_cap - acc_fill && off <= file_size && body_n <= file_size - off &&
                     ((body_n & 511u) == 0 || body_n == file_size - off);
            dstage = direct ? 2 : 0;
        }
        if (!total && have == HDR) {
            u32 len = get32(frame + 8);
            if (frame[0] != 'D' || frame[1] != 'K' || frame[2] != 'B' || frame[3] != '1')
                return -1;
            if (len > DATA_MAX + 4u + SLACK)
                return -1;
            total = HDR + len;
            dstage = (frame[4] == CMD_DATA && acc_cap && handle > 0 && !acc_err && len >= 4u) ? 1 : 0;
        }
    }
}

static i32 step(u8 *ctx)
{
    (void)ctx;
    st_steps++;
    /* The adapter frees its buffers when a take-over ends: the session is over (the stream, the open file and the reply are void) and the next enable allocates them again. */
    if (enabled && !(usb_bulk_state() & DK_USB_BULK_LIVE)) {
        enabled = 0;
        wm_logged = 1; /* the wait for the next take-over is not news */
        clear_stream();
        rep_len = 0;
        busy_tries = 0;
        drop_file();
    }
    if (!enabled) {
        i32 rc = usb_bulk_enable(job_id);
        enable_rc = rc;
        if (rc == DK_USB_BULK_E_NOMEM) { /* no memory for the buffers (the adapter's ST_NOMEM): try again at the next wake */
            if (!nomem_logged) {
                nomem_logged = 1;
                sdk->log_lv(1, (const u8 *)"usb-fast: no memory for the USB buffers", (u32)rc);
            }
            return DK_IO_WAIT;
        }
        nomem_logged = 0;
        if (rc == DK_USB_BULK_E_STATE) {
            /* Not in USB MIDI mode, or a mode switch is settling. The adapter wakes the job about four times a second: it enables as soon as the device is in USB
             * MIDI mode (the user's USB CONFIG, or a take-over). Only a real wrong mode is logged, once, and not while a take-over is on its way. */
            u32 st = usb_bulk_state();
            if ((st & DK_USB_BULK_WRONG_MODE) && !(st & DK_USB_BULK_TAKEN) && !wm_logged) {
                wm_logged = 1;
                sdk->log_lv(2, (const u8 *)"usb-fast: not in USB MIDI mode; bulk waits for a take-over", (u32)rc);
            }
            return DK_IO_WAIT;
        }
        if (rc < 0) {
            sdk->log_lv(0, (const u8 *)"usb-fast: enable failed", (u32)rc);
            return rc;
        }
        enabled = 1;
        wm_logged = 0;
        epoch = DK_USB_BULK_RESETS(usb_bulk_state());
        return DK_IO_WAIT;
    }
    for (;;) {
        u32 now = DK_USB_BULK_RESETS(usb_bulk_state());
        if (now != epoch) { /* a bus reset: what was half done is void, the host starts again */
            epoch = now;
            st_resets++;
            clear_stream();
            rep_len = 0;
            busy_tries = 0;
            drop_file();
        }
        if (handle > 0 && dkrt_blocks() != 0u && dkrt_blocks() - sess_at >= SESS_IDLE_BLOCKS) { /* the runtime has dropped the session: free the buffer, the host is gone */
            st_expired++;
            drop_file();
        }
        if (abort_h > 0) {
            i32 r = sdk->file_abort(abort_h);
            if (r == DK_FILE_E_BUSY && ++busy_tries < BUSY_TRIES)
                return DK_IO_MORE;
            busy_tries = 0;
            abort_h = 0;
        }
        if (rep_len) {
            i32 r = usb_bulk_send(rep, rep_len);
            if (r == DK_FILE_E_BUSY) { /* both send slots in flight: the adapter wakes the job */
                st_tx_busy++;
                return DK_IO_WAIT;
            }
            if (r == DK_USB_BULK_E_AGAIN) /* the engine refused and nothing is in flight: the adapter's tick wakes the job */
                return DK_IO_WAIT;
            if (r < 0) { /* the reply cannot go out: the host's view of the stream is lost, so start over */
                st_send_err++;
                rep_len = 0;
                clear_stream();
                drop_file();
                continue;
            }
            rep_len = 0;
        }
        if (frame_ready) {
            if (dispatch())
                return DK_IO_MORE;
            frame_ready = 0;
            have = total = 0;
            dstage = direct = 0;
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

static void finished(u8 *ctx, i32 status)
{
    (void)ctx;
    (void)status;
}

/* SysEx service command USB_FAST_NONCE (0x36): body nonce[8], the host's proof of which USB device is on its MIDI port; reply empty. The INFO reply of the
 * bulk link returns it. 0x37 STATUS (a debug aid: sixteen u32 words). */
static i32 command(u32 cmd, u32 seq, const u8 *body, u32 len, u8 *out)
{
    (void)seq;
#if UF_KNOBS
    if (cmd == NONCE_CMD + 1 && len > 0 && body[0] == 'A') { /* test knob: 'A' [max bytes u32] sets the largest staging buffer of the next BEGIN (0: none); replies the size of the last one */
        if (len >= 5)
            acc_limit = get32(body + 1);
        put32(out, acc_got);
        return 4;
    }
#endif
#if UF_BENCH
    if (cmd == NONCE_CMD + 1 && len > 0)
        return uf_bench_command(body, len, out);
#endif
    if (cmd == NONCE_CMD + 1) { /* STATUS (a debug aid): what the job has done and where it stands */
        put32(out, st_steps);
        put32(out + 4, st_frames);
        put32(out + 8, st_tx_busy);
        put32(out + 12, (u32)handle);
        put32(out + 16, rep_len);
        put32(out + 20, have);
        put32(out + 24, total);
        put32(out + 28, (frame_ready ? 1u : 0u) | (cur ? 2u : 0u) | (enabled ? 4u : 0u));
        put32(out + 32, usb_bulk_state());
        put32(out + 36, st_last_rc);
        put32(out + 40, st_bufs);
        put32(out + 44, st_bytes);
        put32(out + 48, st_fatal);
        put32(out + 52, st_resets);
        put32(out + 56, (u32)enable_rc);
        put32(out + 60, st_send_err);
        put32(out + 64, acc_cap);
        put32(out + 68, st_expired);
        return 72;
    }
    if (cmd == TAKEOVER_CMD) { /* also the renew while the host works */
        i32 r = usb_bulk_takeover();
        return r < 0 ? r : 0;
    }
    if (cmd == RELEASE_CMD) {
        (void)usb_bulk_giveback();
        return 0;
    }
    if (cmd != NONCE_CMD)
        return E_PROTO;
    if (len != NONCE_LEN)
        return E_MSG;
    nonce_gen++; /* odd: INFO does not trust the bytes */
    for (u32 i = 0; i < NONCE_LEN; i++)
        nonce[i] = body[i];
    nonce_gen++;
    return 0;
}

void dkm_fini(const dk_api *api)
{
    usb_bulk_disable(job_id);
#if UF_BENCH
    uf_bench_fini(api);
#endif
    api->io_cancel(job_id); /* the runtime aborts a file session the job leaves open */
    acc_release();
}

i32 dkm_init(const dk_api *api)
{
    if (api->size < sizeof(dk_api))
        return -48;
    sdk = api;
    frame = (u8 *)(((u32)frame_raw + 15u) & ~15u);
    job_id = api->io_post(step, finished, 0);
    if (job_id <= 0)
        return job_id ? job_id : -1;
#if UF_BENCH
    i32 bench = uf_bench_init(api);
    if (bench != 0) {
        api->io_cancel(job_id);
        return bench;
    }
#endif
    i32 rc = api->register_cmds(TAKEOVER_CMD, TAKEOVER_RANGE, command);
    return rc != 0 ? rc : api->register_cmds(NONCE_CMD, NONCE_RANGE, command);
}
