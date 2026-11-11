//! Acts like the user on the model: keys, the encoder and dialog answers.
//! A call that does not fit the screen (no such row, no dialog) panics with a message,
//! so a wrong test fails loudly.
use crate::*;

fn top() -> usize {
    with(|s| s.stack.last().copied()).expect("no view is open")
}

fn row_kind(id: usize, row: usize) -> Kind {
    with(|s| s.menus[id].rows.get(row).map(|r| r.kind)).unwrap_or_else(|| panic!("no row {row}"))
}

fn is_list(id: usize) -> bool {
    desc_of(id).is_some_and(|d| d.flags & DK_UI_LIST != 0)
}

/// True when the top view is a menu page with the full-width title bar and rows (`DK_UI_WIDE`).
pub fn is_wide() -> bool {
    desc_of(top()).is_some_and(|d| d.flags & DK_UI_WIDE != 0)
}

/// Opens a menu (as `dk_ui_open` does). Returns its result.
pub fn open(m: *mut DkUiMenu) -> c_int {
    dk_ui_open(m)
}

/// Number of open views (menus, lists and panels).
pub fn depth() -> usize {
    with(|s| s.stack.len())
}

/// The row the cursor is on in the top view (`dk_ui_select` and `press_yes` set it).
pub fn cursor() -> usize {
    let id = top();
    with(|s| s.menus[id].selected)
}

/// The title of the top view; empty for a panel.
pub fn title() -> String {
    let id = top();
    desc_of(id)
        .map(|d| text_cut(d.title, 255))
        .unwrap_or_default()
}

/// The rows of the top view as drawn. A toggle ends in ON or OFF, a value in three digits.
/// A list row starts with "* " when checked and "  " when not. An empty list shows its empty text.
/// The blank glyph 0x01 that pads a column reads as a space (`labels` keeps it).
pub fn render() -> Vec<String> {
    let id = top();
    let list = is_list(id);
    let d = desc_of(id);
    let lines: Vec<String> = with(|s| {
        s.menus[id]
            .rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let body = match r.kind {
                    Kind::Toggle { value, .. } => {
                        let on = !value.is_null() && unsafe { *value } != 0;
                        format!("{} {}", r.text, if on { "ON" } else { "OFF" })
                    }
                    Kind::Value { value, .. } => {
                        let v = if value.is_null() {
                            0
                        } else {
                            unsafe { *value }
                        };
                        format!("{} {:03}", r.text, v)
                    }
                    _ => r.text.clone(),
                };
                let body = body.replace('\u{1}', " ");
                if list {
                    let c = s.menus[id].checks.get(i).copied().unwrap_or(false);
                    format!("{}{}", if c { "* " } else { "  " }, body)
                } else {
                    body
                }
            })
            .collect()
    });
    if lines.is_empty() && list {
        if let Some(d) = d.filter(|d| !d.empty.is_null()) {
            return vec![text_cut(d.empty, 255)];
        }
    }
    lines
}

/// The labels of the top view's rows as the menu wrote them, with the padding glyph 0x01.
pub fn labels() -> Vec<String> {
    let id = top();
    with(|s| s.menus[id].rows.iter().map(|r| r.text.clone()).collect())
}

fn activate(id: usize, row: usize) {
    match row_kind(id, row) {
        Kind::Action { f, ctx, arg } => {
            if let Some(f) = f {
                f(ctx, arg)
            }
        }
        Kind::Submenu(sub) => {
            open_id(sub);
        }
        Kind::Text | Kind::Value { .. } => {}
        Kind::Toggle { value, f, ctx } => {
            let new = (!value.is_null() && unsafe { *value } == 0) as c_int;
            if !value.is_null() {
                unsafe { *value = new };
            }
            if let Some(f) = f {
                f(ctx, new)
            }
        }
    }
}

/// YES on a row. A list row toggles its check mark and calls `check`; other rows act.
pub fn press_yes(row: usize) {
    let id = top();
    with(|s| s.menus[id].selected = row);
    if is_list(id) {
        let n = with(|s| s.menus[id].rows.len());
        assert!(row < n, "no row {row}");
        let on = dk_ui_checked(handle(id), row as c_int) == 0;
        dk_ui_check(handle(id), row as c_int, on as c_int);
        if let Some(f) = desc_of(id).and_then(|d| d.check.map(|f| (f, d.ctx))) {
            f.0(f.1, row as c_int, on as c_int);
        }
    } else {
        activate(id, row);
    }
}

