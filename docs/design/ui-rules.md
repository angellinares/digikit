# UI rules for digikit lists and browsers

Status: accepted by Em (2026-11-02).

## Rules

These rules apply to every list, browser and menu that a module or the runtime opens.

1. YES does the action and returns to where the flow started. The whole flow closes (all levels) and the user is back on the page they came from.
2. NO goes back one level and changes nothing.
3. RIGHT opens the side panel for extra actions (for example CLEAR, INFO). LEFT or NO closes the panel.
4. When a list opens, the current choice is marked and the cursor starts on it.
5. No confirm dialogs in a flow. An error shows where the user looks next (the row, or the page), not in a dialog (one-button flows).
6. Use the firmware font and stock views (core/ui `DK_UI_LIST`, `DK_UI_TEXT_PAGE`).

Stock reference: the Digitakt II pool list (manual 9.7). YES selects the sample and closes. Loading a file from the SAMPLES browser assigns it to the track.

## The chooser component (SDK)

A reusable list-and-browse component built on dk/ui.h. It is generalised from the sample picker (sdk/src/sample_picker.c, picker_logic.c).

- `dk_chooser`: a list with a title, rows (text, state), a marked current row, an optional first action row (for example "+ ADD"), an optional side panel with actions, and an optional browser level (folder browser on the SDK file listing).
- Result callback: `done(ctx, result)` is called once when YES finishes the flow. The result is the chosen row or the added item. It is not called on NO.
- "YES closes the flow": after `done`, the chooser closes all its levels in one step.
- The list refreshes while items load (dk_ui_tick).

Users: SHOT's SMP pool list now. Later, any module that picks from a list, and the DIGIKIT > MODULES list after a separate review against these rules.

## SHOT: SMP pool flow

SHOT page (DT2: A TUNE, B SMP, C START, D reserved, E END, F REV, G PITCH, H LEV; DN2: B SMP, C START, E END, F REV, G PITCH; A, D, H free):

```text
DT2 (SRC page)                          DN2 (SYN page)
+-----------------------------+         +-----------------------------+
| TUNE   SMP   START   --     |         |  --    SMP   START   --     |
|  0     00     0             |         |        00     0             |
| END    REV   PITCH   LEV    |         | END    REV   PITCH   --     |
| 127    0     64      100    |         | 127    0     64             |
+-----------------------------+         +-----------------------------+
 D = reserved (stock sample slot)        A, D, H free for DN2-only parameters
```

v1 shows the slot number on SMP; the pool list shows names. The name on the knob needs a value-text hook (later step).

Pool list:

```text
+------------------------+
| SHOT SAMPLES           |
|   + ADD                |
| > 00 KICK        0.4s  |   (> = active entry)
|   01 SNARE       0.3s  |
|   ...                  |
|   15 -                 |
+------------------------+
```

Browser:

```text
+------------------------+
| SAMPLES                |
|   .. /                 |
|   DRUMS /              |
|   kick_01.wav    35 KB |
|   snare_02.wav   28 KB |
+------------------------+
```

Flow:

```text
click SMP (release without a turn) -> pool list, cursor on the active entry
  YES on an entry   -> SMP = that entry; back on the page
  YES on + ADD      -> browser
    YES on a file   -> added to the pool, SMP = the new entry; back on the page (one press)
    error           -> back on the page; the error shows on the page/row (e.g. POOL FULL)
  NO                -> back one level; nothing changed
  RIGHT on an entry -> CLEAR / INFO
turn SMP           -> steps through the pool (p-lockable), as before
```

## Runtime API for parameter-driven flows

- The click callback stays `fn(track)` (DK_PARAM_ACTION).
- New Api entries. They are for the UI task only and valid only during a click callback:
  - `int32_t dkrt_param_get(uint32_t track, uint32_t word)` returns the raw value (unit*256).
  - `int32_t dkrt_param_set(uint32_t track, uint32_t word, int32_t raw)` returns a status.
- They use the stock path a knob turn uses. Get is the page's value getter (view vtable 0x54). Set uses view vtable 0xc4 for the parameter object, then the stock absolute setter (DT2 0x4003b570, DN2 0x40037326; one cf_func anchor resolves both).
- The setter clamps to the row range, stores the value in the sound, notifies the project and redraws. Source: static reading, scratchpad set-param-check.md. To be proven in the emulator.
- Guards:
  - Only the module's own parameter words.
  - Track 0..15.
  - The page view must still be the one clicked.
  - While a trig key is held, set refuses, so a click never records a p-lock.

## Open questions

1. MODULES list: review against these rules as a separate step (recommend yes, after SHOT).
2. Show the sample name on the SMP knob (value-text hook): later step.
