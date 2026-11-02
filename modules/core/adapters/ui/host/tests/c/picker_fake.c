/* Host test of the sample picker (sdk/src/sample_picker.c): its own source, compiled here against a fake dk_api (a listing provider and
   a 16-entry sample pool) and the fake <dk/ui.h> (src/lib.rs). picker.rs presses the keys. The source is included, not linked, so
   that pf_reset can clear its statics between tests. The pure functions (picker_logic.c) are linked. */
#include <stdint.h>
#include <string.h>
#include "../../../../../../../sdk/src/chooser.c"
#include "../../../../../../../sdk/src/sample_picker.c"

#define MAXE 400

static dk_api api;
static struct {
    int n, read, finished, list_calls, cancels, client_calls;
    int32_t ticket, list_result, poll_error;
    char dir[PK_PATH];
    dk_file_entry e[MAXE];
    dk_file_event *notify;
} fl;
static struct {
    int status[TRACKS];
    dk_sample_info info[TRACKS];
    char path[TRACKS][PK_PATH];
    int add_result, adds, clears, last_index, path_calls, picks, last_pick;
    char last_path[PK_PATH];
} sm;

static int32_t f_client(void)
{
    fl.client_calls++;
    return 7;
}
static int32_t f_list(uint32_t c, uint32_t root, const uint8_t *path, dk_file_event *notify, uint8_t *ctx)
{
    (void)c;
    (void)ctx;
    fl.list_calls++;
    if (root != DK_FILES_SAMPLES)
        return -1;
    strcpy(fl.dir, (const char *)path);
    fl.n = fl.read = fl.finished = 0;
    fl.notify = notify;
    if (fl.list_result < 0)
        return fl.list_result;
    return ++fl.ticket;
}
static int32_t f_poll(uint32_t c, uint32_t t, uint8_t *out, uint32_t cap)
{
    (void)c;
    if (t != (uint32_t)fl.ticket || cap < sizeof(dk_file_entry))
        return -1;
    if (fl.poll_error)
        return fl.poll_error;
    if (fl.read < fl.n) {
        memcpy(out, &fl.e[fl.read++], sizeof(dk_file_entry));
        return DK_FILES_ROW;
    }
    return fl.finished ? DK_FILES_END : DK_FILES_PENDING;
}
static int32_t f_cancel(uint32_t c, uint32_t t)
{
    (void)c;
    (void)t;
    fl.cancels++;
    return 0;
}
/* The fake runtime: the first UNUSED entry takes the path (add_result < 0 refuses), the same path again gives its entry. */
static int32_t s_add(uint32_t h, const uint8_t *path)
{
    int t, free = -1;
    (void)h;
    sm.adds++;
    strcpy(sm.last_path, (const char *)path);
    if (sm.add_result < 0)
        return sm.add_result;
    for (t = 0; t < (int)TRACKS; t++) {
        if (sm.status[t] != 0 && !strcmp(sm.path[t], (const char *)path)) {
            sm.last_index = t;
            return t;
        }
        if (sm.status[t] == 0 && free < 0)
            free = t;
    }
    if (free < 0)
        return DK_SAMPLE_E_POOL;
    strcpy(sm.path[free], (const char *)path);
    sm.status[free] = DK_SAMPLE_QUEUED;
    sm.last_index = free;
    return free;
}
static int32_t s_clear(uint32_t h, uint32_t t)
{
    (void)h;
    sm.clears++;
    sm.status[t] = 0;
    sm.path[t][0] = 0;
    return 0;
}
static int32_t s_status(uint32_t h, uint32_t t)
{
    (void)h;
    return sm.status[t];
}
static int32_t s_info(uint32_t h, uint32_t t, uint8_t *out)
{
    (void)h;
    memcpy(out, &sm.info[t], sizeof sm.info[t]);
    return 0;
}
static int32_t s_path(uint32_t h, uint32_t t, uint8_t *buf, uint32_t cap)
{
    uint32_t n = (uint32_t)strlen(sm.path[t]);
    (void)h;
    sm.path_calls++;
    if (!sm.path[t][0])
        return DK_SAMPLE_E_NOT_FOUND;
    if (n + 1 > cap)
        return DK_SAMPLE_E_SEL_PATH;
    memcpy(buf, sm.path[t], n + 1);
    return (int32_t)n;
}

static void on_pick(void *ctx, int index)
{
    (void)ctx;
    sm.picks++;
    sm.last_pick = index;
}

/* Clears every static of the picker and the fakes. with_path: the runtime has sample_pool_path (api 1.6). */
void pf_reset(int with_path)
{
    memset(&fl, 0, sizeof fl);
    memset(&sm, 0, sizeof sm);
    memset(&api, 0, sizeof api);
    g_api = 0;
    g_handle = g_client = 0;
    g_has_path = 0;
    g_list = g_browser = g_finfo = 0;
    g_info = 0;
    g_d = 0;
    g_cur = g_n = g_more = g_state = g_err = g_ticking = g_info_at = 0;
    g_ticket = 0;
    g_dir[0] = 0;
    g_pick = 0;
    g_pick_ctx = 0;
    api.version = 1;
    api.size = with_path ? sizeof(dk_api) : offsetof(dk_api, sample_pool_path);
    api.files_client = f_client;
    api.files_list = f_list;
    api.files_poll = f_poll;
    api.files_cancel = f_cancel;
    api.sample_pool_add = s_add;
    api.sample_pool_clear = s_clear;
    api.sample_pool_status = s_status;
    api.sample_pool_info = s_info;
    api.sample_pool_path = s_path;
}

