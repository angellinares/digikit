/* The on-device sample picker (docs/design/sample-picker.md): the chooser (sdk/src/chooser/chooser.c) filled with the 16 entries of a sampler
   machine's sample pool as a slot list, plus the pool's side panel (CLEAR, INFO) and the INFO page of an entry. The pure functions are in logic.c.
   Everything here runs in the UI task, so the statics need no lock. */
#include <dk/chooser/chooser.h>
#include <dk/platform/files.h>
#include <dk/sample/picker.h>
#include <dk/sample/pool.h>
#include <dk/chooser/logic.h>

#define TRACKS DK_SAMPLE_POOL_ENTRIES

static const dk_api *g_api;
static uint32_t g_handle;
static dk_ui_menu *g_info; /* the INFO page of a pool entry */
static int g_info_at;
static char g_path[DK_CHOOSER_PATH]; /* scratch: a pool path */
static dk_sample_picker_pick_fn *g_pick;
static void *g_pick_ctx;

static void pool_row(void *ctx, int t, int is_current, char *text);
static int pool_busy(void *ctx);
static int pool_fill(void *ctx, int slot, const char *path);
static void pool_error(void *ctx, int code, char *text);
static void pool_panel(void *ctx, dk_ui_menu *p, int t);
static void pool_done(void *ctx, int slot);
static void fill_info(dk_ui_menu *m, void *ctx);

static const dk_ui_menu_desc info_desc = {
    .name = "SampleInfo", .title = "INFO", .flags = DK_UI_TEXT_PAGE, .fill = fill_info};

/* The slots 0 to 15 as the SMP knob numbers them. */
static const dk_chooser_desc pool_desc = {.title = "SAMPLES",
                                          .count = (int)TRACKS,
                                          .row = pool_row,
                                          .busy = pool_busy,
                                          .root = DK_FILES_SAMPLES,
                                          .fill = pool_fill,
                                          .error_text = pool_error,
                                          .panel = pool_panel,
                                          .done = pool_done};

static const char *entry_path(int t)
{
    g_path[0] = 0;
    if (g_api->sample_pool_path(g_handle, (uint32_t)t, (uint8_t *)g_path, sizeof g_path) < 0)
        g_path[0] = 0;
    return g_path;
}

/* One entry: "NN NAME S 1.2s", the error text, or "-" for an empty slot. */
static void pool_row(void *ctx, int t, int is_current, char *text)
{
    int st = g_api->sample_pool_status(g_handle, (uint32_t)t);
    dk_sample_info info = {0, 0, 0, 0};
    (void)ctx;
    if (st == DK_SAMPLE_READY &&
        g_api->sample_pool_info(g_handle, (uint32_t)t, (uint8_t *)&info) < 0)
        info.frames = info.rate = info.channels = 0;
    dk_sample_pool_row_text(text, t, is_current, st,
                            st == DK_SAMPLE_UNUSED ? "" : dk_path_leaf(entry_path(t)), info.frames,
                            info.rate, info.channels);
}

static int pool_busy(void *ctx)
{
    int t;
    (void)ctx;
    for (t = 0; t < (int)TRACKS; t++) {
        int st = g_api->sample_pool_status(g_handle, (uint32_t)t);
        if (st == DK_SAMPLE_QUEUED || st == DK_SAMPLE_LOADING)
            return 1;
    }
    return 0;
}

/* YES on a file: the pool loads it into the slot (a filled slot is replaced). The result is the slot. */
static int pool_fill(void *ctx, int slot, const char *path)
{
    int rc;
    (void)ctx;
    rc = g_api->sample_pool_set(g_handle, (uint32_t)slot, (const uint8_t *)path);
    return rc < 0 ? rc : slot;
}

static void pool_error(void *ctx, int code, char *text)
{
    char info[32];
    int i;
    (void)ctx;
    dk_sample_pool_err_info(info, code);
    for (i = 0; info[i] && i < DK_UI_TOAST - 1; i++)
        text[i] = info[i];
    text[i] = 0;
}

