//! The DSP diagnostics poll and its heartbeat watch, the effect and voice guards, the module list for a stop, and the health snapshot. They stay in
//! the runtime: the fault record, `dk/fault`, ABOUT and the machine table read their results. What they log goes through `dlog::ev`.
use crate::dlog::{ev, sink_present, word_of, INFO};
use crate::{arena, fw_blocks, st};
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use dkrt_sdk::*;

extern "C" {
    /// `core/dsp-link`'s diagnostics: `int dl_diag(u32 *out, u32 n)` writes up to `n` words and returns how many (negative: none). It resolves to 0
    /// when the image has no such function (`[object] optional`).
    fn dl_diag();
}

/// Words asked of `dl_diag`, and the word of its output that the DSP counts up while it runs (the heartbeat). The layout is `core/dsp-link`'s
/// (its README): word 0 is `0xD1A6` with the count in the high half, word 1 the link hook's block beat, word 2 the voice hook's, words 3 to 13 the
/// context registers, word 14 the last breadcrumb (stage | voice << 8 | machine type << 16), word 15 the dispatches to registered machine types, word 16 the routines that returned,
/// words 17 to 23 the guard of `core/dsp-voice` (finding 51): 17 faults (16 bits) | kind << 16 | machine types disabled << 24, 18 type | voice << 16 of the last fault, 19 to 22 the
/// eight parameter words of that call in pairs, 23 the type disabled last. The fault table (always, even without `core/dsp-voice`): 24 `last` of `core/fx`
/// (stage | site << 8 | row byte offset << 16: stage 4 the core is about to call the module, 5 it returned; site 2 master, 3 send), 25 the module step (`DK_STEP`: the block tail copies the module's static word `_dk_step` into every record),
/// 26 and 27 the module calls of the master and the send route, 28 effect faults (16 bits) | row of the last fault (byte offset + 1) << 16, 29 and 30 the
/// addresses of this and of the previous reply record, 31 the effect row switched off last (byte offset + 1).
/// If it moves the heartbeat, change `DIAG_HEARTBEAT` here and nothing else.
pub const DIAG_WORDS: usize = 32;
/// Words of the fault table.
const D_FXLAST: usize = 24;
const D_STEP: usize = 25;
const D_FXFAULT: usize = 28;
const D_REC0: usize = 29;
const D_REC1: usize = 30;
const D_FXDIS: usize = 31;
/// The effect table's row size in bytes.
const FX_ROW_BYTES: u32 = 96;
pub const DIAG_HEARTBEAT: usize = 1;
/// Words that count up while the DSP runs and so say nothing when they move: the two beats and the dispatch and return counts. The poll logs the diagnostics
/// again only when another word changes.
const DIAG_COUNTERS: [usize; 8] = [1, 2, 15, 16, 26, 27, D_REC0, D_REC1];
/// The low half of word 0 of a diagnostics block.
pub const DIAG_MAGIC: u32 = 0xd1a6;
/// The DSP diagnostics are read once a second (blocks); a full set is logged at the first poll, when a word other than a counter changes, and at
/// least every ten seconds. The health snapshot every ten seconds, the heap walk in every fourth snapshot.
const DIAG_EVERY: u32 = 1500;
const DIAG_LOG_EVERY: u32 = 15000;
const HEALTH_EVERY: u32 = 15000;
/// The heartbeat must have moved within this many blocks (3 s) of the block counter's advance; a heartbeat that never moved gets 20 s.
const STALL_BLOCKS: u32 = 4500;
const NEVER_BLOCKS: u32 = 30000;
/// After the DSP was found stopped the poll runs on every pass for this many passes and logs each distinct reply it sees: the two reply
/// records (the DSP's two rings) alternate, and the breadcrumb of the step that stopped may be in only one of them.
const STOPPED_SAMPLES: u32 = 400;
/// A stopped DSP leaves its two reply records one block apart (finding 47, rung C), so after a stop only a heartbeat this many blocks past the
/// larger one counts as running again (a running DSP adds 1500 a second, and the poll is a second apart).
const RESUME_MARGIN: u32 = 16;

