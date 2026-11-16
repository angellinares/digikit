//! The firmware calls of the machines service file: `TableFw`, the firmware's parameter page as the service reaches it (`dkrt_abi::fw::Fw`; the page's vtable slots as
//! `RuntimeFw` did, the same offsets, and the three firmware functions through the table `MF_TAB`, the image's `machines_info`), and the entries that use it: the page's key and
//! turn (called by the hooks), and the exports `dk_param_get` and `dk_param_set`. A zero word is a function the image did not give: the call is a no-op with the neutral answer.
use crate::image::{w, MF_TAB};
use core::ptr::{addr_of, read_volatile};
use dkrt_abi::fw::{Fw, MODE_KEYS};
use dkrt_svc_machines::slots;
use dkrt_svc_rt::ImportCore;

pub struct TableFw(pub *const u32);

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

impl TableFw {
    fn at(&self, i: usize) -> usize {
        unsafe { read_volatile(self.0.add(i)) as usize }
    }
}

impl Fw for TableFw {
    fn mode_key_held(&self) -> bool {
        let f = self.at(w::KEY_DOWN);
        if f == 0 {
            return false;
        }
        let f: unsafe extern "C" fn(u32) -> u32 = unsafe { core::mem::transmute(f) };
        MODE_KEYS.iter().any(|&k| unsafe { f(k) } != 0)
    }
    fn page_value(&self, view: usize, code: u32) -> i32 {
        unsafe {
            let f: unsafe extern "C" fn(usize, u32, i32) -> i32 =
                core::mem::transmute(vcall(view, VT_PAGE_VALUE));
            f(view, code, -1)
        }
    }
    fn object(&self, view: usize, code: u32, track: u32) -> usize {
        unsafe {
            let f: unsafe extern "C" fn(usize, u32, u32) -> usize =
                core::mem::transmute(vcall(view, VT_PAGE_OBJECT));
            f(view, code, track)
        }
    }
    fn object_value(&self, obj: usize, code: u32) -> i32 {
        unsafe {
            let f: unsafe extern "C" fn(usize, u32) -> i32 =
                core::mem::transmute(vcall(obj, VT_OBJECT_VALUE));
            f(obj, code)
        }
    }
    fn store(&self, obj: usize, code: u32, raw: i32, track: u32) {
        let f = self.at(w::SET_VALUE);
        if f == 0 {
            return;
        }
        unsafe {
            let f: unsafe extern "C" fn(*const u8, u32, i32, u32, u32, u32, u32) -> u32 =
                core::mem::transmute(f);
            f(obj as *const u8, code, raw, track, 1, 1, 1);
        }
    }
    fn view_track(&self, track_ref: usize) -> u32 {
        let f = self.at(w::VIEW_TRACK);
        if f == 0 {
            return 0;
        }
        unsafe {
            let f: unsafe extern "C" fn(*const u8) -> u32 = core::mem::transmute(f);
            f(track_ref as *const u8)
        }
    }
    fn param_code(&self, view: usize, enc: u32) -> i32 {
        unsafe {
            let f: unsafe extern "C" fn(usize, u32) -> i32 =
                core::mem::transmute(vcall(view, VT_PARAM_CODE));
            f(view, enc)
        }
    }
}

fn tab() -> TableFw {
    TableFw(addr_of!(MF_TAB) as *const u32)
}

/// The hook entry of the machine page's key event: the click detector of the service.
#[no_mangle]
pub unsafe extern "C" fn dks_page_key(view: *const u8, ev: *const u8) {
    slots::page_key(&ImportCore, &tab(), view, ev)
}

/// The hook entry of the machine page's turn: nonzero when a native page consumed it.
#[no_mangle]
pub unsafe extern "C" fn dks_page_turn(view: *const u8, ev: *const u8) -> u32 {
    slots::page_turn(&ImportCore, view, ev)
}

/// `dk_param_get` (an `[[export]]` of the file): the value of a parameter word in the flow of a click.
#[cfg_attr(target_arch = "m68k", no_mangle)]
pub unsafe extern "C" fn dk_param_get(track: u32, word: u32) -> i32 {
    slots::param_get(&ImportCore, &tab(), track, word)
}

/// `dk_param_set`, as `dk_param_get`.
#[cfg_attr(target_arch = "m68k", no_mangle)]
pub unsafe extern "C" fn dk_param_set(track: u32, word: u32, raw: i32) -> i32 {
    slots::param_set(&ImportCore, &tab(), track, word, raw)
}
