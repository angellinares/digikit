/* core/ui: the device adapter behind <dk/platform/ui.h> (docs/plans/sdk-api.md phase 3, docs/findings/71-ui-components-1-17.md).
 *
 * The same source runs on the Digitakt II and the Digitone II: every firmware call is a generated anchor (dk_fw_*.h).
 *
 * Views. A plain menu or text page is a stock VerticalMenuView, a DK_UI_LIST is a stock MenuWithPopoutSideBarsView (a list with a
 * title bar, check marks and a right side panel). Each is built once, in the firmware heap, and kept for the session through a
 * shared_ptr<View> this module owns (dk_make_sp), so the firmware never frees it. The view stack only holds copies.
 *
 * Rows are stock MenuItems. Their std::functions keep two words in the function's own storage (the menu and the row number), so
 * a copy copies two words, a destroy does nothing and nothing is allocated per row. The row data (callback, context, value
 * pointer, limits) is in an array owned by the menu; a refill rewrites it.
 *
 * Own vtables. The View sub-object of a view reaches its paint and key handler through the View vtable, whose entries are
 * thunks that branch to the real function. A list and a text page get a copy of that vtable (module RAM, written once) with
 * slot 4 (paint) and, for a list, slot 2 (key) replaced; the replacement calls the stock thunk for everything it does not handle.
 *
 * Dialogs. ConfirmWindow and MultiChoiceWindow are make_shared blocks built by hand (0x10 control block, then the object), pushed
 * on the Brain's view manager. The firmware frees them in its sweep. One dialog at a time: g_open holds the serial of the open
 * one; the answer callback clears it, and so does the detach callback of the window (a choice cancelled with NO has no answer).
 *
 * Module RAM is written only by this code, never by the firmware (the menu proof checks stray writes). */
#include <stdint.h>
#include <stddef.h>
#include <dk/core/status.h>
#include <dk/gen/imports.h>
#include <dk/platform/ui.h>
#include "dk_ui_core.h"
#include "tick.h"
#include "dkrt.h"
#include "dk_fw_mem.h"
#include "dk_fw_string.h"
#include "dk_fw_view.h"
#include "dk_fw_menu.h"
#include "dk_fw_window.h"
#include "dk_fw_text.h"
#include "dk_fw_draw.h"

/* The control-block vtables of the two make_shared window types (data anchors; the address is the vtable). */
extern const uint8_t fw_confirm_cb_vt __asm__("cf.window.site.confirm_cb_vptr_lit");
extern const uint8_t fw_choice_cb_vt __asm__("cf.window.site.multichoice_cb_vptr_lit");

/* assembly shim */
void dk_make_sp(shared_ptr *out, uint8_t *parent, uint8_t *view);

#define VIEW_AT 0x38     /* the View sub-object of a Menu view */
#define VMV_FONT_AT 0xa8 /* VerticalMenuView: pointer to the font */
#define CMP_FONT_AT 0x94 /* CompactVerticalMenuView: the font object itself */
#define POP_PANEL_AT 0x380
#define POP_EXT_AT 0x548 /* our word behind a popout view: the dk_ui_menu */
#define VT_WORDS 19      /* View vtable: offset_to_top, typeinfo, 17 slots */
#define VT_PAINT 4
#define VT_KEY 2
#define VT_DETACH_ADD 16
#define VT_CLOSE 10
#define KEY_YES 10
#define KEY_YES2 0x31
#define KEY_NO 12
#define KEY_RIGHT 15
#define MAX_CHOICE 32
#define LIST_ROWS 8
#define LIST_COLS 20 /* characters of a list row that the label of a toggle or value row fills */

enum { K_MENU, K_TEXT, K_LIST, K_PANEL };
enum { R_ACTION, R_SUBMENU, R_TEXT, R_TOGGLE, R_VALUE };

typedef struct row {
    uint8_t kind;
    dk_ui_act_fn *fn;
    void *ctx;
    int arg;
    int *value;
    int min, max;
    dk_ui_menu *sub;
} row;

struct dk_ui_menu {
    const dk_ui_menu_desc *d;
    uint8_t *obj;   /* the Menu object; the View sub-object is at +0x38 */
    shared_ptr *sp; /* kept for the session; in the firmware heap */
    uint8_t *font;
    uint8_t kind;
    uint8_t filling;
    dk_ui_menu *owner; /* a panel: its list */
    dk_ui_menu *panel; /* a list: its side panel */
    row *rows;
    int n, cap;
};

/* std::function<...> as the firmware lays it out: 8 bytes of storage, manager, invoker. */
typedef struct fn {
    uint32_t w0, w1, mgr, inv;
} fn;

typedef uint32_t (*mgr_fn)(uint32_t *dest, const uint32_t *src, uint32_t op);

static const uint32_t typeinfo_dummy = 0;

