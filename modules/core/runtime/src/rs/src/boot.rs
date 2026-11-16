//! Boot-time loading (docs/plans/digikit-runtime.md decision 9, Phase 6; finding 37).
//!
//! `dkrt_boot` runs once per boot from the main-OS task, just before the MainScreenView constructor (`src/cf/tramp.s`
//! `dkrt_boot_hook`): the block read, the DSP and the block-frame interrupt work there and the machine view is not built yet.
//! It reads the selection record, decides about safe mode, raises the boot counter, and loads the selected modules in order
//! through the same loader as ever (a module that fails is skipped and reported; the others load). DSP code is sent with
//! `dl_send` and awaited by polling the state machine here (bounded); what has not finished when the bound is reached continues
//! in the UI-loop hook as before. The boot counter is cleared by `ui_pass` once the session has been stable: `STABLE_BLOCKS` DSP blocks (30 s)
//! after the first UI-loop pass (at the first pass itself in a safe boot, where no module runs). A boot that dies before then counts as bad.
//!
//! The record is the only thing the runtime writes to the card here, and only to its two sectors (`store::Layout::sel_a`/`sel_b`), through
//! `block_write`, which refuses a write when the card's area is not ours (`card.rs`): then the boot counter is not raised either.

use crate::platform::st;
use crate::safe_mode::{key_held, DKRT_STABLE_BLOCKS, LATE_FLOOR};
use crate::selection::{current, update, Choice, REC, REC_COPY, REC_VALID};
use crate::{assign, card, fault, dlog, loader};
use core::ptr::{addr_of, addr_of_mut};
use dkrt_sdk::sel::{self, Record};
use dkrt_sdk::*;

pub use crate::safe_mode::*;
pub use crate::selection::*;


pub(crate) const INFO_MAGIC: u32 = 0x444b_4254; // "DKBT"

#[no_mangle]
pub static mut DKRT_BOOT: DkrtBootInfo = DkrtBootInfo {
    magic: 0,
    version: 1,
    safe: 0,
    reason: 0,
    counter: 0,
    seq: 0,
    copy: 0,
    count: 0,
    loaded: 0,
    ui_reached: 0,
    results: [0; SEL_MAX],
    dsp_wait: 0,
    stable: 0,
    needs: [[0; NEEDS_LEN]; SEL_MAX],
    states: [STATE_SELECTED; SEL_MAX],
    detail: [[0; 2]; SEL_MAX],
};

/// The file name of each module of this boot's selection (the part of its path after the last `/`, up to `NAME_LEN - 1` bytes, NUL padded): the LOADS
/// command reports it with the result, and the record may change after the boot (a SELECT).
pub use dkrt_abi::state::NAME_LEN;
static mut BOOT_NAME: [[u8; NAME_LEN]; SEL_MAX] = [[0; NAME_LEN]; SEL_MAX];

/// The file name of module `k` of this boot's selection (NUL padded, `NAME_LEN` bytes).
pub unsafe fn boot_name(k: usize) -> *const u8 {
    addr_of!(BOOT_NAME[k]) as *const u8
}

/// The states of this boot's modules (`dkrt_abi::state`): a view of `DKRT_BOOT.states` and `.detail`.
pub fn states() -> dkrt_abi::state::BootStates<'static> {
    let b = info();
    dkrt_abi::state::BootStates { states: &mut b.states, detail: &mut b.detail }
}

/// The module records `LOADS` reports, as `dkrt_abi::state::LoadsView`.
pub fn loads_view() -> dkrt_abi::state::LoadsView<'static> {
    let b = info();
    dkrt_abi::state::LoadsView {
        count: b.count as usize,
        results: &b.results,
        states: &b.states,
        detail: &b.detail,
        needs: &b.needs,
        names: unsafe { &*addr_of!(BOOT_NAME) },
    }
}

/// Paths of the selection this boot loaded (for CAPACITY).
pub fn selected() -> u32 {
    info().count
}

pub fn info() -> &'static mut DkrtBootInfo {
    unsafe { &mut *addr_of_mut!(DKRT_BOOT) }
}

