//! The runtime-owned I/O task (docs/plans/usb-fast.md 3.2). A module posts a job with `io_post`: a step function, a done function and a context.
//! The step runs on one RTOS task that the runtime creates on the first post, may block and may call `file_*` (`store_writer.rs`). The
//! queue holds no knowledge of what a job does.
//!
//! Step contract: `step(ctx)` returns `IO_MORE` to be called again, `IO_WAIT` to sleep until `io_wake(id)`, 0 when finished, or a negative error.
//! `done(ctx, status)` is called exactly once in the UI task with the final status, or `IO_E_CANCELLED` after a cancel. A module that is being
//! unloaded gets no `done` (see `retire`).
//!
//! The task: priority 3 (below the UI task at 6 and the MIDI task at 7, near the stock sample loader), a static TCB and stack, created once and
//! never deleted. It sleeps on a semaphore that every post and wake posts (ISR-safe); one wake may stand for several events, so after each
//! wake it runs the jobs until none is runnable (round robin between them, one step each time round).
//!
//! The values `IO_MORE`, `IO_WAIT` and `IO_E_CANCELLED` must match sdk/include/dk/platform/io.h.
//!
//! Rules: no `match` on integers, no `core::fmt`, no slice indexing; a step runs on a 16 KiB stack.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use dkrt_sdk::api::{IoDone, IoStep};
use dkrt_sdk::*;

pub const IO_MORE: i32 = 1;
pub const IO_WAIT: i32 = 2;
pub const IO_E_CANCELLED: i32 = -81;

/// Jobs at once.
const JOBS: usize = 8;
/// Priority of the task (0 to 15, higher wins): UI 6, MIDI 7, the stock sample loader 3.
const PRIORITY: u32 = 3;
const STACK_BYTES: usize = 0x4000;
const TCB_BYTES: usize = 0x60;

const FREE: u8 = 0;
/// Waits for its turn.
const RUN: u8 = 1;
/// Its step returned `IO_WAIT`: sleeping until `io_wake`.
const WAIT: u8 = 2;
/// Its step is running now (the task is inside it).
const STEP: u8 = 3;
/// Finished or cancelled: waits for `done` in the UI task.
const END: u8 = 4;

#[derive(Clone, Copy)]
struct Job {
    id: u32,
    /// Source index of the posting module (record index + 1); 0: the runtime or an adapter.
    owner: u8,
    step: Option<IoStep>,
    done: Option<IoDone>,
    ctx: *mut u8,
    state: u8,
    /// A wake that came while the step was running or the job was queued: the next `IO_WAIT` does not sleep.
    woken: bool,
    cancelled: bool,
    /// The owner is being unloaded: free the entry without calling `done`.
    quiet: bool,
    status: i32,
    /// Queue order: the lowest `seq` among the runnable jobs goes first.
    seq: u32,
}

const EMPTY: Job = Job {
    id: 0,
    owner: 0,
    step: None,
    done: None,
    ctx: core::ptr::null_mut(),
    state: FREE,
    woken: false,
    cancelled: false,
    quiet: false,
    status: 0,
    seq: 0,
};

/// Changed under an interrupt mask (the task, the UI task, an ISR and the MIDI task all touch it).
static mut TABLE: [Job; JOBS] = [EMPTY; JOBS];
static mut NEXT_ID: u32 = 0;
static mut NEXT_SEQ: u32 = 0;

/// The context of the step that runs now: its owner and job id. `STEP_ID` 0: no step runs.
static mut STEP_ID: u32 = 0;
static mut STEP_OWNER: u8 = 0;
/// A `file_*` call of the running step met a busy store (`note_busy`): `drain` sleeps a moment before it runs a step that returns `IO_MORE`.
static mut STEP_BUSY: bool = false;

/// The TCB of the I/O task.
fn io_tcb() -> u32 {
    unsafe { addr_of!(DKRT_IO_TASK.tcb) as usize as u32 }
}

/// The kernel's current task (its TCB address).
pub unsafe fn current_task() -> u32 {
    kernel::current_task()
}

/// The owner of the I/O step that runs now, or `None`: `file_*` accept only that context. The running task must be the I/O task (the kernel's
/// current-TCB pointer is ours): code of any module in the UI or MIDI task while a step is blocked is not the step.
pub fn step_owner() -> Option<u8> {
    unsafe {
        if read_volatile(addr_of!(STEP_ID)) == 0 || current_task() != io_tcb() {
            None
        } else {
            Some(read_volatile(addr_of!(STEP_OWNER)))
        }
    }
}