static uint32_t fn_manager(uint32_t *dest, const uint32_t *src, uint32_t op)
{
    switch (op) {
    case 0:
        *dest = (uint32_t)&typeinfo_dummy;
        break;
    case 1:
        *dest = (uint32_t)src;
        break;
    case 2:
        dest[0] = src[0];
        dest[1] = src[1];
        break;
    default:
        break;
    }
    return 0;
}

static fn make_fn(uint32_t w0, uint32_t w1, void *inv)
{
    fn f = {w0, w1, (uint32_t)fn_manager, (uint32_t)inv};
    return f;
}

/* ---- small helpers ---------------------------------------------------------------------------------------------------- */

static uint32_t *vtable(uint8_t *p)
{
    return *(uint32_t **)p;
}

static void zero(void *p, uint32_t n)
{
    volatile uint8_t *b = (volatile uint8_t *)p;
    while (n--)
        *b++ = 0;
}

static uint8_t *fw_new(uint32_t n)
{
    uint8_t *p = dk_fw_mem_new(n);
    if (p)
        zero(p, n);
    return p;
}

/* Copies at most max-1 bytes of s into buf and terminates it. */
static void cut(char *buf, uint32_t max, const char *s)
{
    uint32_t i = 0;
    if (s)
        while (s[i] && i + 1 < max) {
            buf[i] = s[i];
            i++;
        }
    buf[i] = 0;
}

static uint32_t read_u32(const uint8_t *p)
{
    return *(const uint32_t *)p;
}

static void invalidate(dk_ui_menu *m)
{
    dk_fw_view_invalidate(m->obj + VIEW_AT);
}

static uint8_t *view_manager(void)
{
    return dk_fw_view_brain_view_manager(dk_fw_view_brain_instance());
}

/* The stock icon the builder always wants: the operand of the `pea` of the SETTINGS > SYSTEM item. */
static const uint8_t *stock_icon(void)
{
    const uint16_t *r = (const uint16_t *)&dk_fw_menu_settings_list_system_icon_ref;
    __asm__("" : "+r"(r)); /* the compiler must not see the 1-byte object */
    return (const uint8_t *)(((uint32_t)r[1] << 16) | r[2]);
}

/* A zeroed heap block the size of a VerticalMenuView's tail: the parent dk_make_sp clears a shared_ptr in (at +0x90). */
static uint8_t *g_parent;

/* ---- rows ------------------------------------------------------------------------------------------------------------- */

static void run_row(dk_ui_menu *m, int i);
static void draw_row(dk_ui_menu *m, int i, uint8_t *canvas, int x, int y);
static void adjust_row(dk_ui_menu *m, int i, int delta);

static void inv_act(const uint32_t *any)
{
    run_row((dk_ui_menu *)any[0], (int)any[1]);
}

static void inv_draw(const uint32_t *any, uint8_t *item, uint8_t *canvas, int x, int y)
{
    (void)item;
    draw_row((dk_ui_menu *)any[0], (int)any[1], canvas, x, y);
}

static void inv_adjust(const uint32_t *any, uint8_t *item, int delta)
{
    (void)item;
    adjust_row((dk_ui_menu *)any[0], (int)any[1], delta);
}

/* Field by field: a struct assignment would call memcpy, which this module does not link. */
static void rowcpy(row *d, const row *s)
{
    d->kind = s->kind;
    d->fn = s->fn;
    d->ctx = s->ctx;
    d->arg = s->arg;
    d->value = s->value;
    d->min = s->min;
    d->max = s->max;
    d->sub = s->sub;
}

static row *add_row(dk_ui_menu *m)
{
    if (m->n == m->cap) {
        int cap = m->cap ? m->cap * 2 : LIST_ROWS;
        row *r = (row *)fw_new((uint32_t)cap * sizeof(row));
        if (!r)
            return 0;
        for (int i = 0; i < m->n; i++)
            rowcpy(&r[i], &m->rows[i]);
        if (m->rows)
            dk_fw_mem_free((uint8_t *)m->rows);
        m->rows = r;
        m->cap = cap;
    }
    row *r = &m->rows[m->n];
    r->kind = 0;
    r->fn = 0;
    r->ctx = 0;
    r->arg = 0;
    r->value = 0;
    r->min = r->max = 0;
    r->sub = 0;
    return r;
}

/* One MenuItem with a text label and an action. `adj` is an empty or an encoder function, `draw` 0 (no value drawn) or the function
   that draws the value. `keep_icon`: keep the stock icon the builder gives every item (a row among the stock SETTINGS rows). */
static uint8_t *make_item(const char *text, const fn *act, const fn *adj, const fn *draw,
                          int keep_icon)
{
    char label[DK_UI_ROW];
    cut(label, sizeof label, text);
    uint8_t *item = fw_new(sizeof(MenuItem));
    if (!item)
        return 0;
    dk_fw_menu_item_builder(item, label, (const std_function *)act, stock_icon(),
                            (const std_function *)adj);
    if (keep_icon)
        return item;
    /* the builder gave the item the stock icon's draw function; drop it (its manager frees the copied bitmap) */
    fn *d = (fn *)(item + offsetof(MenuItem, draw));
    if (d->mgr)
        ((mgr_fn)d->mgr)(&d->w0, &d->w0, 3);
    if (draw) {
        d->w0 = draw->w0;
        d->w1 = draw->w1;
        d->mgr = draw->mgr;
        d->inv = draw->inv;
    } else {
        d->w0 = d->w1 = d->mgr = d->inv = 0;
    }
    return item;
}