struct Watch {
    next_diag: u32,
    next_log: u32,
    next_health: u32,
    snapshots: u32,
    /// `dl_diag` is in the image (decided at the first poll).
    probed: bool,
    present: bool,
    seen_alive: bool,
    hb: u32,
    hb_valid: bool,
    changed_at: u32,
    first_at: u32,
    last: [u32; DIAG_WORDS],
    n: u32,
    /// The guard's fault count (word 17, 16 bits) and the number of disabled types already logged.
    faults_seen: u32,
    disabled_seen: u32,
    /// A full set has been logged (the first poll logs one).
    logged: bool,
    /// The last set logged, for the change test.
    logged_words: [u32; DIAG_WORDS],
    /// After the stop: passes left to sample, and the distinct replies logged so far (the stop event's own is the first).
    sampling: u32,
    seen: [[u32; DIAG_WORDS]; 4],
    nseen: usize,
    /// The breadcrumb (word 14) of the reply that showed the stop.
    stop_crumb: u32,
    /// Effect faults and the switched-off row already logged (words 28 and 31).
    fx_faults_seen: u32,
    fx_disabled_seen: u32,
    /// The hang record: passes left before it is written (0: none pending), and what the replies seen after the stop showed: the module step, the effect `last`
    /// and the voice breadcrumb with a call pending (stage 4). The record of a stopped DSP alternate; the step is in only one of them.
    hang_wait: u32,
    m_step: u32,
    m_fx: u32,
    m_voice: u32,
}
static mut W: Watch = Watch {
    next_diag: 0,
    next_log: 0,
    next_health: 0,
    snapshots: 0,
    probed: false,
    present: false,
    seen_alive: false,
    hb: 0,
    hb_valid: false,
    changed_at: 0,
    first_at: 0,
    last: [0; DIAG_WORDS],
    n: 0,
    faults_seen: 0,
    disabled_seen: 0,
    logged: false,
    logged_words: [0; DIAG_WORDS],
    sampling: 0,
    seen: [[0; DIAG_WORDS]; 4],
    nseen: 0,
    stop_crumb: 0,
    fx_faults_seen: 0,
    fx_disabled_seen: 0,
    hang_wait: 0,
    m_step: 0,
    m_fx: 0,
    m_voice: 0,
};
/// Passes after the stop before the hang record is written: the two reply records have both been read by then.
const HANG_WAIT_PASSES: u32 = 40;
/// 1 once the watch decided the DSP stopped. Read by `dkrt_diag`, the proofs and ABOUT.
#[no_mangle]
pub static mut DKRT_DSP_STOPPED: u32 = 0;

fn due(now: u32, at: u32) -> bool {
    (now.wrapping_sub(at) as i32) >= 0
}

pub fn dsp_stopped() -> bool {
    unsafe { read_volatile(addr_of!(DKRT_DSP_STOPPED)) != 0 }
}

/// The breadcrumb the DSP left when the watch decided it stopped (0: not stopped).
pub fn stop_crumb() -> u32 {
    unsafe { if dsp_stopped() { W.stop_crumb } else { 0 } }
}

pub fn heartbeat() -> (bool, u32) {
    unsafe { (W.present && W.hb_valid, W.hb) }
}

/// Called once per UI-loop pass.
pub unsafe fn tick() {
    crate::dlog::run_tick();
    let Some(now) = fw_blocks() else { return };
    let w = &mut *addr_of_mut!(W);
    if !w.probed {
        w.probed = true;
        w.present = core::hint::black_box(dl_diag as usize) != 0;
        w.next_diag = now.wrapping_add(DIAG_EVERY);
        w.next_health = now.wrapping_add(HEALTH_EVERY);
        w.first_at = now;
        w.changed_at = now;
        if !w.present {
            ev(logk::DSP_ABSENT, INFO, [0; 5]);
        }
        return;
    }
    if w.present && w.sampling > 0 {
        w.sampling -= 1;
        sample_stopped(w);
        if w.hang_wait > 0 {
            w.hang_wait -= 1;
            // not while the firmware takes or flashes an OS upgrade: a reset in the flash write would leave the container half written (finding 77, section 5);
            // the stop stays in the log
            if w.hang_wait == 0 && !crate::os_upgrade_active() {
                write_hang(w);
            }
        }
    }
    if due(now, w.next_diag) {
        w.next_diag = now.wrapping_add(DIAG_EVERY);
        if w.present {
            poll_diag(w, now);
        }
    }
    if due(now, w.next_health) {
        w.next_health = now.wrapping_add(HEALTH_EVERY);
        snapshot(w);
    }
}

unsafe fn read_diag(buf: &mut [u32; DIAG_WORDS]) -> usize {
    let f: unsafe extern "C" fn(*mut u32, u32) -> i32 = core::mem::transmute(dl_diag as unsafe extern "C" fn());
    let r = f(buf.as_mut_ptr(), DIAG_WORDS as u32);
    if r <= 0 {
        return 0;
    }
    (r as usize).min(DIAG_WORDS)
}