/// The encoder on a value row: adds `delta`, clamps to min..max, writes the value and calls
/// `changed` when the value changed.
pub fn turn(row: usize, delta: i32) {
    let id = top();
    let Kind::Value {
        value,
        min,
        max,
        f,
        ctx,
    } = row_kind(id, row)
    else {
        panic!("row {row} is not a value row")
    };
    let old = unsafe { *value };
    let new = old.saturating_add(delta).clamp(min, max);
    unsafe { *value = new };
    if new != old {
        if let Some(f) = f {
            f(ctx, new)
        }
    }
}

/// RIGHT on a list row: builds a fresh side panel with the list's `panel` callback and shows it.
pub fn press_right(row: usize) {
    let id = top();
    let d = desc_of(id)
        .filter(|d| d.flags & DK_UI_LIST != 0)
        .expect("RIGHT needs a list");
    let Some(panel) = d.panel else { return };
    let pid = with(|s| {
        s.menus.push(Menu {
            desc: None,
            rows: vec![],
            checks: vec![],
            selected: 0,
        });
        s.stack.push(s.menus.len() - 1);
        s.log(format!("panel {row}"));
        s.menus.len() - 1
    });
    panel(handle(pid), row as c_int, d.ctx);
    // core/ui shows no panel when the callback added no rows
    if with(|s| s.menus[pid].rows.is_empty()) {
        with(|s| s.stack.retain(|&x| x != pid));
    }
}

/// YES on a row of the open side panel: the panel closes, then the row acts.
pub fn choose_panel_row(row: usize) {
    let id = top();
    assert!(desc_of(id).is_none(), "the top view is not a side panel");
    with(|s| {
        s.stack.pop();
    });
    activate(id, row);
}

/// NO. An open dialog takes it (a confirm calls `done(0)`, a choice just closes).
/// Otherwise a panel closes; a list calls `back` and stays open when it returns 1;
/// any other view closes.
pub fn press_no() {
    if let Some(dlg) = with(|s| s.dialog.take()) {
        if dlg.kind == DialogKind::Confirm {
            if let Some(f) = dlg.confirm_done {
                f(dlg.ctx, 0)
            }
        }
        return;
    }
    let id = top();
    if let Some(back) = desc_of(id)
        .filter(|d| d.flags & DK_UI_LIST != 0)
        .and_then(|d| d.back.map(|b| (b, d.ctx)))
    {
        if back.0(back.1) == 1 {
            return;
        }
    }
    with(|s| s.stack.retain(|&x| x != id));
}

/// What the open dialog shows: (kind, title, text, items).
pub fn dialog() -> Option<(DialogKind, String, String, Vec<String>)> {
    with(|s| {
        s.dialog
            .as_ref()
            .map(|d| (d.kind, d.title.clone(), d.text.clone(), d.items.clone()))
    })
}

/// Answers the open confirm. The dialog closes first, then `done` runs.
pub fn answer_confirm(yes: bool) {
    let dlg = with(|s| s.dialog.take()).expect("no dialog is open");
    assert_eq!(dlg.kind, DialogKind::Confirm, "the dialog is not a confirm");
    if let Some(f) = dlg.confirm_done {
        f(dlg.ctx, yes as c_int)
    }
}

/// Picks item `index` of the open choice. The dialog closes first, then `done` runs.
pub fn pick_choice(index: usize) {
    let dlg = with(|s| s.dialog.take()).expect("no dialog is open");
    assert_eq!(dlg.kind, DialogKind::Choice, "the dialog is not a choice");
    assert!(index < dlg.items.len(), "no item {index}");
    if let Some(f) = dlg.confirm_done {
        f(dlg.ctx, index as c_int)
    }
}

/// Moves the clock of the model by `ms` milliseconds, one millisecond at a time, and runs every timer of `dk_ui_tick` when its period is
/// over (in the order set). A timer that an earlier callback stopped does not run.
pub fn advance_ms(ms: u32) {
    for _ in 0..ms {
        with(|s| s.now_ms += 1);
        let mut i = 0;
        loop {
            let due = with(|s| {
                let now = s.now_ms;
                let t = s.ticks.get_mut(i)?;
                if now - t.last >= t.ms as u64 {
                    t.last = now;
                    Some(Some((t.f, t.ctx)))
                } else {
                    Some(None)
                }
            });
            match due {
                None => break,
                Some(Some((f, ctx))) => f(ctx),
                Some(None) => {}
            }
            i += 1;
        }
    }
}

/// The periods (ms) of the running timers, in the order set.
pub fn tick_periods() -> Vec<u32> {
    with(|s| s.ticks.iter().map(|t| t.ms).collect())
}
