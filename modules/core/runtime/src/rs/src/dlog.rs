//! The runtime's event log (docs/findings/46-digikit-debug.md). Every call site calls `ev`. An event is 32 bytes: a sequence number (from 1; 0 is no
//! event), the firmware's block counter (1500 per second) as the time, a kind (`dkrt_sdk::logk`), flags (bit 0: an error; bits 1 to 3: the level; bits
//! 8 to 15: the source module) and five argument words.
//!
//! The runtime keeps a small ring (`RING` events) for the crash record's tail and for a debug module that loads later: every event until the first UI
//! frame, then boot and error events. A replay to a new sink keeps the runtime's sequence numbers, so a gap (the ring overflowed) shows. One
//! sink pointer (`dk_debug_log_sink_set`, an import of the modules) receives every event before any filter: `digikit/debug` holds the long ring, the level
//! filter and the host commands. Without a sink the other events are dropped (one pointer test).
//!
//! The sink channels (`dk_sink_set`, runtime-and-services 5.11) are the generic form of that mechanism: channel `SINK_LOG` receives the same events as the
//! pointer above, channel `SINK_SPAN` receives `(owner, cycles)` records (`Span`) from `span_emit`. Each channel has one owner, the module whose code set it;
//! a second owner is refused with `E_BUSY`. The channel is cleared after its owner's `fini` (`release_sinks`). With no sink set a producer pays one test.
//!
//! Appends mask interrupts for a few instructions: the UI task, the MIDI task and the crash handler all append.
use crate::entry::Ctx;
use crate::{fw_blocks, irq_mask, irq_restore};
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use dkrt_sdk::*;

/// Events of the small ring.
pub const RING: usize = 32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Ev {
    pub seq: u32,
    pub time: u32,
    pub kind: u16,
    pub flags: u16,
    pub a: [u32; 5],
}
const EMPTY: Ev = Ev { seq: 0, time: 0, kind: 0, flags: 0, a: [0; 5] };

#[no_mangle]
pub static mut DKRT_LOG: [Ev; RING] = [EMPTY; RING];
/// Events appended to the small ring so far (the newest event's sequence number).
#[no_mangle]
pub static mut DKRT_LOG_COUNT: u32 = 0;

/// The one sink (`unsafe extern "C" fn(*const Ev)`, 0: none). It runs with interrupts masked, in whatever task logs: short, no waiting, no logging.
static mut SINK: usize = 0;
/// The sink's module also gets a tick: `unsafe extern "C" fn()`, once per UI pass (0: none), with the source of the module that set it (record index + 1).
static mut TICK: usize = 0;
static mut TICK_SRC: u8 = 0;

/// Sink channel numbers (`dk_sink_set`, <dk/core/sink.h>).
pub const SINK_LOG: u32 = 0;
pub const SINK_SPAN: u32 = 1;
const SINKS: usize = 2;

/// One sink channel: the callback (`unsafe extern "C" fn(*const Record)`, 0: none) and its owner (source index of the module that set it, record index + 1).
#[derive(Clone, Copy)]
struct Chan {
    f: usize,
    owner: u8,
}
static mut CHAN: [Chan; SINKS] = [Chan { f: 0, owner: 0 }; SINKS];
/// The clock of the span channel (`unsafe extern "C" fn() -> u32`, 0: none), set with the emit function by `dk_span_sink_set`. Spans are measured only while the
/// span sink and this clock are both set.
static mut SPAN_CLOCK: usize = 0;

/// Span owner ids of the runtime's own producers (a module's span uses its record index, which is below these).
pub const SPAN_PHASE: u32 = 0x100;
pub const SPAN_POLL: u32 = 0x200;

/// The record of the `span` channel: the owner id the producer gave (opaque to the runtime, `owner_of_row`) and the cycles it measured.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Span {
    pub owner: u32,
    pub cycles: u32,
}

