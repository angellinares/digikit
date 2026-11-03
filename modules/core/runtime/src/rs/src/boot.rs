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
use crate::{assign, block_read, block_write, card, crash, dlog, loader, st};
use core::ptr::{addr_of, addr_of_mut, read_volatile};
use dkrt_sdk::sel::{self, Record};
use dkrt_sdk::*;

const INFO_MAGIC: u32 = 0x444b_4254; // "DKBT"

#[repr(C, align(16))]
pub struct Sector([u8; 512]);
#[no_mangle]
pub static mut DKRT_SECBUF: Sector = Sector([0; 512]);

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
};
/// The stable period in DSP blocks (`STABLE_BLOCKS`). A static so that the scenarios can shorten it (they poke it; nothing else writes it).
#[repr(C, align(4))]
pub struct Knob(pub u32); // a ColdFire gcc aligns a u32 to 2: the emulator's poke wants a word
#[no_mangle]
pub static mut DKRT_STABLE_BLOCKS: Knob = Knob(STABLE_BLOCKS);
/// The block count at the first UI-loop pass and the number of passes since (the fallback when the block counter does not move).
#[no_mangle]
pub static mut DKRT_UI_START: Knob = Knob(0);
static mut UI_PASSES: u32 = 0;
/// The boot counter a stable session leaves behind: 1 after a boot that found the crash record of a session that had been stable, else 0.
static mut LATE_FLOOR: u32 = 0;
/// UI-loop passes after which a block counter that has not moved at all is taken for dead and the counter is cleared as in the first
/// pass of old (the UI loop runs 30 to 120 times a second: 1800 passes are 15 s to a minute).
const DEAD_CLOCK_PASSES: u32 = 1800;
/// The record this boot started from (or an empty one) and the bookkeeping the writes need.
static mut REC: Record = Record::empty();
static mut REC_VALID: bool = false;
static mut REC_COPY: u32 = 0;

/// Paths of the selection this boot loaded (for CAPACITY).
pub fn selected() -> u32 {
    info().count
}

pub fn info() -> &'static mut DkrtBootInfo {
    unsafe { &mut *addr_of_mut!(DKRT_BOOT) }
}

fn secbuf() -> *mut u8 {
    unsafe { addr_of_mut!(DKRT_SECBUF) as *mut u8 }
}

/// Read one copy (0 = A, 1 = B).
unsafe fn read_copy(i: u32) -> Option<Record> {
    let sector = if i == 0 { crate::store::layout().sel_a() } else { crate::store::layout().sel_b() };
    if block_read(sector, 512, secbuf()) != 0 {
        return None;
    }
    let b = &*(secbuf() as *const [u8; 512]);
    sel::decode(b)
}

/// The record in use: the valid copy with the higher sequence number (A when equal).
unsafe fn current() -> (Option<Record>, u32) {
    let a = read_copy(0);
    let b = read_copy(1);
    match (a, b) {
        (Some(x), Some(y)) => {
            if y.seq > x.seq {
                (Some(y), 1)
            } else {
                (Some(x), 0)
            }
        }
        (Some(x), None) => (Some(x), 0),
        (None, Some(y)) => (Some(y), 1),
        (None, None) => (None, 0),
    }
}

/// Write `r` to copy `i`. Only these two sectors are ever written.
unsafe fn write_copy(i: u32, r: &Record) -> i32 {
    let sector = if i == 0 { crate::store::layout().sel_a() } else { crate::store::layout().sel_b() };
    let b = &mut *(secbuf() as *mut [u8; 512]);
    sel::encode(r, b);
    let e = block_write(sector, 512, secbuf());
    if e == E_CARD_RO || e == E_RANGE {
        return e;
    }
    if e != 0 {
        return E_SEL_WRITE;
    }
    OK
}

unsafe fn path_ok(p: *const u8) -> bool {
    let mut n = 0;
    while n < SEL_PATH {
        let c = *p.add(n);
        if c == 0 {
            return n > 0;
        }
        if !(0x21..0x7f).contains(&c) {
            return false;
        }
        n += 1;
    }
    false
}

