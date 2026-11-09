/* digikit/probe: collects the words of a DSP probe module (docs/plans/debug-mode-and-probes.md, phase 1).
 *
 * The DSP module stores one word per block in the module step word; the block tail of core/dsp-link copies it into diagnostic word 25 of that block's reply
 * record. dkm_init sets the per-block callback of core/dsp-link; the callback runs in the block interrupt (1500 Hz) with the copy of the previous reply, and keeps
 * word 25 in a ring when the module ran in that record. The ring takes words until it is full (the rest count as overruns); RESET empties it. The words are framed by the
 * module (tools/device/dkprobe_seq.py): header 0xB0 << 24 | n << 16 | sequence, n payload words, checksum s1 ^ s2 (s1 += w; s2 += s1). STATUS checks the frames
 * in the MIDI task, a bounded number of words per request; the callback does no checking.
 *
 * Every record carries the step word of its block, so every block gives a word (1500 words a second). The heartbeat (word 1) steps by 1 from record to record
 * (less: a repeat; more: records missed), and the module call counts (words 26 and 27) say whether a module ran: a record without a new call adds no word.
 *
 *   0x32 STATUS  body: none            reply: 16 u32, big endian (format 3)
 *   0x33 READ    body: u32 first word, u16 count (at most READ_MAX)   reply: u32 words in the ring, u32 first, then the words (big endian)
 *   0x34 RESET   body: none            reply: status 0
 */

#include <dk/api.h>

typedef uint32_t u32;
typedef uint16_t u16;
typedef uint8_t u8;

#define E_MSG (-50)
#define CMD_FIRST 0x32u
#define CMD_COUNT 3u
#define CMD_STATUS 0x32u
#define CMD_READ 0x33u
#define CMD_RESET 0x34u

#define RING_WORDS 24576u /* one run of the SHARC probe is 23,436 words; the rest is the margin for a start inside a frame */
#define READ_MAX 200u
#define PARSE_BUDGET 4096u /* words looked at per STATUS */
#define MAGIC 0xB0u
#define MAX_PAYLOAD 64u

/* byte offset of diagnostic word j in the reply record (core/dsp-link, dl_diag) */
#define DIAG_OFF(j) (0x1cu + 0x54u * (1u + (j) / 2u) + (((j) & 1u) ? 0x48u : 0x44u))
#define W_BEAT 1u
#define W_STEP 25u
#define W_CALLS_MASTER 26u
#define W_CALLS_SEND 27u

/* The ring is module bss, not an `Api.alloc` block: the arena need is then what `dkneeds` reads from the file (docs/design/digikit-dkm.md, "Needs are inferred from the contents"). */
static u32 ring_store[RING_WORDS];

typedef struct Probe {
    u32 *ring;
    volatile u32 n; /* words in the ring */
    /* the callback's counters */
    u32 blocks;   /* callback calls */
    u32 records;  /* new reply records */
    u32 drops;    /* records missed (heartbeat jumps) */
    u32 repeats;  /* the same record again */
    u32 idle;     /* new records with no module call */
    u32 overruns; /* words that did not fit */
    u32 have;     /* the next two are valid */
    u32 last_beat;
    u32 last_calls;
    /* the checker's state (STATUS) */
    u32 parsed;      /* the next word to look at */
    u32 synced;      /* a frame was accepted since RESET */
    u32 have_seq;
    u32 last_seq;
    u32 frames_ok;
    u32 seq_errors;
    u32 cks_errors;
} Probe;

static Probe pr;

static u32 diag(const volatile u16 *rec, u32 j)
{
    u32 h = DIAG_OFF(j) / 2u;
    return (u32)rec[h] | ((u32)rec[h + 1u] << 16);
}

/* The per-block callback: interrupt context, no allocation, no logging. */
static void tick(const volatile u16 *rec)
{
    u32 beat, calls, gap, n;
    if (!rec)
        return;
    pr.blocks++;
    beat = diag(rec, W_BEAT);
    calls = diag(rec, W_CALLS_MASTER) + diag(rec, W_CALLS_SEND);
    if (!pr.have) {
        pr.have = 1;
        pr.last_beat = beat;
        pr.last_calls = calls;
        return;
    }
    gap = beat - pr.last_beat; /* 1 between consecutive records */
    if (gap < 1u) {
        pr.repeats++;
        return;
    }
    if (gap > 1u)
        pr.drops += gap - 1u;
    pr.last_beat = beat;
    pr.records++;
    if (calls == pr.last_calls) {
        pr.idle++;
        return;
    }
    pr.last_calls = calls;
    n = pr.n;
    if (n < RING_WORDS) {
        pr.ring[n] = diag(rec, W_STEP);
        pr.n = n + 1u;
    } else {
        pr.overruns++;
    }
}

