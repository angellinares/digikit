# core/ui

The device adapter behind `<dk/ui.h>` (`sdk/include/dk/ui.h`): menus, text pages, lists with a title bar, check marks and a side panel, rows
(action, submenu, text, toggle, value), confirm and choice dialogs and toasts. C, one source for the Digitone II and the Digitakt II; every firmware
call is a generated anchor (`modules/core/fw/include/dk_fw_*.h`). The host fake of the same API, with its tests, is in `host/`; it also compiles the menu of `core/digikit-menu` against a fake runtime (`host/tests/menu.rs`).

- The `dk_ui_*` functions are plain global symbols. A content module calls them as imports (`[[import]]` of `sdk/sdk.toml`, `requires = "core/ui"`): the runtime's loader links them by name, and hands out a stub that returns the failure value when this module is not in the image (docs/ARCHITECTURE.md). `host/tests/remote.rs` runs `digikit/remote`'s settings page on the host fake.
- A plain menu and a text page are the firmware's `VerticalMenuView`; a `DK_UI_LIST` is a `MenuWithPopoutSideBarsView` (the PRESET MANAGER's
  base) with the right panel as the side panel. Each is built once and kept for the session through a `shared_ptr` this module owns, so the
  firmware never frees it.
- Rows are stock `MenuItem`s. Their `std::function`s keep two words (the menu and the row number) in the function's own storage: no heap
  per row. The row data lives in an array of the menu that a refill rewrites.
- The View vtable of a text page and of a list is a copy in module RAM: paint (slot 4) hides the cursor bar of a text page and draws the
  empty text of a list; key (slot 2) of a list adds RIGHT (side panel), YES (check mark) and the `back` callback, and sends the rest to the
  stock handler.
- Dialogs are `make_shared` blocks built by hand and pushed on the Brain's view manager; the firmware frees them. One at a time.
- `dk_ui_stock_row(stock_menu, text, fn, ctx)` is for other core modules (`include/dk_ui_core.h`, not part of the SDK): a row among the rows of a stock menu (SETTINGS), with its icon. `core/digikit-menu` and `test/ui-demo` use it.
- The one firmware hook of the module is the end of the SETTINGS item list rebuild (`cf.menu.hook.settings_list.tail`, `src/shim.s` `dk_hook`). It calls the optional symbol `dk_app_settings_rows(settings_menu)` (`include/dk_ui_core.h`) that an app implements: patchkit resolves it to 0 when no app is in the image, and the hook then does nothing. `core/digikit-menu` and `test/ui-demo` implement it (they conflict: one DIGIKIT row). The layers are in [docs/ARCHITECTURE.md](../../../../docs/ARCHITECTURE.md).
- `dk_ui_tick(fn, ctx, period_ms)` is a table of four timers (`src/tick.h`, no firmware, tested in `host/tests/tick.rs`) behind one function on the runtime's `dkrt_poll_add` list; it is on the list only while a timer runs. The block clock times the periods (the UI-loop pass when there is none).
- An empty list paints the same inverted title bar as a list with rows, then its `empty` text. A text page opens at its first row; a menu or list keeps its selected row.

Proof and demo: `tests/fixtures/modules/ui-demo`, driven by `uv run python -m tools.patchkit.pk_uidemo shots|leak --device dt2|dn2`.
Details: [docs/findings/71-ui-components-1-17.md](../../../../docs/findings/71-ui-components-1-17.md).
