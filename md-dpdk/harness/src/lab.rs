//! Helpers shared by the `rtt` and `tput` benchmark clients (ported from
//! rusteron's x86 lab so the kernel-UDP and DPDK numbers come from the same
//! client code as rusteron's published ones).

use std::ffi::CString;
use std::time::{Duration, Instant};

/// Pins the calling thread to `cpu`; `None` leaves it where it is.
#[cfg(target_os = "linux")]
pub fn pin(cpu: Option<usize>) {
    let Some(cpu) = cpu else { return };
    // SAFETY: a zeroed cpu_set_t is the empty set, and the call only reads it.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu, &mut set);
        let rc = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
        assert_eq!(rc, 0, "could not pin to cpu {cpu}");
    }
}

/// Thread pinning is Linux only; elsewhere the clients run unpinned (smoke tests).
#[cfg(not(target_os = "linux"))]
pub fn pin(_cpu: Option<usize>) {}

/// `-` or a CPU number.
pub fn cpu_arg(arg: Option<String>) -> Option<usize> {
    arg.filter(|a| a != "-").map(|a| a.parse().expect("cpu number or -"))
}

/// The external driver's dir, from `AERON_DIR`.
pub fn aeron_dir() -> CString {
    CString::new(std::env::var("AERON_DIR").expect("AERON_DIR: the external driver's dir")).unwrap()
}

/// `LABEL`, which names the run in the result line.
pub fn label() -> String {
    std::env::var("LABEL").unwrap_or_else(|_| "-".into())
}

/// Sleeps on `ready` until it holds, failing after `secs`.
pub fn wait_until(what: &str, secs: u64, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The clients' clock: the invariant TSC read with `rdtscp` (which waits for
/// earlier instructions, so a stamp is not hoisted), scaled to nanoseconds by a
/// 32.32 factor calibrated against `CLOCK_MONOTONIC_RAW` at start.
/// Without an invariant TSC (`constant_tsc` and `nonstop_tsc`), off x86-64
/// (macOS smoke tests), or with `BENCH_CLOCK=monotonic` (to compare the two),
/// it counts `Instant` nanoseconds instead.
pub struct Clock {
    origin: Instant,
    /// Nanoseconds per tick in 32.32 fixed point; 0 when ticks are `Instant` ns.
    mult: u64,
}

impl Clock {
    /// Calibrates over 100 ms; call before the measured loop. The rate is
    /// measured against `CLOCK_MONOTONIC_RAW` (unslewed, like the TSC), each
    /// end taking the tightest of 16 bracketed reads, after a warm-up.
    pub fn calibrate() -> Self {
        let origin = Instant::now();
        let monotonic = std::env::var("BENCH_CLOCK").is_ok_and(|c| c == "monotonic");
        if monotonic || !invariant_tsc() {
            return Self { origin, mult: 0 };
        }
        let sample = || {
            let mut best = (u64::MAX, 0u64, 0u64);
            for _ in 0..16 {
                let t0 = rdtscp();
                let at = raw_ns();
                let t1 = rdtscp();
                if t1 - t0 < best.0 {
                    best = (t1 - t0, t0 + (t1 - t0) / 2, at);
                }
            }
            (best.1, best.2)
        };
        sample(); // warm-up: code, vDSO page
        let (tsc0, ns0) = sample();
        let (mut tsc1, mut ns1) = sample();
        while ns1 - ns0 < 100_000_000 {
            (tsc1, ns1) = sample();
        }
        let mult = u64::try_from((u128::from(ns1 - ns0) << 32) / u128::from(tsc1 - tsc0)).unwrap();
        Self { origin, mult }
    }

    /// Whether ticks are TSC (`true`) or `Instant` nanoseconds.
    pub fn is_tsc(&self) -> bool {
        self.mult != 0
    }

    /// The current tick.
    #[inline(always)]
    pub fn ticks(&self) -> u64 {
        if self.mult != 0 {
            rdtscp()
        } else {
            u64::try_from(self.origin.elapsed().as_nanos()).unwrap()
        }
    }

    /// Nanoseconds in `ticks` ticks.
    #[inline(always)]
    pub fn ns(&self, ticks: u64) -> u64 {
        if self.mult != 0 {
            ((u128::from(ticks) * u128::from(self.mult)) >> 32) as u64
        } else {
            ticks
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub fn rdtscp() -> u64 {
    let mut aux = 0u32;
    // SAFETY: rdtscp only reads the TSC and IA32_TSC_AUX; every x86-64 CPU this
    // runs on (invariant TSC checked first) implements it.
    #[allow(unused_unsafe)]
    unsafe {
        core::arch::x86_64::__rdtscp(&mut aux)
    }
}

#[cfg(not(target_arch = "x86_64"))]
pub fn rdtscp() -> u64 {
    0
}

/// `constant_tsc` and `nonstop_tsc` in the first `flags` line of /proc/cpuinfo.
pub fn invariant_tsc() -> bool {
    if !cfg!(target_arch = "x86_64") {
        return false;
    }
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    cpuinfo.lines().find(|l| l.starts_with("flags")).is_some_and(|l| {
        let flags: Vec<&str> = l.split_whitespace().collect();
        flags.contains(&"constant_tsc") && flags.contains(&"nonstop_tsc")
    })
}

/// `CLOCK_MONOTONIC_RAW` in ns: the unslewed clock the TSC is calibrated against.
#[cfg(target_os = "linux")]
pub fn raw_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime only writes the timespec it is given.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC_RAW, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Off Linux there is no TSC clock (macOS smoke tests); `Instant` stands in.
#[cfg(not(target_os = "linux"))]
pub fn raw_ns() -> u64 {
    use std::sync::OnceLock;
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed().as_nanos() as u64
}
