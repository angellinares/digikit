//! Peak windows for `sample_pool_peaks_window`: min/max of `columns` equal parts of a frame range of a READY pool slot. The PCM is
//! read in the worker task (`worker.rs`) through the optional adapter hook `dkpool_window_step` (dkrt_pool.h); the UI task only
//! starts, polls and copies.
//!
//! One window per sample-tracks client. A request is a key (slot index, token, first frame, frame count, columns). States:
//!   Idle    nothing requested, or the last result is not valid any more
//!   Pending a job runs for `cur`; `next` holds a newer request when `has_next`
//!   Ready   `result` holds the pairs of `cur`
//!   Failed  the job ended with an error; it is returned until the key changes (no retry)
//!
//! Task rules. While the state is Pending the worker task may read and write `cur.handle`, `cur.path`, `cur.key`, `result` and
//! `cursor`. The UI task does not touch these then. A new key meanwhile goes to `next`; the old job is cancelled and `done`
//! (UI task) starts the job for `next` after the old one has ended. So two jobs never share a window. A window has at most one job,
//! so `done` needs no job id.
use crate::tracks::{client, core_, CLIENTS};
use crate::worker::{self, JOB_E_CANCELLED};
use core::ptr::{addr_of, addr_of_mut, write_volatile};
use dkrt_sdk::sample_tracks::{Handle, PATH};
use dkrt_sdk::{E_CONTRACT, E_RANGE};

pub const WINDOW_MAX: usize = 128;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Key {
    index: u32,
    token: u32,
    first: u32,
    frames: u32,
    columns: u32,
}

/// A request: the key, a copy of the backing handle and the NUL-terminated path the adapter gets.
#[derive(Clone, Copy)]
pub(crate) struct Req {
    key: Key,
    handle: Handle,
    path: [u8; PATH + 1],
}

