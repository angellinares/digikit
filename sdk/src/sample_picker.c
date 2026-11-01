/* The on-device sample picker (docs/design/sample-picker.md): the pool list of a sampler machine (opened by a click on its SMP knob), a
   folder browser and an INFO page on <dk/ui.h>.
   The logic that needs no API (sort, text, paths) is in picker_logic.c. Everything here runs in the UI task: the fill, action, panel,
   back and listing-notify callbacks and the timer, so the statics need no lock. Menus are built once and kept. */
#include <dk/files.h>
#include <dk/sample_picker.h>
#include <dk/sample_track.h>
#include "picker_logic.h"

#define TRACKS DK_STRACKS
#define TICK_MS 250

enum { ST_IDLE, ST_READING, ST_DONE, ST_ERROR };

static const dk_api *g_api;
static uint32_t g_handle, g_client;
static int g_has_path;
static dk_ui_menu *g_tracks, *g_browser, *g_info; /* g_tracks is the pool list */

static int g_cur;              /* the pool index the SMP value points at (marked in the list); -1 none */
static dk_picker_pick_fn *g_pick;
static void *g_pick_ctx;
static char g_dir[PK_PATH];    /* the folder shown, root-relative, "" is the root */
static char g_path[PK_PATH];   /* scratch: a path to select or show */
static pk_row g_rows[PK_ROWS];
static uint8_t g_idx[PK_ROWS]; /* g_rows in sort order */
static int g_n, g_more, g_state, g_err;
static int32_t g_ticket;
static dk_file_entry g_entry;
static int g_add_err;          /* the last add the runtime or the picker refused (a DK_SAMPLE_E_*), shown on the + ADD row; 0 none */
static int g_ticking;
static int g_info_file, g_info_at; /* INFO shows a pool entry (g_info_at) or a listed file (sorted index g_info_at) */

static void fill_tracks(dk_ui_menu *m, void *ctx);
static void fill_browser(dk_ui_menu *m, void *ctx);
static void fill_info(dk_ui_menu *m, void *ctx);
static void panel_tracks(dk_ui_menu *p, int row, void *ctx);
static void panel_browser(dk_ui_menu *p, int row, void *ctx);
static int back_browser(void *ctx);
static void check_tracks(void *ctx, int row, int on);
static void check_browser(void *ctx, int row, int on);
static void open_browser(void);
static void choose_entry(int t);
static void on_notify(uint32_t client, uint32_t request, int32_t hint, uint8_t *ctx);
static void up_cb(void *ctx, int arg);
static void row_cb(void *ctx, int at);

static const dk_ui_menu_desc tracks_desc = {.name = "Samples", .title = "SAMPLES", .flags = DK_UI_LIST, .fill = fill_tracks,
                                            .panel = panel_tracks, .check = check_tracks};
static const dk_ui_menu_desc browser_desc = {.name = "Browse", .title = "SAMPLES", .flags = DK_UI_LIST, .fill = fill_browser,
                                             .panel = panel_browser, .check = check_browser, .back = back_browser};
static const dk_ui_menu_desc info_desc = {.name = "SampleInfo", .title = "INFO", .flags = DK_UI_TEXT_PAGE, .fill = fill_info};

static int slen(const char *s)
{
    int n = 0;
    while (s[n])
        n++;
    return n;
}

/* s after prefix into out, at most DK_UI_ROW - 1 bytes. */
static void cat(char *out, const char *a, const char *b)
{
    int n = 0;
    while (*a && n < DK_UI_ROW - 1)
        out[n++] = *a++;
    while (*b && n < DK_UI_ROW - 1)
        out[n++] = *b++;
    out[n] = 0;
}

/* A text page shows about 16 characters next to its title column (seen on the DN2 screen). */
#define WRAP 16

