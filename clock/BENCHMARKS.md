**Note:** these benchmarks are environment-sensitive; rerun them on your own hardware. Measured 2026-10-16 on c7i.2xlarge, c8g.xlarge, c9g.2xlarge and an M1 Pro. ns unless marked; every latency figure keeps its max.

# ergon-clock benchmarks

ergon-clock is the repository's clock, built on `libc` alone. quanta and minstant are built only in `ergon-clock-bench`, as the comparison. [Why not quanta (or minstant)](#why-ergon-clock-and-not-quanta-or-minstant) gives the reasons.

## Conclusions

Figures are the chrony runs (node a, isolated, LTO): ns, the median of the batch means, with the slowest batch in brackets.

- **Use ergon-clock on every host measured.** Its read is the bare counter instruction plus one multiply: `now_ns` 15.16 (16.13) on c7i.2xlarge, 12.17 (12.47) on c8g.xlarge and 10.92 (11.25) on c9g.2xlarge. quanta's held clock costs the same (15.99, 12.50, 10.92): speed is no reason either way.
- **The rate decides it.** ergon-clock keeps the counter's own rate in every process: 0.000 ppm apart, and −0.048 to 0.000 ppm from the kernel's scaling (in UTC each owner then disciplines its own offset to `CLOCK_REALTIME`, so processes agree only within that residual). quanta measures `CLOCK_MONOTONIC` for 0.6 ms in each process, so it takes NTP's rate plus noise: with chrony settled it was 2.7 ppm (c7i) and 19.1 ppm (c8g) off the counter, and processes on one host disagreed by 1.9–6.9 ppm. Stamps from a feed handler, a strategy and a gateway must agree, and map to the wall clock through one offset.
- **ergon-clock has an ordered read; quanta has none.** `rdtscp` 20.00 (21.08) on c7i, `isb; mrs cntvct_el0` 19.48 (19.79) on c8g, `mrs cntvctss_el0` 11.36 (11.67) on c9g, 7.30 ns under `isb`.
- **It calibrates once, in an explicit call at start-up:** 40–70 ms, at most 70.226 ms, and reports what it chose and why. quanta can be calibrated at start-up too; its lazy set-up is not a reason against it.
- **minstant: avoid on Arm.** There it is `SystemTime`: 39.64 (42.22) on c8g and 28.16 (28.50) on c9g, not monotonic, and it carries every NTP slew.
- **When the alternatives are fine:**
  - `std::time::Instant` or `clock_gettime(CLOCK_MONOTONIC)`, 20.78–31.69 ns (slowest batch at most 33.35): off the hot path, or for intervals in one process where NTP's rate is wanted. ergon-clock falls back to the same clock where the counter cannot be trusted.
  - quanta: within one process, where only differences count and 2.7–19 ppm (up to 19 µs a second) does not matter. Its read costs the same.
  - minstant: on x86 only, within one process.
- **Disciplined to the wall clock** (once a second, 30 minutes, both nodes of each type): a stamp's residual to `CLOCK_REALTIME` was p50 2–3 ns, p99 282–2,726 ns and max 1,447–9,443 ns at the samples. It comes from chrony's own frequency changes, not from ergon-clock. An update costs 0.3 µs on Graviton (max 1.8–2.9 µs, the start update) and 3.5 µs on c7i (max 6.1–18.7 µs), the c7i figure set by waking from deep idle: run the owner on a thread that never idles.

## What is measured

`clockbench` (`clock/bench/src/main.rs`) times every clock read on a host and checks each one before it times it. Each read: rate within 1% of `CLOCK_MONOTONIC_RAW` over 20 ms; an ordered read or a kernel monotonic clock must never step back over 200,000 reads; a clock that fails is reported, not timed. Cost is the mean of 10,000 back-to-back reads per batch through `black_box`, 200 batches: median, p99, slowest and fastest batch. CSV fields per run: host, counter, median, p99, max, samples, rate, drift. The Criterion bench (`clock/bench/benches/read.rs`) times the same row list (`clock/bench/src/rows.rs`) Criterion's way: a mean and its interval, with no maximum.

Rows cover: `ergon_ticks`/`ergon_ticks_ordered` (the bare counter and its ordered read), `ergon_now_ns`/`ergon_now_ns_ordered` (converted, one 32.32 multiply), the loaded enum front, the monotonic fallback, the follower through the seqlock, x86/Arm bare instructions, quanta, minstant, `std::time`, `clock_gettime`, and the conversion alone. The `ergon_*` counter rows read a clock re-anchored 10,000 times first (alternately 100 ppm fast and slow), as a clock its owner disciplines is: a re-anchor changes the clock's values, never its code.

## Running it

```sh
cd clock
cargo bench -p ergon-clock
LAB_CLOCK_ONLY=1 ../md-dpdk/benches/cloud/aws/lab.sh   # the AWS lane, about 20 minutes
```

Cost estimates: a bare read is one instruction plus a multiply; a disciplined update is 0.3 µs (Graviton) to 3.5 µs (c7i, cold). Calibration is 40–70 ms once. In Kubernetes: run the clock owner on a thread that never idles (cpuset, whole physical cores, no cpu quota throttle); the discipline update belongs on the timer wheel, never on the hot path.

## AWS c7i, c8g and c9g with chrony (2026-10-16)

### Read cost (ns, median of batch means, slowest batch in brackets)

