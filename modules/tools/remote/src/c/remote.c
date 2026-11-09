/* digikit/remote: press keys and turn encoders from a computer (docs/plans/remote-control.md).
 *
 *   0x38 KEY      body: code (u8), down (u8, 0 or 1)         reply: status 0 or core/input's negative result
 *   0x39 TURN     body: encoder (u8, 1..), delta (s8)        reply: one byte, the delta taken (s8, |taken| <= 30); the rest is for the host to send again
 *   0x3a RELEASE  body: none                                 reply: status 0 (every key this module holds is released)
 *   0x3e STATE    body: none                                 reply: enabled (u8, 1: the module is loaded), state format (u8, 1), turn limit (u8, 30)
 *
 * The module is on exactly when it is loaded: the check mark of DIGIKIT > MODULES is the only switch. The module knows no device: codes and
 * encoders are the device file's.
 *
 * Settings (DIGIKIT > MODULES > SETTINGS...): the keys this module holds, the last command it ran and RELEASE ALL KEYS. The rows are text
 * and one action; the module owns no other state.
 *
 * A turn: the firmware adds a delta to a pending count per encoder and flushes at most 30 detents every 4 ticks. The module keeps its own estimate of
 * the pending count (it drains PACE_DRAIN per PACE_BLOCKS blocks of 1/1500 s, deliberately slower than the firmware) and takes only what fits below
 * the limit, so the firmware's clamp is never the thing that drops or delays a turn. */

#include <dk/api.h>
#include <dk/ui.h>

typedef uint32_t u32;
typedef uint8_t u8;
typedef int8_t s8;

#define E_MSG (-50)

#define TURN_LIMIT 30
#define PACE_DRAIN 30u
#define PACE_BLOCKS 96u
#define MAX_ENC 16u

static u8 held[16]; /* a bit per key code 0..127 */
static int pending[MAX_ENC];
static u32 stamp[MAX_ENC];
/* The last command, for the settings: what (0 none, 1 key, 2 turn, 3 release), its two numbers and its result. */
static u8 last_what;
static int last_a, last_b, last_r;
static dk_ui_menu *settings_menu;

static void last_is(u8 what, int a, int b, int r)
{
    last_what = what;
    last_a = a;
    last_b = b;
    last_r = r;
}

static int32_t key(u32 cmd, u32 seq, const u8 *body, u32 len, u8 *out)
{
    int r;
    (void)cmd;
    (void)seq;
    (void)out;
    if (len != 2u || body[1] > 1u)
        return E_MSG;
    if (body[0] > 127u)
        return E_MSG;
    r = input_key(body[0], body[1]);
    last_is(1, body[0], body[1], r);
    if (r == 0) {
        if (body[1])
            held[body[0] >> 3] |= (u8)(1u << (body[0] & 7u));
        else
            held[body[0] >> 3] &= (u8)~(1u << (body[0] & 7u));
    }
    return r;
}

static int32_t turn(u32 cmd, u32 seq, const u8 *body, u32 len, u8 *out)
{
    u32 enc, now, steps;
    int d, room, take, r;
    (void)cmd;
    (void)seq;
    if (len != 2u)
        return E_MSG;
    enc = body[0];
    if (enc < 1u || enc > MAX_ENC)
        return E_MSG;
    now = dkrt_blocks();
    steps = (now - stamp[enc - 1u]) / PACE_BLOCKS;
    if (steps > 0u) {
        int p = pending[enc - 1u];
        u32 drain = steps * PACE_DRAIN;
        if (drain >= (u32)(p < 0 ? -p : p))
            p = 0;
        else
            p += p < 0 ? (int)drain : -(int)drain;
        pending[enc - 1u] = p;
        stamp[enc - 1u] += steps * PACE_BLOCKS;
    }
    d = (s8)body[1];
    /* the room left in the direction of the turn */
    room = d > 0 ? TURN_LIMIT - pending[enc - 1u] : TURN_LIMIT + pending[enc - 1u];
    if (room < 0)
        room = 0;
    if (room > TURN_LIMIT)
        room = TURN_LIMIT;
    take = d > room ? room : (d < -room ? -room : d);
    if (take == 0) {
        out[0] = 0;
        return 1;
    }
    r = input_turn(enc, take);
    last_is(2, (int)enc, take, r);
    if (r != 0)
        return r;
    pending[enc - 1u] += take;
    out[0] = (u8)(s8)take;
    return 1;
}