/// The boot hook (once per boot, main-OS task).
#[no_mangle]
pub unsafe extern "C" fn dkrt_boot() {
    crate::init_once();
    let b = info();
    if b.magic == INFO_MAGIC {
        return; // the constructor ran once already: a second call is a bug, not a second boot
    }
    b.magic = INFO_MAGIC;
    b.version = 1;
    let (rec, copy, unreadable) = match current() {
        Choice::Use { record, copy, .. } => (Some(record), copy, false),
        Choice::Empty => (None, 0, false),
        Choice::Unreadable => (None, 0, true),
    };
    let valid = rec.is_some();
    let mut r = rec.unwrap_or(Record::empty());
    dlog::ev(logk::BOOT_START, dlog::INFO, [RUNTIME_VERSION, crate::device(), r.stable_blocks, r.seq, r.boot_counter]);
    // the card's area: ours, blank or neither (read-only), before anything is written
    card::check();
    // the settings record (the fx routes and send amounts): into RAM before the UI starts
    crate::settings::load();
    // the crash record and the RAM probe of the boot before
    fault::keep_probe();
    if fault::at_boot() {
        let seen = fault::seen();
        if seen.found != 0 && seen.stable != 0 {
            // the crashed boot had already cleared its counter: count it as bad now, and keep that one count until a boot with no crash
            // behind it has run 30 s (`ui_pass`)
            r.boot_counter = r.boot_counter.wrapping_add(1);
            LATE_FLOOR = 1;
        }
    }
    b.seq = if valid { r.seq } else { 0 };
    b.copy = copy;
    b.counter = r.boot_counter;
    b.count = r.count;
    states().begin();
    REC = r;
    REC_VALID = valid;
    REC_COPY = copy;
    if r.stable_blocks != 0 {
        DKRT_STABLE_BLOCKS.0 = r.stable_blocks;
    }
    // safe mode: the record cannot be read, or it asked for it, or the key, or too many boots that did not reach the UI loop
    let reason = if unreadable {
        SAFE_READ
    } else if r.flags & sel::FLAG_SAFE_ONCE != 0 {
        SAFE_FLAG
    } else if key_held() {
        SAFE_KEY
    } else if fault::seen().hang != 0 {
        // the boot before stopped because the DSP stopped (a hang record): one stop is enough, even after a stable boot; the selection stays as it is
        SAFE_DSP
    } else if fault::seen().panic != 0 {
        // the boot before panicked (a panic record): one panic is enough, like a stop
        SAFE_PANIC
    } else if r.boot_counter >= SAFE_AFTER {
        SAFE_COUNTER
    } else {
        SAFE_NONE
    };
    dlog::ev(
        logk::BOOT_RECORD,
        dlog::INFO,
        [r.seq, copy, r.boot_counter, r.count, r.flags],
    );
    if reason != SAFE_NONE {
        b.safe = 1;
        b.reason = reason;
        LATE_FLOOR = 0;
        dlog::ev(logk::SAFE, logk::F_ERROR, [reason, r.boot_counter, r.seq, 0, 0]);
        if reason == SAFE_FLAG {
            // honoured once: clear the flag (the counter is left as it is; the UI loop clears it)
            r.flags &= !sel::FLAG_SAFE_ONCE;
            update(r, copy, valid);
        }
        // a safe boot loads no module: show the safe-mode screen in place of the stock intro (no storage access, built here on the main-OS task)
        crate::intro::show_safe(reason);
        // no provider loads and the installer writes nothing: every hook site stays stock
        loader::set_safe();
        let mut k = 0;
        while k < SEL_MAX {
            b.results[k] = E_SAFE_MODE;
            k += 1;
        }
        states().skip_all(r.count as usize);
        return;
    }
    // raise the counter before anything of a module runs; the first UI-loop pass clears it. On a card the runtime does not write there is no
    // counter, so no safe mode by counter (ABOUT says so).
    if (r.count != 0 || r.boot_counter != 0) && card::writable() {
        r.boot_counter = r.boot_counter.wrapping_add(1);
        update(r, copy, valid);
    }
    // The frames that carry DSP code do not flow yet (they start with the sound engine, later in the task), so a module's CF part
    // goes live now and its DSP part follows over the UI-loop passes (`LOADING` in the report until then).
    // the type numbers of every machine of the selection, before the first load (reads the files; the staging buffer is free)
    assign::plan(&r);
    // the boot plan over the header tables that read gave, with the providers the search found for the hard needs the runtime does not meet: the order to
    // load in, and a module the plan refuses is not loaded
    let mut planned = loader::plan_selection(r.count as usize);
    // the providers load before any module; a module that needs one that did not load is refused before the module stage
    loader::load_providers(&planned);
    loader::settle(&mut planned, r.count as usize);
    let mut k = 0;
    while k < r.count as usize {
        states().planned(k, planned.verdicts[k]);
        k += 1;
    }
    loader::boot(&r, &planned);
    crate::selection::BOOT_IDX = u32::MAX;
    // core/introblank: no module took the intro, so the black intro has nothing to wait for
    crate::intro::end_blank();
    // the sticky map: written once, after the loads, and only when this boot changed it
    if let Some(map) = assign::changed_map() {
        let mut nr = REC;
        nr.map = map;
        update(nr, REC_COPY, REC_VALID);
    }
}

