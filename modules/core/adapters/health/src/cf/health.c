/* core/health: CPU load, DSP block time and heap, for the DIGIKIT HEALTH page (docs/findings/41-digikit-health.md).
 *
 * Time base: the DSP block interrupt (1500 per second). health_block runs at the entry of its handler (tramp.s), counts the
 * block, reads the first word of the record copy (the DSP's own cycle count of its last block handler: it reads EMUCLK at the start
 * and at the end of the handler and stores the difference there) and, every HEALTH_WINDOW blocks, closes a window: the average and
 * the maximum of the DSP's count, and the iterations of the idle loop counted in it.
 *
 * Idle: health_idle is incremented by the loop that replaces the end of the boot thread (tramp.s), which runs only when no task
 * is ready. The reference is the best (highest) window since boot, so 100 % idle is "as idle as this boot has been".
 *
 * Heap: health_heap walks the free lists of the firmware's buddy allocator (finding 01) under the allocator's own mutex, bounded.
 * Called by the runtime (dkrt_health) when the HEALTH page refreshes; it does nothing otherwise.
 *
 * Interface for the runtime (C ABI, no struct results):
 *   void health_read(HealthRaw *out);   the counters and the last closed window (no walk)
 *   void health_heap(HealthRaw *out);   adds the heap figures (the walk)
 *
 * Cost, per block: the hook (a push/pop of four registers, a call) and about 25 instructions; per second: one window close.
 * The idle loop is the CPU's spare time: it costs nothing that a task could use. */

typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned char u8;

#define HEALTH_MAGIC 0x484c5448u /* "HLTH" */
#define HEALTH_VERSION 1u
#define HEALTH_WINDOW 1500u /* blocks per window: one second */
#define HEALTH_BAD 4000000u /* a cycle count above this is not a block time (no frame yet) */
#define HEALTH_WALK 4096u /* free-list nodes visited per call at most */

/* HealthRaw: 28 words. The runtime copies it; fields only grow at the end. */
typedef struct HealthRaw {
    u32 magic;
    u32 version;
    u32 blocks;     /* DSP blocks seen since boot */
    u32 windows;    /* windows closed */
    u32 idle_total; /* idle-loop iterations since boot (wraps) */
    u32 idle_win;   /* iterations in the last closed window */
    u32 idle_ref;   /* the highest window so far, from the second window on */
    u32 dsp_avg;    /* average cycle count of the last closed window */
    u32 dsp_max;    /* its maximum */
    u32 dsp_n;      /* blocks with a valid count in it */
    u32 dsp_last;   /* the count read in the last block */
    u32 flags;      /* bit 0: a window is closed; bit 1: the heap figures are valid; bit 2: the walk was cut short; bit 3: bad list */
    u32 heap_total;
    u32 heap_free;
    u32 heap_largest;
    u32 heap_allocs;
    u32 heap_free_blocks;
    u32 heap_walked;
    u32 pad[2];
} HealthRaw;

#define HF_WINDOW 1u
#define HF_HEAP 2u
#define HF_CUT 4u
#define HF_BAD 8u

/* the idle loop's counter: one word, written only by the idle loop. It fills a whole 16-byte cache line: patchkit places hook tails
 * (code) behind the bss, and a data line that shares a cache line with code is a flush hazard (the proof's flush check). */
typedef struct HealthIdle {
    volatile u32 n;
    u32 pad[3];
} HealthIdle;
HealthIdle health_idle_line __attribute__((aligned(16)));
#define health_idle (health_idle_line.n)

/* the A/B switch of the proof (finding 41): non-zero makes the block hook return at once */
volatile u32 health_off __attribute__((aligned(4)));

/* the window state, written by the block hook only; `pub` is what the readers see (seq even = stable) */
typedef struct HealthState {
    u32 blocks;
    u32 pos;
    u32 acc;
    u32 n;
    u32 mx;
    u32 last;
    u32 idle_at;
    u32 windows;
    u32 seq;
    u32 pub_avg;
    u32 pub_max;
    u32 pub_n;
    u32 pub_idle;
    u32 ref;
    u32 pad[2];
} HealthState;

volatile HealthState health_hs __attribute__((aligned(16)));

