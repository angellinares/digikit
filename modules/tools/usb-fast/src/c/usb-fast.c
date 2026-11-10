/* digikit/usb-fast: file uploads on the USB bulk interface of core/usb-bulk (docs/plans/usb-fast.md section 5, usb-fast-hardware.md "Step 4").
 *
 * One I/O job (<dk/io.h>) enables the interface, takes the received buffers, cuts the byte stream into frames by their own length field, runs each
 * request on file_create / file_write / file_close / file_abort (<dk/file.h>) and sends the reply on EP2 IN. The numbers are those of sdk/sdk.toml
 * [protocol.bulk] (request: 'DKB1' | cmd u8 | seq u8 | flags u16 | len u32 | body; reply: 'DKB1' | cmd|0x40 | seq | status i16 | len u32 | body;
 * big-endian). SysEx stays the runtime's channel for everything else; this module adds one SysEx command, SET_NONCE (0x36), by which the host proves
 * which USB device is on its MIDI port.
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
 * VERIFY is reserved: the reply is E_PROTO (the host verifies over SysEx: both devices are CPU bound there with tiny replies). */
/* the code is placed as it is: no jump tables (data in the code) */
#pragma GCC optimize("no-jump-tables")
#include <dk/api.h>
#include <dk/file.h>
#include <dk/io.h>
#include <dk/usb_bulk.h>

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
/* sdk/sdk.toml [protocol.status] */
#define E_MSG (-50)
#define E_STATE (-53)
#define E_PROTO (-60)
/* a file_* call that answers E_BUSY (after waiting about 3 s for the store) is called again once or twice, then the host is told: tries 1 and 2 retry, the third replies */
#define BUSY_TRIES 3u

static const dk_api *sdk;
static i32 job_id;
static int enabled;
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
static i32 abort_h; /* a handle to abort after a reset (file_abort may need several steps) */

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
                if (++busy_tries < BUSY_TRIES)
                    return 1;
                busy_tries = 0;
                reply(cmd, seq, DK_FILE_E_BUSY, 0, 0);
                return 0;
            }
            busy_tries = 0; /* 0, or E_STATE: the runtime had expired the session already */
            handle = 0;
        }
        const u8 *h4 = body + p + 1 + 4;
        int none = (h4[0] | h4[1] | h4[2] | h4[3]) == 0;
        r = sdk->file_create(get32(body), (const char *)(body + 4), get32(body + p + 1), none ? 0 : h4,
                             (flags & FLAG_REPLACE) ? DK_FILE_REPLACE : 0);
        if (r == DK_FILE_E_BUSY && ++busy_tries < BUSY_TRIES)
            return 1;
        busy_tries = 0;
        if (r > 0) {
            handle = r;
            r = 0;
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
        /* frame is 16-byte aligned and the data starts at HDR + 4 = 16: file_write gets an aligned pointer */
        r = sdk->file_write(handle, get32(body), body + 4, len - 4);
        st_last_rc = (u32)r;
        if (r == DK_FILE_E_BUSY && ++busy_tries < BUSY_TRIES)
            return 1;
        busy_tries = 0;
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
        r = sdk->file_close(handle, hash);
        if (r == DK_FILE_E_BUSY && ++busy_tries < BUSY_TRIES)
            return 1;
        busy_tries = 0;
        if (r != DK_FILE_E_BUSY) /* the session is closed whatever the result */
            handle = 0;
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
        if (r == 0 || r == DK_FILE_E_STATE) /* only then is the session gone; on any other error the host asks again */
            handle = 0;
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
        u32 n = cur_len - cur_off;
        if (n > want - have)
            n = want - have;
        copy(frame + have, cur + cur_off, n);
        have += n;
        cur_off += n;
        if (cur_off >= cur_len) {
            usb_bulk_release(cur);
            cur = 0;
        }
        if (!total && have == HDR) {
            u32 len = get32(frame + 8);
            if (frame[0] != 'D' || frame[1] != 'K' || frame[2] != 'B' || frame[3] != '1')
                return -1;
            if (len > DATA_MAX + 4u + SLACK)
                return -1;
            total = HDR + len;
        }
    }
}

static i32 step(u8 *ctx)
{
    (void)ctx;
    st_steps++;
    if (!enabled) {
        enabled = 1;
        i32 rc = usb_bulk_enable(job_id);
        enable_rc = rc;
        if (rc < 0) {
            sdk->log_lv(0, (const u8 *)"usb-fast: enable failed", (u32)rc);
            return rc;
        }
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
        return 64;
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
    api->io_cancel(job_id); /* the runtime aborts a file session the job leaves open */
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
    return api->register_cmds(NONCE_CMD, NONCE_RANGE, command);
}
