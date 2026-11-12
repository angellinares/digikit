/* core/digikit-menu: SETTINGS > DIGIKIT, written in C on <dk/platform/ui.h> (core/ui draws it with the firmware's own views).
 *
 * DIGIKIT is a menu: ABOUT, HEALTH and MEMORY (text pages), and MODULES (a list). MODULES has one
 * row per .dkm file of the store. The check mark of a row means "load at the next restart"; it starts from the saved selection.
 * RIGHT opens the side panel (LOAD or UNLOAD, ROUTE... for a loaded effect, SETTINGS... for a loaded module that has a dkm_menu, INFO).
 * The check mark is the only on/off of a module. Nothing is saved until NO leaves the list with
 * changes pending and APPLY & RESTART? is answered with YES.
 *
 * The runtime does the work of the old menu: dkrt_scan (the files, their needs), dkrt_fit (what fits next to the checked ones)
 * and the selection and settings calls. This file keeps the pending state and builds the screens.
 *
 * The globals without static are read by the proofs (tools/coldfire/digikit_menu_proof.py and the pk_digikit_* tools) by name.
 * The module links without memcpy and memset: no struct copies, no aggregate initialisers of large objects. */
#include <stdint.h>
#include <stddef.h>
#include <dk/platform/ui.h>
#include <dk_ui_core.h>
#include <dk/gen/limits.h>
#include "dkrt.h"

#define MAX_ENT DKRT_SCAN_MAX
#define MAX_FXR 4
#define ROUTE_MASTER 1
#define ROUTE_SEND 2
#define NAME_COLS 12

/* ---- state read by the proofs ---------------------------------------------------------------------------------------- */

/* 0 = not read, 1 = ok, 2 = no store, 3 = read error, 4 = bad store */
uint32_t STORE;
uint32_t NENT;
/* 1 when the file's header says this runtime or device cannot load it. */
uint8_t NOK[MAX_ENT];
/* 1 when the runtime has the file loaded in this boot, 2 while its DSP part still loads. */
uint8_t LOADED[MAX_ENT];
/* The pending selection: 1 when the row is checked. It starts as the saved selection. */
uint8_t TICK[MAX_ENT];
/* Rows of the pages as text (24 bytes each) and how many there are. */
uint8_t DK_LOAD[40][24];
uint32_t DK_LOAD_N;
uint8_t DK_ABOUT[24][24];
uint32_t DK_ABOUT_N;
uint8_t DK_MEM[8][24];
uint32_t DK_MEM_N;
uint8_t DK_LINES[12][24];
/* The last dialog the menu opened: title, text and how many were opened. */
uint8_t DK_POPUP[48];
uint8_t DK_POPUP_TEXT[48];
uint32_t DK_POPUPS;
/* The sector reads of the last scan (the cost of opening MODULES). */
uint32_t DK_NEEDS_READS;
/* What the last APPLY did: the results of dkrt_selection_set and dkrt_restart (which returns only when it refuses), and how often it ran. */
int32_t DK_APPLY_SET;
int32_t DK_APPLY_RESTART;
uint32_t DK_APPLIES;
/* The routes of the loaded effects (1 master, 2 send), as pending; the results of the last settings writes. */
uint8_t DK_FXR_PEND[MAX_FXR];
int32_t DK_FXR_SET;
int32_t DK_FXR_FLUSH;
/* Changes pending as the last NO counted them (selection and routes). */
uint32_t DK_PENDING;
/* The rows of the last side panel as text, and the routes the last ROUTE... choice offered (1 master, 2 send), in the order shown. */
uint8_t DK_PANEL[4][24];
uint32_t DK_PANEL_N;
uint32_t DK_ROUTE_N;
uint8_t DK_ROUTE_ITEM[2];
DkrtHealth DK_HEALTH;
/* The HEALTH page's menu (a core/ui object: its Menu view is the pointer at +4); the health proof reads the view's own flags. */
dk_ui_menu *DK_HEALTH_MENU;
uint32_t DK_REDRAWS;
uint32_t DK_POLLS;

/* ---- private state --------------------------------------------------------------------------------------------------- */

static DkrtScan g_ent[MAX_ENT];
static DkrtFitOut g_fit;
static DkrtSelection g_sel;
static DkrtBootInfo g_boot;
static DkrtFx g_fx[MAX_FXR];
static uint32_t g_fx_n, g_fx_slots;
static uint8_t g_fx_saved[MAX_FXR];
static int8_t g_row_fx[MAX_ENT]; /* the first effect of a loaded row's module, -1: none */
static uint8_t g_saved[MAX_ENT];
static int8_t g_sel_of
    [MAX_ENT]; /* the index of a row in the saved selection (the boot's results and needs are by that index), -1: not selected */
static uint32_t g_saved_extra;
static int32_t g_selection_error;
static int g_info_row;
static int g_route_row;
static uint32_t g_safe_shown, g_seen, g_last, g_calls;
static char g_empty_text[24];
static int
    g_hdr; /* 1: the MODULES list starts with the safe-mode header, so row r of the list is module r - 1 */
static int
    g_lead; /* 1: the rows start with a mark column, ticked as " " and a selected module that is not loaded as "-" */

static dk_ui_menu *g_main, *g_about, *g_mem, *g_modules, *g_info;

/* The settings menu of each loaded module that has a dkm_menu, by the module's index in dkrt_module order. Each is built the first
   time the user opens it and kept: no module is unloaded at run time, so the module's code and the ctx it passes stay valid. */
#define MAX_SET 16
#define NAME_ROW 32 /* a module name (31 characters at most) without its author */
/* Calls the module's dkm_menu at address a (DkrtModule.menu) with the menu to fill. The host tests, where an address is wider than 32 bits, define their own. */
#ifndef DK_CALL_MENU
#define DK_CALL_MENU(a, m) ((void (*)(dk_ui_menu *, void *))(uintptr_t)(a))((m), 0)
#endif
/* The same for a module of the new entry (dk_module.menu, DkrtModule.ctx not 0): menu(ctx, menu). */
#ifndef DK_CALL_MENU_CTX
#define DK_CALL_MENU_CTX(a, c, m)                                                                  \
    ((void (*)(void *, dk_ui_menu *))(uintptr_t)(a))((void *)(uintptr_t)(c), (m))
#endif
static dk_ui_menu *g_set[MAX_SET];
static char g_set_title[MAX_SET][NAME_ROW];
static dk_ui_menu_desc g_set_desc[MAX_SET];

/* ---- text ------------------------------------------------------------------------------------------------------------ */

typedef struct txt {
    char b[48];
    int n;
} txt;

static void t_init(txt *t)
{
    t->n = 0;
    t->b[0] = 0;
}

static txt *t_c(txt *t, char c)
{
    if (c && t->n < 47) {
        t->b[t->n++] = c;
        t->b[t->n] = 0;
    }
    return t;
}

static txt *t_s(txt *t, const char *s)
{
    while (*s)
        t_c(t, *s++);
    return t;
}

static txt *t_b(txt *t, const uint8_t *s, int max)
{
    for (int i = 0; i < max && s[i]; i++)
        t_c(t, (char)s[i]);
    return t;
}

/* The name of a missing module as the runtime keeps it (usb-bulk), in upper case. */
static txt *t_needs(txt *t, const uint8_t *s)
{
    for (int i = 0; i < DKRT_NEEDS_LEN && s[i]; i++)
        t_c(t, (char)(s[i] >= 'a' && s[i] <= 'z' ? s[i] - 32 : s[i]));
    return t;
}