/// The result `res` of the load of selection entry `k` (the file at `path`): the boot record, the log events and the name the LOADS report shows.
pub unsafe fn report_module(k: usize, path: *const u8, res: i32, missing: &[u8; NEEDS_LEN]) {
    let b = info();
    st().last_result = res;
    b.results[k] = res;
    if res == OK {
        b.loaded += 1;
    }
    let p = path;
    let mut at = 0;
    let mut i = 0;
    while i < SEL_PATH && *p.add(i) != 0 {
        if *p.add(i) == b'/' {
            at = i + 1;
        }
        i += 1;
    }
    let bad = if res == OK || res == LOADING { dlog::INFO } else { logk::F_ERROR };
    dlog::ev(
        logk::MOD_LOAD,
        bad,
        [k as u32, res as u32, dlog::word_of(p, at, SEL_PATH), dlog::word_of(p, at + 4, SEL_PATH), dlog::word_of(p, at + 8, SEL_PATH)],
    );
    let mut j = 0;
    while j < NAME_LEN - 1 && at + j < SEL_PATH && *p.add(at + j) != 0 {
        BOOT_NAME[k][j] = *p.add(at + j);
        j += 1;
    }
    if res == E_IMPORT && missing[0] != 0 {
        let m = *missing;
        b.needs[k] = m;
        dlog::ev(
            logk::MOD_NEEDS,
            logk::F_ERROR,
            [k as u32, dlog::word_of(m.as_ptr(), 0, NEEDS_LEN), dlog::word_of(m.as_ptr(), 4, NEEDS_LEN), dlog::word_of(m.as_ptr(), 8, NEEDS_LEN), 0],
        );
    }
}

/// The result `res` of the load of the provider file at the store path `path` (`services/<name>.dks`) that provides the service `hash`, loaded at `base` (0 when
/// it did not load): log only. The event is `MOD_LOAD` with the provider index (`loader::PROVIDER_IDX`, not a selection index), the result, the first four
/// characters of the file name, the service hash and the base; the boot record and `LOADS` do not change.
pub unsafe fn report_provider(path: &[u8], res: i32, hash: u32, base: u32) {
    let mut at = 0;
    let mut i = 0;
    while i < path.len() {
        if path[i] == b'/' {
            at = i + 1;
        }
        i += 1;
    }
    let mut name = [0u8; 4];
    let mut j = 0;
    while j < 4 && at + j < path.len() {
        name[j] = path[at + j];
        j += 1;
    }
    let bad = if res == OK { dlog::INFO } else { logk::F_ERROR };
    dlog::ev(
        logk::MOD_LOAD,
        bad,
        [loader::PROVIDER_IDX, res as u32, dlog::word_of(name.as_ptr(), 0, 4), hash, base],
    );
}

/// The DSP part of the module loaded for selection index `idx` is over with result `r` (called by the DSP loader).
pub unsafe fn report_dsp(idx: u32, r: i32) {
    dlog::ev(logk::MOD_DSP, if r == OK { dlog::INFO } else { logk::F_ERROR }, [idx, r as u32, 0, 0, 0]);
    if (idx as usize) < SEL_MAX {
        let b = info();
        b.results[idx as usize] = r;
        states().dsp_done(idx as usize, r);
        if r == OK {
            b.loaded += 1;
        }
    }
}

pub unsafe fn boot_info(out: *mut DkrtBootInfo) {
    let b = info();
    let o = &mut *out;
    *o = DkrtBootInfo {
        magic: b.magic,
        version: b.version,
        safe: b.safe,
        reason: b.reason,
        counter: b.counter,
        seq: b.seq,
        copy: b.copy,
        count: b.count,
        loaded: b.loaded,
        ui_reached: b.ui_reached,
        results: b.results,
        dsp_wait: b.dsp_wait,
        stable: b.stable,
        needs: b.needs,
        states: b.states,
        detail: b.detail,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK;
    use dkrt_abi::plan::{Reason, Verdict};

    #[test]
    fn the_boot_record_feeds_the_loads_reply_and_the_boot_info_copy() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            *info() = core::mem::zeroed();
            info().count = 2;
            states().begin();
            states().planned(0, Verdict::Accepted);
            states().init(0);
            states().finished(0, OK);
            states().planned(1, Verdict::Refused(Reason::DuplicateKey(7, 99)));
            info().results[1] = E_PLAN_KEY;
            BOOT_NAME[0][..5].copy_from_slice(b"a.dkm");
            let mut out = [0u8; dkrt_abi::state::loads_len(SEL_MAX)];
            let n = dkrt_abi::state::loads_v2(&loads_view(), &mut out);
            assert_eq!(n, dkrt_abi::state::loads_len(2));
            assert_eq!(out[..2], [2, 2]);
            assert_eq!(out[2 + 4], STATE_ACTIVE);
            let e1 = 2 + dkrt_abi::state::LOADS_ENTRY;
            assert_eq!((out[e1 + 4], out[e1 + 11], out[e1 + 15]), (STATE_REFUSED, 7, 99));
            let mut copy: DkrtBootInfo = core::mem::zeroed();
            boot_info(&mut copy);
            assert_eq!((copy.states[..2].to_vec(), copy.detail[1]), (vec![STATE_ACTIVE, STATE_REFUSED], [7, 99]));
        }
    }
}