/* A list calls the draw function of an item with an origin that is not the row's (a value drawn there lands on the title bar). A toggle or
   value row of a list shows its value in its label instead: the text, then ON, OFF or the number (three digits) at the right end of
   LIST_COLS columns. A change fills the list again (value_changed), which writes the label again. */
static void value_label(char *out, const char *text, const row *r)
{
    char val[4];
    int v = r->value ? *r->value : 0, vn = 3, n = 0;
    if (r->kind == R_TOGGLE) {
        val[0] = 'O';
        val[1] = v ? 'N' : 'F';
        val[2] = 'F';
        vn = v ? 2 : 3;
    } else {
        val[0] = v < 0 ? '-' : (char)('0' + (v / 100) % 10);
        if (v < 0)
            v = -v;
        val[1] = (char)('0' + (v / 10) % 10);
        val[2] = (char)('0' + v % 10);
    }
    while (text[n] && n < LIST_COLS - vn - 1) {
        out[n] = text[n];
        n++;
    }
    while (n < LIST_COLS - vn)
        out[n++] = ' ';
    for (int k = 0; k < vn; k++)
        out[n++] = val[k];
    out[n] = 0;
}

/* A toggle or value changed: a list writes its labels again (the selection stays), a menu draws the value. */
static void refill(dk_ui_menu *m, int top);
static void value_changed(dk_ui_menu *m)
{
    if (m->kind == K_LIST)
        refill(m, 0);
    else
        invalidate(m);
}

static void add_item(dk_ui_menu *m, const char *text, int i, int with_value)
{
    char label[DK_UI_ROW];
    int in_list = m->kind == K_LIST && with_value &&
                  (m->rows[i].kind == R_TOGGLE || m->rows[i].kind == R_VALUE);
    if (in_list) {
        value_label(label, text, &m->rows[i]);
        text = label;
    }
    fn act = make_fn((uint32_t)m, (uint32_t)i, inv_act);
    fn adj = make_fn(0, 0, 0);
    adj.mgr = 0;
    if (with_value && m->rows[i].kind == R_VALUE)
        adj = make_fn((uint32_t)m, (uint32_t)i, inv_adjust);
    fn draw = make_fn((uint32_t)m, (uint32_t)i, inv_draw);
    uint8_t *item = make_item(text, &act, &adj, with_value && !in_list ? &draw : 0, 0);
    if (item)
        dk_fw_menu_add_item(m->obj, item);
}

/* For other core modules: a row among the rows of a stock menu (SETTINGS), with its icon. fn(ctx, 0) runs on YES. */
static void inv_stock(const uint32_t *any)
{
    ((dk_ui_act_fn *)any[0])((void *)any[1], 0);
}

void dk_ui_stock_row(uint8_t *stock_menu, const char *text, dk_ui_act_fn *fn_, void *ctx)
{
    fn act = make_fn((uint32_t)fn_, (uint32_t)ctx, inv_stock);
    fn adj = make_fn(0, 0, 0);
    adj.mgr = 0;
    uint8_t *item = make_item(text, &act, &adj, 0, 1);
    if (item)
        dk_fw_menu_add_item(stock_menu, item);
}

static void add(dk_ui_menu *m, const char *text, uint8_t kind, dk_ui_act_fn *fn_, void *ctx,
                int arg, int with_value)
{
    if (!m)
        return;
    row *r = add_row(m);
    if (!r)
        return;
    r->kind = kind;
    r->fn = fn_;
    r->ctx = ctx;
    r->arg = arg;
    m->n++;
    add_item(m, text, m->n - 1, with_value);
    invalidate(m);
}

static void list_focus_main(dk_ui_menu *m)
{
    if (m->kind == K_PANEL && m->owner)
        dk_fw_menu_popout_set_active(m->owner->obj, m->owner->obj);
}

int dk_ui_open(dk_ui_menu *m);

static void run_row(dk_ui_menu *m, int i)
{
    if (!m || i < 0 || i >= m->n)
        return;
    row r;
    rowcpy(&r, &m->rows[i]); /* a callback may refill the menu */
    list_focus_main(m);
    switch (r.kind) {
    case R_ACTION:
        if (r.fn)
            r.fn(r.ctx, r.arg);
        break;
    case R_SUBMENU:
        dk_ui_open(r.sub);
        break;
    case R_TOGGLE:
        if (r.value) {
            int v = *r.value == 0;
            *r.value = v;
            value_changed(m);
            if (r.fn)
                r.fn(r.ctx, v);
        }
        break;
    default:
        break;
    }
}

