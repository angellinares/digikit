//! What the runtime reports: `dkrt_status`, `dkrt_capacity`, `dkrt_diag` and the `dk_debug_*` imports that read the device.

use crate::platform::{self, device};
use crate::platform::st;
use crate::state::{init_once, MAX_MODULES, STAGE_BYTES};
use crate::handlers::services;
use crate::{arena, boot, card, fault, dlog, dsp, settings, speed, store, watch};
use core::ptr::{addr_of, read_volatile};
use dkrt_abi::platform::Platform;
use dkrt_sdk::*;

extern "C" {
    /// Per-device data from the device file (`[digikit] peek`), defined by `src/cf/<dev>/device.s`: the number of ranges and `lo, hi` pairs.
    static dkrt_peek_n: u32;
    static dkrt_peek_ranges: [u32; 8];
    /// The profiling clock of the device file (`[digikit] span_clock`, `span_clock_hz`): its address (0 when absent) and frequency.
    static dkrt_span_clock: u32;
    static dkrt_span_clock_hz: u32;
}

/// Import `dk_debug_speed_flags`: the run-time switches of the speed levers (`speed::update`).
pub(crate) unsafe extern "C" fn debug_speed_flags(clear: u32, set: u32) -> u32 {
    speed::update(clear, set)
}

/// Import `dk_debug_peek`: how many RAM ranges the device file allows PEEK to read; for `i` below that, range `i` as `lo, hi` into `out`.
pub(crate) unsafe extern "C" fn debug_peek(i: u32, out: *mut u32) -> u32 {
    let n = read_volatile(addr_of!(dkrt_peek_n)).min(4);
    if i < n {
        let t = addr_of!(dkrt_peek_ranges) as *const u32;
        *out = read_volatile(t.add(2 * i as usize));
        *out.add(1) = read_volatile(t.add(2 * i as usize + 1));
    }
    n
}

/// Import `dk_debug_span_clock`: the address of the profiling clock (0 when the device has none); its frequency in Hz into `out_hz` when that is not null.
pub(crate) unsafe extern "C" fn debug_span_clock(out_hz: *mut u32) -> u32 {
    let addr = read_volatile(addr_of!(dkrt_span_clock));
    if addr != 0 && !out_hz.is_null() {
        *out_hz = read_volatile(addr_of!(dkrt_span_clock_hz));
    }
    addr
}

/// Import `dk_debug_poll`: the owner and function of poll slot `slot` (`dkrt_poll_add`): 0 when the slot is empty, 1 + the module record, or 0xff for the firmware image.
pub(crate) unsafe extern "C" fn debug_poll(slot: u32, out_fn: *mut u32) -> u32 {
    if slot as usize >= crate::state::POLLS {
        return 0;
    }
    let f = read_volatile(addr_of!(crate::state::DKRT_POLLS[slot as usize]));
    if f == 0 {
        return 0;
    }
    if !out_fn.is_null() {
        *out_fn = f as u32;
    }
    match read_volatile(addr_of!(crate::state::DKRT_POLL_OWNER[slot as usize])) {
        crate::state::POLL_OWNER_RUNTIME => 0xff,
        rec => rec as u32 + 1,
    }
}

/// Import `dk_debug_track_type`: the machine type of a track of the active kit, from the firmware's own accessors (read-only); -1 past track 15 or when the
/// firmware has no type for it, -2 when there is no project yet.
pub(crate) unsafe extern "C" fn debug_track_type(track: u32) -> i32 {
    match crate::PLATFORM.track_type(track) {
        None => -2,
        Some(t) => t,
    }
}



