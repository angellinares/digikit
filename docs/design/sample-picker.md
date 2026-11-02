# On-device sample picker: the SMP pool list

Status: slice 2 accepted by Em on 2026-11-01; reshaped on 2026-11-02 for the sample pool (the machine owns 16 entries, the parameter SMP names the entry of a note, a click on SMP opens this list) and for the UI rules of 2026-11-02 (docs/design/ui-rules.md): the list is the chooser component filled with the pool, YES sets SMP and closes the whole flow, errors show on the page. Built statically, not run in the emulator yet.
Sources: sdk/include/dk/{ui,files,sample_track,sample_picker,chooser,params}.h, docs/design/sample-api-1.5.md, docs/MODULE-AUTHORING.md, modules/core/runtime/src/rs/src/tracks.rs, finding 71.

## 0. Facts that shape the design

- OLED: 128 x 64 px, 1 bit. A DK_UI_LIST has an inverted title bar at the top (about 8 px) and 7 px rows, so about 7 rows and about 21 characters per row in the firmware font. Row text is cut at 23 characters (DK_UI_ROW 24). A toast is hidden while a menu is open.
- Keys in a list: UP/DOWN move, YES runs the row, RIGHT opens the side panel, LEFT/NO closes it, NO on the list calls `desc.back` (return 1 = stay open) or closes the list.
- `sample_pool_add` copies the path and returns the pool index at once (or an error); the load runs in the runtime UI task. Errors that happen later are sticky in `sample_pool_status` until the entry is added again or cleared.
- DT2 has a sample backing (`core/sample-pool`); DN2 has it in the `sample-tracks` patch set only. Without a backing `sample_pool_add` returns E_CONTRACT.
- The picker is shared source compiled into the machine (`sdk/src/sample_picker.c` on the chooser `sdk/src/chooser.c`), so it gets the machine's pool handle in `dk_picker_init`.
- A content module has no UI timer of its own; `dk_ui_tick` (core/ui) refreshes the list while a load runs.

## 1. Entry: the click on SMP

SMP is word 0 of SHOT (knob B on both devices). Turning it steps through the pool (0 to 15, p-lockable). Releasing the knob without a turn opens this list over the machine page. The page hook (core/adapters/ui, the `DK_PARAM_ACTION` record) calls the machine's action function in the UI task. Contract (docs/design/machine-page-click.md); SHOT declares and implements:

```c
DK_PARAM_ACTION(shot, 0, shot_smp_click)
void shot_smp_click(uint32_t track);
```

`track` is the track of the page. The action reads the SMP value with `param_get(track, 0)` (raw, unit * 256: the entry is `raw >> 8`) and opens the list with `dk_picker_open_pool(entry, smp_pick, 0)`: that entry is marked and the cursor starts on it. YES on an entry, or on a file added with `+ ADD`, calls `smp_pick(ctx, index)` after every list has closed; it writes `param_set(track, 0, index << 8)`, the stock setter of a knob turn. `param_get` and `param_set` work from the click callback until the page gets its next key or turn (the flow of the click; a held TRIG or FUNC refuses the write). The picker cannot write a parameter itself: the machine does it in `smp_pick`. The module-settings entry of SHOT is gone.

## 2. Screens and keys

Common: font and views are the firmware's (core/ui). One button: YES on an entry or on a file finishes the flow and returns to the page. No confirm.

### 2.1 Screen 1: the pool list (DK_UI_LIST, title `SAMPLES`)

```
+----------------------+
|       SAMPLES        |   title bar (inverted)
| + ADD                |   row 0: the browser
| 00 KICK808    S 1.2s |   rows 1 to 16: entries 0 to 15, as the knob numbers them
| 01>SNARE_A    M 0.4s |   '>' marks the entry the SMP value points at
| 02 LOADING...        |
| 03 -                 |   an empty entry (a hole)
| 04 !POOL FULL        |   sticky error of that entry
+----------------------+   (17 rows, the list scrolls)
```