static txt *t_num(txt *t, uint32_t v)
{
    char tmp[10];
    int k = 0;
    do {
        tmp[k++] = (char)('0' + v % 10);
        v /= 10;
    } while (v);
    while (k)
        t_c(t, tmp[--k]);
    return t;
}

static txt *t_int(txt *t, int32_t v)
{
    if (v < 0) {
        t_c(t, '-');
        return t_num(t, (uint32_t)(-v));
    }
    return t_num(t, (uint32_t)v);
}

/* The list font is not monospace: a letter, a digit, '-' or '_' takes 4 pixels, a space 3, '.' 2 and '~' 5 (measured on the DT2 1.16 list
   font). Character 0x01 draws nothing and takes 4 pixels, so it pads a column the width of one letter. */
#define PAD '\x01'

static int px_of(const char *s)
{
    int w = 0;
    for (; *s; s++)
        w += *s == ' ' ? 3 : *s == '.' ? 2 : *s == '~' ? 5 : 4;
    return w;
}

/* Pads to `px` pixels. `exact`: spaces until the rest is a multiple of 4, then PAD; a rest of 1 or 2 pixels stays (only after a '.' or
   a '~'). Not `exact`: PAD only, which ends up to 3 pixels short but takes no more characters than the letters it stands for. */
static txt *t_pad_px(txt *t, int px, int exact)
{
    int rest = px - px_of(t->b);
    while (exact && rest >= 3 && rest % 4) {
        t_c(t, ' ');
        rest -= 3;
    }
    for (; rest >= 4; rest -= 4)
        t_c(t, PAD);
    return t;
}

static char upper(char c)
{
    return c >= 'a' && c <= 'z' ? (char)(c - 32) : c;
}

static char lower(char c)
{
    return c >= 'A' && c <= 'Z' ? (char)(c + 32) : c;
}

/* Copies a row's text into a 24-byte proof array. */
static void keep(uint8_t *dst, const char *s)
{
    int n = 0;
    while (n < 23 && s[n]) {
        dst[n] = (uint8_t)(s[n] == PAD ? ' ' : s[n]);
        n++;
    }
    while (n < 24)
        dst[n++] = 0;
}

static void copy_text(uint8_t *dst, int max, const char *s)
{
    int n = 0;
    while (n < max - 1 && s[n]) {
        dst[n] = (uint8_t)s[n];
        n++;
    }
    while (n < max)
        dst[n++] = 0;
}

/* ---- the runtime's result texts -------------------------------------------------------------------------------------- */

/* A short text (upper case, at most 14 characters) for each runtime result code, one table per owner range: the loader's -1..-19,
   the registry's -20..-29, the DSP loader's -30..-39, boot-time loading's -40..-49. A code without text shows as ERROR n. */
static const char *const text_runtime[20] = {
    "OK",
    "NOT FOUND",
    "BAD STORE",
    "READ ERROR",
    "BAD SHA",
    "BAD FORMAT",
    "RUNTIME OLD",
    "NO IMPORT",
    "NO MEMORY",
    "INIT FAILED",
    "ALREADY LOADED",
    "SLOT TAKEN",
    "BUSY",
    "TOO BIG",
    "TABLE FULL",
    "",
    "",
    "",
    "",
    "",
};
static const char *const text_registry[10] = {
    "NO REGISTRY", "OTHER DEVICE", "BAD TYPE", "TYPE USED", "NO PARAM ROW",
    "NO PAGE ROW", "TYPE IN USE",  "",         "",          "",
};
static const char *const text_dsp[10] = {
    "NO DSP LINK",
    "BAD DSP CODE",
    "DSP IMPORT",
    "NO DSP SPACE",
    "NO VOICE ROW",
    "DSP XFER",
    "OTHER DEVICE",
    "BAD VOICE TYPE",
    "",
    "",
};
static const char *const text_boot[10] = {
    "BAD PATH", "TOO MANY",     "WRITE FAILED", "CARD BUSY", "SAFE MODE",
    "",         "OUT OF RANGE", "READ ONLY",    "",          "",
};

static void status_text(txt *t, int32_t code)
{
    uint32_t k = code < 0 ? (uint32_t)(-code) : (uint32_t)code;
    const char *known = "";
    if (code >= 0)
        known = "";
    else if (k < 20)
        known = text_runtime[k];
    else if (k < 30)
        known = text_registry[k - 20];
    else if (k < 40)
        known = text_dsp[k - 30];
    else if (k < 50)
        known = text_boot[k - 40];
    if (!known[0]) {
        t_s(t, "ERROR ");
        t_int(t, code);
    } else {
        t_s(t, known);
    }
}

/* The text of a module's result at boot: OK, LOADING (the DSP part is still going), NEEDS <module> when the load failed because a module it
   imports from is not in the image (`needs`, the runtime's DkrtBootInfo.needs of that selection index) or the code's text. */
static void result_text(txt *t, int32_t code, const uint8_t *needs)
{
    if (code == 1)
        t_s(t, "LOADING");
    else if (code == 0)
        t_s(t, "OK");
    else if (code == DKRT_E_IMPORT && needs[0]) {
        t_s(t, "NEEDS ");
        t_needs(t, needs);
    } else
        status_text(t, code);
}

static void safe_reason(txt *t, uint32_t reason)
{
    // a table, not an if chain: six dense values become a jump table, which is data in the code
    static const char *const why[7] = {"UNKNOWN",     "2 BAD BOOTS", "KEY HELD",   "REQUESTED",
                                       "DSP STOPPED", "CPU PANIC",   "READ FAILED"};
    _Static_assert(DKRT_SAFE_COUNTER == 1 && DKRT_SAFE_KEY == 2 && DKRT_SAFE_FLAG == 3 &&
                       DKRT_SAFE_DSP == 4 && DKRT_SAFE_PANIC == 5 && DKRT_SAFE_READ == 6,
                   "why[] follows the DKRT_SAFE_* values");
    t_s(t, reason < 7 ? why[reason] : why[0]);
}

/* The file name of a path, upper case, at most max characters: from the end, or with head the first ones. */
static void name_of(const uint8_t *p, int len, int max, int head, txt *t)
{
    int start = 0;
    for (int k = 0; k < len; k++)
        if (p[k] == '/')
            start = k + 1;
    int n = len - start;
    if (n > max) {
        if (!head)
            start += n - max;
        n = max;
    }
    for (int k = 0; k < n; k++)
        t_c(t, upper((char)p[start + k]));
}

static int path_len(const uint8_t *p, int max)
{
    int n = 0;
    while (n < max && p[n])
        n++;
    return n;
}

/* The row's file name without .dkm, upper case, at most max characters. A longer name keeps its start and its end around a ~, so that
   names with a common start (dyn-fx-gain-send, dyn-fx-gain-sendonly) still differ. */
static void row_name(int i, int max, txt *t)
{
    const uint8_t *p = g_ent[i].path;
    int l = path_len(p, 63);
    if (l > 4 && p[l - 4] == '.' && lower((char)p[l - 3]) == 'd' && lower((char)p[l - 2]) == 'k' &&
        lower((char)p[l - 1]) == 'm')
        l -= 4;
    txt full;
    t_init(&full);
    name_of(p, l, 47, 1, &full);
    if (full.n <= max) {
        t_s(t, full.b);
        return;
    }
    int tail = max / 2 - 1, head = max - 1 - tail;
    for (int k = 0; k < head; k++)
        t_c(t, full.b[k]);
    t_c(t, '~');
    t_s(t, full.b + full.n - tail);
}

