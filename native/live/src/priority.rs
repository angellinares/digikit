//! Scheduling for the render thread (an OS edge).
//!
//! macOS has two levels:
//! - [`raise_current_thread`]: `pthread_set_qos_class_self_np(
//!   QOS_CLASS_USER_INTERACTIVE)`, the class the scheduler keeps on
//!   performance cores ahead of default-class work.
//! - [`realtime_current_thread`]: the Mach time-constraint policy
//!   (`THREAD_TIME_CONSTRAINT_POLICY`), the class CoreAudio's own I/O
//!   threads run in: scheduled ahead of every timeshare thread, so a busy
//!   machine does not preempt rendering.
//!
//! Elsewhere both do nothing and return false.

#[cfg(target_os = "macos")]
mod mac {
    #[repr(C)]
    #[derive(Default)]
    pub struct Timebase {
        pub numer: u32,
        pub denom: u32,
    }

    #[repr(C)]
    pub struct TimeConstraint {
        pub period: u32,
        pub computation: u32,
        pub constraint: u32,
        pub preemptible: i32,
    }

    unsafe extern "C" {
        pub fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
        pub fn mach_thread_self() -> u32;
        pub fn mach_timebase_info(info: *mut Timebase) -> i32;
        pub fn thread_policy_set(thread: u32, flavor: u32, info: *const i32, count: u32) -> i32;
        pub fn mach_port_deallocate(task: u32, name: u32) -> i32;
        pub static mach_task_self_: u32;
    }

    pub const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
    pub const THREAD_TIME_CONSTRAINT_POLICY: u32 = 2;
    pub const THREAD_TIME_CONSTRAINT_POLICY_COUNT: u32 = 4;
}

/// Interactive QoS for the calling thread. True when it was set.
pub fn raise_current_thread() -> bool {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: a plain libSystem call on the current thread.
        unsafe { mac::pthread_set_qos_class_self_np(mac::QOS_CLASS_USER_INTERACTIVE, 0) == 0 }
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// The time-constraint (real-time) policy for the calling thread: it
/// expects about COMPUTATION_NS of work every PERIOD_NS, finished within
/// CONSTRAINT_NS of its start. True when it was set.
pub fn realtime_current_thread(period_ns: u64, computation_ns: u64, constraint_ns: u64) -> bool {
    #[cfg(target_os = "macos")]
    {
        let mut tb = mac::Timebase::default();
        // SAFETY: TB is a valid out-parameter.
        if unsafe { mac::mach_timebase_info(&mut tb) } != 0 || tb.numer == 0 {
            return false;
        }
        let abs = |ns: u64| {
            (ns as u128 * tb.denom as u128 / tb.numer as u128).min(u32::MAX as u128) as u32
        };
        let policy = mac::TimeConstraint {
            period: abs(period_ns),
            computation: abs(computation_ns),
            constraint: abs(constraint_ns),
            preemptible: 1,
        };
        // SAFETY: POLICY is THREAD_TIME_CONSTRAINT_POLICY_COUNT integers;
        // the thread port is this thread's own and released afterwards.
        unsafe {
            let port = mac::mach_thread_self();
            let rc = mac::thread_policy_set(
                port,
                mac::THREAD_TIME_CONSTRAINT_POLICY,
                &policy as *const mac::TimeConstraint as *const i32,
                mac::THREAD_TIME_CONSTRAINT_POLICY_COUNT,
            );
            mac::mach_port_deallocate(mac::mach_task_self_, port);
            rc == 0
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (period_ns, computation_ns, constraint_ns);
        false
    }
}