/* Text rows of at most WRAP characters: broken at a space (dropped) or after a '/' when there is one, else at the width. */
static void wrap(dk_ui_menu *m, const char *s)
{
    char row[DK_UI_ROW];
    while (*s) {
        int n = slen(s), cut = n, i;
        if (n > WRAP) {
            cut = WRAP;
            for (i = WRAP; i > 0; i--)
                if (s[i] == ' ' || s[i - 1] == '/') {
                    cut = i;
                    break;
                }
        }
        for (i = 0; i < cut; i++)
            row[i] = s[i];
        row[cut] = 0;
        dk_ui_text(m, row);
        s += cut;
        while (*s == ' ')
            s++;
    }
}

/* ---- the runtime's view of the pool ---- */

static const char *entry_path(int t)
{
    g_path[0] = 0;
    if (g_has_path && g_api->sample_pool_path(g_handle, (uint32_t)t, (uint8_t *)g_path, sizeof g_path) < 0)
        g_path[0] = 0;
    return g_path;
}

static void tick(void *ctx)
{
    (void)ctx;
    if (g_tracks && dk_ui_is_open(g_tracks))
        dk_ui_refresh(g_tracks); /* fill_tracks stops the timer when no load is left */
    else {
        dk_ui_tick(tick, 0, 0);
        g_ticking = 0;
    }
}

/* The pool list: row 0 is "+ ADD" (with the refusal of the last add, if any), rows 1 to 16 are the entries 0 to 15 (holes show "-"). */
static void fill_tracks(dk_ui_menu *m, void *ctx)
{
    char row[DK_UI_ROW], tail[20];
    int t, active = 0;
    (void)ctx;
    if (g_add_err) {
        pk_err_tail(tail, g_add_err);
        cat(row, "+ ADD  ", tail);
    } else
        cat(row, "+ ADD", "");
    dk_ui_text(m, row);
    for (t = 0; t < (int)TRACKS; t++) {
        int st = g_api->sample_pool_status(g_handle, (uint32_t)t);
        dk_sample_info info = {0, 0, 0, 0};
        if (st == DK_SAMPLE_READY && g_api->sample_pool_info(g_handle, (uint32_t)t, (uint8_t *)&info) < 0)
            info.frames = info.rate = info.channels = 0;
        if (st == DK_SAMPLE_QUEUED || st == DK_SAMPLE_LOADING)
            active = 1;
        pk_pool_row(row, t, t == g_cur, st, st == DK_SAMPLE_UNUSED ? "" : pk_leaf(entry_path(t)), info.frames, info.rate, info.channels);
        dk_ui_text(m, row);
    }
    if (active && !g_ticking && dk_ui_has(DK_UI_CAP_TICK) && dk_ui_tick(tick, 0, TICK_MS) == 0)
        g_ticking = 1;
    else if (!active && g_ticking) {
        dk_ui_tick(tick, 0, 0);
        g_ticking = 0;
    }
}

/* ---- the folder browser ---- */

static void relist(void)
{
    if (g_state == ST_READING)
        g_api->files_cancel(g_client, (uint32_t)g_ticket);
    g_n = g_more = 0;
    g_state = ST_READING;
    g_ticket = g_api->files_list(g_client, DK_FILES_SAMPLES, (const uint8_t *)g_dir, on_notify, 0);
    if (g_ticket <= 0) {
        g_state = ST_ERROR;
        g_err = g_ticket < 0 ? g_ticket : DK_SAMPLE_E_CONTRACT;
    }
}

static void add_entry(void)
{
    const char *leaf = pk_leaf((const char *)g_entry.path);
    pk_row r;
    int i, full;
    if ((g_entry.kind != DK_FILE_FILE && g_entry.kind != DK_FILE_DIRECTORY) || !leaf[0])
        return;
    if (slen(leaf) >= PK_NAME) { /* a name that does not fit is not shown; it counts as hidden */
        g_more++;
        return;
    }
    for (i = 0; leaf[i]; i++)
        r.name[i] = leaf[i];
    for (; i < PK_NAME; i++)
        r.name[i] = 0;
    r.dir = g_entry.kind == DK_FILE_DIRECTORY;
    r.size = (g_entry.flags & DK_FILE_SIZE_VALID) && !r.dir ? g_entry.size_bytes : 0;
    full = g_n == PK_ROWS;
    if (!pk_insert(g_rows, g_idx, &g_n, PK_ROWS, &r) || full)
        g_more++; /* one row is left out, this one or the last of the order */
}

