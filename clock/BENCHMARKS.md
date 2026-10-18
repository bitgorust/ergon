**Note:** these benchmarks are environment-sensitive; rerun them on your own hardware. The results below were measured on 2026-10-16 on four AWS instance types (c7i.2xlarge, c8g.xlarge, c9g.xlarge and c9g.2xlarge: x86-64, Graviton4 and Graviton5) and on a developer's Mac. On the Mac the clock must be correct, but its speed is not a target.

# ergon-clock benchmarks

ergon-clock is the repository's clock, built on `libc` alone. quanta and minstant are built only in `ergon-clock-bench`, as the comparison. [Why not quanta (or minstant)](#why-ergon-clock-and-not-quanta-or-minstant) gives the reasons, from the numbers below.

## Conclusions

Figures are the [chrony runs](#aws-c7i-c8g-and-c9g-with-chrony-2026-10-16) (node a, isolated, LTO): ns, the median of the batch means, with the slowest batch in brackets.

- **Use ergon-clock on every host measured.** Its read is the bare counter instruction plus one multiply: `now_ns` 15.16 (16.13) on c7i.2xlarge, 12.17 (12.47) on c8g.xlarge and 10.92 (11.25) on c9g.2xlarge. quanta's held clock costs the same (15.99, 12.50, 10.92): speed is no reason either way.
- **The rate decides it.** ergon-clock keeps the counter's own rate in every process: 0.000 ppm apart, and −0.048 to 0.000 ppm from the kernel's scaling (in UTC each owner then disciplines its own offset to `CLOCK_REALTIME`, so processes agree only within that residual). quanta measures `CLOCK_MONOTONIC` for 0.6 ms in each process, so it takes NTP's rate plus noise: with chrony settled it was 2.7 ppm (c7i) and 19.1 ppm (c8g) off the counter, and processes on one host disagreed by 1.9–6.9 ppm. Stamps from a feed handler, a strategy and a gateway must agree, and map to the wall clock through one offset.
- **ergon-clock has an ordered read; quanta has none.** `rdtscp` 20.00 (21.08) on c7i, `isb; mrs cntvct_el0` 19.48 (19.79) on c8g, `mrs cntvctss_el0` 11.36 (11.67) on c9g, 7.30 ns under `isb`.
- **It calibrates once, in an explicit call at start-up:** 40–70 ms, at most 70.226 ms, and reports what it chose and why. quanta can be calibrated at start-up too; its lazy set-up is not a reason against it.
- **minstant: avoid on Arm.** There it is `SystemTime`: 39.64 (42.22) on c8g and 28.16 (28.50) on c9g, not monotonic, and it carries every NTP slew.
- **When the alternatives are fine:**
  - `std::time::Instant` or `clock_gettime(CLOCK_MONOTONIC)`, 20.78–31.69 ns (slowest batch at most 33.35): off the hot path, or for intervals in one process where NTP's rate is wanted. ergon-clock falls back to the same clock where the counter cannot be trusted.
  - quanta: within one process, where only differences count and 2.7–19 ppm (up to 19 µs a second) does not matter. Its read costs the same.
  - minstant: on x86 only, within one process.
- **Disciplined to the wall clock** (once a second, 30 minutes, both nodes of each type): a stamp's residual to `CLOCK_REALTIME` was p50 2–3 ns, p99 282–2,726 ns and max 1,447–9,443 ns at the samples. It comes from chrony's own frequency changes, not from ergon-clock ([below](#the-residual-comes-from-chrony)). An update costs 0.3 µs on Graviton (max 1.8–2.9 µs, the start update) and 3.5 µs on c7i (max 6.1–18.7 µs), the c7i figure set by waking from deep idle: run the owner on a thread that never idles ([below](#the-c7i-update-cost-cold-caches-after-deep-idle)).

## What is measured

`clockbench` (`clock/bench/src/main.rs`) times every clock read on a host and checks each one before it times it. The Criterion bench (`clock/bench/benches/read.rs`) times the same row list (`clock/bench/src/rows.rs`) Criterion's way: a mean and its interval, with no maximum.

`ergon_now_ns` and `ergon_now_ns_ordered` are the counter arm of the front `Clock::now_ns` and `now_ns_ordered`, which compile to the same instructions as `ergon_clock_now_ns`'s enum (x86: one register move more than the counter arm alone), in either domain; the other ergon types named here are under `ergon_clock::advanced`.

| Row | What it reads |
|---|---|
| `ergon_ticks`, `ergon_ticks_ordered` | `advanced::CounterClock::ticks` (`rdtsc`, `mrs cntvct_el0`) and `ticks_ordered`: the clock's `OrderedRead`, which is `rdtscp`; `mrs cntvctss_el0` where the CPU has `FEAT_ECV`; else `isb; mrs cntvct_el0` |
| `ergon_now_ns`, `ergon_now_ns_ordered` | the same reads converted to ns (one 32.32 multiply) |
| `ergon_now_ns_loaded`, `ergon_clock_now_ns` | `advanced::CounterClock::now_ns` and `advanced::Clock::now_ns` (the enum) with the clock loaded from memory at every read (`black_box(&clock)`), as one held in a struct is, so the enum's branch is taken at every read. With the clock in registers the compiler hoists the branch out of the timed loop. |
| `ergon_monotonic_now_ns` | `advanced::MonotonicClock::now_ns`: the fallback (`CLOCK_MONOTONIC`; macOS `CLOCK_UPTIME_RAW`), converted by the counter's formula (a factor of one until disciplined) |
| `follower_now_ns` | `advanced::share::ClockFollower::clock().now_ns()`: the counter clock as another thread reads it through the seqlock its owner publishes to (one acquire load and a compare, then the read; absent in older CSVs) |
| `front_follower_now_ns` | `Follower::now_ns()`: the same through the front API's follower, whose fields sit in `Cell`s (absent in older CSVs) |
| `raw_rdtsc`, `raw_lfence_rdtsc`, `raw_rdtscp`, `raw_rdtscp_lfence` | x86-64's bare instructions |
| `raw_cntvct`, `raw_isb_cntvct`, `raw_cntvctss` | Arm's bare reads; `CNTVCTSS_EL0` where the CPU has `FEAT_ECV` |
| `quanta_raw`, `quanta_now`, `quanta_now_ns` | a held `quanta::Clock`: `raw()`, `now()` (an opaque `Instant`), `delta_as_nanos(origin, raw())` |
| `quanta_instant_now` | the global `quanta::Instant::now()` (quanta's default features) |
| `minstant_now`, `minstant_elapsed_ns`, `minstant_unix_ns` | `minstant::Instant::now()`, `elapsed()`, `now().as_unix_nanos(&anchor)` |
| `std_instant_now`, `std_instant_elapsed_ns`, `std_systemtime_ns` | `std::time` |
| `clock_gettime_*` | `clock_gettime` of `CLOCK_MONOTONIC`, `_RAW`, `CLOCK_REALTIME`, and Linux's `_COARSE` (macOS: `CLOCK_UPTIME_RAW`) |
| `convert_*` | the conversion alone over captured ticks: ergon's `to_ns` and `ticks_to_ns`, quanta's `delta_as_nanos` and `scaled` |

The `ergon_*` counter rows read a clock re-anchored 10,000 times first (alternately 100 ppm fast and slow, the last at its own factor), as a clock its owner disciplines to the wall clock is: a re-anchor changes the clock's values, never its code, and the gates hold for it.

For each read:

- **Check, before anything is timed.** Its rate must be within 1% of `CLOCK_MONOTONIC_RAW` over 20 ms (longer for a coarse clock). An ordered read or a kernel monotonic clock must never step back over 200,000 reads. A clock that fails is reported, not timed, and `clockbench` exits 1.
- **Cost.** Each batch's mean over 10,000 back-to-back reads, each through `black_box`, after a warm-up, for 200 batches. Reported: median, p99, slowest batch and fastest batch.
- **Gaps.** The gaps between 1,000,000 consecutive reads, recorded first and analysed after. One untimed pass fills the buffer first, so the timed pass takes no page faults (runs before 2026-10-16's fix did: [below](#gaps-between-consecutive-reads-ns-resolution-p50-p9999-max-steps-back)):
  - the smallest non-zero gap (the resolution, or the cost when that is larger);
  - p50, p99, p99.99 and the maximum;
  - reads equal to and lower than the one before;
  - the thread's context switches and page faults over the pass;
  - every gap of 10 µs or more, with its `CLOCK_REALTIME`, so `md-dpdk/benches/cloud/clockbench.py` can attribute it against the lab's gap spinner and trace.
- **Across cores.** Two threads on `--cpu` and `--cpu2` hand off 100,000 times each way. One thread reads and publishes; the other sees the value and reads its own. Reported: second reads lower than the first, the largest step back, and the offset between the two directions' least differences.
- **Drift** at checkpoints up to `--drift-secs`, each sample the tightest of 8 brackets, against two references:
  - `CLOCK_MONOTONIC_RAW`, what ergon-clock calibrates against. Where the kernel keeps time with the counter it is the counter itself, scaled by the kernel, so a counter clock's drift from it is how its conversion differs from the kernel's, not its accuracy.
  - `CLOCK_MONOTONIC`, which the host's NTP client steers to its sources' rate and slews while it removes an offset. A clock at the counter's raw rate drifts from it by the oscillator's error as the client sees it (a few ppm) plus that slew, which after a boot is most of it. In the [reference runs](#aws-c7i-c8g-and-c9g-and-an-m1-pro-2026-10-16) the client was systemd-timesyncd ([their time sync](#time-sync-in-these-runs)); in the [newest runs](#aws-c7i-c8g-and-c9g-with-chrony-2026-10-16), chrony.
- **Calibration** of ergon-clock and of quanta in 20 fresh processes: the time taken and the spread of the rate chosen. A process whose ergon-clock calibration fell back to the monotonic clock has no counter rate; the record says why instead (`fallback:<reason>`).
- **Gates.**
  - `ergon_now_ns` and `ergon_ticks` within 1 ns of the bare read.
  - `ergon_ticks_ordered` within 1 ns of the bare read of the instruction its ordered read takes (`raw_rdtscp`, `raw_cntvctss` or `raw_isb_cntvct`).
  - `follower_now_ns` within 1.5 ns of `ergon_now_ns` (`follower_ns_over_owner_ns`), `front_follower_now_ns` likewise (`front_follower_ns_over_owner_ns`), and `ergon_monotonic_now_ns` within 1.5 ns of the kernel read of the clock it wraps (`fallback_ns_over_clock_gettime`: `clock_gettime_monotonic`; on macOS `clock_gettime_uptime_raw`). The fallback's read against its previous run is compared across runs, not here.
  - Enforced with `--enforce-gates` on Linux, never on macOS. The comparisons (`cmp`) are reported, not gated.

`clockbench --discipline SECS` runs only the discipline of the counter clock to `CLOCK_REALTIME` (`ergon_clock::advanced::discipline`), for `SECS` seconds:

- The clock is calibrated in the realtime domain (`Trust::CrossCheck`), and its owner takes the start update at once, then one update a second: a sample (the tightest of up to 8 brackets of `CLOCK_REALTIME` between two ordered reads, 16 at the start, and on Linux up to 3 brackets of `CLOCK_REALTIME - CLOCK_MONOTONIC`), the controller, the re-anchor and the publish to the seqlock, timed together.
- A verifier thread reads the published clock through a `ClockFollower` at `--verifier-hz` (100) and brackets `CLOCK_REALTIME` between two of its ordered reads: the residual a stamp taken between the owner's samples carries.
- Both threads busy-wait between their work, as a runtime's threads do: the owner on `--cpu`, the verifier on `--cpu2` (the two go together). The [chrony runs](#disciplined-clock-residual) predate this: their owner slept between updates and their verifier shared its CPU.
- On Linux each update also reads `adjtimex` (status, `maxerror`, frequency).

The CSV is one record a line, its kind first. A run starts with `meta,tool,clockbench`.

| Record | Fields |
|---|---|
| `meta` | key, value: host, build, settings (`--discipline`: also `owner_context_switches` and `verifier_context_switches`, voluntary;involuntary over the run, and `verifier_cpus`), and both calibrations' reports (`ergon_*`: `Trust::CrossCheck`; `default_*`: `Trust::Kernel`), among them `ergon_ordered_read` (its spaces written as `_`) and the time-sync section: `*_time_sync` (`ptp-grade`, `ntp-disciplined`, `unsynchronised` or `unknown`), `*_time_sync_source`, `*_time_sync_bound_ns` and `*_time_sync_bound_from`, `*_time_daemons`, `*_ptp_clocks`, `*_kernel_sync`, `*_kernel_maxerror_us` (absent in older CSVs) |
| `check` | row, rate error ppm, steps back, `ok` or `fail`, why |
| `cost` | row, unit, batches, reads a batch, median, p99, max, min (ns) |
| `delta` | row, reads, smallest non-zero gap, p50, p99, p99.99, max (ns), equal reads, steps back, largest step back (ns), gaps of 10 µs or more, voluntary and involuntary context switches, page faults (absent in older CSVs) |
| `gap` | row, `CLOCK_REALTIME` at the gap's end (ns), gap (ns) |
| `skew` | row, handoffs each way, least a-to-b and b-to-a differences, offset, inversions a-to-b and b-to-a, largest inversion (ns) |
| `convert` | row, batches, reads a batch, median, p99, max, min (ns) |
| `calib` | ergon or quanta, processes, min, median and max ms, min, median and max rate (Hz) and spread (ppm) of the processes that chose the counter (`-` where none did), rate sources and `fallback:<reason>`s |
| `drift` | row, seconds, the reference's ns elapsed, error (ns), ppm, reference (`raw`: `CLOCK_MONOTONIC_RAW`; `monotonic`: `CLOCK_MONOTONIC`; absent in older CSVs: `raw`) |
| `gate` | gate, value, limit (ns), `pass`, `fail` or `not-enforced`, the row and the bare row compared (absent in older CSVs) |
| `cmp` | comparison, difference (ns) |
| `discipline` | (`--discipline`) seconds since the start update, action (`slewed`, `stepped:<ns>`, `held`), residual before the update, wall offset after it, the sample's error bound (ns), rate applied and frequency estimate (ppb), wall-clock step seen (ns), flag bits (`ergon_clock::advanced::discipline::flags`), the update's cost (ns), `adjtimex` status, `maxerror` (µs) and frequency (ppm) as `status;maxerror;freq` or `-` |
| `truth` | (`--discipline`) seconds, `CLOCK_REALTIME` less the follower's clock at the bracket's middle (ns), the bracket's width (ns) |
| `discipline_summary` | (`--discipline`) series (`residual_at_samples_ns`, `residual_verifier_ns`: absolute, the verifier's brackets over ten times their median width left out; `verifier_interrupted_width_ns`: those brackets' widths; `update_cost_ns`), n, p50, p99, p99.9, max |

## Running it

On any host, from the repository root:

```sh
cargo run --release -p ergon-clock-bench --bin clockbench -- --cpu 2 --cpu2 3 --label "$(hostname)" >clockbench.csv
cargo bench -p ergon-clock-bench --bench read          # Criterion
python3 md-dpdk/benches/cloud/clockbench.py clockbench.csv
```

On AWS, a pair of each type, each running the clock benchmarks alone. Each run:

- bootstraps the lab's Debian 13 with the backports kernel;
- builds `clockbench` with and without LTO;
- runs it pinned in the pinned and then the isolated state, with 120 s of drift, beside the gap spinner and trace;
- deletes everything and verifies it.

Each host's CSV lands in `target/md-dpdk-bench/aws-<stamp>/<node>-clockbench-<state>.csv`, and the run's own comparison in `clock-summary.md`. The estimates, from `LAB_ESTIMATE=1` with the same variables (no cloud call):

```sh
LAB_CLOCK_ONLY=1 LAB_SPOT=1 LAB_MAX_HOURS=1 LAB_TYPE=c7i.2xlarge md-dpdk/benches/cloud/aws/lab.sh
#   wall time about 0.31 h: 0.62 instance-hours; instances at most $0.22 (on-demand prices: spot is lower)
LAB_CLOCK_ONLY=1 LAB_SPOT=1 LAB_MAX_HOURS=1 LAB_ALLOW_ARM=1 LAB_TYPE=c8g.xlarge md-dpdk/benches/cloud/aws/lab.sh
#   wall time about 0.31 h: 0.62 instance-hours; instances at most $0.10
LAB_CLOCK_ONLY=1 LAB_SPOT=1 LAB_MAX_HOURS=1 LAB_ALLOW_ARM=1 LAB_TYPE=c9g.xlarge md-dpdk/benches/cloud/aws/lab.sh
#   wall time about 0.31 h: 0.62 instance-hours; instances at most $0.11
python3 md-dpdk/benches/cloud/clockbench.py target/md-dpdk-bench/aws-<c7i stamp> target/md-dpdk-bench/aws-<c8g stamp> \
    target/md-dpdk-bench/aws-<c9g stamp> >clock-comparison.md
```

Every run of the lab, not only `LAB_CLOCK_ONLY=1`, also runs `clockbench` (quick: no drift) at boot and in each isolated state, beside `clockcheck`.

The discipline alone, pinned, for half an hour:

```sh
cargo run --release -p ergon-clock-bench --bin clockbench -- --discipline 1800 --cpu 2 --label "$(hostname)" >discipline.csv
```

## AWS c7i, c8g and c9g with chrony (2026-10-16)

The newest runs. Every host kept time with chrony against the Amazon Time Sync Service (the [reference runs](#aws-c7i-c8g-and-c9g-and-an-m1-pro-2026-10-16) had systemd-timesyncd), and the c9g run is the first of ergon-clock's own `CNTVCTSS_EL0` ordered read.

| | c7i.2xlarge | c8g.xlarge | c9g.2xlarge |
|---|---|---|---|
| Run | `aws-20261016-231320` | `aws-20261016-223331` | `aws-20261016-223251` |
| CPU | Xeon Platinum 8488C (Sapphire Rapids) | Neoverse V2 (Graviton4) | Neoverse V3 (Graviton5) |
| Ordered read | `rdtscp` | `isb; mrs cntvct_el0` | `mrs cntvctss_el0` |
| `clockbench` settings | defaults | defaults | quick: 50 batches, 200,000 gap reads, 20,000 handoffs each way, no drift, 5 processes |
| Time sync, as the report saw it | chronyd to 169.254.169.123, bound 165,803 ns | the same, bound 107,871 ns | the same, bound 87,018 ns |

- All: spot, us-east-1a, Debian 13, Linux 7.2.6, rustc 1.99.0 with LTO, node a, isolated CPUs (`isolcpus=nohz,domain,managed_irq,1-N nohz_full=1-N rcu_nocbs=1-N irqaffinity=0`), `clockbench` on CPU 1 and its partner on CPU 3. The c9g run is the transport lab's, whose `clockbench` pass is the quick one.
- Every clock passed its check. ergon-clock took the counter with the default trust on every host.

### Read cost with chrony, ns: median of the batch means (slowest batch)

| read | c7i.2xlarge | c8g.xlarge | c9g.2xlarge |
|---|---|---|---|
| `ergon_ticks` | 14.25 (15.82) | 12.17 (12.52) | 10.91 (11.29) |
| `ergon_now_ns` | 15.16 (16.13) | 12.17 (12.47) | 10.92 (11.25) |
| `ergon_ticks_ordered` | 20.00 (21.08) | 19.48 (19.79) | 11.36 (11.67) |
| `ergon_now_ns_ordered` | 22.73 (23.88) | 21.93 (22.34) | 11.35 (11.65) |
| `follower_now_ns` | 15.15 (16.24) | 12.17 (12.41) | 10.92 (11.28) |
| bare unordered (`raw_rdtsc`, `raw_cntvct`) | 14.24 (15.19) | 12.16 (12.60) | 10.91 (11.40) |
| bare ordered (`raw_rdtscp`, `raw_isb_cntvct`) | 20.00 (21.11) | 19.48 (19.79) | 18.66 (18.95) |
| `raw_cntvctss` | – | – | 11.35 (11.67) |
| `ergon_monotonic_now_ns` (the fallback) | 26.87 (28.31) | 32.93 (33.24) | 22.37 (22.74) |
| `quanta_now_ns` | 15.99 (16.95) | 12.50 (12.89) | 10.92 (11.23) |
| `quanta_instant_now` | 16.67 (17.82) | 12.23 (12.55) | 11.00 (11.34) |
| `minstant_now` | 14.55 (15.44) | 39.64 (42.22) | 28.16 (28.50) |
| `minstant_unix_ns` | 21.31 (22.32) | 42.98 (43.45) | 31.03 (31.32) |
| `std_instant_now` | 24.85 (25.99) | 31.69 (33.35) | 21.29 (21.68) |
| `clock_gettime_monotonic` | 24.25 (25.54) | 30.92 (31.22) | 20.78 (21.30) |
| `clock_gettime_realtime` | 24.25 (25.50) | 30.94 (31.97) | 21.09 (21.65) |
| `clock_gettime_monotonic_coarse` | 4.07 (5.04) | 8.42 (8.68) | 7.27 (7.69) |

- **The same as the reference runs,** within 0.5 ns a read: `ergon_now_ns` was 15.63 (c7i), 12.15 (c8g) and 10.91 ns (c9g.xlarge) there.
- **Graviton5's ordered read is confirmed:** the run reports `ergon_ordered_read=mrs_cntvctss_el0`. `ergon_ticks_ordered` is 11.36 ns, 0.02 ns over the bare `CNTVCTSS_EL0` (gate: pass), 7.30 ns under `isb; mrs cntvct_el0` and 0.45 ns over the unordered read. Its ordered read in ns costs no more than its ticks (11.35): with no `isb`, nothing waits for the last multiply. No read of it stepped back across cores (0 of 40,000 handoffs), and its longest gap in 200,000 reads was 17 ns. Still to run on a Graviton5: the ordering test ([Open](#open)).
- **The x86 extra register move costs nothing measurable. Keep it.** `advanced::Clock::now_ns`, the enum, takes one more `mov` on x86-64 since the fallback shares the counter's conversion ([the hot path's code](#the-hot-paths-code)). On c7i it read in 15.15 ns (16.20) against `ergon_now_ns`'s 15.16 (16.13), and 0.61 ns under `ergon_now_ns_loaded`'s 15.76 (16.86); the reference run gave −0.62 ns too. Gaps, resolution / p50 / p99.99 / max: 13 / 15 / 124 / 7,435 ns against 13 / 15 / 120 / 7,351.
- **A follower's read** (`advanced::share::ClockFollower`, the seqlock) costs what the owner's does on every host (gate: pass).
- **The fallback failed its gate on node a of all three:** `ergon_monotonic_now_ns` is 2.62, 2.01 and 1.58 ns over `clock_gettime_monotonic` (limit 1.50). Node b: 1.47 (pass), 2.03 and 1.46 (pass). Cause: the counter's multiply, which the fallback had taken on. Fixed since, not rerun ([Open](#open)).
- **Conversion alone is slower than quanta's on c9g:** `convert_ergon_ticks_to_ns` 3.91 ns (4.84) against `convert_quanta_delta_as_nanos` 1.22 (1.22); on c7i 0.96 (1.86) against 1.11 (1.22), on c8g 1.03 (2.63) against 1.56 (1.89). Inside a read it is hidden (`ergon_now_ns` 10.92 = `quanta_now_ns` 10.92), but code that converts captured ticks later pays it. See [Open](#open).
- **Gaps:** no read stepped back on one thread, and none across two cores (0 of 200,000 handoffs; c9g 0 of 40,000). The longest gap of any read but the coarse clock (whose 4 ms steps are its resolution) was 12.7 µs (c7i, `clock_gettime_realtime`), 10.5 µs (c8g, `ergon_now_ns_loaded`) and 5.3 µs (c9g, `raw_cntvctss`); the trace found no cause inside the guest for the c7i and c8g ones. `ergon_now_ns`'s own: 7,351 ns, 3,153 ns and 3,867 ns, against 18,879, 1,955 and 20,277 ns in the reference runs: the platform's vCPU stops, not the reads.

### Drift with chrony: ppm at 120 s against `CLOCK_MONOTONIC_RAW` / against `CLOCK_MONOTONIC`

| clock | c7i.2xlarge | c8g.xlarge |
|---|---|---|
| `ergon_now_ns` | −0.048 / +2.589 | 0.000 / +20.209 |
| `quanta_now_ns` | −2.718 / −0.082 | −19.073 / +1.136 |
| `minstant_elapsed_ns` | −2.864 / −0.228 | −20.209 / 0.000 |
| `std_instant_elapsed_ns` | −2.636 / 0.000 | −20.209 / 0.000 |

- **ergon-clock is unchanged:** −0.048 ppm (5,701 ns in 120 s) on c7i and 0.000 (4 ns) on c8g, as in the reference runs (−0.048, 5,702 ns; 0.000, 6 ns).
- **NTP's own rate** (`CLOCK_MONOTONIC` against `CLOCK_MONOTONIC_RAW`) was −2.636 ppm on c7i and −20.209 on c8g: chrony's frequency correction, 2.627 and 20.174 ppm fast. ergon-clock keeps the counter's raw rate, so it parts from `CLOCK_MONOTONIC` by that oscillator error: 20.2 µs a second on this c8g.
- **quanta lands on NTP's rate, not the counter's.** It measures `CLOCK_MONOTONIC` for 0.6 ms, so with chrony settled it was within 0.082 ppm (c7i) and 1.136 ppm (c8g) of `CLOCK_MONOTONIC`, and therefore 2.7 and 19.1 ppm off the counter (326,190 and 2,288,953 ns in 120 s).
- **The old figures came from the boot.** The reference runs' quanta rates, −29 to +131 ppm on these hosts and −978 to +849 ppm across all runs, were systemd-timesyncd slewing `CLOCK_MONOTONIC` after each boot ([their time sync](#time-sync-in-these-runs)), not quanta's settled error. Settled, the error is NTP's frequency correction: 2.7–19 ppm here.
- c9g's chrony run had no drift pass (the transport lab's quick `clockbench`).

### Calibration with chrony: ms min / median / max; the rate's spread between processes, ppm

| | c7i.2xlarge (20 processes) | c8g.xlarge (20) | c9g.2xlarge (5) |
|---|---|---|---|
| ergon-clock | 70.212 / 70.215 / 70.221; 0.000 | 40.186 / 40.188 / 40.203; 0.000 | 40.131 / 40.131 / 40.133; 0.000 |
| quanta | 0.590 / 0.595 / 0.609; 6.866 | 0.579 / 0.579 / 0.582; 4.768 | 0.558 / 0.559 / 0.563; 1.907 |

- quanta's processes still disagree, by 1.9–6.9 ppm with chrony settled, against up to 64.9 ppm while systemd-timesyncd slewed (the reference runs). ergon-clock's agree exactly on every host.

### Disciplined clock residual

`clockbench --discipline 1800` on both nodes, isolated, LTO: the clock disciplined to `CLOCK_REALTIME` once a second for 30 minutes. Residual: `CLOCK_REALTIME` less the clock, absolute, ns. "Verifier": a follower's copy bracketing `CLOCK_REALTIME` between two of its ordered reads 100 times a second (interrupted brackets, over ten times the median width, left out). "Update": one owner update (sample, controller, re-anchor, publish).

| host, node | at samples p50 / p99 / max | verifier p50 / p99 / max | interrupted | update p50 / p99 / max |
|---|---|---|---|---|
| c7i.2xlarge a | 3 / 2,726 / 9,443 | 134 / 2,641 / 9,404 | 79 | 3,478 / 4,460 / 18,737 |
| c7i.2xlarge b | 2 / 685 / 3,950 | 133 / 692 / 3,821 | 108 | 3,480 / 4,426 / 6,064 |
| c8g.xlarge a | 2 / 532 / 2,154 | 4 / 523 / 2,144 | 2 | 334 / 572 / 2,894 |
| c8g.xlarge b | 2 / 291 / 1,447 | 3 / 303 / 1,465 | 0 | 329 / 435 / 2,895 |
| c9g.2xlarge a | 2 / 282 / 2,580 | 12 / 240 / 2,662 | 2 | 295 / 373 / 1,759 |
| c9g.2xlarge b | 2 / 397 / 2,657 | 7 / 360 / 2,662 | 3 | 278 / 365 / 1,840 |

Interrupted brackets, widths: c7i a 79 (4.9–19.2 µs), b 108 (5.9–18.0 µs); c8g a 2 (3.1–3.5 µs), b 0; c9g a 2 (4.7–5.1 µs), b 3 (4.0–5.0 µs).

- No sample was rejected and no clock was stepped after the start update (at start: c7i b by 5 ns, c8g b by 153 ns, c9g b by 65 ns; the others slewed). chrony stepped no wall clock.
- On both Gravitons every update's maximum is the start update (16 brackets, not 8). c7i's updates cost ten times Graviton's, and its maxima (18.7 µs at 143 s on node a) are not the start ([below](#the-c7i-update-cost-cold-caches-after-deep-idle)).
- The residual is to the host's wall clock. The wall clock's own error to UTC is chrony's bound (above): 87 to 166 µs.
- c7i's verifier p50 (133–134 ns, against 3–12 on Graviton) comes from its wide brackets, about 350 ns there ([below](#the-c7i-update-cost-cold-caches-after-deep-idle)), not from the clock: at the samples its p50 is 2–3 ns, like Graviton's.

#### The residual comes from chrony

- **Each node's largest residual comes right after chrony changes the kernel's frequency** (the `adjtimex` frequency each update reads; it changed every 16 s at the median). The clock is sampled once a second, so it drifts at the new rate until the next sample. On every node the largest residual is 0.8–1.0 × the change × 1 s:
  - c7i a: 9,443 ns after a −9.83 ppm change, and 8,110 ns after +10.15 ppm;
  - c8g a: 2,154 ns after −2.75 ppm; c9g b: 2,657 ns after +3.42 ppm.
- **c7i's chrony is the noisier one** (ergon-clock's code is the same on both):

  | chrony, over the 30 minutes | c7i | c8g and c9g |
  |---|---|---|
  | frequency change, median | 0.12–0.15 ppm | 0.009–0.031 ppm |
  | frequency change, max | 5.0–10.1 ppm | 1.5–3.4 ppm |
  | RMS offset (`chronyc tracking`) | 6.6–43.5 µs | 0.5–5.8 µs |
  | skew | up to 0.70 ppm | 0.008–1.12 ppm |

- **To cut it, if it matters:** sample more often (10 Hz cuts the peak about ten times, for about 3 µs of CPU a second on Graviton), or sample again about 100 ms after `adjtimex` shows a frequency change.

#### The c7i update cost: cold caches after deep idle

These runs' bench slept between updates. It now busy-waits ([Open](#open)).

- **The code costs about the same on both architectures.** An update makes no system call: 1–8 brackets, 1–3 step probes, i128 arithmetic and a seqlock publish (`clock/src/discipline.rs`, `wall.rs`, `share.rs`). c7i's reads cost 20–25 ns, close to Graviton's.
- **Cause: every update runs straight after the owner wakes from a sleep, and on c7i the sleep lets the core go into deep idle.**
  - The bench's owner sleeps in chunks of at most 1 ms between updates (`clock/bench/src/discipline.rs`, `run`).
  - c7i exposes Intel's C-states to the guest (`idle intel_idle` in the run's tune log; c7i.4xlarge's C6 has a 190 µs exit). C6 empties the core's L1 and L2 caches and TLBs.
  - Graviton shows `idle none`: its idle is WFI, which keeps the caches.
- **Evidence in the existing CSVs, no new run:**
  - The verifier's bracket is two ordered reads around one `CLOCK_REALTIME` read, taken right after its own 10 ms sleep. On c7i its median width is 357 ns (a) and 345 ns (b), where the warm cost is about 44 ns (`rdtscp` 20.00 + `clock_gettime_realtime` 24.25).
  - On c8g and c9g it is 51 and 32 ns: the warm sums (19.48 + 30.94 = 50.4; 11.35 + 21.09 = 32.4).
  - Inside an update, the owner's best bracket is warm (error 35–40 ns, so about 50 ns wide): the cold cost is paid on first touch.
  - The cheapest of c7i's 3,602 updates cost 2,710 ns: never once warm.
- **The 18.7 µs maximum (c7i a, at 143 s) cannot be attributed:** no trace ran during the discipline.

#### The interrupted verifier brackets on c7i

The verifier now has its own CPU (`--cpu2`) ([Open](#open)).

- **Cause: the verifier shares the owner's CPU.** `clockbench --discipline` pins its main thread first (`discipline_only`, `--cpu`); the verifier, spawned after, inherits that one-CPU mask, although vm.sh gives the process two CPUs (`taskset -c "$APP_CPU,$RECEIVER_CPU"`). The owner's 1 ms sleep loop wakes about 950 times a second on that CPU.
- **The numbers fit:**
  - Expected hits = wakes a second × bracket width × reads: 950 × 0.35 µs × 176,000 ≈ 59, against 79 and 108 seen.
  - On Graviton the brackets are 7–11 times narrower, so about 6–9 are expected; 0–3 were seen.
  - The hits spread evenly over time: 1 falls within 2 ms after an update, so they are not the updates.
  - An interrupted bracket's width is one owner wake on that core: a median of 17 µs on c7i, 3–5 µs on Graviton.

#### The fix and how to verify it

1. **Done: the verifier has its own CPU.** `clockbench --discipline` pins it to `--cpu2`; vm.sh passes `$RECEIVER_CPU`.
2. **Done: neither thread sleeps.** The owner busy-waits for each update and the verifier for each read, as a runtime's threads do. So no update or bracket starts on a core back from idle.
3. **Measure c7i with C-states capped:** vm.sh already has `CSTATE=1` (`intel_idle.max_cstate=1` at run time).
4. **In production,** run `Discipline::sample_and_apply` on the busy-spinning runtime thread, which never idles, or cap C-states on the owner's core with per-CPU PM QoS. The costs to document: about 0.3 µs warm on Graviton, about 3.5 µs straight after deep idle on c7i.

How to verify:

- **Fixes 1 and 2 are tested on Linux** (Docker, arm64): the verifier may run on its `--cpu2` alone, and neither thread makes a voluntary context switch over a run (the sleeping owner made 989 in 2 s). Each run's CSV now says so (`owner_context_switches`, `verifier_context_switches`, `verifier_cpus`).
- **The cold-cost cause needs one c7i.2xlarge run.** Nothing local has `intel_idle` C6: the Mac is Apple silicon and Docker's VM is arm64. `LAB_CLOCK_ONLY=1`, 30 minutes of discipline, with and without `CSTATE=1`: about 0.85 h on spot at $0.40 an hour for the pair, about $0.34. If the cause is right: update p50 about 0.3–0.7 µs, verifier median about 45 ns, interrupted brackets 0–3.
- **Start the diagnostics before the discipline** (lib.sh `discipline_both`), so its maxima can be attributed.

## AWS c7i, c8g and c9g, and an M1 Pro (2026-10-16)

### Environment

| | c7i.2xlarge | c8g.xlarge | c9g.xlarge | M1 Pro (correctness only) |
|---|---|---|---|---|
| CPU | Intel Xeon Platinum 8488C (Sapphire Rapids) | Neoverse V2 (Graviton4) | Neoverse V3 (Graviton5) | Apple M1 Pro, 8 performance and 2 efficiency cores |
| Counter | TSC, 2.4 GHz (invariant, `rdtscp`) | `CNTVCT_EL0`, `CNTFRQ_EL0` 1 GHz | `CNTVCT_EL0`, `CNTFRQ_EL0` 1 GHz | `CNTVCT_EL0`, 24 MHz |
| `FEAT_ECV` | – | no | yes (`HWCAP2_ECV`) | no |
| Kernel clock source | `tsc` | `arch_sys_counter` | `arch_sys_counter` | none to read (macOS 27.0.1) |
| ergon-clock's rate | the hypervisor's TSC leaf, 2,400,000,000 Hz, which a 50 ms measurement could not tell apart (measured 2,399,999,912 Hz, bound 1.5 ppm) | the register, 1 GHz (measured 999,999,950 to 1,000,000,050 Hz across the runs: one tick in the 20 ms check) | the register, 1 GHz | the register, 24 MHz (only with `Trust::CrossCheck`; the default falls back: `no-kernel-vouch`) |
| OS | Debian 13, Linux 7.2.6+deb13-cloud-amd64 | Debian 13, Linux 7.2.6+deb13-cloud-arm64 | Debian 13, Linux 7.2.6+deb13-cloud-arm64 | macOS 27.0.1 |

- **AWS:** a pair of each type, spot, us-east-1a, each running nothing but `clockbench`. rustc 1.99.0, the workspace's release profile (LTO, one codegen unit), and the same without LTO.
- **States:** pinned (the boot after bootstrap), then isolated: `isolcpus=nohz,domain,managed_irq,1-3 nohz_full=1-3 rcu_nocbs=1-3 irqaffinity=0`. `clockbench` ran on CPU 1, its cross-core partner on CPU 3, beside the lab's gap spinner and a kernel trace.
- **Settings:** `clockbench`'s defaults: 200 batches of 10,000 reads, 1,000,000 gap reads, 100,000 handoffs each way, 120 s of drift (60 s on the Mac) and 20 calibration processes.
- **The tables** show each type's node a in the isolated state, built with LTO. [Every run](#every-run-drift-and-calibration) lists both nodes and both states. CSVs and logs: `target/md-dpdk-bench/aws-20261016-085442` (c7i), `aws-20261016-083958` (c8g) and `aws-20261016-084001` (c9g). Merge them with `python3 md-dpdk/benches/cloud/clockbench.py <dirs>`.
- **The Mac** ran unpinned (macOS has no CPU affinity) with other work on it, and no trace or gap spinner: correctness only. Its numbers are not a performance target, and its maxima are that load's preemptions. CSVs: `target/clockbench/mac-m1pro-20261016`.
- **Every clock passed its check on every host**, in both builds and every state. ergon-clock took the counter on every AWS host with the default trust (`Trust::Kernel`), at the rates above.
- **The ordered read in these runs was `isb; mrs cntvct_el0` on both Gravitons.** The c9g run predates the choice of `CNTVCTSS_EL0`. Its `raw_cntvctss` row is the bare instruction ergon-clock now takes there.

#### Time sync in these runs

The hosts' NTP client was systemd-timesyncd, not chrony. These were not precision-time runs (`ptp: not a precision-time run` in each run's `env-*.txt`). The lab installed chrony only for those (`md-dpdk/benches/cloud/vm.sh` in each run's `ergon.tar`: `if precision_time; then ... apt-get install ... chrony`), and none of the six bootstrap logs mentions it. Debian's EC2 image keeps time with systemd-timesyncd against the Amazon Time Sync Service at 169.254.169.123. systemd-timesyncd is an SNTP client: it slews the wall clock through the kernel's PLL and keeps no error bound. So the "NTP slew" figures below, up to +533 ppm and −412 ppm over 120 s, are systemd-timesyncd removing the offsets it found after each boot, not the oscillators' drift. Hosts that had settled were within 1.5 ppm; one (c8g b, isolated) started a fresh slew of about 10 ppm mid-run. The conclusions stand: ergon-clock's rate does not follow the slew, and quanta's and minstant's do. The rerun with chrony is [above](#aws-c7i-c8g-and-c9g-with-chrony-2026-10-16): settled, quanta was 2.7–19 ppm off the counter, not hundreds. Its CSVs carry the time-sync section (`meta`, above).

### Read cost, ns: median of the batch means (the slowest batch's mean)

| read | c7i.2xlarge | c8g.xlarge | c9g.xlarge | M1 Pro |
|---|---|---|---|---|
| `ergon_ticks` | 14.69 (15.40) | 12.14 (12.34) | 10.91 (12.58) | 0.32 (0.35) |
| `ergon_now_ns` | 15.63 (16.68) | 12.15 (12.36) | 10.91 (13.21) | 0.61 (1.35) |
| `ergon_now_ns_loaded` | 16.25 (17.37) | 12.15 (12.33) | 10.91 (13.19) | 0.64 (2.36) |
| `ergon_clock_now_ns` | 15.63 (16.40) | 12.15 (12.35) | 10.91 (12.72) | 0.72 (1.34) |
| `raw_rdtsc` / `raw_cntvct` (bare) | 14.69 (15.48) | 12.14 (12.35) | 10.91 (12.93) | 0.32 (0.63) |
| `quanta_raw` | 14.69 (15.50) | 12.14 (12.31) | 10.91 (12.61) | 0.64 (0.70) |
| `quanta_now` | 16.56 (17.65) | 12.15 (12.43) | 10.91 (12.64) | 1.22 (1.41) |
| `quanta_now_ns` | 15.88 (16.65) | 12.50 (12.70) | 10.92 (12.65) | 1.39 (1.75) |
| `quanta_instant_now` | 17.19 (18.14) | 12.21 (12.95) | 10.97 (13.30) | 3.27 (11.13) |
| `minstant_now` | 15.00 (15.85) | 40.01 (40.48) | 28.32 (30.52) | 23.72 (29.56) |
| `minstant_elapsed_ns` | 25.31 (26.31) | 45.27 (45.85) | 33.30 (35.57) | 31.98 (38.26) |
| `minstant_unix_ns` | 21.98 (22.48) | 43.01 (43.73) | 31.08 (33.32) | 30.13 (37.48) |
| `ergon_monotonic_now_ns` (the fallback) | 25.31 (26.58) | 30.75 (31.48) | 21.14 (23.64) | 17.42 (20.98) |
| `std_instant_now` | 25.32 (26.28) | 31.69 (32.08) | 21.44 (23.65) | 17.75 (22.53) |
| `std_instant_elapsed_ns` | 27.99 (29.34) | 39.12 (39.37) | 27.13 (29.82) | 23.98 (27.93) |
| `std_systemtime_ns` | 29.37 (30.23) | 39.54 (40.08) | 28.22 (30.51) | 23.88 (32.67) |
| `clock_gettime_monotonic` | 25.01 (26.06) | 30.92 (31.13) | 20.91 (23.45) | 22.94 (26.50) |
| `clock_gettime_monotonic_raw` | 25.32 (26.20) | 31.26 (31.47) | 20.98 (23.45) | 17.40 (21.82) |
| `clock_gettime_realtime` | 25.01 (27.10) | 30.93 (31.15) | 21.08 (23.49) | 16.38 (19.81) |
| `clock_gettime_monotonic_coarse` | 4.53 (6.76) | 8.58 (8.77) | 7.33 (7.94) | – |

- **ergon-clock's read is the bare instruction.** `ergon_ticks` equals the bare read on every host. `ergon_now_ns`, with its multiply, is within 0.94 ns of it on c7i and equal on both Gravitons; the gates passed.
- **The enum's branch** (`ergon_clock_now_ns` against `ergon_now_ns_loaded`) costs nothing measurable: −0.62 ns (c7i), 0.00, 0.00; on the Mac, +0.09.
- **Against quanta's held clock** (`quanta_now_ns`), ergon-clock's read is 0.25 ns cheaper on c7i, 0.35 on c8g and 0.00 on c9g. Against quanta's global `Instant::now`, the one most code calls, it is 1.56, 0.06 and 0.06 ns cheaper.
- **The slowest batches** are 0.2 to 2.9 ns above their medians. That is the batch one of the gaps [below](#gaps-between-consecutive-reads-ns-resolution-p50-p9999-max-steps-back) landed in: a 20 µs gap in a batch of 10,000 reads of 11 ns adds 2 ns to its mean.

### Ordered reads, ns: median (slowest batch)

| read | c7i.2xlarge | c8g.xlarge | c9g.xlarge | M1 Pro |
|---|---|---|---|---|
| `ergon_ticks_ordered` | 20.63 (21.45) `rdtscp` | 19.47 (19.68) `isb` | 18.64 (20.86) `isb` | 10.48 (17.29) `isb` |
| `ergon_now_ns_ordered` | 23.44 (24.49) | 21.93 (22.29) | 20.91 (23.83) | 12.65 (16.29) |
| `raw_rdtscp` | 20.63 (21.67) | – | – | – |
| `raw_lfence_rdtsc` | 21.25 (22.45) | – | – | – |
| `raw_rdtscp_lfence` | 25.31 (26.19) | – | – | – |
| `raw_isb_cntvct` | – | 19.47 (19.65) | 18.64 (20.09) | 10.55 (12.72) |
| `raw_cntvctss` (`FEAT_ECV`) | – | – | **11.37 (13.69)** | – |

- **On Graviton5, `CNTVCTSS_EL0` is 7.27 ns cheaper than `isb; mrs cntvct_el0`** (11.37 against 18.64 ns; slowest batches 13.69 against 20.09), only 0.46 ns more than the unordered read (10.91; slowest batch 12.93). ergon-clock now takes it wherever the operating system reports `FEAT_ECV`. These runs measured the bare instruction; ergon-clock's own read of it, measured later, is 11.36 ns ([the chrony runs](#aws-c7i-c8g-and-c9g-with-chrony-2026-10-16)).
- **`ergon_ticks_ordered` equals the bare ordered read** on every AWS host (gate: 0.00 ns).
- **An ordered read in ns back to back costs 2.3 to 2.8 ns more than the ordered ticks** (`ergon_now_ns_ordered`). The next read's barrier waits for the last one's multiply. A measurement bracket takes ordered ticks and converts their difference afterwards, and never pays it.
- **The choice of ordered read is a byte of the clock** (`OrderedRead`, at offset 32, after the conversion's four words; the `Clock` enum tells its variants apart by the domain byte after it). A read branches on it (`ldrb`, `cmp`, `b.ne`, [below](#the-hot-paths-code)), never on anything global; x86 has no branch. On the M1's `isb` path, three runs of `ergon_ticks_ordered` against `raw_isb_cntvct` measured:

  | run | `ergon_ticks_ordered` | `raw_isb_cntvct` | difference of medians |
  |---|---|---|---|
  | 1 | 10.55 (16.62) | 10.88 (19.55) | −0.32 |
  | 2 | 10.58 (16.24) | 10.55 (13.97) | +0.03 |
  | 3 | 10.94 (138.23) | 10.88 (16.12) | +0.06 |

  The fastest batches were 10.42 against 10.43, 10.42 against 10.42, and 10.55 against 10.42. That is no cost the `isb` does not hide. The runs were unpinned, at a load average of 16 to 18 from other builds, which caused the slowest batches (the 138.23 ns batch is preemptions). Criterion under the same load could not resolve fractions of a nanosecond. The branch on the `CNTVCTSS_EL0` path, where nothing hides it, is measured only on a Graviton5.

### Conversion alone, ns: median (slowest batch)

| conversion | c7i.2xlarge | c8g.xlarge | c9g.xlarge | M1 Pro |
|---|---|---|---|---|
| `convert_ergon_to_ns` | 0.79 (0.89) | 1.22 (1.46) | 2.46 (5.25) | 0.69 (1.03) |
| `convert_ergon_ticks_to_ns` | 0.99 (2.27) | 1.10 (4.32) | 2.07 (3.94) | 0.79 (3.13) |
| `convert_quanta_delta_as_nanos` | 1.14 (1.79) | 1.54 (1.76) | 1.22 (3.38) | 1.17 (3.41) |
| `convert_quanta_scaled` | 1.40 (1.45) | 1.58 (1.71) | 1.15 (1.16) | 1.12 (1.38) |

On c9g the conversion alone, in a loop that does nothing else, is 0.85 to 1.24 ns slower than quanta's. In a read (`ergon_now_ns` against `quanta_now_ns`, above) the two are equal there: the multiply overlaps the counter read. The slowest batches are again the batch a vCPU stall landed in: c9g's 5.25 ns is 28 µs more over 10,000 conversions.

### Built without LTO: ergon-clock's reads are `#[inline]`, quanta's are calls

| read, ns | c7i.2xlarge | c8g.xlarge | c9g.xlarge | M1 Pro |
|---|---|---|---|---|
| `ergon_ticks` | 14.69 (15.49) | 12.14 (12.34) | 10.91 (12.43) | 0.34 (3.87) |
| `quanta_raw` | 15.00 (16.05) | 12.15 (12.41) | 10.91 (13.23) | 0.95 (1.50) |
| `ergon_now_ns` | 15.63 (16.32) | 12.14 (12.34) | 10.91 (13.13) | 0.63 (2.15) |
| `quanta_now_ns` | 17.03 (17.80) | 12.15 (12.37) | 10.91 (13.17) | 1.49 (3.35) |
| `quanta_now` | 17.05 (17.91) | 12.15 (12.34) | 10.91 (12.64) | 1.60 (3.48) |
| `ergon_now_ns_ordered` | 23.44 (23.96) | 21.93 (22.12) | 20.91 (23.62) | 12.65 (18.98) |
| `convert_ergon_ticks_to_ns` | 0.97 (1.02) | 1.30 (4.10) | 3.93 (5.33) | 0.78 (0.82) |
| `convert_quanta_delta_as_nanos` | 1.14 (1.73) | 1.53 (1.71) | 1.22 (1.94) | 1.16 (1.25) |

Without LTO, quanta's `now_ns` is 1.40 ns slower than ergon-clock's on c7i.

### Gaps between consecutive reads, ns: resolution, p50, p99.99, max (steps back)

| read | c7i.2xlarge | c8g.xlarge | c9g.xlarge | M1 Pro |
|---|---|---|---|---|
| `ergon_ticks` | 12.5, 14.2, 1,783.3, 25,863.3 (0) | 7.0, 12.0, 918.0, 27,291.0 (0) | 5.0, 11.0, 898.0, 23,999.0 (0) | 41.7, 0.0, 1,208.3, 46,208.3 (0) |
| `ergon_ticks_ordered` | 19.2, 20.8, 2,283.3, 12,165.8 (0) | 13.0, 19.0, 907.0, 3,880.0 (0) | 12.0, 19.0, 829.0, 17,329.0 (0) | 41.7, 0.0, 83.3, 21,291.7 (0) |
| `ergon_now_ns` | 13.0, 15.0, 25.0, 18,879.0 (0) | 7.0, 12.0, 14.0, 1,955.0 (0) | 5.0, 11.0, 12.0, 20,277.0 (0) | 41.0, 0.0, 42.0, 333.0 (0) |
| `ergon_now_ns_ordered` | 21.0, 23.0, 33.0, 14,177.0 (0) | 16.0, 22.0, 24.0, 1,956.0 (0) | 15.0, 21.0, 87.0, 14,372.0 (0) | 41.0, 0.0, 83.0, 21,291.0 (0) |
| `ergon_clock_now_ns` | 14.0, 16.0, 29.0, 10,824.0 (0) | 7.0, 12.0, 14.0, 1,849.0 (0) | 5.0, 11.0, 12.0, 15,031.0 (0) | 41.0, 0.0, 42.0, 1,125.0 (0) |
| bare unordered (`raw_rdtsc`, `raw_cntvct`) | 12.5, 14.2, 25.0, 19,875.0 (0) | 7.0, 12.0, 14.0, 2,096.0 (0) | 5.0, 11.0, 12.0, 19,525.0 (0) | 41.7, 0.0, 41.7, 208.3 (0) |
| bare ordered (`raw_rdtscp`, `raw_isb_cntvct`) | 19.2, 20.8, 32.5, 10,358.3 (0) | 13.0, 19.0, 22.0, 2,014.0 (0) | 12.0, 19.0, 86.0, 22,917.0 (0) | 41.7, 0.0, 83.3, 20,541.7 (0) |
| `raw_cntvctss` | – | – | 5.0, 11.0, 12.0, 16,070.0 (0) | – |
| `quanta_now_ns` | 14.0, 16.0, 28.0, 12,041.0 (0) | 7.0, 12.0, 14.0, 1,914.0 (0) | 5.0, 11.0, 16.0, 17,041.0 (0) | 41.0, 0.0, 42.0, 1,000.0 (0) |
| `minstant_unix_ns` | 15.0, 22.0, 78.0, 10,439.0 (0) | 35.0, 43.0, 61.0, 2,078.0 (0) | 24.0, 31.0, 42.0, 23,721.0 (0) | 1,000.0, 0.0, 1,000.0, 23,000.0 (0) |
| `std_instant_elapsed_ns` | 25.0, 28.0, 180.0, 12,388.0 (0) | 32.0, 39.0, 61.0, 1,963.0 (0) | 20.0, 27.0, 39.0, 15,443.0 (0) | 41.0, 41.0, 125.0, 30,458.0 (0) |
| `clock_gettime_monotonic_raw` | 24.0, 26.0, 144.0, 16,945.0 (0) | 25.0, 31.0, 56.0, 2,036.0 (0) | 15.0, 21.0, 31.0, 15,415.0 (0) | 41.0, 0.0, 84.0, 111,792.0 (0) |
| `clock_gettime_realtime` | 23.0, 25.0, 148.0, 20,518.0 (0) | 24.0, 31.0, 56.0, 1,944.0 (0) | 14.0, 21.0, 32.0, 24,241.0 (0) | 1,000.0, 0.0, 1,000.0, 33,000.0 (1) |
| `clock_gettime_monotonic_coarse` | 3,999,899, 0, 0, 3,999,899 (0) | 4,000,514, 0, 0, 4,000,514 (0) | 3,999,999, 0, 0, 3,999,999 (0) | – |

- **No counter read stepped back on one thread**, on any host. The one step back is the Mac's `CLOCK_REALTIME`, the wall clock, which NTP steps.
- **The maxima, 10 to 27 µs in the isolated state,** hit every read alike: ergon-clock's, quanta's, the bare instructions and `clock_gettime`. c8g's host was quieter, at 2 to 4 µs for most reads, though its `ergon_ticks` still had one of 27 µs.
  - `clockbench.py` attributed each read's longest gap against the run's kernel trace and gap spinner.
  - In the isolated state almost none had a cause inside the guest: no interrupt, IPI or context switch on the CPU, and no stall of the spinner's CPU. The vCPU stopped without the guest seeing it, so these maxima are the platform's, not a clock's.
  - In the pinned state, where the CPU still took ticks, Graviton's were mostly the timer interrupt (`arch_timer`) and IPIs, and some on every host were preemptions.
- **The p99.99s of `ergon_ticks` and `ergon_ticks_ordered`** (0.8 to 2.3 µs; the same instructions measured later, `raw_rdtsc`, `raw_cntvct`, `raw_rdtscp` and `raw_isb_cntvct`, 12 to 156 ns, in all twelve LTO runs) are the benchmark's own page faults, not the reads. They are the first two rows measured, in every run. The gap pass warmed only the first 10,000 slots of its 8 MB buffer, and for those two rows glibc handed out memory never touched (the first buffer newly mapped, the second from a newly grown heap), so the timed pass first-touched the rest: about 2,000 faults of around a microsecond each (4 KiB pages; transparent huge pages were off). From the third row on, `calloc` reused memory and zeroed it before returning it, so it was already touched.
  - Reproduced on the Mac: with `raw_cntvct` measured first (`--only raw_cntvct`), its p99.99 was 1,166.67 ns against 41.67 ns later in a full run.
  - Fixed in `clockbench`: one untimed pass fills the whole buffer first, and the `delta` record carries the pass's page faults. With the fix, the Mac's first row had a p99.99 of 41.67 ns and no faults, and the unit test `the_gap_pass_takes_no_page_faults_while_it_times` fails without the fill (4,091 faults on macOS's 16 KiB pages, 31 on the Docker VM's huge pages).
  - The trace records no faults, so it cannot say whether they also made `ergon_ticks`' 27.3 µs maximum on c8g node a, where every other read's was at most 3.9 µs. A rerun records the faults and settles it.
- **`CLOCK_MONOTONIC_COARSE`** advances by the 4 ms tick (`CONFIG_HZ=250`): no use as a timestamp.
- **On the Mac** the counter ticks at 24 MHz (41.7 ns), so most back-to-back reads see the same tick (a median gap of 0). macOS's `CLOCK_MONOTONIC` and `CLOCK_REALTIME`, and so minstant there, have 1 µs resolution.

### Across two threads on two CPUs: second reads lower than the first, of 200,000 handoffs (largest step back, ns; offset, ns)

| read | c7i.2xlarge | c8g.xlarge | c9g.xlarge | M1 Pro |
|---|---|---|---|---|
| `ergon_ticks` | 0 (0.0; −1.7) | 0 (0.0; 0.5) | 0 (0.0; 0.0) | 0 (0.0; 0.0) |
| `ergon_ticks_ordered` | 0 (0.0; 0.0) | 0 (0.0; 1.5) | 0 (0.0; −2.5) | 0 (0.0; 0.0) |
| `ergon_now_ns` | 0 (0.0; 10.5) | 0 (0.0; 0.0) | 0 (0.0; 0.0) | 0 (0.0; 0.0) |
| `ergon_now_ns_ordered` | 0 (0.0; −3.0) | 0 (0.0; 1.0) | 0 (0.0; −3.0) | 0 (0.0; 0.0) |
| `raw_cntvctss` | – | – | 0 (0.0; −0.5) | – |
| `quanta_raw` | 0 (0.0; −32.5) | 0 (0.0; −1.0) | 0 (0.0; −1.5) | 0 (0.0; 0.0) |
| `quanta_now_ns` | 0 (0.0; −1.0) | 0 (0.0; 0.0) | 0 (0.0; 2.0) | 0 (0.0; 0.0) |
| `minstant_elapsed_ns` | 0 (0.0; 3.5) | 0 (0.0; −2.5) | 0 (0.0; −0.5) | 0 (0.0; 0.0) |
| `std_instant_elapsed_ns` | 0 (0.0; −0.5) | 0 (0.0; −0.5) | 0 (0.0; −0.5) | 0 (0.0; 0.0) |
| `clock_gettime_monotonic_raw` | 0 (0.0; 3.0) | 0 (0.0; 1.0) | 0 (0.0; 0.0) | 0 (0.0; 0.0) |

- **No read went backwards across cores** in the runs shown. The counters agree between cores to within a few ns.
- **An offset is half the difference of the two directions' least latencies**, so it carries any asymmetry between them. On c7i, `quanta_raw`'s −32.5 ns is the same `rdtsc` as `ergon_ticks`' −1.7 ns in the same run: one direction's least latency was 65 ns longer, and the counters were no further apart.
- **An unordered read may be taken before the handoff it follows.** Three of c7i node a's other runs saw it, never more than 5 of 200,000 handoffs:
  - `ergon_now_ns`: 5 by up to 54 ns without LTO, and 1 by 59 ns pinned;
  - `quanta_now_ns`: 1 by 32 ns;
  - `ergon_ticks`: 1 by 38 ns.

  Ordered reads never did, on any host or run.

### Drift against `CLOCK_MONOTONIC_RAW`: ppm at 120 s (largest error, ns)

That clock is the kernel's unadjusted scaling of its clock source, which ergon-clock calibrates against. Where that source is the counter, a counter clock's drift from it is how its conversion differs from the kernel's, not its accuracy.

| clock | c7i.2xlarge | c8g.xlarge | c9g.xlarge | M1 Pro (60 s) |
|---|---|---|---|---|
| `ergon_now_ns` | −0.048 (5,702) | 0.000 (6) | 0.000 (4) | −0.001 (84) |
| `quanta_now_ns` | −29.039 (3,485,125) | +130.653 (15,677,811) | 0.000 (1) | +3.332 (199,969) |
| `minstant_elapsed_ns` | −30.513 (3,662,032) | +102.422 (12,290,224) | +0.135 (16,231) | +2.882 (173,000) |
| `ergon_monotonic_now_ns` (`CLOCK_MONOTONIC`) | −1.495 (305,616) | +102.422 (12,290,222) | +0.135 (16,231) | 0.000 (22) |
| `std_instant_elapsed_ns` | −1.495 (305,619) | +102.422 (12,290,223) | +0.135 (16,231) | −0.000 (63) |
| `std_systemtime_ns` | −1.495 (305,617) | +102.422 (12,290,222) | +0.135 (16,232) | +2.876 (172,625) |

### Drift against `CLOCK_MONOTONIC`: ppm at 120 s (largest error, ns)

That clock is steered by the NTP client to its sources' rate, and slewed while the client removes an offset. A clock at the counter's raw rate drifts from it by the oscillator's error as the client sees it, plus any slewing during the run. In these runs the client was systemd-timesyncd, and the slew after each boot was most of the drift ([time sync in these runs](#time-sync-in-these-runs)).

| clock | c7i.2xlarge | c8g.xlarge | c9g.xlarge | M1 Pro (60 s) |
|---|---|---|---|---|
| `ergon_now_ns` | +1.448 (302,770) | −102.412 (12,290,222) | −0.135 (16,231) | −2.873 (172,417) |
| `quanta_now_ns` | −27.544 (3,305,656) | +28.228 (3,387,589) | −0.135 (16,231) | +0.454 (27,240) |
| `minstant_elapsed_ns` | −29.018 (3,482,561) | 0.000 (1) | 0.000 (1) | 0.000 (0) |
| `std_instant_elapsed_ns` | 0.000 (4) | −0.000 (2) | 0.000 (2) | −2.875 (172,542) |

- **ergon-clock keeps the counter's own rate in every process:** −0.048 ppm from the kernel's scaling of the TSC on c7i, and 0.000 on Graviton and the Mac.
  - On c7i, −0.048 ppm is the gap between the hypervisor's nominal 2.4 GHz, which ergon-clock takes because a 50 ms measurement cannot tell it apart, and the kernel's own calibration of the TSC. It is the same in all four c7i runs: 5.7 µs in 120 s.
  - On Arm both use `CNTFRQ_EL0`, so the conversion matches the kernel's to the nanosecond.
- **Against `CLOCK_MONOTONIC`, ergon-clock drifts by what the NTP client was doing during the run.** systemd-timesyncd was slewing c8g's `CLOCK_MONOTONIC` by +102 ppm, 12.3 ms in 120 s. That slew is not the counter's error, and not the oscillator's either: it is the client removing an offset after the boot. A trading process stamps with the counter and maps to the wall clock through a `WallOffset` it remeasures, so it needs the counter's rate to stay put while the client slews.

### Calibration in 20 fresh processes: ms min / median / max; the rate's spread between processes, ppm

| | c7i.2xlarge | c8g.xlarge | c9g.xlarge | M1 Pro |
|---|---|---|---|---|
| ergon-clock | 70.212 / 70.216 / 70.226 ms; 0.000 ppm (hypervisor) | 40.184 / 40.186 / 40.195 ms; 0.000 ppm (register) | 40.165 / 40.166 / 40.191 ms; 0.000 ppm (register) | 40.071 / 40.076 / 40.112 ms; 0.000 ppm (register) |
| quanta | 0.589 / 0.590 / 0.606 ms; 6.867 ppm | 0.579 / 0.579 / 0.585 ms; 4.768 ppm | 0.558 / 0.559 / 0.574 ms; 2.861 ppm | 200.003 / 200.004 / 200.009 ms; 4.583 ppm |

- **ergon-clock takes 70 ms on x86 and 40 ms on Arm (at most 70.226 and 40.195 ms),** once, at start-up, in an explicit call:
  - x86: a 50 ms measurement that confirms the hypervisor's nominal rate, and a 20 ms verification.
  - Arm: a 20 ms check of the register, and the verification.
  - Every process chooses the same rate.
- **quanta calibrates in 0.6 ms on Linux**, lazily, on the first `quanta::Clock::new` or `Instant::now` in a process. It measures against `CLOCK_MONOTONIC` and stops as soon as its mean error is under 10 ns.
  - Every process chooses a different rate.
  - On macOS, whose `CLOCK_MONOTONIC` has 1 µs resolution, it never gets there and runs to its 200 ms deadline.

### Every run: drift and calibration

All twelve LTO runs: both nodes, pinned (the first boot) and isolated (the second). They are ppm at 120 s against `CLOCK_MONOTONIC_RAW`. "NTP slew" is `CLOCK_MONOTONIC` against it: what systemd-timesyncd was doing during that run. That is mostly its slew after the boot, not oscillator drift ([time sync in these runs](#time-sync-in-these-runs)).

| run | NTP slew | `ergon_now_ns` | `quanta_now_ns` | `minstant_elapsed_ns` | ergon-clock spread | quanta spread |
|---|---|---|---|---|---|---|
| c7i a isolated | −1.495 | −0.048 | −29.039 | −30.513 | 0.000 | 6.867 |
| c7i a pinned | +224.088 | −0.048 | +543.165 | +543.079 | 0.000 | 10.294 |
| c7i b isolated | +0.407 | −0.048 | −2.718 | +0.186 | 0.000 | 4.578 |
| c7i b pinned | +26.622 | −0.048 | +197.554 | +200.325 | 0.000 | 18.308 |
| c8g a isolated | +102.422 | 0.000 | +130.653 | +102.422 | 0.000 | 4.768 |
| c8g a pinned | −411.535 | 0.000 | −978.470 | −411.535 | 0.000 | 64.907 |
| c8g b isolated | −10.134 | 0.000 | 0.000 | −10.134 | 0.000 | 2.861 |
| c8g b pinned | +81.957 | 0.000 | +276.566 | +81.957 | 0.000 | 9.534 |
| c9g a isolated | +0.135 | 0.000 | 0.000 | +0.135 | 0.000 | 2.861 |
| c9g a pinned | +86.391 | 0.000 | +297.546 | +86.391 | 0.000 | 8.581 |
| c9g b isolated | +533.462 | 0.000 | +848.770 | +533.462 | 0.000 | 12.387 |
| c9g b pinned | +90.742 | −0.000 | +304.222 | +90.742 | 0.000 | 11.441 |

- **ergon-clock's rate was the same in every run and every process.**
- **quanta's ranged from −978 to +849 ppm, as much as 1 ms in a second.** It follows whatever systemd-timesyncd was slewing `CLOCK_MONOTONIC` by during its 0.6 ms calibration, plus the noise of a 0.6 ms window. Its rate also differed between fresh processes on the same host by up to 64.9 ppm. These are boot-slew figures: with chrony settled ([above](#aws-c7i-c8g-and-c9g-with-chrony-2026-10-16)) quanta was 2.7–19.1 ppm off the counter, and its processes 1.9–6.9 ppm apart.
- **minstant on Graviton is `SystemTime`**, so it carries the slew exactly. On x86 its TSC calibration lands within a few ppm of quanta's.

### Gates and comparisons, ns

| | c7i.2xlarge | c8g.xlarge | c9g.xlarge | M1 Pro |
|---|---|---|---|---|
| gate `unordered_ns_over_bare_read` | 0.94: pass | 0.00: pass | 0.00: pass | 0.29: not enforced |
| gate `unordered_ticks_over_bare_read` | 0.00: pass | 0.00: pass | 0.00: pass | 0.00: not enforced |
| gate `ordered_ticks_over_bare_ordered_read` | 0.00: pass | 0.00: pass | 0.00: pass | −0.06: not enforced |
| `ergon_now_ns_ordered` − bare ordered read | +2.81 | +2.46 | +2.28 | +2.10 |
| `ergon_now_ns` − `quanta_now_ns` | −0.25 | −0.35 | −0.00 | −0.78 |
| `ergon_ticks` − `quanta_raw` | −0.00 | −0.00 | −0.00 | −0.32 |
| `ergon_now_ns` − `minstant_unix_ns` | −6.35 | −30.86 | −20.17 | −29.52 |
| `ergon_now_ns` − `std_instant_now` | −9.69 | −19.55 | −10.53 | −17.14 |
| `ergon_clock_now_ns` − `ergon_now_ns_loaded` | −0.62 | 0.00 | −0.00 | +0.09 |

The limit of every gate is 1.00 ns. From this change on, the ordered gate compares against the bare row of the instruction the clock's ordered read takes (`raw_cntvctss` on a Graviton5), and names it in its record.

## Why ergon-clock and not quanta (or minstant)

The question was whether to keep a clock of our own or use quanta: no point reinventing the wheel without a real gain, and this is for HFT. The measurements say keep ergon-clock.

1. **A read costs the same.** ergon-clock's `now_ns` is the bare counter instruction plus one multiply.
   - It is 0.25 ns cheaper than quanta's held clock on c7i, 0.35 ns on c8g and 0.00 on c9g (the chrony runs: 0.83, 0.33 and 0.00).
   - Against quanta's global `Instant::now` it is 1.56, 0.06 and 0.06 ns cheaper.
   - Without LTO, quanta's reads are calls and 1.40 ns slower on c7i.
   - Read cost is no reason to choose quanta; it is not a reason against it either.
2. **quanta's rate is NTP's, not the counter's, and different in every process.**
   - It calibrates in 0.6 ms against `CLOCK_MONOTONIC`, which NTP steers. With chrony settled (the [chrony runs](#aws-c7i-c8g-and-c9g-with-chrony-2026-10-16)) it landed within 1.2 ppm of `CLOCK_MONOTONIC`, so 2.7 ppm (c7i) and 19.1 ppm (c8g) off the counter: up to 19 µs a second.
   - Two processes on one host disagreed by 1.9–6.9 ppm.
   - Right after a boot it is far worse: while systemd-timesyncd slewed (the [reference runs](#time-sync-in-these-runs)), quanta was up to 978 ppm off and processes up to 64.9 ppm apart.
   - For HFT, stamps from the feed handler, the strategy and the order gateway must be comparable across processes and map to the wall clock through one offset. A rate that is per-process and off by several ppm breaks both, and after a boot by hundreds: 100 ppm is 100 µs a second.
   - ergon-clock takes the rate the kernel scales the same counter by (`CNTFRQ_EL0`, or the hypervisor's TSC rate once a 50 ms measurement cannot tell them apart). It verifies that rate and publishes it (`Params`), so every process converts a tick to the same nanosecond: 0.000 ppm spread, −0.048 to 0.000 ppm from the kernel, in every run.
3. **quanta has no ordered read.** A measurement bracket's end needs one.
   - ergon-clock's is `rdtscp`, `isb; mrs cntvct_el0`, or on a CPU with `FEAT_ECV` `mrs cntvctss_el0`.
   - On Graviton5 the bare `CNTVCTSS_EL0` costs 11.37 ns (slowest batch 13.69) against `isb`'s 18.64 (20.09): 7.27 ns cheaper, only 0.46 ns over an unordered read. ergon-clock's own read of it, measured later, is 11.36 ns (slowest batch 11.67), 7.30 ns under `isb` ([the chrony runs](#aws-c7i-c8g-and-c9g-with-chrony-2026-10-16)).
4. **ergon-clock's calibration is one explicit call, and says what it did.**
   - It calibrates at start-up (40 to 70 ms; at most 70.226 ms), reports what it chose and why (`Report`), and falls back to `CLOCK_MONOTONIC` with the reason where the counter cannot be trusted.
   - Its `Copy` clock has no global state, thread-local, lock or lazy initialisation on the read. (quanta's lazy set-up can also run at start-up; that is not a reason against it.)
5. **minstant is worse on both counts.**
   - On Graviton (and macOS) it falls back to `SystemTime`, the wall clock. That is not monotonic, costs 40 to 45 ns on c8g (slowest batches 40.48 to 45.85) and 28 to 33 ns on c9g (30.52 to 35.57), and carries every NTP slew.
   - On x86 its TSC calibration lands on NTP's rate, like quanta's: 2.9 ppm off the counter with chrony settled, and 0.2 to 543 ppm off while systemd-timesyncd slewed after boot.

quanta and minstant stay in `ergon-clock-bench` only, so the head-to-head can be rerun.

## Open

- **Graviton5's ordered read in a test.** [The chrony runs](#aws-c7i-c8g-and-c9g-with-chrony-2026-10-16) settled the cost: `ergon_ordered_read=mrs_cntvctss_el0`, `ergon_ticks_ordered` 0.02 ns over `raw_cntvctss`, 7.30 ns under `isb`. Still to run on a Graviton5: `cargo test -p ergon-clock`'s `the_ordered_read_waits_for_earlier_work`, which checks that the self-synchronised read waits for earlier cache-missing loads as `isb` does. Until then this rests on the Arm architecture (`CNTVCTSS_EL0` "is a self-synchronised view of the CNTVCT_EL0 counter, and cannot be read speculatively") and on Linux reading it in place of `isb; mrs cntvct_el0` wherever every CPU has `FEAT_ECV` (`arch_timer_read_cntvct_el0`, which its vDSO's counter read also takes).
- **The kernel's clock source after calibration.** A long-running process should call `Report::recheck()` from its housekeeping (a sysfs read, no allocation while the answer is yes). If the kernel stops keeping time with the counter (its watchdog judging the TSC unstable), the process calibrates again and hands every thread the new clock (`Discipline::rebase` anchors it where the old one reads, never behind). Nothing does so on its own: the clock is a `Copy` value and does not watch.
- **The fixed gap pass.** The reference runs' `ergon_ticks` and `ergon_ticks_ordered` gap tails include the pass's own page faults ([above](#gaps-between-consecutive-reads-ns-resolution-p50-p9999-max-steps-back)). With the fix, [the chrony runs](#aws-c7i-c8g-and-c9g-with-chrony-2026-10-16)' `ergon_ticks` p99.99 is 115.8 ns (c7i) and 14.0 ns (c8g), against 1,783.3 and 918.0 before.
- **A calibration's fallback in fresh processes** is now reported as no counter rate and why (`calib` record, `fallback:<reason>`). Before, it showed the monotonic clock's 1 GHz as a "rate" with 0 ppm spread. No run here fell back.
- **chrony.** [The chrony runs](#aws-c7i-c8g-and-c9g-with-chrony-2026-10-16) have drift on c7i and c8g; c9g's chrony run had no drift pass. Calibration in the `calib` rows does not include the time-sync probe: the children turn it off.
- **Conversion alone on c9g** is 3.91 ns against quanta's 1.22 (`convert_ergon_ticks_to_ns` against `convert_quanta_delta_as_nanos`). A read hides it; code that converts captured ticks afterwards pays it. Not yet explained.
- **The fallback's gate: fixed, awaiting a rerun.** `fallback_ns_over_clock_gettime` was 2.62 (c7i), 2.01 (c8g) and 1.58 ns (c9g) on node a in the chrony runs, over its 1.50 ns limit (node b: 1.47, 2.03, 1.46).
  - Cause: the fallback took the counter's formula, so its read ended in the 32.32 multiply (x86: `sub`, `imul`, `shrd`, `add`). The kernel's next read waits at its barrier (`rdtscp`, `lfence`, `isb`) for that result. So the multiply's latency, 6–9 cycles, was paid on every read. The old fallback added an offset: one cycle.
  - Fix: with a factor of exactly one (the fallback as made, until its owner disciplines it) the read takes the add, bit for bit the same result, behind one predicted branch on the factor. A disciplined fallback still multiplies.
  - Disassembly (release, LTO; arm64 and x86-64): the fallback's read is now `clock_gettime`, then `add` and `sub`. `CounterClock::now_ns` and `now_ns_ordered` are byte for byte unchanged. `Clock::now_ns`'s counter arm has no added branch or load ([the hot path's code](#the-hot-paths-code)).
  - Local proof, 5 interleaved runs each, median of 300 batch means:
    - M1 Pro (macOS): the fallback over `clock_gettime_uptime_raw` was 1.30–2.91 ns (median 2.14) before, 1.41–2.27 (2.12) with the branch never taken, and −0.23–0.24 (0.22) now.
    - Linux arm64 in Docker on the same Mac (`CLOCK_MONOTONIC` through the vDSO): the gate was 1.95–2.30 ns with the branch never taken (fail), and −0.24–0.42 (0.32) now (pass).
  - To confirm on AWS at the next cloud run.
- **c7i's no-LTO build failed two read gates** in the chrony run: `unordered_ns_over_bare_read` 1.72 ns (node a) and `ordered_ticks_over_bare_ordered_read` 1.05 ns (node b), limit 1.00. LTO passed on both nodes. The batch medians there sit on two levels about 1 ns apart (`raw_rdtsc` 13.43 or 14.24 ns, `raw_rdtscp` 18.86 or 20.00), and the bare read landed on the lower one. To repeat before reading it as a cost.
- **c7i's discipline updates** cost 3.5 µs at the median against 0.3 µs on Graviton. That figure came from the sleeping owner: every update ran straight after deep idle, with cold caches ([cause](#the-c7i-update-cost-cold-caches-after-deep-idle)). The bench now busy-waits and gives the verifier its own CPU ([the fix](#the-fix-and-how-to-verify-it)). Not rerun: one c7i.2xlarge run, with and without `CSTATE=1`, is still needed. Node a's slowest updates (18.7 µs at 143 s, 15.0 µs at 1,017 s) are not attributed: no diagnostics ran during the discipline.

## Earlier: Linux arm64 in Docker Desktop's VM on the M1 Pro (2026-10-16)

Correctness only, a VM's reads under Apple's hypervisor on a loaded laptop:

- **Environment.** Kernel 6.12.72-linuxkit, clock source `arch_sys_counter`, no high-resolution timers: `clock_getres` says 1 ms for every clock, though their reads advance by 41 ns.
- **Calibration's error bound.** Calibration takes the step that `CLOCK_MONOTONIC_RAW`'s reads advance by as that clock's resolution, not `clock_getres`'s 1 ms. Taking 1 ms had given an uncertainty of 49,957 ppm, enough to accept any candidate rate within about 10%. It also never takes a candidate more than 50 ppm from the measured rate. The default trust took the counter, at the register's 24 MHz, in 40.1 ms; twenty fresh processes chose the same rate.
- **quanta on a coarse counter.** It stopped in 0.55 to 13 ms (median 0.6 ms). Its rate comes from one pair of reads about 0.56 ms apart, and a 24 MHz tick is 74 ppm of that. So:
  - 19 of 20 processes chose rates within 2.5 ppm of the register, and one chose 72 ppm off, one tick;
  - the rates spread 34 ppm in another 20 processes and 142 ppm in a run of 5.

## The hot path's code

`#[inline(never)]` wrappers, release, one codegen unit, without LTO.

`CounterClock::now_ns`, 64-bit Arm (macOS and Linux alike):

```text
mrs   x8, CNTVCT_EL0
ldp   x9, x10, [x0]
sub   x8, x8, x9
ldr   x9, [x0, #16]
mul   x11, x8, x9
smulh x8, x8, x9
extr  x8, x8, x11, #32
add   x0, x10, x8
ret
```

`CounterClock::now_ns_ordered`, 64-bit Arm: the clock's `OrderedRead` byte picks `CNTVCTSS_EL0` (encoded `S3_3_C14_C0_6`, which every assembler accepts) or `isb` then `CNTVCT_EL0`, then the same multiply.

```text
ldrb  w8, [x0, #32]
cmp   w8, #2
b.ne  1f
mrs   x8, S3_3_C14_C0_6
b     2f
1: isb
mrs   x8, CNTVCT_EL0
2: ldp x9, x10, [x0]
...   (as now_ns)
```

`CounterClock::now_ns`, x86-64 (Linux): the DPDK transport's sequence, with no branch and no call.

```text
rdtsc
shlq   $32, %rdx
orq    %rdx, %rax
subq   (%rdi), %rax
imulq  16(%rdi)
shrdq  $32, %rdx, %rax
addq   8(%rdi), %rax
retq
```

`now_ns_ordered` on x86-64 is the same with `rdtscp` and one more instruction: no branch, since x86 has one ordered read.

A disciplined clock is re-anchored by value (`Clock::retuned`), so these reads are unchanged by it: `CounterClock::now_ns` and `now_ns_ordered` assemble byte for byte as they did before the fallback took the counter's formula, on both architectures (release, LTO; diffed), and so they do after the fallback's factor-one read. `Clock::now_ns`, the enum, still tests one tag byte (`[x0, #33]`, 2 for the fallback).

The fallback arm compares its factor with one (2^32). When it is one (the fallback as made, until disciplined) the read is `clock_gettime`, then `add` and `sub`: no multiply waits for the kernel's next read. Otherwise it multiplies, laid out after the counter arm (a `#[cold]` hint).

- Arm: the counter arms of `Clock::now_ns` and `now_ns_ordered` run the same instructions as before.
- x86-64: each counter arm ends `shld; add; mov` where `CounterClock` ends `shrd; add`: one register move more, at the end. It is the compiler's choice of register for the result (`rdx`), not the multiply's jump into that tail: with the multiply in a cold call of its own, the move stays. Before, `Clock::now_ns` had two at the start and `now_ns_ordered` none.
- Neither counter arm gained a branch or a load.

quanta's `delta_as_nanos(origin, raw())`, x86-64. `Clock::raw` is not `#[inline]`, so without LTO it is a call. Then come a branch on the clock's kind, a saturating compare and a variable shift:

```text
pushq  %r14 ; pushq %rbx ; pushq %rax
callq  *quanta::Clock::raw@GOTPCREL(%rip)
subq   %r14, %rax ; jbe .zero
cmpl   $1, (%rbx) ; jne .done
mulq   24(%rbx)
movzbl 32(%rbx), %ecx ; movq %rdx, %rsi ; shrq %cl, %rsi ; shrdq %cl, %rdx, %rax ; testb $64, %cl ; cmovneq %rsi, %rax
.done: popq ... ; retq
```
