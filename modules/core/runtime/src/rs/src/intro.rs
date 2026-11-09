//! `intro_take` (api 1.9): a module in `dkm_init` replaces the stock boot animation with frames of its own.
//!
//! The firmware's intro tick (30 a second, intro task) reads the current-entry pointer `{callback, count}` each frame and calls
//! `callback(index, count, bitmap)`. It then reads the pointer and the entry's count again and does a signed compare: `count > index` goes on,
//! otherwise the intro moves to the next scene or, with no next scene, the pointer becomes NULL and the intro is over. NULL also means that
//! no intro runs. The intro task has priority 7 and the main-OS task 6; a higher number is more urgent, so the main OS cannot interrupt `callback`,
//! and a callback that never returns starves it until power-off. The main OS waits for the end of the intro before the UI loop starts, so the first UI pass is always after it.
//!
//! `end_blank` (core/introblank): the adapter's own scene table replaces the stock tables with a black one; after the module loads, `dkrt_boot` ends it
//! when nothing else took the intro (count 0 in the adapter's entry; the stock compare then ends the intro at the next tick).
//!
//! `take` (in `dkm_init`) only records the request. The loader's `commit` fills the static entry and publishes it with one aligned write to the
//! pointer once the module's load has finished; a rolled-back module's request is dropped (`discard`), so no published entry points into freed code. `tramp` is the callback: it asks the module for the
//! bitmap of the tick, blits it and counts. The count in the entry is 0x7FFF_FFFF (not 0xFFFF_FFFF: the compare is signed) until the last
//! tick or a NULL frame; then `tramp` writes 0 and the stock compare ends the intro. `tramp` calls no `dk_api` function, no allocator and
//! no logger (`dlog::CURRENT_SRC` is one global and tasks preempt each other). The module's `done` runs once in the UI task at the first
//! UI pass (`ui_pass`), with the module as the log source: the module frees its buffer there, never in `frame`.
//!
//! A fault in `frame` reaches the stock exception handler: `crash::install` runs only from the UI loop, so no runtime handler is set yet.
//! The boot counter stays armed until the stable period of the UI loop, so repeated failed boots end in safe mode (no module loads).
use crate::dlog;
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
#[cfg(not(test))]
use core::sync::atomic::{compiler_fence, Ordering};
use dkrt_sdk::api::{IntroDone, IntroFrame};
use dkrt_sdk::*;

/// The most ticks one animation has (10 s at 30 a second).
pub const TICKS_MAX: u32 = 300;
/// The entry's count while the animation runs: the stock compare is signed, so this is the largest value.
const COUNT_OPEN: u32 = 0x7FFF_FFFF;

/// `{callback, count}` as the firmware's scene table has it.
#[repr(C, align(4))]
struct Entry {
    callback: usize,
    count: u32,
}

static mut ENTRY: Entry = Entry { callback: 0, count: 0 };
static mut FRAME: Option<IntroFrame> = None;
static mut DONE: Option<IntroDone> = None;
static mut CTX: *mut u8 = core::ptr::null_mut();
static mut TICKS: u32 = 0;
static mut TICK: u32 = 0;
/// A fixed frame that `tramp` blits on every tick instead of asking a module (the safe-mode screen). NULL when a module supplies the frames.
#[cfg(not(test))]
static mut STATIC_PIXELS: *const u8 = core::ptr::null();
/// The safe-mode frame, rendered on the main-OS task by `show_safe`; the intro task only reads it.
#[cfg(not(test))]
static mut SAFE_BUF: [u8; crate::safe_screen::FRAME_BYTES] = [0; crate::safe_screen::FRAME_BYTES];
/// Ticks the safe-mode screen shows (4 s at 30 a second).
pub const SAFE_TICKS: u32 = 120;
/// Record index of the module that took the intro.
static mut OWNER: usize = 0;
/// 0 free, 1 requested (`take` in `dkm_init`, not published), 2 committed (pointer written), 3 `done` has run (or there was none).
static mut STATE: u32 = 0;
const REQUESTED: u32 = 1;
const COMMITTED: u32 = 2;
const FINISHED: u32 = 3;

