//! Background job queue on the stock loader worker task. A module posts a job with `job_post`: a step function, a done function
//! and a context. The queue holds no knowledge of what a job does.
//!
//! Step contract: `step(ctx)` returns `JOB_MORE` to be called again, 0 when finished, or a negative error. `done(ctx, status)`
//! is called exactly once in the UI task with the final status, or `JOB_E_CANCELLED` after a cancel. A module that is being
//! unloaded gets no `done` (see `retire`).
//!
//! Task rules: `step` runs in the worker task. It computes on its own memory and calls no `dk_api` function. `done` runs in the
//! UI task and may call the API.
//!
//! The stock worker is shared with the stock sample loads, so one job is in flight at a time and the worker runs steps of
//! it only for `BUDGET_BLOCKS` per submit. Then the job goes to the end of the queue (round robin). Without the block clock
//! the worker runs exactly one step per submit.
//!
//! Values `JOB_MORE` and `JOB_E_CANCELLED` must match sdk/include/dk/job.h.
use core::ffi::c_void;
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use core::sync::atomic::{compiler_fence, Ordering};
use dkrt_sdk::api::{JobDone, JobStep};
use dkrt_sdk::*;

pub const JOB_MORE: i32 = 1;
pub const JOB_E_CANCELLED: i32 = -80;

/// The DT2 sample pool can have 16 requests queued at once (SP_REQS), plus module jobs.
const JOBS: usize = 24;
/// Time one submit may use: about 10 ms of the 1500 Hz block clock.
const BUDGET_BLOCKS: u32 = 15;

const FREE: u8 = 0;
const QUEUED: u8 = 1;
const RUNNING: u8 = 2;

#[derive(Clone, Copy)]
struct Job {
    id: u32,
    /// Source index of the posting module (record index + 1); 0: the runtime or an adapter.
    owner: u8,
    step: Option<JobStep>,
    done: Option<JobDone>,
    ctx: *mut u8,
    state: u8,
    cancelled: bool,
    /// The owner is being unloaded: free the entry without calling `done`.
    quiet: bool,
    /// Queue order: the lowest `seq` among the queued jobs goes first.
    seq: u32,
}

const EMPTY: Job = Job {
    id: 0,
    owner: 0,
    step: None,
    done: None,
    ctx: core::ptr::null_mut(),
    state: FREE,
    cancelled: false,
    quiet: false,
    seq: 0,
};

/// UI-task state (changed under an interrupt mask; the worker never reads it).
static mut TABLE: [Job; JOBS] = [EMPTY; JOBS];
static mut NEXT_ID: u32 = 0;
static mut NEXT_SEQ: u32 = 0;

// The flight: the job on the firmware worker. The UI task writes STEP/CTX/IDX/CANCEL, then STATE = FLIGHT_RUN last. The worker
// writes RESULT, then STATE = FLIGHT_END last. The UI task sets STATE = FLIGHT_NONE after it has read RESULT. Every access is
// volatile; each word is written by one task at a time, so no compare-and-swap is needed.
const FLIGHT_NONE: u32 = 0;
const FLIGHT_RUN: u32 = 1;
const FLIGHT_END: u32 = 2;
static mut FLIGHT_STATE: u32 = FLIGHT_NONE;
static mut FLIGHT_STEP: Option<JobStep> = None;
static mut FLIGHT_CTX: *mut u8 = core::ptr::null_mut();
static mut FLIGHT_IDX: usize = 0;
static mut FLIGHT_CANCEL: u32 = 0;
static mut FLIGHT_RESULT: i32 = 0;

// The std::function objects for the firmware queue (the same layout the sample adapters use): job = storage[2], manager, invoker;
// done = an empty function (8 zero words).
static mut FN: [u32; 4] = [0; 4];
static mut DONE: [u32; 8] = [0; 8];

unsafe fn locked<T>(f: impl FnOnce() -> T) -> T {
    let sr = crate::irq_mask();
    let result = f();
    crate::irq_restore(sr);
    result
}

/// The std::function manager: copy word 0 for a copy or move, nothing else.
extern "C" fn manager(dest: *mut u32, src: *const u32, op: i32) -> i32 {
    unsafe {
        if op == 1 || op == 2 {
            write_volatile(dest, read_volatile(src));
        }
    }
    0
}