Entry row: `NN NAME` (name cut with `~` like core/digikit-menu) and a tail: `S 1.2s` / `M 0.4s` (READY), `LOADING...` / `QUEUED`, `!` plus the error text (2.4), or `-` for an empty entry. Rows are always the 16 indexes, so a row number is the SMP value; indexes are stable and holes stay.
Keys: YES on + ADD opens the browser. YES on an entry calls `pick(index)` and closes the list (back to the page). The cursor starts on the marked (current) entry. RIGHT on an entry: panel `CLEAR` (`sample_pool_clear`, one press, no confirm; its notes stop, the row goes to `-`) and `INFO` (screen 4). RIGHT on + ADD: no panel. NO: back to the machine page.
Refresh: `dk_ui_refresh` after any action and, while an entry is QUEUED/LOADING, on the UI tick.
Names: after a project change the runtime reloads the list; the row asks `sample_pool_path` for the name. Without the API (a 1.5 runtime) the name is blank.

### 2.2 Screen 2: folder browser (DK_UI_LIST, title `SAMPLES`), opened by YES on + ADD

```
+----------------------+
|       SAMPLES        |
| ..  /DRUMS/KICKS     |   row 0: parent folder, shows the current path
| > 808                |   folder (">" prefix)
| > ACOUSTIC           |
|   KICK_01       92K  |   file, size right aligned
|   KICK_02       88K  |
|   KICK_LONG_N~_FINAL |   long name: start ~ end
|   BAD NAME TOO LONG  |   disabled: path over the provider limit
+----------------------+
```

Source: `files_list(client, DK_FILES_SAMPLES, dir, notify, ctx)` (DT2 +Drive, DN2 store `samples/`). `files_client()` is acquired once in the picker's init call. The picker owns one request at a time; a new folder cancels the old ticket (`files_cancel`) and requests the new one.
Reading: `notify` runs in the UI task and only polls. It drains `files_poll` until it returns 0 (pending) or 2 (end), copies each row into the picker's table, then calls `dk_ui_refresh`. Fill and action callbacks never read storage. While the ticket is pending, fill adds one text row `READING...`; at end with no rows, `desc.empty` = `NO SAMPLES HERE`. A negative poll result shows one text row `LIST ERROR` plus the code and `NO` still works.
Table: up to 128 rows, each: kind, 4-byte size (if SIZE_VALID), and the leaf name (47 bytes). The full path is the current directory string plus the leaf, built when needed (limit 254 DT2, 151 DN2 sample paths; a longer one is a disabled row). Row identity is the full path, never a row index or the truncated display name (plan rule). More than 128 rows: the first 128 by sort order and a last text row `+N MORE`; the workbench can arrange folders. **[O]** ask Em if 128 is enough.
Sort: at end of listing, sort rows folders first, then names, case-insensitive, by an index array (insertion sort, 128 rows). The provider order is not promised, so the picker sorts. A change of the folder while the user is on row k keeps no selection.
Paging: core/ui scrolls the list. "Paging" is only the 128-row window above; no network of continuation pages is needed.
Keys: UP/DOWN move. YES on a folder = enter it (relist). YES on `..` = parent. YES on a file = add it to the pool and finish the flow (2.3). NO = parent folder; at the root NO closes the browser (`back` returns 1 when it went up, 0 at the root). RIGHT on a file = side panel: `ADD` (same as YES), `INFO` (screen 4 with the listing facts). LEFT/NO closes the panel.

Preview of size/channels/length: the listing gives size and hash only. A module cannot read the header (`store_read` reads the runtime store, not +Drive, and a sample is not in it). So the row shows size (K/M). Channels and length appear after the load (READY, `sample_pool_info`) in screen 1 and INFO. A header preflight in the picker needs a new read call and is not worth it: the runtime already checks the header in the add's preflight and reports FORMAT. **Recommendation: no preview in v1.**

### 2.3 Add and load progress

YES on file F: the chooser builds the path and the picker calls `sample_pool_add(handle, path)`.
- Returns an index: the browser and the list close in one step, then `pick(ctx, index)` runs and the machine sets SMP to it (SHOT: `param_set`). If the file was already in the pool the index is its entry and nothing loads. The next list shows `QUEUED`, `LOADING...`, then `S 1.2s`. The entry is never reported READY by the picker itself; only `sample_pool_status == 3` sets the row.
- Returns an error: the browser and the list close and the page shows the error text (the INFO text of the table below, at most 31 characters) as a toast; no pool entry changed and `pick` is not called. There is no dialog.
A path over 255 bytes is refused by the chooser itself (`PATH TOO LONG OR BAD`), without a call.