/// Log a diagnostics set raw, four words to an event: argument 0 is the count and the chunk.
unsafe fn log_words(buf: &[u32; DIAG_WORDS], n: usize, flags: u16) {
    let mut c = 0;
    while c * 4 < n {
        let mut a = [(n as u32) << 8 | c as u32, 0, 0, 0, 0];
        let mut j = 0;
        while j < 4 {
            if c * 4 + j < n {
                a[1 + j] = buf[c * 4 + j];
            }
            j += 1;
        }
        ev(logk::DSP_DIAG, flags, a);
        c += 1;
    }
}

/// Whether two sets differ in a word that is not a counter.
fn differs(a: &[u32; DIAG_WORDS], b: &[u32; DIAG_WORDS], n: usize) -> bool {
    let mut j = 0;
    while j < n {
        if !DIAG_COUNTERS.contains(&j) && a[j] != b[j] {
            return true;
        }
        j += 1;
    }
    false
}

/// A pass after the stop: log each reply not seen yet, as error events.
unsafe fn sample_stopped(w: &mut Watch) {
    let mut buf = [0u32; DIAG_WORDS];
    let n = read_diag(&mut buf);
    if n == 0 || buf[0] & 0xffff != DIAG_MAGIC {
        return;
    }
    if w.stop_crumb == 0 {
        w.stop_crumb = buf[14];
    }
    merge_stop(w, &buf);
    // the larger heartbeat of the two records is the frozen one: the reference for "it moves again" (see `RESUME_MARGIN`)
    if n > DIAG_HEARTBEAT && (buf[DIAG_HEARTBEAT].wrapping_sub(w.hb) as i32) > 0 {
        w.hb = buf[DIAG_HEARTBEAT];
    }
    let mut i = 0;
    while i < w.nseen {
        if w.seen[i] == buf {
            return;
        }
        i += 1;
    }
    if w.nseen < w.seen.len() {
        w.seen[w.nseen] = buf;
        w.nseen += 1;
        log_words(&buf, n, logk::F_ERROR);
    }
}

unsafe fn poll_diag(w: &mut Watch, now: u32) {
    let mut buf = [0u32; DIAG_WORDS];
    let n = read_diag(&mut buf);
    if n == 0 {
        // no diagnostic block in the reply (a cleared record, or one the stopped DSP never finished): not a beat. After the DSP was seen
        // alive this counts toward the stall like a heartbeat that did not move.
        if w.hb_valid && w.seen_alive {
            stall_check(w, now, &buf);
        }
        return;
    }
    w.n = n as u32;
    w.last = buf;
    if n >= DIAG_WORDS {
        log_guard(w, &buf);
        log_fx(w, &buf);
    }
    // the words, raw: the first time, when a word other than a counter changed, and every ten seconds
    if !w.logged || differs(&buf, &w.logged_words, n) || due(now, w.next_log) {
        w.logged = true;
        w.logged_words = buf;
        w.next_log = now.wrapping_add(DIAG_LOG_EVERY);
        log_words(&buf, n, INFO);
    }
    // the heartbeat watch. The two reply records alternate and a stopped DSP leaves each with its last words, so a heartbeat that
    // differs from the last one may be the older record: only a larger value counts as a beat.
    if n > DIAG_HEARTBEAT && buf[0] & 0xffff == DIAG_MAGIC {
        let hb = buf[DIAG_HEARTBEAT];
        let ahead = hb.wrapping_sub(w.hb) as i32;
        let margin = if dsp_stopped() { RESUME_MARGIN as i32 } else { 0 };
        if !w.hb_valid || ahead > margin {
            if w.hb_valid {
                w.seen_alive = true;
            }
            w.hb = hb;
            w.hb_valid = true;
            w.changed_at = now;
            if dsp_stopped() {
                // it moves again
                write_volatile(addr_of_mut!(DKRT_DSP_STOPPED), 0);
                w.sampling = 0;
                w.hang_wait = 0;
                ev(logk::DSP_STOPPED, INFO, [hb, buf[2], buf[14], buf[15], 1]);
            }
        } else {
            if ahead > 0 {
                w.hb = hb; // the other record of a stopped DSP: the larger value is the reference
            }
            stall_check(w, now, &buf);
        }
    }
}