/// A `file_*` call of the running step answered E_BUSY.
pub fn note_busy() {
    unsafe { write_volatile(addr_of_mut!(STEP_BUSY), true) }
}

/// The job whose step runs now (0: none).
pub fn step_job() -> u32 {
    unsafe { read_volatile(addr_of!(STEP_ID)) }
}

unsafe fn locked<T>(f: impl FnOnce() -> T) -> T {
    let sr = crate::irq_mask();
    let result = f();
    crate::irq_restore(sr);
    result
}

fn next_seq() -> u32 {
    unsafe {
        NEXT_SEQ = NEXT_SEQ.wrapping_add(1);
        NEXT_SEQ
    }
}

// ---- the kernel ---------------------------------------------------------------------------------------------------------------

/// The task's stack, TCB and semaphore in one static (the kernel writes them from firmware code: the write-watch of the proofs leaves the object out,
/// tests/regression/runtime/write-safety.ts). The semaphore is 8 bytes: a flag and the one waiting TCB.
#[repr(C, align(16))]
pub struct IoTask {
    stack: [u8; STACK_BYTES],
    tcb: [u8; TCB_BYTES],
    sem: [u32; 2],
}
#[no_mangle]
pub static mut DKRT_IO_TASK: IoTask = IoTask { stack: [0; STACK_BYTES], tcb: [0; TCB_BYTES], sem: [0; 2] };
/// 0: no task, 1: being created, 2: it exists.
static mut TASK_UP: u32 = 0;

#[cfg(not(test))]
mod kernel {
    use super::*;

    /// Create and start the task. False: the kernel refused.
    pub unsafe fn create() -> bool {
        dkfw::rtos::sem_init(addr_of_mut!(DKRT_IO_TASK.sem) as *mut u32, 0);
        let tcb = addr_of_mut!(DKRT_IO_TASK.tcb) as u32;
        let ok = dkfw::rtos::task_create(tcb, entry as usize as u32, PRIORITY, addr_of_mut!(DKRT_IO_TASK.stack) as u32, STACK_BYTES as u32);
        if ok != 1 {
            return false;
        }
        dkfw::rtos::task_start(tcb);
        true
    }

    /// The current task's TCB address (the kernel's pointer variable).
    pub unsafe fn current_task() -> u32 {
        read_volatile(addr_of!(dkfw::rtos::CURRENT_TCB) as *const u32)
    }

    /// Sleep 1 ms (task context).
    pub unsafe fn pause() {
        dkfw::rtos::sleep_us(1000);
    }

    /// Sleep until a post (task context).
    pub unsafe fn pend() {
        dkfw::rtos::sem_pend(addr_of_mut!(DKRT_IO_TASK.sem) as *mut u32);
    }

    /// Wake the task: sets the flag and reschedules. ISR-safe.
    pub unsafe fn post() {
        dkfw::rtos::sem_post(addr_of_mut!(DKRT_IO_TASK.sem) as *mut u32);
    }
}

/// Host tests: a fake kernel that counts and refuses on request; the test calls `drain` itself.
#[cfg(test)]
pub static mut TEST_REFUSE: bool = false;
#[cfg(test)]
pub static mut TEST_CREATES: u32 = 0;
#[cfg(test)]
pub static mut TEST_POSTS: u32 = 0;
#[cfg(test)]
pub static mut TEST_PAUSES: u32 = 0;
/// The fake kernel's current task: the I/O task unless a test says another.
#[cfg(test)]
pub static mut TEST_CURRENT: u32 = 0;
#[cfg(test)]
mod kernel {
    use super::*;

    pub unsafe fn create() -> bool {
        let _ = (entry as extern "C" fn() -> !, addr_of_mut!(DKRT_IO_TASK), PRIORITY);
        if TEST_REFUSE {
            return false;
        }
        TEST_CREATES += 1;
        true
    }

    pub unsafe fn pend() {}

    pub unsafe fn post() {
        TEST_POSTS += 1;
    }

    pub unsafe fn pause() {
        TEST_PAUSES += 1;
    }

    pub unsafe fn current_task() -> u32 {
        TEST_CURRENT
    }
}

