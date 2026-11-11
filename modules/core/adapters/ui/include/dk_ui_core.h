/* dk_ui_core.h: calls of core/ui for other core modules only. Not part of the module SDK (sdk/include/dk/platform/ui.h). */
#ifndef DK_UI_CORE_H
#define DK_UI_CORE_H
#include <stdint.h>
#include <dk/platform/ui.h>

/* A row among the rows of a stock firmware menu (SETTINGS), with the menu's icon. fn(ctx, 0) runs on YES.
   Call it from a hook that rebuilds the stock menu; stock_menu is the menu object the hook gets. */
void dk_ui_stock_row(uint8_t *stock_menu, const char *text, dk_ui_act_fn *fn, void *ctx);

/* Implemented by an app (core/digikit-menu) and called by core/ui from its hook at the end of the SETTINGS item list rebuild: add the
   app's rows with dk_ui_stock_row. An optional link: core/ui resolves it at patch time and does nothing when no module provides it. */
void dk_app_settings_rows(uint8_t *stock_menu);

#endif