extern volatile u32 health_rec_lit;   /* the literal: address of the record copy */
extern volatile u32 health_desc_lit;  /* the literal: address of the allocator descriptor */
extern volatile u32 health_mutex_lit; /* the literal: address of the allocator mutex */
extern volatile u32 health_lock_lit;  /* the literal: address of the firmware's mutex lock function */
extern volatile u32 health_unlock_lit;

/* called from the block interrupt, at the entry of its handler (tramp.s); d0/d1/a0/a1 are saved there */
void health_block(void)
{
    volatile HealthState *s = &health_hs;
    u32 x;
    if (health_off)
        return;
    x = *(volatile u32 *)health_rec_lit;
    s->blocks++;
    s->last = x;
    if (x != 0u && x < HEALTH_BAD) {
        s->acc += x;
        s->n++;
        if (x > s->mx)
            s->mx = x;
    }
    if (++s->pos >= HEALTH_WINDOW) {
        u32 idle = health_idle;
        u32 w = idle - s->idle_at;
        s->idle_at = idle;
        s->seq++; /* odd: being written */
        s->pub_idle = w;
        s->pub_n = s->n;
        s->pub_avg = s->n ? s->acc / s->n : 0u;
        s->pub_max = s->mx;
        s->windows++;
        if (s->windows >= 2u && w > s->ref)
            s->ref = w; /* the first window is the boot's */
        s->seq++;
        s->pos = 0u;
        s->acc = 0u;
        s->n = 0u;
        s->mx = 0u;
    }
}

void health_read(HealthRaw *o)
{
    volatile HealthState *s = &health_hs;
    u32 q, avg, mx, n, idle;
    do {
        q = s->seq;
        avg = s->pub_avg;
        mx = s->pub_max;
        n = s->pub_n;
        idle = s->pub_idle;
    } while ((q & 1u) || q != s->seq);
    o->magic = HEALTH_MAGIC;
    o->version = HEALTH_VERSION;
    o->blocks = s->blocks;
    o->windows = s->windows;
    o->idle_total = health_idle;
    o->idle_win = idle;
    o->idle_ref = s->ref;
    o->dsp_avg = avg;
    o->dsp_max = mx;
    o->dsp_n = n;
    o->dsp_last = s->last;
    o->flags = s->windows ? HF_WINDOW : 0u;
}

/* The allocator (finding 01): descriptor words desc[0] initialised, [1] base, [2] bitmap, [3] free-list heads (one u32 per order,
 * a block index in 16-byte units, negative = empty), [4] highest order, [5] block count, [6] live allocations. A free block holds,
 * at base + 16 * index, its previous index at +4 and its next at +8 (negative = none). */
void health_heap(HealthRaw *o)
{
    const u32 *d = (const u32 *)health_desc_lit;
    void (*lock)(u32) = (void (*)(u32))health_lock_lit;
    void (*unlock)(u32) = (void (*)(u32))health_unlock_lit;
    u32 mutex = health_mutex_lit;
    u32 free_b = 0u, largest = 0u, nfree = 0u, walked = 0u, fl = 0u;
    u32 order, maxo, nblocks, base;
    const u32 *heads;
    if (d == 0 || lock == 0 || unlock == 0 || mutex == 0u)
        return;
    lock(mutex);
    if (d[0] == 0u) { /* not initialised: the heap is one free block */
        unlock(mutex);
        return;
    }
    base = d[1];
    heads = (const u32 *)d[3];
    maxo = d[4];
    nblocks = d[5];
    if (maxo > 31u) {
        fl |= HF_BAD;
        maxo = 0u;
    }
    for (order = maxo + 1u; order-- > 0u && !(fl & HF_BAD);) {
        int idx = (int)heads[order];
        while (idx >= 0) {
            u32 bsize = 16u << order;
            if ((u32)idx >= nblocks) {
                fl |= HF_BAD;
                break;
            }
            if (walked >= HEALTH_WALK) {
                fl |= HF_CUT;
                break;
            }
            walked++;
            nfree++;
            free_b += bsize;
            if (bsize > largest)
                largest = bsize;
            idx = *(const int *)(base + 16u * (u32)idx + 8u);
        }
        if (fl & HF_CUT)
            break;
    }
    o->heap_allocs = d[6];
    o->heap_total = nblocks * 16u;
    unlock(mutex);
    o->heap_free = free_b;
    o->heap_largest = largest;
    o->heap_free_blocks = nfree;
    o->heap_walked = walked;
    o->flags |= HF_HEAP | (fl & (HF_CUT | HF_BAD));
}
