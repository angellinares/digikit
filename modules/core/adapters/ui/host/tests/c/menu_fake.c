/* Host test of core/digikit-menu: the menu's own C source, compiled here with fakes of the runtime's dkrt_* calls and of
   dk_ui_stock_row. The fake <dk/ui.h> (src/lib.rs) shows the screens, and modules/core/adapters/ui/host/tests/menu.rs presses the keys.
   The menu source is included, not linked, so that fk_reset can clear its statics between tests. */
/* an address of a module's dkm_menu is 32 bits on the device; here the fake modules use 1 for fake_dkm_menu */
#include <stdint.h>
#include <dk/ui.h>
static void fake_dkm_menu_call(uint32_t a, dk_ui_menu *m);
#define DK_CALL_MENU(a, m) fake_dkm_menu_call((a), (m))
#include "../../../../../apps/digikit-menu/src/menu.c"
#include <string.h>
#include <stdio.h>
#include <stdlib.h>
#include <assert.h>

/* ---- the fake store and runtime ---------------------------------------------------------------------------------------- */

static struct {
    int n;
    DkrtScan e[DKRT_SCAN_MAX];
    uint32_t arena[DKRT_SCAN_MAX]; /* bytes the entry takes in the arena */
    DkrtSelection sel;
    int scan_result, get_result;
    DkrtBootInfo boot;
    int fx_n;
    DkrtFx fx[MAX_FXR];
    uint32_t send_slots;
    uint32_t arena_free;
    int mod_n;
    DkrtModule mod[4];
    int restarts, restart_result, set_result, selection_sets;
    DkrtSelection set_sel;
    int puts;
    char put_key[2][48];
    uint8_t put_val[2];
    int flushes;
    uint32_t blocks;
} fk;
static int settings_runs, settings_acts;

static dk_ui_act_fn *stock_fn;

void dk_ui_stock_row(uint8_t *stock_menu, const char *text, dk_ui_act_fn *fn, void *ctx)
{
    (void)stock_menu;
    (void)text;
    (void)ctx;
    stock_fn = fn;
}

int32_t dkrt_selection_get(DkrtSelection *out)
{
    if (fk.get_result != DKRT_OK)
        return fk.get_result;
    memcpy(out, &fk.sel, sizeof *out);
    return DKRT_OK;
}

int32_t dkrt_selection_set(const DkrtSelection *sel)
{
    fk.selection_sets++;
    memcpy(&fk.set_sel, sel, sizeof *sel);
    return fk.set_result;
}

int32_t dkrt_restart(void)
{
    fk.restarts++;
    return fk.restart_result;
}

void dkrt_boot_info(DkrtBootInfo *out)
{
    memcpy(out, &fk.boot, sizeof *out);
}

void dkrt_status(DkrtStatus *out)
{
    memset(out, 0, sizeof *out);
    out->runtime = 0x000501;
    out->arena_free = fk.arena_free;
}

void dkrt_health(DkrtHealth *out)
{
    memset(out, 0, sizeof *out);
}

uint32_t dkrt_capacity(DkrtCap *out, uint32_t max)
{
    if (max < 1)
        return 0;
    out[0].kind = DKRT_CAPK_SEND_SLOTS;
    out[0].limit = fk.send_slots;
    out[0].used = 0;
    out[0].largest = 0;
    return 1;
}

uint32_t dkrt_blocks(void)
{
    return fk.blocks;
}

void dkrt_poll_set(uintptr_t f)
{
    (void)f;
}

void dkrt_diag(DkrtDiag *out)
{
    memset(out, 0, sizeof *out);
}

int32_t dkrt_scan(DkrtScan *out, uint32_t max)
{
    if (fk.scan_result < 0)
        return fk.scan_result;
    uint32_t n = (uint32_t)fk.n < max ? (uint32_t)fk.n : max;
    memcpy(out, fk.e, n * sizeof *out);
    return (int32_t)n;
}

