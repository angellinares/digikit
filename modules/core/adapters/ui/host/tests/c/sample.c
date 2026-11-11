/* A sample module on <dk/platform/ui.h> that uses every call. The Rust tests drive it. */
#include <dk/platform/ui.h>
#include <stdio.h>

static dk_ui_menu *main_menu, *tools_menu, *info_page, *files_list, *empty_list;

static int applied, last_arg, remote, level, level_changes, remote_changes;
static int check_row = -1, check_on = -1, panel_row = -1, panel_action = -1;
static int changed, saved, discarded, confirm_answer = -1, picked = -1;
static int rc_confirm, rc_choice, rc_toast, rc_open_list;
static int files_back_calls;

/* Getters for the tests. */
int s_applied(void) { return applied; }
int s_last_arg(void) { return last_arg; }
int s_remote(void) { return remote; }
int s_level(void) { return level; }
int s_level_changes(void) { return level_changes; }
int s_remote_changes(void) { return remote_changes; }
int s_check_row(void) { return check_row; }
int s_check_on(void) { return check_on; }
int s_panel_row(void) { return panel_row; }
int s_panel_action(void) { return panel_action; }
int s_saved(void) { return saved; }
int s_discarded(void) { return discarded; }
int s_confirm_answer(void) { return confirm_answer; }
int s_picked(void) { return picked; }
int s_rc_confirm(void) { return rc_confirm; }
int s_rc_choice(void) { return rc_choice; }
int s_rc_toast(void) { return rc_toast; }
int s_rc_open_list(void) { return rc_open_list; }
int s_back_calls(void) { return files_back_calls; }
int s_is_open_files(void) { return dk_ui_is_open(files_list); }
int s_files_checked(int row) { return dk_ui_checked(files_list, row); }
dk_ui_menu *s_main(void) { return main_menu; }
dk_ui_menu *s_files(void) { return files_list; }
dk_ui_menu *s_empty(void) { return empty_list; }
dk_ui_menu *s_info(void) { return info_page; }
void s_set_remote(int v) { remote = v; }
void s_refresh_main(void) { dk_ui_refresh(main_menu); }

static void apply(void *ctx, int arg) {
    (void)ctx;
    applied++;
    last_arg = arg;
    dk_ui_refresh(main_menu);
}
static void remote_cb(void *ctx, int v) {
    (void)ctx;
    remote_changes++;
    remote = v;
}
static void level_cb(void *ctx, int v) {
    (void)ctx;
    level_changes++;
    level = v;
}

static void confirm_done(void *ctx, int yes) {
    (void)ctx;
    confirm_answer = yes;
}
static void choice_done(void *ctx, int index) {
    (void)ctx;
    picked = index;
}
static void ask(void *ctx, int arg) {
    (void)ctx;
    (void)arg;
    rc_confirm = dk_ui_confirm("ERASE", "Erase everything?", confirm_done, 0);
}
static void pick(void *ctx, int arg) {
    static const char *const items[] = {"ONE", "TWO", "THREE"};
    (void)ctx;
    (void)arg;
    rc_choice = dk_ui_choice("PICK ONE", items, 3, choice_done, 0);
}
static void note(void *ctx, int arg) {
    (void)ctx;
    (void)arg;
    rc_toast = dk_ui_toast("SAVED 123456789012345678901234567890");
}
static void open_files(void *ctx, int arg) {
    (void)ctx;
    (void)arg;
    rc_open_list = dk_ui_open(files_list);
}

static void main_fill(dk_ui_menu *m, void *ctx) {
    char buf[DK_UI_ROW];
    (void)ctx;
    snprintf(buf, sizeof buf, "APPLIED %d", applied); /* the menu copies the text */
    dk_ui_action(m, "APPLY", apply, 0, 42);
    dk_ui_submenu(m, "TOOLS", tools_menu);
    if (dk_ui_has(DK_UI_CAP_TOGGLE))
        dk_ui_toggle(m, "REMOTE", &remote, remote_cb, 0);
    else
        dk_ui_text(m, "REMOTE N/A");
    if (dk_ui_has(DK_UI_CAP_VALUE))
        dk_ui_value(m, "LEVEL", &level, 0, 127, level_cb, 0);
    dk_ui_text(m, buf);
}