/// The flags of a non-error event at the info level (an error event is `logk::F_ERROR`: level 0).
pub const INFO: u16 = dkrt_abi::core_api::LOG_INFO;

/// The module whose code is running (record index + 1; 0: the runtime).
static mut CURRENT_SRC: u8 = 0;
/// What that module code is: one of the `PHASE_*` values. The crash record keeps it.
static mut CURRENT_PHASE: u8 = PHASE_NONE;

/// No module code is running.
pub const PHASE_NONE: u8 = 0;
/// The module's `init`.
pub const PHASE_INIT: u8 = 1;
/// The module's `tick`.
pub const PHASE_TICK: u8 = 2;
/// The module's `fini`.
pub const PHASE_FINI: u8 = 3;
/// A callback of the module: a host command handler, an I/O or job step, an intro frame, a menu action.
pub const PHASE_CALLBACK: u8 = 4;

/// Module code is about to run in `phase`: events it logs carry its source. Returns the previous source and phase for `leave_module`.
pub fn enter_module(idx: usize, phase: u8) -> u16 {
    unsafe {
        let prev = u16::from(read_volatile(addr_of!(CURRENT_SRC))) | u16::from(read_volatile(addr_of!(CURRENT_PHASE))) << 8;
        write_volatile(addr_of_mut!(CURRENT_SRC), idx as u8 + 1);
        write_volatile(addr_of_mut!(CURRENT_PHASE), phase);
        prev
    }
}

pub fn leave_module(prev: u16) {
    unsafe {
        write_volatile(addr_of_mut!(CURRENT_SRC), prev as u8);
        write_volatile(addr_of_mut!(CURRENT_PHASE), (prev >> 8) as u8);
    }
}

/// The source index of the module whose code runs now (record index + 1): its `init`, `fini`, `tick` or a callback; 0 outside one.
pub fn current_source() -> u8 {
    unsafe { read_volatile(addr_of!(CURRENT_SRC)) }
}

/// What the running module code is (`PHASE_*`), for the crash record.
pub fn current_phase() -> u8 {
    unsafe { read_volatile(addr_of!(CURRENT_PHASE)) }
}

/// Call a module's `init` (record `idx`, context `ctx`) with its source set.
pub unsafe fn call_init(idx: usize, f: unsafe extern "C" fn(*const Ctx) -> i32, ctx: *const Ctx) -> i32 {
    let prev = enter_module(idx, PHASE_INIT);
    let r = f(ctx);
    leave_module(prev);
    if r != OK {
        release_sinks(idx);
    }
    r
}

/// Call a module's `fini` (record `idx`, context `ctx`) with its source set.
pub unsafe fn call_fini(idx: usize, f: unsafe extern "C" fn(*const Ctx), ctx: *const Ctx) {
    let prev = enter_module(idx, PHASE_FINI);
    f(ctx);
    leave_module(prev);
    release_sinks(idx);
}

/// Is a sink set (the debug module is loaded)?
pub fn sink_present() -> bool {
    unsafe { read_volatile(addr_of!(SINK)) != 0 }
}

/// Kinds below 32 that the small ring keeps without the error flag: the boot's own events (and the machine types assigned while modules load, which a debug
/// module that loads after them would otherwise miss).
const BOOT_KINDS: u32 = (1 << logk::BOOT_START)
    | (1 << logk::CARD_CHECK)
    | (1 << logk::SAFE)
    | (1 << logk::BOOT_RECORD)
    | (1 << logk::MOD_LOAD)
    | (1 << logk::MOD_DSP)
    | (1 << logk::MENU)
    | (1 << logk::UI_FIRST)
    | (1 << logk::STABLE)
    | (1 << logk::RAM_KEEP)
    | (1 << logk::DSP_ABSENT)
    | (1 << logk::MACHINE_TYPE_ASSIGNED);

