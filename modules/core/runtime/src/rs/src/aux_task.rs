//! The helper task: one RTOS task that runs one function at a time for a caller that blocks on something else meanwhile. The close's card read-back
//! (`store_writer`, and the +Drive adapter's close hash through `dkrt_aux_run`) reads piece n + 1 from the card, which blocks the I/O task on the card's
//! semaphore, while the helper hashes piece n. The task is created at the first `run`, with a static TCB and stack, and never deleted.
//!
//! One call at a time: `run` answers `E_BUSY` while a run is not yet collected with `wait`, and the caller then does the work itself. The function runs
//! with its own 4 KiB stack: no firmware calls, no waiting. Priority 3, the I/O task's (UI 6, MIDI 7): it never starves, and it runs while the I/O task sleeps.
//!
//! Rules: no `match` on integers, no `core::fmt`, no slice indexing.
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use dkrt_sdk::proto::E_STATE;
use dkrt_sdk::{E_BUSY, E_NOT_FOUND, OK};

const PRIORITY: u32 = 3;
const STACK_BYTES: usize = 0x1000;
const TCB_BYTES: usize = 0x60;

/// Stack, TCB and the two semaphores in one static (the kernel writes them from firmware code: the write-watch of the proofs leaves the object out,
/// tests/regression/runtime/write-safety.ts and tools/patchkit/pk_dkrt_machine.py: 4208 bytes). A semaphore is 8 bytes: a flag and the one waiting TCB.
#[repr(C, align(16))]
pub struct AuxTask {
    stack: [u8; STACK_BYTES],
    tcb: [u8; TCB_BYTES],
    work: [u32; 2],
    done: [u32; 2],
}
const _: () = assert!(core::mem::size_of::<AuxTask>() == 4208);
#[no_mangle]
pub static mut DKRT_AUX_TASK: AuxTask = AuxTask { stack: [0; STACK_BYTES], tcb: [0; TCB_BYTES], work: [0; 2], done: [0; 2] };

type Func = extern "C" fn(*mut u8);

/// 0: no task, 1: being created, 2: it exists.
static mut TASK_UP: u32 = 0;
/// 0: idle, 1: a run was started and not yet collected.
static mut BUSY: u32 = 0;
static mut FUNC: Option<Func> = None;
static mut CTX: *mut u8 = core::ptr::null_mut();

#[cfg(not(test))]
mod kernel {
    use super::*;
    use crate::PLATFORM;
    use dkrt_abi::platform::Tasking;

    pub unsafe fn create() -> bool {
        PLATFORM.sem_init(addr_of_mut!(DKRT_AUX_TASK.work) as *mut u32, 0);
        PLATFORM.sem_init(addr_of_mut!(DKRT_AUX_TASK.done) as *mut u32, 0);
        let tcb = addr_of_mut!(DKRT_AUX_TASK.tcb) as u32;
        if !PLATFORM.task_create(tcb, entry as usize as u32, PRIORITY, addr_of_mut!(DKRT_AUX_TASK.stack) as u32, STACK_BYTES as u32) {
            return false;
        }
        PLATFORM.task_start(tcb);
        true
    }
    pub unsafe fn pend_work() {
        PLATFORM.sem_pend(addr_of_mut!(DKRT_AUX_TASK.work) as *mut u32);
    }
    pub unsafe fn post_work() {
        PLATFORM.sem_post(addr_of_mut!(DKRT_AUX_TASK.work) as *mut u32);
    }
    pub unsafe fn pend_done() {
        PLATFORM.sem_pend(addr_of_mut!(DKRT_AUX_TASK.done) as *mut u32);
    }
    pub unsafe fn post_done() {
        PLATFORM.sem_post(addr_of_mut!(DKRT_AUX_TASK.done) as *mut u32);
    }
}

/// Host tests: no task; `run` calls the function at once and `wait` has nothing to wait for.
#[cfg(test)]
mod kernel {
    use super::*;
    pub unsafe fn create() -> bool {
        let _ = (entry as extern "C" fn() -> !, addr_of_mut!(DKRT_AUX_TASK), PRIORITY);
        true
    }
    pub unsafe fn pend_work() {}
    pub unsafe fn post_work() {
        if let Some(f) = read_volatile(addr_of!(FUNC)) {
            f(read_volatile(addr_of!(CTX)));
        }
    }
    pub unsafe fn pend_done() {}
    pub unsafe fn post_done() {}
}

extern "C" fn entry() -> ! {
    unsafe {
        loop {
            kernel::pend_work();
            if let Some(f) = read_volatile(addr_of!(FUNC)) {
                f(read_volatile(addr_of!(CTX)));
            }
            kernel::post_done();
        }
    }
}

unsafe fn locked<T>(f: impl FnOnce() -> T) -> T {
    dkrt_abi::platform::locked(&crate::PLATFORM, f)
}

/// Create the task when it does not exist yet (the same hand-over as `io::ensure_task`: a second caller meanwhile gets false).
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

/// Start `f(ctx)` on the helper task. `OK`, `E_BUSY` (a run is not collected yet) or `E_NOT_FOUND` (the kernel gave no task). Task context only; collect
/// with `wait`.
pub unsafe fn run(f: Func, ctx: *mut u8) -> i32 {
    if !ensure_task() {
        return E_NOT_FOUND;
    }
    let took = locked(|| {
        if read_volatile(addr_of!(BUSY)) != 0 {
            false
        } else {
            write_volatile(addr_of_mut!(BUSY), 1);
            write_volatile(addr_of_mut!(FUNC), Some(f));
            write_volatile(addr_of_mut!(CTX), ctx);
            true
        }
    });
    if !took {
        return E_BUSY;
    }
    kernel::post_work();
    OK
}

/// Wait until the run started with `run` has finished (the caller sleeps on the helper's semaphore). `E_STATE` when none was started.
pub unsafe fn wait() -> i32 {
    if read_volatile(addr_of!(BUSY)) == 0 {
        return E_STATE;
    }
    kernel::pend_done();
    write_volatile(addr_of_mut!(BUSY), 0);
    OK
}

/// Adapter entry: `run` with the address of `void f(u8 *ctx)`.
#[no_mangle]
pub unsafe extern "C" fn dkrt_aux_run(f: usize, ctx: *mut u8) -> i32 {
    match core::mem::transmute::<usize, Option<Func>>(f) {
        Some(f) => run(f, ctx),
        None => dkrt_sdk::E_CONTRACT,
    }
}

/// Adapter entry: `wait`.
#[no_mangle]
pub unsafe extern "C" fn dkrt_aux_wait() -> i32 {
    wait()
}