static void tools_fill(dk_ui_menu *m, void *ctx) {
    (void)ctx;
    dk_ui_submenu(m, "INFO", info_page);
    dk_ui_action(m, "FILES", open_files, 0, 0);
    dk_ui_action(m, "EMPTY", (dk_ui_act_fn *)0, 0, 0);
    dk_ui_action(m, "ERASE", ask, 0, 0);
    dk_ui_action(m, "PICK", pick, 0, 0);
    dk_ui_action(m, "NOTE", note, 0, 0);
}

static void info_fill(dk_ui_menu *m, void *ctx) {
    (void)ctx;
    dk_ui_text(m, "DIGIKIT MENU");
    dk_ui_text(m, "THIS LINE IS MUCH TOO LONG FOR A ROW");
}

static void files_fill(dk_ui_menu *m, void *ctx) {
    (void)ctx;
    dk_ui_text(m, "ALPHA");
    dk_ui_text(m, "BETA");
    dk_ui_text(m, "GAMMA");
}
static void file_check(void *ctx, int row, int on) {
    (void)ctx;
    check_row = row;
    check_on = on;
    changed = 1;
}
static void panel_act(void *ctx, int arg) {
    (void)ctx;
    panel_action = arg & 1; /* 0 RENAME, 1 DELETE */
    panel_row = arg >> 1;
}
static void files_panel(dk_ui_menu *p, int row, void *ctx) {
    (void)ctx;
    dk_ui_action(p, "RENAME", panel_act, 0, row << 1);
    dk_ui_action(p, "DELETE", panel_act, 0, (row << 1) | 1);
}
static void save_done(void *ctx, int yes) {
    (void)ctx;
    confirm_answer = yes;
    if (yes) saved++; else discarded++;
    changed = 0;
    dk_ui_close(files_list);
}
static int files_back(void *ctx) {
    (void)ctx;
    files_back_calls++;
    if (!changed) return 0;
    dk_ui_confirm("SAVE?", "Keep the changes?", save_done, 0);
    return 1; /* stay until answered */
}

static void empty_fill(dk_ui_menu *m, void *ctx) {
    (void)m;
    (void)ctx;
}

static const dk_ui_menu_desc main_desc = {.name = "Main", .title = "SAMPLE", .fill = main_fill};
static const dk_ui_menu_desc tools_desc = {.name = "Tools", .title = "TOOLS", .fill = tools_fill};
static const dk_ui_menu_desc info_desc = {.name = "Info", .title = "INFO", .flags = DK_UI_TEXT_PAGE, .fill = info_fill};
static const dk_ui_menu_desc files_desc = {.name = "Files", .title = "FILES", .flags = DK_UI_LIST, .fill = files_fill,
                                           .empty = "NO FILES", .panel = files_panel, .check = file_check,
                                           .back = files_back};
static const dk_ui_menu_desc empty_desc = {.name = "Empty", .title = "NONE", .flags = DK_UI_LIST, .fill = empty_fill,
                                           .empty = "NOTHING HERE"};

/* Resets the state and builds all menus. Returns the main menu (0 when menus are missing). */
dk_ui_menu *s_init(void) {
    applied = last_arg = remote = level = level_changes = remote_changes = 0;
    check_row = check_on = panel_row = panel_action = confirm_answer = picked = -1;
    changed = saved = discarded = files_back_calls = 0;
    rc_confirm = rc_choice = rc_toast = rc_open_list = 1;
    tools_menu = dk_ui_menu_new(&tools_desc);
    info_page = dk_ui_menu_new(&info_desc);
    files_list = dk_ui_menu_new(&files_desc);
    empty_list = dk_ui_menu_new(&empty_desc);
    main_menu = dk_ui_menu_new(&main_desc);
    return main_menu;
}