/// The std::function invoker, in the worker task.
extern "C" fn invoker(_storage: *const u8) {
    unsafe { run_flight() }
}

/// Run steps of the flight job while it asks for more, is not cancelled and the budget lasts. Then publish the last result.
unsafe fn run_flight() {
    let step = read_volatile(addr_of!(FLIGHT_STEP));
    let ctx = read_volatile(addr_of!(FLIGHT_CTX));
    let start = crate::fw_blocks();
    let mut result = JOB_E_CANCELLED;
    if let Some(step) = step {
        loop {
            result = step(ctx);
            if result != JOB_MORE || read_volatile(addr_of!(FLIGHT_CANCEL)) != 0 {
                break;
            }
            let spent = match (start, crate::fw_blocks()) {
                (Some(a), Some(b)) => b.wrapping_sub(a) >= BUDGET_BLOCKS,
                _ => true,
            };
            if spent {
                break;
            }
        }
    }
    write_volatile(addr_of_mut!(FLIGHT_RESULT), result);
    compiler_fence(Ordering::SeqCst);
    write_volatile(addr_of_mut!(FLIGHT_STATE), FLIGHT_END);
}

/// Queue the flight on the firmware worker. False: no worker, nothing was queued. FLIGHT_STATE is set last before the queue
/// call: the worker may run the job before the call returns.
#[cfg(not(test))]
unsafe fn submit() -> bool {
    let worker = dkfw::samples::worker_get();
    if worker == 0 {
        return false;
    }
    // volatile stores: a plain loop would become a memset call
    write_volatile(addr_of_mut!(FN[0]), 0);
    write_volatile(addr_of_mut!(FN[1]), 0);
    write_volatile(addr_of_mut!(FN[2]), manager as usize as u32);
    write_volatile(addr_of_mut!(FN[3]), invoker as usize as u32);
    let mut i = 0;
    while i < 8 {
        write_volatile(addr_of_mut!(DONE[i]), 0);
        i += 1;
    }
    compiler_fence(Ordering::SeqCst);
    write_volatile(addr_of_mut!(FLIGHT_STATE), FLIGHT_RUN);
    dkfw::samples::worker_queue(
        worker,
        b"DkJob\0".as_ptr(),
        addr_of_mut!(FN) as *mut c_void,
        addr_of_mut!(DONE) as *mut c_void,
    );
    true
}

/// Host test variant: records the submission; the test runs `run_flight` itself.
#[cfg(test)]
pub static mut TEST_WORKER: bool = true;
#[cfg(test)]
pub static mut TEST_SUBMITS: u32 = 0;
#[cfg(test)]
unsafe fn submit() -> bool {
    let _ = (manager as extern "C" fn(*mut u32, *const u32, i32) -> i32, invoker as extern "C" fn(*const u8));
    let _ = (addr_of_mut!(FN) as *mut c_void, addr_of_mut!(DONE) as *mut c_void);
    if !TEST_WORKER {
        return false;
    }
    TEST_SUBMITS += 1;
    write_volatile(addr_of_mut!(FLIGHT_STATE), FLIGHT_RUN);
    true
}

/// Call `done` of a freed entry in the UI task with the module's source set.
unsafe fn deliver(job: Job, status: i32) {
    if job.quiet {
        return;
    }
    if let Some(done) = job.done {
        if job.owner != 0 {
            let prev = crate::dlog::enter_module(job.owner as usize - 1);
            done(job.ctx, status);
            crate::dlog::leave_module(prev);
        } else {
            done(job.ctx, status);
        }
    }
}

fn next_seq() -> u32 {
    unsafe {
        NEXT_SEQ = NEXT_SEQ.wrapping_add(1);
        NEXT_SEQ
    }
}

