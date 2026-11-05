//! CPU time consumed by the calling thread (exact, from the kernel's
//! per-thread accounting; not a sampling profiler).

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod imp {
    #[repr(C)]
    struct Timespec {
        sec: i64,
        nsec: i64,
    }
    unsafe extern "C" {
        fn clock_gettime(clock: i32, ts: *mut Timespec) -> i32;
    }
    #[cfg(target_os = "macos")]
    const CLOCK_THREAD_CPUTIME_ID: i32 = 16;
    #[cfg(target_os = "linux")]
    const CLOCK_THREAD_CPUTIME_ID: i32 = 3;

    pub fn thread_cpu_ns() -> u64 {
        let mut ts = Timespec { sec: 0, nsec: 0 };
        // SAFETY: `ts` is a valid, writable timespec for the call.
        if unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut ts) } != 0 {
            return 0;
        }
        (ts.sec as u64)
            .saturating_mul(1_000_000_000)
            .saturating_add(ts.nsec as u64)
    }

    #[cfg(target_os = "macos")]
    unsafe extern "C" {
        fn thread_selfcounts(kind: i32, buf: *mut std::ffi::c_void, nbytes: usize) -> i32;
    }

    #[cfg(target_os = "macos")]
    pub fn thread_counters() -> (u64, u64) {
        let mut buf = [0u64; 2];
        // SAFETY: `buf` is a valid, writable buffer of two u64 for the call.
        let rc = unsafe { thread_selfcounts(1, buf.as_mut_ptr().cast(), size_of::<[u64; 2]>()) };
        if rc != 0 {
            return (0, 0);
        }
        (buf[0], buf[1])
    }

    #[cfg(not(target_os = "macos"))]
    pub fn thread_counters() -> (u64, u64) {
        (0, 0)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    pub fn thread_cpu_ns() -> u64 {
        0
    }

    pub fn thread_counters() -> (u64, u64) {
        (0, 0)
    }
}

/// Hardware counters of the calling thread: (cycles, retired instructions).
/// (0, 0) where unavailable.
pub fn thread_counters() -> (u64, u64) {
    imp::thread_counters()
}

/// Nanoseconds of CPU time (user + system) the calling thread has used; 0 where
/// the platform has no per-thread clock.
pub fn thread_cpu_ns() -> u64 {
    imp::thread_cpu_ns()
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    #[test]
    fn thread_counters_advance_on_macos() {
        let (c0, i0) = super::thread_counters();
        let mut x = 0u64;
        for _ in 0..5_000_000u64 {
            x = x.wrapping_add(std::hint::black_box(1));
        }
        let (c1, i1) = super::thread_counters();
        assert!(x > 0);
        assert!(c1 > c0, "{c0} {c1}");
        assert!(i1 > i0, "{i0} {i1}");
    }

    #[test]
    fn counts_own_busy_time_and_not_sleep() {
        let start = super::thread_cpu_ns();
        let t = std::time::Instant::now();
        let mut x = 0u64;
        // Spin until the thread itself has used 30 ms of CPU (capped by wall time), so a loaded machine
        // (parallel verify) cannot starve the loop below the 20 ms bound.
        while (start == 0 || super::thread_cpu_ns() - start < 30_000_000)
            && t.elapsed() < std::time::Duration::from_secs(5)
        {
            x = x.wrapping_add(std::hint::black_box(1));
            if start == 0 && t.elapsed() >= std::time::Duration::from_millis(30) {
                break;
            }
        }
        let busy = super::thread_cpu_ns() - start;
        std::thread::sleep(std::time::Duration::from_millis(60));
        let after_sleep = super::thread_cpu_ns() - start;
        assert!(x > 0);
        if start != 0 {
            assert!(busy >= 20_000_000, "{busy}");
            assert!(after_sleep - busy < 20_000_000, "{busy} {after_sleep}");
        }
    }
}