/// The task's entry: it never returns.
extern "C" fn entry() -> ! {
    unsafe {
        loop {
            kernel::pend();
            drain();
        }
    }
}

/// Run the runnable jobs, one step each in turn, until none is runnable.
unsafe fn drain() {
    loop {
        let pick = locked(|| {
            let mut best: Option<usize> = None;
            let mut i = 0;
            while i < JOBS {
                if TABLE[i].state == RUN {
                    best = match best {
                        Some(b) if (TABLE[i].seq.wrapping_sub(TABLE[b].seq) as i32) >= 0 => Some(b),
                        _ => Some(i),
                    };
                }
                i += 1;
            }
            match best {
                Some(b) => {
                    TABLE[b].state = STEP;
                    write_volatile(addr_of_mut!(STEP_OWNER), TABLE[b].owner);
                    write_volatile(addr_of_mut!(STEP_ID), TABLE[b].id);
                    Some((b, TABLE[b].step, TABLE[b].ctx))
                }
                None => None,
            }
        });
        let (b, step, ctx) = match pick {
            Some(p) => p,
            None => return,
        };
        write_volatile(addr_of_mut!(STEP_BUSY), false);
        let result = match step {
            Some(f) => f(ctx),
            None => IO_E_CANCELLED,
        };
        // a step that met a busy store and asks to run again waits a moment first (a step that never met one runs again at once)
        if result == IO_MORE && read_volatile(addr_of!(STEP_BUSY)) {
            kernel::pause();
        }
        let id = read_volatile(addr_of!(STEP_ID));
        write_volatile(addr_of_mut!(STEP_ID), 0);
        let ended = locked(|| {
            let j = &mut TABLE[b];
            if j.cancelled || (result != IO_MORE && result != IO_WAIT) {
                j.status = if j.cancelled { IO_E_CANCELLED } else { result };
                j.state = END;
                true
            } else if result == IO_MORE {
                j.state = RUN;
                j.seq = next_seq();
                false
            } else if j.woken {
                j.woken = false;
                j.state = RUN;
                j.seq = next_seq();
                false
            } else {
                j.state = WAIT;
                false
            }
        });
        if ended {
            crate::store_writer::end_job(id);
        }
    }
}

// ---- the API ------------------------------------------------------------------------------------------------------------------

/// Create the task when it does not exist yet. The kernel call is made outside the interrupt mask: `TASK_UP` is 1 while one poster creates it
/// (another poster then gets `false`, `E_BUSY`: it asks again) and 2 once it exists.
unsafe fn ensure_task() -> bool {
    let first = locked(|| {
        let up = read_volatile(addr_of!(TASK_UP));
        if up == 0 {
            write_volatile(addr_of_mut!(TASK_UP), 1);
        }
        up
    });
    if first == 2 {
        return true;
    }
    if first == 1 {
        return false;
    }
    let ok = kernel::create();
    locked(|| write_volatile(addr_of_mut!(TASK_UP), if ok { 2 } else { 0 }));
    ok
}

/// Add a job. Returns its id (> 0), `E_CONTRACT` for a null step, `E_BUSY` when all entries are in use or another poster is creating the task,
/// `E_NOT_FOUND` when the kernel refused the task. Task context.
pub unsafe fn post(owner: u8, step: Option<IoStep>, done: Option<IoDone>, ctx: *mut u8) -> i32 {
    if step.is_none() {
        return E_CONTRACT;
    }
    if !ensure_task() {
        return if read_volatile(addr_of!(TASK_UP)) == 1 { E_BUSY } else { E_NOT_FOUND };
    }
    let id = locked(|| {
        let mut i = 0;
        while i < JOBS {
            if TABLE[i].state == FREE {
                NEXT_ID = NEXT_ID.wrapping_add(1) & 0x7fff_ffff;
                if NEXT_ID == 0 {
                    NEXT_ID = 1;
                }
                TABLE[i] = Job { id: NEXT_ID, owner, step, done, ctx, state: RUN, woken: false, cancelled: false, quiet: false, status: 0, seq: next_seq() };
                return NEXT_ID as i32;
            }
            i += 1;
        }
        E_BUSY
    });
    if id > 0 {
        kernel::post();
    }
    id
}