/// Fill `out` (a `DkrtStatus`).
#[no_mangle]
pub unsafe extern "C" fn dkrt_status(out: *mut DkrtStatus) {
    init_once();
    let s = st();
    let (free, largest) = arena::stats();
    let mut n = 0;
    let mut i = 0;
    while i < MAX_MODULES {
        if s.modules[i].used != 0 {
            n += 1;
        }
        i += 1;
    }
    let o = &mut *out;
    o.magic = s.magic;
    o.runtime = s.runtime;
    o.device = device();
    o.arena_base = s.arena_lo;
    o.arena_size = s.arena_hi - s.arena_lo;
    o.arena_free = free;
    o.arena_largest = largest;
    o.modules = n;
    o.ticks = s.ticks;
    o.last_result = s.last_result;
    o.loads = s.loads;
    o.tick_calls = s.tick_calls;
    o.state = dsp::state();
    o.dsp_size = dsp::area_bytes();
    let (dfree, dlargest) = dsp::area_stats();
    o.dsp_free = dfree;
    o.dsp_largest = dlargest;
}

/// The value of capacity kind number `k` (the index in `CAP_KINDS`): `base` is the runtime's own row for it. A loaded service that counts the kind answers for it
/// and owns `used`; else the image's `cap_<kind>` info entry (an `[[info]]` of the image adapter that reserves the capacity, a pointer to a u32 limit word)
/// gives the limit with `used` 0; else `base` (zeros for a kind with no row). The runtime names no service and no adapter: only the generic `cap_<kind>` name.
pub(crate) unsafe fn kind_value<P: Platform>(p: &P, table: &dkrt_abi::service::ServiceTable, k: usize, base: (u8, u32, u32, u32)) -> (u8, u32, u32, u32) {
    use dkrt_sdk::proto::{CAP_CEILING, CAP_INFO_HASHES, CAP_KINDS};
    let kind = CAP_KINDS[k];
    if let Some((limit, used)) = table.cap(kind) {
        return (kind, limit, used, 0);
    }
    let w = p.info(CAP_INFO_HASHES[k]);
    if w.is_null() {
        return base;
    }
    // never more than the consumers' arrays hold (`ceiling` of the kind in sdk.toml)
    (kind, read_volatile(w).min(CAP_CEILING[k]), 0, 0)
}

/// `dkrt_capacity(out, max)`: the entries of the CAPACITY command (one `DkrtCap` per resource, kinds in sdk/sdk.toml `[capacity.kinds]`), at most
/// `max` of them; returns how many it wrote. The SysEx command and the DIGIKIT menu's MEMORY page read the same figures.
#[no_mangle]
pub unsafe extern "C" fn dkrt_capacity(out: *mut DkrtCap, max: u32) -> u32 {
    init_once();
    use dkrt_sdk::proto::*;
    let s = st();
    let (af, al) = arena::stats();
    let (df, dl) = dsp::area_stats();
    let area = dsp::area_bytes();
    let arena = s.arena_hi - s.arena_lo;
    let mut mods = 0;
    let mut i = 0;
    while i < MAX_MODULES {
        if s.modules[i].used != 0 {
            mods += 1;
        }
        i += 1;
    }
    let stage = STAGE_BYTES as u32;
    // the store: its area in KiB (0 when the card is too small for it), and what the headers say is used (a card with no header: nothing)
    let st_total = if card::state() == CARD_TOO_SMALL { 0 } else { store::layout().limit() / 2 };
    let st_used = match store::seen() {
        Some((used, _)) => used,
        None => 0,
    };
    let (set_limit, set_used) = settings::capacity();
    // the DSP load: the headroom and the voices of the device profile; the load is the boot plan's (the module set is fixed per session)
    let (dsp_used, dsp_top) = crate::loader::planned_dsp();
    let pf = platform::profile();
    // the services declare their own counts (machine, voice, parameter and page rows, effect and send slots, map entries), per kind
    let table = services();
    // the values of each kind (a table, not a `match`: a jump table would put data in .text)
    let entries: [(u8, u32, u32, u32); 9] = [
        (CAPK_MODULES, MAX_MODULES as u32, mods, 0),
        (CAPK_SELECTION, dkrt_sdk::SEL_MAX as u32, boot::selected(), 0),
        (CAPK_ARENA, arena, arena - af, al),
        (CAPK_STAGE, stage, 0, stage),
        (CAPK_DSP_AREA, area, area - df, dl),
        (CAPK_STORE, st_total, st_used, 0),
        (CAPK_SETTINGS, set_limit, set_used, 0),
        (CAPK_DSP_COST, pf.dsp_headroom, dsp_used, dsp_top),
        (CAPK_DSP_VOICES, pf.dsp_voices, 0, 0),
    ];
    // one entry per kind of `[capacity.kinds]`, in its order; a kind with no row above reports zeros
    let mut k = 0;
    while k < CAP_KINDS.len() && (k as u32) < max {
        let kind = CAP_KINDS[k];
        let mut v = (kind, 0, 0, 0);
        let mut e = 0;
        while e < entries.len() {
            if entries[e].0 == kind {
                v = entries[e];
            }
            e += 1;
        }
        v = kind_value(&crate::PLATFORM, &table, k, v);
        *out.add(k) = DkrtCap { kind: kind as u32, limit: v.1, used: v.2, largest: v.3 };
        k += 1;
    }
    k as u32
}

