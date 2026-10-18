# ergon-clock

ergon-clock reads the time from the CPU's counter: one instruction and one
multiply. It gives UTC nanoseconds since 1970 and keeps them on UTC. It is
for busy-spin threads (they loop, never sleep) that read the time millions
of times a second.

> **AI assistance.** Written with heavy AI assistance
> ([disclosure](../project/ai-assistance.md)). Not production-ready.

```toml
[dependencies]
ergon-clock = "0.1"
```

Numbers: ns on AWS c7i.2xlarge (x86), c8g.xlarge (Graviton4) and
c9g.2xlarge (Graviton5), median (slowest batch) ([Measured](#measured)).
One copy of every figure, with its max and slowest batch, is in
[`clock/BENCHMARKS.md`](https://github.com/mimran1980/ergon/blob/main/clock/BENCHMARKS.md).

## Words used here

| word | meaning |
|---|---|
| hardware counter | A register in each CPU core that counts up at a fixed rate: the TSC on x86-64 (`rdtsc`), `CNTVCT_EL0` on 64-bit Arm. One instruction reads it. |
| calibrate | Find, once at start-up, how many counter ticks make a second, so each later read is one multiply. |
| wall clock | `CLOCK_REALTIME`: the time of day, UTC. Pro: comparable across hosts and with exchange and NIC stamps. Con: NTP or an admin can step it back or forward. |
| monotonic clock | `CLOCK_MONOTONIC`, `std::time::Instant`. Pro: never goes back. Con: counts from boot, so not comparable across hosts. |
| UTC | The world's time standard: what exchanges and NICs stamp in. |
| epoch | A clock's zero: 1970-01-01 00:00 UTC for UTC stamps; boot for a monotonic clock. |
| NTP | Network Time Protocol: keeps the wall clock near UTC over the network (chrony, ntpd). |
| PTP | Precision Time Protocol: time from a NIC's hardware clock, well under a microsecond from UTC. |
| ppm | Parts per million, a rate error: 1 ppm is 1 µs a second, 3.6 ms an hour. |
| drift | How far a clock moves from true time because its rate is off. |
| gap | How far the clock has run from `CLOCK_REALTIME`: what `update` closes, and what the `Gap` warning reports. |
| owner, follower | The one thread holding the `Clock` and calling `update` is the owner. Each other thread gets a `Follower`: a reader of the owner's published state. |
| step, slew | Two ways to correct a clock. A step jumps at once, maybe backwards. A slew runs the clock a little fast or slow until it catches up. |
| disciplined clock | A clock whose rate is steered, a little at a time, to follow another. |
| unordered, ordered read | An unordered read may run before earlier work finishes: cheapest. An ordered read waits for it. |
| vDSO | Kernel code mapped into each process: `clock_gettime` and `Instant::now` run there, with no system call. |
| fallback | What ergon-clock reads when it cannot trust the counter: `CLOCK_MONOTONIC` plus an offset, so the stamps stay UTC, at about `Instant`'s cost. |
| hypervisor clocksource | On some VMs Linux keeps time with the hypervisor's clock (Azure's `hyperv_clocksource_tsc_page`, `kvm-clock`), not the counter. |

## Why it exists

- **Cheap reads:** one counter read and one multiply, faster than
  `Instant::now`.
- **UTC stamps for that price**, and durations that never go negative.
- **An ordered read** for timing short sections.
- **One rate** shared by every process on a host (they convert ticks the
  same way), and a `Params` layout to do the same in C.

## Do I need it?

`std::time::Instant` is enough for durations at low rates. It has no UTC.

| your code | use |
|---|---|
| times requests, functions or tests; under about 100,000 reads a second | `std::time::Instant` |
| needs UTC stamps, to compare with an exchange, a NIC or another host | ergon-clock |
| a busy-spin thread: millions of reads a second | ergon-clock |
| dates and time zones | `std::time::SystemTime` and a calendar crate |

## What does `now_ns` return?

- **UTC nanoseconds since 1970**, a `u64`. A duration is a difference:
  `clock.since_ns(start)` or `elapsed_ns(start, end)`, never negative.
- **Kept on UTC** by `clock.update()` from the loop's idle time. Without
  it the clock drifts by the hardware's rate error (up to 73 ms an hour);
  only `update()` and `health()` report that, so call one of them.
- **Since boot** instead: `Config { domain: Domain::Monotonic, .. }`.

## Why is it faster?

```text
Instant::now()   in the vDSO: check the kernel's time data ─► read the counter (ordered)
                 ─► scale ─► add ─► check again

clock.now_ns()   read the counter ─► one multiply, by numbers set at start-up
                 and changed only by `update` between reads
```

The kernel checks twice because NTP may change its numbers mid-read.
ergon-clock's numbers change only between reads: the owner thread rewrites
them in `update`, readers see one consistent set at a time.

## Ordered reads

A CPU starts an instruction once its inputs are ready. A counter read has
no inputs and no memory dependency, so `now_ns` may run before the work
above it has finished. `now_ns_ordered` waits for every earlier
instruction: `rdtscp` on x86; on Arm `isb` then the read, or
`CNTVCTSS_EL0` where the CPU has it (Graviton5). `now_ns_begin` is the
read for a measurement's start: ordered after earlier work and, on x86,
fenced so the region's work cannot start before it (`rdtscp; lfence`).

An unordered read is **not** fine at the start of a measurement. The
region's work can begin before the stamp is taken, so the interval comes
out too short.

**Without an ordered end** a measurement's end is read early. A stamp
taken after a hand-off from another thread can come out earlier than that
thread's. Ordered stamps never did. A later event must never carry an
earlier stamp. Data:
[`clock/BENCHMARKS.md`](https://github.com/mimran1980/ergon/blob/main/clock/BENCHMARKS.md).

```rust,ignore
{{#include ../../../clock/tests/guide.rs:ordered_read}}
```

**The compiler moves code too.** `now_ns_ordered` is an `asm!` block
without `nomem`: no load or store it could alias moves across it. `now_ns`
is the same on x86 (LLVM treats `rdtsc` as touching memory), not on Arm
(`asm!` with `nomem`), where loads and stores can move across it. Work that
touches no memory can still move either way: a compiled probe moved pure
arithmetic out from between two ordered reads. Pass the work's input
through `std::hint::black_box` after the start read, and its result before
the end read, as above. `black_box` is not a CPU fence: it stops the
compiler, and the read's own ordering is what stops the CPU.

A stamp is when the instruction read the counter. It is not proof that a
store has reached memory or that a packet has left the NIC: a store can
sit in the store buffer, and a packet can sit in the TX ring, long after
the stamp. Measure those ends on their own — read the store back, or use
the NIC's transmit timestamp and the peer's receive stamp.

## Which read when

| read | for |
|---|---|
| `now_ns` | event stamps, deadlines, ages. Never a measurement |
| `now_ns_begin` | the start of a measurement |
| `now_ns_ordered` | the end of a measurement; stamps that must never be earlier than another thread's (a follower can still be about 1 ns early at an update) |

- **Unordered, like `now_ns`:** quanta's reads, and minstant's
  `Instant::now()` and `elapsed()` (minstant 0.1.7 reads a plain
  `_rdtsc()`; `elapsed()` is `Instant::now() - self`).
- **Ordered, like `now_ns_ordered`:** `std::time::Instant` on Linux (the
  vDSO uses `rdtsc_ordered` on x86, `isb` or `CNTVCTSS_EL0` on Arm).

## A low-latency loop

Calibrate once; stamp each event with `now_ns`; time its handling up to
`now_ns_ordered` and keep the slowest (the event's stamp starts this one,
because the question is how long from arrival to done; a measurement of the
work alone starts at `now_ns_begin`); `update` when idle — idle meaning
between two pieces of work, even in a loop that never sleeps:

```rust,ignore
{{#include ../../../clock/tests/guide.rs:busy_loop}}
```

## Why not quanta or minstant?

Not speed: on x86 all read at about the same cost. The reasons:

| | ergon-clock | quanta | minstant |
|---|---|---|---|
| one rate in every process, and in C | yes: the same calibration in every process | no: measured per process, and the rates differ | x86: measured per process |
| ordered read | yes | no | no |
| UTC stamps kept on UTC, at counter cost | yes | no | no: a fixed offset |
| on Graviton | the counter | the counter | the wall clock, and it can go back |
| checks that Linux keeps time with the counter | yes | no: x86 CPU flags only, nothing on Arm | only that `tsc` is listed in `available_clocksource` |

## Details

### Calibration and the fallback

`calibrate` blocks once, for tens of milliseconds, plus a short time-sync
probe. It checks the counter (by default Linux must keep its own time with
it: clock source `tsc` or `arch_sys_counter`; a read must be cheap, or a
hypervisor traps it), takes its rate (Arm's frequency register, one read;
x86: a short measurement against `CLOCK_MONOTONIC_RAW`), verifies it, and
anchors the result to `CLOCK_REALTIME`.

```rust,ignore
{{#include ../../../clock/tests/guide.rs:calibrate}}
```

- **Fallback:** if a check fails, the clock reads `CLOCK_MONOTONIC` (macOS:
  `CLOCK_UPTIME_RAW`) plus an offset, about `Instant`'s cost, and
  `calibrate` says why. `advanced::Policy::RequireCounter` makes it an
  error.
- **Why not read the wall clock and clamp it** (`max(last, now)`)? It would
  never go back, but after a backward step it would stand still for the
  size of the step (durations read 0), and a forward step would still jump.
  ergon-clock never reads the raw wall clock: its UTC clock is the counter
  (or `CLOCK_MONOTONIC`) plus an offset that slews within ±500 ppm. It never
  goes back, never stands still and never jumps after start.
- **Recheck:** Linux can stop trusting the counter later.
  `report.recheck()` from an idle-timer callback says so; then calibrate
  again and make the new clock's followers again (the old ones keep the old
  calibration and drift). A new calibration is a new anchor, so its first
  stamps can jump; the front API has no step after that.

### Virtual machines

Where Linux keeps time with a hypervisor's clock (Azure by default), and on
macOS, the clock falls back and says so at start-up:

```text
ergon-clock warning: the clock reads CLOCK_MONOTONIC, not the hardware counter: slower, about what Instant::now costs; why: Linux keeps time with hyperv_clocksource_tsc_page, a hypervisor's clock; fix: Config { trust: ergon_clock::advanced::Trust::CrossCheck, .. } lets calibration's own checks take the counter
```

`advanced::Trust::CrossCheck` takes the counter if calibration's own checks
pass. Not yet measured on such a VM. Every AWS host here uses the counter
by default.

### `update` and health

- **Call `clock.update()` from idle time**, as often as you like. At most
  once a second it samples `CLOCK_REALTIME` and steers the clock (a few
  microseconds, more after deep idle: [UTC and Time Sync](wall-clock.md)).
  Every other call is one counter read, a compare and the health maths; on
  the fallback it is one `clock_gettime` too. It never prints, blocks or
  allocates.
- **It never jumps after start:** it slews at most 500 ppm (0.5 ms a
  second), so closing a 100 ms gap to `CLOCK_REALTIME` takes about 3.3
  minutes and 1 s about 33 minutes. Stamps are off until then.
- **It returns a `Health`:** time since the last good update, the gap to
  `CLOCK_REALTIME`, the error bound from the host's time sync, and the
  catch-up time at 500 ppm. `health.warning()` is `Stale` after 5 s
  without a good update, else `Gap` when the gap is over
  `Config::gap_warn_ns` (1 ms). `warning.line()` is the one-line message,
  for your logger. `health.sampled` says this call took its once-a-second
  sample: log only then.
- **Other threads** read through `clock.follower()`, moved there at
  start-up. One follower per thread, made from the clock that is still
  being updated.
- **Not live yet and warned of a gap?** Wait for time sync to settle and
  calibrate again. The front API has no "step once" option: a forward step
  cannot fix a clock that is ahead, a backward step is ruled out (it would
  go back), and steps make followers jump and timers fire. One is available
  under `advanced::discipline` if you need it.

### Start-up warnings

`calibrate` prints one line to stderr per problem: it fell back; or, in
UTC, the host's clock is not synchronised; not settled (chronyd or the
kernel PLL is still correcting it by more than `gap_warn_ns`); or has a
leap second pending. The line says how long the correction takes at 500
ppm.

```text
ergon-clock warning: UTC: the host's clock is not settled: chronyd is still correcting it by 120.00 ms; a disciplined clock follows at most 0.5 ms a second, so UTC stamps can be off for at least 4.0 min; start after time sync settles
```

`ERGON_HOST_CHECKS=off` (or `0`) or `print_warnings: false` turns the lines
off; `report.warnings()` keeps them (the report has a second list,
`findings()`, of every thing the calibration looked at). `advanced::calibrate`
never prints.

### Measured

A read is a few tens of nanoseconds on every host measured; an ordered
end costs more than an unordered read. The longest stall between two
`now_ns` reads was 7.4 µs (c7i), 3.2 µs (c8g) and 3.9 µs (c9g) — the VM
pausing the vCPU. No read went backwards. One copy of every figure, with
its max and slowest batch, is in
[`clock/BENCHMARKS.md`](https://github.com/mimran1980/ergon/blob/main/clock/BENCHMARKS.md#aws-c7i-c8g-and-c9g-with-chrony-2026-10-16).

### Fake time for tests

Write time-reading code against `TimeSource`. The real clocks, `Follower`
and `ManualClock` (time moves only when you move it) implement it, with no
dynamic calls:

```rust,ignore
{{#include ../../../clock/tests/guide.rs:fake_time}}
```

### The `advanced` module

Everything else: `advanced::Clock`, a fixed `Copy` clock that never
follows updates, and its silent `advanced::calibrate`; `segment`, that
clock's current state, for raw-tick bracketing; raw ticks (`ticks`,
`ticks_ordered`, `ticks_to_ns`); `Params`, a calibration in C layout (the
[DPDK transport](../md-dpdk/overview.md) uses it); `Trust`, `Policy`, the
discipline and seqlock under `update` and `Follower`, `WallOffset` and the
time-sync probe.

### Best practices

- Calibrate once, before the hot loop; log `clock.report()`.
- Call `update` from idle time; log its warnings off the hot path, and
  only when `health.sampled` is true. The owner/follower picture is in
  [UTC and Time Sync](wall-clock.md).
- One `Follower` per other thread; no lock, no `Arc`.
- Under about 1 µs, bracket with
  `advanced::segment(&clock).ticks_ordered()` at both ends and convert the
  difference once: raw ordered ticks, no UTC conversion. For a start and an
  end that must be UTC stamps, use `now_ns` then `now_ns_ordered` as usual.
- Never mix since-boot and UTC stamps.
- Never stamp with `CLOCK_MONOTONIC_COARSE`: its 4 ms steps.

### Verify

```sh
cargo test -p ergon-clock --all-features -- --test-threads=1
cargo run -p ergon-clock --example timesync                 # this host's clocks and time sync
cargo run --release -p ergon-clock-bench --bin clockbench   # the benchmarks, on your host
```

## References

- Intel SDM: [RDTSC](https://www.felixcloutier.com/x86/rdtsc) ("not a
  serializing instruction"), [RDTSCP](https://www.felixcloutier.com/x86/rdtscp)
  ("wait until all previous instructions have executed"); the
  [manuals](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html).
- Linux, [`arch/x86/include/asm/tsc.h`](https://github.com/torvalds/linux/blob/master/arch/x86/include/asm/tsc.h):
  `rdtsc()` "without ordering constraints", `rdtsc_ordered()` "in program
  order".
- Linux, [`arch/arm64/include/asm/arch_timer.h`](https://github.com/torvalds/linux/blob/master/arch/arm64/include/asm/arch_timer.h):
  `isb` before `mrs cntvct_el0`, or `CNTVCTSS_EL0` with ECV.
- Arm ARM D6.1.3, quoted on
  [linux-arm-kernel (2017)](https://lists.infradead.org/pipermail/linux-arm-kernel/2017-May/505057.html):
  reads of `CNTVCT_EL0` "can occur speculatively and out of order".
- Gabriele Paoloni, [How to Benchmark Code Execution Times on Intel IA-32
  and IA-64 Instruction Set Architectures](https://cis.temple.edu/~qzeng/cis3207-spring18/files/ia-32-ia-64-benchmark-code-execution-paper.pdf),
  Intel white paper, 2010 (Intel's own link is gone).
- Mike Spertus, [P0342R1, What does "current time" mean?](https://open-std.org/JTC1/sc22/wg21/docs/papers/2023/p0342r1.pdf),
  WG21, 2023: the compiler "reordered the code to get the end time prior to
  running the calculation".
- Ali Farazdaghi, [Timing Using RDTSC on x86_64](https://alifara.codeberg.page/posts/timing-using-rdtsc/), 2025.
- Marek Majkowski, [Counting cycles - RDTSC](https://idea.popcount.org/2013-01-28-counting-cycles---rdtsc), 2013.
- A real bug: D's druntime, bug 20118 (now
  [dmd#17181](https://github.com/dlang/dmd/issues/17181)), "rdtsc is
  unordered without preceding memory barrier", 2019, still open.
