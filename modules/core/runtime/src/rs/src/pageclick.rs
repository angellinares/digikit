//! The encoder click on a machine's page (docs/design/machine-page-click.md).
//!
//! A machine module declares `DK_PARAM_ACTION(machine, word, fn)`. The packer lowers it to an ACTION record of the `register` table; `registry::apply`
//! stores the callback's address per parameter row (`set`). Two hooks at the entry of the machine page's key handler and turn handler
//! (`dkrt_pagekey_hook`, `dkrt_pageturn_hook`, src/cf/tramp.s) call `dkrt_page_key` and `dkrt_page_turn` and then run the stock handler unchanged:
//! the hooks never consume an event.
//!
//! A click is a press of an encoder's key (codes 0x29 to 0x30) and its release with no turn in between. Press and turn is the stock coarse step
//! and never a click. The parameter under the encoder comes from the page itself (vtable slot 47 of the view, the call the stock list-opener
//! uses): the dispatch is by parameter row, so it does not depend on the device's knob layout. The callback runs from the UI loop pass
//! that follows the release (`tick`), in the UI task, with the track number: `void fn(u32 track)`.
//!
//! Without any action, `dkrt_page_key` and `dkrt_page_turn` return at one test of `ARMED`.
use crate::paramapi::{self, Ctx};
use crate::registry;
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use dkrt_sdk::limits;

/// Key code of the click of encoder 0; encoder `e` is code `FIRST_CODE + e`, for e = 0 to 7.
pub const FIRST_CODE: u32 = 0x29;
pub const ENCODERS: u32 = 8;
/// Flag bits of a key event (`event+0x10`).
pub const F_PRESS: u32 = 0x01;
pub const F_CHORD: u32 = 0x02;
pub const F_REPEAT: u32 = 0x08;
pub const F_RELEASE: u32 = 0x10;
/// The page's `encoder -> parameter code` function: vtable slot 47 (offset 0xbc), `i32 (view, encoder)`.
const VT_PARAM_CODE: usize = 47;
/// The page's track reference (`view + 0x74`, the argument of `cf.page.fn.view_track`).
const VIEW_TRACK_REF: usize = 0x74;
const NONE: u32 = 0xff;

/// Tells a click from a press and a turn. Pure state, no firmware access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Detector {
    /// The encoder whose press is waiting for its release (`NONE`: no press).
    enc: u32,
    view: usize,
    turned: bool,
}

impl Detector {
    pub const fn new() -> Self {
        Detector { enc: NONE, view: 0, turned: false }
    }

    /// A key event of `view`. Returns the encoder of a click: its release, when its press came on the same view and no turn came in between.
    /// A chord (FUNC and so on) and a repeat never make a click; another key's press cancels the pending press.
    pub fn key(&mut self, view: usize, code: u32, flags: u32) -> Option<u32> {
        let enc = code.wrapping_sub(FIRST_CODE);
        if enc >= ENCODERS {
            if flags & F_PRESS != 0 && flags & F_REPEAT == 0 {
                self.enc = NONE;
            }
            return None;
        }
        if flags & F_RELEASE != 0 {
            let click = self.enc == enc && self.view == view && !self.turned && flags & F_CHORD == 0;
            self.enc = NONE;
            return if click { Some(enc) } else { None };
        }
        if flags & F_PRESS != 0 && flags & F_REPEAT == 0 {
            self.enc = if flags & F_CHORD == 0 { enc } else { NONE };
            self.view = view;
            self.turned = false;
        }
        None
    }

    /// An encoder turn on `view`: the press that is waiting becomes a coarse step.
    pub fn turn(&mut self, view: usize) {
        if self.enc != NONE && self.view == view {
            self.turned = true;
        }
    }
}

/// The callback of parameter code `code`, or 0. `actions[k]` belongs to the parameter row `first + k`.
pub fn find(actions: &[u32], first: u32, code: i32) -> u32 {
    let k = (code as u32).wrapping_sub(first) as usize;
    if code >= 0 && k < actions.len() {
        actions[k]
    } else {
        0
    }
}

/// What a click queues: the callback of the parameter `code` and the track (asked for only when there is a callback), or None: the stock click.
pub fn resolve(actions: &[u32], first: u32, code: i32, track: impl FnOnce() -> u32) -> Option<(u32, u32)> {
    match find(actions, first, code) {
        0 => None,
        f => Some((f, track())),
    }
}