/// Does the small ring keep this event? Until the first UI frame (the boot's end, after every module's `init`) every event, so a debug module that loads
/// after another module sees what that module logged. Then boot events and error events, except the long detail of a replayed crash record and the raw
/// DSP words of a stop (the fault record holds both).
fn keeps(kind: u16, flags: u16) -> bool {
    if flags & logk::F_ERROR != 0 {
        return kind != logk::CRASH_REG && kind != logk::CRASH_STACK && kind != logk::CRASH_TAIL && kind != logk::DSP_DIAG;
    }
    if crate::boot::info().ui_reached == 0 {
        return true;
    }
    kind == logk::SETTINGS_LOAD || kind == logk::FW_READY || (kind < 32 && (BOOT_KINDS >> kind) & 1 != 0)
}

/// Set (or, with 0, clear) the sink and its tick. The events of the small ring, oldest first, are handed to a new sink at once, so it starts with the boot.
/// `dk_debug_log_sink_set(sink, tick)`, an import of the modules (sdk/sdk.toml).
#[no_mangle]
pub unsafe extern "C" fn dk_debug_log_sink_set(f: *mut u8, tick: *mut u8) {
    let f = f as usize;
    write_volatile(addr_of_mut!(TICK_SRC), read_volatile(addr_of!(CURRENT_SRC)));
    write_volatile(addr_of_mut!(TICK), tick as usize);
    let sr = irq_mask();
    if f != 0 {
        let call: unsafe extern "C" fn(*const Ev) = core::mem::transmute(f);
        let (oldest, newest) = range();
        let mut s = oldest;
        while s != 0 && s <= newest {
            let e = (*addr_of!(DKRT_LOG))[(s as usize - 1) % RING];
            if e.seq == s {
                call(&e);
            }
            s += 1;
        }
    }
    write_volatile(addr_of_mut!(SINK), f);
    irq_restore(sr);
    if f != 0 {
        crate::fault::replay_detail();
    }
}

/// `dk_log_event(kind, flags, a)`: the import of a module that logs a kind of its own, and the call of the adapters that link to the runtime. `a` points to five words.
#[no_mangle]
pub unsafe extern "C" fn dk_log_event(kind: u32, flags: u32, a: *const u32) {
    ev(kind as u16, flags as u16, [*a, *a.add(1), *a.add(2), *a.add(3), *a.add(4)]);
}

/// `dk_sink_set(channel, sink)`, an import of the modules (sdk/sdk.toml): set (or, with 0, clear) the sink of `channel` for the module that is running.
/// Returns 0, `E_RANGE` (no such channel), `E_CONTEXT` (no module code is running) or `E_BUSY` (another module owns the channel; it is not changed).
#[no_mangle]
pub unsafe extern "C" fn dk_sink_set(channel: u32, sink: *mut u8) -> i32 {
    if channel as usize >= SINKS {
        return E_RANGE;
    }
    let owner = current_source();
    if owner == 0 {
        return E_CONTEXT;
    }
    let sr = irq_mask();
    let c = &mut (*addr_of_mut!(CHAN))[channel as usize];
    let r = if c.owner != 0 && c.owner != owner {
        E_BUSY
    } else {
        write_volatile(addr_of_mut!(c.owner), if sink.is_null() { 0 } else { owner });
        write_volatile(addr_of_mut!(c.f), sink as usize);
        if channel == SINK_SPAN {
            // The old API leaves no clock behind: a span sink set this way measures nothing.
            write_volatile(addr_of_mut!(SPAN_CLOCK), 0);
        }
        OK
    };
    irq_restore(sr);
    r
}

