/* The DN2 sample pool's LP0 page protocol, on the ColdFire side and without firmware access (docs/design/dn2-sample-pool.md section 2).
 * tests/patchkit/test_sample_pool_dn2.py compiles it on the host. The DSP side (src/dsp/dn2/sample-pool.s) implements the same rules in
 * assembly; spw_dsp_rule is the reference the assembled receiver is tested against.
 *
 * One page is 4,096 bytes of one plane (1,024 words) with a 32-bit tag in front: 0xD0000000 | seq8 << 20 | page_index20. The page index
 * counts 4 KiB pages from the start of the pool's DDR range. seq8 grows by one per accepted page (mod 256); an exact repeat of the last tag
 * is accepted again (a resend is safe). Pages may come in any order.
 *
 * The DSP answers in a 16-byte extension of its reply record, eight numeric halves (a word is its low half, then its high half):
 *   h0 0x5350 (magic)   h1 2 (version)   h2 status: 0 unarmed, 1 armed and idle, 2 armed after an error, 3 dead
 *   h3 error code | error count << 8      h4 h5 pages accepted since boot (low, high)
 *   h6 last accepted tag: seq8 << 8 | page_index & 0xff      h7 (h0 + ... + h6 mod 65536) xor 0x5a5a
 * A page is acknowledged when the status is 1, the count is one more than before the send and h6 names the page. An error is new when the
 * error count differs from the one before the send. */
#ifndef SP_WIRE_H
#define SP_WIRE_H
#include <stdint.h>
#include "dkrt_pool.h"
#include "cf/pool_alloc.h"

#define SPW_TAG_MAGIC 0xd0000000u
#define SPW_ACK_MAGIC 0x5350u
#define SPW_ACK_VERSION 2u
#define SPW_ACK_XOR 0x5a5au

#define SPW_ST_UNARMED 0u
#define SPW_ST_ARMED 1u
#define SPW_ST_ERROR 2u   /* armed again, the last page was rejected */
#define SPW_ST_DEAD 3u

#define SPW_ERR_TAG 4u    /* tag prefix or page index out of range: nothing was written */
#define SPW_ERR_SEQ 5u    /* sequence number is neither the next one nor the last one */

/* Wall time is the firmware's DSP block counter, 1500 per second. */
#define SPW_ARMED_BLOCKS 300u   /* 200 ms: the receiver must be armed before the first page of a load */
#define SPW_ACK_BLOCKS 75u      /* 50 ms: one page must be acknowledged */
#define SPW_RESENDS 3u

#define SPW_HEADER_BYTES 64u
#define SPW_SECTOR 512u
#define SPW_RBUF_SECTORS 128u   /* one card read: 64 KiB, the most dkrt_store_read takes (15 mono or 7 stereo chunks) */

static inline uint32_t spw_tag(uint32_t seq, uint32_t page)
{
    return SPW_TAG_MAGIC | ((seq & 0xffu) << 20) | (page & 0xfffffu);
}
static inline uint32_t spw_tag_seq(uint32_t tag) { return (tag >> 20) & 0xffu; }
static inline uint32_t spw_tag_page(uint32_t tag) { return tag & 0xfffffu; }
static inline uint32_t spw_tag_last(uint32_t tag) { return (spw_tag_seq(tag) << 8) | (tag & 0xffu); }   /* the ack's h6 */

/* The receiver's decision on a tag (the DSP implements this): 0 accept (new page), 1 repeat of the last tag (copy again, no count),
 * SPW_ERR_TAG, SPW_ERR_SEQ. `last_tag` starts as spw_tag(0, 0) after boot. */
static inline uint32_t spw_dsp_rule(uint32_t last_tag, uint32_t tag, uint32_t ddr_pages)
{
    if ((tag >> 28) != 0xdu || spw_tag_page(tag) >= ddr_pages)
        return SPW_ERR_TAG;
    if (tag == last_tag)
        return 1;
    if (spw_tag_seq(tag) != ((spw_tag_seq(last_tag) + 1u) & 0xffu))
        return SPW_ERR_SEQ;
    return 0;
}

