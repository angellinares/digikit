/* <dk/chooser.h>: a list to choose from, with an optional "+ ADD" row that opens a file browser (docs/design/ui-rules.md, "The chooser
   component"). Shared source, not a runtime call: the module lists sdk/src/chooser.c and sdk/src/picker_logic.c in its [[object]] sources.
   It sits on <dk/ui.h> (core/ui lists, the firmware font) and the read-only listing of <dk/files.h>.

   The rules it keeps (ui-rules.md):
     - the list opens with the current entry marked and the cursor on it (the row callback draws the mark, dk_ui_select puts the cursor);
     - YES on an entry or on a file finishes the flow: every level closes in one step, then done(ctx, result) runs, on the page the flow
       started from;
     - NO goes back one level and changes nothing (browser: up one folder, out of the browser at the root, then out of the list);
     - RIGHT opens the side panel (panel callback); LEFT or NO closes it;
     - an error never opens a dialog: add_file's error closes the flow and shows its text as a toast on the page; done is not called;
     - while busy() says rows change by themselves, the list refreshes on dk_ui_tick.

   One chooser exists at a time (statics); a second open reuses it. Its menus are built on the first open, so the title of the first open stays.

     dkm_init:   dk_chooser_init(api);
     click:      dk_chooser_open(&desc, current);     desc is static (kept)
     dkm_fini:   dk_chooser_fini();

   Everything runs in the UI task. The chooser does nothing when core/ui has no lists, when init failed, or when init was not called. */
#ifndef DK_CHOOSER_H
#define DK_CHOOSER_H
#include <dk/api.h>
#include <dk/ui.h>

#define DK_CHOOSER_HOLE 1  /* row flag: shown, but YES does not choose it (an empty entry) */
#define DK_CHOOSER_E_PATH (-40) /* the browser's path does not fit 255 bytes: add_file is not called, the flow fails with this code */

typedef struct dk_chooser_desc {
    const char *title; /* list title */
    int count;         /* entries 0 .. count-1 */
    /* Writes the text of entry i (at most DK_UI_ROW - 1 characters) and returns DK_CHOOSER_* flags. is_current: the entry is the current one;
       mark it in the text. */
    int (*row)(void *ctx, int i, int is_current, char *text);
    int (*busy)(void *ctx); /* optional: 1 while any row changes without a key (a load); the list then refreshes every 250 ms */
    const char *add;        /* optional first row, for example "+ ADD": YES opens the browser */
    uint32_t root;          /* DK_FILES_* root of the browser */
    /* YES on a file (root-relative path, at most 255 bytes). Returns the result for done (>= 0), or a negative error. Required with add. */
    int (*add_file)(void *ctx, const char *path);
    /* The text of an error (at most 31 characters) for the toast; optional (the default is "ERROR -n"). */
    void (*error_text)(void *ctx, int code, char *text);
    /* RIGHT on entry i: add side-panel rows with dk_ui_action. Optional. */
    void (*panel)(void *ctx, dk_ui_menu *panel, int entry);
    /* The flow finished: the entry index, or what add_file returned. Every level is closed. Optional. */
    void (*done)(void *ctx, int result);
    void *ctx;
} dk_chooser_desc;

/* Acquires the listing client; call in dkm_init. The chooser keeps api. 0 when it can run, -1 when not (open then does nothing). */
int dk_chooser_init(const dk_api *api);
/* Opens the list. current is the entry to mark (-1: none); the cursor starts on it. 0 when opened. Call from the UI task. */
int dk_chooser_open(const dk_chooser_desc *d, int current);
/* Fills the open list again (a panel action changed a row). */
void dk_chooser_refresh(void);
/* Text rows of at most 16 characters, broken at a space or after a '/': for a module's own text page. */
void dk_chooser_wrap(dk_ui_menu *m, const char *s);
/* Stops the timer and the listing; call in dkm_fini. */
void dk_chooser_fini(void);
#endif
