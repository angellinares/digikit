/* Private DN2 LP0 transport/control diagnostic. No public ABI/cache guarantee.
 * Stock send remains unbounded: the owned external runner supplies a deadline. */
typedef unsigned int u32;
typedef unsigned short u16;
#define LP_START 1u
#define LP_BUSY 2u
#define LP_DONE 3u
#define LP_ERROR 4u
#define LP_PAGES 235u
#define LP_BYTES 960000u
#define LP_STOCK_BYTES 0xabcu
#define LP_CLOSE_POLLS 65536u
struct LpMailbox { volatile u32 state, error, tag, sent; };
struct LpAck { volatile u32 sequence, valid, status, error, pages, bytes; };
struct LpCloseMailbox { volatile u32 state, error, id, track, epoch, sent; };
struct LpCloseAck { volatile u32 sequence, valid, status, id, track, epoch; };
struct LpMailbox lp_mail __attribute__((aligned(16)));
struct LpAck lp_ack __attribute__((aligned(16)));
struct LpCloseMailbox lp_close_mail __attribute__((aligned(16)));
struct LpCloseAck lp_close_ack __attribute__((aligned(16)));
volatile u32 lp_staging[1024] __attribute__((aligned(16)));
static volatile u32 lp_close_staging[1024] __attribute__((aligned(16)));
static volatile u32 lp_failed, lp_bad_reply;
static u32 lp_next, lp_sent;
/* Single UI producer publishes immutable ISR-visible identity before active=1. */
static volatile u32 lp_close_active, lp_close_sent, lp_close_bad;
static volatile u32 lp_close_id, lp_close_track, lp_close_epoch;
static u32 lp_close_polls, lp_close_last;
extern void lp_stock_send(u32 tag, const volatile u32 *source, u32 format);
extern void *lp_stock_copy(void *destination, const void *source, u32 length);
u32 lp_wire_size(void) { return 0xaccu; }
static u32 progress_bytes(u32 pages) {
    return pages == LP_PAGES ? LP_BYTES : pages * 4096u;
}

/* One ISR writer. Complete extension is owned until the original frame rearm;
 * each kind retains its own seqlock snapshot when the other kind arrives. */
void *lp_copy_reply(void *destination, const void *source, u32 length) {
    void *result = lp_stock_copy(destination, source, length);
    if (length != LP_STOCK_BYTES || !source || !destination) {
        lp_bad_reply = 1;
        return result;
    }
    const volatile u16 *e = (const volatile u16 *)source + LP_STOCK_BYTES / 2;
    if (e[0] != 0x4c50u) {
        u32 status = e[1] >> 8;
        u32 id = (u32)e[2] | ((u32)e[3] << 16);
        u32 track = (u32)e[4] | ((u32)e[5] << 16);
        u32 epoch = (u32)e[6] | ((u32)e[7] << 16);
        u32 valid = e[0] == 0x4241u && (e[1] & 255u) == 1 && status >= 1 && status <= 3 ? 1 : 2;
        /* Preserve bad matching observations even if the next frame is valid.
         * The UI's active=0 terminal claim ends this tuple's latch lifetime. */
        if (lp_close_active && lp_close_sent && id == lp_close_id &&
            track == lp_close_track && epoch == lp_close_epoch &&
            (valid != 1 || (status != 1 && status != 2))) lp_close_bad = 1;
        if (lp_close_ack.valid == valid && lp_close_ack.status == status &&
            lp_close_ack.id == id && lp_close_ack.track == track && lp_close_ack.epoch == epoch)
            return result;
        ++lp_close_ack.sequence;
        lp_close_ack.valid = valid; lp_close_ack.status = status;
        lp_close_ack.id = id; lp_close_ack.track = track; lp_close_ack.epoch = epoch;
        ++lp_close_ack.sequence;
        return result;
    }
    u32 status = e[2], error = e[3];
    u32 pages = (u32)e[4] | ((u32)e[5] << 16);
    u32 bytes = (u32)e[6] | ((u32)e[7] << 16);
    u32 valid = 2;
    if (e[1] == 1 && pages <= LP_PAGES && bytes == progress_bytes(pages) && status <= 4 &&
        ((status < 2 && !pages && !error) || (status == 2 && pages < LP_PAGES && !error) ||
         (status == 3 && pages == LP_PAGES && !error) || (status == 4 && error))) valid = 1;
    if (valid == 2 || error || status == 4) lp_bad_reply = 1;
    if (lp_ack.valid == valid && lp_ack.status == status && lp_ack.error == error &&
        lp_ack.pages == pages && lp_ack.bytes == bytes) return result;
    ++lp_ack.sequence;
    lp_ack.valid = valid; lp_ack.status = status; lp_ack.error = error;
    lp_ack.pages = pages; lp_ack.bytes = bytes;
    ++lp_ack.sequence;
    return result;
}