/* ---- dialogs --------------------------------------------------------------------------------------------------------- */

static void say(const char *title, const char *text, void (*done)(void *, int))
{
    copy_text(DK_POPUP, sizeof DK_POPUP, title);
    copy_text(DK_POPUP_TEXT, sizeof DK_POPUP_TEXT, text);
    DK_POPUPS++;
    dk_ui_confirm(title, text, done, 0);
}

static void fail_text(const char *what, int32_t code)
{
    txt t;
    t_init(&t);
    status_text(&t, code);
    say(what, t.b, 0);
}

/* ---- boot state ------------------------------------------------------------------------------------------------------ */

static void read_boot(void)
{
    dkrt_boot_info(&g_boot);
}

/* ---- the module store and the pending selection ---------------------------------------------------------------------- */

/* The saved selection, matched against the rows: a checked row for each path the store still has. */
static void load_selection(void)
{
    g_selection_error = dkrt_selection_get(&g_sel);
    if (g_selection_error != DKRT_OK) {
        STORE = 3;
        return;
    }
    for (uint32_t i = 0; i < MAX_ENT; i++) {
        TICK[i] = 0;
        g_sel_of[i] = -1;
    }
    g_saved_extra = 0;
    uint32_t n = g_sel.count > DKRT_SEL_MAX ? DKRT_SEL_MAX : g_sel.count;
    for (uint32_t k = 0; k < n; k++) {
        const uint8_t *p = g_sel.paths[k];
        int l = path_len(p, DKRT_SEL_PATH - 1);
        int found = 0;
        for (uint32_t i = 0; i < NENT; i++) {
            const uint8_t *q = g_ent[i].path;
            if (path_len(q, 63) != l)
                continue;
            int same = 1;
            for (int c = 0; c < l; c++)
                same &= q[c] == p[c];
            if (same) {
                TICK[i] = 1;
                g_sel_of[i] = (int8_t)k;
                found = 1;
            }
        }
        if (!found)
            g_saved_extra++;
    }
    for (uint32_t i = 0; i < MAX_ENT; i++)
        g_saved[i] = TICK[i];
}

/* The loaded effects, the routes the settings give them and the send slots; the pending routes start as the saved ones. */
static void read_fx(void)
{
    g_fx_n = 0;
    while (g_fx_n < MAX_FXR && dkrt_fx(g_fx_n, &g_fx[g_fx_n]) == DKRT_OK) {
        g_fx_saved[g_fx_n] = (uint8_t)g_fx[g_fx_n].saved;
        DK_FXR_PEND[g_fx_n] = (uint8_t)g_fx[g_fx_n].saved;
        g_fx_n++;
    }
    DkrtCap caps[DKRT_CAP_MAX];
    uint32_t n = dkrt_capacity(caps, DKRT_CAP_MAX);
    g_fx_slots = 0;
    for (uint32_t i = 0; i < n; i++)
        if (caps[i].kind == DKRT_CAPK_SEND_SLOTS)
            g_fx_slots = caps[i].limit;
}

static int same_text(const uint8_t *a, const uint8_t *b, int max)
{
    for (int i = 0; i < max; i++) {
        if (a[i] != b[i])
            return 0;
        if (!a[i])
            return 1;
    }
    return 1;
}

/* For each row of a loaded module: its first effect (a module's effects share one route setting). */
static void match_fx(void)
{
    for (uint32_t i = 0; i < MAX_ENT; i++)
        g_row_fx[i] = -1;
    for (uint32_t k = 0;; k++) {
        static DkrtModule mod;
        if (dkrt_module(k, &mod) != DKRT_OK)
            break;
        for (uint32_t i = 0; i < NENT; i++) {
            if (!same_text(g_ent[i].path, mod.path, 63))
                continue;
            for (uint32_t j = 0; j < g_fx_n; j++)
                if (g_row_fx[i] < 0 && same_text(g_fx[j].id, mod.name, 32))
                    g_row_fx[i] = (int8_t)j;
        }
    }
}

/* Scans the store: the runtime lists the .dkm files under modules/ with their header verdict, needs and loaded state. */
static void scan_store(void)
{
    NENT = 0;
    for (uint32_t i = 0; i < MAX_ENT; i++) {
        TICK[i] = 0;
        g_saved[i] = 0;
        NOK[i] = 0;
        LOADED[i] = 0;
    }
    g_saved_extra = 0;
    int32_t n = dkrt_scan(g_ent, MAX_ENT);
    if (n < 0) {
        STORE = n == DKRT_E_NOT_FOUND ? 2 : n == DKRT_E_READ ? 3 : 4;
        return;
    }
    NENT = (uint32_t)n;
    STORE = 1;
    for (uint32_t i = 0; i < NENT; i++) {
        uint32_t f = g_ent[i].flags;
        if (!(f & DKRT_SCAN_COMPAT))
            NOK[i] = 1;
        if (f & DKRT_SCAN_LOADING)
            LOADED[i] = 2;
        else if (f & DKRT_SCAN_LOADED)
            LOADED[i] = 1;
    }
    DK_NEEDS_READS = dkrt_scan_reads();
    load_selection();
}

/* Which modules fit next to the checked ones: the runtime judges them. */
static void refit(void)
{
    dkrt_fit(g_ent, NENT, TICK, &g_fit);
}

static int wont(int i)
{
    return g_fit.rows[i].wont != 0;
}

static uint32_t sends_except(int skip)
{
    uint32_t n = 0;
    for (uint32_t j = 0; j < g_fx_n; j++)
        if ((int)j != skip && DK_FXR_PEND[j] == ROUTE_SEND)
            n++;
    return n;
}

/* Changes since the saved state: rows checked or unchecked, effects with another route, and safe mode (a restart leaves it). */
static uint32_t changes(void)
{
    uint32_t n = 0;
    for (uint32_t i = 0; i < NENT; i++)
        n += TICK[i] != g_saved[i];
    for (uint32_t j = 0; j < g_fx_n; j++)
        n += DK_FXR_PEND[j] != g_fx_saved[j];
    return n;
}

static int restart_pending(void)
{
    return changes() > 0 || g_boot.safe != 0;
}

/* ---- the DIGIKIT menu ------------------------------------------------------------------------------------------------ */

static void safe_notice(void *ctx, int arg)
{
    (void)ctx;
    (void)arg;
    txt t;
    t_init(&t);
    safe_reason(&t, g_boot.reason);
    say("SAFE MODE", t.b, 0);
}

static void open_health(void *ctx, int arg);
static void open_modules(void *ctx, int arg);

static void fill_main(dk_ui_menu *m, void *ctx)
{
    (void)ctx;
    if (g_boot.safe != 0) {
        txt t;
        t_init(&t);
        t_s(&t, "SAFE: ");
        safe_reason(&t, g_boot.reason);
        dk_ui_action(m, t.b, safe_notice, 0, 0);
    }
    dk_ui_submenu(m, "ABOUT", g_about);
    dk_ui_action(m, "HEALTH", open_health, 0, 0);
    dk_ui_submenu(m, "MEMORY", g_mem);
    dk_ui_action(m, "MODULES", open_modules, 0, 0);
}