/* The listing is ready: poll until pending. Rows are kept in sort order as they come; the screen shows them at the end. */
static void on_notify(uint32_t client, uint32_t request, int32_t hint, uint8_t *ctx)
{
    (void)hint;
    (void)ctx;
    if (client != g_client || (int32_t)request != g_ticket || g_state != ST_READING)
        return;
    for (;;) {
        int32_t rc = g_api->files_poll(client, request, (uint8_t *)&g_entry, sizeof g_entry);
        if (rc == DK_FILES_ROW) {
            add_entry();
            continue;
        }
        if (rc == DK_FILES_END)
            g_state = ST_DONE;
        else if (rc < 0) {
            g_state = ST_ERROR;
            g_err = rc;
        }
        break;
    }
    if (g_state != ST_READING)
        dk_ui_refresh(g_browser);
}

static void fill_browser(dk_ui_menu *m, void *ctx)
{
    char row[DK_UI_ROW], nm[PK_TEXT + 1], sz[8], tmp[PK_TEXT + 1];
    int i;
    (void)ctx;
    cat(tmp, "..  /", g_dir);
    pk_trunc(row, tmp, PK_TEXT);
    dk_ui_text(m, row);
    if (g_state == ST_READING) {
        dk_ui_text(m, "READING...");
        return;
    }
    if (g_state == ST_ERROR) {
        char num[12];
        pk_utoa(num, (uint32_t)(g_err < 0 ? -g_err : g_err));
        cat(row, "LIST ERROR -", num);
        dk_ui_text(m, row);
        return;
    }
    for (i = 0; i < g_n; i++) {
        const pk_row *r = &g_rows[g_idx[i]];
        if (r->dir) {
            pk_trunc(nm, r->name, PK_TEXT - 2);
            cat(row, "> ", nm);
        } else if (r->size) {
            pk_size_text(sz, r->size);
            pk_trunc(nm, r->name, PK_TEXT - 3 - slen(sz));
            cat(tmp, "  ", nm);
            cat(row, tmp, " ");
            cat(row, row, sz);
        } else {
            pk_trunc(nm, r->name, PK_TEXT - 2);
            cat(row, "  ", nm);
        }
        dk_ui_text(m, row);
    }
    if (g_n == 0)
        dk_ui_text(m, "NO SAMPLES HERE");
    if (g_more) {
        char num[12];
        pk_utoa(num, (uint32_t)g_more);
        cat(tmp, "+", num);
        cat(row, tmp, " MORE");
        dk_ui_text(m, row);
    }
}

/* Leaves the browser for the track list. The browser is closed here and the track list is opened again: a list that opened another
   view is not on the stack any more when that view closes (seen on the device screen), and dk_ui_open only refreshes a list that is. */
static void leave_browser(void)
{
    if (g_state == ST_READING)
        g_api->files_cancel(g_client, (uint32_t)g_ticket);
    g_state = ST_IDLE;
    dk_ui_close(g_browser);
    dk_ui_open(g_tracks);
}

/* NO and the ".." row: up one folder, or out of the browser at the root. Always consumes the key. */
static int back_browser(void *ctx)
{
    (void)ctx;
    if (pk_parent(g_dir)) {
        relist();
        dk_ui_refresh(g_browser);
    } else
        leave_browser();
    return 1;
}

static void up_cb(void *ctx, int arg)
{
    (void)ctx;
    (void)arg;
    back_browser(0);
}

/* YES on a file: add it to the pool. The index (the new one, or the entry that already holds the file) goes to the pick callback, so the
   machine sets its SMP value to it. A refusal shows on the + ADD row of the list and nothing is picked. */
static void select_file(int at)
{
    int rc;
    if (pk_join(g_path, sizeof g_path, g_dir, g_rows[g_idx[at]].name) < 0)
        rc = DK_SAMPLE_E_SEL_PATH;
    else
        rc = g_api->sample_pool_add(g_handle, (uint8_t *)g_path);
    g_add_err = rc < 0 ? rc : 0;
    if (rc >= 0)
        g_cur = rc;
    leave_browser();
    if (rc >= 0 && g_pick)
        g_pick(g_pick_ctx, rc);
}

