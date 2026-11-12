/* <dk/platform/ui.h>: menus, lists and dialogs on the device screen. Hand-written.
   The calls are device-agnostic. core/ui implements them for each device with the firmware's own views.

   The new entry (<dk/core/module.h>, include it first; the prototypes are in <dk/gen/imports.h>): `dk_ui_service_open(ctx, &ui)` in `init` gives the UI handle,
   or DK_E_MISSING when core/ui is not in the image; the module then loads and decides whether it can run without menus. The menu calls have new names
   and the same rules as the old ones: dk_ui_menu_open/close/select/refresh/is_open and the rows dk_ui_item_text/action/submenu/toggle/value/check/checked
   (an action or change callback is a `dk_ui_action_fn`: `void (void *arg, int32_t value)`; `dk_ui_item_action(menu, text, run, arg, value)` gives it the pointer `arg`
   and the `value` the row was added with). dk_ui_menu_new, dk_ui_toast, dk_ui_confirm, dk_ui_choice and dk_ui_tick
   keep their names and are declared below. The module's settings menu is the `menu` field of `dk_module`: `void menu(dk_module_ctx *ctx, dk_ui_menu *menu)`.
   Transitional: `dk_ui_service_open` is the accessor that becomes `dk_ui_open` (docs/plans/sdk.md section 5.1); the name `dk_ui_open` is still the old call that opens a
   menu (below), and S2b-2 renames `dk_ui_service_open` to `dk_ui_open`. The text below is the old entry and the calls both entries share.

   A menu is built once and kept. Its fill function adds the rows each time the menu opens or is
   refreshed:

     static void fill(dk_ui_menu *m, void *ctx) {
         dk_ui_action(m, "APPLY", apply, ctx, 0);
         dk_ui_toggle(m, "LOUD", &loud, loud_changed, ctx);
     }
     static const dk_ui_menu_desc main_desc = {.name = "MyMenu", .title = "MY MODULE", .fill = fill};
     dk_ui_open(dk_ui_menu_new(&main_desc));

   Lifetime rules:
   - Text is copied when a row is added or a dialog opens. The caller may reuse its buffers.
   - ctx is kept and passed to the callbacks later, after the opening function has returned.
     It must point at static or module memory, never at the stack.
   - Callbacks run on the UI task. A dialog's callback runs after the dialog has closed.
   - Menus are never freed. Dialogs free themselves after they close.
   - A content module (a .dkm) has no unload at run time, so no UI object of the module is freed. Every
     ctx, callback and descriptor a module passes must still point at its own memory (static data or
     code of the module), never at the stack or at a heap block that is freed later. Build a menu when
     the user opens it (in the entry point below, or from a callback), after all modules are loaded.

   A module with settings exports dkm_menu with DK_MENU(fill). The DIGIKIT menu shows SETTINGS... for
   the loaded module and calls dkm_menu only when the user opens it; fill adds the rows of that menu:

     static void settings(dk_ui_menu *m, void *ctx) { dk_ui_action(m, "RESET", reset, 0, 0); }
     DK_MENU(settings)

   The calls are imports of the module (sdk/sdk.toml, requires core/ui). When core/ui is not in the
   image the module still loads and each call returns its failure value: dk_ui_menu_new 0, the
   int calls -1, dk_ui_has, dk_ui_checked and dk_ui_is_open 0, the void calls nothing.

   Keys: YES runs a row's action, flips a toggle, or (in a list with a check function) toggles the
   row's check mark. The encoder changes a value row; changed is called only when the value moves.
   In a list, RIGHT opens the side panel; choosing a panel row closes the panel, then runs the row.
   LEFT or NO closes the panel. NO on a menu goes back.

   Before a call that a device may not have, check dk_ui_has(cap). */
#ifndef DK_UI_H
#define DK_UI_H
#include <stdint.h>

/* Bytes of row text, terminator included. Longer text is cut. */
#define DK_UI_ROW 24
/* Characters a toast shows. */
#define DK_UI_TOAST 32

typedef struct dk_ui_menu dk_ui_menu;

/* Adds the rows of a menu or of a list's side panel. */
typedef void dk_ui_fill_fn(dk_ui_menu *m, void *ctx);
/* A row action or a change: arg is the value the row was added with (an action) or the new value
   (a toggle or a value). */
typedef void dk_ui_act_fn(void *ctx, int arg);

/* Menu flags. */
enum {
    DK_UI_TEXT_PAGE = 1, /* rows are text to read: no highlighted row, the keys scroll */
    DK_UI_LIST = 2,      /* a dense list with a title bar, a check column and a side panel */
    DK_UI_WIDE =
        4, /* a menu page with a full-width title bar and full-width rows (a list without check marks or side panel) */
};