static void adjust_row(dk_ui_menu *m, int i, int delta)
{
    if (!m || i < 0 || i >= m->n || m->rows[i].kind != R_VALUE || !m->rows[i].value)
        return;
    row r;
    rowcpy(&r, &m->rows[i]);
    int v = *r.value + delta;
    if (v > r.max)
        v = r.max;
    if (v < r.min)
        v = r.min;
    if (v == *r.value)
        return;
    *r.value = v;
    value_changed(m);
    if (r.fn)
        r.fn(r.ctx, v);
}

static void draw_row(dk_ui_menu *m, int i, uint8_t *canvas, int x, int y)
{
    if (!m || i < 0 || i >= m->n || !m->font)
        return;
    const row *r = &m->rows[i];
    int v = r->value ? *r->value : 0;
    if (r->kind == R_TOGGLE)
        dk_fw_text_draw_box(canvas, m->font, x + 0x4e, y + 2, 2, 0, 0, "%s", v ? "ON" : "OFF");
    else if (r->kind == R_VALUE)
        dk_fw_text_draw_box(canvas, m->font, x + 0x4e, y + 2, 2, 0, 0, "%03d", v);
}

void dk_ui_action(dk_ui_menu *m, const char *text, dk_ui_act_fn *fn_, void *ctx, int arg)
{
    add(m, text, R_ACTION, fn_, ctx, arg, 0);
}

void dk_ui_submenu(dk_ui_menu *m, const char *text, dk_ui_menu *sub)
{
    if (!sub)
        return;
    if (m) {
        row *r = add_row(m);
        if (!r)
            return;
        r->kind = R_SUBMENU;
        r->sub = sub;
        m->n++;
        add_item(m, text, m->n - 1, 0);
        invalidate(m);
    }
}

void dk_ui_text(dk_ui_menu *m, const char *text)
{
    add(m, text, R_TEXT, 0, 0, 0, 0);
}

void dk_ui_toggle(dk_ui_menu *m, const char *text, int *value, dk_ui_act_fn *changed, void *ctx)
{
    if (!m)
        return;
    row *r = add_row(m);
    if (!r)
        return;
    r->kind = R_TOGGLE;
    r->value = value;
    r->fn = changed;
    r->ctx = ctx;
    m->n++;
    add_item(m, text, m->n - 1, 1);
    invalidate(m);
}

void dk_ui_value(dk_ui_menu *m, const char *text, int *value, int min, int max,
                 dk_ui_act_fn *changed, void *ctx)
{
    if (!m)
        return;
    row *r = add_row(m);
    if (!r)
        return;
    r->kind = R_VALUE;
    r->value = value;
    r->min = min;
    r->max = max;
    r->fn = changed;
    r->ctx = ctx;
    m->n++;
    add_item(m, text, m->n - 1, 1);
    invalidate(m);
}

/* ---- own View vtables ------------------------------------------------------------------------------------------------- */

static uint32_t g_text_vt[VT_WORDS];
static uint32_t g_list_vt[VT_WORDS];
static uint32_t g_text_stock_paint;
static uint32_t g_list_stock_paint;
static uint32_t g_list_stock_key;

/* The stock View vtable slots 2 and 4 of these views are thunks that start with `moveq #-0x38,d0` (0x70c8). */
static int is_thunk(uint32_t addr)
{
    return *(const uint16_t *)addr == 0x70c8;
}

static int rows_of(const uint8_t *obj)
{
    return (int)((read_u32(obj + offsetof(CompactVerticalMenuView, items_end)) -
                  read_u32(obj + offsetof(CompactVerticalMenuView, items_begin))) >>
                 3);
}

/* A text page paints without the highlighted row: the cursor is set out of range around the stock paint. */
static void text_paint(uint8_t *view, uint8_t *canvas)
{
    uint32_t *cursor = (uint32_t *)(view - VIEW_AT + offsetof(VerticalMenuView, cursor));
    uint32_t saved = *cursor;
    *cursor = 0x7fffffff;
    ((void (*)(uint8_t *, uint8_t *))g_text_stock_paint)(view, canvas);
    *cursor = saved;
}

/* A list with no rows. The firmware's list code is not made for an empty Menu (its paint, LED and key code read the item at the
   selection), so an empty list keeps one hidden item (see refill) and its paint is ours: the title bar and the descriptor's empty
   text, centred. The bar is drawn as the stock paint draws it: clear the bar, the title (flags 2 at x 0x40, y 0x41 minus the field at
   +0x84, which is 7), the two side lines, then an invert fill of the inside (y counts from the bottom). */