### 2.4 Errors

Async errors (row tail on screen 1, sticky until the entry is added again or cleared), INFO text (screen 4) and the toast text of a refused add:

| Code | Row tail (max 14) | INFO text |
| --- | --- | --- |
| E_NOT_FOUND -1 | `!MISSING` | FILE NOT FOUND |
| E_READ -3 | `!READ ERROR` | CARD READ FAILED |
| E_FORMAT -5 | `!BAD FORMAT` | NEEDS PCM16 48K MONO OR STEREO |
| E_TOO_BIG -13 | `!TOO BIG` | DOES NOT FIT THE POOL |
| E_LOST -15 | `!LOST` | BACKING LOST, RELOAD FAILED |
| E_POOL -16 | `!POOL FULL` | POOL FULL: CLEAR AN ENTRY |
| E_CHANGED -17 | `!CHANGED` | FILE CHANGED SINCE SELECTION |
| E_SEL_PATH -40 | `!BAD PATH` | PATH TOO LONG OR BAD |
| E_CONTRACT -48 | `NO SAMPLE POOL` | NO SAMPLE POOL ON THIS DEVICE |
| other | `!ERR -nn` | (code) |

E_CHANGED fix: YES on the same file again (the add selects the entry again). E_LOST on DT2 after a stock reset: the runtime reloads; the row passes LOADING again.

### 2.5 CLEAR

One press, no confirm: `sample_pool_clear`. The row goes to `-`. A note with SMP on that entry is silent from then on (the SMP value is not changed). Elektron project files are not touched (keep-stock rule): the pool list lives in the digikit runtime, per project.

### 2.6 Screen 4: INFO (DK_UI_TEXT_PAGE, title `INFO`)

```
+----------------------+
|        INFO          |
| SAMPLE 2             |
| KICK808              |
| DRUMS/KICKS/         |   path (wrapped)
| KICK808              |
| STEREO 48000 HZ      |
| 57600 FRAMES         |
| LENGTH 1.2s          |
| STATE READY          |
+----------------------+
```

An error entry shows its INFO text in `STATE`; an empty entry shows `EMPTY`. From the browser, INFO shows name, path and size of a listed file.

## 3. API calls, callbacks and gaps

| Call | From | Purpose |
| --- | --- | --- |
| `files_client` | `dk_picker_init` (in `dkm_init`) | listing client, positive handle |
| `files_list`, `files_cancel` | browser actions, `back` | start/replace a listing |
| `files_poll` | files notify callback (UI task) | copy rows, then `dk_ui_refresh` |
| `sample_pool_add(h, path)` | file row action | add, section 2.3 |
| `sample_pool_clear(h, index)` | CLEAR action | section 2.5 |
| `sample_pool_status`, `sample_pool_info` | fill of screen 1 and 4 | state, frames/rate/channels |
| `sample_pool_path` (API 1.6) | fill of screen 1 and 4 | the name of an entry |
| `dk_ui_tick` (core/ui) | fill of screen 1 | refresh while a load runs |

The picker checks that the Api `size` covers `sample_pool_info` and the files calls before use, and `sample_pool_path` separately. The handle is the machine's own (`api->sample_tracks(local_id)`), passed to `dk_picker_init`; the runtime does not check ownership, so a picker never guesses `handle = 1`. A stand-alone picker module would need a find call (`sample_tracks_find`); not built.

Paths: `sample_pool_add` takes the root-relative path of a listing row; the runtime adds the device root (sample-api-1.5.md 6.2).

## 4. Module layout, limits, tests

