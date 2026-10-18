//! The system calls the benchmarks need beyond ergon-clock's.
//!
//! Pinning a thread, its context switches, and the kernel clocks by id: the
//! bench crate's only `unsafe`.
#![allow(unsafe_code)]

/// Pins the calling thread to `cpu` (Linux `sched_setaffinity`).
///
/// Elsewhere (macOS has no CPU affinity) it does nothing and says so
/// (`Ok(false)`).
///
/// # Errors
/// The kernel refused (a CPU that does not exist, or is not allowed).
#[allow(clippy::missing_const_for_fn)] // const only where the Linux body is compiled out
pub fn pin(cpu: usize) -> std::io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        if cpu >= 1024 {
            return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
        }
        // SAFETY: a zeroed cpu_set_t is a valid empty set; CPU_SET stays in
        // bounds for cpu < CPU_SETSIZE (1024), checked above.
        let rc = unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_SET(cpu, &mut set);
            libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &raw const set)
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(true)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = cpu;
        Ok(false)
    }
}

/// The CPUs the calling thread may run on (Linux `sched_getaffinity`),
/// ascending; empty elsewhere (macOS has no CPU affinity) or where the
/// kernel refused.
#[must_use]
#[allow(clippy::missing_const_for_fn)] // const only where the Linux body is compiled out
pub fn allowed_cpus() -> Vec<usize> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: a zeroed cpu_set_t is a valid empty set, and the kernel
        // writes at most its size.
        let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        // SAFETY: a valid pointer to a cpu_set_t of the size given.
        let rc = unsafe {
            libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &raw mut set)
        };
        if rc != 0 {
            return Vec::new();
        }
        // SAFETY: CPU_ISSET reads a set the kernel filled, cpu < CPU_SETSIZE.
        (0..1024)
            .filter(|&cpu| unsafe { libc::CPU_ISSET(cpu, &set) })
            .collect()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

/// The calling thread's voluntary and involuntary context switches so far.
///
/// Linux `getrusage(RUSAGE_THREAD)`; zeros elsewhere. A measurement's growth
/// in them says whether the thread slept or was preempted in it.
#[must_use]
#[allow(clippy::missing_const_for_fn)] // const only where the Linux body is compiled out
pub fn context_switches() -> (u64, u64) {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: a zeroed rusage is a valid out-parameter.
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: a valid pointer to a rusage.
        if unsafe { libc::getrusage(libc::RUSAGE_THREAD, &raw mut ru) } == 0 {
            return (
                u64::try_from(ru.ru_nvcsw).unwrap_or(0),
                u64::try_from(ru.ru_nivcsw).unwrap_or(0),
            );
        }
    }
    (0, 0)
}

/// The page faults served without I/O so far.
///
/// `getrusage`'s `ru_minflt`: the calling thread's on Linux
/// (`RUSAGE_THREAD`), the process's elsewhere. A page's first touch is one;
/// a measurement's growth in them says whether it paid for its own memory.
#[must_use]
pub fn minor_faults() -> u64 {
    #[cfg(target_os = "linux")]
    let who = libc::RUSAGE_THREAD;
    #[cfg(not(target_os = "linux"))]
    let who = libc::RUSAGE_SELF;
    // SAFETY: a zeroed rusage is a valid out-parameter.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: a valid pointer to a rusage.
    if unsafe { libc::getrusage(who, &raw mut ru) } == 0 {
        return u64::try_from(ru.ru_minflt).unwrap_or(0);
    }
    0
}

/// `clock_gettime(id)` in ns (0 on failure: the rows that read it are
/// checked for a sane rate first).
#[inline]
#[must_use]
pub fn clock_ns(id: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: a valid, writable timespec.
    unsafe { libc::clock_gettime(id, &raw mut ts) };
    u64::try_from(ts.tv_sec)
        .unwrap_or(0)
        .wrapping_mul(1_000_000_000)
        .wrapping_add(u64::try_from(ts.tv_nsec).unwrap_or(0))
}

/// The kernel's release (`uname -r`).
#[must_use]
pub fn kernel() -> String {
    // SAFETY: a zeroed utsname is a valid out-parameter.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: a valid pointer to a utsname.
    if unsafe { libc::uname(&raw mut u) } != 0 {
        return "-".to_owned();
    }
    let release: Vec<u8> = u
        .release
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c.to_ne_bytes()[0])
        .collect();
    String::from_utf8_lossy(&release).into_owned()
}
