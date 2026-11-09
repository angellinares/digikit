/* DT2 1.17 / DN2 1.12 diagnostic only. Host publishes dd_mail.state last; staging is
 * immutable until DONE. ERROR is sticky until boot. No allocation or runtime API.
 * The stock sender consumes staging synchronously into its SPI queue. Staging
 * remains statically allocated even after an error; no timed retirement rule. */
typedef unsigned int u32;
typedef unsigned short u16;

#define DD_MAGIC 0xDDF4A601u
#define DD_ACK 0xA6DDu
#define DD_TIMEOUT 900u
#define DD_IDLE 0u
#define DD_BUSY 1u
#define DD_DONE 2u
#define DD_ERROR 3u
#define DD_START 4u

/* Mailbox words: state, err, stall, sent. Host only writes START at state. */
struct DdMailbox { volatile u32 state, err, stall, sent; };
struct DdMailbox dd_mail __attribute__((aligned(16)));
volatile u16 dd_frame[1344] __attribute__((aligned(16)));
static u32 dd_previous_quiet;
static u32 dd_failed, dd_op;
extern void dd_stock_send(int len, const u16 *src, int reclen, const volatile u16 *rec);

static u32 frame_word(u32 i)
{
    return (u32)dd_frame[2u * i] | ((u32)dd_frame[2u * i + 1u] << 16);
}

/* Grants come only from exact successful DSP acknowledgments. They permit this
 * isolated diagnostic to replace normal frames while a closed bank is loading,
 * even if a trigger remains held. This mutes the shared output during transfer;
 * it is not a production live-audio ingress path. */
static u32 dd_retired_token, dd_loading_token, dd_sealed_token, dd_opened_token;

static u32 request_op(void)
{
    u32 s1 = 0, s2 = 0, i, op = frame_word(4), seq = frame_word(3);
    u32 offset = frame_word(5), count = frame_word(6);
    if (frame_word(0) != 4 || frame_word(1) != DD_MAGIC ||
        frame_word(2) == 0 || count > 640 || offset > 240000 ||
        count > 240000 - offset)
        return 0;
    if (op == 1) {
        if (seq || offset || count) return 0;
    } else if (op == 2) {
        if (seq < 1 || seq > 375 || !count) return 0;
    } else if (op == 3) {
        if (seq != 376 || offset != 240000 || count) return 0;
    } else if (op == 4 || op == 5) {
        if (seq != (op == 4 ? 377u : 378u) || offset || count) return 0;
    } else return 0;
    for (i = 0; i < 7; ++i) {
        s1 += frame_word(i);
        s2 += s1;
    }
    for (i = 0; i < count; ++i) {
        s1 += frame_word(32 + i);
        s2 += s1;
    }
    return frame_word(7) == (s1 ^ s2) ? op : 0;
}

static u32 closed_grant(u32 op)
{
    u32 token = frame_word(2);
    if (op == 4) return 1;
    if (op == 1)
        return token == dd_loading_token ||
            (dd_retired_token && dd_retired_token != 0xffffffffu &&
             token == dd_retired_token + 1u);
    if (op == 2) return token == dd_loading_token;
    if (op == 3) return token == dd_loading_token || token == dd_sealed_token;
    return op == 5 && (token == dd_sealed_token || token == dd_opened_token);
}

static void acknowledged(u32 op)
{
    u32 token = frame_word(2);
    if (op == 4) {
        dd_retired_token = token;
        dd_loading_token = dd_sealed_token = dd_opened_token = 0;
    } else if (op == 1) {
        dd_loading_token = token;
        dd_retired_token = dd_sealed_token = dd_opened_token = 0;
    } else if (op == 3) {
        dd_sealed_token = token;
        dd_loading_token = dd_retired_token = 0;
    } else if (op == 5) {
        dd_opened_token = token;
        dd_retired_token = dd_loading_token = dd_sealed_token = 0;
    }
}

static void fail(u32 err)
{
    dd_failed = 1;
    dd_op = 0;
    dd_mail.err = err;
    dd_mail.state = DD_ERROR;
}

// Keep compiler dispatch tables out of executable sections checked as instructions.
__attribute__((optimize("no-jump-tables")))
void dd_frame_hook(int len, const u16 *src, int reclen, const volatile u16 *rec)
{
    /* Exact supported stock lengths; diagnostic capacity remains 0xa80. */
    u32 normal_frame = (len == 0x802 || len == 0xa80) &&
        reclen == 0xabc && src && src[0] == 3;
    u32 quiet = normal_frame && src[1] == 0 &&
        (src[17] | src[18] | src[19] | src[20]) == 0;
    u32 safe = quiet && dd_previous_quiet;
    u32 state = dd_mail.state;
    u32 op = dd_op;
    dd_previous_quiet = quiet;
    if (dd_failed) {
        dd_mail.state = DD_ERROR;
    } else if (state == DD_START || state == DD_BUSY) {
        if (state == DD_START) {
            dd_op = 0;
            op = request_op();
            dd_op = op;
            dd_mail.err = 0;
            dd_mail.stall = 0;
            dd_mail.sent = 0;
            dd_mail.state = DD_BUSY;
            if (!op) fail(101);
        }
        if (!dd_failed && dd_mail.sent && rec && rec[0x60 / 2] == DD_ACK) {
            u32 ack = (u32)rec[0x60 / 2] | ((u32)rec[0x60 / 2 + 1] << 16);
            u32 token = (u32)rec[0x64 / 2] | ((u32)rec[0x64 / 2 + 1] << 16);
            if (token == frame_word(2) && ((ack >> 16) & 511u) == frame_word(3)) {
                u32 status = ack >> 25;
                if (op == 4 && status == 6u) {
                    /* PENDING: retain staging, return control and poll boundedly. */
                } else if (status == (op == 3 ? 1u : op == 4 ? 5u : op == 5 ? 7u : 0u)) {
                    acknowledged(op);
                    dd_mail.err = status;
                    dd_mail.state = DD_DONE;
                } else if (status >= 2u && status <= 4u) fail(status);
                else fail(102);

            }
        }
        if (!dd_failed && dd_mail.state == DD_BUSY) {
            if (++dd_mail.stall > DD_TIMEOUT) fail(100);
            else if (safe || (normal_frame && op && closed_grant(op))) {
                ++dd_mail.sent;
                dd_stock_send(0xa80, (const u16 *)dd_frame, reclen, rec);
                return;
            }
        }
    }
    dd_stock_send(len, src, reclen, rec);
}
