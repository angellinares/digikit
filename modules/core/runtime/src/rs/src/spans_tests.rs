//! The span producers (`entry::tick_all`, `tick::phase`, `tick::poll_all`) over a fake clock and a fake emit: nothing is read or emitted without a span sink
//! and a clock, and a span carries the owner of its producer and the clock's difference (a wrap included). The sink channel is global state: the tests
//! share the lock of `cleanup_tests`.
use crate::test_lock::LOCK;
use crate::dlog::{self, Span, SINK_SPAN};
use crate::entry::{self, Ctx};
use crate::state::{DKRT_POLLS, POLLS};
use crate::tick::{phase, poll_all};
use core::ptr::addr_of_mut;
use dkrt_sdk::OK;
use std::vec::Vec;

/// The fake clock: each read returns `NOW` and then adds `STEP`, so a span of one read pair is `STEP` cycles whatever the measured code does.
static mut NOW: u32 = 0;
static mut STEP: u32 = 0;
static mut CLOCK_CALLS: u32 = 0;
/// What the fake emit got, as `(owner, cycles)`.
static mut EMITS: Vec<(u32, u32)> = Vec::new();
/// How many times the fake module tick ran.
static mut TICKED: u32 = 0;

unsafe extern "C" fn fake_clock() -> u32 {
    CLOCK_CALLS += 1;
    let v = NOW;
    NOW = NOW.wrapping_add(STEP);
    v
}

unsafe extern "C" fn fake_emit(s: *const Span) {
    (*addr_of_mut!(EMITS)).push(((*s).owner, (*s).cycles));
}

unsafe extern "C" fn fake_poll() {}

unsafe extern "C" fn fake_tick(_: *const Ctx) {
    TICKED += 1;
}

fn p(f: usize) -> *mut u8 {
    f as *mut u8
}

/// Start a case: the clock at `now` stepping by `step`, nothing recorded, no sink, no poll and no tick.
unsafe fn reset(now: u32, step: u32) {
    NOW = now;
    STEP = step;
    CLOCK_CALLS = 0;
    TICKED = 0;
    (*addr_of_mut!(EMITS)).clear();
    *crate::st() = core::mem::zeroed();
    let mut i = 0;
    while i < POLLS {
        (*addr_of_mut!(DKRT_POLLS))[i] = 0;
        i += 1;
    }
    let mut rec = 0;
    while rec < crate::state::MAX_MODULES {
        entry::publish_tick(rec, 0);
        rec += 1;
    }
    // A test before may have left the channel owned by another module (E_BUSY): clear it, not ask.
    dlog::test_reset_sinks();
}

/// Run `f` as code of module 0 (a sink is set from a module's code).
unsafe fn in_module<R>(f: impl FnOnce() -> R) -> R {
    let prev = dlog::enter_module(0, dlog::PHASE_INIT);
    let r = f();
    dlog::leave_module(prev);
    r
}

fn emits() -> Vec<(u32, u32)> {
    unsafe { (*addr_of_mut!(EMITS)).clone() }
}

#[test]
fn no_sink_means_no_clock_read_and_no_span() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        reset(0, 5);
        (*addr_of_mut!(DKRT_POLLS))[0] = fake_poll as *const () as usize;
        phase(0);
        poll_all();
        entry::tick_all();
        assert_eq!(CLOCK_CALLS, 0);
        assert!(emits().is_empty());
        assert_eq!(dlog::span_t0(), None);
    }
}

#[test]
fn an_emit_without_a_clock_measures_nothing() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        reset(0, 5);
        assert_eq!(in_module(|| dlog::dk_sink_set(SINK_SPAN, p(fake_emit as *const () as usize))), OK);
        (*addr_of_mut!(DKRT_POLLS))[0] = fake_poll as *const () as usize;
        phase(0);
        poll_all();
        entry::tick_all();
        assert_eq!(CLOCK_CALLS, 0);
        assert!(emits().is_empty());
        assert_eq!(dlog::span_t0(), None);
        in_module(|| dlog::dk_sink_set(SINK_SPAN, core::ptr::null_mut()));
    }
}

/// The span of a module tick (`entry::tick_all` measures the call with the record index, the emit after `leave_module`): one span per call with the owner and the clock's
/// difference, a wrap of the counter included. The 32-bit `DKRT_TICKS` table cannot hold a fake's address on a 64-bit host, so `tick_all` itself is not run here.
#[test]
fn spanned_emits_one_span_per_call_with_its_owner() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        reset(u32::MAX - 2, 10);
        assert_eq!(in_module(|| dlog::dk_span_sink_set(p(fake_emit as *const () as usize), p(fake_clock as *const () as usize))), OK);
        let mut n = 0;
        for rec in [0u32, 2, 5] {
            let r = dlog::spanned(rec, || {
                fake_tick(core::ptr::null());
                7
            });
            assert_eq!(r, 7);
            n += 1;
            assert_eq!(TICKED, n);
            assert_eq!(CLOCK_CALLS, 2 * n);
            assert_eq!(emits().len(), n as usize);
            assert_eq!(emits().last().copied(), Some((rec, 10)));
        }
        // the first span wrapped the counter, the later ones did not
        assert_eq!(emits(), [(0, 10), (2, 10), (5, 10)]);
        in_module(|| dlog::dk_span_sink_set(core::ptr::null_mut(), core::ptr::null_mut()));
        let before = CLOCK_CALLS;
        dlog::spanned(9, || fake_tick(core::ptr::null()));
        assert_eq!(CLOCK_CALLS, before);
        assert_eq!(emits().len(), 3);
    }
}