static void list_paint(uint8_t *view, uint8_t *canvas)
{
    uint8_t *obj = view - VIEW_AT;
    dk_ui_menu *m = *(dk_ui_menu **)(obj + POP_EXT_AT);
    if (!m || !m->d || m->n != 0) {
        ((void (*)(uint8_t *, uint8_t *))g_list_stock_paint)(view, canvas);
        return;
    }
    const CompactVerticalMenuView *c = (const CompactVerticalMenuView *)obj;
    int d = (int)read_u32(obj + 0x84);
    int x0 = c->x, x1 = c->x + (int)c->width - 1;
    const char *title = (const char *)c->title;
    dk_fw_draw_fill_rect(canvas, 0, 0x3f, 0x7f, 0x40 - d, 0);
    if (title)
        dk_fw_text_draw_fmt(canvas, obj + CMP_FONT_AT, 0x40, 0x41 - d, 2, "%s", title);
    dk_fw_draw_fill_rect(canvas, x0, 0x3e, x0, 0x41 - d, -1);
    dk_fw_draw_fill_rect(canvas, x1, 0x3e, x1, 0x41 - d, -1);
    dk_fw_draw_fill_rect(canvas, x0 + 1, 0x3f, x1 - 1, 0x40 - d, -1);
    if (m->d->empty)
        dk_fw_text_draw_fmt(canvas, obj + CMP_FONT_AT, 0x40, 0x1c, 2, "%s", m->d->empty);
}

static void toggle_check(dk_ui_menu *m, int row_);

static void open_panel(dk_ui_menu *m)
{
    dk_ui_menu *pm = m->panel;
    uint8_t *obj = m->obj;
    int row_ = (int)((MenuWithPopoutSideBarsView *)obj)->main.selected;
    if (!pm || !m->d->panel || row_ < 0 || row_ >= m->n)
        return;
    dk_fw_menu_clear_items(pm->obj);
    pm->n = 0;
    pm->filling = 1;
    m->d->panel(pm, row_, m->d->ctx);
    pm->filling = 0;
    if (pm->n > 0)
        dk_fw_menu_popout_set_active(obj, pm->obj);
}

/* Keys of a list. Everything not handled here goes to the stock handler (focus moves, cursor, NO closes). */
static uint32_t list_key(uint8_t *view, uint8_t *ev)
{
    uint8_t *obj = view - VIEW_AT;
    dk_ui_menu *m = *(dk_ui_menu **)(obj + POP_EXT_AT);
    MenuWithPopoutSideBarsView *p = (MenuWithPopoutSideBarsView *)obj;
    uint32_t id = read_u32(ev + 0xc), fl = read_u32(ev + 0x10);
    if (m && m->d && p->active == (uint32_t)obj) {
        if (id == KEY_NO && (fl & 16) && m->d->back && m->d->back(m->d->ctx) == 1)
            return 1;
        if (m->n == 0) /* only the hidden item: nothing to move on or act on; NO closes */
            return id == KEY_NO ? ((uint32_t (*)(uint8_t *, uint8_t *))g_list_stock_key)(view, ev)
                                : 1;
        if (id == KEY_RIGHT && (fl & 1) && m->d->panel && p->has_right) {
            open_panel(m);
            return 1;
        }
        if ((id == KEY_YES || id == KEY_YES2) && (fl & 16) && m->d->check) {
            int row_ = (int)p->main.selected;
            if (row_ >= 0 && row_ < m->n)
                toggle_check(m, row_);
            return 1;
        }
    }
    return ((uint32_t (*)(uint8_t *, uint8_t *))g_list_stock_key)(view, ev);
}

/* Copies the View vtable of `obj` into `copy` and replaces slot 4 (and slot 2 when key is not 0). 0 when a slot is not the thunk. */
static int own_vtable(uint8_t *obj, uint32_t *copy, uint32_t *stock_paint, uint32_t *stock_key,
                      void *paint, void *key)
{
    uint32_t *old = *(uint32_t **)(obj + VIEW_AT);
    if (old == &copy[2])
        return 1;
    if (!*stock_paint) {
        if (!is_thunk(old[VT_PAINT]) || (key && !is_thunk(old[VT_KEY])))
            return 0;
        for (int i = 0; i < VT_WORDS; i++)
            copy[i] = old[i - 2];
        *stock_paint = copy[2 + VT_PAINT];
        copy[2 + VT_PAINT] = (uint32_t)paint;
        if (key) {
            *stock_key = copy[2 + VT_KEY];
            copy[2 + VT_KEY] = (uint32_t)key;
        }
    }
    *(uint32_t **)(obj + VIEW_AT) = &copy[2];
    return 1;
}

/* ---- menus ------------------------------------------------------------------------------------------------------------ */

