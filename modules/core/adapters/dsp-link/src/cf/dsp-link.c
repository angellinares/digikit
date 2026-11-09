/* core/dsp-link: the ColdFire end of the code channel (docs/findings/31-dsp-link.md).
 *
 * The block-frame sender (an interrupt handler, 1500 times a second) calls the driver with the frame: dl_frame_hook (tramp.s)
 * replaces that call, runs dl_tick and continues in the stock driver with the same arguments. The driver builds a PUSHR
 * queue of 4 bytes per frame halfword (a command halfword, then the data halfword) and starts the DMA at its end, from the
 * words the caller prepared. Only `len` bytes of the 0xabc are data; the data halves of the entries behind them are never
 * written by any stock code, and the DSP's receive DMA stores them in its ring (finding 23, S4b). dl_tick writes a chunk
 * into those halves before the driver runs, and reads the DSP's acknowledgement from the record copy the driver made
 * from the previous reply.
 *
 * A transfer is a run of chunks, each carrying its own destination (idx, off), checked by the DSP, which takes them in
 * order. The sender keeps up to WINDOW chunks in flight and goes back to the first one not taken when the acknowledgement
 * stalls. Format and the DSP side: src/dsp/dsp-link.inc.
 *
 * Interface for the runtime (C ABI, no struct results):
 *   int dl_send(u32 dst_l2, const void *buf, u32 len);   0 started; -1 busy; -2 bad destination or size
 *   int dl_send_win(u32 win, u32 dst_word, const void *buf, u32 len);   the same into window `win` (1: the voices registry, 2: the effect rows of core/fx),
 *       the destination in words from the start of the window; the DSP checks it against the window's size (status 3)
 *   int dl_poll(DlStatus *st);                           the state (DL_IDLE, DL_BUSY, DL_DONE, DL_ERROR), and *st filled
 *   int dl_diag(u32 *out, u32 n);                        the DSP's diagnostic words of the last block (layout: README.md and
 *       src/dsp/dsp-link.inc); returns the number of words written (at most n, at most DL_DG_WORDS), -1 when the image has no record
 *       address, -2 when the reply carries no diagnostic block (core/dsp-voice absent from the image, or the DSP halted
 *       before it ran: the heartbeat words of an earlier call stop moving, the block stays in the record copy)
 *   void dl_set_tick_cb(void (*cb)(const volatile u16 *rec));   one per-block callback (NULL: off), called from dl_tick with the copy of
 *       the previous reply, in the block interrupt. It must be a short C function with no allocation and no logging. Off costs one pointer test.
 *   void dl_tick_cb_drop(u32 lo, u32 hi);   the runtime calls it after a module's fini: clears the callback when it points into [lo, hi)
 * dst_l2 is a byte address in the DSP's reserved area (_dl_area, DL_AREA_BYTES long), 4-aligned; len is a multiple of 4;
 * buf must stay valid until the state is DL_DONE or DL_ERROR. DL_DONE means the DSP has taken every chunk, has the bytes
 * in L2 and has invalidated its instruction caches. */

#include "dl-limits.h"

typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned char u8;

#define DL_MAGIC 0xD15Cu
#define DL_ACK_MAGIC 0xA5D1u
#define DL_ACK_OFF 0x60u
#define DL_DG_MAGIC 0xD1A6u /* low half of diagnostic word 0; the high half is the word count */
#define DL_DG_WORDS 32u
#define DL_AREA_BYTES DL_AREA_MAX_BYTES /* the largest area the 17-bit offset reaches (generated, sdk.toml); the DSP knows the real size (status 3) */
#define DL_WINDOW 12u
#define DL_STALL_FRAMES 900u /* 0.6 s without progress */
#define DL_END_FRAMES 6u

#define DL_IDLE 0u
#define DL_BUSY 1u
#define DL_DONE 2u
#define DL_ERROR 3u
#define DL_START 4u /* internal: dl_send has filled the request; the next tick starts it */

#define DL_E_RANGE 3u /* the DSP refused the destination (its status 3) */
#define DL_E_TIMEOUT 100u
#define DL_E_GEOMETRY 101u

typedef struct DlStatus {
    u32 state;
    u32 err;
    u32 nchunks; /* chunks of the transfer (known after the first tick) */
    u32 acked;   /* chunks the DSP has taken */
    u32 sent;    /* chunks written into frames, resends included */
    u32 resent;  /* times the sender went back */
    u32 nerr;    /* chunks the DSP dropped for a bad check */
} DlStatus;

