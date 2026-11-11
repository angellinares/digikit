//! Host fake of `<dk/platform/ui.h>`. The `dk_ui_*` functions below keep a model of menus, the view stack,
//! the open dialog and the toasts. `driver` acts like the user on that model.
//! The model is per thread; `session()` serialises tests, because the C code under test has statics.
use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::{Mutex, MutexGuard};

pub mod driver;

pub const DK_UI_ROW: usize = 24;
pub const DK_UI_TOAST: usize = 32;
pub const DK_UI_TEXT_PAGE: u32 = 1;
pub const DK_UI_LIST: u32 = 2;
pub const DK_UI_WIDE: u32 = 4;

pub const CAP_MENU: c_int = 1;
pub const CAP_TEXT_PAGE: c_int = 2;
pub const CAP_LIST: c_int = 3;
pub const CAP_TOGGLE: c_int = 4;
pub const CAP_VALUE: c_int = 5;
pub const CAP_CONFIRM: c_int = 6;
pub const CAP_CHOICE: c_int = 7;
pub const CAP_TOAST: c_int = 8;
pub const CAP_TICK: c_int = 9;
/// Room for timers, as in core/ui (`UI_TICKS` of tick.h).
pub const TICKS: usize = 4;

/// Opaque `dk_ui_menu *`: the menu index plus one. Never dereferenced.
#[repr(C)]
pub struct DkUiMenu {
    _p: [u8; 0],
}
type Ctx = *mut c_void;
type FillFn = extern "C" fn(*mut DkUiMenu, Ctx);
type ActFn = extern "C" fn(Ctx, c_int);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MenuDesc {
    pub name: *const c_char,
    pub title: *const c_char,
    pub flags: u32,
    pub fill: Option<FillFn>,
    pub ctx: Ctx,
    pub empty: *const c_char,
    pub panel: Option<extern "C" fn(*mut DkUiMenu, c_int, Ctx)>,
    pub check: Option<extern "C" fn(Ctx, c_int, c_int)>,
    pub back: Option<extern "C" fn(Ctx) -> c_int>,
}

#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Action {
        f: Option<ActFn>,
        ctx: Ctx,
        arg: c_int,
    },
    Submenu(usize),
    Text,
    Toggle {
        value: *mut c_int,
        f: Option<ActFn>,
        ctx: Ctx,
    },
    Value {
        value: *mut c_int,
        min: c_int,
        max: c_int,
        f: Option<ActFn>,
        ctx: Ctx,
    },
}

pub(crate) struct Row {
    pub text: String,
    pub kind: Kind,
}

pub(crate) struct Menu {
    pub desc: Option<MenuDesc>, // None for a list's side panel
    pub rows: Vec<Row>,
    pub checks: Vec<bool>,
    pub selected: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DialogKind {
    Confirm,
    Choice,
}

pub(crate) struct Dialog {
    pub kind: DialogKind,
    pub title: String,
    pub text: String,
    pub items: Vec<String>,
    pub confirm_done: Option<extern "C" fn(Ctx, c_int)>,
    pub ctx: Ctx,
}

type TickFn = extern "C" fn(Ctx);

/// A `dk_ui_tick` timer: the model keeps time in milliseconds, which only `driver::advance_ms` moves.
#[derive(Clone, Copy)]
pub(crate) struct Tick {
    pub f: TickFn,
    pub ctx: Ctx,
    pub ms: u32,
    pub last: u64,
}

#[derive(Default)]
pub(crate) struct Model {
    pub ticks: Vec<Tick>,
    pub now_ms: u64,
    pub menus: Vec<Menu>,
    pub stack: Vec<usize>,
    pub dialog: Option<Dialog>,
    pub toasts: Vec<String>,
    pub log: Vec<String>,
    pub caps_off: HashSet<c_int>,
}

thread_local! {
    static MODEL: RefCell<Model> = RefCell::new(Model::default());
}

/// Runs `f` on the model. Never call a C callback inside `f`: it would re-enter the model.
pub(crate) fn with<R>(f: impl FnOnce(&mut Model) -> R) -> R {
    MODEL.with(|m| f(&mut m.borrow_mut()))
}

static LOCK: Mutex<()> = Mutex::new(());

/// Takes the test lock and clears the model and the caps. Hold the result for the whole test.
pub fn session() -> MutexGuard<'static, ()> {
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    with(|m| *m = Model::default());
    g
}

/// Switches a capability off or on for the rest of the session.
pub fn set_cap(cap: c_int, on: bool) {
    with(|m| {
        if on {
            m.caps_off.remove(&cap);
        } else {
            m.caps_off.insert(cap);
        }
    });
}

/// The log of `dk_ui_*` calls, oldest first.
pub fn call_log() -> Vec<String> {
    with(|m| m.log.clone())
}

/// Toasts shown so far, oldest first.
pub fn toasts() -> Vec<String> {
    with(|m| m.toasts.clone())
}