static mut DET: Detector = Detector::new();
static mut ACTIONS: [u32; limits::PARAM_ROWS] = [0; limits::PARAM_ROWS];
/// The number of parameter rows that have an action: 0 keeps both hooks at one test.
static mut ARMED: u32 = 0;
/// A click waiting for the next UI pass: the callback (0: none), the track, the clicked view and the clicked parameter code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pending {
    pub callback: u32,
    pub track: u32,
    pub view: usize,
    pub code: u32,
}
const NO_CLICK: Pending = Pending { callback: 0, track: 0, view: 0, code: 0 };
static mut PENDING: Pending = NO_CLICK;
/// The click whose callback is running (`param_get` and `param_set` work only while it is set).
static mut CURRENT: Option<Ctx> = None;

/// Set (`addr` != 0) or clear (0) the action of parameter row `k` (a row of core/dynreg). Called by the registry.
pub unsafe fn set(k: usize, addr: u32) {
    if k >= limits::PARAM_ROWS {
        return;
    }
    let a = &mut *addr_of_mut!(ACTIONS);
    let old = a[k];
    a[k] = addr;
    let mut n = 0;
    let mut i = 0;
    while i < limits::PARAM_ROWS {
        if a[i] != 0 {
            n += 1;
        }
        i += 1;
    }
    write_volatile(addr_of_mut!(ARMED), n);
    // a click of an unloaded module must not run
    let p = &mut *addr_of_mut!(PENDING);
    if addr == 0 && old != 0 && p.callback == old {
        *p = NO_CLICK;
    }
}

unsafe fn word(p: *const u8, off: usize) -> u32 {
    read_volatile(p.add(off) as *const u32)
}

/// Hook at the entry of the page's key handler (stack: view, event).
#[cfg(not(test))]
#[no_mangle]
pub unsafe extern "C" fn dkrt_page_key(view: *const u8, ev: *const u8) {
    if read_volatile(addr_of!(ARMED)) == 0 {
        return;
    }
    let det = &mut *addr_of_mut!(DET);
    if let Some(enc) = det.key(view as usize, word(ev, 0xc), word(ev, 0x10)) {
        click(view, enc);
    }
}

/// Hook at the entry of the page's turn handler (stack: view, event). Only an encoder event (`event[3] - 1` is 0 to 7) is a turn.
#[cfg(not(test))]
#[no_mangle]
pub unsafe extern "C" fn dkrt_page_turn(view: *const u8, ev: *const u8) {
    if read_volatile(addr_of!(ARMED)) == 0 {
        return;
    }
    if word(ev, 0xc).wrapping_sub(1) < ENCODERS {
        (*addr_of_mut!(DET)).turn(view as usize);
    }
}

/// A click of encoder `enc` on `view`: queue the action of the parameter under it, if it has one.
#[cfg(not(test))]
unsafe fn click(view: *const u8, enc: u32) {
    let vt = word(view, 0) as *const u8;
    let code_of: unsafe extern "C" fn(*const u8, u32) -> i32 = core::mem::transmute(word(vt, 4 * VT_PARAM_CODE) as usize);
    let code = code_of(view, enc);
    let first = registry::param_first();
    let track = || dkfw::page::view_track(word(view, VIEW_TRACK_REF) as *const u8);
    if let Some((callback, track)) = resolve(&*addr_of!(ACTIONS), first, code, track) {
        *addr_of_mut!(PENDING) = Pending { callback, track, view: view as usize, code: code as u32 };
    }
}

/// The context of a click: the owner is the module of the clicked parameter row.
pub fn context(p: &Pending, owners: &[u8], first: u32) -> Option<Ctx> {
    let k = p.code.wrapping_sub(first) as usize;
    match owners.get(k) {
        Some(&owner) if owner != 0 => Some(Ctx { view: p.view, track: p.track, owner }),
        _ => None,
    }
}

/// The UI loop pass: run the queued action (`void fn(u32 track)`). `param_get` and `param_set` see the clicked view while it runs.
#[cfg(not(test))]
pub unsafe fn tick() {
    let p = *addr_of!(PENDING);
    if p.callback != 0 {
        *addr_of_mut!(PENDING) = NO_CLICK;
        let owners = &(*addr_of!(registry::DKRT_REG)).param_owner;
        let Some(ctx) = context(&p, owners, registry::param_first()) else { return };
        *addr_of_mut!(CURRENT) = Some(ctx);
        let call: unsafe extern "C" fn(u32) = core::mem::transmute(p.callback as usize);
        call(p.track);
        *addr_of_mut!(CURRENT) = None;
    }
}