static int build_menu(dk_ui_menu *m, const dk_ui_menu_desc *d)
{
    uint32_t title;
    uint8_t alloc = 0;
    uint8_t *obj;
    dk_fw_string_std_string_ctor(&title, d->title ? d->title : "", &alloc);
    if (m->kind == K_LIST) {
        obj = fw_new(POP_EXT_AT + 8);
        if (!obj)
            goto fail;
        dk_fw_menu_popout_ctor(obj, d->name, 0x40, 0, 1, 1);
        dk_fw_menu_set_title(obj, &title);
        m->font = obj + CMP_FONT_AT;
        *(dk_ui_menu **)(obj + POP_EXT_AT) = m;
        m->obj = obj;
        if (!own_vtable(obj, g_list_vt, &g_list_stock_paint, &g_list_stock_key, (void *)list_paint,
                        (void *)list_key))
            goto fail;
        dk_ui_menu *pm = (dk_ui_menu *)fw_new(sizeof(dk_ui_menu));
        if (!pm)
            goto fail;
        pm->kind = K_PANEL;
        pm->obj = obj + POP_PANEL_AT;
        pm->font = obj + CMP_FONT_AT;
        pm->owner = m;
        m->panel = pm;
    } else {
        obj = fw_new(sizeof(VerticalMenuView));
        if (!obj)
            goto fail;
        dk_fw_menu_vertical_menu_view_ctor(obj, d->name, 8, &title, 0);
        m->obj = obj;
        m->font = *(uint8_t **)(obj + VMV_FONT_AT);
        if (m->kind == K_TEXT &&
            !own_vtable(obj, g_text_vt, &g_text_stock_paint, 0, (void *)text_paint, 0))
            goto fail;
    }
    dk_fw_string_std_string_dtor(&title);
    m->sp = (shared_ptr *)fw_new(sizeof(shared_ptr));
    if (!m->sp)
        return 0;
    dk_make_sp(m->sp, g_parent, obj + VIEW_AT);
    return 1;
fail:
    dk_fw_string_std_string_dtor(&title);
    return 0;
}

int dk_ui_has(int cap)
{
    return cap >= DK_UI_CAP_MENU && cap <= DK_UI_CAP_TICK;
}

dk_ui_menu *dk_ui_menu_new(const dk_ui_menu_desc *d)
{
    if (!d)
        return 0;
    if (!g_parent) {
        g_parent = fw_new(0xa0);
        if (!g_parent)
            return 0;
    }
    dk_ui_menu *m = (dk_ui_menu *)fw_new(sizeof(dk_ui_menu));
    if (!m)
        return 0;
    m->d = d;
    m->kind = d->flags & (DK_UI_LIST | DK_UI_WIDE) ? K_LIST
              : d->flags & DK_UI_TEXT_PAGE         ? K_TEXT
                                                   : K_MENU;
    if (!build_menu(m, d))
        return 0; /* the partly built objects stay: the firmware heap is not given back */
    return m;
}

/* Fill again: the selection stays on the same row number when it can, or goes to the top (`top`). */
static void refill(dk_ui_menu *m, int top)
{
    if (m->filling)
        return;
    int sel = top ? 0 : (int)((CompactVerticalMenuView *)m->obj)->selected;
    dk_fw_menu_clear_items(m->obj);
    m->n = 0;
    m->filling = 1;
    if (m->d->fill)
        m->d->fill(m, m->d->ctx);
    m->filling = 0;
    if (m->n == 0 && m->kind == K_LIST)
        add_item(m, "", 0,
                 0); /* the hidden item of an empty list: m->n stays 0, so run_row ignores it */
    int count = rows_of(m->obj);
    if (count > 0) {
        if (sel >= count)
            sel = count - 1;
        if (sel < 0)
            sel = 0;
        ((void (*)(uint8_t *, uint32_t, uint32_t))vtable(m->obj)[7])(m->obj, (uint32_t)sel, 2);
    }
    invalidate(m);
}

int dk_ui_is_open(const dk_ui_menu *m)
{
    if (!m || !m->obj || m->kind == K_PANEL)
        return 0;
    const uint8_t *v = m->obj + VIEW_AT;
    return read_u32(v + offsetof(View, mgr)) != 0 && v[0x30] == 0;
}

int dk_ui_open(dk_ui_menu *m)
{
    if (!m || !m->obj || !m->d || m->kind == K_PANEL)
        return -1;
    refill(m, m->kind == K_TEXT && !dk_ui_is_open(m)); /* a text page opens at its first row */
    if (dk_ui_is_open(m))
        return 0;
    return dk_fw_view_view_controller_push(view_manager(), m->sp, 0) ? 0 : -1;
}

void dk_ui_refresh(dk_ui_menu *m)
{
    if (m && m->obj && m->d && m->kind != K_PANEL)
        refill(m, 0);
}

void dk_ui_select(dk_ui_menu *m, int row_)
{
    if (!m || !m->obj || !m->d || m->kind == K_PANEL || row_ < 0)
        return;
    ((CompactVerticalMenuView *)m->obj)->selected =
        (uint32_t)row_; /* the next fill puts the cursor there (clamped to the rows) */
    if (dk_ui_is_open(m))
        refill(m, 0);
}

void dk_ui_close(dk_ui_menu *m)
{
    if (!dk_ui_is_open(m))
        return;
    uint8_t *v = m->obj + VIEW_AT;
    ((void (*)(uint8_t *))vtable(v)[VT_CLOSE])(v);
}