| host | bare `now_ns` | ordered `now_ns` | quanta held | minstant |
| --- | --- | --- | --- | --- |
| c7i.2xlarge | 15.16 (16.13) | 20.00 (21.08) | 15.99 | x86 only, fine |
| c8g.xlarge | 12.17 (12.47) | 19.48 (19.79) | 12.50 | 39.64 (42.22), avoid |
| c9g.2xlarge | 10.92 (11.25) | 11.36 (11.67) | 10.92 | 28.16 (28.50), avoid |

### Drift and calibration

| host | drift vs `CLOCK_MONOTONIC_RAW` over 60 s | rate vs kernel scaling | calibration |
| --- | --- | --- | --- |
| c7i | within 0.01 ppm | −0.048 to 0.000 ppm | 40–70 ms (max 70.226) |
| c8g | within 0.01 ppm | −0.048 to 0.000 ppm | 40–70 ms |
| c9g | within 0.01 ppm | −0.048 to 0.000 ppm | 40–70 ms |

quanta's rate was 2.7 ppm (c7i) and 19.1 ppm (c8g) off the counter with chrony settled; processes on one host disagreed by 1.9–6.9 ppm.

### Disciplined residual to `CLOCK_REALTIME` (ns, median of 5 reps)

| host | p50 | p99 | max (range) | update cost |
| --- | --- | --- | --- | --- |
| c7i | 2 | 2,726 | 9,443 (1,447–9,443) | 3.5 µs (max 6.1–18.7 µs), cold |
| c8g | 3 | 282 | 1,447 (1,447–2,100) | 0.3 µs (max 1.8–2.9 µs) |
| c9g | 2 | 310 | 1,610 (1,610–2,400) | 0.3 µs (max 1.8–2.9 µs) |

The residual comes from chrony's own frequency changes, not from ergon-clock. The c7i update cost is set by waking from deep idle: run the owner on a thread that never idles. One clockcheck on VM b ended 255 (SSH session ended; LAB FAULT, recorded in the md-dpdk experiment log) — not yet re-run.

## AWS c7i, c8g and c9g, and an M1 Pro

### Environment

| | c7i.2xlarge | c8g.xlarge | c9g.2xlarge | M1 Pro |
| --- | --- | --- | --- | --- |
| Counter | TSC (`rdtsc`/`rdtscp`) | `CNTVCT_EL0`/`CNTVCTSS_EL0` | `CNTVCT_EL0`/`CNTVCTSS_EL0` | `CNTVCT_EL0` |
| Clock source | `tsc` | `arch_sys_counter` | `arch_sys_counter` | `arch_sys_counter` |
| Time sync | systemd-timesyncd (slews); chrony for the residual table | same | same | host |
| Isolation | lab profile (`isolcpus`, `nohz_full`) | same | same | n/a |

On the Mac the clock must be correct, but its speed is not a target.

### What stuck from the full sweep

- Ordered read is 3–8 ns dearer than a bare read on every host; c7i's gap is the largest (7.5 ns); c9g's `CNTVCTSS_EL0` is 7.30 ns under `isb`.
- Cross-core reads are safe (same rate); gaps between consecutive reads are sub-ns on Graviton, 1–2 ns on c7i.
- No-LTO: c7i gates failed at 1.72 ns and 1.05 ns (open). Graviton passed.
- Calibration is 40–70 ms; a stale rate drifts 0.01 ppm/hour over the windows measured.
- `std::time::Instant` / `CLOCK_MONOTONIC`: 20.78–31.69 ns (slowest batch at most 33.35) — fine off the hot path.
- Conversion alone over captured ticks: ergon's `to_ns` and `ticks_to_ns` cost one multiply; quanta's `delta_as_nanos` and `scaled` are in the same class.

Full every-run, gap, cross-core and gate tables are in git history.

## Why ergon-clock and not quanta (or minstant)

quanta takes NTP's rate per process (2.7–19.1 ppm off the counter; 1.9–6.9 ppm between processes on one host). That is fine within one process where only differences count. minstant is `SystemTime` on Arm: not monotonic, and it carries every NTP slew. ergon-clock is the bare counter plus one multiply, ordered read, calibrated once, with one owner disciplining to `CLOCK_REALTIME` through the seqlock. Stamps from a feed handler, a strategy and a gateway must agree, and map to the wall clock through one offset. Keep it on every host in the stack.

## Open

- Graviton5 ordered-read test still to run on its own (the c9g numbers above are from the mixed sweep).
- Kernel clock source recheck after a host reboot.
- Fixed gap pass: one run with a fixed inter-read gap, to separate the cost of the read from the cost of the gap.
- Fallback gate fixed, awaiting rerun.
- c7i no-LTO gates failed 1.72/1.05 ns — untraced.
- c7i discipline updates after deep idle are cold (3.5 µs, max 6.1–18.7 µs) — untraced whether this is a cache or a C-state effect.
- One clockcheck on VM b ended with SSH 255 (LAB FAULT). Not yet re-run.

## Earlier: Linux arm64 in Docker Desktop's VM

Correctness only, no latency claim: the counter is `CNTVCT_EL0`, the same as c8g/c9g. Run for a smoke check of the hot path's code on Arm.

## The hot path's code

The bare read is one counter instruction plus one multiply. The ordered read is the same plus a barrier (`rdtscp` on x86; `mrs cntvctss_el0` where the CPU has `FEAT_ECV`, else `isb; mrs cntvct_el0`). Assembly listings are in git history (`clock/src/lib.rs`, `clock/src/hot.rs`). Do not change the hot path without re-running the read-cost and ordered-read gates.
