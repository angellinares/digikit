/* test/ui-demo: every kind of row and view of <dk/platform/ui.h>, to see core/ui on the screen. */
#include <stdint.h>
#include <dk/platform/ui.h>
#include <dk_ui_core.h>

static int g_toggle = 1;
int dk_demo_value = 64;
int dk_demo_checks[3];
int dk_demo_events_n;
int dk_demo_answer_v = -1;
static dk_ui_menu *g_main, *g_page, *g_list, *g_empty;

static void on_answer(void *ctx, int v)
{
    (void)ctx;
    dk_demo_answer_v = v;
    dk_demo_events_n++;
}

static void do_toast(void *ctx, int arg)
{
    (void)ctx;
    (void)arg;
    dk_demo_events_n++;
    dk_ui_toast("HELLO FROM CORE UI");
}

static void do_confirm(void *ctx, int arg)
{
    (void)ctx;
    (void)arg;
    dk_ui_confirm("DELETE EVERYTHING?", "REALLY? Y/N", on_answer, 0);
}

static void do_choice(void *ctx, int arg)
{
    static const char *const items[] = {"SEND", "MASTER", "OFF"};
    (void)ctx;
    (void)arg;
    dk_ui_choice("ROUTE", items, 3, on_answer, 0);
}

static void changed(void *ctx, int v)
{
    (void)ctx;
    (void)v;
    dk_demo_events_n++;
}

static void fill_page(dk_ui_menu *m, void *ctx)
{
    (void)ctx;
    dk_ui_text(m, "A TEXT PAGE HAS NO");
    dk_ui_text(m, "HIGHLIGHTED ROW.");
    dk_ui_text(m, "THE KEYS SCROLL IT.");
    dk_ui_text(m, "LINE FOUR");
    dk_ui_text(m, "LINE FIVE");
    dk_ui_text(m, "LINE SIX");
    dk_ui_text(m, "LINE SEVEN");
    dk_ui_text(m, "LINE EIGHT");
}

static void fill_list(dk_ui_menu *m, void *ctx)
{
    static const char *const names[] = {"FIRST ITEM", "SECOND ITEM", "THIRD ITEM"};
    (void)ctx;
    for (int i = 0; i < 3; i++)
        dk_ui_text(m, names[i]);
}

static void fill_none(dk_ui_menu *m, void *ctx)
{
    (void)m;
    (void)ctx;
}

static void panel_act(void *ctx, int arg)
{
    (void)ctx;
    dk_demo_events_n++;
    dk_ui_toast(arg ? "PANEL: SECOND" : "PANEL: FIRST");
}

static void list_panel(dk_ui_menu *p, int row, void *ctx)
{
    (void)row;
    (void)ctx;
    dk_ui_action(p, "LOAD", panel_act, 0, 0);
    dk_ui_action(p, "INFO", panel_act, 0, 1);
}

static void list_check(void *ctx, int row, int on)
{
    (void)ctx;
    if (row >= 0 && row < 3)
        dk_demo_checks[row] = on;
    dk_demo_events_n++;
}

/* NO is refused while the third item is checked (the list stays open). */
static int list_back(void *ctx)
{
    (void)ctx;
    dk_demo_events_n++;
    return dk_demo_checks[2];
}

static const dk_ui_menu_desc page_desc = {.name = "UiDemoPage", .title = "TEXT PAGE", .flags = DK_UI_TEXT_PAGE, .fill = fill_page};
static const dk_ui_menu_desc list_desc = {.name = "UiDemoList", .title = "DEMO LIST", .flags = DK_UI_LIST, .fill = fill_list,
                                          .empty = "NO ROWS", .panel = list_panel, .check = list_check, .back = list_back};
static const dk_ui_menu_desc empty_desc = {.name = "UiDemoEmpty", .title = "EMPTY LIST", .flags = DK_UI_LIST, .fill = fill_none,
                                           .empty = "NOTHING HERE", .panel = list_panel, .check = list_check};

static void fill_main(dk_ui_menu *m, void *ctx)
{
    (void)ctx;
    dk_ui_toggle(m, "TOGGLE", &g_toggle, changed, 0);
    dk_ui_value(m, "VALUE", &dk_demo_value, 0, 127, changed, 0);
    dk_ui_action(m, "TOAST", do_toast, 0, 0);
    dk_ui_action(m, "CONFIRM", do_confirm, 0, 0);
    dk_ui_action(m, "CHOICE", do_choice, 0, 0);
    dk_ui_submenu(m, "TEXT PAGE", g_page);
    dk_ui_submenu(m, "LIST", g_list);
    dk_ui_submenu(m, "EMPTY LIST", g_empty);
    dk_ui_text(m, "A TEXT ROW");
}

static const dk_ui_menu_desc main_desc = {.name = "UiDemoMenu", .title = "UI DEMO", .fill = fill_main};

static void open_main(void *ctx, int arg)
{
    (void)ctx;
    (void)arg;
    if (!g_main) {
        g_page = dk_ui_menu_new(&page_desc);
        g_list = dk_ui_menu_new(&list_desc);
        g_empty = dk_ui_menu_new(&empty_desc);
        g_main = dk_ui_menu_new(&main_desc);
    }
    if (g_main)
        dk_ui_open(g_main);
}

/* Called from the hook at the end of the SETTINGS rebuild (core/ui). */
void dk_app_settings_rows(uint8_t *settings_menu)
{
    dk_ui_stock_row(settings_menu, "DIGIKIT", open_main, 0);
}

/* Read by the proof (tools/patchkit/pk_uidemo.py): dk_demo_value, dk_demo_checks, dk_demo_events_n, dk_demo_answer_v. */