/// `dkrt_diag(out)`: what the debug log, the card check and the DSP watch found (a `DkrtDiag`).
#[no_mangle]
pub unsafe extern "C" fn dkrt_diag(out: *mut DkrtDiag) {
    init_once();
    let o = &mut *out;
    let c = card::info();
    let (dsp_ok, hb) = watch::heartbeat();
    let k = fault::seen();
    *o = DkrtDiag {
        magic: DIAG_MAGIC,
        version: 1,
        card_state: c.state,
        card_first_nonblank: c.first_nonblank,
        card_hash: c.hash,
        card_refused: c.refused,
        card_read_errors: c.read_errors,
        debug_on: dlog::sink_present() as u32,
        log_oldest: 0,
        log_newest: 0,
        dsp_diag_ok: dsp_ok as u32,
        dsp_stopped: watch::dsp_stopped() as u32,
        dsp_heartbeat: hb,
        crash_found: k.found,
        crash_pc: k.pc,
        crash_vec: k.vec,
        ram_kept: k.ram_kept,
        dsp_crumb: watch::stop_crumb(),
        fault_rows: 0,
        fault_text: [[0; 24]; FAULT_ROWS],
    };
    // the last fault, kept in the settings store (`dk/fault`): CPU crash or DSP stop, the module, the place
    let mut v = [0u8; dkrt_sdk::fault::BYTES];
    if settings::get(dkrt_sdk::fault::KEY, &mut v) == dkrt_sdk::fault::BYTES as i32 {
        if let Some(f) = dkrt_sdk::fault::Fault::decode(&v) {
            let mut rows = [dkrt_sdk::fault::Row::new(), dkrt_sdk::fault::Row::new(), dkrt_sdk::fault::Row::new(), dkrt_sdk::fault::Row::new()];
            let n = dkrt_sdk::fault::rows(&f, &mut rows);
            let mut k = 0;
            while k < n && k < FAULT_ROWS {
                o.fault_text[k] = rows[k].b;
                k += 1;
            }
            o.fault_rows = k as u32;
        }
    }
}

#[cfg(test)]
mod cap_tests {
    use super::*;
    use crate::fake_platform::FakePlatform;
    use dkrt_abi::profiles;
    use dkrt_sdk::proto::*;

    fn hash(kind: u8) -> u32 {
        CAP_INFO_HASHES[CAP_KINDS.iter().position(|k| *k == kind).unwrap()]
    }

    fn caps(p: &FakePlatform, table: &dkrt_abi::service::ServiceTable) -> std::vec::Vec<DkrtCap> {
        (0..CAP_KINDS.len())
            .map(|k| {
                let v = unsafe { kind_value(p, table, k, (CAP_KINDS[k], 0, 0, 0)) };
                DkrtCap { kind: v.0 as u32, limit: v.1, used: v.2, largest: v.3 }
            })
            .collect()
    }

