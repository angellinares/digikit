/* The chooser component (sdk/include/dk/chooser/chooser.h, docs/design/ui-rules.md): a list of slots, a folder browser and a file INFO page on
   <dk/platform/ui.h>. The logic that needs no API (sort, text, paths) is in logic.c. Everything here runs in the UI task: the fill, action,
   panel, back and listing-notify callbacks and the timer, so the statics need no lock. Menus are built once and kept. */
#include <dk/chooser/chooser.h>
#include <dk/platform/files.h>
#include <dk/chooser/logic.h>

#define TICK_MS 250
/* A text page shows about 16 characters next to its title column (seen on the DN2 screen). */
#define WRAP 16

enum { ST_IDLE, ST_READING, ST_DONE, ST_ERROR };

static const dk_api *g_api;
static uint32_t g_client;
static const dk_chooser_desc *g_d;
static dk_ui_menu *g_list, *g_browser, *g_finfo;
static dk_ui_menu_desc g_list_desc, g_browser_desc, g_finfo_desc;

static int g_cur;                    /* the slot marked in the list; -1 none */
static int g_slot;                   /* the slot the browser loads into */
static char g_dir[DK_CHOOSER_PATH];  /* the folder shown, root-relative, "" is the root */
static char g_path[DK_CHOOSER_PATH]; /* scratch: a path to fill or show */
static dk_chooser_row g_rows[DK_CHOOSER_ROWS];
static uint8_t g_idx[DK_CHOOSER_ROWS]; /* g_rows in sort order */
static int g_n, g_more, g_state, g_err;
static int32_t g_ticket;
static dk_file_entry g_entry;
static int g_ticking;
static int g_info_at; /* the file INFO page shows the file at this sorted index */

static void fill_list(dk_ui_menu *m, void *ctx);
static void fill_browser(dk_ui_menu *m, void *ctx);
static void fill_finfo(dk_ui_menu *m, void *ctx);
static void panel_list(dk_ui_menu *p, int row, void *ctx);
static void panel_browser(dk_ui_menu *p, int row, void *ctx);
static int back_browser(void *ctx);
static void check_list(void *ctx, int row, int on);
static void check_browser(void *ctx, int row, int on);
static void on_notify(uint32_t client, uint32_t request, int32_t hint, uint8_t *ctx);
static void tick(void *ctx);

static int slen(const char *s)
{
    int n = 0;
    while (s[n])
        n++;
    return n;
}

void dk_chooser_wrap(dk_ui_menu *m, const char *s)
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

/* ---- leaving ---- */

static void stop_listing(void)
{
    if (g_state == ST_READING)
        g_api->files_cancel(g_client, (uint32_t)g_ticket);
    g_state = ST_IDLE;
}

static void stop_timer(void)
{
    if (g_ticking)
        dk_ui_tick(tick, 0, 0);
    g_ticking = 0;
}

/* The whole flow ends: every level closes in one step, back on the page the flow started from. */
static void close_all(void)
{
    stop_listing();
    dk_ui_close(g_browser);
    dk_ui_close(g_list);
    stop_timer();
}

/* The file was loaded: the flow ends with `result` (the slot). */
static void finish(int result)
{
    const dk_chooser_desc *d = g_d;
    close_all();
    if (d && d->done)
        d->done(d->ctx, result);
}

/* An error never opens a dialog: the flow closes and the page shows the text. */
static void fail(int code)
{
    char text[DK_UI_TOAST], num[12];
    const dk_chooser_desc *d = g_d;
    close_all();
    text[0] = 0;
    if (d && d->error_text)
        d->error_text(d->ctx, code, text);
    if (!text[0]) {
        dk_text_utoa(num, (uint32_t)(code < 0 ? -code : code));
        dk_text_cat(text, "ERROR -", num);
    }
    dk_ui_toast(text);
}

/* NO in the browser at the root: back to the list. The browser is closed here and the list is opened again: a list that opened another
   view is not on the stack any more when that view closes (seen on the device screen), and dk_ui_open only refreshes a list that is. */
static void leave_browser(void)
{
    stop_listing();
    dk_ui_close(g_browser);
    dk_ui_open(g_list);
}

/* ---- the list ---- */

static void tick(void *ctx)
{
    (void)ctx;
    if (g_list && dk_ui_is_open(g_list))
        dk_ui_refresh(g_list); /* fill_list stops the timer when no row is busy any more */
    else {
        dk_ui_tick(tick, 0, 0);
        g_ticking = 0;
    }
}

static void fill_list(dk_ui_menu *m, void *ctx)
{
    char row[DK_UI_ROW];
    int i, busy;
    (void)ctx;
    if (!g_d)
        return;
    for (i = 0; i < g_d->count; i++) {
        row[0] = 0;
        g_d->row(g_d->ctx, i, i == g_cur, row);
        row[DK_UI_ROW - 1] = 0;
        dk_ui_text(m, row);
    }
    busy = g_d->busy && g_d->busy(g_d->ctx);
    if (busy && !g_ticking && dk_ui_has(DK_UI_CAP_TICK) && dk_ui_tick(tick, 0, TICK_MS) == 0)
        g_ticking = 1;
    else if (!busy)
        stop_timer();
}

