# ergon-clock

[![Crates.io](https://img.shields.io/crates/v/ergon-clock)](https://crates.io/crates/ergon-clock)
[![API Docs](https://docs.rs/ergon-clock/badge.svg)](https://docs.rs/ergon-clock/)

A clock for busy-spin threads: UTC nanoseconds from the CPU's counter, one
counter read and one multiply per read.

> **AI assistance.** Like the rest of ergon, this crate was written **with heavy
> AI assistance**. See
> [AI-ASSISTANCE.md](https://github.com/mimran1980/ergon/blob/main/AI-ASSISTANCE.md).
> It is not production-ready.

## What it does

- **`calibrate`**: the one set-up call, at start-up (40–70 ms, plus up to
  50 ms for the time-sync probe). It checks that the counter may be used,
  takes its rate, and returns the clock with a report of what it chose and
  why. It prints one line per problem: a slow fallback, a host clock not
  synchronised or not settled.
- **`now_ns`**: UTC nanoseconds since 1970. One counter instruction
  (`rdtsc` on x86-64, `mrs cntvct_el0` on 64-bit Arm) and one multiply:
  10.92–15.16 ns (slowest batch 16.13) on c7i.2xlarge, c8g.xlarge and
  c9g.2xlarge (runs `aws-20261016-231320`, `aws-20261016-223331`,
  `aws-20261016-223251`), against 21.29–31.69 ns (33.35) for `Instant::now`.
- **`now_ns_ordered`**: the same read, taken only after all earlier work
  has finished (`rdtscp`; on Arm `isb` then the read, or `CNTVCTSS_EL0`).
- **`update`**: called from the loop's idle time. Once a second it keeps
  the clock on UTC and returns a health check. Corrections are slewed at
  most 0.5 ms a second, and never backwards.
- **`follower`**: a reader for each other thread. The clock itself is not
  `Send`: the thread that calibrated it updates it.
- **`ManualClock`** and the `TimeSource` trait: fake time for tests and
  replays.
- It depends on `libc` only.

## When to use it

| Use it for | Use something else for |
|---|---|
| UTC stamps and durations on hot threads: event time, timeouts, latency histograms | Under about 100,000 reads a second: `std::time::Instant` is enough for durations (it has no UTC) |
| Stamps compared with an exchange's, a NIC's or another host's | Dates and time zones: `std::time::SystemTime` and a calendar crate |
| Code that must run the same live, in a replay and in a test | Stamps compared across hosts more closely than their time sync allows: nothing local can give you that |

## Which read

| read | for |
|---|---|
| `now_ns` | event stamps, deadlines, ages, the start of a measurement |
| `now_ns_ordered` | the end of a measurement; stamps that must never be earlier than another thread's (a follower can still be about 1 ns early at an update) |

`now_ns` may be read before earlier work finishes, so a measurement that
ends with it comes out too short. Neither read stops the compiler moving
work that touches no memory: pass the work's input and result through
`std::hint::black_box`.

## Guarantees and limits

- **The counter only where it is safe.** By default the counter is used
  only where Linux keeps time with it (clock source `tsc` on x86-64,
  `arch_sys_counter` on Arm), the instruction is not trapped in a
  virtual machine, and the measured rate matches the one the kernel
  advertises. Otherwise the clock is `CLOCK_MONOTONIC` plus an offset
  that keeps the stamps on UTC, about `Instant`'s cost, and `calibrate`
  prints why. Never the raw wall clock: a fallback stamp is never a bare
  `CLOCK_MONOTONIC` count and never zero.
  `advanced::Policy::RequireCounter` makes a fallback an error.
- **Virtual machines.** Where Linux keeps time with a hypervisor's clock
  (Azure's `hyperv_clocksource_tsc_page`, `kvm-clock`, `xen`), and on
  macOS, the clock falls back. `advanced::Trust::CrossCheck` lets
  calibration's own checks (a measured rate against the kernel's, and a
  read-cost probe) accept the counter anyway.
- **Never backwards, and no jump after start.** A follower can be about
  1 ns early at an update; the owner never goes back. `update` slews at
  most 0.5 ms a second: a gap of 100 ms between the clock and
  `CLOCK_REALTIME` takes about 3.3 minutes to close. Without updates the
  clock drifts by the hardware's rate error (up to 73 ms an hour
  measured); `update()` and `health()` report it, and nothing else does.
- **Durations are never negative.** `since_ns` and `elapsed_ns` stop at
  zero.
- **`ERGON_HOST_CHECKS=off`** (or `0`) silences the start-up lines;
  `report().warnings()` keeps them.

## How to use it

```toml
[dependencies]
ergon-clock = "0.1"
```

Calibrate once at start-up, log the report, and update the clock from the
loop's idle time (between two pieces of work, even in a loop that never
sleeps):

```rust
use ergon_clock::{Config, calibrate, elapsed_ns};

fn main() -> Result<(), ergon_clock::Error> {
    let mut clock = calibrate(&Config::default())?; // prints a line per problem
    println!("clock: {}", clock.report()); // what was chosen, and why

    // Event time, deadlines, ages: the unordered read (ns since 1970, UTC).
    let stamp = clock.now_ns();
    let age = clock.since_ns(stamp);

    // A measurement: its end is the ordered read, taken after every earlier
    // instruction has finished.
    let start = clock.now_ns();
    let took = elapsed_ns(start, clock.now_ns_ordered());

    // From the loop's idle time: real work at most once a second.
    let health = clock.update();
    if health.sampled && let Some(warning) = health.warning() {
        eprintln!("{}", warning.line()); // off the hot path
    }

    // Another thread: a follower, moved there at start-up.
    let follower = clock.follower();
    let read = std::thread::spawn(move || follower.now_ns()).join();
    assert!(age < 1_000_000_000 && took < 1_000_000_000 && read.is_ok());
    Ok(())
}
```

Write time-reading code against `TimeSource`, and test it with a clock you
move by hand:

```rust
use ergon_clock::{ManualClock, TimeSource};

/// A quote older than 5 ms is stale.
fn is_stale(clock: &impl TimeSource, stamped_ns: u64) -> bool {
    clock.since_ns(stamped_ns) > 5_000_000
}

fn main() {
    let clock = ManualClock::new(1_000);
    assert!(!is_stale(&clock, 1_000));
    clock.advance(5_000_001);
    assert!(is_stale(&clock, 1_000));
}
```

Everything else is in `advanced`: the fixed `Copy` clock, raw ticks,
`Params` for C, `Trust` and `Policy`, the discipline and seqlock under
`update` and `Follower`, `WallOffset`, and the time-sync probe.

## Further reading

- The book: a glossary, ordered reads, measured results, and why not
  quanta or minstant:
  [ergon-clock](https://mimran1980.github.io/ergon/clock/overview.html).
- [BENCHMARKS.md](https://github.com/mimran1980/ergon/blob/main/clock/BENCHMARKS.md):
  every read against quanta, minstant, `std::time` and `clock_gettime` on
  x86-64 and Graviton.
- API reference: [docs.rs/ergon-clock](https://docs.rs/ergon-clock/).

Licensed under Apache-2.0.