/// Machine types the guard counts for `MODULES` (the run-time types are below 32).
const GUARD_TYPES: usize = 32;
static mut GUARD_FAULTS: [u32; GUARD_TYPES] = [0; GUARD_TYPES];
/// Bit t: the guard switched type t off (seen in the polls since this boot).
static mut GUARD_OFF: u32 = 0;

/// Faults the guard counted for machine type `t` since the boot, as the polls saw them.
pub fn guard_faults(t: u32) -> u32 {
    if (t as usize) < GUARD_TYPES {
        unsafe { read_volatile(addr_of!(GUARD_FAULTS[t as usize])) }
    } else {
        0
    }
}

/// Did the guard switch machine type `t` off?
pub fn guard_disabled(t: u32) -> bool {
    t < GUARD_TYPES as u32 && unsafe { read_volatile(addr_of!(GUARD_OFF)) } & (1 << t) != 0
}

/// The guard's words (17 to 23): a new fault is a `MACHINE_FAULT`, a newly disabled type a `MACHINE_DISABLED`. The poll is a second apart, so a burst is one event
/// whose fault count says how many (the last fault is the one named); the full words also go to the raw `DSP_DIAG` set when they change.
unsafe fn log_guard(w: &mut Watch, buf: &[u32; DIAG_WORDS]) {
    let faults = buf[17] & 0xffff;
    let kind = (buf[17] >> 16) & 0xff;
    let disabled = buf[17] >> 24;
    if faults != w.faults_seen {
        // per type, for MODULES: the guard's words name only the type of the last fault, so a burst that spans two types within one poll counts for the last
        let t = (buf[18] & 0xffff) as usize;
        if t < GUARD_TYPES {
            GUARD_FAULTS[t] = GUARD_FAULTS[t].wrapping_add(faults.wrapping_sub(w.faults_seen) & 0xffff);
        }
        w.faults_seen = faults;
        ev(
            logk::MACHINE_FAULT,
            logk::F_ERROR,
            [buf[18], kind | (faults << 8), buf[19], buf[20], buf[21]],
        );
    }
    if disabled != w.disabled_seen {
        let t = buf[23] as usize;
        if t < GUARD_TYPES {
            GUARD_OFF |= 1 << t;
        }
        w.disabled_seen = disabled;
        ev(logk::MACHINE_DISABLED, logk::F_ERROR, [buf[23], disabled, faults, 0, 0]);
    }
}

/// The heartbeat did not move: decide whether the DSP stopped. The stop event names the step (the heartbeat, the voice hook's beat, the last
/// breadcrumb, the dispatch count), then every word of the reply follows as error events.
unsafe fn stall_check(w: &mut Watch, now: u32, buf: &[u32; DIAG_WORDS]) {
    if dsp_stopped() {
        return;
    }
    let still = now.wrapping_sub(w.changed_at);
    let limit = if w.seen_alive { STALL_BLOCKS } else { NEVER_BLOCKS };
    if still >= limit {
        write_volatile(addr_of_mut!(DKRT_DSP_STOPPED), 1);
        w.stop_crumb = buf[14];
        ev(logk::DSP_STOPPED, logk::F_ERROR, [w.hb, buf[2], buf[14], buf[15], 0]);
        log_words(buf, DIAG_WORDS, logk::F_ERROR);
        w.seen[0] = *buf;
        w.nseen = 1;
        w.sampling = STOPPED_SAMPLES;
        list_modules();
        w.m_step = 0;
        w.m_fx = 0;
        w.m_voice = 0;
        merge_stop(w, buf);
        // The hang record (crash.rs): only after a valid reply was read (a DSP that has not started leaves no diagnostic block) and not in a safe boot (a stop there
        // would reset again and again). The first UI pass has run: the watch is called from it. A DSP that stops within the first seconds (a module that hangs at once)
        // never shows a second beat; it counts as stopped after the 20 s of `NEVER_BLOCKS`.
        if w.hb_valid && crate::boot::info().safe == 0 && crate::boot::info().ui_reached != 0 {
            w.hang_wait = HANG_WAIT_PASSES;
        }
    }
}

/// What a reply of the stopped DSP adds to the picture: the module step (the block tail copies it into every record, so it is the step of the last block that
/// ended; a step stored in the call that stopped the DSP is lost with the core), and the sites with a call pending.
fn merge_stop(w: &mut Watch, buf: &[u32; DIAG_WORDS]) {
    if buf[D_STEP] != 0 {
        w.m_step = buf[D_STEP];
    }
    if buf[D_FXLAST] & 0xff == dkrt_sdk::fault::STAGE_CALL {
        w.m_fx = buf[D_FXLAST];
    }
    if buf[14] & 0xff == dkrt_sdk::fault::STAGE_CALL {
        w.m_voice = buf[14];
    }
}