/* ---- ABOUT ----------------------------------------------------------------------------------------------------------- */

/* Is the scanned file of the store path p (l characters) a build with --trace? */
static int traced_path(const uint8_t *p, int l)
{
    for (uint32_t i = 0; i < NENT; i++) {
        const uint8_t *q = g_ent[i].path;
        if (path_len(q, 63) != l)
            continue;
        int same = 1;
        for (int c = 0; c < l; c++)
            same &= q[c] == p[c];
        if (same)
            return (g_ent[i].flags & DKRT_SCAN_TRACE) != 0;
    }
    return 0;
}

static void arow(dk_ui_menu *m, const char *s)
{
    if (DK_ABOUT_N < 24)
        keep(DK_ABOUT[DK_ABOUT_N++], s);
    dk_ui_text(m, s);
}

static void fill_about(dk_ui_menu *m, void *ctx)
{
    (void)ctx;
    DK_ABOUT_N = 0;
    /* the per-module TRACE tag reads the scan of the store: scan once when MODULES has not (a later scan of MODULES resets its ticks, so not again here) */
    if (STORE == 0)
        scan_store();
    arow(m, "DIGIKIT 0.1.0");
    DkrtStatus st;
    dkrt_status(&st);
    read_boot();
    txt t;
    t_init(&t);
    t_s(&t, "RUNTIME ");
    t_num(&t, st.runtime >> 16);
    t_c(&t, '.');
    t_num(&t, st.runtime >> 8 & 255);
    t_c(&t, '.');
    t_num(&t, st.runtime & 255);
    arow(m, t.b);
    DkrtDiag d;
    dkrt_diag(&d);
    /* a card the runtime does not write: why, and what that switches off */
    if (d.card_state == DKRT_CARD_FOREIGN || d.card_state == DKRT_CARD_UNREADABLE) {
        arow(m, "CARD READ-ONLY");
        t_init(&t);
        if (d.card_state == DKRT_CARD_FOREIGN) {
            t_s(&t, "NOT OURS AT ");
            t_num(&t, d.card_first_nonblank);
        } else {
            t_s(&t, "CARD UNREADABLE");
        }
        arow(m, t.b);
        arow(m, "NO BOOT COUNTER");
    }
    if (d.dsp_stopped != 0) {
        arow(m, "DSP STOPPED");
        if (d.dsp_crumb != 0) {
            /* the DSP's last breadcrumb: the step, the voice and the machine type (finding 48) */
            static const char *const step[8] = {"?",    "ENTER", "FOUND", "STOCK",
                                                "CALL", "RET",   "COPY",  "DONE"};
            uint32_t s = d.dsp_crumb & 0xff;
            t_init(&t);
            t_s(&t, "LAST: ");
            t_s(&t, s < 8 ? step[s] : "?");
            t_s(&t, " V");
            t_num(&t, (d.dsp_crumb >> 8) & 0xff);
            t_s(&t, " T");
            t_num(&t, d.dsp_crumb >> 16);
            arow(m, t.b);
        }
    }
    /* the last fault (a CPU crash or a DSP stop), kept on the card: FAULT: CPU CRASH or FAULT: DSP STOP, IN <module>, PC or the DSP site and step */
    for (uint32_t k = 0; k < d.fault_rows && k < DKRT_FAULT_ROWS; k++)
        arow(m, (const char *)d.fault_text[k]);
    if (g_boot.safe != 0) {
        t_init(&t);
        t_s(&t, "SAFE: ");
        safe_reason(&t, g_boot.reason);
        arow(m, t.b);
    }
    t_init(&t);
    t_s(&t, "BOOT: ");
    t_num(&t, g_boot.loaded);
    t_s(&t, " OF ");
    t_num(&t, g_boot.count);
    t_s(&t, " OK");
    arow(m, t.b);
    /* each module of this boot's selection with its result; the names come from the saved selection, which is what the boot
       loaded unless it was saved again without a restart */
    int32_t selection_result = dkrt_selection_get(&g_sel);
    if (selection_result != DKRT_OK)
        arow(m, "SELECTION READ ERROR");
    uint32_t n = selection_result != DKRT_OK   ? 0
                 : g_boot.count > DKRT_SEL_MAX ? DKRT_SEL_MAX
                                               : g_boot.count;
    for (uint32_t k = 0; k < n; k++) {
        const uint8_t *p = g_sel.paths[k];
        t_init(&t);
        name_of(p, path_len(p, DKRT_SEL_PATH - 1), 17, 0, &t);
        arow(m, t.b);
        t_init(&t);
        t_s(&t, "- ");
        result_text(&t, g_boot.results[k], g_boot.needs[k]);
        if (traced_path(p, path_len(p, DKRT_SEL_PATH - 1)))
            t_s(&t, " TRACE");
        arow(m, t.b);
    }
    t_init(&t);
    t_s(&t, "FREE ");
    t_num(&t, st.arena_free / 1024);
    t_s(&t, " KB");
    arow(m, t.b);
    arow(m, d.ram_kept != 0 ? "RAM KEPT: YES" : "RAM KEPT: NO");
}

/* ---- MEMORY ---------------------------------------------------------------------------------------------------------- */

/* The resources the page shows, in order: the CAPACITY kind, the row label and 1 when the row shows a percent (bytes), 0 for used/limit.
   Left out: the staging buffer, the selection, the effect slots and the map entries (they say nothing a user can act on). */
static const uint8_t mem_kind[7] = {
    DKRT_CAPK_DSP_AREA,   DKRT_CAPK_ARENA,      DKRT_CAPK_MODULES,  DKRT_CAPK_MACHINE_ROWS,
    DKRT_CAPK_VOICE_ROWS, DKRT_CAPK_PARAM_ROWS, DKRT_CAPK_PAGE_ROWS};
static const char *const mem_label[7] = {"DSP   ", "CPU   ", "MODS  ", "MACH  ",
                                         "VOICE ", "PARAM ", "PAGE  "};
static const uint8_t mem_pct[7] = {1, 1, 0, 0, 0, 0, 0};

/* One MEMORY row: the label, a ten-character bar (| used, . free: both are 2 pixels wide, so the bar keeps its width; # is 6 pixels
   and a full bar runs off the row), then a percent or used/limit. */
static void mem_row(dk_ui_menu *m, const char *label, const DkrtCap *e, int pct)
{
    uint32_t used = e->used > e->limit ? e->limit : e->used;
    uint32_t full = used * 10 / e->limit;
    txt t;
    t_init(&t);
    t_s(&t, label);
    for (uint32_t k = 0; k < 10; k++)
        t_c(&t, k < full ? '|' : '.');
    t_c(&t, ' ');
    if (pct) {
        t_num(&t, used * 100 / e->limit);
        t_c(&t, '%');
    } else {
        t_num(&t, e->used);
        t_c(&t, '/');
        t_num(&t, e->limit);
    }
    if (DK_MEM_N < 8)
        keep(DK_MEM[DK_MEM_N++], t.b);
    dk_ui_text(m, t.b);
}