void pf_short_api(void) { api.size = offsetof(dk_api, files_client); }
int pf_init(uint32_t handle) { return dk_picker_init(&api, handle); }
void pf_open_pool(int current, int with_pick) { dk_picker_open_pool(current, with_pick ? on_pick : 0, 0); }
void pf_fini(void) { dk_picker_fini(); }

/* the fake provider */
void pf_add(const char *path, int dir, uint32_t size)
{
    dk_file_entry *e = &fl.e[fl.n++];
    memset(e, 0, sizeof *e);
    e->kind = dir ? DK_FILE_DIRECTORY : DK_FILE_FILE;
    e->flags = dir || !size ? 0 : DK_FILE_SIZE_VALID;
    e->size_bytes = size;
    strcpy((char *)e->path, path);
}
void pf_finish(void) /* the listing is complete: the notify callback runs, as the UI task would run it */
{
    fl.finished = 1;
    fl.notify(7, (uint32_t)fl.ticket, 0, 0);
}
void pf_partial(void) { fl.notify(7, (uint32_t)fl.ticket, 0, 0); } /* rows are ready, the end is not */
void pf_stale_notify(void) { fl.notify(7, (uint32_t)fl.ticket - 1, 0, 0); }
void pf_fail(int rc)
{
    fl.poll_error = rc;
    fl.notify(7, (uint32_t)fl.ticket, 0, 0);
}
void pf_list_result(int rc) { fl.list_result = rc; }
int pf_list_calls(void) { return fl.list_calls; }
int pf_cancels(void) { return fl.cancels; }
const char *pf_list_dir(void) { return fl.dir; }
int pf_client_calls(void) { return fl.client_calls; }

/* the fake sample table */
void pf_track(int t, int status, const char *path, uint32_t frames, uint32_t rate, uint32_t channels)
{
    sm.status[t] = status;
    strcpy(sm.path[t], path);
    sm.info[t].frames = frames;
    sm.info[t].rate = rate;
    sm.info[t].channels = channels;
}
void pf_status(int t, int status) { sm.status[t] = status; }
void pf_add_result(int rc) { sm.add_result = rc; }
int pf_adds(void) { return sm.adds; }
int pf_clears(void) { return sm.clears; }
int pf_last_index(void) { return sm.last_index; }
int pf_picks(void) { return sm.picks; }
int pf_last_pick(void) { return sm.last_pick; }
const char *pf_last_path(void) { return sm.last_path; }

/* ---- a toy chooser: five entries (entry 2 is a hole), to test the component without the sample pool ---- */
static struct {
    int done_calls, last_done, add_calls, panel_entry, with_text;
    int add_rc;
    char last_add[PK_PATH];
    dk_chooser_desc d;
} cf;

static int toy_row(void *ctx, int i, int is_current, char *text)
{
    (void)ctx;
    text[0] = is_current ? '>' : ' ';
    text[1] = 'r';
    text[2] = (char)('0' + i);
    text[3] = 0;
    return i == 2 ? DK_CHOOSER_HOLE : 0;
}
static int toy_add(void *ctx, const char *path)
{
    (void)ctx;
    cf.add_calls++;
    strcpy(cf.last_add, path);
    return cf.add_rc;
}
static void toy_error(void *ctx, int code, char *text)
{
    (void)ctx;
    (void)code;
    if (cf.with_text)
        strcpy(text, "TOY FAILED");
}
static void toy_panel(void *ctx, dk_ui_menu *p, int entry)
{
    (void)ctx;
    cf.panel_entry = entry;
    dk_ui_action(p, "ONE", 0, 0, entry);
}
static void toy_done(void *ctx, int result)
{
    (void)ctx;
    cf.done_calls++;
    cf.last_done = result;
}

int cf_init(void) { return dk_chooser_init(&api); }
int cf_open(int current, int with_add, int with_text)
{
    memset(&cf, 0, sizeof cf);
    cf.with_text = with_text;
    cf.add_rc = 42;
    cf.d = (dk_chooser_desc){.title = "TOY", .count = 5, .row = toy_row, .add = with_add ? "+ NEW" : 0, .root = DK_FILES_SAMPLES,
                             .add_file = toy_add, .error_text = toy_error, .panel = toy_panel, .done = toy_done};
    return dk_chooser_open(&cf.d, current);
}
void cf_add_result(int rc) { cf.add_rc = rc; }
int cf_done_calls(void) { return cf.done_calls; }
int cf_last_done(void) { return cf.last_done; }
int cf_add_calls(void) { return cf.add_calls; }
const char *cf_last_add(void) { return cf.last_add; }
int cf_panel_entry(void) { return cf.panel_entry; }