typedef struct dk_ui_menu_desc {
    const char *name;  /* view name, for logs */
    const char *title; /* shown in the title */
    uint32_t flags;    /* DK_UI_* */
    dk_ui_fill_fn *fill;
    void *ctx;
    /* DK_UI_LIST only. Each may be 0. */
    const char *empty; /* text shown when the list has no rows */
    void (*panel)(dk_ui_menu *panel, int row,
                  void *ctx);                  /* RIGHT on row: add the side-panel rows */
    void (*check)(void *ctx, int row, int on); /* YES on row toggled its check mark */
    int (*back)(void *ctx);                    /* NO on the list: return 1 to stay open */
} dk_ui_menu_desc;

/* Capabilities for dk_ui_has. */
enum {
    DK_UI_CAP_MENU = 1,
    DK_UI_CAP_TEXT_PAGE,
    DK_UI_CAP_LIST,
    DK_UI_CAP_TOGGLE,
    DK_UI_CAP_VALUE,
    DK_UI_CAP_CONFIRM,
    DK_UI_CAP_CHOICE,
    DK_UI_CAP_TOAST,
    DK_UI_CAP_TICK,
};

/* 1 when this device has the capability. A missing one makes its calls do nothing and return -1. */
int dk_ui_has(int cap);

/* Menus and lists. */
dk_ui_menu *dk_ui_menu_new(const dk_ui_menu_desc *d); /* 0 when out of memory; d must stay valid */
int dk_ui_open(
    dk_ui_menu *
        m); /* fill, then show on top of the current view; if open, refresh. A menu or list keeps its selected row
                                  from the last time; a text page opens at its first row. */
void dk_ui_close(dk_ui_menu *m);
/* Puts the cursor of a list or menu on row `row` (counted as the rows are added; clamped to the rows). Call it before dk_ui_open to start a
   list on its current choice: dk_ui_open keeps the cursor row. On an open list the cursor moves at once. */
void dk_ui_select(dk_ui_menu *m, int row);
int dk_ui_is_open(const dk_ui_menu *m);
void dk_ui_refresh(dk_ui_menu *m); /* fill again; the selection stays on the same row when it can */

/* Rows, added from a fill function. In a list, the row number (from 0, in the order added) is the
   row argument of panel and check. */
void dk_ui_action(dk_ui_menu *m, const char *text, dk_ui_act_fn *fn, void *ctx, int arg);
void dk_ui_submenu(dk_ui_menu *m, const char *text, dk_ui_menu *sub);
void dk_ui_text(dk_ui_menu *m, const char *text); /* no action */
/* ON/OFF. *value is read when drawn; changed gets the new value after *value is written. */
void dk_ui_toggle(dk_ui_menu *m, const char *text, int *value, dk_ui_act_fn *changed, void *ctx);
/* A number from min to max, changed by the encoder. */
void dk_ui_value(dk_ui_menu *m, const char *text, int *value, int min, int max,
                 dk_ui_act_fn *changed, void *ctx);

/* The check marks of a DK_UI_LIST. */
void dk_ui_check(dk_ui_menu *list, int row, int on);
int dk_ui_checked(const dk_ui_menu *list, int row);

/* Dialogs. Each returns 0 when shown, -1 when not (missing capability, out of memory, or another
   dialog is open). */
/* YES or NO. done gets 1 for YES, 0 for NO. title and text: up to 80 characters each. */
int dk_ui_confirm(const char *title, const char *text, void (*done)(void *ctx, int yes), void *ctx);
/* One of n items. done gets the index; NO closes the dialog without calling done. */
int dk_ui_choice(const char *title, const char *const *items, int n,
                 void (*done)(void *ctx, int index), void *ctx);
/* A short message on the main screen that goes away by itself. */
int dk_ui_toast(const char *text);

/* A timer in the UI task (DK_UI_CAP_TICK). fn(ctx) runs about every period_ms, in the same task as the fill and action callbacks, so it
   may call dk_ui_refresh and read module state without a lock. Use it to redraw a screen that shows a change nobody triggers with a key
   (a load that is still running). Rules:
   - The accuracy is the UI loop (about 8 ms) at best; a callback runs late when the task is busy. Do not count calls: read the state.
   - Keep fn short. It runs between key events and delays them. It must not wait.
   - fn and ctx stay in use: static module code and memory, never the stack.
   - A timer is identified by (fn, ctx). Calling again with the same pair changes its period (the next call is one period away).
     period_ms 0 stops it, also from inside fn; stopping a timer that does not run is not an error. Stop the timer when its screen
     closes: a running timer costs a call per period.
   - Room for 4 timers in the image, shared by all modules. A longer period than 1,000,000 ms is cut.
   Returns 0 when set or stopped, -1 when it could not be (no core/ui or no DK_UI_CAP_TICK, fn 0, or the table is full). */
typedef void dk_ui_tick_fn(void *ctx);
int dk_ui_tick(dk_ui_tick_fn *fn, void *ctx, uint32_t period_ms);

/* The module settings entry point: defines dkm_menu (docs/design/digikit-dkm.md) with fill as its body. The DIGIKIT
   menu passes the menu to fill and 0 as ctx. */
#define DK_MENU(fill)                                                                              \
    void dkm_menu(dk_ui_menu *m, void *ctx)                                                        \
    {                                                                                              \
        fill(m, ctx);                                                                              \
    }

#endif