    fn limits(c: &[DkrtCap]) -> [(u32, u32); 3] {
        let g = |kind: u8| c.iter().find(|e| e.kind == kind as u32).map(|e| (e.limit, e.used)).unwrap();
        [g(CAPK_MACHINE_ROWS), g(CAPK_PARAM_ROWS), g(CAPK_PAGE_ROWS)]
    }

    /// With no machines service loaded the limits are the `cap_<kind>` words of the image's info table, and a module that uses one of each is accepted.
    #[test]
    fn the_image_info_table_gives_the_limits_when_no_service_is_loaded() {
        let _g = crate::test_lock::LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = FakePlatform::new(profiles::DT2);
        for (kind, v) in [(CAPK_MACHINE_ROWS, 8), (CAPK_PARAM_ROWS, 16), (CAPK_PAGE_ROWS, 8)] {
            p.info_entries.borrow_mut().push((hash(kind), std::vec![v]));
            p.info_data.borrow_mut().push(hash(kind));
        }
        let none = dkrt_abi::service::ServiceTable::new(&[], &crate::core_api::REAL_CORE);
        let c = caps(&p, &none);
        assert_eq!(limits(&c), [(8, 0), (16, 0), (8, 0)]);
        // every kind that needs room has it: only the three rows are checked here
        let n = dkrt_fit::Needs { machine_rows: 1, param_rows: 1, page_rows: 1, ..Default::default() };
        let rows: std::vec::Vec<DkrtCap> = c.iter().filter(|e| [CAPK_MACHINE_ROWS, CAPK_PARAM_ROWS, CAPK_PAGE_ROWS].contains(&(e.kind as u8))).copied().collect();
        assert_eq!(dkrt_fit::fits_now(&n, &rows), None, "uses {{1,1,1}} are accepted");
        // an info value over the ceiling of its kind is reported as the ceiling
        p.info_entries.borrow_mut().push((hash(CAPK_FX_SLOTS), std::vec![1000]));
        let c = caps(&p, &none);
        let fx = c.iter().find(|e| e.kind == CAPK_FX_SLOTS as u32).unwrap();
        assert_eq!((fx.limit, fx.used), (CAPMAX_FX_SLOTS, 0));
        assert!(CAPMAX_FX_SLOTS < 1000);
        // without the entries (no adapter reserves them) the limits are 0 and the module is refused
        let empty = FakePlatform::new(profiles::DT2);
        assert_eq!(limits(&caps(&empty, &none)), [(0, 0), (0, 0), (0, 0)]);
    }

    /// A loaded service's answer wins over the info entry and owns `used`.
    #[test]
    fn a_loaded_services_answer_wins_over_the_info_entry() {
        use dkrt_abi::service::{Service, ServiceRow, ServiceTable};
        struct Machines;
        impl<C: crate::core_api::CoreApi> Service<C> for Machines {
            unsafe fn cap(_: &C, kind: u8) -> Option<(u32, u32)> {
                (kind == CAPK_MACHINE_ROWS).then_some((12, 2))
            }
        }
        static ROWS: [ServiceRow; 1] = [ServiceRow::of::<Machines, crate::core_api::RealCore>("machines", &[])];
        static ROWS_P: dkrt_abi::service::RowPtrs<1> = dkrt_abi::service::RowPtrs::of(&ROWS);
        let _g = crate::test_lock::LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = FakePlatform::new(profiles::DT2);
        for (kind, v) in [(CAPK_MACHINE_ROWS, 8), (CAPK_PARAM_ROWS, 16)] {
            p.info_entries.borrow_mut().push((hash(kind), std::vec![v]));
            p.info_data.borrow_mut().push(hash(kind));
        }
        let table = ServiceTable::new(&ROWS_P.0, &crate::core_api::REAL_CORE);
        let c = caps(&p, &table);
        let l = limits(&c);
        assert_eq!(l[0], (12, 2), "the service's answer wins and owns used");
        assert_eq!(l[1], (16, 0), "a kind the service does not count comes from the info entry");
    }
}