/* The flow finished with a loaded slot: the machine sets its SMP value to it. */
static void pool_done(void *ctx, int slot)
{
    (void)ctx;
    if (g_pick)
        g_pick(g_pick_ctx, slot);
}

static void clear_cb(void *ctx, int t)
{
    (void)ctx;
    g_api->sample_pool_clear(g_handle, (uint32_t)t);
    dk_chooser_refresh();
}

static void info_cb(void *ctx, int t)
{
    (void)ctx;
    g_info_at = t;
    dk_ui_open(g_info);
}

/* RIGHT on a slot: CLEAR (one press, no confirm) and INFO. */
static void pool_panel(void *ctx, dk_ui_menu *p, int t)
{
    (void)ctx;
    dk_ui_action(p, "CLEAR", clear_cb, 0, t);
    dk_ui_action(p, "INFO", info_cb, 0, t);
}

/* ---- the INFO page ---- */

static void number_line(dk_ui_menu *m, const char *a, uint32_t v, const char *b)
{
    char num[12], row[DK_UI_ROW];
    dk_text_utoa(num, v);
    dk_text_cat(row, a, num);
    dk_text_cat(row, row, b);
    dk_ui_text(m, row);
}

static void fill_info(dk_ui_menu *m, void *ctx)
{
    char row[DK_UI_ROW], num[12], text[32], nm[DK_CHOOSER_TEXT + 1];
    int t = g_info_at, st = g_api->sample_pool_status(g_handle, (uint32_t)t);
    dk_sample_info info = {0, 0, 0, 0};
    (void)ctx;
    dk_text_utoa(num, (uint32_t)t);
    dk_text_cat(row, "SAMPLE ", num);
    dk_ui_text(m, row);
    if (st == DK_SAMPLE_UNUSED) {
        dk_ui_text(m, "EMPTY");
        return;
    }
    if (entry_path(t)[0]) {
        dk_text_trunc(nm, dk_path_leaf(g_path), 16);
        dk_ui_text(m, nm);
        dk_chooser_wrap(m, g_path);
    }
    if (st == DK_SAMPLE_READY &&
        g_api->sample_pool_info(g_handle, (uint32_t)t, (uint8_t *)&info) >= 0) {
        number_line(m, info.channels == 2 ? "STEREO " : "MONO ", info.rate, " HZ");
        number_line(m, "", info.frames, " FRAMES");
        dk_text_secs(text, info.frames, info.rate);
        dk_text_cat(row, "LENGTH ", text);
        dk_ui_text(m, row);
    }
    if (st < 0) {
        dk_sample_pool_err_info(text, st);
        dk_ui_text(m, "STATE");
        dk_chooser_wrap(m, text);
    } else
        dk_ui_text(m, st == DK_SAMPLE_QUEUED    ? "STATE QUEUED"
                      : st == DK_SAMPLE_LOADING ? "STATE LOADING"
                                                : "STATE READY");
}

/* ---- the module's calls ---- */

int dk_sample_picker_init(const dk_api *api, uint32_t handle)
{
    g_handle = 0;
    g_api = api;
    if (!api || !handle ||
        api->size < offsetof(dk_api, sample_pool_set) + sizeof api->sample_pool_set ||
        !api->sample_pool_set || !api->sample_pool_clear || !api->sample_pool_status ||
        !api->sample_pool_info || !api->sample_pool_path)
        return -1;
    if (dk_chooser_init(api) < 0)
        return -1;
    g_handle = handle;
    return 0;
}

void dk_sample_picker_open_pool(int current, dk_sample_picker_pick_fn *pick, void *ctx)
{
    if (!g_handle)
        return;
    g_pick = pick;
    g_pick_ctx = ctx;
    if (!g_info)
        g_info = dk_ui_menu_new(&info_desc);
    if (g_info)
        dk_chooser_open(&pool_desc, current);
}

void dk_sample_picker_fini(void)
{
    dk_chooser_fini();
    g_handle = 0;
    g_pick = 0;
}