#[cfg(test)]
pub unsafe fn tick() {}

/// The page's own functions, as a knob turn uses them (docs/design/ui-rules.md, "Runtime API for parameter-driven flows").
#[cfg(not(test))]
struct PageFw;

/// Vtable slots of the page (`view`) and of its parameter object.
#[cfg(not(test))]
const VT_PAGE_VALUE: usize = 0x54;
#[cfg(not(test))]
const VT_PAGE_OBJECT: usize = 0xc4;
#[cfg(not(test))]
const VT_OBJECT_VALUE: usize = 0x28;

#[cfg(not(test))]
unsafe fn vcall(obj: usize, off: usize) -> usize {
    word(word(obj as *const u8, 0) as *const u8, off) as usize
}

#[cfg(not(test))]
impl paramapi::Fw for PageFw {
    fn mode_key_held(&self) -> bool {
        paramapi::MODE_KEYS.iter().any(|&k| unsafe { dkfw::input::key_down(k) } != 0)
    }
    fn page_value(&self, view: usize, code: u32) -> i32 {
        unsafe {
            let f: unsafe extern "C" fn(usize, u32, i32) -> i32 = core::mem::transmute(vcall(view, VT_PAGE_VALUE));
            f(view, code, -1)
        }
    }
    fn object(&self, view: usize, code: u32, track: u32) -> usize {
        unsafe {
            let f: unsafe extern "C" fn(usize, u32, u32) -> usize = core::mem::transmute(vcall(view, VT_PAGE_OBJECT));
            f(view, code, track)
        }
    }
    fn object_value(&self, obj: usize, code: u32) -> i32 {
        unsafe {
            let f: unsafe extern "C" fn(usize, u32) -> i32 = core::mem::transmute(vcall(obj, VT_OBJECT_VALUE));
            f(obj, code)
        }
    }
    fn store(&self, obj: usize, code: u32, raw: i32, track: u32) {
        unsafe {
            dkfw::param::set_value(obj as *const u8, code, raw, track, 1, 1, 1);
        }
    }
}

/// `Api.param_get`.
#[cfg(not(test))]
pub unsafe fn param_get(track: u32, word: u32) -> i32 {
    let owners = &(*addr_of!(registry::DKRT_REG)).param_owner;
    paramapi::get(&PageFw, *addr_of!(CURRENT), owners, registry::param_first(), track, word)
}