static void fail(u32 error) {
    lp_failed = 1;
    lp_mail.error = error; lp_mail.state = LP_ERROR;
    lp_close_mail.error = error; lp_close_mail.state = LP_ERROR;
}
static u32 progress(u32 *status, u32 *pages, u32 *bytes) {
    u32 seq = lp_ack.sequence;
    if (seq & 1u) return 0;
    u32 valid = lp_ack.valid, error = lp_ack.error;
    *status = lp_ack.status; *pages = lp_ack.pages; *bytes = lp_ack.bytes;
    if (seq != lp_ack.sequence || !valid) return 0;
    if (valid != 1 || error || *status == 4) { fail(2); return 0; }
    return 1;
}
static void close_tick(void) {
    u32 status, pages, bytes;
    if (!lp_close_active) {
        u32 id = lp_close_mail.id, track = lp_close_mail.track, epoch = lp_close_mail.epoch;
        if (!id || id > 0x7fffffffu || id <= lp_close_last || track > 1 ||
            !epoch || epoch > 0x7fffffffu) { fail(5); return; }
        lp_close_id = id; lp_close_track = track; lp_close_epoch = epoch;
        lp_close_last = id;
        lp_close_staging[0] = id; lp_close_staging[1] = track; lp_close_staging[2] = epoch;
        lp_close_sent = lp_close_polls = lp_close_mail.sent = 0;
        lp_close_active = 1; /* Publish identity last to the ISR. */
    }
    if (++lp_close_polls > LP_CLOSE_POLLS) { fail(6); return; }
    if (!lp_close_sent) {
        if (!progress(&status, &pages, &bytes) || status < 2) return;
        lp_close_sent = 1; lp_close_mail.state = LP_BUSY;
        lp_stock_send(0x01014243, lp_close_staging, 1);
        lp_close_mail.sent = 1;
        return; /* Never authorize reuse from sender return. */
    }
    u32 seq = lp_close_ack.sequence;
    if (seq & 1u) return;
    u32 valid = lp_close_ack.valid;
    status = lp_close_ack.status;
    u32 id = lp_close_ack.id, track = lp_close_ack.track, epoch = lp_close_ack.epoch;
    if (seq != lp_close_ack.sequence || !valid) return;
    if (id != lp_close_id || track != lp_close_track || epoch != lp_close_epoch) return;
    if (valid != 1 || (status != 1 && status != 2)) { fail(7); return; }
    if (status == 2) {
        /* Terminal claim: subsequent ISR replies cannot latch against this tuple.
         * Check sticky evidence AFTER retiring it, so earlier bad observations
         * cannot be hidden by a later valid snapshot or this publication gap. */
        lp_close_active = 0;
        if (lp_close_bad || lp_bad_reply) { fail(7); return; }
        lp_close_mail.state = LP_DONE;
    }
}
void lp_tick(void) {
    u32 status, pages, bytes;
    if (lp_failed) { lp_mail.state = lp_close_mail.state = LP_ERROR; return; }
    if (lp_bad_reply) { fail(2); return; }
    if (lp_close_bad) { fail(7); return; }
    /* Existing in-flight data completes first; otherwise CLOSE has queued priority. */
    if (!lp_sent && (lp_close_active || lp_close_mail.state == LP_START)) { close_tick(); return; }
    if (!lp_sent && lp_mail.state != LP_START) return;
    if (lp_next >= LP_PAGES || lp_mail.tag != lp_next) { fail(1); return; }
    if (!progress(&status, &pages, &bytes)) return;
    if (!lp_sent) {
        if (pages < lp_next || status < 2) return;
        if (pages != lp_next || bytes != progress_bytes(lp_next) || status != 2) { fail(3); return; }
        lp_mail.state = LP_BUSY; lp_sent = 1;
        lp_stock_send(lp_next, lp_staging, 1);
        lp_mail.sent = lp_next + 1;
        return;
    }
    if (pages < lp_next + 1) return;
    if (pages != lp_next + 1 || bytes != progress_bytes(lp_next + 1) ||
        status != (pages == LP_PAGES ? 3u : 2u)) { fail(3); return; }
    ++lp_next; lp_sent = 0; lp_mail.state = LP_DONE;
}