typedef struct spw_ack {
    uint32_t status, error, errcount, count, last;
} spw_ack;

static inline uint32_t spw_ack_sum(const uint16_t *e)
{
    return ((uint32_t)e[0] + e[1] + e[2] + e[3] + e[4] + e[5] + e[6]) & 0xffffu;
}

/* Decode the eight halves. 0 for a wrong magic or version, a bad checksum or a status above 3 (fail closed). */
static inline int spw_ack_decode(const volatile uint16_t *e, spw_ack *a)
{
    uint16_t h[8];
    uint32_t i;
    for (i = 0; i < 8; i++)
        h[i] = e[i];
    if (h[0] != SPW_ACK_MAGIC || h[1] != SPW_ACK_VERSION || h[2] > SPW_ST_DEAD || h[7] != ((spw_ack_sum(h) ^ SPW_ACK_XOR) & 0xffffu))
        return 0;
    a->status = h[2];
    a->error = h[3] & 0xffu;
    a->errcount = h[3] >> 8;
    a->count = (uint32_t)h[4] | ((uint32_t)h[5] << 16);
    a->last = h[6];
    return 1;
}

/* The ISR's snapshot of the last good decode, with a sequence counter (odd while the writer is inside). One writer: the reply copy hook. */
typedef struct spw_snap {
    volatile uint32_t seq, valid, status, error, errcount, count, last;
} spw_snap;

static inline void spw_snap_store(spw_snap *s, int valid, const spw_ack *a)
{
    ++s->seq;
    s->valid = valid ? 1u : 0u;
    if (valid) {
        s->status = a->status;
        s->error = a->error;
        s->errcount = a->errcount;
        s->count = a->count;
        s->last = a->last;
    }
    ++s->seq;
}

/* One attempt, as the UI task reads it: 0 when the writer is inside or ran during the read, or nothing valid arrived yet. */
static inline int spw_snap_read(const spw_snap *s, spw_ack *a)
{
    uint32_t q = s->seq, valid;
    if (q & 1u)
        return 0;
    valid = s->valid;
    a->status = s->status;
    a->error = s->error;
    a->errcount = s->errcount;
    a->count = s->count;
    a->last = s->last;
    if (q != s->seq || !valid)
        return 0;
    return 1;
}

/* ---- The page state machine of one transfer: stop and wait, bounded --------------------------------------------------------------------- */

#define SPW_WAIT 0
#define SPW_READY 1    /* the receiver is armed: a new page may go */
#define SPW_ACKED 2    /* the page in flight is acknowledged */
#define SPW_RESEND 3   /* the ack is late: send the same page again */
#define SPW_E_TIMEOUT (-1)
#define SPW_E_DEAD (-2)
#define SPW_E_TAG (-3)   /* the receiver rejected a tag (error 4 or 5) */
#define SPW_E_DSP (-4)   /* any other receiver error */

typedef struct spw_x {
    uint32_t inflight;      /* a page was sent and is not acknowledged */
    uint32_t tag;           /* the tag in flight */
    uint32_t base_count;    /* pages accepted, as of the last READY or ACKED snapshot */
    uint32_t base_errcount;
    uint32_t base_last;     /* h6 of that snapshot */
    uint32_t sent_at;       /* block clock of the last send */
    uint32_t wait_since;    /* block clock of the last progress (the start, an armed receiver, an acknowledgement) */
    uint32_t resends;
} spw_x;

static inline void spw_begin(spw_x *x, uint32_t now)
{
    x->inflight = 0;
    x->tag = 0;
    x->base_count = x->base_errcount = x->base_last = 0;
    x->sent_at = 0;
    x->wait_since = now;
    x->resends = 0;
}