static void row_cb(void *ctx, int at)
{
    (void)ctx;
    if (at < 0 || at >= g_n || g_state != ST_DONE)
        return;
    if (g_rows[g_idx[at]].dir) {
        if (pk_join(g_path, sizeof g_path, g_dir, g_rows[g_idx[at]].name) < 0) {
            g_state = ST_ERROR;
            g_err = DK_SAMPLE_E_SEL_PATH;
        } else {
            int i;
            for (i = 0; g_path[i]; i++)
                g_dir[i] = g_path[i];
            g_dir[i] = 0;
            relist();
        }
        dk_ui_refresh(g_browser);
    } else
        select_file(at);
}

static void select_cb(void *ctx, int at)
{
    (void)ctx;
    select_file(at);
}

static void open_info(int file, int at)
{
    g_info_file = file;
    g_info_at = at;
    dk_ui_open(g_info);
}

static void file_info_cb(void *ctx, int at)
{
    (void)ctx;
    open_info(1, at);
}

/* YES on a row of a list that has a check callback comes here and is the only thing that happens: the stock handler of a list without
   one closes the list after the row's action. The mark core/ui sets is taken off at once, so a row never shows a check. */
static void check_browser(void *ctx, int row, int on)
{
    (void)ctx;
    (void)on;
    dk_ui_check(g_browser, row, 0);
    if (row == 0)
        up_cb(0, 0);
    else if (g_state == ST_DONE)
        row_cb(0, row - 1);
}

/* YES on an entry: the machine's SMP value becomes this entry and the list closes, back to the machine page. A hole is not chosen. */
static void choose_entry(int t)
{
    if (t < 0 || t >= (int)TRACKS || g_api->sample_pool_status(g_handle, (uint32_t)t) == DK_SAMPLE_UNUSED)
        return;
    g_cur = t;
    dk_ui_close(g_tracks);
    if (g_pick)
        g_pick(g_pick_ctx, t);
}

static void check_tracks(void *ctx, int row, int on)
{
    (void)ctx;
    (void)on;
    dk_ui_check(g_tracks, row, 0);
    if (row == 0)
        open_browser();
    else
        choose_entry(row - 1);
}

/* RIGHT on a listed file: row 0 is "..", the files are rows 1 to g_n. */
static void panel_browser(dk_ui_menu *p, int row, void *ctx)
{
    (void)ctx;
    if (g_state != ST_DONE || row < 1 || row > g_n || g_rows[g_idx[row - 1]].dir)
        return;
    dk_ui_action(p, "SELECT", select_cb, 0, row - 1);
    dk_ui_action(p, "INFO", file_info_cb, 0, row - 1);
}

/* ---- the pool list: actions ---- */

static void open_browser(void)
{
    g_add_err = 0;
    relist();
    dk_ui_open(g_browser);
}

static void clear_cb(void *ctx, int t)
{
    (void)ctx;
    g_api->sample_pool_clear(g_handle, (uint32_t)t);
    dk_ui_refresh(g_tracks);
}

static void track_info_cb(void *ctx, int t)
{
    (void)ctx;
    open_info(0, t);
}

/* RIGHT on an entry row (row 0 is + ADD and has no panel): CLEAR and INFO. */
static void panel_tracks(dk_ui_menu *p, int row, void *ctx)
{
    (void)ctx;
    if (row < 1 || row > (int)TRACKS)
        return;
    dk_ui_action(p, "CLEAR", clear_cb, 0, row - 1);
    dk_ui_action(p, "INFO", track_info_cb, 0, row - 1);
}

/* ---- the INFO page ---- */

static void number_line(dk_ui_menu *m, const char *a, uint32_t v, const char *b)
{
    char num[12], row[DK_UI_ROW], tmp[DK_UI_ROW];
    pk_utoa(num, v);
    cat(tmp, a, num);
    cat(row, tmp, b);
    dk_ui_text(m, row);
}