/// Cancel a job of `owner`: a job that waits or sleeps never runs again and gets `done(IO_E_CANCELLED)` at the next UI pass; a running one
/// stops after its step returns. Returns 0, or `E_NOT_FOUND` for an unknown id or another owner's job.
pub unsafe fn cancel(owner: u8, id: i32) -> i32 {
    if id <= 0 {
        return E_NOT_FOUND;
    }
    let found = locked(|| {
        let mut i = 0;
        while i < JOBS {
            let j = &mut TABLE[i];
            if j.state != FREE && j.state != END && j.id == id as u32 && j.owner == owner {
                j.cancelled = true;
                if j.state != STEP {
                    j.status = IO_E_CANCELLED;
                    j.state = END;
                }
                return Some(j.state == END);
            }
            i += 1;
        }
        None
    });
    match found {
        Some(ended) => {
            if ended {
                crate::store_writer::end_job(id as u32);
            }
            OK
        }
        None => E_NOT_FOUND,
    }
}

/// Wake the job `id` (any owner): its next turn comes, or, when its step is still running or it was not yet run, its next `IO_WAIT` does not
/// sleep. Unknown ids are ignored. Safe in an interrupt handler.
pub fn wake(id: i32) {
    if id <= 0 {
        return;
    }
    let hit = unsafe {
        locked(|| {
            let mut i = 0;
            while i < JOBS {
                let j = &mut TABLE[i];
                if j.id == id as u32 && (j.state == RUN || j.state == WAIT || j.state == STEP) {
                    if j.state == WAIT {
                        j.state = RUN;
                        j.seq = next_seq();
                    } else {
                        j.woken = true;
                    }
                    return true;
                }
                i += 1;
            }
            false
        })
    };
    if hit {
        unsafe { kernel::post() };
    }
}

/// `dkrt_io_wake(id)`: `wake` for an adapter (core/usb-bulk calls it from the USB interrupt when a buffer is full). Safe in an interrupt handler.
#[no_mangle]
pub extern "C" fn dkrt_io_wake(id: i32) {
    wake(id)
}

/// Loader calls it before an unload frees `rec`'s memory. The owner's waiting, sleeping and finished jobs are dropped without `done`. False: its
/// step is running; the job is cancelled and gets no `done`, and the caller keeps the module memory (`E_BUSY`) until a later call returns true.
pub unsafe fn retire(rec: usize) -> bool {
    let owner = rec as u8 + 1;
    let mut dropped = [0u32; JOBS];
    let (busy, n) = locked(|| {
        let mut busy = false;
        let mut n = 0;
        let mut i = 0;
        while i < JOBS {
            let j = &mut TABLE[i];
            if j.state != FREE && j.owner == owner {
                if j.state == STEP {
                    j.cancelled = true;
                    j.quiet = true;
                    busy = true;
                } else {
                    dropped[n] = j.id;
                    n += 1;
                    *j = EMPTY;
                }
            }
            i += 1;
        }
        (busy, n)
    });
    let mut k = 0;
    while k < n {
        crate::store_writer::end_job(dropped[k]);
        k += 1;
    }
    !busy
}

/// UI task, once per pass: call `done` of the finished jobs.
pub unsafe fn tick() {
    let mut i = 0;
    while i < JOBS {
        let ended = locked(|| {
            let j = &mut TABLE[i];
            if j.state == END {
                let c = *j;
                *j = EMPTY;
                Some(c)
            } else {
                None
            }
        });
        if let Some(job) = ended {
            deliver(job);
        }
        i += 1;
    }
}

/// Call `done` of a freed entry in the UI task with the module's source set.
unsafe fn deliver(job: Job) {
    if job.quiet {
        return;
    }
    if let Some(done) = job.done {
        if job.owner != 0 {
            let prev = crate::dlog::enter_module(job.owner as usize - 1);
            done(job.ctx, job.status);
            crate::dlog::leave_module(prev);
        } else {
            done(job.ctx, job.status);
        }
    }
}