impl Model {
    fn has(&self, cap: c_int) -> bool {
        !self.caps_off.contains(&cap)
    }
    fn log(&mut self, s: String) {
        self.log.push(s);
    }
}

fn text_cut(p: *const c_char, max_bytes: usize) -> String {
    if p.is_null() {
        return String::new();
    }
    let b = unsafe { CStr::from_ptr(p) }.to_bytes();
    String::from_utf8_lossy(&b[..b.len().min(max_bytes)]).into_owned()
}

fn row_text(p: *const c_char) -> String {
    text_cut(p, DK_UI_ROW - 1)
}

pub(crate) fn handle(id: usize) -> *mut DkUiMenu {
    (id + 1) as *mut DkUiMenu
}

fn id_of(m: *const DkUiMenu) -> Option<usize> {
    let v = m as usize;
    (v != 0 && with(|s| v - 1 < s.menus.len())).then(|| v - 1)
}

pub(crate) fn desc_of(id: usize) -> Option<MenuDesc> {
    with(|s| s.menus[id].desc)
}

/// Clears the rows and calls the fill function.
pub(crate) fn refill(id: usize) {
    let Some(d) = desc_of(id) else { return };
    with(|s| s.menus[id].rows.clear());
    if let Some(f) = d.fill {
        f(handle(id), d.ctx);
    }
}

pub(crate) fn open_id(id: usize) -> c_int {
    let Some(d) = desc_of(id) else { return -1 };
    let ok = with(|s| {
        s.has(CAP_MENU)
            && (d.flags & DK_UI_TEXT_PAGE == 0 || s.has(CAP_TEXT_PAGE))
            && (d.flags & (DK_UI_LIST | DK_UI_WIDE) == 0 || s.has(CAP_LIST))
    });
    if !ok {
        return -1;
    }
    refill(id);
    with(|s| {
        if !s.stack.contains(&id) {
            s.stack.push(id);
        }
    });
    0
}

fn add(m: *mut DkUiMenu, text: *const c_char, kind: Kind, what: &str) {
    let Some(id) = id_of(m) else { return };
    let text = row_text(text);
    with(|s| {
        s.log(format!("{what} {text}"));
        s.menus[id].rows.push(Row { text, kind });
    });
}

#[no_mangle]
pub extern "C" fn dk_ui_has(cap: c_int) -> c_int {
    with(|s| s.has(cap)) as c_int
}

/// # Safety
/// `d` is null or points at a valid descriptor.
#[no_mangle]
pub unsafe extern "C" fn dk_ui_menu_new(d: *const MenuDesc) -> *mut DkUiMenu {
    if d.is_null() {
        return std::ptr::null_mut();
    }
    let d = unsafe { *d };
    with(|s| {
        let ok = s.has(CAP_MENU)
            && (d.flags & DK_UI_TEXT_PAGE == 0 || s.has(CAP_TEXT_PAGE))
            && (d.flags & (DK_UI_LIST | DK_UI_WIDE) == 0 || s.has(CAP_LIST));
        if !ok {
            return std::ptr::null_mut();
        }
        s.log(format!("menu_new {}", text_cut(d.name, 64)));
        s.menus.push(Menu {
            desc: Some(d),
            rows: vec![],
            checks: vec![],
            selected: 0,
        });
        handle(s.menus.len() - 1)
    })
}

#[no_mangle]
pub extern "C" fn dk_ui_open(m: *mut DkUiMenu) -> c_int {
    let Some(id) = id_of(m) else { return -1 };
    with(|s| s.log(format!("open {id}")));
    open_id(id)
}

#[no_mangle]
pub extern "C" fn dk_ui_close(m: *mut DkUiMenu) {
    if let Some(id) = id_of(m) {
        with(|s| {
            s.log(format!("close {id}"));
            s.stack.retain(|&x| x != id);
        });
    }
}

#[no_mangle]
pub extern "C" fn dk_ui_select(m: *mut DkUiMenu, row: c_int) {
    let Some(id) = id_of(m) else { return };
    if row >= 0 {
        with(|s| {
            s.log(format!("select {id} {row}"));
            s.menus[id].selected = row as usize;
        });
    }
}

#[no_mangle]
pub extern "C" fn dk_ui_is_open(m: *const DkUiMenu) -> c_int {
    id_of(m).is_some_and(|id| with(|s| s.stack.contains(&id))) as c_int
}

#[no_mangle]
pub extern "C" fn dk_ui_refresh(m: *mut DkUiMenu) {
    let Some(id) = id_of(m) else { return };
    with(|s| s.log(format!("refresh {id}")));
    refill(id);
    with(|s| {
        let n = s.menus[id].rows.len();
        s.menus[id].selected = s.menus[id].selected.min(n.saturating_sub(1));
    });
}

#[no_mangle]
pub extern "C" fn dk_ui_action(
    m: *mut DkUiMenu,
    text: *const c_char,
    f: Option<ActFn>,
    ctx: Ctx,
    arg: c_int,
) {
    add(m, text, Kind::Action { f, ctx, arg }, "action");
}