/* Lets go of every key this module holds: 0, or the last error of core/input. */
static int release_all(void)
{
    u32 c;
    int worst = 0;
    for (c = 0; c < 128u; c++) {
        if (held[c >> 3] & (1u << (c & 7u))) {
            int r = input_key(c, 0);
            if (r == 0)
                held[c >> 3] &= (u8)~(1u << (c & 7u));
            else
                worst = r;
        }
    }
    last_is(3, 0, 0, worst);
    return worst;
}

static int32_t release(u32 cmd, u32 seq, const u8 *body, u32 len, u8 *out)
{
    (void)cmd;
    (void)seq;
    (void)body;
    (void)len;
    (void)out;
    return release_all();
}

static int32_t state(u32 cmd, u32 seq, const u8 *body, u32 len, u8 *out)
{
    (void)cmd;
    (void)seq;
    (void)body;
    (void)len;
    out[0] = 1;
    out[1] = 1;
    out[2] = TURN_LIMIT;
    return 3;
}

/* One handler for the range 0x38 to 0x3a, so that the module uses two of the runtime's registration slots, not four. */
static int32_t panel(u32 cmd, u32 seq, const u8 *body, u32 len, u8 *out)
{
    if (cmd == 0x38u)
        return key(cmd, seq, body, len, out);
    if (cmd == 0x39u)
        return turn(cmd, seq, body, len, out);
    return release(cmd, seq, body, len, out);
}

int32_t dkm_init(const dk_api *api)
{
    int a = api->register_cmds(0x38, 3, panel);
    int d = api->register_cmds(0x3e, 1, state);
    api->log((const u8 *)"remote registered", (u32)(a | d));
    return (a | d) ? -1 : 0;
}

/* ---- settings ------------------------------------------------------------------------------------------------------------ */

static int held_count(void)
{
    int c, n = 0;
    for (c = 0; c < 128; c++)
        n += (held[c >> 3] >> (c & 7)) & 1;
    return n;
}

/* Appends the decimal text of v at s[n]; returns the new length. */
static int put_num(char *s, int n, int v)
{
    char tmp[12];
    int k = 0;
    if (v < 0) {
        s[n++] = '-';
        v = -v;
    }
    do {
        tmp[k++] = (char)('0' + v % 10);
        v /= 10;
    } while (v);
    while (k)
        s[n++] = tmp[--k];
    return n;
}

static int put_str(char *s, int n, const char *t)
{
    while (*t)
        s[n++] = *t++;
    return n;
}

static void do_release(void *ctx, int arg)
{
    (void)ctx;
    (void)arg;
    release_all();
    if (settings_menu)
        dk_ui_refresh(settings_menu);
}

static void fill(dk_ui_menu *m, void *ctx)
{
    char t[DK_UI_ROW];
    int n;
    (void)ctx;
    settings_menu = m;
    n = put_str(t, 0, "KEYS HELD ");
    n = put_num(t, n, held_count());
    t[n] = 0;
    dk_ui_text(m, t);
    if (last_what == 0) {
        dk_ui_text(m, "LAST: NONE");
    } else if (last_what == 3) {
        n = put_str(t, 0, "LAST: RELEASE");
        t[n] = 0;
        dk_ui_text(m, t);
    } else {
        n = put_str(t, 0, last_what == 1 ? "LAST: KEY " : "LAST: TURN ");
        n = put_num(t, n, last_a);
        t[n++] = last_what == 1 ? (last_b ? 'D' : 'U') : ' ';
        if (last_what == 2)
            n = put_num(t, n, last_b);
        t[n] = 0;
        dk_ui_text(m, t);
    }
    if (last_what && last_r) {
        n = put_str(t, 0, "ERROR ");
        n = put_num(t, n, last_r);
        t[n] = 0;
        dk_ui_text(m, t);
    }
    dk_ui_action(m, "RELEASE ALL KEYS", do_release, 0, 0);
}

DK_MENU(fill)