const REQ0: Req = Req {
    key: Key { index: 0, token: 0, first: 0, frames: 0, columns: 0 },
    handle: Handle { id: 0, token: 0, base_l: 0, base_r: 0, frames: 0, rate: 0, channels: 0 },
    path: [0; PATH + 1],
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum State {
    Idle,
    Pending,
    Ready,
    Failed(i32),
}

pub(crate) struct Window {
    client: usize,
    state: State,
    job: u32,
    cur: Req,
    next: Req,
    has_next: bool,
    /// The client went away while Pending: `done` ends in Idle.
    abandon: bool,
    /// `columns` pairs of int16 min, max; written by the worker.
    result: [i16; 2 * WINDOW_MAX],
    /// Next frame to read; written by the worker.
    cursor: u32,
}

const WINDOW0: Window = Window {
    client: 0,
    state: State::Idle,
    job: 0,
    cur: REQ0,
    next: REQ0,
    has_next: false,
    abandon: false,
    result: [0; 2 * WINDOW_MAX],
    cursor: 0,
};

static mut WIN: [Window; CLIENTS] = [WINDOW0; CLIENTS];

// ---- the adapter hook and the slot check (replaced in host tests) ---------------------------------------------------------------

#[cfg(not(test))]
unsafe fn hook_present() -> bool {
    crate::tracks::window_hook()
}

#[cfg(not(test))]
unsafe fn hook_step(h: *const Handle, path: *const u8, first: u32, frames: u32, columns: u32, out: *mut i16, cursor: *mut u32) -> i32 {
    crate::tracks::window_step(h, path, first, frames, columns, out, cursor)
}

/// Does the slot of the key still hold the same load?
#[cfg(not(test))]
unsafe fn alive(client: usize, key: &Key) -> bool {
    core_().peak_handle(client, key.index as usize, key.token).is_ok()
}

/// Host test adapter: `HOOK_MORE` steps ask for more, then it fills two values and ends with `HOOK_END`.
#[cfg(test)]
static mut HOOK_MORE: u32 = 0;
#[cfg(test)]
static mut HOOK_END: i32 = 0;
/// Host test: does the slot still hold the load of the key?
#[cfg(test)]
static mut ALIVE: bool = true;

#[cfg(test)]
unsafe fn hook_present() -> bool {
    true
}

#[cfg(test)]
unsafe fn hook_step(_h: *const Handle, _path: *const u8, first: u32, frames: u32, _columns: u32, out: *mut i16, cursor: *mut u32) -> i32 {
    if HOOK_MORE > 0 {
        HOOK_MORE -= 1;
        return 1;
    }
    *out = -5;
    *out.add(1) = 7;
    *cursor = first + frames;
    HOOK_END
}

#[cfg(test)]
unsafe fn alive(_client: usize, _key: &Key) -> bool {
    ALIVE
}

// ---- state machine ----------------------------------------------------------------------------------------------------------------

/// The arguments that need no slot: `out` aligned, `columns` in range, enough capacity, a non-empty range.
fn check_args(out: *const u8, frames: u32, columns: u32, capacity: u32) -> Result<(), i32> {
    if out.is_null() || (out as usize & 1) != 0 || columns < 1 || columns as usize > WINDOW_MAX || capacity < columns * 4 || frames < 1 {
        return Err(E_CONTRACT);
    }
    Ok(())
}

/// Zero the result, set the cursor and post the job for `cur`. 0, or the error of `worker::post` (the state is Idle then).
unsafe fn start(w: *mut Window) -> i32 {
    let mut i = 0;
    while i < 2 * WINDOW_MAX {
        write_volatile(addr_of_mut!((*w).result[i]), 0);
        i += 1;
    }
    write_volatile(addr_of_mut!((*w).cursor), (*w).cur.key.first);
    let id = worker::post(0, Some(step), Some(done), w as *mut u8);
    if id > 0 {
        (*w).state = State::Pending;
        (*w).job = id as u32;
        0
    } else {
        (*w).state = State::Idle;
        id
    }
}

/// Worker task. Calls the adapter for one chunk and returns its value (1: more). It touches only the fields that belong to the
/// worker while the state is Pending.
unsafe extern "C" fn step(ctx: *mut u8) -> i32 {
    let w = ctx as *mut Window;
    hook_step(
        addr_of!((*w).cur.handle),
        addr_of!((*w).cur.path) as *const u8,
        (*w).cur.key.first,
        (*w).cur.key.frames,
        (*w).cur.key.columns,
        addr_of_mut!((*w).result) as *mut i16,
        addr_of_mut!((*w).cursor),
    )
}

/// UI task, once per job. A newer request waits in `next`: start it. Otherwise take the result.
unsafe extern "C" fn done(ctx: *mut u8, status: i32) {
    let w = ctx as *mut Window;
    if (*w).state != State::Pending {
        return;
    }
    if (*w).has_next {
        (*w).cur = (*w).next;
        (*w).has_next = false;
        start(w);
        return;
    }
    if (*w).abandon {
        (*w).abandon = false;
        (*w).state = State::Idle;
        return;
    }
    (*w).state = if status == 0 {
        State::Ready
    } else if status == JOB_E_CANCELLED {
        State::Idle
    } else {
        State::Failed(status)
    };
    // the slot was loaded again meanwhile: the next call asks again
    let key = (*w).cur.key;
    if !alive((*w).client, &key) {
        (*w).state = State::Idle;
    }
}

/// The key logic of one call with a valid request: the columns when ready (copied to `out`), 0 while pending, or an error.
unsafe fn lookup(w: *mut Window, req: &Req, out: *mut u8) -> i32 {
    let key = req.key;
    let state = (*w).state;
    if state == State::Pending {
        let active = if (*w).has_next { (*w).next.key } else { (*w).cur.key };
        if !(*w).abandon && key == active {
            return 0;
        }
        if !(*w).has_next && !(*w).abandon {
            worker::cancel(0, (*w).job);
        }
        (*w).next = *req;
        (*w).has_next = true;
        (*w).abandon = false;
        return 0;
    }
    if (*w).cur.key == key {
        if state == State::Ready {
            core::ptr::copy_nonoverlapping(addr_of!((*w).result) as *const u8, out, key.columns as usize * 4);
            return key.columns as i32;
        }
        if let State::Failed(e) = state {
            return e;
        }
    }
    (*w).cur = *req;
    start(w)
}

/// The client's module went away: stop its job; no result is kept.
unsafe fn reset_window(w: *mut Window) {
    if (*w).state == State::Pending {
        worker::cancel(0, (*w).job);
        (*w).has_next = false;
        (*w).abandon = true;
    } else {
        (*w).state = State::Idle;
    }
}

/// Client teardown (`tracks::remove`).
pub(crate) unsafe fn reset(client: usize) {
    if client < CLIENTS {
        reset_window(addr_of_mut!(WIN[client]));
    }
}

/// `sample_pool_peaks_window`, UI task.
pub(crate) unsafe fn window(handle: u32, index: u32, token: u32, first: u32, frames: u32, columns: u32, out: *mut u8, capacity: u32) -> i32 {
    let c = match client(handle) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if let Err(e) = check_args(out, frames, columns, capacity) {
        return e;
    }
    if !hook_present() {
        return E_CONTRACT;
    }
    let h = match core_().peak_handle(c, index as usize, token) {
        Ok(h) => h,
        Err(e) => return e,
    };
    if first as u64 + frames as u64 > h.frames as u64 {
        return E_RANGE;
    }
    let mut req = REQ0;
    req.key = Key { index, token, first, frames, columns };
    req.handle = h;
    match core_().path(c, index as usize) {
        Ok(p) => {
            core::ptr::copy_nonoverlapping(p.as_ptr(), req.path.as_mut_ptr(), p.len());
            req.path[p.len()] = 0;
        }
        Err(e) => return e,
    }
    let w = addr_of_mut!(WIN[c]);
    (*w).client = c;
    lookup(w, &req, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::{test_pump, test_reset, TEST_LOCK};

    fn key(first: u32) -> Key {
        Key { index: 1, token: 7, first, frames: 1000, columns: 4 }
    }

    fn req(first: u32) -> Req {
        let mut r = REQ0;
        r.key = key(first);
        r
    }

    unsafe fn setup() {
        test_reset();
        HOOK_MORE = 0;
        HOOK_END = 0;
        ALIVE = true;
    }

    #[test]
    fn argument_errors() {
        let mut buf = [0i16; 8];
        let p = buf.as_mut_ptr() as *const u8;
        assert_eq!(check_args(p, 10, 4, 16), Ok(()));
        assert_eq!(check_args(core::ptr::null(), 10, 4, 16), Err(E_CONTRACT));
        assert_eq!(check_args(unsafe { p.add(1) }, 10, 4, 16), Err(E_CONTRACT));
        assert_eq!(check_args(p, 10, 0, 16), Err(E_CONTRACT));
        assert_eq!(check_args(p, 10, WINDOW_MAX as u32 + 1, 4096), Err(E_CONTRACT));
        assert_eq!(check_args(p, 10, 4, 15), Err(E_CONTRACT));
        assert_eq!(check_args(p, 0, 4, 16), Err(E_CONTRACT));
        assert_eq!(check_args(p, 10, WINDOW_MAX as u32, WINDOW_MAX as u32 * 4), Ok(()));
    }

    #[test]
    fn new_key_posts_a_job_and_same_key_waits() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            setup();
            let mut w = WINDOW0;
            let mut out = [0u8; 16];
            assert_eq!(lookup(&mut w, &req(0), out.as_mut_ptr()), 0);
            assert_eq!(w.state, State::Pending);
            let job = w.job;
            assert_eq!(lookup(&mut w, &req(0), out.as_mut_ptr()), 0);
            assert_eq!(w.job, job);
            assert!(!w.has_next);
        }
    }

    #[test]
    fn steps_then_done_make_it_ready_and_copy() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            setup();
            HOOK_MORE = 2;
            let mut w = WINDOW0;
            let mut out = [0u8; 16];
            lookup(&mut w, &req(0), out.as_mut_ptr());
            test_pump();
            test_pump();
            test_pump();
            assert_eq!(w.state, State::Pending);
            test_pump();
            assert_eq!(w.state, State::Ready);
            assert_eq!(w.cursor, 1000);
            assert_eq!(lookup(&mut w, &req(0), out.as_mut_ptr()), 4);
            assert_eq!(&out[..2], &(-5i16).to_ne_bytes());
            assert_eq!(&out[2..4], &7i16.to_ne_bytes());
        }
    }

    #[test]
    fn key_change_while_pending_cancels_and_restarts() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            setup();
            HOOK_MORE = 100;
            let mut w = WINDOW0;
            let mut out = [0u8; 16];
            lookup(&mut w, &req(0), out.as_mut_ptr());
            let first_job = w.job;
            test_pump();
            assert_eq!(lookup(&mut w, &req(500), out.as_mut_ptr()), 0);
            assert!(w.has_next);
            test_pump(); // the old job ends cancelled, `done` posts the new one
            assert!(!w.has_next);
            assert_eq!(w.state, State::Pending);
            assert_ne!(w.job, first_job);
            assert_eq!(w.cur.key, key(500));
            assert_eq!(w.cursor, 500);
            HOOK_MORE = 0;
            test_pump();
            test_pump();
            assert_eq!(w.state, State::Ready);
            assert_eq!(lookup(&mut w, &req(500), out.as_mut_ptr()), 4);
        }
    }

    #[test]
    fn failed_status_stays_until_the_key_changes() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            setup();
            HOOK_END = -3;
            let mut w = WINDOW0;
            let mut out = [0u8; 16];
            lookup(&mut w, &req(0), out.as_mut_ptr());
            test_pump();
            test_pump();
            assert_eq!(w.state, State::Failed(-3));
            let job = w.job;
            assert_eq!(lookup(&mut w, &req(0), out.as_mut_ptr()), -3);
            assert_eq!(w.job, job);
            assert_eq!(lookup(&mut w, &req(1), out.as_mut_ptr()), 0);
            assert_eq!(w.state, State::Pending);
            assert_ne!(w.job, job);
        }
    }

    #[test]
    fn cancelled_job_goes_idle() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            setup();
            let mut w = WINDOW0;
            let mut out = [0u8; 16];
            lookup(&mut w, &req(0), out.as_mut_ptr());
            assert_eq!(worker::cancel(0, w.job), 0);
            test_pump();
            assert_eq!(w.state, State::Idle);
        }
    }

    #[test]
    fn a_reloaded_slot_goes_idle() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            setup();
            let mut w = WINDOW0;
            let mut out = [0u8; 16];
            lookup(&mut w, &req(0), out.as_mut_ptr());
            ALIVE = false;
            test_pump();
            test_pump();
            assert_eq!(w.state, State::Idle);
        }
    }

    #[test]
    fn reset_while_pending_ends_idle_after_the_job() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            setup();
            HOOK_MORE = 100;
            let mut w = WINDOW0;
            let mut out = [0u8; 16];
            lookup(&mut w, &req(0), out.as_mut_ptr());
            test_pump();
            reset_window(&mut w);
            assert_eq!(w.state, State::Pending);
            test_pump();
            assert_eq!(w.state, State::Idle);
            assert!(!w.abandon);
        }
    }
}
