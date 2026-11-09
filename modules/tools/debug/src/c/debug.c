/* digikit/debug: the runtime's event log, its level filter, and the commands that read it (docs/findings/46-digikit-debug.md).
 *
 * The runtime calls the sink (`dkrt_log_sink_set`) with every event it logs, with interrupts masked, in whatever task logs. The sink applies the level
 * filter and appends to a ring of RING events (static memory). When the sink is set the runtime first hands over the events of its small ring (the boot and the
 * errors so far). An event is 32 bytes: seq (from 1, assigned here), time (the block counter, 1500 per second), kind, flags (bit 0: an error, bits 1 to 3: the
 * level, bits 8 to 15: the source module, record index + 1) and five words. An error event is always kept; the others pass when their level is not above the
 * level of their source module, or the default level.
 *
 *   0x20 LOG     body: u32 from_seq        reply: u32 oldest, u32 newest, u8 n, then n events (32 bytes each, big endian) from `from_seq` or the oldest kept
 *   0x21 PEEK    body: u32 addr, u16 len   reply: u32 addr, u16 len, the bytes (RAM inside the device file's `[digikit] peek` ranges, 1 to 512 bytes)
 *   0x22 LOGCFG  body: u8 op (0 get, 1 set; a set adds u8 default level and 8 bytes, the level of module record 0 to 7, 0xff: the default)
 *                reply: u8 default level, 8 module levels, u8 1 (the debug module is loaded)
 *
 * The tick (once per pass of the UI task, set with the sink) reads the machine type of each of the 16 tracks every 20 ms from the firmware (`dkrt_track_type`) and logs a change (MACHINE_CHANGE). */

#include <dk/api.h>

typedef uint32_t u32;
typedef uint16_t u16;
typedef uint8_t u8;

/* The byte at a device address; the host test reads a fake memory instead. */
#ifndef DEBUG_READ8
#define DEBUG_READ8(a) (*(const volatile u8 *)(uintptr_t)(a))
#endif

#define E_MSG (-50)
#define E_PROTO (-60)
#define E_PEEK (-63)

#define CMD_FIRST 0x20u
#define CMD_COUNT 3u
#define CMD_LOG 0x20u
#define CMD_PEEK 0x21u
#define CMD_LOGCFG 0x22u

#define RING 512u
#define REPLY_MAX 879u /* the reply body of a service command */
#define LOG_PER_REPLY ((REPLY_MAX - 9u) / 32u)
#define PEEK_MAX 512u
#define MAX_RANGES 4u
#define MODULES 8u /* the module records LOGCFG has a level for */

#define F_ERROR 1u
#define LEVEL_SHIFT 1u
#define LEVEL_MASK 7u
#define SOURCE_SHIFT 8u
#define L_INFO 2u
#define LEVEL_NONE 0xffu
#define LEVEL_MAX 4u

#define K_MACHINE_CHANGE 26u
#define TRACKS 16u
#define TRACKS_EVERY 30u /* blocks: 20 ms */
#define TRACKS_FROM 3000u /* blocks after the first pass: the project exists by then */

typedef struct Ev {
    u32 seq;
    u32 time;
    u16 kind;
    u16 flags;
    u32 a[5];
} Ev;

/* The ring is module bss, not an `Api.alloc` block: the arena need is then what `dkneeds` reads from the file (docs/design/digikit-dkm.md, "Needs are inferred from the contents"). */
static Ev ring[RING];
static volatile u32 count; /* events appended: the newest sequence number */
static volatile u32 base; /* sequence numbers up to here were lost before this module loaded (the runtime's small ring overflowed) */
static volatile u32 level_default = L_INFO;
static volatile u8 level_module[MODULES];

static u32 now(void)
{
    return dkrt_blocks();
}

/* The sink: interrupts are masked, so the append needs no lock. */
static void sink(const Ev *e)
{
    u32 flags = e->flags;
    u32 n, i;
    Ev *s;
    if (!(flags & F_ERROR)) {
        u32 limit = level_default;
        u32 src = (flags >> SOURCE_SHIFT) & 0xffu;
        if (src > 0u && src <= MODULES && level_module[src - 1u] != LEVEL_NONE)
            limit = level_module[src - 1u];
        if (((flags >> LEVEL_SHIFT) & LEVEL_MASK) > limit)
            return;
    }
    n = count + 1u;
    if (e->seq > n && count == 0u) { /* the runtime's replay starts after lost events: keep its numbering, so the reader sees the gap */
        n = e->seq;
        base = n - 1u;
    }
    s = &ring[(n - 1u) % RING];
    s->seq = 0; /* a reader that is cut off here sees no event, not a half-written one */
    s->time = e->time;
    s->kind = e->kind;
    s->flags = e->flags;
    for (i = 0; i < 5u; i++)
        s->a[i] = e->a[i];
    s->seq = n;
    count = n;
}

static void put32(u8 *o, u32 v)
{
    o[0] = (u8)(v >> 24);
    o[1] = (u8)(v >> 16);
    o[2] = (u8)(v >> 8);
    o[3] = (u8)v;
}

static u32 get32(const u8 *b)
{
    return ((u32)b[0] << 24) | ((u32)b[1] << 16) | ((u32)b[2] << 8) | b[3];
}