/* ---- check marks ------------------------------------------------------------------------------------------------------ */

int dk_ui_checked(const dk_ui_menu *list, int row_)
{
    if (!list || list->kind != K_LIST || row_ < 0)
        return 0;
    return dk_fw_menu_is_checked(list->obj, row_) & 1;
}

void dk_ui_check(dk_ui_menu *list, int row_, int on)
{
    if (!list || list->kind != K_LIST || row_ < 0)
        return;
    int cur = dk_ui_checked(list, row_);
    if (on && !cur)
        dk_fw_menu_check(list->obj, row_);
    else if (!on && cur)
        dk_fw_menu_uncheck(list->obj, row_);
    invalidate(list);
}

static void toggle_check(dk_ui_menu *m, int row_)
{
    int on = !dk_ui_checked(m, row_);
    dk_ui_check(m, row_, on);
    if (m->d->check)
        m->d->check(m->d->ctx, row_, on);
}

/* ---- dialogs ---------------------------------------------------------------------------------------------------------- */

static uint32_t g_open; /* serial of the open dialog, 0: none */
static uint32_t g_serial;

/* the answer: the window closed itself first, then runs this */
static void inv_answer(const uint32_t *any, int arg)
{
    g_open = 0;
    void (*done)(void *, int) = (void (*)(void *, int))any[0];
    if (done)
        done((void *)any[1], arg);
}

static void inv_answer_bool(const uint32_t *any, int arg)
{
    inv_answer(any, (arg & 0xff) != 0);
}

/* the window left the view stack (detach, with the manager and the view as arguments): any way of closing ends the dialog */
static void inv_detach(const uint32_t *any)
{
    if (g_open == any[0])
        g_open = 0;
}

static void on_detach(uint8_t *view, uint32_t serial)
{
    fn f = make_fn(serial, 0, inv_detach);
    ((void (*)(uint8_t *, fn *))vtable(view)[VT_DETACH_ADD])(view, &f);
}

/* The make_shared block header: the control-block vtable, use and weak count 1, the object pointer (the object is at +0x10). */
static void init_block(uint8_t *blk, const uint8_t *cb_vt)
{
    uint32_t *b = (uint32_t *)blk;
    b[0] = (uint32_t)cb_vt;
    b[1] = 1;
    b[2] = 1;
    b[3] = (uint32_t)blk + 0x10;
}

/* Pushes the block on the view manager; 0 when shown. Our count of 1 is released, the stack's copy keeps the window. */
static int show(uint8_t *blk, uint32_t serial)
{
    shared_ptr sp = {(uint32_t)blk + 0x10, (uint32_t)blk};
    on_detach(blk + 0x10, serial);
    uint32_t ok = dk_fw_view_view_controller_push(view_manager(), &sp, 0);
    dk_fw_string_shared_ptr_release(&sp.ctl);
    return ok ? 0 : -1;
}

int dk_ui_confirm(const char *title, const char *text, void (*done)(void *, int), void *ctx)
{
    if (g_open)
        return -1;
    uint8_t *blk = fw_new(0x8c);
    if (!blk)
        return -1;
    char t[81], x[81];
    cut(t, sizeof t, title);
    cut(x, sizeof x, text);
    uint8_t alloc = 0;
    uint32_t st, sx;
    dk_fw_string_std_string_ctor(&st, t, &alloc);
    dk_fw_string_std_string_ctor(&sx, x, &alloc);
    fn cb = make_fn((uint32_t)done, (uint32_t)ctx, inv_answer_bool);
    uint32_t serial = ++g_serial;
    init_block(blk, &fw_confirm_cb_vt);
    dk_fw_window_confirm_ctor(blk + 0x10, &st, &sx, (const std_function *)&cb, 0);
    dk_fw_string_std_string_dtor(&sx);
    dk_fw_string_std_string_dtor(&st);
    g_open = serial;
    if (show(blk, serial) != 0) {
        g_open = 0;
        return -1;
    }
    return 0;
}

int dk_ui_choice(const char *title, const char *const *items, int n, void (*done)(void *, int),
                 void *ctx)
{
    if (g_open || !items || n <= 0 || n > MAX_CHOICE)
        return -1;
    uint8_t *blk = fw_new(0xe8);
    uint32_t *strs = (uint32_t *)fw_new(4 * (uint32_t)n);
    if (!blk || !strs)
        return -1;
    uint8_t alloc = 0;
    char buf[DK_UI_ROW];
    for (int i = 0; i < n; i++) {
        cut(buf, sizeof buf, items[i]);
        dk_fw_string_std_string_ctor(&strs[i], buf, &alloc);
    }
    uint32_t vec[3] = {(uint32_t)strs, (uint32_t)(strs + n), (uint32_t)(strs + n)};
    uint32_t st;
    cut(buf, sizeof buf, title);
    dk_fw_string_std_string_ctor(&st, buf, &alloc);
    fn cb = make_fn((uint32_t)done, (uint32_t)ctx, inv_answer);
    uint32_t serial = ++g_serial;
    init_block(blk, &fw_choice_cb_vt);
    dk_fw_window_multichoice_ctor(blk + 0x10, vec, &st, (const std_function *)&cb, 1);
    dk_fw_string_std_string_dtor(&st);
    for (int i = 0; i < n; i++)
        dk_fw_string_std_string_dtor(&strs[i]);
    dk_fw_mem_free((uint8_t *)strs);
    g_open = serial;
    if (show(blk, serial) != 0) {
        g_open = 0;
        return -1;
    }
    return 0;
}

