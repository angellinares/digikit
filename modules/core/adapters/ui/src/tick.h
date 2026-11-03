/* The timers behind dk_ui_tick (<dk/ui.h>): a small table and its due check, with no firmware and no runtime call, so the host can test it
   (modules/core/adapters/ui/host/tests/tick.rs). ui.c owns the table, feeds it the runtime's block clock and calls `ui_ticks_run` from its
   one function of dkrt_poll_add. Times are counted in clock units: `per_s` units make a second (blocks 1500, or UI-loop passes 120 when the
   runtime has no block clock). A period is rounded up to whole units and is at least one. The clock may wrap. */
#ifndef DK_UI_TICK_H
#define DK_UI_TICK_H
#include <stdint.h>
#include <dk/ui.h>

#define UI_TICKS 4u
/* Longest period; a longer one is cut. */
#define UI_TICK_MAX_MS 1000000u

typedef struct ui_tick {
    dk_ui_tick_fn *fn; /* 0: free */
    void *ctx;
    uint32_t ms;
    uint32_t last; /* clock value of the last call, or of the start */
} ui_tick;

typedef struct ui_ticks {
    ui_tick t[UI_TICKS];
} ui_ticks;

static inline uint32_t ui_ticks_active(const ui_ticks *ts)
{
    uint32_t i, n = 0;
    for (i = 0; i < UI_TICKS; i++)
        if (ts->t[i].fn)
            n++;
    return n;
}

/* Set, change or remove the timer of (fn, ctx): `ms` 0 removes it (0 even when there is none). A new or changed timer first fires one period
   after `now`. -1 when fn is 0 or the table is full. */
static inline int ui_ticks_set(ui_ticks *ts, dk_ui_tick_fn *fn, void *ctx, uint32_t ms, uint32_t now)
{
    uint32_t i;
    ui_tick *free_slot = 0;
    if (!fn)
        return -1;
    if (ms > UI_TICK_MAX_MS)
        ms = UI_TICK_MAX_MS;
    for (i = 0; i < UI_TICKS; i++) {
        ui_tick *t = &ts->t[i];
        if (t->fn == fn && t->ctx == ctx) {
            if (ms == 0) {
                t->fn = 0;
            } else {
                t->ms = ms;
                t->last = now;
            }
            return 0;
        }
        if (!t->fn && !free_slot)
            free_slot = t;
    }
    if (ms == 0)
        return 0;
    if (!free_slot)
        return -1;
    free_slot->fn = fn;
    free_slot->ctx = ctx;
    free_slot->ms = ms;
    free_slot->last = now;
    return 0;
}

/* Call every timer that is due at clock value `now`. A callback may set or remove timers, its own included. Returns the timers left. */
static inline uint32_t ui_ticks_run(ui_ticks *ts, uint32_t now, uint32_t per_s)
{
    uint32_t i;
    for (i = 0; i < UI_TICKS; i++) {
        ui_tick *t = &ts->t[i];
        uint32_t need;
        if (!t->fn)
            continue;
        need = (t->ms * per_s + 999u) / 1000u;
        if (need == 0)
            need = 1;
        if (now - t->last >= need) {
            t->last = now;
            t->fn(t->ctx);
        }
    }
    return ui_ticks_active(ts);
}
#endif