/// A request is published or dropped only by the record that made it, and only while it is still a request.
pub fn owns_request(state: u32, owner: usize, rec: usize) -> bool {
    state == REQUESTED && owner == rec
}

/// The result of `intro_take` before anything is written: `E_NOT_FOUND` outside `dkm_init`, `E_CONTRACT` for no `frame` or `ticks` outside
/// 1 to `TICKS_MAX`, `E_BUSY` when an intro is taken already, `E_BOOT_ONLY` when the firmware's intro pointer is NULL (the intro is over).
pub fn check(in_init: bool, taken: bool, has_frame: bool, ticks: u32, intro_running: bool) -> i32 {
    if !in_init {
        E_NOT_FOUND
    } else if !has_frame || ticks == 0 || ticks > TICKS_MAX {
        E_CONTRACT
    } else if taken {
        E_BUSY
    } else if !intro_running {
        E_BOOT_ONLY
    } else {
        OK
    }
}

#[cfg(not(test))]
const MASK: [u32; 256] = [0xFFFF_FFFF; 256];

/// The firmware's Bitmap as `blit` reads it (28 bytes): width +4, height +8, bands +0xc, pixels +0x10, mask +0x14.
#[cfg(not(test))]
#[repr(C)]
struct BitmapView {
    vptr: u32,
    width: u32,
    height: u32,
    bands: u32,
    pixels: *const u8,
    mask: *const u32,
    owns: u8,
    pad: [u8; 3],
}

/// The intro's callback, on the intro task: ColdFire C ABI, arguments on the stack. The stock index is ignored.
#[cfg(not(test))]
unsafe extern "C" fn tramp(_index: u32, _count: u32, bitmap: *mut u8) {
    let tick = read_volatile(addr_of!(TICK));
    let frame = read_volatile(addr_of!(FRAME));
    let mut pixels: *const u8 = core::ptr::null();
    if tick < read_volatile(addr_of!(TICKS)) {
        let fixed = read_volatile(addr_of!(STATIC_PIXELS));
        if !fixed.is_null() {
            pixels = fixed;
        } else if let Some(f) = frame {
            pixels = f(tick, read_volatile(addr_of!(CTX)));
        }
    }
    if pixels.is_null() {
        // the last frame stays on the screen
        write_volatile(addr_of_mut!(ENTRY.count), 0);
        return;
    }
    // field stores, so that the compiler adds no memcpy or template
    let mut view = core::mem::MaybeUninit::<BitmapView>::uninit();
    let v = view.as_mut_ptr();
    write_volatile(addr_of_mut!((*v).vptr), 0);
    write_volatile(addr_of_mut!((*v).width), 128);
    write_volatile(addr_of_mut!((*v).height), 64);
    write_volatile(addr_of_mut!((*v).bands), 2);
    write_volatile(addr_of_mut!((*v).pixels), pixels);
    write_volatile(addr_of_mut!((*v).mask), MASK.as_ptr());
    write_volatile(addr_of_mut!((*v).owns), 0);
    write_volatile(addr_of_mut!((*v).pad), [0; 3]);
    dkfw::draw::blit(bitmap, v as *mut u8, 0, 0, 0);
    let next = tick + 1;
    write_volatile(addr_of_mut!(TICK), next);
    if next >= read_volatile(addr_of!(TICKS)) {
        write_volatile(addr_of_mut!(ENTRY.count), 0);
    }
}

