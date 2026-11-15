//! `RuntimeFw`: the runtime's implementation of `dkrt_abi::fw::Fw`, the firmware's parameter page as the machines service sees it: the page's vtable slots,
//! the key state, the stock parameter setter and the view's track. This is the declared adapter of that interface and, with `platform.rs`, the one file of
//! the runtime that names `dkfw` (`check:layers`); the service crates never do.

use core::ptr::read_volatile;
use dkrt_abi::fw::{Fw, MODE_KEYS};

/// The page's own functions, as a knob turn uses them (docs/design/ui-rules.md, "Runtime API for parameter-driven flows").
pub struct RuntimeFw;

/// Vtable slots of the page (`view`) and of its parameter object.
const VT_PAGE_VALUE: usize = 0x54;
const VT_PAGE_OBJECT: usize = 0xc4;
const VT_OBJECT_VALUE: usize = 0x28;
/// The page's `encoder -> parameter code` function: vtable slot 47 (offset 0xbc), `i32 (view, encoder)`.
const VT_PARAM_CODE: usize = 4 * 47;

unsafe fn word(p: usize, off: usize) -> usize {
    read_volatile((p + off) as *const u32) as usize
}

unsafe fn vcall(obj: usize, off: usize) -> usize {
    word(word(obj, 0), off)
}

impl Fw for RuntimeFw {
    fn mode_key_held(&self) -> bool {
        MODE_KEYS.iter().any(|&k| unsafe { dkfw::input::key_down(k) } != 0)
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
    fn view_track(&self, track_ref: usize) -> u32 {
        unsafe { dkfw::page::view_track(track_ref as *const u8) }
    }
    fn param_code(&self, view: usize, enc: u32) -> i32 {
        unsafe {
            let f: unsafe extern "C" fn(usize, u32) -> i32 = core::mem::transmute(vcall(view, VT_PARAM_CODE));
            f(view, enc)
        }
    }
}