/// `Api.param_set`.
#[cfg(not(test))]
pub unsafe fn param_set(track: u32, word: u32, raw: i32) -> i32 {
    let owners = &(*addr_of!(registry::DKRT_REG)).param_owner;
    paramapi::set(&PageFw, *addr_of!(CURRENT), owners, registry::param_first(), track, word, raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: usize = 0x1000;
    const PRESS: u32 = F_PRESS;
    const REL: u32 = F_RELEASE;

    fn code(e: u32) -> u32 {
        FIRST_CODE + e
    }

    #[test]
    fn press_then_release_is_a_click() {
        let mut d = Detector::new();
        assert_eq!(d.key(V, code(1), PRESS), None);
        assert_eq!(d.key(V, code(1), REL), Some(1));
        assert_eq!(d.key(V, code(1), REL), None, "one click per press");
    }

    #[test]
    fn a_turn_between_press_and_release_is_a_coarse_step() {
        let mut d = Detector::new();
        d.key(V, code(2), PRESS);
        d.turn(V);
        assert_eq!(d.key(V, code(2), REL), None);
        // the next press starts clean
        d.key(V, code(2), PRESS);
        assert_eq!(d.key(V, code(2), REL), Some(2));
    }

    #[test]
    fn a_turn_without_a_press_is_ignored() {
        let mut d = Detector::new();
        d.turn(V);
        d.key(V, code(0), PRESS);
        assert_eq!(d.key(V, code(0), REL), Some(0));
    }

    #[test]
    fn repeats_neither_start_nor_break_a_click() {
        let mut d = Detector::new();
        d.key(V, code(3), PRESS);
        assert_eq!(d.key(V, code(3), F_REPEAT), None);
        assert_eq!(d.key(V, code(3), PRESS | F_REPEAT), None);
        assert_eq!(d.key(V, code(3), REL), Some(3));
        // a repeat alone does not arm a release
        assert_eq!(d.key(V, code(4), PRESS | F_REPEAT), None);
        assert_eq!(d.key(V, code(4), REL), None);
    }

    #[test]
    fn a_release_of_another_encoder_is_not_a_click() {
        let mut d = Detector::new();
        d.key(V, code(1), PRESS);
        assert_eq!(d.key(V, code(2), REL), None);
        assert_eq!(d.key(V, code(1), REL), None, "the pending press was spent");
    }

    #[test]
    fn another_key_or_another_view_cancels() {
        let mut d = Detector::new();
        d.key(V, code(1), PRESS);
        d.key(V, 0x02, PRESS); // SRC key
        assert_eq!(d.key(V, code(1), REL), None);
        d.key(V, code(1), PRESS);
        assert_eq!(d.key(V + 0x100, code(1), REL), None, "released on another page");
        // a release of a key that is not an encoder's leaves the press alone
        d.key(V, code(1), PRESS);
        d.key(V, 0x02, REL);
        assert_eq!(d.key(V, code(1), REL), Some(1));
    }

    #[test]
    fn a_chord_is_not_a_click() {
        let mut d = Detector::new();
        d.key(V, code(1), PRESS | F_CHORD);
        assert_eq!(d.key(V, code(1), REL), None);
        d.key(V, code(1), PRESS);
        assert_eq!(d.key(V, code(1), REL | F_CHORD), None);
    }

    #[test]
    fn codes_outside_the_encoders_are_no_click() {
        let mut d = Detector::new();
        d.key(V, FIRST_CODE - 1, PRESS);
        assert_eq!(d.key(V, FIRST_CODE - 1, REL), None);
        d.key(V, FIRST_CODE + ENCODERS, PRESS);
        assert_eq!(d.key(V, FIRST_CODE + ENCODERS, REL), None);
        assert_eq!(d.key(V, 0, REL), None);
    }

    #[test]
    fn a_click_on_a_parameter_with_an_action_gives_its_callback_and_the_track() {
        let actions = [0, 0xa0, 0, 0xb0];
        assert_eq!(resolve(&actions, 100, 101, || 5), Some((0xa0, 5)));
        assert_eq!(resolve(&actions, 100, 103, || 15), Some((0xb0, 15)));
    }

    #[test]
    fn a_click_on_a_stock_knob_or_a_parameter_without_an_action_passes_through() {
        let actions = [0, 0xa0, 0, 0xb0];
        let never = || -> u32 { panic!("the track is not read") };
        assert_eq!(resolve(&actions, 100, 100, never), None);
        assert_eq!(resolve(&actions, 100, 0xcd, never), None, "a stock parameter code (SAMP)");
        assert_eq!(resolve(&actions, 100, 0, never), None, "an empty knob");
        assert_eq!(resolve(&actions, 100, -1, never), None);
        assert_eq!(resolve(&[0; 4], 100, 101, never), None, "no module declares an action");
    }

    #[test]
    fn the_action_is_found_by_parameter_row() {
        let actions = [0, 0xa0, 0, 0xb0];
        assert_eq!(find(&actions, 100, 101), 0xa0);
        assert_eq!(find(&actions, 100, 103), 0xb0);
        assert_eq!(find(&actions, 100, 100), 0, "a row without an action: the stock click");
        assert_eq!(find(&actions, 100, 0xcd), 0, "a stock parameter code is below the run-time rows");
        assert_eq!(find(&actions, 100, 104), 0, "past the last row");
        assert_eq!(find(&actions, 100, -1), 0);
        assert_eq!(find(&actions, 100, 0), 0);
    }

    #[test]
    fn the_click_context_keeps_the_view_the_track_and_the_owner_of_the_clicked_row() {
        let owners = [0u8, 3, 3, 5];
        let p = Pending { callback: 0xa0, track: 6, view: 0x2000, code: 101 };
        assert_eq!(context(&p, &owners, 100), Some(Ctx { view: 0x2000, track: 6, owner: 3 }));
        let p = Pending { code: 103, ..p };
        assert_eq!(context(&p, &owners, 100).map(|c| c.owner), Some(5));
    }

    #[test]
    fn a_click_whose_row_was_unloaded_has_no_context() {
        let owners = [0u8, 0, 3];
        let p = Pending { callback: 0xa0, track: 1, view: 0x2000, code: 100 };
        assert_eq!(context(&p, &owners, 100), None, "free row");
        assert_eq!(context(&Pending { code: 50, ..p }, &owners, 100), None, "a stock code");
        assert_eq!(context(&Pending { code: 200, ..p }, &owners, 100), None, "past the table");
    }
}
