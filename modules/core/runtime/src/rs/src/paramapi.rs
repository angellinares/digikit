//! Read and write a digikit parameter from the flow of a click (`Api.param_get`, `Api.param_set`; docs/design/ui-rules.md).
//!
//! The firmware access goes through the `Fw` trait, so the rules are host-tested with a fake page: the call must come from the flow a click
//! started (the callback, or a list it opened: `Held`), the word must belong to the module whose callback runs, the track must be 0 to 15, and
//! nothing is written while a mode key is held. The real `Fw` (pageclick.rs) calls the page's own functions, the same ones a knob turn uses.

/// Called outside the flow of a click (the page the call needs is not known, or it got input since).
pub const E_STATE: i32 = -0x10001;
/// A track above 15, or a word the calling module does not have.
pub const E_ARG: i32 = -0x10002;
/// A mode key (TRIG, FUNC, ...) is held: the stock page would record a p-lock instead of changing the base value.
pub const E_HELD: i32 = -0x10003;

pub const TRACKS: u32 = 16;
/// Logical key indexes the stock parameter page tests before it writes (FUNC and the trig/track modes).
pub const MODE_KEYS: [u32; 3] = [0x10, 0x11, 0x16];

/// The click that is being served: the clicked view, the page's track and the module that owns the clicked parameter row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ctx {
    pub view: usize,
    pub track: u32,
    pub owner: u8,
}

/// The click flow that may use the calls. It starts when the callback runs and ends at the next key or turn the page gets (a modal list
/// the callback opened takes the keys itself, so the page sees none until the flow is over) and when the module unloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Held(Option<Ctx>);

impl Held {
    pub const fn new() -> Self {
        Held(None)
    }
    pub fn begin(&mut self, ctx: Ctx) {
        self.0 = Some(ctx);
    }
    pub fn input(&mut self) {
        self.0 = None;
    }
    pub fn ctx(&self) -> Option<Ctx> {
        self.0
    }
}

/// What the firmware gives us (one implementation on the device, one fake in the tests).
pub trait Fw {
    /// Any of `MODE_KEYS` is held.
    fn mode_key_held(&self) -> bool;
    /// The page's own value of `code`, raw (the page's track, no p-lock step).
    fn page_value(&self, view: usize, code: u32) -> i32;
    /// The parameter object of `code` for `track` (0: none).
    fn object(&self, view: usize, code: u32, track: u32) -> usize;
    /// The value of `code` in `obj`, raw.
    fn object_value(&self, obj: usize, code: u32) -> i32;
    /// The stock absolute setter.
    fn store(&self, obj: usize, code: u32, raw: i32, track: u32);
}

/// The registry row of `word` of module `owner`: the `word`-th row it owns, in declaration order.
pub fn row_of_word(owners: &[u8], owner: u8, word: u32) -> Option<usize> {
    if owner == 0 {
        return None;
    }
    owners.iter().enumerate().filter(|(_, o)| **o == owner).map(|(k, _)| k).nth(word as usize)
}

fn target(ctx: Option<Ctx>, owners: &[u8], first: u32, track: u32, word: u32) -> Result<(Ctx, u32), i32> {
    let ctx = ctx.ok_or(E_STATE)?;
    if track >= TRACKS {
        return Err(E_ARG);
    }
    let k = row_of_word(owners, ctx.owner, word).ok_or(E_ARG)?;
    Ok((ctx, first + k as u32))
}

/// `param_get`: the raw value (unit * 256) of `word` on `track` (a halfword: -0x8000 to 0x7fff), or a status below -0x8000.
pub fn get(fw: &impl Fw, ctx: Option<Ctx>, owners: &[u8], first: u32, track: u32, word: u32) -> i32 {
    let (ctx, code) = match target(ctx, owners, first, track, word) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let raw = if track == ctx.track {
        fw.page_value(ctx.view, code)
    } else {
        let obj = fw.object(ctx.view, code, track);
        if obj == 0 {
            return E_ARG;
        }
        fw.object_value(obj, code)
    };
    raw as i16 as i32
}

