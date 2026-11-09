//! The runtime's event log (docs/findings/46-digikit-debug.md). Every call site calls `ev`. An event is 32 bytes: a sequence number (from 1; 0 is no
//! event), the firmware's block counter (1500 per second) as the time, a kind (`dkrt_sdk::logk`), flags (bit 0: an error; bits 1 to 3: the level; bits
//! 8 to 15: the source module) and five argument words.
//!
//! The runtime keeps a small ring (`RING` events) for the crash record's tail and for a debug module that loads later: every event until the first UI
//! frame, then boot and error events. A replay to a new sink keeps the runtime's sequence numbers, so a gap (the ring overflowed) shows. One
//! sink pointer (`dkrt_log_sink_set`, an import of the modules) receives every event before any filter: `digikit/debug` holds the long ring, the level
//! filter and the host commands. Without a sink the other events are dropped (one pointer test).
//!
//! Appends mask interrupts for a few instructions: the UI task, the MIDI task and the crash handler all append.
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

/// The flags of a non-error event at the info level (an error event is `logk::F_ERROR`: level 0).
pub const INFO: u16 = logk::L_INFO << logk::LEVEL_SHIFT;

/// The module whose code is running (record index + 1; 0: the runtime).
static mut CURRENT_SRC: u8 = 0;

/// Module code is about to run: events it logs carry its source. Returns the previous source for `leave_module`.
pub fn enter_module(idx: usize) -> u8 {
    unsafe {
        let prev = read_volatile(addr_of!(CURRENT_SRC));
        write_volatile(addr_of_mut!(CURRENT_SRC), idx as u8 + 1);
        prev
    }
}

pub fn leave_module(prev: u8) {
    unsafe { write_volatile(addr_of_mut!(CURRENT_SRC), prev) }
}

/// The source index of the module whose code runs now (record index + 1): its `init`, `fini`, slot 0 or a registered service command; 0 outside one.
pub fn current_source() -> u8 {
    unsafe { read_volatile(addr_of!(CURRENT_SRC)) }
}

/// Call a module's `init` (record `idx`) with its source set.
pub unsafe fn call_init(idx: usize, f: unsafe extern "C" fn(*const Api) -> i32, api: *const Api) -> i32 {
    let prev = enter_module(idx);
    let r = f(api);
    leave_module(prev);
    r
}

/// Call a module's `fini` (record `idx`) with its source set.
pub unsafe fn call_fini(idx: usize, f: unsafe extern "C" fn(*const Api), api: *const Api) {
    let prev = enter_module(idx);
    f(api);
    leave_module(prev);
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
    kind == logk::SETTINGS_LOAD || (kind < 32 && (BOOT_KINDS >> kind) & 1 != 0)
}

/// Set (or, with 0, clear) the sink and its tick. The events of the small ring, oldest first, are handed to a new sink at once, so it starts with the boot.
/// `dkrt_log_sink_set(sink, tick)` of the module imports (sdk/sdk.toml).
#[no_mangle]
pub unsafe extern "C" fn dkrt_log_sink_set(f: *mut u8, tick: *mut u8) {
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
        crate::crash::replay_detail();
    }
}

/// `dkrt_log_ev(kind, flags, a)`: the import of a module that logs a kind of its own. `a` points to five words.
#[no_mangle]
pub unsafe extern "C" fn dkrt_log_ev(kind: u32, flags: u32, a: *const u32) {
    ev(kind as u16, flags as u16, [*a, *a.add(1), *a.add(2), *a.add(3), *a.add(4)]);
}

/// After a module's `fini`: the sink and the tick may not point into its memory [lo, hi) (a module that forgot to clear them).
pub unsafe fn sink_drop(lo: u32, hi: u32) {
    let sr = irq_mask();
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
        let prev = enter_module(src as usize - 1);
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
    if sink == 0 && !keep {
        return;
    }
    let time = fw_blocks().unwrap_or(0);
    let sr = irq_mask();
    let sink = read_volatile(addr_of!(SINK));
    if sink != 0 {
        let call: unsafe extern "C" fn(*const Ev) = core::mem::transmute(sink);
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
