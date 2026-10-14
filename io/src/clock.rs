//! Clocks, CPU pinning and per-thread accounting.
//!
//! Kernel receive timestamps (`SO_TIMESTAMPING`) are `CLOCK_REALTIME`, so
//! [`realtime_ns`] (vDSO, no syscall on a TSC clocksource) is the clock to compare
//! them with. [`rdtsc`] is cheaper but in another clock domain; use it for
//! durations only.
#![allow(unsafe_code)]

/// `CLOCK_REALTIME` in nanoseconds.
#[must_use]
pub fn realtime_ns() -> i64 {
    clock_ns(libc::CLOCK_REALTIME)
}

/// `CLOCK_MONOTONIC` in nanoseconds.
#[must_use]
pub fn monotonic_ns() -> i64 {
    clock_ns(libc::CLOCK_MONOTONIC)
}

fn clock_ns(id: libc::clockid_t) -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec.
    unsafe { libc::clock_gettime(id, &raw mut ts) };
    ts.tv_sec * 1_000_000_000 + ts.tv_nsec
}

/// Raw counter: `rdtsc` on x86-64, `cntvct_el0` on aarch64, 0 elsewhere.
#[must_use]
#[inline]
pub fn rdtsc() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: RDTSC has no preconditions on x86-64.
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    #[cfg(target_arch = "aarch64")]
    {
        let v: u64;
        // SAFETY: CNTVCT_EL0 is readable from EL0 on Linux and macOS.
        unsafe { core::arch::asm!("mrs {}, cntvct_el0", out(reg) v, options(nomem, nostack)) };
        v
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        0
    }
}

/// [`rdtsc`] ticks per microsecond, measured once against `CLOCK_MONOTONIC` over
/// 20 ms and cached. Call it before any timed loop so the first call does not land
/// inside one.
#[must_use]
pub fn ticks_per_us() -> f64 {
    static TPU: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *TPU.get_or_init(|| {
        let (m0, t0) = (monotonic_ns(), rdtsc());
        while monotonic_ns() - m0 < 20_000_000 {
            std::hint::spin_loop();
        }
        let (m1, t1) = (monotonic_ns(), rdtsc());
        #[allow(clippy::cast_precision_loss)]
        let tpu = t1.wrapping_sub(t0) as f64 / ((m1 - m0) as f64 / 1000.0);
        tpu.max(1e-3)
    })
}

/// Pins the calling thread to one CPU (Linux; no-op elsewhere).
///
/// # Errors
/// `sched_setaffinity` fails.
#[allow(clippy::missing_const_for_fn)] // const only where the body is compiled out
pub fn pin_current_thread(cpu: usize) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: zeroed cpu_set_t is valid; CPU_SET stays in bounds for cpu < 1024.
        let rc = unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_SET(cpu, &mut set);
            libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &raw const set)
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = cpu;
    Ok(())
}

/// Voluntary and involuntary context switches of the calling thread (Linux;
/// zeros elsewhere). A voluntary switch means the thread slept in the kernel.
#[must_use]
#[allow(clippy::missing_const_for_fn)] // const only where the body is compiled out
pub fn thread_ctx_switches() -> (u64, u64) {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: zeroed rusage is a valid out-parameter.
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: valid pointer to a rusage.
        if unsafe { libc::getrusage(libc::RUSAGE_THREAD, &raw mut ru) } == 0 {
            return (
                u64::try_from(ru.ru_nvcsw).unwrap_or(0),
                u64::try_from(ru.ru_nivcsw).unwrap_or(0),
            );
        }
    }
    (0, 0)
}

/// User and system CPU time of the calling thread in clock ticks, from
/// `/proc/thread-self/stat` (zeros when unavailable).
#[must_use]
pub fn thread_cpu_ticks() -> (u64, u64) {
    let Ok(s) = std::fs::read_to_string("/proc/thread-self/stat") else {
        return (0, 0);
    };
    let Some(close) = s.rfind(')') else {
        return (0, 0);
    };
    let rest: Vec<&str> = s[close + 1..].split_whitespace().collect();
    let field = |i: usize| rest.get(i).and_then(|v| v.parse().ok()).unwrap_or(0);
    (field(11), field(12))
}