/// `dk_span_sink_set(emit, clock)`, an import of the modules (sdk/sdk.toml): take the span channel as `dk_sink_set(SINK_SPAN, emit)` does and store the clock
/// with it. `(0, 0)` clears both. Returns 0, `E_CONTEXT` (no module code is running) or `E_BUSY` (another module owns the channel; it is not changed).
#[no_mangle]
pub unsafe extern "C" fn dk_span_sink_set(emit: *mut u8, clock: *mut u8) -> i32 {
    let owner = current_source();
    if owner == 0 {
        return E_CONTEXT;
    }
    let sr = irq_mask();
    let c = &mut (*addr_of_mut!(CHAN))[SINK_SPAN as usize];
    let r = if c.owner != 0 && c.owner != owner {
        E_BUSY
    } else {
        write_volatile(addr_of_mut!(c.owner), if emit.is_null() { 0 } else { owner });
        write_volatile(addr_of_mut!(c.f), emit as usize);
        write_volatile(addr_of_mut!(SPAN_CLOCK), if emit.is_null() { 0 } else { clock as usize });
        OK
    };
    irq_restore(sr);
    r
}

/// Host tests: no sink channel, no old sink and no span clock, whoever a test before left owning them.
#[cfg(test)]
pub(crate) unsafe fn test_reset_sinks() {
    *addr_of_mut!(CHAN) = [Chan { f: 0, owner: 0 }; SINKS];
    *addr_of_mut!(SINK) = 0;
    write_volatile(addr_of_mut!(SPAN_CLOCK), 0);
}

/// The module with record index `idx` is gone (after its `fini`, or a failed `init`): it owns no channel any more.
pub unsafe fn release_sinks(idx: usize) {
    let owner = idx as u8 + 1;
    let sr = irq_mask();
    let mut i = 0;
    while i < SINKS {
        let c = &mut (*addr_of_mut!(CHAN))[i];
        if c.owner == owner {
            write_volatile(addr_of_mut!(c.f), 0);
            write_volatile(addr_of_mut!(c.owner), 0);
            if i == SINK_SPAN as usize {
                write_volatile(addr_of_mut!(SPAN_CLOCK), 0);
            }
        }
        i += 1;
    }
    irq_restore(sr);
}

/// Is a sink set on `channel`? (The one flag test of a producer.)
#[inline]
pub fn sink_on(channel: u32) -> bool {
    unsafe { read_volatile(addr_of!((*addr_of!(CHAN))[channel as usize].f)) != 0 }
}

/// Hand a `(owner, cycles)` record to the `span` sink; nothing happens (one flag test) without one. Runs in the producer's task with interrupts masked.
pub unsafe fn span_emit(owner: u32, cycles: u32) {
    if !sink_on(SINK_SPAN) {
        return;
    }
    let sr = irq_mask();
    let f = read_volatile(addr_of!((*addr_of!(CHAN))[SINK_SPAN as usize].f));
    if f != 0 {
        let call: unsafe extern "C" fn(*const Span) = core::mem::transmute(f);
        call(&Span { owner, cycles });
    }
    irq_restore(sr);
}

/// The start of a measured span: the span clock's value, only when the span sink and the clock are both set (one test of the channel word first). A
/// producer keeps it and calls `span_end` after the measured code.
#[inline(always)]
pub fn span_t0() -> Option<u32> {
    if !sink_on(SINK_SPAN) {
        return None;
    }
    clock_slow()
}

/// The slow path of `span_t0` (a span sink is set): read the clock, or `None` while it is not set.
#[cold]
#[inline(never)]
fn clock_slow() -> Option<u32> {
    unsafe {
        let k = read_volatile(addr_of!(SPAN_CLOCK));
        if k == 0 {
            return None;
        }
        let clock: unsafe extern "C" fn() -> u32 = core::mem::transmute(k);
        Some(clock())
    }
}

/// The end of a span that `span_t0` started: hand `(owner, cycles)` to the span sink, `cycles` the clock's difference (a wrap of the counter is fine).
#[cold]
#[inline(never)]
pub unsafe fn span_end(owner: u32, t0: u32) {
    let k = read_volatile(addr_of!(SPAN_CLOCK));
    if k == 0 {
        return;
    }
    let clock: unsafe extern "C" fn() -> u32 = core::mem::transmute(k);
    span_emit(owner, clock().wrapping_sub(t0));
}