/// Write the hang record and reset (crash.rs). The site is the one with a call pending: an effect (the master or the send route) or a voice; with none, the stop was
/// in the core itself and the record has no site.
unsafe fn write_hang(w: &mut Watch) {
    use dkrt_sdk::fault::*;
    let mut diag = w.seen[0];
    diag[D_STEP] = w.m_step;
    let (site, stage, id, last, module) = if w.m_fx != 0 {
        let site = (w.m_fx >> 8) & 0xff;
        let off = w.m_fx >> 16;
        (site, w.m_fx & 0xff, off, w.m_fx, crate::dsp::module_of_fx_table_row(off / FX_ROW_BYTES))
    } else if w.m_voice != 0 {
        let ty = w.m_voice >> 16;
        (SITE_VOICE, w.m_voice & 0xff, ty, w.m_voice, crate::dsp::module_of_type(ty))
    } else {
        (SITE_NONE, 0, 0, w.stop_crumb, None)
    };
    ev(logk::FAULT_FOUND, logk::F_ERROR, [KIND_DSP | site << 8 | stage << 16, id, w.m_step, last, module.map_or(u32::MAX, |m| m as u32)]);
    crate::settings::flush_if_dirty();
    crate::crash::write_hang(&crate::crash::Hang { site, id, stage, step: w.m_step, last, module: module.unwrap_or(usize::MAX), diag });
}

/// The effect guard's words (28 and 31): a new fault is an `FX_FAULT`, a row switched off an `FX_DISABLED`, each with the source of the module that owns the row.
unsafe fn log_fx(w: &mut Watch, buf: &[u32; DIAG_WORDS]) {
    let faults = buf[D_FXFAULT] & 0xffff;
    if faults != w.fx_faults_seen {
        w.fx_faults_seen = faults;
        let off = (buf[D_FXFAULT] >> 16).wrapping_sub(1);
        fx_event(logk::FX_FAULT, off / FX_ROW_BYTES, faults);
    }
    if buf[D_FXDIS] != w.fx_disabled_seen {
        w.fx_disabled_seen = buf[D_FXDIS];
        if buf[D_FXDIS] != 0 {
            fx_event(logk::FX_DISABLED, (buf[D_FXDIS] - 1) / FX_ROW_BYTES, faults);
        }
    }
}

unsafe fn fx_event(kind: u16, row: u32, faults: u32) {
    let src = match crate::dsp::module_of_fx_table_row(row) {
        Some(m) => (m as u16 + 1) << logk::SOURCE_SHIFT,
        None => 0,
    };
    ev(kind, logk::F_ERROR | src, [row, faults, 0, 0, 0]);
}

/// The loaded modules, one event each (`logk::MOD_LIST`, an error event when the DSP stopped): index, slots mask, 12 bytes of name.
unsafe fn list_modules() {
    let s = st();
    let mut i = 0;
    while i < MAX_MODULES_N {
        let m = &s.modules[i];
        if m.used != 0 {
            ev(
                logk::MOD_LIST,
                logk::F_ERROR,
                [i as u32, m.slot_mask, word_of(m.name.as_ptr(), 0, 32), word_of(m.name.as_ptr(), 4, 32), word_of(m.name.as_ptr(), 8, 32)],
            );
        }
        i += 1;
    }
}
const MAX_MODULES_N: usize = crate::MAX_MODULES;

/// A health snapshot: CPU and DSP load, arena free, and (every fourth time) the free firmware heap in KiB.
unsafe fn snapshot(w: &mut Watch) {
    let walk = w.snapshots % 4 == 0;
    w.snapshots = w.snapshots.wrapping_add(1);
    if !sink_present() {
        return;
    }
    let (afree, _) = arena::stats();
    let mut a = [0u32; 5];
    a[3] = afree;
    a[4] = 0xffff_ffff;
    if crate::health_present() {
        let mut h: DkrtHealth = core::mem::zeroed();
        crate::dkrt_health_light(&mut h, walk);
        a[0] = (h.flags << 16) | h.cpu_x10.min(0xffff);
        a[1] = h.dsp_avg_x10;
        a[2] = h.dsp_max_x10;
        if h.flags & HEALTH_HEAP != 0 {
            a[4] = h.heap_free / 1024;
        }
    }
    ev(logk::HEALTH, INFO, a);
}