/// `intro_take`: see the module comment and sdk/sdk.toml. Only inside `dkm_init`. It records the request and writes nothing the firmware reads:
/// the pointer is written by `commit` when the module's load has finished (a module that is rolled back after `dkm_init` must not be reachable).
#[cfg(not(test))]
pub unsafe fn take(frame: Option<IntroFrame>, done: Option<IntroDone>, ctx: *mut u8, ticks: u32) -> i32 {
    let source = dlog::current_source();
    let in_init = source != 0 && crate::files::in_init(source as usize - 1);
    let r = check(in_init, read_volatile(addr_of!(STATE)) != 0, frame.is_some(), ticks, intro_running());
    if r != OK {
        return r;
    }
    write_volatile(addr_of_mut!(FRAME), frame);
    write_volatile(addr_of_mut!(DONE), done);
    write_volatile(addr_of_mut!(CTX), ctx);
    write_volatile(addr_of_mut!(TICKS), ticks);
    write_volatile(addr_of_mut!(TICK), 0);
    write_volatile(addr_of_mut!(OWNER), source as usize - 1);
    write_volatile(addr_of_mut!(STATE), REQUESTED);
    OK
}

/// Is the firmware's intro still running (its current-entry pointer is not NULL)?
#[cfg(not(test))]
unsafe fn intro_running() -> bool {
    read_volatile(addr_of!(dkfw::rtos::INTRO_ENTRY) as *const u32) != 0
}

/// The one place that makes the stock intro run an entry of ours: fill the static entry (`callback`, open count), then publish it with one
/// aligned write to the firmware's pointer. A NULL pointer (the intro is over) stays NULL and nothing is written: the result is false.
/// Every source of frames (a module after its load, the safe-mode screen, later a blank intro) installs through this and ends through `tramp`.
#[cfg(not(test))]
unsafe fn install(callback: usize) -> bool {
    let ptr = addr_of!(dkfw::rtos::INTRO_ENTRY) as *mut u32;
    if read_volatile(ptr) == 0 {
        return false;
    }
    write_volatile(addr_of_mut!(ENTRY.callback), callback);
    write_volatile(addr_of_mut!(ENTRY.count), COUNT_OPEN);
    compiler_fence(Ordering::SeqCst);
    write_volatile(ptr, addr_of!(ENTRY) as u32);
    compiler_fence(Ordering::SeqCst);
    true
}

/// The loader, when module `rec` is fully loaded (`dkm_init` returned 0 and its slots are in): publish its request, if it has one. The intro may
/// have ended since `take`: then nothing is published (the request stays, so `done` still runs at the first UI pass).
#[cfg(not(test))]
pub unsafe fn commit(rec: usize) {
    if !owns_request(read_volatile(addr_of!(STATE)), read_volatile(addr_of!(OWNER)), rec) {
        return;
    }
    if install(tramp as usize) {
        write_volatile(addr_of_mut!(STATE), COMMITTED);
    }
}

/// `dkrt_boot` in a safe boot (no module loads, so nothing else takes the intro): replace the running stock intro with a fixed frame for
/// `SAFE_TICKS` ticks. The frame is rendered here, on the main-OS task, before the pointer is published; the intro task only blits it. Does nothing
/// when the intro is over already.
#[cfg(not(test))]
pub unsafe fn show_safe(reason: u32) {
    if !intro_running() {
        return;
    }
    let buf = &mut *addr_of_mut!(SAFE_BUF);
    crate::safe_screen::render(reason, buf);
    write_volatile(addr_of_mut!(STATIC_PIXELS), buf.as_ptr());
    write_volatile(addr_of_mut!(FRAME), None);
    write_volatile(addr_of_mut!(DONE), None);
    write_volatile(addr_of_mut!(TICKS), SAFE_TICKS);
    write_volatile(addr_of_mut!(TICK), 0);
    write_volatile(addr_of_mut!(OWNER), usize::MAX);
    if install(tramp as usize) {
        write_volatile(addr_of_mut!(STATE), COMMITTED);
    }
}

#[cfg(not(test))]
extern "C" {
    /// Data symbol of core/introblank (its scene table, `{callback, count}` and a terminator); resolves to 0 when the module is not in the image
    /// (`[object] optional`).
    fn introblank_table();
}