/// Run `f` as a measured span of `owner`: the clock is read before and after only while a span sink and clock are set.
#[inline(always)]
pub unsafe fn spanned<R>(owner: u32, f: impl FnOnce() -> R) -> R {
    let t0 = span_t0();
    let r = f();
    if let Some(t) = t0 {
        span_end(owner, t);
    }
    r
}

/// After a module's `fini`, or when its range is freed: the sink, the tick and the channels may not point into its memory [lo, hi) (a module that forgot to clear them).
pub unsafe fn sink_drop(lo: u32, hi: u32) {
    let sr = irq_mask();
    let mut i = 0;
    while i < SINKS {
        let c = &mut (*addr_of_mut!(CHAN))[i];
        if c.f as u32 >= lo && (c.f as u32) < hi {
            write_volatile(addr_of_mut!(c.f), 0);
            write_volatile(addr_of_mut!(c.owner), 0);
            if i == SINK_SPAN as usize {
                write_volatile(addr_of_mut!(SPAN_CLOCK), 0);
            }
        }
        i += 1;
    }
    let k = read_volatile(addr_of!(SPAN_CLOCK)) as u32;
    if k != 0 && k >= lo && k < hi {
        write_volatile(addr_of_mut!(SPAN_CLOCK), 0);
    }
    let f = read_volatile(addr_of!(SINK)) as u32;
    if f >= lo && f < hi {
        write_volatile(addr_of_mut!(SINK), 0);
    }
    let t = read_volatile(addr_of!(TICK)) as u32;
    if t >= lo && t < hi {
        write_volatile(addr_of_mut!(TICK), 0);
    }
    irq_restore(sr);
}

/// Once per UI pass: the sink module's tick, with that module as the source of what it logs.
pub unsafe fn run_tick() {
    let t = read_volatile(addr_of!(TICK));
    let src = read_volatile(addr_of!(TICK_SRC));
    if t != 0 && src != 0 {
        let call: unsafe extern "C" fn() = core::mem::transmute(t);
        let prev = enter_module(src as usize - 1, PHASE_CALLBACK);
        call();
        leave_module(prev);
    }
}

/// Append an event. The source (flags bits 8 to 15) defaults to the module that is running. The sink gets it first; the small ring keeps it when it is
/// an error or a boot event.
pub unsafe fn ev(kind: u16, flags: u16, a: [u32; 5]) {
    let mut flags = flags;
    if (flags >> logk::SOURCE_SHIFT) & logk::SOURCE_MASK == 0 {
        flags |= (read_volatile(addr_of!(CURRENT_SRC)) as u16) << logk::SOURCE_SHIFT;
    }
    let sink = read_volatile(addr_of!(SINK));
    let keep = keeps(kind, flags);
    if sink == 0 && !keep && !sink_on(SINK_LOG) {
        return;
    }
    let time = fw_blocks().unwrap_or(0);
    let sr = irq_mask();
    let sink = read_volatile(addr_of!(SINK));
    if sink != 0 {
        let call: unsafe extern "C" fn(*const Ev) = core::mem::transmute(sink);
        call(&Ev { seq: 0, time, kind, flags, a });
    }
    let chan = read_volatile(addr_of!((*addr_of!(CHAN))[SINK_LOG as usize].f));
    if chan != 0 {
        let call: unsafe extern "C" fn(*const Ev) = core::mem::transmute(chan);
        call(&Ev { seq: 0, time, kind, flags, a });
    }
    if keep {
        let n = read_volatile(addr_of!(DKRT_LOG_COUNT)).wrapping_add(1);
        let e = &mut (*addr_of_mut!(DKRT_LOG))[(n as usize - 1) % RING];
        e.seq = 0; // a reader that is cut off here sees no event, not a half-written one
        e.time = time;
        e.kind = kind;
        e.flags = flags;
        e.a = a;
        e.seq = n;
        write_volatile(addr_of_mut!(DKRT_LOG_COUNT), n);
    }
    irq_restore(sr);
}