/* The state of the sender. Offsets are read and written by tests (tools/patchkit/pk_dsplink.py): keep the order. */
typedef struct DlLink {
    volatile u32 state;
    u32 err;
    u32 buf;     /* address of the bytes */
    u32 dst_off; /* destination, words from the start of _dl_area */
    u32 nwords;
    u32 xid;
    u32 cap;     /* payload words per chunk (from the frame geometry) */
    u32 nchunks;
    u32 acked;
    u32 next;
    u32 stall;
    u32 since;
    u32 sent;
    u32 resent;
    u32 nerr;
    u32 ending;
    u32 dirty;
    u32 win; /* the window of the transfer (appended: the order of the fields above is read by tests) */
    u32 pad[2]; /* 80 bytes: the state fills whole 16-byte cache lines, no code shares a line it writes to (flush check) */
} DlLink;

DlLink dl_link __attribute__((aligned(16)));

/* The per-block callback of a probe module (dl_set_tick_cb); 0 = none. */
static void (*volatile dl_tick_cb)(const volatile u16 *rec);

void dl_set_tick_cb(void (*cb)(const volatile u16 *rec))
{
    dl_tick_cb = cb;
}

void dl_tick_cb_drop(u32 lo, u32 hi)
{
    u32 a = (u32)dl_tick_cb;
    if (a >= lo && a < hi)
        dl_tick_cb = 0;
}

extern char _dl_area[]; /* the DSP object's reserved area (a DSP address) */
extern volatile u32 dl_tx_lit; /* the literal of the driver's `lea tx_queue,a0`: the queue's address */
extern volatile u32 dl_rec_lit; /* the literal of the handler's first `pea`: the address of the reply record copy (dl_diag) */

static u32 word_at(const u8 *p)
{
    return (u32)p[0] | ((u32)p[1] << 8) | ((u32)p[2] << 16) | ((u32)p[3] << 24);
}

static void put_half(u32 tx, u32 entry, u32 v)
{
    *(volatile u16 *)(tx + 4u * entry + 2u) = (u16)v;
}

/* word j of the tail, which starts at frame entry e0 */
static void put_word(u32 tx, u32 e0, u32 j, u32 v)
{
    put_half(tx, e0 + 2u * j, v & 0xffffu);
    put_half(tx, e0 + 2u * j + 1u, v >> 16);
}

int dl_send(u32 dst_l2, const void *buf, u32 len)
{
    u32 lo = (u32)_dl_area;
    DlLink *l = &dl_link;
    u32 st = l->state;
    if (st == DL_BUSY || st == DL_START || l->ending)
        return -1;
    if ((dst_l2 & 3u) || (len & 3u) || len == 0u || dst_l2 < lo || dst_l2 - lo >= DL_AREA_BYTES ||
        len > DL_AREA_BYTES - (dst_l2 - lo) || buf == 0)
        return -2;
    l->win = 0;
    l->buf = (u32)buf;
    l->dst_off = (dst_l2 - lo) >> 2;
    l->nwords = len >> 2;
    l->err = 0;
    l->acked = 0;
    l->nerr = 0;
    l->state = DL_START; /* last: the tick takes the request only now */
    return 0;
}

int dl_send_win(u32 win, u32 dst_word, const void *buf, u32 len)
{
    DlLink *l = &dl_link;
    u32 st = l->state;
    if (st == DL_BUSY || st == DL_START || l->ending)
        return -1;
    if (win == 0u || win > 2u || (len & 3u) || len == 0u || dst_word >= DL_AREA_MAX_WORDS || buf == 0)
        return -2;
    l->win = win;
    l->buf = (u32)buf;
    l->dst_off = dst_word;
    l->nwords = len >> 2;
    l->err = 0;
    l->acked = 0;
    l->nerr = 0;
    l->state = DL_START;
    return 0;
}

int dl_poll(DlStatus *s)
{
    DlLink *l = &dl_link;
    u32 st = l->state;
    if (st == DL_START)
        st = DL_BUSY;
    s->state = st;
    s->err = l->err;
    s->nchunks = l->nchunks;
    s->acked = l->acked;
    s->sent = l->sent;
    s->resent = l->resent;
    s->nerr = l->nerr;
    return (int)st;
}

/* Record byte offset of diagnostic word j: slot row 1 + j / 2 (0x1c + 0x54 row), +0x44 for even j, +0x48 for odd j (words 14 and 15 are in row 8, 16 and 17 in row 9, 22 and 23 in row 12, 24 and 25 in row 13, 30 and 31 in row 16). */
static u32 diag_off(u32 j)
{
    return 0x1cu + 0x54u * (1u + j / 2u) + ((j & 1u) ? 0x48u : 0x44u);
}

int dl_diag(u32 *out, u32 n)
{
    const volatile u16 *rec = (const volatile u16 *)dl_rec_lit;
    u32 j, w0, count;
    if (!rec)
        return -1;
    w0 = (u32)rec[diag_off(0) / 2u] | ((u32)rec[diag_off(0) / 2u + 1u] << 16);
    if ((w0 & 0xffffu) != DL_DG_MAGIC)
        return -2;
    count = w0 >> 16;
    if (count > DL_DG_WORDS)
        count = DL_DG_WORDS;
    if (count > n)
        count = n;
    for (j = 0; j < count; j++) {
        u32 h = diag_off(j) / 2u;
        out[j] = (u32)rec[h] | ((u32)rec[h + 1u] << 16);
    }
    return (int)count;
}