static void fill_mem(dk_ui_menu *m, void *ctx)
{
    (void)ctx;
    DK_MEM_N = 0;
    DkrtCap caps[DKRT_CAP_MAX];
    uint32_t n = dkrt_capacity(caps, DKRT_CAP_MAX);
    for (int r = 0; r < 7; r++)
        for (uint32_t k = 0; k < n; k++)
            /* a resource the device does not have (limit 0) has no row */
            if (caps[k].kind == mem_kind[r] && caps[k].limit != 0)
                mem_row(m, mem_label[r], &caps[k], mem_pct[r]);
}

/* ---- HEALTH (finding 41) --------------------------------------------------------------------------------------------- */

/* The page refreshes when this many DSP blocks passed (half a second), or every 60 UI passes without the block clock. */
#define H_BLOCKS 750

/* n bytes as 12.3 MB (tenths) or, below a megabyte, 511 KB. */
static void size_text(txt *t, uint32_t n)
{
    if (n >= 1024 * 1024) {
        uint32_t tenth = (n / 1024 * 10) / 1024;
        t_num(t, tenth / 10);
        t_c(t, '.');
        t_num(t, tenth % 10);
        t_s(t, " MB");
    } else {
        t_num(t, n / 1024);
        t_s(t, " KB");
    }
}

/* A size in KiB as 812 KB, 12.3 MB or 21.9 GB (the store figures: a store can be 22 GiB). */
static void size_text_kb(txt *t, uint32_t kb)
{
    if (kb >= 1024 * 1024) {
        uint32_t tenth = (kb / 1024 * 10) / 1024;
        t_num(t, tenth / 10);
        t_c(t, '.');
        t_num(t, tenth % 10);
        t_s(t, " GB");
    } else if (kb >= 1024) {
        uint32_t tenth = (kb * 10) / 1024;
        t_num(t, tenth / 10);
        t_c(t, '.');
        t_num(t, tenth % 10);
        t_s(t, " MB");
    } else {
        t_num(t, kb);
        t_s(t, " KB");
    }
}

/* A load in tenths of a percent as 37%. */
static void pct_text(txt *t, uint32_t x10)
{
    t_num(t, (x10 + 5) / 10);
    t_c(t, '%');
}

/* The next free row of the proof array DK_LINES; the page has up to 12 rows (the two BIG rows, the largest free block, only show when it differs from the total; the page is about 16 characters wide). */
static int g_hrows;

static void hrow(dk_ui_menu *m, const txt *t)
{
    if (g_hrows < 12)
        keep(DK_LINES[g_hrows++], t->b);
    dk_ui_text(m, t->b);
}

static void fill_health(dk_ui_menu *m, void *ctx)
{
    (void)ctx;
    const DkrtHealth *h = &DK_HEALTH;
    txt t;
    for (int r = 0; r < 12; r++)
        keep(DK_LINES[r], "");
    g_hrows = 0;
    t_init(&t);
    /* relative to the quietest window since boot, not to the CPU's capacity */
    t_s(&t, "CPU REL ");
    if (h->flags & DKRT_HEALTH_CPU)
        pct_text(&t, h->cpu_x10);
    else
        t_s(&t, "--");
    hrow(m, &t);
    t_init(&t);
    /* of an assumed 1 GHz block (finding 100) */
    t_s(&t, "DSP 1GHZ ");
    if (h->flags & DKRT_HEALTH_DSP)
        pct_text(&t, h->dsp_avg_x10);
    else
        t_s(&t, "--");
    hrow(m, &t);
    t_init(&t);
    t_s(&t, "DSP MAX ");
    if (h->flags & DKRT_HEALTH_DSP)
        pct_text(&t, h->dsp_max_x10);
    else
        t_s(&t, "--");
    hrow(m, &t);
    t_init(&t);
    t_s(&t, "RAM FREE ");
    if (h->flags & DKRT_HEALTH_HEAP) {
        if (h->flags & DKRT_HEALTH_HEAP_CUT)
            t_c(&t, '>');
        size_text(&t, h->heap_free);
    } else {
        t_s(&t, "--");
    }
    hrow(m, &t);
    t_init(&t);
    t_s(&t, "LARGEST ");
    if (h->flags & DKRT_HEALTH_HEAP)
        size_text(&t, h->heap_largest);
    else
        t_s(&t, "--");
    hrow(m, &t);
    t_init(&t);
    t_s(&t, "CPU FREE ");
    size_text(&t, h->arena_free);
    hrow(m, &t);
    /* arena_largest is a payload (block size minus the 16-byte block header, DK_ARENA_HDR); arena_free sums block sizes */
    if (h->arena_largest + DK_ARENA_HDR < h->arena_free) {
        t_init(&t);
        t_s(&t, "CPU BIG ");
        size_text(&t, h->arena_largest);
        hrow(m, &t);
    }
    t_init(&t);
    t_s(&t, "DSP FREE ");
    if (h->dsp_area_total != 0)
        size_text(&t, h->dsp_area_free);
    else
        t_s(&t, "--");
    hrow(m, &t);
    if (h->dsp_area_total != 0 && h->dsp_area_largest < h->dsp_area_free) {
        t_init(&t);
        t_s(&t, "DSP BIG ");
        size_text(&t, h->dsp_area_largest);
        hrow(m, &t);
    }
    t_init(&t);
    t_s(&t, "STORE USED ");
    if (h->flags & DKRT_HEALTH_STORE)
        size_text_kb(&t, h->store_used);
    else
        t_s(&t, "--");
    hrow(m, &t);
    t_init(&t);
    t_s(&t, "STORE FREE ");
    if (h->flags & DKRT_HEALTH_STORE)
        size_text_kb(&t, h->store_free);
    else
        t_s(&t, "--");
    hrow(m, &t);
    if (!(h->flags & DKRT_HEALTH_MODULE)) {
        t_init(&t);
        t_s(&t, "NO HEALTH MODULE");
        hrow(m, &t);
    }
}

static void refresh_health(void)
{
    dkrt_health(&DK_HEALTH);
    dk_ui_refresh(DK_HEALTH_MENU);
    DK_REDRAWS++;
}

/* The UI loop's per-pass call while the HEALTH page is registered: refresh about twice a second without a key press, and stop when
   the page was closed. */
void dk_poll(void)
{
    DK_POLLS++;
    if (!DK_HEALTH_MENU) {
        dkrt_poll_set(0);
        return;
    }
    if (dk_ui_is_open(DK_HEALTH_MENU)) {
        g_seen = 1;
    } else if (g_seen) {
        dkrt_poll_set(0);
        g_seen = 0;
        return;
    } else {
        return;
    }
    uint32_t b = dkrt_blocks();
    if (b != 0) {
        if (g_last != 0 && b - g_last < H_BLOCKS)
            return;
        g_last = b;
    } else {
        if (++g_calls < 60)
            return;
        g_calls = 0;
    }
    refresh_health();
}

static void open_health(void *ctx, int arg)
{
    (void)ctx;
    (void)arg;
    dkrt_health(&DK_HEALTH);
    DK_REDRAWS++;
    dk_ui_open(DK_HEALTH_MENU);
    g_seen = 0;
    g_last = 0;
    g_calls = 0;
    dkrt_poll_set((uintptr_t)dk_poll);
}

/* ---- MODULES --------------------------------------------------------------------------------------------------------- */

static const char *kind_text(int i)
{
    const DkrtScan *e = &g_ent[i];
    if (!(e->flags & DKRT_SCAN_NEEDS))
        return "----";
    if (e->need_fx_slots)
        return "FX";
    if (e->need_machine_rows)
        return "MACH";
    return "TOOL";
}