/// Add a job. Returns its id (> 0), `E_CONTRACT` for a null step, `E_BUSY` when all entries are in use.
pub unsafe fn post(owner: u8, step: Option<JobStep>, done: Option<JobDone>, ctx: *mut u8) -> i32 {
    if step.is_none() {
        return E_CONTRACT;
    }
    locked(|| {
        let mut i = 0;
        while i < JOBS {
            if TABLE[i].state == FREE {
                NEXT_ID = NEXT_ID.wrapping_add(1) & 0x7fff_ffff;
                if NEXT_ID == 0 {
                    NEXT_ID = 1;
                }
                TABLE[i] = Job {
                    id: NEXT_ID,
                    owner,
                    step,
                    done,
                    ctx,
                    state: QUEUED,
                    cancelled: false,
                    quiet: false,
                    seq: next_seq(),
                };
                return NEXT_ID as i32;
            }
            i += 1;
        }
        E_BUSY
    })
}

/// Adapter entry (owner 0): queue a job on the background worker. `step` and `done` are function addresses (`JobStep`, `JobDone`);
/// `done` may be 0. Returns a job id > 0, `E_CONTRACT` for a null step, `E_BUSY` when the queue is full. Task context: UI task.
#[no_mangle]
pub unsafe extern "C" fn dkrt_job_post(step: usize, done: usize, ctx: *mut u8) -> i32 {
    post(
        0,
        core::mem::transmute::<usize, Option<JobStep>>(step),
        core::mem::transmute::<usize, Option<JobDone>>(done),
        ctx,
    )
}

/// Adapter entry (owner 0): cancel a job posted with `dkrt_job_post`. Returns 0 or `E_NOT_FOUND`.
#[no_mangle]
pub unsafe extern "C" fn dkrt_job_cancel(id: u32) -> i32 {
    cancel(0, id)
}

/// Cancel a job of `owner`. A queued job gets its `done(JOB_E_CANCELLED)` at the next tick; a running one stops after its
/// current step. Returns 0, or `E_NOT_FOUND` for an unknown id or another owner's job.
pub unsafe fn cancel(owner: u8, id: u32) -> i32 {
    if id == 0 {
        return E_NOT_FOUND;
    }
    locked(|| {
        let mut i = 0;
        while i < JOBS {
            let j = &mut TABLE[i];
            if j.state != FREE && j.id == id && j.owner == owner {
                j.cancelled = true;
                if j.state == RUNNING {
                    write_volatile(addr_of_mut!(FLIGHT_CANCEL), 1);
                }
                return OK;
            }
            i += 1;
        }
        E_NOT_FOUND
    })
}

/// Loader calls it before an unload frees `rec`'s memory. The owner's queued jobs are dropped without `done`. False: its job is
/// on the worker; it is cancelled and gets no `done`, and the caller keeps the module memory (`E_BUSY`) until a later call
/// returns true.
pub unsafe fn retire(rec: usize) -> bool {
    let owner = rec as u8 + 1;
    locked(|| {
        let mut busy = false;
        let mut i = 0;
        while i < JOBS {
            let j = &mut TABLE[i];
            if j.state != FREE && j.owner == owner {
                if j.state == RUNNING {
                    j.cancelled = true;
                    j.quiet = true;
                    write_volatile(addr_of_mut!(FLIGHT_CANCEL), 1);
                    busy = true;
                } else {
                    *j = EMPTY;
                }
            }
            i += 1;
        }
        !busy
    })
}