static void finish(DlLink *l, u32 state, u32 err)
{
    l->err = err;
    l->state = state;
    l->ending = DL_END_FRAMES; /* END chunks tell the DSP to stop acknowledging */
}

static void write_chunk(DlLink *l, u32 tx, u32 e0, u32 idx, int end)
{
    u32 cap = l->cap;
    u32 first = idx * cap;
    u32 n = end ? 0u : l->nwords - first;
    u32 w0, w1, s1, s2, j;
    const u8 *p;
    if (n > cap)
        n = cap;
    w0 = DL_MAGIC | (idx << 16) | (l->win << 30);
    w1 = end ? (1u << 26) : ((l->dst_off + first) | (n << 17) | ((idx + 1u == l->nchunks) ? (1u << 25) : 0u));
    w1 |= l->xid << 27;
    s1 = w0 + w1;
    s2 = w0 + s1;
    put_word(tx, e0, 0, w0);
    put_word(tx, e0, 1, w1);
    p = (const u8 *)l->buf + 4u * first;
    for (j = 0; j < n; j++) {
        u32 w = word_at(p + 4u * j);
        s1 += w;
        s2 += s1;
        put_word(tx, e0, 2u + j, w);
    }
    put_word(tx, e0, 2u + n, s1 ^ s2);
}

/* Called before the stock driver, with its arguments: len = data bytes, src = the data, reclen = frame bytes,
 * rec = the copy of the DSP's previous reply. Interrupt context. */
void dl_tick(int len, const u16 *src, int reclen, const volatile u16 *rec)
{
    DlLink *l = &dl_link;
    u32 st = l->state;
    u32 tx, e0, total, words;
    void (*cb)(const volatile u16 *) = dl_tick_cb;
    (void)src;
    if (cb)
        cb(rec);
    if (st != DL_START && st != DL_BUSY && !l->ending && !l->dirty)
        return;
    tx = dl_tx_lit;
    e0 = ((u32)len / 2u + 1u) & ~1u;
    total = (u32)reclen / 2u;
    words = (total > e0 + 2u && total < 0x600u) ? (total - e0) / 2u : 0u;
    if (st == DL_START) {
        if (words < 4u) {
            l->cap = 0;
            finish(l, DL_ERROR, DL_E_GEOMETRY);
            return;
        }
        l->cap = words - 3u;
        l->nchunks = (l->nwords + l->cap - 1u) / l->cap;
        l->xid = (l->xid & 31u) + 1u;
        if (l->xid > 31u)
            l->xid = 1;
        l->next = 0;
        l->acked = 0;
        l->stall = 0;
        l->since = 0;
        l->sent = 0;
        l->resent = 0;
        st = l->state = DL_BUSY;
    }
    if (st == DL_BUSY) {
        u32 idx;
        if (rec && rec[DL_ACK_OFF / 2u] == DL_ACK_MAGIC) {
            u32 cum = rec[DL_ACK_OFF / 2u + 1u];
            u32 xs = rec[DL_ACK_OFF / 2u + 2u];
            if ((xs & 0xffu) == l->xid) {
                u32 status = xs >> 8;
                l->nerr = rec[DL_ACK_OFF / 2u + 3u];
                if (cum > l->acked && cum <= l->nchunks) {
                    l->acked = cum;
                    l->stall = 0;
                }
                if (status >= 2u) {
                    finish(l, DL_ERROR, status);
                    st = DL_ERROR;
                } else if (status == 1u && l->acked >= l->nchunks) {
                    finish(l, DL_DONE, 0);
                    st = DL_DONE;
                }
            }
        }
        if (st == DL_BUSY) {
            if (++l->stall > DL_STALL_FRAMES) {
                finish(l, DL_ERROR, DL_E_TIMEOUT);
            } else {
                l->since++;
                if (l->next > l->acked + DL_WINDOW || (l->next >= l->nchunks && (l->since % 24u) == 23u)) {
                    l->next = l->acked;
                    l->resent++;
                }
                idx = l->next < l->nchunks ? l->next : l->nchunks - 1u;
                if (l->next < l->nchunks)
                    l->next++;
                write_chunk(l, tx, e0, idx, 0);
                l->sent++;
                l->dirty = 1;
                return;
            }
        }
    }
    if (l->ending) {
        if (words >= 4u && l->cap) {
            write_chunk(l, tx, e0, 0, 1);
            l->dirty = 1;
        }
        l->ending--;
        return;
    }
    if (l->dirty && words >= 1u) {
        put_word(tx, e0, 0, 0); /* no magic: the DSP sees stock frames again */
        l->dirty = 0;
    }
}
