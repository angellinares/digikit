//! What the runtime reports: `dkrt_status`, `dkrt_capacity`, `dkrt_diag` and the `dk_debug_*` imports that read the device.

use crate::platform::{self, device};
use crate::plan;
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
    let (dsp_used, dsp_top) = plan::planned_dsp();
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
        if let Some((limit, used)) = table.cap(kind) {
            v = (kind, limit, used, 0);
        }
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