#[no_mangle]
pub extern "C" fn dk_ui_submenu(m: *mut DkUiMenu, text: *const c_char, sub: *mut DkUiMenu) {
    if let Some(sid) = id_of(sub) {
        add(m, text, Kind::Submenu(sid), "submenu");
    }
}

#[no_mangle]
pub extern "C" fn dk_ui_text(m: *mut DkUiMenu, text: *const c_char) {
    add(m, text, Kind::Text, "text");
}

#[no_mangle]
pub extern "C" fn dk_ui_toggle(
    m: *mut DkUiMenu,
    text: *const c_char,
    value: *mut c_int,
    f: Option<ActFn>,
    ctx: Ctx,
) {
    if dk_ui_has(CAP_TOGGLE) != 0 {
        add(m, text, Kind::Toggle { value, f, ctx }, "toggle");
    }
}

#[no_mangle]
pub extern "C" fn dk_ui_value(
    m: *mut DkUiMenu,
    text: *const c_char,
    value: *mut c_int,
    min: c_int,
    max: c_int,
    f: Option<ActFn>,
    ctx: Ctx,
) {
    if dk_ui_has(CAP_VALUE) != 0 {
        add(
            m,
            text,
            Kind::Value {
                value,
                min,
                max,
                f,
                ctx,
            },
            "value",
        );
    }
}

#[no_mangle]
pub extern "C" fn dk_ui_check(list: *mut DkUiMenu, row: c_int, on: c_int) {
    let Some(id) = id_of(list) else { return };
    if row < 0 {
        return;
    }
    with(|s| {
        s.log(format!("check {row} {on}"));
        let c = &mut s.menus[id].checks;
        if c.len() <= row as usize {
            c.resize(row as usize + 1, false);
        }
        c[row as usize] = on != 0;
    });
}

#[no_mangle]
pub extern "C" fn dk_ui_checked(list: *const DkUiMenu, row: c_int) -> c_int {
    let Some(id) = id_of(list) else { return 0 };
    with(|s| {
        row >= 0
            && s.menus[id]
                .checks
                .get(row as usize)
                .copied()
                .unwrap_or(false)
    }) as c_int
}

#[no_mangle]
pub extern "C" fn dk_ui_confirm(
    title: *const c_char,
    text: *const c_char,
    done: Option<extern "C" fn(Ctx, c_int)>,
    ctx: Ctx,
) -> c_int {
    with(|s| {
        if !s.has(CAP_CONFIRM) || s.dialog.is_some() {
            return -1;
        }
        let (title, text) = (text_cut(title, 80), text_cut(text, 80));
        s.log(format!("confirm {title}"));
        s.dialog = Some(Dialog {
            kind: DialogKind::Confirm,
            title,
            text,
            items: vec![],
            confirm_done: done,
            ctx,
        });
        0
    })
}

/// # Safety
/// `items` points at `n` valid C strings.
#[no_mangle]
pub unsafe extern "C" fn dk_ui_choice(
    title: *const c_char,
    items: *const *const c_char,
    n: c_int,
    done: Option<extern "C" fn(Ctx, c_int)>,
    ctx: Ctx,
) -> c_int {
    with(|s| {
        if !s.has(CAP_CHOICE) || s.dialog.is_some() || n <= 0 || items.is_null() {
            return -1;
        }
        let items = (0..n as usize)
            .map(|i| row_text(unsafe { *items.add(i) }))
            .collect();
        let title = row_text(title);
        s.log(format!("choice {title}"));
        s.dialog = Some(Dialog {
            kind: DialogKind::Choice,
            title,
            text: String::new(),
            items,
            confirm_done: done,
            ctx,
        });
        0
    })
}

#[no_mangle]
pub extern "C" fn dk_ui_toast(text: *const c_char) -> c_int {
    with(|s| {
        if !s.has(CAP_TOAST) {
            return -1;
        }
        let t: String = text_cut(text, 4096).chars().take(DK_UI_TOAST).collect();
        s.log(format!("toast {t}"));
        s.toasts.push(t);
        0
    })
}

#[no_mangle]
pub extern "C" fn dk_ui_tick(f: Option<TickFn>, ctx: Ctx, period_ms: u32) -> c_int {
    with(|s| {
        let Some(f) = f else { return -1 };
        if !s.has(CAP_TICK) {
            return -1;
        }
        s.log(format!("tick {period_ms}"));
        let now = s.now_ms;
        let at = s
            .ticks
            .iter()
            .position(|t| t.f as usize == f as usize && t.ctx == ctx);
        match (at, period_ms) {
            (Some(i), 0) => {
                s.ticks.remove(i);
            }
            (Some(i), ms) => {
                s.ticks[i].ms = ms.min(1_000_000);
                s.ticks[i].last = now;
            }
            (None, 0) => {}
            (None, ms) => {
                if s.ticks.len() >= TICKS {
                    return -1;
                }
                s.ticks.push(Tick {
                    f,
                    ctx,
                    ms: ms.min(1_000_000),
                    last: now,
                });
            }
        }
        0
    })
}