/// `param_set`: the stock setter with `raw` (clamped to the row range by the setter). 0 or a negative status.
pub fn set(fw: &impl Fw, ctx: Option<Ctx>, owners: &[u8], first: u32, track: u32, word: u32, raw: i32) -> i32 {
    let (ctx, code) = match target(ctx, owners, first, track, word) {
        Ok(t) => t,
        Err(e) => return e,
    };
    if fw.mode_key_held() {
        return E_HELD;
    }
    let obj = fw.object(ctx.view, code, track);
    if obj == 0 {
        return E_ARG;
    }
    fw.store(obj, code, raw, track);
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::RefCell;

    #[derive(Default)]
    struct Fake {
        held: bool,
        no_object: bool,
        calls: RefCell<Vec<String>>,
    }

    impl Fw for Fake {
        fn mode_key_held(&self) -> bool {
            self.held
        }
        fn page_value(&self, view: usize, code: u32) -> i32 {
            self.calls.borrow_mut().push(format!("page_value {view:#x} {code}"));
            0x0500
        }
        fn object(&self, view: usize, code: u32, track: u32) -> usize {
            self.calls.borrow_mut().push(format!("object {view:#x} {code} {track}"));
            if self.no_object { 0 } else { 0x7000 }
        }
        fn object_value(&self, obj: usize, code: u32) -> i32 {
            self.calls.borrow_mut().push(format!("object_value {obj:#x} {code}"));
            0xffff_f800u32 as i32 // a negative halfword with garbage above
        }
        fn store(&self, obj: usize, code: u32, raw: i32, track: u32) {
            self.calls.borrow_mut().push(format!("store {obj:#x} {code} {raw} {track}"));
        }
    }

    // rows: 0 and 1 belong to module 2 (words 0, 1), row 2 to module 3, row 3 is free, row 4 is module 2's word 2
    const OWNERS: [u8; 6] = [2, 2, 3, 0, 2, 0];
    const CTX: Option<Ctx> = Some(Ctx { view: 0x1000, track: 4, owner: 2 });
    const FIRST: u32 = 100;

    #[test]
    fn a_word_is_the_nth_row_of_the_owner() {
        assert_eq!(row_of_word(&OWNERS, 2, 0), Some(0));
        assert_eq!(row_of_word(&OWNERS, 2, 1), Some(1));
        assert_eq!(row_of_word(&OWNERS, 2, 2), Some(4));
        assert_eq!(row_of_word(&OWNERS, 2, 3), None);
        assert_eq!(row_of_word(&OWNERS, 3, 0), Some(2));
        assert_eq!(row_of_word(&OWNERS, 0, 0), None, "free rows have no owner");
    }

    #[test]
    fn the_flow_of_a_click_ends_with_the_next_input_of_the_page() {
        let mut h = Held::new();
        let f = Fake::default();
        assert_eq!(get(&f, h.ctx(), &OWNERS, FIRST, 4, 0), E_STATE, "no click yet");
        h.begin(CTX.unwrap());
        // the callback has returned and a list is open: the flow is still ours
        assert_eq!(set(&f, h.ctx(), &OWNERS, FIRST, 4, 0, 0x100), 0);
        h.input();
        assert_eq!(set(&f, h.ctx(), &OWNERS, FIRST, 4, 0, 0x100), E_STATE, "the page got a key");
        assert_eq!(get(&f, h.ctx(), &OWNERS, FIRST, 4, 0), E_STATE);
        // a second click starts again
        h.begin(Ctx { track: 7, ..CTX.unwrap() });
        assert_eq!(get(&f, h.ctx(), &OWNERS, FIRST, 7, 0), 0x0500);
    }

    #[test]
    fn get_and_set_refuse_outside_a_callback() {
        let f = Fake::default();
        assert_eq!(get(&f, None, &OWNERS, FIRST, 4, 0), E_STATE);
        assert_eq!(set(&f, None, &OWNERS, FIRST, 4, 0, 0x100), E_STATE);
        assert!(f.calls.borrow().is_empty());
    }

    #[test]
    fn a_foreign_word_or_a_bad_track_is_refused_without_a_call() {
        let f = Fake::default();
        assert_eq!(get(&f, CTX, &OWNERS, FIRST, 4, 3), E_ARG, "module 2 has three words");
        assert_eq!(set(&f, CTX, &OWNERS, FIRST, 4, 9, 0), E_ARG);
        assert_eq!(get(&f, CTX, &OWNERS, FIRST, 16, 0), E_ARG);
        assert_eq!(set(&f, CTX, &OWNERS, FIRST, 99, 0, 0), E_ARG);
        let other = Some(Ctx { owner: 7, ..CTX.unwrap() });
        assert_eq!(get(&f, other, &OWNERS, FIRST, 4, 0), E_ARG, "a module without rows");
        assert!(f.calls.borrow().is_empty());
    }

    #[test]
    fn get_of_the_page_track_uses_the_page_getter() {
        let f = Fake::default();
        assert_eq!(get(&f, CTX, &OWNERS, FIRST, 4, 1), 0x0500);
        assert_eq!(*f.calls.borrow(), ["page_value 0x1000 101"]);
    }

    #[test]
    fn get_of_another_track_reads_its_object_and_sign_extends() {
        let f = Fake::default();
        assert_eq!(get(&f, CTX, &OWNERS, FIRST, 9, 2), -0x800);
        assert_eq!(*f.calls.borrow(), ["object 0x1000 104 9", "object_value 0x7000 104"]);
        let g = Fake { no_object: true, ..Fake::default() };
        assert_eq!(get(&g, CTX, &OWNERS, FIRST, 9, 0), E_ARG);
    }

    #[test]
    fn set_gets_the_object_of_the_view_and_calls_the_setter_with_the_code_and_track() {
        let f = Fake::default();
        assert_eq!(set(&f, CTX, &OWNERS, FIRST, 4, 0, 5 << 8), 0);
        assert_eq!(*f.calls.borrow(), ["object 0x1000 100 4", "store 0x7000 100 1280 4"]);
        let f = Fake::default();
        assert_eq!(set(&f, CTX, &OWNERS, FIRST, 15, 2, -256), 0);
        assert_eq!(*f.calls.borrow(), ["object 0x1000 104 15", "store 0x7000 104 -256 15"]);
    }

    #[test]
    fn set_refuses_while_a_mode_key_is_held_but_get_still_reads() {
        let f = Fake { held: true, ..Fake::default() };
        assert_eq!(set(&f, CTX, &OWNERS, FIRST, 4, 0, 0x100), E_HELD);
        assert!(f.calls.borrow().is_empty(), "nothing is written");
        assert_eq!(get(&f, CTX, &OWNERS, FIRST, 4, 0), 0x0500);
    }

    #[test]
    fn set_without_an_object_writes_nothing() {
        let f = Fake { no_object: true, ..Fake::default() };
        assert_eq!(set(&f, CTX, &OWNERS, FIRST, 4, 0, 0x100), E_ARG);
        assert!(!f.calls.borrow().iter().any(|c| c.starts_with("store")));
    }
}