/// `dkrt_selection_get`.
pub unsafe fn selection_get(out: *mut DkrtSelection) -> i32 {
    let (r, _) = current();
    let o = &mut *out;
    let r = r.unwrap_or(Record::empty());
    o.count = r.count;
    o.flags = r.flags;
    o.paths = r.paths;
    OK
}

/// `dkrt_selection_set`: both copies, A then B, with the next sequence number. The boot counter carries over.
pub unsafe fn selection_set(s: *const DkrtSelection) -> i32 {
    let s = &*s;
    let r = sel::check_count(s.count);
    if r != OK {
        return r;
    }
    let mut k = 0;
    while k < s.count as usize {
        if !path_ok(s.paths[k].as_ptr()) {
            return E_SEL_PATH;
        }
        k += 1;
    }
    let (cur, _) = current();
    let cur = cur.unwrap_or(Record::empty());
    let r = sel::next_record(&cur, s.flags, s.count, &s.paths);
    dlog::ev(logk::MENU, dlog::INFO, [logk::MENU_SELECT, s.count, 0, 0, 0]);
    let e = write_copy(0, &r);
    if e != OK {
        return e;
    }
    write_copy(1, &r)
}

/// `CMD_SELECT_ONE`: write copy `i` only, with `seq + 1` (the state of a power cut between the two writes).
pub unsafe fn selection_set_one(i: u32, s: *const DkrtSelection) -> i32 {
    let s = &*s;
    let e = sel::check_one(s.count, i);
    if e != OK {
        return e;
    }
    let (cur, _) = current();
    let cur = cur.unwrap_or(Record::empty());
    let r = sel::next_record(&cur, s.flags, s.count, &s.paths);
    write_copy(i, &r)
}

/// Write the record with a changed counter/flags into the copy that is not the current one (the current one stays valid if
/// the write is cut).
unsafe fn update(mut r: Record, current_copy: u32, valid: bool) -> i32 {
    r.seq = if valid { r.seq.wrapping_add(1) } else { 1 };
    let target = if valid { 1 - current_copy } else { 0 };
    let e = write_copy(target, &r);
    if e == OK {
        REC = r;
        REC_VALID = true;
        REC_COPY = target;
    }
    e
}