/* A size in bytes as <1K, 259K or 1.2M. */
static void bytes_text(txt *t, uint32_t n)
{
    if (n >= 1024 * 1024) {
        uint32_t tenth = (n / 1024 * 10) / 1024;
        t_num(t, tenth / 10);
        t_c(t, '.');
        t_num(t, tenth % 10);
        t_c(t, 'M');
    } else if (n < 1024) {
        t_s(t, "<1K");
    } else {
        t_num(t, n / 1024);
        t_c(t, 'K');
    }
}

/* A module that the saved selection has and this boot did not load (safe mode, or the load failed): the check column cannot say "-", so
   the row text does (g_lead) and the check mark stays off. A module the user checked just now is a pending change and keeps its check mark. */
static int dashed(int i)
{
    return TICK[i] && g_saved[i] && !LOADED[i];
}

/* The missing module of a dashed row (usb-bulk): the boot's load failed with E_IMPORT and the runtime kept the name; 0 when not. */
static const uint8_t *row_needs(int i)
{
    int k = g_sel_of[i];
    if (!dashed(i) || k < 0 || (uint32_t)k >= DKRT_SEL_MAX || (uint32_t)k >= g_boot.count)
        return 0;
    return g_boot.results[k] == DKRT_E_IMPORT && g_boot.needs[k][0] ? g_boot.needs[k] : 0;
}

/* A row: the name (12), the kind (4) and the memory the module takes, right aligned: 23 letter widths, which fit the list. The columns
   are placed in pixels (t_pad_px), because a space is narrower than a letter. With a mark column the name has 11 characters and the
   other columns stay where they are. */
static void module_text(int i, int exact, txt *t)
{
    t_init(t);
    if (g_lead) {
        t_c(t, dashed(i) ? '-' : PAD);
        row_name(i, NAME_COLS - 1, t);
    } else {
        row_name(i, NAME_COLS, t);
    }
    t_pad_px(t, (NAME_COLS + 1) * 4, exact);
    t_s(t, kind_text(i));
    txt sz;
    t_init(&sz);
    if (g_ent[i].flags & DKRT_SCAN_NEEDS)
        t_b(&sz, g_fit.rows[i].mem, DKRT_FIT_MEM);
    else
        bytes_text(&sz, g_ent[i].size);
    t_pad_px(t, (NAME_COLS + 1 + 4 + 1 + 5) * 4 - px_of(sz.b), exact);
    t_s(t, sz.b);
}

/* Exact padding needs more characters than the letters it stands for (three spaces for 9 pixels); when the row would then pass the
   23 characters a list row keeps, it is padded with PAD only. */
static void module_row(dk_ui_menu *m, int i)
{
    txt t;
    module_text(i, 1, &t);
    if (t.n > DK_UI_ROW - 1)
        module_text(i, 0, &t);
    if (DK_LOAD_N < 40)
        keep(DK_LOAD[DK_LOAD_N++], t.b);
    dk_ui_text(m, t.b);
}

static void fill_modules(dk_ui_menu *m, void *ctx)
{
    (void)ctx;
    DK_LOAD_N = 0;
    g_hdr = 0;
    g_empty_text[0] = 0;
    if (STORE != 1) {
        copy_text((uint8_t *)g_empty_text, sizeof g_empty_text,
                  STORE == 2   ? "NO STORE"
                  : STORE == 3 ? "STORE READ ERROR"
                               : "BAD STORE");
        keep(DK_LOAD[DK_LOAD_N++], g_empty_text);
        return;
    }
    if (NENT == 0) {
        copy_text((uint8_t *)g_empty_text, sizeof g_empty_text, "NO MODULES ON CARD");
        keep(DK_LOAD[DK_LOAD_N++], g_empty_text);
        return;
    }
    refit();
    g_lead = 0;
    for (uint32_t i = 0; i < NENT; i++)
        if (g_saved[i] && !LOADED[i])
            g_lead = 1;
    g_hdr = g_boot.safe != 0;
    if (g_hdr)
        dk_ui_text(m, "SAFE MODE: NOT LOADED");
    for (uint32_t i = 0; i < NENT; i++)
        module_row(m, (int)i);
    for (uint32_t i = 0; i < MAX_ENT; i++)
        dk_ui_check(m, (int)i + g_hdr, i < NENT && TICK[i] && !dashed((int)i));
    if (g_hdr)
        dk_ui_check(m, 0, 0);
}

/* The check mark of a row after TICK changed. A saved row that is not loaded also changes its "-", which is row text. */
static void show_tick(int row)
{
    dk_ui_check(g_modules, row + g_hdr, TICK[row] && !dashed(row));
    if (g_saved[row] && !LOADED[row])
        dk_ui_refresh(g_modules);
}

static void refuse(int row, const char *what)
{
    txt title;
    t_init(&title);
    row_name(row, 10, &title);
    t_c(&title, ' ');
    t_s(&title, what);
    if (NOK[row]) {
        say(title.b, "BUILT FOR ANOTHER DEVICE OR RUNTIME", 0);
    } else {
        txt why;
        t_init(&why);
        t_b(&why, g_fit.rows[row].reason, DKRT_FIT_REASON);
        say(title.b, why.b, 0);
    }
}

/* Checks or unchecks a row. A module that does not fit next to the checked ones, or that this runtime or device cannot load, stays
   unchecked and a dialog says why. */
static void set_tick(int row, int on)
{
    if (g_selection_error != DKRT_OK) {
        fail_text("SELECTION READ ERROR", g_selection_error);
        return;
    }
    if (row < 0 || row >= (int)NENT)
        return;
    if (!on) {
        TICK[row] = 0;
        show_tick(row);
        return;
    }
    if (NOK[row]) {
        TICK[row] = 0;
        show_tick(row);
        refuse(row, "NOT COMPATIBLE");
        return;
    }
    TICK[row] = 1;
    refit();
    if (wont(row)) {
        TICK[row] = 0;
        show_tick(row);
        refuse(row, "DOES NOT FIT");
        return;
    }
    show_tick(row);
}

static void modules_check(void *ctx, int row, int on)
{
    (void)ctx;
    row -= g_hdr;
    if (row < 0) { /* the header is not a module */
        dk_ui_check(g_modules, 0, 0);
        return;
    }
    if (row < (int)NENT &&
        dashed(row)) /* the check mark is off for it, so YES came as "on": it means off */
        on = 0;
    set_tick(row, on);
}

static void panel_load(void *ctx, int row)
{
    (void)ctx;
    if (row >= 0 && row < (int)NENT)
        set_tick(row, !TICK[row]);
}

static void route_done(void *ctx, int index)
{
    (void)ctx;
    if (index < 0 || (uint32_t)index >= DK_ROUTE_N || g_route_row < 0)
        return;
    int j = g_row_fx[g_route_row];
    if (j < 0)
        return;
    for (uint32_t k = 0; k < g_fx_n; k++)
        if (same_text(g_fx[k].id, g_fx[j].id, 32))
            DK_FXR_PEND[k] = DK_ROUTE_ITEM[index];
}