static void relist(void);

static void open_browser(void)
{
    relist();
    dk_ui_open(g_browser);
}

/* YES on a slot opens the browser for it. The row has a check callback, so this is the only thing that happens: the stock handler of a list
   without one closes the list after the row's action. The mark core/ui sets is taken off at once, so a row never shows a check. */
static void check_list(void *ctx, int row, int on)
{
    (void)ctx;
    (void)on;
    dk_ui_check(g_list, row, 0);
    if (!g_d || row < 0 || row >= g_d->count)
        return;
    g_slot = row;
    open_browser();
}

/* RIGHT on a slot row. */
static void panel_list(dk_ui_menu *p, int row, void *ctx)
{
    (void)ctx;
    if (!g_d || !g_d->panel || row < 0 || row >= g_d->count)
        return;
    g_d->panel(g_d->ctx, p, row);
}

/* ---- the folder browser ---- */

static void relist(void)
{
    stop_listing();
    g_n = g_more = 0;
    g_state = ST_READING;
    g_ticket = g_api->files_list(g_client, g_d->root, (const uint8_t *)g_dir, on_notify, 0);
    if (g_ticket <= 0) {
        g_state = ST_ERROR;
        g_err = g_ticket < 0 ? g_ticket : -48;
    }
}

static void add_entry(void)
{
    const char *leaf = dk_path_leaf((const char *)g_entry.path);
    dk_chooser_row r;
    int i, full;
    if ((g_entry.kind != DK_FILE_FILE && g_entry.kind != DK_FILE_DIRECTORY) || !leaf[0])
        return;
    if (slen(leaf) >=
        DK_CHOOSER_NAME) { /* a name that does not fit is not shown; it counts as hidden */
        g_more++;
        return;
    }
    for (i = 0; leaf[i]; i++)
        r.name[i] = leaf[i];
    for (; i < DK_CHOOSER_NAME; i++)
        r.name[i] = 0;
    r.dir = g_entry.kind == DK_FILE_DIRECTORY;
    r.size = (g_entry.flags & DK_FILE_SIZE_VALID) && !r.dir ? g_entry.size_bytes : 0;
    full = g_n == DK_CHOOSER_ROWS;
    if (!dk_chooser_row_insert(g_rows, g_idx, &g_n, DK_CHOOSER_ROWS, &r) || full)
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
    char row[DK_UI_ROW], nm[DK_CHOOSER_TEXT + 1], sz[8], tmp[DK_CHOOSER_TEXT + 1];
    int i;
    (void)ctx;
    dk_text_cat(tmp, "..  /", g_dir);
    dk_text_trunc(row, tmp, DK_CHOOSER_TEXT);
    dk_ui_text(m, row);
    if (g_state == ST_READING) {
        dk_ui_text(m, "READING...");
        return;
    }
    if (g_state == ST_ERROR) {
        char num[12];
        dk_text_utoa(num, (uint32_t)(g_err < 0 ? -g_err : g_err));
        dk_text_cat(row, "LIST ERROR -", num);
        dk_ui_text(m, row);
        return;
    }
    for (i = 0; i < g_n; i++) {
        const dk_chooser_row *r = &g_rows[g_idx[i]];
        if (r->dir) {
            dk_text_trunc(nm, r->name, DK_CHOOSER_TEXT - 2);
            dk_text_cat(row, "> ", nm);
        } else if (r->size) {
            dk_text_size(sz, r->size);
            dk_text_trunc(nm, r->name, DK_CHOOSER_TEXT - 3 - slen(sz));
            dk_text_cat(tmp, "  ", nm);
            dk_text_cat(row, tmp, " ");
            dk_text_cat(row, row, sz);
        } else {
            dk_text_trunc(nm, r->name, DK_CHOOSER_TEXT - 2);
            dk_text_cat(row, "  ", nm);
        }
        dk_ui_text(m, row);
    }
    if (g_n == 0)
        dk_ui_text(m, "NO SAMPLES HERE");
    if (g_more) {
        char num[12];
        dk_text_utoa(num, (uint32_t)g_more);
        dk_text_cat(tmp, "+", num);
        dk_text_cat(row, tmp, " MORE");
        dk_ui_text(m, row);
    }
}

/* NO and the ".." row: up one folder, or out of the browser at the root. Always consumes the key. */
static int back_browser(void *ctx)
{
    (void)ctx;
    if (dk_path_parent(g_dir)) {
        relist();
        dk_ui_refresh(g_browser);
    } else
        leave_browser();
    return 1;
}