/// `dkrt_boot`, after the module loads: when core/introblank is in the image and nothing took over the intro (no module published a request, no
/// safe screen), end the black intro now: count 0 in the adapter's entry makes the stock compare end it at the next tick (the pointer becomes NULL,
/// the semaphore is posted and the main OS goes on). Without the adapter, or when the current entry is not the adapter's, nothing is written.
#[cfg(not(test))]
pub unsafe fn end_blank() {
    let table = core::hint::black_box(introblank_table as usize) as *mut u32;
    if table.is_null() {
        return;
    }
    let ptr = addr_of!(dkfw::rtos::INTRO_ENTRY) as *const u32;
    if read_volatile(ptr) != table as u32 {
        return;
    }
    write_volatile(table.add(1), 0);
}

/// The loader, when module `rec` is rolled back or unloaded: its request is dropped, and its `done` never runs (the module is gone). A
/// committed intro of `rec` is ended too (count 0) and marked finished. The unload runs in `dsp::step`, after `ui_pass` of the same
/// tick, so this is not reached for a committed intro in practice.
pub unsafe fn discard(rec: usize) {
    if owns_request(read_volatile(addr_of!(STATE)), read_volatile(addr_of!(OWNER)), rec) {
        write_volatile(addr_of_mut!(DONE), None);
        write_volatile(addr_of_mut!(FRAME), None);
        write_volatile(addr_of_mut!(STATE), 0);
    } else if read_volatile(addr_of!(STATE)) == COMMITTED && read_volatile(addr_of!(OWNER)) == rec {
        write_volatile(addr_of_mut!(ENTRY.count), 0);
        write_volatile(addr_of_mut!(DONE), None);
        write_volatile(addr_of_mut!(STATE), FINISHED);
    }
}

#[cfg(test)]
pub unsafe fn take(_: Option<IntroFrame>, _: Option<IntroDone>, _: *mut u8, _: u32) -> i32 {
    E_BOOT_ONLY
}
#[cfg(test)]
pub unsafe fn commit(_: usize) {}
#[cfg(test)]
pub unsafe fn show_safe(_: u32) {}
#[cfg(test)]
pub unsafe fn end_blank() {}

/// Every UI-loop pass: when an intro was taken, run its `done` once, as the taking module. One flag test when none was taken.
pub unsafe fn ui_pass() {
    // REQUESTED here: the intro ended before `commit`, which left the request unpublished
    let state = read_volatile(addr_of!(STATE));
    if state != COMMITTED && state != REQUESTED {
        return;
    }
    write_volatile(addr_of_mut!(STATE), FINISHED);
    if let Some(done) = read_volatile(addr_of!(DONE)) {
        let prev = dlog::enter_module(read_volatile(addr_of!(OWNER)));
        done(read_volatile(addr_of!(CTX)));
        dlog::leave_module(prev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_orders_the_refusals() {
        assert_eq!(check(true, false, true, 1, true), OK);
        assert_eq!(check(true, false, true, TICKS_MAX, true), OK);
        assert_eq!(check(false, false, true, 10, true), E_NOT_FOUND);
        assert_eq!(check(true, false, false, 10, true), E_CONTRACT);
        assert_eq!(check(true, false, true, 0, true), E_CONTRACT);
        assert_eq!(check(true, false, true, TICKS_MAX + 1, true), E_CONTRACT);
        assert_eq!(check(true, true, true, 10, true), E_BUSY);
        assert_eq!(check(true, false, true, 10, false), E_BOOT_ONLY);
    }

    #[test]
    fn only_the_owner_publishes_or_drops_a_request() {
        assert!(owns_request(REQUESTED, 3, 3));
        assert!(!owns_request(REQUESTED, 3, 4));
        assert!(!owns_request(0, 3, 3));
        assert!(!owns_request(COMMITTED, 3, 3));
        assert!(!owns_request(FINISHED, 3, 3));
    }
}