static void fill_info(dk_ui_menu *m, void *ctx)
{
    char row[DK_UI_ROW], num[12], text[32], nm[PK_TEXT + 1];
    (void)ctx;
    if (g_info_file) {
        const pk_row *r;
        if (g_info_at < 0 || g_info_at >= g_n)
            return;
        r = &g_rows[g_idx[g_info_at]];
        pk_trunc(nm, r->name, WRAP);
        dk_ui_text(m, nm);
        if (pk_join(g_path, sizeof g_path, g_dir, r->name) >= 0)
            wrap(m, g_path);
        if (r->size)
            number_line(m, "SIZE ", r->size, " BYTES");
        return;
    }
    {
        int t = g_info_at, st = g_api->sample_pool_status(g_handle, (uint32_t)t);
        dk_sample_info info = {0, 0, 0, 0};
        pk_utoa(num, (uint32_t)t);
        cat(row, "SAMPLE ", num);
        dk_ui_text(m, row);
        if (st == DK_SAMPLE_UNUSED) {
            dk_ui_text(m, "EMPTY");
            return;
        }
        if (entry_path(t)[0]) {
            pk_trunc(nm, pk_leaf(g_path), WRAP);
            dk_ui_text(m, nm);
            wrap(m, g_path);
        }
        if (st == DK_SAMPLE_READY && g_api->sample_pool_info(g_handle, (uint32_t)t, (uint8_t *)&info) >= 0) {
            number_line(m, info.channels == 2 ? "STEREO " : "MONO ", info.rate, " HZ");
            number_line(m, "", info.frames, " FRAMES");
            pk_secs_text(text, info.frames, info.rate);
            cat(row, "LENGTH ", text);
            dk_ui_text(m, row);
        }
        if (st < 0) {
            pk_err_info(text, st);
            dk_ui_text(m, "STATE");
            wrap(m, text);
        } else
            dk_ui_text(m, st == DK_SAMPLE_QUEUED ? "STATE QUEUED" : st == DK_SAMPLE_LOADING ? "STATE LOADING" : "STATE READY");
    }
}

/* ---- the module's calls ---- */

static int api_has(const dk_api *a, uint32_t end)
{
    return a->size >= end;
}

int dk_picker_init(const dk_api *api, uint32_t handle)
{
    g_client = 0;
    g_has_path = 0;
    g_handle = 0;
    g_api = api;
    if (!api || !handle || !api_has(api, offsetof(dk_api, sample_pool_info) + sizeof api->sample_pool_info) || !api->files_client
        || !api->files_list || !api->files_poll || !api->files_cancel || !api->sample_pool_add || !api->sample_pool_clear
        || !api->sample_pool_status || !api->sample_pool_info)
        return -1;
    {
        int32_t c = api->files_client();
        if (c <= 0)
            return -1;
        g_client = (uint32_t)c;
    }
    g_handle = handle;
    g_has_path = api_has(api, offsetof(dk_api, sample_pool_path) + sizeof api->sample_pool_path) && api->sample_pool_path;
    return 0;
}

void dk_picker_open_pool(int current, dk_picker_pick_fn *pick, void *ctx)
{
    if (!g_handle || !g_client || !dk_ui_has(DK_UI_CAP_LIST) || !dk_ui_has(DK_UI_CAP_TEXT_PAGE))
        return;
    g_cur = current;
    g_pick = pick;
    g_pick_ctx = ctx;
    g_add_err = 0;
    if (!g_tracks)
        g_tracks = dk_ui_menu_new(&tracks_desc);
    if (!g_browser)
        g_browser = dk_ui_menu_new(&browser_desc);
    if (!g_info)
        g_info = dk_ui_menu_new(&info_desc);
    if (g_tracks && g_browser && g_info)
        dk_ui_open(g_tracks);
}

void dk_picker_fini(void)
{
    if (g_ticking)
        dk_ui_tick(tick, 0, 0);
    g_ticking = 0;
    if (g_state == ST_READING && g_api && g_client)
        g_api->files_cancel(g_client, (uint32_t)g_ticket);
    g_state = ST_IDLE;
    g_handle = g_client = 0;
    g_pick = 0;
}