/* The tag of the next new page: one more than the last accepted sequence number (so a load that gave up after a lost page starts right). */
static inline uint32_t spw_next_tag(const spw_x *x, uint32_t page)
{
    return spw_tag(((x->base_last >> 8) + 1u) & 0xffu, page);
}

/* The page was handed to the sender. */
static inline void spw_sent(spw_x *x, uint32_t tag, uint32_t now)
{
    x->inflight = 1;
    x->tag = tag;
    x->sent_at = now;
    x->resends = 0;
}

static inline void spw_resent(spw_x *x, uint32_t now)
{
    x->sent_at = now;
    x->resends++;
}

/* One look at the snapshot (`ack` is 0 when no good snapshot could be read this time). */
static inline int32_t spw_poll(spw_x *x, uint32_t now, const spw_ack *ack)
{
    if (ack && ack->status == SPW_ST_DEAD)
        return SPW_E_DEAD;
    if (!x->inflight) {
        if (ack && (ack->status == SPW_ST_ARMED || ack->status == SPW_ST_ERROR)) {
            x->base_count = ack->count;
            x->base_errcount = ack->errcount;
            x->base_last = ack->last;
            x->wait_since = now;
            return SPW_READY;
        }
        return (now - x->wait_since >= SPW_ARMED_BLOCKS) ? SPW_E_TIMEOUT : SPW_WAIT;
    }
    if (ack) {
        if (ack->status == SPW_ST_ERROR && ack->errcount != x->base_errcount)
            return (ack->error == SPW_ERR_TAG || ack->error == SPW_ERR_SEQ) ? SPW_E_TAG : SPW_E_DSP;
        if (ack->status == SPW_ST_ARMED && ack->count == x->base_count + 1u && ack->last == spw_tag_last(x->tag)) {
            x->inflight = 0;
            x->base_count = ack->count;
            x->base_errcount = ack->errcount;
            x->base_last = ack->last;
            x->wait_since = now;
            return SPW_ACKED;
        }
    }
    if (now - x->sent_at >= SPW_ACK_BLOCKS)
        return x->resends < SPW_RESENDS ? SPW_RESEND : SPW_E_TIMEOUT;
    return SPW_WAIT;
}

/* ---- File geometry and the page data ---------------------------------------------------------------------------------------------------- */

/* The 64-byte header of a native file (dkrt_sdk::sample::native_header): byte 1 is 0 mono or 1 stereo, rate 48 kHz at 8, PCM length at 4,
 * rate 48 kHz at 8, PCM length at 4; the PCM length is a whole number of frames and the file holds 64 + PCM bytes. The trailer is optional
 * and the other header bytes are ignored (as in native_header). 0 or DKPOOL_E_FORMAT. */
static inline int32_t spw_header(const uint8_t *h, uint32_t size, uint32_t *frames, uint32_t *channels)
{
    uint32_t ch, pcm, rate;
    if (size < SPW_HEADER_BYTES || h[1] > 1)
        return DKPOOL_E_FORMAT;
    ch = (uint32_t)h[1] + 1u;
    pcm = ((uint32_t)h[4] << 24) | ((uint32_t)h[5] << 16) | ((uint32_t)h[6] << 8) | h[7];
    rate = ((uint32_t)h[8] << 24) | ((uint32_t)h[9] << 16) | ((uint32_t)h[10] << 8) | h[11];
    if (rate != SP_NATIVE_RATE || pcm == 0 || pcm > size - SPW_HEADER_BYTES || pcm % (2u * ch))
        return DKPOOL_E_FORMAT;
    *frames = pcm / (2u * ch);
    *channels = ch;
    return 0;
}

/* Chunk c of a sample is 2,048 frames: one mono page, or one page of each stereo plane. Where it is, for the frame count of a chunk
 * (spw_span_of is the read). */