int dk_ui_toast(const char *text)
{
    char buf[DK_UI_TOAST + 1];
    cut(buf, sizeof buf, text);
    dk_fw_window_toast(dk_fw_window_ui_states(), "%s", buf);
    return 0;
}

/* ---- dk_ui_tick ------------------------------------------------------------------------------------------------------- */

/* The timers (tick.h). One function, ui_poll, is on the runtime's per-pass list (dkrt_poll_add) while any timer runs: the UI loop calls it
   in the UI task, the task of every UI callback. The runtime's block clock (1500 per second) times the periods; without one the passes
   of the UI loop (about 120 per second) do. */
static ui_ticks g_ticks;
static uint32_t g_passes;
static int g_polling;

static uint32_t tick_clock(uint32_t *per_s)
{
    uint32_t now = dkrt_blocks();
    if (now) {
        *per_s = 1500;
        return now;
    }
    *per_s = 120;
    return ++g_passes;
}

static void ui_poll(void)
{
    uint32_t per_s, now = tick_clock(&per_s);
    if (ui_ticks_run(&g_ticks, now, per_s) == 0 && g_polling) {
        dkrt_poll_remove((uintptr_t)ui_poll);
        g_polling = 0;
    }
}

int dk_ui_tick(dk_ui_tick_fn *fn, void *ctx, uint32_t period_ms)
{
    uint32_t per_s, now = tick_clock(&per_s);
    int r = ui_ticks_set(&g_ticks, fn, ctx, period_ms, now);
    if (r == 0 && !g_polling && ui_ticks_active(&g_ticks)) {
        if (dkrt_poll_add((uintptr_t)ui_poll) != 0) {
            ui_ticks_set(&g_ticks, fn, ctx, 0, now);
            return -1;
        }
        g_polling = 1;
    }
    return r;
}

/* ---- the service imports of <dk/gen/imports.h> (docs/plans/sdk.md section 5.1) ----
   The new names of the calls above, for the modules of the new entry. The old names stay until the modules move (S2b-2). */

static int ui_token; /* the dk_ui handle: only a token, there is one UI service per image */

int32_t dk_ui_service_open(dk_module_ctx *ctx, dk_ui **out)
{
    if (!ctx || !out)
        return DK_E_CONTRACT;
    *out = (dk_ui *)&ui_token;
    return 0;
}

int32_t dk_ui_menu_open(dk_ui_menu *m)
{
    return dk_ui_open(m);
}

void dk_ui_menu_close(dk_ui_menu *m)
{
    dk_ui_close(m);
}

void dk_ui_menu_select(dk_ui_menu *m, int32_t row)
{
    dk_ui_select(m, (int)row);
}

void dk_ui_menu_refresh(dk_ui_menu *m)
{
    dk_ui_refresh(m);
}

int32_t dk_ui_menu_is_open(const dk_ui_menu *m)
{
    return dk_ui_is_open(m);
}

void dk_ui_menu_add_text(dk_ui_menu *m, const char *text)
{
    dk_ui_text(m, text);
}

void dk_ui_menu_add_action(dk_ui_menu *m, const char *text, dk_ui_action_fn *run, void *arg,
                           int32_t value)
{
    dk_ui_action(m, text, (dk_ui_act_fn *)run, arg, (int)value);
}

void dk_ui_menu_add_submenu(dk_ui_menu *m, const char *text, dk_ui_menu *sub)
{
    dk_ui_submenu(m, text, sub);
}

void dk_ui_menu_add_toggle(dk_ui_menu *m, const char *text, int32_t *value,
                           dk_ui_action_fn *changed, void *arg)
{
    dk_ui_toggle(m, text, (int *)value, (dk_ui_act_fn *)changed, arg);
}

void dk_ui_menu_add_value(dk_ui_menu *m, const char *text, int32_t *value, int32_t min, int32_t max,
                          dk_ui_action_fn *changed, void *arg)
{
    dk_ui_value(m, text, (int *)value, (int)min, (int)max, (dk_ui_act_fn *)changed, arg);
}

void dk_ui_menu_check(dk_ui_menu *list, int32_t row, int32_t on)
{
    dk_ui_check(list, (int)row, (int)on);
}

int32_t dk_ui_menu_checked(const dk_ui_menu *list, int32_t row)
{
    return dk_ui_checked(list, (int)row);
}