/* Check frames from `parsed` on, at most PARSE_BUDGET words. Before the first accepted frame a candidate that fails is skipped without a count (the ring starts
 * anywhere in a frame); after it, a failed candidate is a checksum error and the scan goes on one word later. */
static void check_frames(void)
{
    u32 budget = PARSE_BUDGET;
    u32 n = pr.n;
    u32 p = pr.parsed;
    while (p < n && budget > 0u) {
        u32 h = pr.ring[p];
        u32 cnt = (h >> 16) & 0xffu;
        if ((h >> 24) == MAGIC && cnt >= 1u && cnt <= MAX_PAYLOAD) {
            u32 i, s1 = 0, s2 = 0;
            if (p + cnt + 1u >= n)
                break; /* the frame is not all here yet (or the ring ended inside it) */
            for (i = 0; i <= cnt; i++) {
                s1 += pr.ring[p + i];
                s2 += s1;
            }
            if ((s1 ^ s2) == pr.ring[p + cnt + 1u]) {
                u32 seq = h & 0xffffu;
                if (pr.have_seq && ((seq - pr.last_seq) & 0xffffu) != 1u)
                    pr.seq_errors++;
                pr.have_seq = 1;
                pr.last_seq = seq;
                pr.synced = 1;
                pr.frames_ok++;
                p += cnt + 2u;
                budget = budget > cnt + 2u ? budget - (cnt + 2u) : 0u;
                continue;
            }
            if (pr.synced)
                pr.cks_errors++;
        }
        p++;
        budget--;
    }
    pr.parsed = p;
}

static void put32(u8 *o, u32 v)
{
    o[0] = (u8)(v >> 24);
    o[1] = (u8)(v >> 16);
    o[2] = (u8)(v >> 8);
    o[3] = (u8)v;
}

static int32_t cmd(u32 c, u32 seq, const u8 *body, u32 len, u8 *out)
{
    u32 i;
    (void)seq;
    if (c == CMD_STATUS) {
        check_frames();
        put32(out + 0, 3u); /* format */
        put32(out + 4, pr.blocks);
        put32(out + 8, pr.records);
        put32(out + 12, pr.drops);
        put32(out + 16, pr.repeats);
        put32(out + 20, pr.idle);
        put32(out + 24, pr.n);
        put32(out + 28, pr.overruns);
        put32(out + 32, RING_WORDS);
        put32(out + 36, pr.parsed);
        put32(out + 40, pr.frames_ok);
        put32(out + 44, pr.seq_errors);
        put32(out + 48, pr.cks_errors);
        put32(out + 52, pr.have_seq ? pr.last_seq : 0xffffffffu);
        put32(out + 56, pr.last_beat);
        put32(out + 60, pr.synced);
        return 64;
    }
    if (c == CMD_READ) {
        u32 first, count, n = pr.n;
        if (len != 6u)
            return E_MSG;
        first = ((u32)body[0] << 24) | ((u32)body[1] << 16) | ((u32)body[2] << 8) | body[3];
        count = ((u32)body[4] << 8) | body[5];
        if (count > READ_MAX)
            count = READ_MAX;
        if (first >= n)
            count = 0;
        else if (count > n - first)
            count = n - first;
        put32(out, n);
        put32(out + 4, first);
        for (i = 0; i < count; i++)
            put32(out + 8u + 4u * i, pr.ring[first + i]);
        return (int32_t)(8u + 4u * count);
    }
    if (c == CMD_RESET) {
        pr.n = 0;
        pr.blocks = pr.records = pr.drops = pr.repeats = pr.idle = pr.overruns = 0;
        pr.have = 0;
        pr.parsed = pr.synced = pr.have_seq = pr.last_seq = 0;
        pr.frames_ok = pr.seq_errors = pr.cks_errors = 0;
        return 0;
    }
    return E_MSG;
}

int32_t dkm_init(const dk_api *api)
{
    int r;
    pr.ring = ring_store;
    r = api->register_cmds(CMD_FIRST, CMD_COUNT, cmd);
    if (r != 0) {
        api->log((const u8 *)"probe: register_cmds refused", (u32)r);
        pr.ring = 0;
        return -1;
    }
    dl_set_tick_cb((uint8_t *)tick);
    api->log((const u8 *)"probe ready", RING_WORDS);
    return 0;
}

void dkm_fini(const dk_api *api)
{
    (void)api;
    dl_set_tick_cb(0);
    pr.ring = 0;
}