#[test]
fn a_phase_and_each_poll_carry_their_own_owner() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        reset(u32::MAX - 2, 10);
        assert_eq!(in_module(|| dlog::dk_span_sink_set(p(fake_emit as *const () as usize), p(fake_clock as *const () as usize))), OK);
        phase(1);
        // the counter wrapped between the two reads: the difference is still the step
        assert_eq!(emits(), [(0x101, 10)]);
        (*addr_of_mut!(DKRT_POLLS))[0] = fake_poll as *const () as usize;
        (*addr_of_mut!(DKRT_POLLS))[1] = fake_poll as *const () as usize;
        poll_all();
        assert_eq!(emits(), [(0x101, 10), (0x200, 10), (0x201, 10)]);
        assert_eq!(CLOCK_CALLS, 6);
        in_module(|| dlog::dk_span_sink_set(core::ptr::null_mut(), core::ptr::null_mut()));
        (*addr_of_mut!(DKRT_POLLS))[0] = 0;
        (*addr_of_mut!(DKRT_POLLS))[1] = 0;
    }
}

#[test]
fn releasing_the_sinks_clears_the_clock() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        reset(0, 5);
        assert_eq!(in_module(|| dlog::dk_span_sink_set(p(fake_emit as *const () as usize), p(fake_clock as *const () as usize))), OK);
        assert!(dlog::span_t0().is_some());
        dlog::release_sinks(0);
        assert_eq!(dlog::span_t0(), None);
        // an emit set again without a clock does not measure: the clock is gone, not only the channel
        assert_eq!(in_module(|| dlog::dk_sink_set(SINK_SPAN, p(fake_emit as *const () as usize))), OK);
        let calls = CLOCK_CALLS;
        assert_eq!(dlog::span_t0(), None);
        assert_eq!(CLOCK_CALLS, calls);
        in_module(|| dlog::dk_sink_set(SINK_SPAN, core::ptr::null_mut()));
    }
}

#[test]
fn dk_sink_set_on_the_span_channel_clears_the_clock() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        reset(0, 5);
        assert_eq!(in_module(|| dlog::dk_span_sink_set(p(fake_emit as *const () as usize), p(fake_clock as *const () as usize))), OK);
        assert!(dlog::span_t0().is_some());
        assert_eq!(in_module(|| dlog::dk_sink_set(SINK_SPAN, core::ptr::null_mut())), OK);
        let calls = CLOCK_CALLS;
        assert_eq!(dlog::span_t0(), None);
        (*addr_of_mut!(DKRT_POLLS))[0] = fake_poll as *const () as usize;
        phase(0);
        poll_all();
        entry::tick_all();
        assert_eq!(CLOCK_CALLS, calls);
        assert!(emits().is_empty());
        // The clock is gone too: a sink set again through the old API still measures nothing.
        assert_eq!(in_module(|| dlog::dk_sink_set(SINK_SPAN, p(fake_emit as *const () as usize))), OK);
        assert_eq!(dlog::span_t0(), None);
        in_module(|| dlog::dk_sink_set(SINK_SPAN, core::ptr::null_mut()));
    }
}

/// `dkrt_poll_add` records the owner of a slot: the module record whose loaded range holds the function, the runtime marker for any other address.
#[test]
fn a_poll_slot_records_the_module_that_registered_it() {
    let _g = LOCK.lock().unwrap();
    unsafe {
        reset(0, 5);
        let s = crate::st();
        s.modules[1].used = 1;
        s.modules[1].base = 0x1000;
        s.modules[1].size = 0x100;
        s.modules[3].used = 1;
        s.modules[3].base = 0x2000;
        s.modules[3].size = 0x100;
        assert_eq!(crate::dkrt_poll_add(0x1010), OK);
        assert_eq!(crate::dkrt_poll_add(0x9000), OK);
        assert_eq!(crate::dkrt_poll_add(0x20ff), OK);
        let mut f = 0u32;
        assert_eq!(crate::report::debug_poll(0, &mut f), 2, "record 1 gives 2");
        assert_eq!(f, 0x1010);
        assert_eq!(crate::report::debug_poll(1, &mut f), 0xff, "outside every module: the runtime marker");
        assert_eq!(f, 0x9000);
        assert_eq!(crate::report::debug_poll(2, &mut f), 4);
        assert_eq!(f, 0x20ff);
        assert_eq!(crate::report::debug_poll(3, &mut f), 0, "an empty slot");
        assert_eq!(crate::report::debug_poll(4, core::ptr::null_mut()), 0, "past the table");
        // adding a function that is in the table keeps its slot and owner
        assert_eq!(crate::dkrt_poll_add(0x1010), OK);
        assert_eq!(crate::report::debug_poll(0, core::ptr::null_mut()), 2);
        // a module that is released takes its polls with it; the others stay
        crate::api::poll_drop_range(0x1000, 0x100);
        assert_eq!(crate::report::debug_poll(0, &mut f), 0);
        assert_eq!(crate::report::debug_poll(2, &mut f), 4);
        // removing one frees the slot; a new function reuses it with its own owner
        crate::dkrt_poll_remove(0x9000);
        assert_eq!(crate::report::debug_poll(1, &mut f), 0);
        assert_eq!(crate::dkrt_poll_add(0x1020), OK);
        assert_eq!(crate::report::debug_poll(0, &mut f), 2);
        assert_eq!(f, 0x1020);
        reset(0, 5);
    }
}
