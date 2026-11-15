//! The native page calls of the firmware adapter core/machines (`mf_native_*`), behind `CoreApi::native_*`. The symbols are weak: a firmware without the
//! adapter has none, and the calls answer "not available".
#[cfg(not(test))]
use dkrt_sdk::records::KIND_NATIVE_PAGE_V2;

#[cfg(not(test))]
extern "C" {
    fn mf_native_register(kind: u32, title: *const u8, draw: u32, event: u32, bindings: *const u32, words: *const u32, word_count: u32, arg: u32, with_arg: u32) -> i32;
    fn mf_native_register_v2(kind: u32, page: u32, flags: u32, title: *const u8, draw: u32, event: u32, bindings: *const u32, words: *const u32, word_count: u32, arg: u32, with_arg: u32) -> i32;
    fn mf_native_remove(kind: u32);
    fn mf_native_turn(view: *const u8, event: *const u8) -> u32;
}

pub fn available(kind: u32) -> bool {
    #[cfg(test)] { let _ = kind; true }
    #[cfg(not(test))] { if kind == KIND_NATIVE_PAGE_V2 { core::hint::black_box(mf_native_register_v2 as usize) != 0 } else { core::hint::black_box(mf_native_register as usize) != 0 } }
}

#[allow(clippy::too_many_arguments)]
pub unsafe fn register(kind: u32, owner: u32, page: u32, flags: u32, title: *const u8, draw: u32, event: u32, bindings: *const u32, words: *const u32, word_count: u32, arg: u32, with_arg: u32) -> i32 {
    #[cfg(not(test))] {
        if kind == KIND_NATIVE_PAGE_V2 {
            mf_native_register_v2(owner, page, flags, title, draw, event, bindings, words, word_count, arg, with_arg)
        } else {
            mf_native_register(owner, title, draw, event, bindings, words, word_count, arg, with_arg)
        }
    }
    #[cfg(test)] { let _ = (kind, owner, page, flags, title, draw, event, bindings, words, word_count, arg, with_arg); dkrt_sdk::OK }
}

pub unsafe fn remove(kind: u32) {
    #[cfg(not(test))] {
        if core::hint::black_box(mf_native_remove as usize) != 0 { mf_native_remove(kind); }
    }
    #[cfg(test)] { let _ = kind; }
}

pub unsafe fn turn(view: *const u8, event: *const u8) -> u32 {
    #[cfg(not(test))] {
        if core::hint::black_box(mf_native_turn as usize) != 0 { mf_native_turn(view, event) } else { 0 }
    }
    #[cfg(test)] { let _ = (view, event); 0 }
}
