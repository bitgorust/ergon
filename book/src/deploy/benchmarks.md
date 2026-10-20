# Benchmarks: x86 and Graviton

Aeron transport latency between two hosts — not SBE codec benchmarks (those
are [`sbe/BENCHMARKS.md`](https://github.com/mimran1980/ergon/blob/main/sbe/BENCHMARKS.md)).
Which platform: latency here, then [cost](#cost-per-hour). Each transport
on each platform, Azure included:
[the DPDK chapter](../md-dpdk/overview.md#measured-between-two-hosts).

Terms: [deployment glossary](overview.md#glossary),
[network and DPDK glossary](../md-dpdk/overview.md#glossary).

> The DPDK rows are this repository's
> [proof-of-concept transport](../md-dpdk/overview.md). No other kernel-bypass
> product was run on this hardware, so none is compared in these tables.

**In short:** Graviton5 (`c9g.2xlarge`) had the lowest idle medians measured
(13.69 µs DPDK, 18.54 kernel), at half the hourly price of the x86 pair with
as many physical cores. Its median lead (7 µs) is no more than placement alone
moved an x86 pair, so it is not proven. Worst round trip under load at 1 M
msgs/s: 105 and 114 µs on two of the three Graviton pairs, against 155 on the
x86 VM and 103 on metal; the third pair saw 166 µs. Metal costs 12 times as
much and its idle DPDK maximum was the highest of the three idle DPDK maxima
(845 µs).

## Method

**Method.** Aeron ping-pong between two hosts in one cluster placement group,
us-east-1, Debian 13, Linux 7.2.6, Aeron 1.53.3, isolated CPUs, 5 reps. Idle:
32 B, one in flight, 300,000 round trips a rep. Under load: 288 B at 1 M
msgs/s, latency from each message's intended send time. p50 and p99.99 are the
median across the five reps of that rep's percentile. **max is the largest
single round trip in any rep** — 1,500,000 idle round trips a cell — not the
median of the per-rep maxima. That median hides single stalls: metal idle DPDK
ran 48, 57, 71, 83 and 845 µs across its five reps, so a "71 µs max" is the
middle rep, not the worst.

### How figures are aggregated

Six different things are called "a number" in this book. They are never
compared to each other, and a table cell names which one it is.

| aggregation | what one value is | how it is summarised |
|---|---|---|
| batch throughput | messages or operations completed in a timed window | the window's rate; a mean of windows, never a percentile |
| individual latency | one round trip or one call | percentiles of the individuals, and the absolute maximum |
| sampled clock residual | one sample's offset from a reference clock | percentiles of the samples; the sample rate and window are part of the figure |
| UTC accuracy | one stamp's error against the wall clock | percentiles of the errors, with the discipline interval named |
| median of repetition maxima | the worst value in one rep, then the middle rep | a summary of the reps' worst, **not** a worst case |
| absolute observed maximum | the single worst value in any rep | one number; the per-rep spread goes in a note beside it |

"Max" unqualified means the absolute observed maximum. Where a cell is the
median of per-rep maxima it says so.

In [`md-dpdk/BENCHMARKS.md`](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md)
the same quantities are written `max X (a–b)`: X is the median of the five
per-rep maxima, the brackets hold the five's smallest and largest, and the
bracket's upper end is the absolute observed maximum. Never read X as a worst
case.

**Provenance.** Each published figure cites its run directory under
`target/md-dpdk-bench/`. Every run keeps `commit`, `dirty` (the working tree at
run time), `ergon.tar` (the source-content snapshot, so the hash is the tree),
`env-*.txt` (kernel, DPDK, CPU, rustc, NIC, CPU list), the kernel command line
and isolation settings in the `*-tune-*.log` files, the per-mode flag in the
mode name (for example `dpdk@AERON_RECEIVER_IO_VECTOR_CAPACITY=16`), and
`results.csv` with `summary.md`. `summary.md` prints `median (min–max of rep
maxima)`; this book quotes from that, and names the worst rep when it differs.

## Latency

"Kernel" is the best kernel path measured on that platform: busy polling
with a 4,096-entry ring on x86; interrupt moderation off with the NIC's
interrupts on a core of their own on Graviton (busy polling collapsed there
at 1 M msgs/s).

| instance | idle, kernel: p50 / p99.99 / max | idle, DPDK: p50 / p99.99 / max | 1 M msgs/s, kernel: p50 / max (lossy reps) | 1 M msgs/s, DPDK: p50 / max (lossy reps) |
|---|---|---|---|---|
| `c7i.4xlarge`, x86 VM | 28.22 / 56.48 / 4,846 | 21.28 / 43.71 / 116 | 43.74 / 6,664 (1 of 5) | 30.38 / 155 (0 of 5) |
| `c7i.metal-24xl`, x86 metal | 33.70 / 44.54 / 307 | 28.13 / 38.72 / 845 | 42.94 / 1,150 (0 of 5) | 34.30 / 103 (0 of 5) |
| `c9g.2xlarge`, Graviton5 VM | 18.54 / 38.14 / 155 | 13.69 / 28.67 / 155 | 44.9 / 216 (0 of 5) | 19.0 / 166 (0 of 5) |

The five per-rep maxima behind each max (smallest and largest of the five, in
the table's order): idle kernel 349–4,846, 55–307, 66–155; idle DPDK 52–116,
48–845, 68–155; 1 M msgs/s kernel 536–6,664, 114–1,150, 111–216; 1 M msgs/s
DPDK 139–155, 77–103, 92–166.

Runs: `c7i.4xlarge` round 7, 2026-10-15, `aws-20261015-185813`
([data](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md#aws-c7i4xlarge-round-7-p50--p999--max-median-across-reps));
`c7i.metal-24xl` round 4, 2026-10-15, `aws-20261015-012400`
([data](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md#conclusions));
`c9g.2xlarge` round G1, 2026-10-16, `aws-20261016-072005`, spot, built with
`-mcpu=native`
([data](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md#conclusions)).

- **Graviton5 against the x86 VM:** idle median 13.69 against 21.28 µs on
  DPDK, 18.54 against 28.22 on the kernel path. At 1 M msgs/s DPDK 19.0
  against 30.38; the kernel paths were level (43–45 µs).
- **Tail under load (DPDK, 288 B, worst round trip in any rep):** at 100 k
  msgs/s 83–101 µs on the three Graviton pairs against 123 on the x86 VM. At
  1 M msgs/s 105 (G2), 114 (G3) and 166 (G1) against 155 on the x86 VM and
  103 on metal: G2 and G3 beat the VM, G1 does not.
- **Bare metal:** the lowest median of per-rep idle-kernel maxima, 66.94 µs
  (55.20–306.69), but the highest medians. Its absolute idle-kernel maximum
  was 306.69 µs, so the "lowest" holds only on that label. Its kernel rows
  predate the interrupt re-pinning fix, so they are pessimistic.
- **Treat the gap with care.** Each row is a different pair and round.
  Placement alone moved an x86 median by 7 µs, and an earlier `c9g.xlarge`
  pair's DPDK median was 19.9 µs, not 13.7. x86 had more tuning rounds.
- **Two more Graviton pairs reproduced the medians:** DPDK 13.78 and 13.97,
  kernel 18.83 and 18.93 µs (rounds
  [G2](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md#conclusions),
  `aws-20261016-135004`, and
  [G3](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md#graviton5-c9g2xlarge-round-g3--best-measured-p50--p9999--max),
  `aws-20261016-223251`).

ergon-io's and ergon-clock's x86 and Graviton results are in their own
chapters: [io](../io/overview.md), [clock](../clock/overview.md).

## Cost per hour

A pair is two instances. us-east-1, Linux; on-demand list prices, and spot
prices as of 2026-10-15 (instances.vantage.sh), as
[`estimate.py`](https://github.com/mimran1980/ergon/blob/main/md-dpdk/benches/cloud/estimate.py)
holds them. Spot moves by the hour and the zone.

| pair | physical cores each | on demand, $/h | spot, $/h | measured in |
|---|---|---|---|---|
| `c7i.2xlarge` | 4 (8 vCPUs) | 0.714 | 0.400 | AWS rounds 1–2, the kernel moderation A/B |
| `c7i.4xlarge` | 8 (16 vCPUs) | 1.428 | not recorded | rounds 3 and 5–7, EKS |
| `c7i.metal-24xl` | 48 (96 vCPUs) | 8.568 | not recorded | round 4 |
| `c8g.xlarge` | 4 | 0.319 | 0.154 | the clock runs |
| `c9g.xlarge` | 4 | 0.348 | not recorded | the earlier Graviton5 pair |
| `c9g.2xlarge` | 8 | 0.696 | 0.306 | rounds G1–G3 |

- **Metal costs 6 times a `c7i.4xlarge`** and 12 times a `c9g.2xlarge`, for
  a tighter tail and fewer stalls, not a lower median.
- Azure pricing is not measured in the lab and is not stated here: the cost
  figures above are AWS list prices only, from the dated table in
  [`md-dpdk/BENCHMARKS.md`](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md).

## Bare metal

`c7i.metal-24xl`, round 4: no hypervisor, so the high percentiles tighten.
DPDK's idle p99.99 was 38.72 µs against 56.06 on a `c7i.4xlarge` (round 3),
and it lost nothing up to 1.25 M msgs/s of 288 B; the worst round trip at each
rate was 103–154 µs. The absolute worst did not tighten: 3 of 10 idle DPDK
reps had one round trip of 676–845 µs. No recommendation rests on it:
the bare-metal pair is outside the platforms run since 2026-10-16 (Graviton
spot and Azure), so it stands as a historical observation. On the VM pair the
gap split classifies spikes of that size as the process being descheduled
([data](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md#what-the-gap-split-says));
that class is not measured on metal.

## Kubernetes: EKS on `c7i.4xlarge`

DPDK at both ends, the media driver as a DaemonSet pod, round trips between
the two nodes' pods, 2026-10-14, run `eks-20261014-195824`
([data](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md#experiment-log)):
p50 28.37, p99.99 63.33, worst round trip 148 µs (the five per-rep maxima were
112–148, median 126), no NAKs or retransmits. Every rep's max was a stall of
the client's own loop, not the driver's. Setup: [Kubernetes](kubernetes.md).

## Platform limits

- **AWS caps one flow at 7.8–8.0 Gbit/s**, x86 and Graviton, kernel and DPDK
  alike. Measured rates: x86 (`c7i.4xlarge`, round 7) 15.17 M msgs/s at 32 B
  and 3.03 M at 288 B, 0.705 M at 1,344 B; Graviton 0.6855 M at 1,344 B. Flat-out
  at 32 B and 288 B on Graviton is not measured. An
  Aeron channel is one flow; spread a heavier stream over several channels
  ([data](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md#aws-c7i4xlarge-round-7-p50--p999--max-median-across-reps)).
- **Azure vCPUs stall once a minute**, at a second fixed per VM, for up to
  3.7 ms. Find them with a two-minute spin and keep busy threads off them
  ([data](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md#experiment-log)).
- **A lost frame on a quiet stream costs about 100 ms:** Aeron's C driver
  notices the gap only at the next heartbeat. One Azure round trip took
  134 ms this way. Give sparse, latency-critical streams a ~1 kHz keep-alive.