/// UI task, once per pass: collect the finished flight, then submit the oldest queued job.
pub unsafe fn tick() {
    // 1. the flight ended: requeue it at the end, or finish it
    if read_volatile(addr_of!(FLIGHT_STATE)) == FLIGHT_END {
        compiler_fence(Ordering::SeqCst);
        let result = read_volatile(addr_of!(FLIGHT_RESULT));
        let idx = read_volatile(addr_of!(FLIGHT_IDX));
        write_volatile(addr_of_mut!(FLIGHT_STATE), FLIGHT_NONE);
        let finished = locked(|| {
            let j = &mut TABLE[idx];
            if result == JOB_MORE && !j.cancelled {
                j.state = QUEUED;
                j.seq = next_seq();
                None
            } else {
                let status = if j.cancelled { JOB_E_CANCELLED } else { result };
                let c = *j;
                *j = EMPTY;
                Some((c, status))
            }
        });
        if let Some((job, status)) = finished {
            deliver(job, status);
        }
    }
    // 2. cancelled jobs that never ran
    let mut i = 0;
    while i < JOBS {
        let dropped = locked(|| {
            let j = &mut TABLE[i];
            if j.state == QUEUED && j.cancelled {
                let c = *j;
                *j = EMPTY;
                Some(c)
            } else {
                None
            }
        });
        if let Some(job) = dropped {
            deliver(job, JOB_E_CANCELLED);
        }
        i += 1;
    }
    // 3. nothing in flight: submit the oldest queued job
    if read_volatile(addr_of!(FLIGHT_STATE)) != FLIGHT_NONE {
        return;
    }
    let pick = locked(|| {
        let mut best: Option<usize> = None;
        let mut i = 0;
        while i < JOBS {
            if TABLE[i].state == QUEUED {
                best = match best {
                    Some(b) if (TABLE[i].seq.wrapping_sub(TABLE[b].seq) as i32) >= 0 => Some(b),
                    _ => Some(i),
                };
            }
            i += 1;
        }
        if let Some(b) = best {
            TABLE[b].state = RUNNING;
            write_volatile(addr_of_mut!(FLIGHT_STEP), TABLE[b].step);
            write_volatile(addr_of_mut!(FLIGHT_CTX), TABLE[b].ctx);
            write_volatile(addr_of_mut!(FLIGHT_IDX), b);
            write_volatile(addr_of_mut!(FLIGHT_CANCEL), 0);
        }
        best
    });
    if let Some(b) = pick {
        if !submit() {
            // no worker: nothing was queued; finish the job with an error
            let job = locked(|| {
                let c = TABLE[b];
                TABLE[b] = EMPTY;
                c
            });
            deliver(job, E_READ);
        }
    }
}

/// Host tests of other files share the one queue: they hold this lock.
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Host test helper: empty the queue and the flight.
#[cfg(test)]
pub(crate) unsafe fn test_reset() {
    TABLE = [EMPTY; JOBS];
    NEXT_ID = 0;
    NEXT_SEQ = 0;
    FLIGHT_STATE = FLIGHT_NONE;
    FLIGHT_CANCEL = 0;
    TEST_WORKER = true;
    TEST_SUBMITS = 0;
}

