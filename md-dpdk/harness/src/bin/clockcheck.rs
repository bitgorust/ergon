//! The benchmark clocks on this host: what each costs to read, and how far the
//! calibrated TSC (`lab::Clock`) drifts from `CLOCK_MONOTONIC`.
//!
//! clockcheck [seconds]   (drift checkpoints up to `seconds`, default 60)
//!
//! Prints `clock,<name>,read_ns,<mean>,delta_ns,<p50>,<p99>,<max>` per clock:
//! the mean cost of one read over 10 M back-to-back reads, and the
//! distribution of the gap between consecutive reads in a second pass that
//! also records each gap (resolution plus cost plus the recording, plus
//! anything that interrupts the thread). Then
//! `drift,<seconds>,<tsc ns>,<monotonic ns>,<ppm>,minstant_ppm,<ppm>,raw_ppm,<ppm>,
//! mono_vs_raw_ppm,<ppm>` at each checkpoint: the calibrated TSC's and
//! `minstant`'s drift against `CLOCK_MONOTONIC`, the TSC's against
//! `CLOCK_MONOTONIC_RAW`, and NTP's slew of `CLOCK_MONOTONIC` itself; and
//! `host,...` lines: the invariant-TSC flags and the kernel clock source.
//! Run it pinned (`taskset`) on an idle CPU.

#[path = "../lab.rs"]
#[allow(dead_code)]
mod lab;

use hdrhistogram::Histogram;
use lab::{Clock, invariant_tsc, rdtscp};
use std::hint::black_box;
use std::time::{Duration, Instant};

const READS: u64 = 10_000_000;

fn measure(name: &str, ns_per_unit: f64, mut read: impl FnMut() -> u64) {
    for _ in 0..1_000_000 {
        black_box(read());
    }
    let start = Instant::now();
    for _ in 0..READS {
        black_box(read());
    }
    let mean = start.elapsed().as_nanos() as f64 / READS as f64;

    let mut gaps = Histogram::<u64>::new_with_bounds(1, 10_000_000_000, 3).unwrap();
    let mut last = read();
    for _ in 0..READS {
        let now = read();
        gaps.saturating_record(((now.wrapping_sub(last)) as f64 * ns_per_unit) as u64);
        last = now;
    }
    println!(
        "clock,{name},read_ns,{mean:.1},delta_ns,{},{},{}",
        gaps.value_at_quantile(0.5),
        gaps.value_at_quantile(0.99),
        gaps.max()
    );
}

fn main() {
    let seconds: u64 = std::env::args().nth(1).map_or(60, |s| s.parse().expect("seconds"));
    println!("host,invariant_tsc,{}", invariant_tsc());
    println!(
        "host,clocksource,{}",
        std::fs::read_to_string("/sys/devices/system/clocksource/clocksource0/current_clocksource")
            .unwrap_or_default()
            .trim()
    );

    let origin = Instant::now();
    measure("clock_gettime_monotonic", 1.0, || origin.elapsed().as_nanos() as u64);
    measure("aeron_nano_clock", 1.0, || rusteron_client::Aeron::nano_clock() as u64);
    let tsc = Clock::calibrate();
    if tsc.is_tsc() {
        // ticks to ns through the calibrated factor
        let ns_per_tick = tsc.ns(1 << 32) as f64 / (1u64 << 32) as f64;
        println!("host,tsc_ghz,{:.4}", 1.0 / ns_per_tick);
        #[cfg(target_arch = "x86_64")]
        measure("rdtsc", ns_per_tick, || {
            // SAFETY: rdtsc only reads the time-stamp counter.
            #[allow(unused_unsafe)]
            unsafe {
                core::arch::x86_64::_rdtsc()
            }
        });
        measure("rdtscp", ns_per_tick, rdtscp);
        measure("tsc_clock_ns", 1.0, || tsc.ns(tsc.ticks()));
        let anchor = minstant::Instant::now();
        measure("minstant", 1.0, || anchor.elapsed().as_nanos() as u64);

        // drift of the calibrated TSC against CLOCK_MONOTONIC
        let (t0, m0, i0, r0) = (tsc.ticks(), minstant::Instant::now(), Instant::now(), lab::raw_ns());
        for checkpoint in [1u64, 10, 30, 60, 120, 300, 600].into_iter().filter(|&c| c <= seconds) {
            while i0.elapsed() < Duration::from_secs(checkpoint) {
                std::thread::sleep(Duration::from_millis(1));
            }
            let (t, m, i, r) = (tsc.ticks(), minstant::Instant::now(), Instant::now(), lab::raw_ns());
            let tsc_ns = tsc.ns(t - t0) as f64;
            let minstant_ns = (m - m0).as_nanos() as f64;
            let mono_ns = (i - i0).as_nanos() as f64;
            let raw = (r - r0) as f64;
            println!(
                "drift,{checkpoint},{tsc_ns:.0},{mono_ns:.0},{:.3},minstant_ppm,{:.3},raw_ppm,{:.3},mono_vs_raw_ppm,{:.3}",
                (tsc_ns - mono_ns) / mono_ns * 1e6,
                (minstant_ns - mono_ns) / mono_ns * 1e6,
                (tsc_ns - raw) / raw * 1e6,
                (mono_ns - raw) / raw * 1e6
            );
        }
    } else {
        println!("host,tsc,unavailable");
    }
}