/* ROUTE...: SEND and MASTER, limited to the routes the effect allows and to the send slots that are free. */
static void panel_route(void *ctx, int row)
{
    (void)ctx;
    int j = row >= 0 && row < (int)NENT ? g_row_fx[row] : -1;
    if (j < 0)
        return;
    static const char *items[2];
    DK_ROUTE_N = 0;
    g_route_row = row;
    if ((g_fx[j].allowed & ROUTE_SEND) &&
        (DK_FXR_PEND[j] == ROUTE_SEND || sends_except(j) < g_fx_slots)) {
        items[DK_ROUTE_N] = "SEND";
        DK_ROUTE_ITEM[DK_ROUTE_N++] = ROUTE_SEND;
    }
    if (g_fx[j].allowed & ROUTE_MASTER) {
        items[DK_ROUTE_N] = "MASTER";
        DK_ROUTE_ITEM[DK_ROUTE_N++] = ROUTE_MASTER;
    }
    if (DK_ROUTE_N)
        dk_ui_choice("ROUTE", items, (int)DK_ROUTE_N, route_done, 0);
}

/* The loaded module of a store row that has a dkm_menu: its dkrt_module index, or -1. */
static int settings_module(int row, DkrtModule *mod)
{
    if (row < 0 || row >= (int)NENT)
        return -1;
    for (uint32_t k = 0; k < MAX_SET && dkrt_module(k, mod) == DKRT_OK; k++)
        if (same_text(g_ent[row].path, mod->path, 63))
            return mod->menu && mod->state == DKRT_MOD_LOADED ? (int)k : -1;
    return -1;
}

/* The fill of a module's settings menu: the module adds the rows. ctx is the module's index. */
static void fill_settings(dk_ui_menu *m, void *ctx)
{
    static DkrtModule mod;
    if (dkrt_module((uint32_t)(uintptr_t)ctx, &mod) == DKRT_OK && mod.menu) {
        if (mod.ctx)
            DK_CALL_MENU_CTX(mod.menu, mod.ctx, m);
        else
            DK_CALL_MENU(mod.menu, m);
    }
}

static void panel_settings(void *ctx, int row)
{
    (void)ctx;
    static DkrtModule mod;
    int k = settings_module(row, &mod);
    if (k < 0)
        return;
    if (!g_set[k]) {
        /* the title is the module's name without its author (digikit/remote: REMOTE); the wide title bar holds 28 characters; the stock bar cuts a longer one at the start with '..' */
        int from = 0;
        for (int c = 0; c < 32 && mod.name[c]; c++)
            if (mod.name[c] == '/')
                from = c + 1;
        copy_text((uint8_t *)g_set_title[k], NAME_ROW, "");
        for (int c = 0; c < NAME_ROW - 1 && mod.name[from + c]; c++)
            g_set_title[k][c] = upper((char)mod.name[from + c]);
        g_set_desc[k].name = "DigikitModuleMenu";
        g_set_desc[k].title = g_set_title[k];
        g_set_desc[k].flags = DK_UI_WIDE;
        g_set_desc[k].fill = fill_settings;
        g_set_desc[k].ctx = (void *)(uintptr_t)k;
        g_set[k] = dk_ui_menu_new(&g_set_desc[k]);
    }
    if (g_set[k])
        dk_ui_open(g_set[k]);
}

static void panel_info(void *ctx, int row)
{
    (void)ctx;
    g_info_row = row;
    dk_ui_open(g_info);
}

static void prow(dk_ui_menu *p, const char *text, dk_ui_act_fn *fn, int row)
{
    if (DK_PANEL_N < 4)
        keep(DK_PANEL[DK_PANEL_N++], text);
    dk_ui_action(p, text, fn, 0, row);
}

static void modules_panel(dk_ui_menu *p, int row, void *ctx)
{
    (void)ctx;
    DK_PANEL_N = 0;
    row -= g_hdr;
    if (row < 0 || row >= (int)NENT)
        return;
    prow(p, TICK[row] ? "UNLOAD" : "LOAD", panel_load, row);
    if (g_row_fx[row] >= 0)
        prow(p, "ROUTE...", panel_route, row);
    DkrtModule mod;
    if (settings_module(row, &mod) >= 0)
        prow(p, "SETTINGS...", panel_settings, row);
    prow(p, "INFO", panel_info, row);
    /* the detail of a dashed row, last so that the cursor starts on LOAD: why it is not loaded (a text row, no action) */
    const uint8_t *needs = row_needs(row);
    if (needs) {
        txt t;
        t_init(&t);
        t_s(&t, "NEEDS ");
        t_needs(&t, needs);
        if (DK_PANEL_N < 4)
            keep(DK_PANEL[DK_PANEL_N++], t.b);
        dk_ui_text(p, t.b);
    }
}

/* ---- INFO ------------------------------------------------------------------------------------------------------------ */

static void irow(dk_ui_menu *m, const txt *t)
{
    dk_ui_text(m, t->b);
}

static void fill_info(dk_ui_menu *m, void *ctx)
{
    (void)ctx;
    int i = g_info_row;
    if (i < 0 || i >= (int)NENT) {
        dk_ui_text(m, "NO MODULE");
        return;
    }
    const DkrtScan *e = &g_ent[i];
    txt t;
    t_init(&t);
    row_name(i, 23, &t);
    irow(m, &t);
    /* the version is in the file's header; the runtime reads it */
    t_init(&t);
    t_s(&t, "VERSION ");
    DkrtModuleHeader hdr;
    if (dkrt_module_header(e->first, &hdr) == DKRT_OK) {
        t_num(&t, hdr.version >> 16);
        t_c(&t, '.');
        t_num(&t, hdr.version >> 8 & 255);
        t_c(&t, '.');
        t_num(&t, hdr.version & 255);
    } else {
        t_s(&t, "--");
    }
    irow(m, &t);
    t_init(&t);
    t_s(&t, "SIZE ");
    if (e->flags & DKRT_SCAN_NEEDS)
        t_b(&t, g_fit.rows[i].mem, DKRT_FIT_MEM);
    else
        bytes_text(&t, e->size);
    irow(m, &t);
    /* the module alone: its share of the DSP and the CPU memory */
    static uint8_t only[MAX_ENT];
    for (uint32_t k = 0; k < MAX_ENT; k++)
        only[k] = (int)k == i;
    dkrt_fit(g_ent, NENT, only, &g_fit);
    t_init(&t);
    const uint8_t *line = g_fit.line;
    int cut = 0;
    while (line[cut] && !(line[cut] == ' ' && line[cut + 1] == 'C' && line[cut + 2] == 'P'))
        cut++;
    if (line[0]) {
        t_b(&t, line, cut);
        irow(m, &t);
        t_init(&t);
        t_b(&t, line + (line[cut] ? cut + 1 : cut), DKRT_FIT_LINE);
        irow(m, &t);
    } else {
        t_s(&t, "DSP --");
        irow(m, &t);
        t_init(&t);
        t_s(&t, "CPU --");
        irow(m, &t);
    }
    /* does it fit next to the checked ones */
    for (uint32_t k = 0; k < MAX_ENT; k++)
        only[k] = TICK[k] || (int)k == i;
    dkrt_fit(g_ent, NENT, only, &g_fit);
    t_init(&t);
    if (NOK[i]) {
        t_s(&t, "FIT: NOT COMPATIBLE");
    } else if (wont(i)) {
        t_s(&t, "NO FIT: ");
        t_b(&t, g_fit.rows[i].reason, DKRT_FIT_REASON);
    } else {
        t_s(&t, "FITS");
    }
    irow(m, &t);
    t_init(&t);
    t_s(&t, "STATUS ");
    t_s(&t, e->flags & DKRT_SCAN_LOADING  ? "LOADING"
            : e->flags & DKRT_SCAN_LOADED ? "LOADED"
                                          : "NOT LOADED");
    irow(m, &t);
    if (row_needs(i)) {
        t_init(&t);
        t_s(&t, "NEEDS ");
        t_needs(&t, row_needs(i));
        irow(m, &t);
    }
    if ((TICK[i] != 0) != (LOADED[i] != 0)) {
        t_init(&t);
        t_s(&t, TICK[i] ? "LOADS AT RESTART" : "UNLOADS AT RESTART");
        irow(m, &t);
    }
    int j = g_row_fx[i];
    if (j >= 0) {
        t_init(&t);
        t_s(&t, "ROUTE ");
        t_s(&t, DK_FXR_PEND[j] == ROUTE_SEND ? "SEND" : "MASTER");
        irow(m, &t);
    }
    /* the next fill of the list shows the verdicts of the checked set again */
    refit();
}