/// The first four bytes of `s` from `at` as a big-endian word (0 past the end).
pub unsafe fn word_of(s: *const u8, at: usize, max: usize) -> u32 {
    let mut v = 0u32;
    let mut i = 0;
    while i < 4 {
        let b = if at + i < max { *s.add(at + i) } else { 0 };
        v = (v << 8) | b as u32;
        i += 1;
    }
    v
}

/// Sequence numbers of the oldest and newest event kept (0, 0: none).
pub fn range() -> (u32, u32) {
    let n = unsafe { read_volatile(addr_of!(DKRT_LOG_COUNT)) };
    if n == 0 {
        return (0, 0);
    }
    let oldest = if n as usize > RING { n - RING as u32 + 1 } else { 1 };
    (oldest, n)
}

/// Copy the event with sequence `seq` to `dst` as 32 big-endian bytes; false if it is no longer (or not yet) in the ring.
pub unsafe fn copy_event(seq: u32, dst: *mut u8) -> bool {
    let (oldest, newest) = range();
    if seq == 0 || seq < oldest || seq > newest {
        return false;
    }
    let sr = irq_mask();
    let e = (*addr_of!(DKRT_LOG))[(seq as usize - 1) % RING];
    irq_restore(sr);
    if e.seq != seq {
        return false;
    }
    crate::wr32(dst, e.seq);
    crate::wr32(dst.add(4), e.time);
    *dst.add(8) = (e.kind >> 8) as u8;
    *dst.add(9) = e.kind as u8;
    *dst.add(10) = (e.flags >> 8) as u8;
    *dst.add(11) = e.flags as u8;
    let mut i = 0;
    while i < 5 {
        crate::wr32(dst.add(12 + 4 * i), e.a[i]);
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK;

    static mut LOGGED: u32 = 0;
    static mut LAST_KIND: u16 = 0;
    static mut SPANS: u32 = 0;
    static mut LAST_SPAN: (u32, u32) = (0, 0);

    unsafe extern "C" fn fake_log(e: *const Ev) {
        LOGGED += 1;
        LAST_KIND = (*e).kind;
    }
    unsafe extern "C" fn fake_span(s: *const Span) {
        SPANS += 1;
        LAST_SPAN = ((*s).owner, (*s).cycles);
    }

    /// Holds the test lock; the channels a test set are cleared when it ends, so the next test of any file starts without an owner.
    struct Fresh(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

    impl Drop for Fresh {
        fn drop(&mut self) {
            unsafe { test_reset_sinks() }
        }
    }

    /// A clean channel table and counters; the guard serializes the tests that share the statics.
    unsafe fn fresh() -> Fresh {
        let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        test_reset_sinks();
        LOGGED = 0;
        SPANS = 0;
        Fresh(g)
    }

    unsafe fn set_as(idx: usize, channel: u32, f: usize) -> i32 {
        let prev = enter_module(idx, PHASE_INIT);
        let r = dk_sink_set(channel, f as *mut u8);
        leave_module(prev);
        r
    }

    #[test]
    fn set_refuse_a_second_owner_and_clear() {
        unsafe {
            let _g = fresh();
            assert_eq!(set_as(2, SINK_LOG, fake_log as *const () as usize), OK);
            assert_eq!(set_as(3, SINK_LOG, fake_span as *const () as usize), E_BUSY);
            assert_eq!(set_as(3, SINK_LOG, 0), E_BUSY, "another module cannot clear it either");
            assert!(sink_on(SINK_LOG));
            // the other channel is free, and the owner may replace or clear its own
            assert_eq!(set_as(3, SINK_SPAN, fake_span as *const () as usize), OK);
            assert_eq!(set_as(2, SINK_LOG, fake_log as *const () as usize), OK);
            assert_eq!(set_as(2, SINK_LOG, 0), OK);
            assert!(!sink_on(SINK_LOG));
            assert_eq!(set_as(3, SINK_LOG, fake_log as *const () as usize), OK);
            assert_eq!(set_as(0, 9, fake_log as *const () as usize), E_RANGE);
            // no module code is running: no owner
            release_sinks(3);
            assert_eq!(dk_sink_set(SINK_LOG, fake_log as *const () as usize as *mut u8), E_CONTEXT);
        }
    }

    #[test]
    fn freeing_a_range_drops_the_channels_that_point_into_it() {
        unsafe {
            let _g = fresh();
            let f = fake_log as *const () as usize;
            assert_eq!(set_as(2, SINK_LOG, f), OK);
            assert_eq!(set_as(3, SINK_SPAN, fake_span as *const () as usize), OK);
            // a range that holds only the log sink: the span sink stays
            sink_drop(f as u32, f as u32 + 1);
            assert!(!sink_on(SINK_LOG));
            assert!(sink_on(SINK_SPAN));
            // its owner can set it again: the owner slot was cleared too
            assert_eq!(set_as(3, SINK_LOG, f), OK);
        }
    }

    #[test]
    fn log_events_reach_the_log_sink_and_the_ring_is_unchanged() {
        unsafe {
            let _g = fresh();
            let before = read_volatile(addr_of!(DKRT_LOG_COUNT));
            ev(logk::CRASH_TAIL, INFO, [0; 5]); // not kept by the ring (no error flag)
            assert_eq!(LOGGED, 0);
            assert_eq!(set_as(1, SINK_LOG, fake_log as *const () as usize), OK);
            ev(logk::SETTINGS_LOAD, INFO, [1, 2, 3, 4, 5]);
            ev(logk::CRASH_TAIL, INFO, [0; 5]);
            assert_eq!(LOGGED, 2);
            assert_eq!(LAST_KIND, logk::CRASH_TAIL);
            // the old sink still works beside the channel
            *addr_of_mut!(SINK) = fake_log as *const () as usize;
            ev(logk::SETTINGS_LOAD, INFO, [0; 5]);
            assert_eq!(LOGGED, 4);
            assert!(read_volatile(addr_of!(DKRT_LOG_COUNT)) >= before);
        }
    }

    #[test]
    fn the_sink_is_cleared_after_fini_and_a_failed_init() {
        unsafe {
            let _g = fresh();
            assert_eq!(set_as(4, SINK_LOG, fake_log as *const () as usize), OK);
            assert_eq!(set_as(4, SINK_SPAN, fake_span as *const () as usize), OK);
            unsafe extern "C" fn fini(_: *const Ctx) {}
            call_fini(4, fini, core::ptr::null());
            assert!(!sink_on(SINK_LOG) && !sink_on(SINK_SPAN));
            ev(logk::SETTINGS_LOAD, INFO, [0; 5]);
            assert_eq!(LOGGED, 0);
            // another module may own it now
            assert_eq!(set_as(5, SINK_LOG, fake_log as *const () as usize), OK);
            unsafe extern "C" fn bad_init(_: *const Ctx) -> i32 { -1 }
            call_init(5, bad_init, core::ptr::null());
            assert!(!sink_on(SINK_LOG));
        }
    }

    #[test]
    fn span_without_a_sink_calls_nothing() {
        unsafe {
            let _g = fresh();
            span_emit(7, 100);
            assert_eq!(SPANS, 0);
            assert_eq!(set_as(1, SINK_SPAN, fake_span as *const () as usize), OK);
            span_emit(7, 100);
            assert_eq!((SPANS, LAST_SPAN), (1, (7, 100)));
            assert_eq!(set_as(1, SINK_SPAN, 0), OK);
            span_emit(8, 5);
            assert_eq!(SPANS, 1);
        }
    }
}