Files:
- `sdk/include/dk/chooser.h` and `sdk/src/chooser.c`: the generic component (list, browser, file INFO page, timer): `dk_chooser_init`, `dk_chooser_open(desc, current)`, `dk_chooser_refresh`, `dk_chooser_wrap`, `dk_chooser_fini`; the descriptor `dk_chooser_desc` has the row callback, `busy`, the add row, `add_file`, `error_text`, `panel` and `done`.
- `sdk/include/dk/sample_picker.h`: `dk_picker_init`, `dk_picker_open_pool(current, pick, ctx)`, `dk_picker_fini`.
- `sdk/src/sample_picker.c`: the pool's descriptor for the chooser, CLEAR and INFO, the entry INFO page.
- `sdk/src/picker_logic.h` and `sdk/src/picker_logic.c`: pure functions, no API calls: sort order, leaf-name truncation, path join/parent, size/length text, error text for a code, `pk_pool_row`.
- `modules/machines/shot/src/module.c`: lists the three sources, calls `dk_picker_init` in `dkm_init`, exports `shot_smp_click` (`DK_PARAM_ACTION`) and sets SMP in `smp_pick`.

Limits: statics about 8 KB (128 rows x 56 bytes, path buffers 256 bytes x 2). Menus are never freed: each is built once (pool list, browser, INFO page) and kept. All ctx and descriptors in module memory. Rows 23 characters. One browser instance (not re-entrant): a second open reuses it.

Tests:
1. Host tests, `modules/core/adapters/ui/host/tests/picker.rs` (`cargo test -p dkui-host`): the chooser and picker sources compiled against a fake `dk_api` (a listing provider and a 16-entry pool that adds into the first free entry) and the fake core/ui: list rows per state, the marker and the cursor on the current entry, YES on an entry (done once, all levels closed), + ADD to a file to done in one press, NO one level, an error that closes the flow and shows its text, CLEAR and the hole, INFO, sort order, the 128-row cap, long names, paths at the 255-byte limit, the timer; a toy chooser tests the component without the pool. The runtime side of `param_get` and `param_set` is in `modules/core/runtime/src/rs/src/paramapi.rs` (`tests`).
2. `dk:dev` screenshot sheet on DN2 (not run yet): `mise run dk:dev -- digikit/shot --device dn2 --store 'digikit/shot*' --keys '...,shot:pool,...'`. Look at every PNG before handoff.
3. One device test of the click path (SMP click, YES on an entry sets SMP, add a file, the note plays it) comes with the emulator proof; shot-dt2 and shot-dn2 cover the pool API and the SMP value on the device without the click.
4. Gate: `mise run module:check digikit/shot`, `mise run test:host`.

## 5. Decisions

1. The picker is shared source compiled into each sampler; a stand-alone module later needs a find call. (Em, 2026-11-01.)
2. The list shows all 16 indexes, so the row number is the SMP value. Names on the knob come later (a formatter field in core/params).
3. YES on an entry sets SMP and closes the whole flow. YES on + ADD opens the browser and YES on a file adds it, sets SMP to it and closes the whole flow in one press. (Em, 2026-11-02, ui-rules.md; replaces "the user turns SMP".)
4. No header preview in v1 (size only). 128 rows per folder, the picker sorts.
5. Refusals: no dialog (Em, 2026-11-01, one-button flows); the flow closes and the page shows the error text as a toast.
6. DN2 shows `NO SAMPLE POOL` while the image has no DN2 pool adapter.

## 5b. As built

- Two calls plus the fini: `dk_picker_init(api, handle)` in `dkm_init` (the listing client is init-only), `dk_picker_open_pool(current, pick, ctx)` from the SMP click, `dk_picker_fini()`.
- The 128-row table keeps the first 128 rows in sort order while the listing arrives (insertion by index), so the cap does not depend on the provider order. A name over 47 bytes is not shown and counts in `+N MORE`.
- A path over 255 bytes is refused by the chooser itself (toast `PATH TOO LONG OR BAD`); there are no disabled rows, because the picker does not know the device limit.
- The lists use a `check` callback and take the mark off at once. A list without it is closed by the stock handler after the row action, and a list that opens another view is not on the stack when that view closes (seen on the DN2 screen). On NO at the browser's root the chooser closes the browser itself and opens the pool list again; YES closes the browser, then the list **[O]**: the order and the toast on the page after the closes are for the emulator proof.
- The cursor starts on the current entry through `dk_ui_select(list, row)` (core/ui, a runtime import) called before `dk_ui_open`; the row is the entry index plus one for the + ADD row **[O]**.
- INFO text pages show about 16 characters per row; list rows about 21 to 23. Row text in lists is cut at 23.