typedef struct spw_chunk {
    uint32_t sector;    /* first file sector */
    uint32_t sectors;   /* sectors to read (at most SPW_RBUF_SECTORS) */
    uint32_t skip;      /* byte offset of the PCM in the first sector (always 64) */
    uint32_t frames;    /* frames in this chunk: 2,048, less in the last one */
} spw_chunk;

static inline void spw_chunk_of(uint32_t channels, uint32_t frames, uint32_t c, spw_chunk *o)
{
    uint32_t left = frames - c * SPA_PAGE_FRAMES, n = left < SPA_PAGE_FRAMES ? left : SPA_PAGE_FRAMES;
    o->sector = (SPW_HEADER_BYTES + SPA_PAGE_BYTES * channels * c) / SPW_SECTOR;
    o->skip = SPW_HEADER_BYTES;
    o->frames = n;
    o->sectors = (o->skip + n * 2u * channels + SPW_SECTOR - 1u) / SPW_SECTOR;
}

/* One card read covers whole chunks from chunk c on: as many as fit in SPW_RBUF_SECTORS, fewer at the end of the sample. Chunk c + j starts
 * at byte `skip + j * chunk_bytes` of the buffer: a chunk is a multiple of 512 bytes, so every read starts on the 64-byte header offset. */
typedef struct spw_span {
    uint32_t sector;    /* first file sector */
    uint32_t sectors;   /* sectors to read (at most SPW_RBUF_SECTORS) */
    uint32_t skip;      /* byte offset of chunk c in the buffer (always 64) */
    uint32_t chunks;    /* whole chunks in the read */
} spw_span;

static inline void spw_span_of(uint32_t channels, uint32_t frames, uint32_t c, spw_span *o)
{
    uint32_t chunk_bytes = SPA_PAGE_BYTES * channels, total = (frames + SPA_PAGE_FRAMES - 1u) / SPA_PAGE_FRAMES;
    uint32_t most = (SPW_RBUF_SECTORS * SPW_SECTOR - SPW_HEADER_BYTES) / chunk_bytes, k = total - c, last, bytes;
    if (k > most)
        k = most;
    last = frames - (c + k - 1u) * SPA_PAGE_FRAMES;
    if (last > SPA_PAGE_FRAMES)
        last = SPA_PAGE_FRAMES;
    bytes = SPW_HEADER_BYTES + (k - 1u) * chunk_bytes + last * 2u * channels;
    o->sector = (SPW_HEADER_BYTES + chunk_bytes * c) / SPW_SECTOR;
    o->skip = SPW_HEADER_BYTES;
    o->chunks = k;
    o->sectors = (bytes + SPW_SECTOR - 1u) / SPW_SECTOR;
}

static inline void spw_zero_tail(uint8_t *page, uint32_t from)
{
    volatile uint8_t *p = page;
    uint32_t i;
    for (i = from; i < SPA_PAGE_BYTES; i++)
        p[i] = 0;
}

/* Mono: the chunk's PCM bytes (big endian halves, as in the file) into a page; the rest of the page is zero. */
static inline void spw_page_mono(const uint8_t *pcm, uint32_t frames, uint8_t *page)
{
    uint32_t i, n = frames * 2u;
    for (i = 0; i < n; i++)
        page[i] = pcm[i];
    spw_zero_tail(page, n);
}

/* Stereo: file order is L R L R (halves); planar output, each plane's halves stay as they are. */
static inline void spw_page_stereo(const uint8_t *pcm, uint32_t frames, uint8_t *left, uint8_t *right)
{
    uint32_t i;
    for (i = 0; i < frames; i++) {
        left[2u * i] = pcm[4u * i];
        left[2u * i + 1u] = pcm[4u * i + 1u];
        right[2u * i] = pcm[4u * i + 2u];
        right[2u * i + 1u] = pcm[4u * i + 3u];
    }
    spw_zero_tail(left, frames * 2u);
    spw_zero_tail(right, frames * 2u);
}

#endif