/* YES on a file: the module loads it into the slot. A result finishes the flow (every level closes, done gets the slot); an error closes the flow and
   shows its text on the page. */
static void select_file(int at)
{
    int rc;
    if (dk_path_join(g_path, sizeof g_path, g_dir, g_rows[g_idx[at]].name) < 0)
        rc = DK_CHOOSER_E_PATH;
    else
        rc = g_d->fill(g_d->ctx, g_slot, g_path);
    if (rc < 0)
        fail(rc);
    else
        finish(rc);
}

static void enter_folder(int at)
{
    int i;
    if (dk_path_join(g_path, sizeof g_path, g_dir, g_rows[g_idx[at]].name) < 0) {
        g_state = ST_ERROR;
        g_err = DK_CHOOSER_E_PATH;
    } else {
        for (i = 0; g_path[i]; i++)
            g_dir[i] = g_path[i];
        g_dir[i] = 0;
        relist();
    }
    dk_ui_refresh(g_browser);
}

static void check_browser(void *ctx, int row, int on)
{
    int at = row - 1;
    (void)ctx;
    (void)on;
    dk_ui_check(g_browser, row, 0);
    if (row == 0) {
        back_browser(0);
        return;
    }
    if (g_state != ST_DONE || at < 0 || at >= g_n)
        return;
    if (g_rows[g_idx[at]].dir)
        enter_folder(at);
    else
        select_file(at);
}

static void select_cb(void *ctx, int at)
{
    (void)ctx;
    select_file(at);
}

static void file_info_cb(void *ctx, int at)
{
    (void)ctx;
    g_info_at = at;
    dk_ui_open(g_finfo);
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

static void fill_finfo(dk_ui_menu *m, void *ctx)
{
    char nm[DK_CHOOSER_TEXT + 1], row[DK_UI_ROW], num[12];
    const dk_chooser_row *r;
    (void)ctx;
    if (g_info_at < 0 || g_info_at >= g_n)
        return;
    r = &g_rows[g_idx[g_info_at]];
    dk_text_trunc(nm, r->name, WRAP);
    dk_ui_text(m, nm);
    if (dk_path_join(g_path, sizeof g_path, g_dir, r->name) >= 0)
        dk_chooser_wrap(m, g_path);
    if (r->size) {
        dk_text_utoa(num, r->size);
        dk_text_cat(nm, "SIZE ", num);
        dk_text_cat(row, nm, " BYTES");
        dk_ui_text(m, row);
    }
}

/* ---- the module's calls ---- */

int dk_chooser_init(const dk_api *api)
{
    g_client = 0;
    g_api = api;
    g_d = 0;
    if (!api || api->size < offsetof(dk_api, files_cancel) + sizeof api->files_cancel ||
        !api->files_client || !api->files_list || !api->files_poll || !api->files_cancel)
        return -1;
    {
        int32_t c = api->files_client();
        if (c <= 0)
            return -1;
        g_client = (uint32_t)c;
    }
    return 0;
}

int dk_chooser_open(const dk_chooser_desc *d, int current)
{
    if (!g_api || !g_client || !d || !d->row || !d->fill || !dk_ui_has(DK_UI_CAP_LIST) ||
        !dk_ui_has(DK_UI_CAP_TEXT_PAGE))
        return -1;
    g_d = d;
    g_cur = current;
    if (!g_list) {
        g_list_desc = (dk_ui_menu_desc){.name = "Chooser",
                                        .title = d->title,
                                        .flags = DK_UI_LIST,
                                        .fill = fill_list,
                                        .panel = panel_list,
                                        .check = check_list};
        g_browser_desc = (dk_ui_menu_desc){.name = "ChooserBrowse",
                                           .title = d->title,
                                           .flags = DK_UI_LIST,
                                           .fill = fill_browser,
                                           .panel = panel_browser,
                                           .check = check_browser,
                                           .back = back_browser};
        g_finfo_desc = (dk_ui_menu_desc){
            .name = "ChooserFile", .title = "INFO", .flags = DK_UI_TEXT_PAGE, .fill = fill_finfo};
        g_list = dk_ui_menu_new(&g_list_desc);
        g_browser = dk_ui_menu_new(&g_browser_desc);
        g_finfo = dk_ui_menu_new(&g_finfo_desc);
    }
    if (!g_list || !g_browser || !g_finfo)
        return -1;
    g_state = ST_IDLE;
    dk_ui_select(g_list, current >= 0 && current < d->count ? current : 0);
    return dk_ui_open(g_list);
}

void dk_chooser_refresh(void)
{
    if (g_list && dk_ui_is_open(g_list))
        dk_ui_refresh(g_list);
}

void dk_chooser_fini(void)
{
    stop_timer();
    if (g_api && g_client)
        stop_listing();
    g_state = ST_IDLE;
    g_client = 0;
    g_d = 0;
}