/// Host test helper: one UI pass, then the worker runs what the pass submitted.
#[cfg(test)]
pub(crate) unsafe fn test_pump() {
    tick();
    if FLIGHT_STATE == FLIGHT_RUN {
        run_flight();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::TEST_LOCK as LOCK;

    struct Script {
        tag: u32,
        calls: u32,
        /// Steps that return JOB_MORE before the job finishes with `end`.
        more: u32,
        end: i32,
        done_calls: u32,
        status: i32,
    }

    static mut ORDER: [u32; 32] = [0; 32];
    static mut ORDER_N: usize = 0;

    fn script(tag: u32, more: u32) -> Script {
        Script { tag, calls: 0, more, end: 0, done_calls: 0, status: 99 }
    }

    unsafe extern "C" fn step(ctx: *mut u8) -> i32 {
        let s = &mut *(ctx as *mut Script);
        s.calls += 1;
        ORDER[ORDER_N] = s.tag;
        ORDER_N += 1;
        if s.calls <= s.more {
            JOB_MORE
        } else {
            s.end
        }
    }

    unsafe extern "C" fn done(ctx: *mut u8, status: i32) {
        let s = &mut *(ctx as *mut Script);
        s.done_calls += 1;
        s.status = status;
    }

    unsafe fn reset() {
        TABLE = [EMPTY; JOBS];
        NEXT_ID = 0;
        NEXT_SEQ = 0;
        FLIGHT_STATE = FLIGHT_NONE;
        FLIGHT_CANCEL = 0;
        TEST_WORKER = true;
        TEST_SUBMITS = 0;
        ORDER_N = 0;
    }

    /// One UI pass, then the worker runs what the pass submitted.
    unsafe fn pump() {
        tick();
        if FLIGHT_STATE == FLIGHT_RUN {
            run_flight();
        }
    }

    fn ptr(s: &mut Script) -> *mut u8 {
        s as *mut Script as *mut u8
    }

    #[test]
    fn full_queue_is_busy_and_null_step_is_refused() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let mut s = script(1, 0);
            let mut n = 0;
            while n < JOBS {
                assert!(post(0, Some(step), Some(done), ptr(&mut s)) > 0);
                n += 1;
            }
            assert_eq!(post(0, Some(step), Some(done), ptr(&mut s)), E_BUSY);
            assert_eq!(post(0, None, Some(done), ptr(&mut s)), E_CONTRACT);
        }
    }

    #[test]
    fn more_twice_then_done_once() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let mut s = script(1, 2);
            assert!(post(0, Some(step), Some(done), ptr(&mut s)) > 0);
            pump();
            pump();
            pump();
            assert_eq!(s.calls, 3);
            assert_eq!(s.done_calls, 0);
            tick();
            assert_eq!(s.done_calls, 1);
            assert_eq!(s.status, 0);
            tick();
            assert_eq!(s.done_calls, 1);
            assert_eq!(TEST_SUBMITS, 3);
        }
    }

    #[test]
    fn error_result_is_the_status() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let mut s = script(1, 0);
            s.end = -5;
            post(0, Some(step), Some(done), ptr(&mut s));
            pump();
            tick();
            assert_eq!((s.done_calls, s.status), (1, -5));
        }
    }

    #[test]
    fn two_more_jobs_alternate() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let mut a = script(1, 100);
            let mut b = script(2, 100);
            post(0, Some(step), Some(done), ptr(&mut a));
            post(0, Some(step), Some(done), ptr(&mut b));
            let mut n = 0;
            while n < 4 {
                pump();
                n += 1;
            }
            assert_eq!(&ORDER[..4], &[1, 2, 1, 2]);
        }
    }

    #[test]
    fn cancel_queued_calls_done_once() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let mut a = script(1, 100);
            let mut b = script(2, 0);
            post(3, Some(step), Some(done), ptr(&mut a));
            let id = post(3, Some(step), Some(done), ptr(&mut b));
            tick(); // a is in flight, b waits
            assert_eq!(cancel(4, id as u32), E_NOT_FOUND);
            assert_eq!(cancel(3, 0x7000), E_NOT_FOUND);
            assert_eq!(cancel(3, id as u32), OK);
            assert_eq!(b.done_calls, 0);
            tick();
            assert_eq!((b.done_calls, b.status, b.calls), (1, JOB_E_CANCELLED, 0));
            tick();
            assert_eq!(b.done_calls, 1);
        }
    }

    #[test]
    fn cancel_running_stops_after_the_current_step() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let mut a = script(1, 100);
            let id = post(0, Some(step), Some(done), ptr(&mut a));
            tick();
            assert_eq!(cancel(0, id as u32), OK);
            run_flight();
            assert_eq!(a.calls, 1);
            tick();
            assert_eq!((a.done_calls, a.status), (1, JOB_E_CANCELLED));
            tick();
            assert_eq!(a.calls, 1);
        }
    }

    #[test]
    fn retire_drops_queued_and_waits_for_the_flight() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let mut a = script(1, 100);
            let mut b = script(2, 0);
            let mut c = script(3, 0);
            post(3, Some(step), Some(done), ptr(&mut a)); // owner 3 = record 2
            post(3, Some(step), Some(done), ptr(&mut b));
            post(5, Some(step), Some(done), ptr(&mut c));
            tick(); // a is in flight
            assert!(!retire(2));
            run_flight();
            tick();
            assert_eq!((a.done_calls, b.done_calls, b.calls), (0, 0, 0));
            assert!(retire(2));
            pump();
            tick();
            assert_eq!((c.done_calls, c.status), (1, 0));
        }
    }

    #[test]
    fn missing_worker_finishes_with_an_error() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            TEST_WORKER = false;
            let mut a = script(1, 0);
            post(0, Some(step), Some(done), ptr(&mut a));
            tick();
            assert_eq!((a.done_calls, a.status, a.calls), (1, E_READ, 0));
            assert_eq!(TEST_SUBMITS, 0);
            assert_eq!(FLIGHT_STATE, FLIGHT_NONE);
        }
    }
}