/* ---- leaving MODULES: APPLY & RESTART? ------------------------------------------------------------------------------ */

/* Saves the selection and the changed routes, then restarts. dkrt_restart returns only when it refuses. */
static void apply(void)
{
    if (g_selection_error != DKRT_OK) {
        fail_text("NOT SAVED", g_selection_error);
        return;
    }
    DK_APPLIES++;
    static DkrtSelection sel;
    uint32_t n = 0;
    sel.flags = 0;
    for (uint32_t i = 0; i < DKRT_SEL_MAX; i++)
        for (uint32_t c = 0; c < DKRT_SEL_PATH; c++)
            sel.paths[i][c] = 0;
    for (uint32_t i = 0; i < NENT; i++) {
        if (TICK[i] && n < DKRT_SEL_MAX) {
            const uint8_t *p = g_ent[i].path;
            int l = path_len(p, 63);
            if (l < DKRT_SEL_PATH) {
                for (int c = 0; c < l; c++)
                    sel.paths[n][c] = p[c];
                n++;
            }
        }
    }
    sel.count = n;
    int32_t r = dkrt_selection_set(&sel);
    DK_APPLY_SET = r;
    if (r != DKRT_OK) {
        fail_text("NOT SAVED", r);
        return;
    }
    for (uint32_t j = 0; j < g_fx_n; j++) {
        if (DK_FXR_PEND[j] != g_fx_saved[j]) {
            txt key;
            t_init(&key);
            t_s(&key, "fx/route/");
            t_b(&key, g_fx[j].id, 32);
            uint8_t v = DK_FXR_PEND[j] == ROUTE_SEND;
            r = dkrt_settings_put((const uint8_t *)key.b, &v, 1);
            DK_FXR_SET = r;
            if (r != DKRT_OK) {
                fail_text("NOT SAVED", r);
                return;
            }
        }
    }
    r = dkrt_settings_flush();
    DK_FXR_FLUSH = r;
    if (r != DKRT_OK) {
        fail_text("NOT SAVED", r);
        return;
    }
    for (uint32_t i = 0; i < MAX_ENT; i++)
        g_saved[i] = TICK[i];
    for (uint32_t j = 0; j < g_fx_n; j++)
        g_fx_saved[j] = DK_FXR_PEND[j];
    g_saved_extra = 0;
    r = dkrt_restart();
    DK_APPLY_RESTART = r;
    fail_text("NO RESTART", r);
}

/* NO on the confirm drops the pending changes and closes the list. */
static void discard(void)
{
    for (uint32_t i = 0; i < MAX_ENT; i++)
        TICK[i] = g_saved[i];
    for (uint32_t j = 0; j < g_fx_n; j++)
        DK_FXR_PEND[j] = g_fx_saved[j];
    dk_ui_close(g_modules);
}

static void apply_answer(void *ctx, int yes)
{
    (void)ctx;
    if (yes)
        apply();
    else
        discard();
}

static int modules_back(void *ctx)
{
    (void)ctx;
    uint32_t n = changes();
    DK_PENDING = n;
    if (!restart_pending())
        return 0;
    txt t;
    t_init(&t);
    if (n) {
        t_num(&t, n);
        t_s(&t, n == 1 ? " CHANGE" : " CHANGES");
    } else {
        t_s(&t, "LEAVES SAFE MODE");
    }
    say("APPLY & RESTART?", t.b, apply_answer);
    return 1;
}

static void open_modules(void *ctx, int arg)
{
    (void)ctx;
    (void)arg;
    read_boot();
    scan_store();
    read_fx();
    match_fx();
    dk_ui_select(g_modules,
                 STORE == 1 && NENT > 0 &&
                     g_boot.safe != 0); /* the first module, below the safe-mode header */
    dk_ui_open(g_modules);
}

/* ---- the SETTINGS row ------------------------------------------------------------------------------------------------ */

static const dk_ui_menu_desc main_desc = {
    .name = "DigikitMenu", .title = "DIGIKIT", .fill = fill_main};
static const dk_ui_menu_desc about_desc = {
    .name = "DigikitAboutMenu", .title = "ABOUT", .flags = DK_UI_TEXT_PAGE, .fill = fill_about};
static const dk_ui_menu_desc health_desc = {
    .name = "DigikitHealthMenu", .title = "HEALTH", .flags = DK_UI_TEXT_PAGE, .fill = fill_health};
static const dk_ui_menu_desc mem_desc = {
    .name = "DigikitMemoryMenu", .title = "MEMORY", .flags = DK_UI_TEXT_PAGE, .fill = fill_mem};
static const dk_ui_menu_desc info_desc = {
    .name = "DigikitInfoMenu", .title = "INFO", .flags = DK_UI_TEXT_PAGE, .fill = fill_info};
static const dk_ui_menu_desc modules_desc = {.name = "DigikitModulesMenu",
                                             .title = "MODULES",
                                             .flags = DK_UI_LIST,
                                             .fill = fill_modules,
                                             .empty = g_empty_text,
                                             .panel = modules_panel,
                                             .check = modules_check,
                                             .back = modules_back};

static void open_main(void *ctx, int arg)
{
    (void)ctx;
    (void)arg;
    read_boot();
    if (!g_main) {
        g_about = dk_ui_menu_new(&about_desc);
        DK_HEALTH_MENU = dk_ui_menu_new(&health_desc);
        g_mem = dk_ui_menu_new(&mem_desc);
        g_info = dk_ui_menu_new(&info_desc);
        g_modules = dk_ui_menu_new(&modules_desc);
        g_main = dk_ui_menu_new(&main_desc);
    }
    if (!g_main)
        return;
    dk_ui_open(g_main);
    if (g_boot.safe != 0 && !g_safe_shown) {
        g_safe_shown = 1;
        safe_notice(0, 0);
    }
}

/* Called by core/ui from its hook at the end of the SETTINGS rebuild, with the settings menu: adds DIGIKIT after SYSTEM. */
void dk_app_settings_rows(uint8_t *settings_menu)
{
    dk_ui_stock_row(settings_menu, "DIGIKIT", open_main, 0);
}

/* For the test that calls the module in place: the number of .dkm entries found by the last scan. */
uint32_t dk_entry_count(void)
{
    return NENT;
}
/* x */