/// Is the safe-mode key down among the events waiting in the UI queue? (The panel's held keys arrive as key-down events
/// shortly after reset; whether they are queued yet at this point is untested on a device **[O]**.)
unsafe fn key_held() -> bool {
    // `UI_QUEUE_LIT`: the 4-byte literal `pea <ui queue>` of the UI loop.
    let lit = core::hint::black_box(&raw const dkfw::view::UI_LOOP_QUEUE_LIT as usize) as u32;
    if lit == 0 {
        return false;
    }
    let q = read_volatile(lit as *const u32);
    // the firmware's objects live in the cached DDR window (`[board]` of the device files)
    let ddr = |a: u32| {
        (dkrt_sdk::device::DDR_CACHED_BASE..dkrt_sdk::device::DDR_CACHED_BASE + dkrt_sdk::device::DDR_BYTES).contains(&a) && a & 3 == 0
    };
    if !ddr(q) {
        return false;
    }
    let w = |a: u32, k: u32| read_volatile((a + 4 * k) as *const u32);
    let (count, mask, buf, rd) = (w(q, 1), w(q, 4), w(q, 5), w(q, 7));
    if count == 0 || count > 64 || mask > 0xffff || !ddr(buf) {
        return false;
    }
    let mut i = 0;
    while i < count {
        let item = read_volatile((buf + 4 * ((rd + i) & mask)) as *const u32);
        if ddr(item) && read_volatile(item as *const u8) == 0 {
            // a key event: +4 the key, +8 the state
            if w(item, 1) == SAFE_KEY_CODE && w(item, 2) != 0 {
                return true;
            }
        }
        i += 1;
    }
    false
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
    let (rec, copy) = current();
    let valid = rec.is_some();
    let mut r = rec.unwrap_or(Record::empty());
    dlog::ev(logk::BOOT_START, dlog::INFO, [RUNTIME_VERSION, crate::device(), r.stable_blocks, r.seq, r.boot_counter]);
    // the card's area: ours, blank or neither (read-only), before anything is written
    card::check();
    // the settings record (the fx routes and send amounts): into RAM before the UI starts
    crate::settings::load();
    // the crash record and the RAM probe of the boot before
    crash::keep_probe();
    if crash::at_boot() {
        let seen = crash::seen();
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
    REC = r;
    REC_VALID = valid;
    REC_COPY = copy;
    if r.stable_blocks != 0 {
        DKRT_STABLE_BLOCKS.0 = r.stable_blocks;
    }
    // safe mode: the record asked for it, or the key, or too many boots that did not reach the UI loop
    let reason = if r.flags & sel::FLAG_SAFE_ONCE != 0 {
        SAFE_FLAG
    } else if key_held() {
        SAFE_KEY
    } else if crash::seen().hang != 0 {
        // the boot before stopped because the DSP stopped (a hang record): one stop is enough, even after a stable boot; the selection stays as it is
        SAFE_DSP
    } else if crash::seen().panic != 0 {
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
        let mut k = 0;
        while k < SEL_MAX {
            b.results[k] = E_SAFE_MODE;
            k += 1;
        }
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
    loader::EARLY = true;
    // the type numbers of every machine of the selection, before the first load (reads the files; the staging buffer is free)
    assign::plan(&r);
    let mut k = 0;
    while k < r.count as usize {
        loader::BOOT_IDX = k as u32;
        let res = loader::load(r.paths[k].as_ptr());
        st().last_result = res;
        b.results[k] = res;
        if res == OK {
            b.loaded += 1;
        }
        let p = r.paths[k].as_ptr();
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
        k += 1;
    }
    loader::EARLY = false;
    loader::BOOT_IDX = u32::MAX;
    // the sticky map: written once, after the loads, and only when this boot changed it
    if let Some(map) = assign::changed_map() {
        let mut nr = REC;
        nr.map = map;
        update(nr, REC_COPY, REC_VALID);
    }
}

/// The DSP part of the module loaded for selection index `idx` is over with result `r` (called by the DSP loader).
pub unsafe fn note_result(idx: u32, r: i32) {
    dlog::ev(logk::MOD_DSP, if r == OK { dlog::INFO } else { logk::F_ERROR }, [idx, r as u32, 0, 0, 0]);
    if (idx as usize) < SEL_MAX {
        let b = info();
        b.results[idx as usize] = r;
        if r == OK {
            b.loaded += 1;
        }
    }
}

/// Every pass of the UI loop (`dkrt_tick`). The first pass notes the block count; the boot counter goes back to 0 when the session has run
/// `DKRT_STABLE_BLOCKS` blocks past it, or at once in a safe boot (nothing of a module runs there). A hang or a fault in between leaves
/// the counter raised, so the next boots count it: two such boots in a row start the third in safe mode.
pub unsafe fn ui_pass() {
    let b = info();
    if b.stable != 0 {
        return;
    }
    if b.magic != INFO_MAGIC {
        b.ui_reached = 1;
        b.stable = 1;
        return; // the boot hook did not run (an image patched without the boot site)
    }
    let now = crate::fw_blocks().unwrap_or(0);
    if b.ui_reached == 0 {
        b.ui_reached = 1;
        DKRT_UI_START.0 = now;
        UI_PASSES = 0;
        dlog::ev(logk::UI_FIRST, dlog::INFO, [now, b.safe, b.loaded, 0, 0]);
    }
    UI_PASSES = UI_PASSES.wrapping_add(1);
    let elapsed = now.wrapping_sub(DKRT_UI_START.0);
    let period = core::ptr::read_volatile(addr_of!(DKRT_STABLE_BLOCKS.0));
    let dead = elapsed == 0 && UI_PASSES >= DEAD_CLOCK_PASSES;
    if b.safe != 0 || elapsed >= period || dead {
        b.stable = 1;
        // The record as the card holds it now, not the one this boot started from: the menu may have saved a selection since.
        // A boot that followed a crash in a stable session leaves one bad boot counted (`LATE_FLOOR`), so that a second such crash leads to safe mode.
        let floor = LATE_FLOOR;
        let (cur, copy) = current();
        if let Some(mut r) = cur {
            if r.boot_counter != floor {
                r.boot_counter = floor;
                update(r, copy, true);
            }
        }
        dlog::ev(logk::STABLE, dlog::INFO, [now, floor, 0, 0, 0]);
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
    };
}