/* Copy event `seq` to `o` (32 bytes, big endian). False when it is gone, or was overwritten while it was copied (a writer in an interrupt). */
static int copy_event(u32 seq, u8 *o)
{
    const volatile Ev *e = &ring[(seq - 1u) % RING];
    u32 i, k, f;
    if (e->seq != seq)
        return 0;
    put32(o, seq);
    put32(o + 4, e->time);
    k = e->kind;
    f = e->flags;
    o[8] = (u8)(k >> 8);
    o[9] = (u8)k;
    o[10] = (u8)(f >> 8);
    o[11] = (u8)f;
    for (i = 0; i < 5u; i++)
        put32(o + 12u + 4u * i, e->a[i]);
    return e->seq == seq;
}

static int32_t cmd_log(const u8 *body, u32 len, u8 *out)
{
    u32 from, newest, oldest, n = 0, s;
    if (len < 4u)
        return E_MSG;
    from = get32(body);
    newest = count;
    oldest = newest > RING ? newest - RING + 1u : (newest ? 1u : 0u);
    if (newest != 0u && oldest <= base)
        oldest = base + 1u;
    put32(out, oldest);
    put32(out + 4, newest);
    if (newest != 0u) {
        s = from < oldest ? oldest : from;
        while (s <= newest && n < LOG_PER_REPLY) {
            if (copy_event(s, out + 9u + 32u * n))
                n++;
            s++;
        }
    }
    out[8] = (u8)n;
    return (int32_t)(9u + 32u * n);
}

static int peek_allowed(u32 addr, u32 len)
{
    u32 end = addr + len;
    u32 i, n, r[2];
    if (end < addr)
        return 0;
    n = dkrt_peek_range(0u, r);
    if (n > MAX_RANGES)
        n = MAX_RANGES;
    for (i = 0; i < n; i++) {
        dkrt_peek_range(i, r);
        if (addr >= r[0] && end <= r[1])
            return 1;
    }
    return 0;
}

static int32_t cmd_peek(const u8 *body, u32 len, u8 *out)
{
    u32 addr, n, i;
    if (len != 6u)
        return E_MSG;
    addr = get32(body);
    n = ((u32)body[4] << 8) | body[5];
    if (n == 0u || n > PEEK_MAX || !peek_allowed(addr, n))
        return E_PEEK;
    put32(out, addr);
    out[4] = body[4];
    out[5] = body[5];
    for (i = 0; i < n; i++)
        out[6u + i] = DEBUG_READ8(addr + i);
    return (int32_t)(6u + n);
}

static int32_t cmd_logcfg(const u8 *body, u32 len, u8 *out)
{
    u32 i;
    if (len < 1u)
        return E_MSG;
    if (body[0] == 1u) {
        if (len != 10u)
            return E_MSG;
        if (body[1] > LEVEL_MAX)
            return E_PROTO;
        for (i = 0; i < MODULES; i++)
            if (body[2u + i] > LEVEL_MAX && body[2u + i] != LEVEL_NONE)
                return E_PROTO;
        level_default = body[1];
        for (i = 0; i < MODULES; i++)
            level_module[i] = body[2u + i];
    } else if (body[0] != 0u) {
        return E_PROTO;
    }
    out[0] = (u8)level_default;
    for (i = 0; i < MODULES; i++)
        out[1u + i] = level_module[i];
    out[9] = 1u; /* the debug log is on while this module is loaded */
    return 10;
}

static int32_t cmd(u32 c, u32 seq, const u8 *body, u32 len, u8 *out)
{
    (void)seq;
    if (c == CMD_LOG)
        return cmd_log(body, len, out);
    if (c == CMD_PEEK)
        return cmd_peek(body, len, out);
    if (c == CMD_LOGCFG)
        return cmd_logcfg(body, len, out);
    return E_PROTO;
}

/* ---- the track watch (the sink's tick, once per pass of the UI task) ---------------------------------------------------------------- */

static struct {
    u32 started, next;
    u32 valid;
    int32_t types[TRACKS];
} watch;

/* The tick of the sink: the runtime calls it once per pass of the UI task, so the module needs no slot (a slot has one owner). */
static void tick(void)
{
    u32 t, a[5];
    u32 b = now();
    if (!watch.started) {
        watch.started = 1u;
        watch.next = b + TRACKS_FROM;
        return;
    }
    if ((int32_t)(b - watch.next) < 0)
        return;
    watch.next = b + TRACKS_EVERY;
    if (dkrt_track_type(0u) == -2)
        return; /* no project yet */
    for (t = 0; t < TRACKS; t++) {
        int32_t ty = dkrt_track_type(t);
        if (watch.valid && ty != watch.types[t]) {
            a[0] = t;
            a[1] = (u32)watch.types[t];
            a[2] = (u32)ty;
            a[3] = a[4] = 0u;
            dkrt_log_ev(K_MACHINE_CHANGE, L_INFO << LEVEL_SHIFT, a);
        }
        watch.types[t] = ty;
    }
    watch.valid = 1u;
}

int32_t dkm_init(const dk_api *api)
{
    u32 i;
    for (i = 0; i < MODULES; i++)
        level_module[i] = LEVEL_NONE;
    level_default = L_INFO;
    if (api->register_cmds(CMD_FIRST, CMD_COUNT, cmd) != 0) {
        api->log((const u8 *)"debug: register_cmds refused", 0);
        return -1;
    }
    dkrt_log_sink_set((uint8_t *)sink, (uint8_t *)tick);
    return 0;
}

void dkm_fini(const dk_api *api)
{
    (void)api;
    dkrt_log_sink_set(0, 0);
}