/// Host tests of other files share the one queue: they hold this lock.
#[cfg(test)]
pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use super::TEST_LOCK as LOCK;

    struct Script {
        tag: u32,
        calls: u32,
        /// Steps that return `IO_MORE` before the job finishes with `end`.
        more: u32,
        end: i32,
        /// What the step returns after `more` steps instead of `end`, when not 0 (`IO_WAIT`: sleeps until a wake, then ends).
        wait_after: bool,
        owner_seen: Option<u8>,
        /// The step meets a busy store (`note_busy`) in its first call.
        busy_first: bool,
        /// The step asks the loader to retire this record (an unload while the step runs) and notes the answer.
        retire_rec: Option<usize>,
        retire_seen: Option<bool>,
        done_calls: u32,
        status: i32,
    }

    static mut ORDER: [u32; 32] = [0; 32];
    static mut ORDER_N: usize = 0;

    fn script(tag: u32, more: u32) -> Script {
        Script { tag, calls: 0, more, end: 0, wait_after: false, owner_seen: None, busy_first: false, retire_rec: None, retire_seen: None, done_calls: 0, status: 99 }
    }

    unsafe extern "C" fn step(ctx: *mut u8) -> i32 {
        let s = &mut *(ctx as *mut Script);
        s.calls += 1;
        ORDER[ORDER_N] = s.tag;
        ORDER_N += 1;
        s.owner_seen = step_owner();
        if s.busy_first && s.calls == 1 {
            note_busy();
        }
        if let Some(rec) = s.retire_rec {
            s.retire_seen = Some(crate::loader::retire_sdk(rec));
        }
        if s.calls <= s.more {
            IO_MORE
        } else if s.wait_after && s.calls == s.more + 1 {
            IO_WAIT
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
        STEP_ID = 0;
        TASK_UP = 0;
        TEST_REFUSE = false;
        TEST_CREATES = 0;
        TEST_POSTS = 0;
        TEST_PAUSES = 0;
        TEST_CURRENT = io_tcb();
        STEP_BUSY = false;
        ORDER_N = 0;
    }

    fn ptr(s: &mut Script) -> *mut u8 {
        s as *mut Script as *mut u8
    }

    #[test]
    fn full_table_is_busy_null_step_is_refused_and_the_task_is_made_once() {
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
            assert_eq!((TEST_CREATES, TEST_POSTS), (1, JOBS as u32));
        }
    }

    #[test]
    fn a_refused_task_leaves_no_job() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            TEST_REFUSE = true;
            let mut s = script(1, 0);
            assert_eq!(post(0, Some(step), Some(done), ptr(&mut s)), E_NOT_FOUND);
            assert_eq!(TABLE[0].state, FREE);
            TEST_REFUSE = false;
            assert!(post(0, Some(step), Some(done), ptr(&mut s)) > 0);
        }
    }

    #[test]
    fn more_twice_then_done_once_and_the_step_knows_its_owner() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let mut s = script(1, 2);
            assert!(post(3, Some(step), Some(done), ptr(&mut s)) > 0);
            assert_eq!(step_owner(), None);
            drain();
            assert_eq!((s.calls, s.done_calls, s.owner_seen), (3, 0, Some(3)));
            assert_eq!(step_owner(), None);
            tick();
            assert_eq!((s.done_calls, s.status), (1, 0));
            tick();
            assert_eq!(s.done_calls, 1);
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
            drain();
            tick();
            assert_eq!((s.done_calls, s.status), (1, -5));
        }
    }

    #[test]
    fn jobs_take_turns() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let mut a = script(1, 2);
            let mut b = script(2, 2);
            post(0, Some(step), Some(done), ptr(&mut a));
            post(0, Some(step), Some(done), ptr(&mut b));
            drain();
            assert_eq!(&ORDER[..6], &[1, 2, 1, 2, 1, 2]);
        }
    }

    #[test]
    fn a_waiting_job_sleeps_until_the_wake_and_a_wake_before_the_wait_is_not_lost() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let mut a = script(1, 0);
            a.wait_after = true;
            let ida = post(0, Some(step), Some(done), ptr(&mut a));
            drain();
            assert_eq!(a.calls, 1);
            assert_eq!(TABLE[0].state, WAIT);
            drain();
            assert_eq!(a.calls, 1, "it sleeps");
            let posts = TEST_POSTS;
            wake(ida);
            assert_eq!(TEST_POSTS, posts + 1, "a wake posts the semaphore");
            wake(ida + 100);
            assert_eq!(TEST_POSTS, posts + 1, "an unknown id is ignored");
            drain();
            assert_eq!(a.calls, 2);
            tick();
            assert_eq!(a.done_calls, 1);
            // a wake that comes while the job has not run yet is kept for its wait
            let mut b = script(2, 0);
            b.wait_after = true;
            let idb = post(0, Some(step), Some(done), ptr(&mut b));
            wake(idb);
            drain();
            assert_eq!(b.calls, 2, "the early wake kept it awake");
        }
    }

    #[test]
    fn cancel_a_sleeping_job_calls_done_once_and_a_running_one_stops_after_its_step() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let mut a = script(1, 0);
            a.wait_after = true;
            let ida = post(3, Some(step), Some(done), ptr(&mut a));
            drain();
            assert_eq!(cancel(4, ida), E_NOT_FOUND, "another owner");
            assert_eq!(cancel(3, 0x7000), E_NOT_FOUND);
            assert_eq!(cancel(3, ida), OK);
            assert_eq!(a.done_calls, 0);
            tick();
            assert_eq!((a.done_calls, a.status), (1, IO_E_CANCELLED));
            assert_eq!(cancel(3, ida), E_NOT_FOUND, "gone");
            // cancelled while its step runs: the step is the one that cancels it
            let mut b = script(2, 100);
            let idb = post(3, Some(step), Some(done), ptr(&mut b));
            TABLE[0].cancelled = true; // what `cancel` does for a running step
            let _ = idb;
            drain();
            assert_eq!(b.calls, 1);
            tick();
            assert_eq!((b.done_calls, b.status), (1, IO_E_CANCELLED));
        }
    }

    #[test]
    fn retire_drops_the_owners_jobs_and_waits_for_the_running_step() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let mut a = script(1, 0);
            a.wait_after = true;
            let mut b = script(2, 0);
            let mut c = script(3, 0);
            post(3, Some(step), Some(done), ptr(&mut a)); // owner 3 = record 2
            post(3, Some(step), Some(done), ptr(&mut b));
            post(5, Some(step), Some(done), ptr(&mut c));
            // the owner's step is running: the table says so
            TABLE[1].state = STEP;
            assert!(!retire(2));
            assert_eq!(TABLE[0].state, FREE, "the sleeping one is dropped");
            assert!(TABLE[1].cancelled && TABLE[1].quiet);
            TABLE[1].state = END; // the step returned
            assert!(retire(2));
            drain();
            tick();
            assert_eq!((a.done_calls, b.done_calls, b.calls), (0, 0, 0));
            assert_eq!((c.done_calls, c.status), (1, 0));
        }
    }

    #[test]
    fn a_step_is_a_file_context_only_in_the_io_task() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let mut a = script(1, 0);
            post(3, Some(step), Some(done), ptr(&mut a));
            TEST_CURRENT = io_tcb().wrapping_add(0x100); // the UI or MIDI task runs while the step is "running": not the step
            drain();
            assert_eq!(a.owner_seen, None);
            reset();
            let mut b = script(2, 0);
            post(3, Some(step), Some(done), ptr(&mut b));
            drain();
            assert_eq!(b.owner_seen, Some(3));
            assert_eq!(step_owner(), None, "outside a step there is no owner");
        }
    }

    #[test]
    fn a_step_that_met_a_busy_store_sleeps_before_it_runs_again() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let mut a = script(1, 2);
            a.busy_first = true;
            post(0, Some(step), Some(done), ptr(&mut a));
            drain();
            assert_eq!((a.calls, TEST_PAUSES), (3, 1), "one pause: after the busy step only");
            reset();
            let mut b = script(2, 5);
            post(0, Some(step), Some(done), ptr(&mut b));
            drain();
            assert_eq!((b.calls, TEST_PAUSES), (6, 0), "no pause for steps that never met a busy store");
        }
    }

    #[test]
    fn a_rollback_while_a_step_runs_is_refused_until_it_has_returned() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            let mut a = script(1, 0);
            a.retire_rec = Some(2); // owner 3
            post(3, Some(step), Some(done), ptr(&mut a));
            drain();
            assert_eq!(a.retire_seen, Some(false), "the loader must keep the module's memory while its step runs");
            assert!(crate::loader::retire_sdk(2), "the step has returned (the job is cancelled): the memory may go");
            tick();
            assert_eq!(a.done_calls, 0, "no done after a retire");
        }
    }

    #[test]
    fn a_second_poster_while_the_task_is_being_made_is_busy() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            reset();
            TASK_UP = 1;
            let mut a = script(1, 0);
            assert_eq!(post(0, Some(step), Some(done), ptr(&mut a)), E_BUSY);
            TASK_UP = 0;
            assert!(post(0, Some(step), Some(done), ptr(&mut a)) > 0);
            assert_eq!(TASK_UP, 2);
        }
    }
}