uint32_t dkrt_scan_reads(void)
{
    return 0;
}

static void kb_text(uint8_t *dst, uint32_t bytes)
{
    char b[16];
    if (bytes < 1024)
        snprintf(b, sizeof b, "<1K");
    else
        snprintf(b, sizeof b, "%uK", (unsigned)(bytes / 1024));
    memcpy(dst, b, strlen(b) + 1);
}

/* The rule of the runtime in short: the checked entries are taken in list order into an empty arena; one that does not fit is
   flagged and not taken. An unchecked entry is judged against what the checked ones leave. */
int32_t dkrt_fit(const DkrtScan *entries, uint32_t n, const uint8_t *ticked, DkrtFitOut *out)
{
    (void)entries;
    memset(out, 0, sizeof *out);
    uint32_t used = 0;
    for (uint32_t i = 0; i < n; i++) {
        if (ticked[i]) {
            if (used + fk.arena[i] <= fk.arena_free)
                used += fk.arena[i];
            else
                out->rows[i].wont = 1;
        }
    }
    for (uint32_t i = 0; i < n; i++) {
        if (!ticked[i] && used + fk.arena[i] > fk.arena_free)
            out->rows[i].wont = 1;
        if (out->rows[i].wont) {
            snprintf((char *)out->rows[i].reason, DKRT_FIT_REASON, "CPU %uK > %uK FREE", (unsigned)(fk.arena[i] / 1024),
                     (unsigned)((fk.arena_free - used) / 1024));
        }
        if (fk.e[i].flags & DKRT_SCAN_NEEDS)
            kb_text(out->rows[i].mem, fk.arena[i]);
    }
    out->count = n;
    if (used)
        snprintf((char *)out->line, DKRT_FIT_LINE, "DSP 0%% CPU %u%%", (unsigned)(used * 100 / fk.arena_free));
    return DKRT_OK;
}

int32_t dkrt_module(uint32_t index, DkrtModule *out)
{
    if ((int)index >= fk.mod_n)
        return DKRT_E_NOT_FOUND;
    memcpy(out, &fk.mod[index], sizeof *out);
    return DKRT_OK;
}

int32_t dkrt_fx(uint32_t index, DkrtFx *out)
{
    if ((int)index >= fk.fx_n)
        return DKRT_E_NOT_FOUND;
    memcpy(out, &fk.fx[index], sizeof *out);
    return DKRT_OK;
}

int32_t dkrt_settings_put(const uint8_t *key, const uint8_t *value, uint32_t len)
{
    (void)len;
    if (fk.puts < 2) {
        snprintf(fk.put_key[fk.puts], sizeof fk.put_key[0], "%s", (const char *)key);
        fk.put_val[fk.puts] = value[0];
    }
    fk.puts++;
    return DKRT_OK;
}

int32_t dkrt_settings_flush(void)
{
    fk.flushes++;
    return DKRT_OK;
}

int32_t dkrt_module_header(uint32_t first, DkrtModuleHeader *out)
{
    (void)first;
    out->version = 0x000100; /* version 0.1.0 */
    return DKRT_OK;
}

/* ---- control for the tests ---------------------------------------------------------------------------------------------- */

void fk_reset(void)
{
    memset(&fk, 0, sizeof fk);
    settings_runs = settings_acts = 0;
    fk.arena_free = 512 * 1024;
    stock_fn = 0;
    g_main = g_about = DK_HEALTH_MENU = g_mem = g_modules = g_info = 0;
    memset(g_set, 0, sizeof g_set);
    g_safe_shown = 0;
    g_hdr = g_lead = 0;
    STORE = 0;
    g_selection_error = 0;
    NENT = 0;
    memset(TICK, 0, sizeof TICK);
}

