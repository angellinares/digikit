/* Host test of dk_ui_tick. Part 1 runs the timer table of core/ui (src/tick.h, the same header ui.c uses) with a clock the test moves.
   Part 2 is a content module's use of the call through the fake <dk/ui.h> (src/lib.rs): a screen that redraws while a "load" runs. */
#include <stddef.h>
#include <stdint.h>
#include <dk/ui.h>
#include "../../../src/tick.h"

/* ---- part 1: the table ---- */

static ui_ticks ts;
static int calls[6];
static uint32_t stop_after[6];

static void cb(void *ctx)
{
    int id = (int)(intptr_t)ctx;
    calls[id]++;
    if (stop_after[id] && calls[id] >= (int)stop_after[id])
        ui_ticks_set(&ts, cb, ctx, 0, 0);
}

static void cb_other(void *ctx)
{
    (void)ctx;
    calls[5]++;
}

void tk_reset(void)
{
    for (int i = 0; i < 6; i++) {
        calls[i] = 0;
        stop_after[i] = 0;
    }
    ts = (ui_ticks){0};
}
int tk_set(int id, uint32_t ms, uint32_t now) { return ui_ticks_set(&ts, cb, (void *)(intptr_t)id, ms, now); }
int tk_set_other(uint32_t ms, uint32_t now) { return ui_ticks_set(&ts, cb_other, 0, ms, now); }
int tk_set_null(void) { return ui_ticks_set(&ts, 0, 0, 10, 0); }
uint32_t tk_run(uint32_t now, uint32_t per_s) { return ui_ticks_run(&ts, now, per_s); }
int tk_calls(int id) { return calls[id]; }
void tk_stop_after(int id, uint32_t n) { stop_after[id] = n; }
uint32_t tk_active(void) { return ui_ticks_active(&ts); }

/* ---- part 2: a module that redraws while a load runs ---- */

static dk_ui_menu *screen;
static int loading_polls;
static int fills;
static int load_done;

static void fill(dk_ui_menu *m, void *ctx)
{
    (void)ctx;
    fills++;
    dk_ui_text(m, load_done ? "READY" : "LOADING...");
}

static void poll(void *ctx)
{
    (void)ctx;
    loading_polls++;
    if (load_done)
        dk_ui_tick(poll, 0, 0); /* done: stop from inside the callback */
    dk_ui_refresh(screen);
}

static const dk_ui_menu_desc desc = {.name = "ticks", .title = "TICKS", .fill = fill};

int tk_module_open(void)
{
    loading_polls = fills = load_done = 0;
    screen = dk_ui_menu_new(&desc);
    if (!screen || dk_ui_open(screen) != 0)
        return -1;
    return dk_ui_tick(poll, 0, 100);
}
void tk_module_finish(void) { load_done = 1; }
int tk_module_polls(void) { return loading_polls; }
int tk_module_fills(void) { return fills; }
int tk_module_stop(void) { return dk_ui_tick(poll, 0, 0); }
int tk_module_restart(uint32_t ms) { return dk_ui_tick(poll, 0, ms); }
int tk_module_has_cap(void) { return dk_ui_has(DK_UI_CAP_TICK); }