/* A .dkm in the store: flags are DKRT_SCAN_* bits; arena is the bytes it takes; fx_slots or machines say what kind it is. */
void fk_add(const char *path, uint32_t flags, uint32_t arena, uint32_t fx_slots, uint32_t machines)
{
    DkrtScan *e = &fk.e[fk.n];
    memset(e, 0, sizeof *e);
    snprintf((char *)e->path, sizeof e->path, "%s", path);
    e->flags = flags;
    e->size = arena / 2;
    e->need_arena = arena;
    e->need_fx_slots = fx_slots;
    e->need_machine_rows = machines;
    fk.arena[fk.n] = arena;
    fk.n++;
}

void fk_select(const char *path)
{
    snprintf((char *)fk.sel.paths[fk.sel.count], DKRT_SEL_PATH, "%s", path);
    fk.sel.count++;
}

void fk_scan_result(int r)
{
    fk.scan_result = r;
}

/* A module the runtime has loaded (dkrt_module), and its effect (dkrt_fx): allowed routes 1 master, 2 send. */
void fk_loaded(const char *path, const char *name)
{
    DkrtModule *m = &fk.mod[fk.mod_n++];
    memset(m, 0, sizeof *m);
    snprintf((char *)m->path, sizeof m->path, "%s", path);
    snprintf((char *)m->name, sizeof m->name, "%s", name);
}

/* A loaded module that has a dkm_menu: the menu gets the fake settings entry, which adds a text row and an action row. */
static void settings_action(void *ctx, int arg)
{
    (void)ctx;
    settings_acts += arg;
}
static void fake_dkm_menu(dk_ui_menu *m, void *ctx)
{
    settings_runs++;
    assert(ctx == 0);
    dk_ui_text(m, "HELD 0");
    dk_ui_action(m, "RELEASE", settings_action, 0, 1);
}
void fk_loaded_menu(const char *path, const char *name)
{
    fk_loaded(path, name);
    fk.mod[fk.mod_n - 1].menu = 1; /* an address on the host is a small number: DK_CALL_MENU maps it */
}
int fk_settings_runs(void) { return settings_runs; }
int fk_settings_acts(void) { return settings_acts; }

static void fake_dkm_menu_call(uint32_t a, dk_ui_menu *m)
{
    assert(a == 1);
    fake_dkm_menu(m, 0);
}

void fk_fx(const char *id, uint32_t allowed, uint32_t saved)
{
    DkrtFx *f = &fk.fx[fk.fx_n++];
    memset(f, 0, sizeof *f);
    f->allowed = allowed;
    f->saved = saved;
    f->running = saved;
    snprintf((char *)f->id, sizeof f->id, "%s", id);
    snprintf((char *)f->tab, sizeof f->tab, "GAN");
}

void fk_send_slots(uint32_t n)
{
    fk.send_slots = n;
}

void fk_restart_result(int r)
{
    fk.restart_result = r;
}

int fk_open_digikit(void)
{
    if (!stock_fn)
        dk_app_settings_rows(0);
    if (!stock_fn)
        return -1;
    stock_fn(0, 0);
    return g_main ? 0 : -1;
}

int fk_restarts(void) { return fk.restarts; }
int fk_selection_sets(void) { return fk.selection_sets; }
int fk_set_count(void) { return (int)fk.set_sel.count; }
const char *fk_set_path(int i) { return (const char *)fk.set_sel.paths[i]; }
int fk_puts(void) { return fk.puts; }
const char *fk_put_key(int i) { return fk.put_key[i]; }
int fk_put_val(int i) { return fk.put_val[i]; }
int fk_flushes(void) { return fk.flushes; }
int fk_tick(int i) { return TICK[i]; }

void fk_get_result(int r) { fk.get_result = r; }
void fk_apply(void) { apply(); }
void fk_boot_safe(void) { fk.boot.safe = 1; fk.boot.reason = DKRT_SAFE_COUNTER; }
void fk_boot_read_failure(void) { fk.boot.safe = 1; fk.boot.reason = DKRT_SAFE_READ; fk.boot.count = 1; }

void fk_set_tick(int row, int on) { set_tick(row, on); }
