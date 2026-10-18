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

**In short:** Graviton5 (`c9g.2xlarge`) had the lowest latency measured, at
half the hourly price of the x86 pair with as many physical cores. Its median
lead (7 µs) is no more than placement alone moved an x86 pair, so it is not
proven; its tail lead under load is clearer. Only x86 bare metal matched that
tail, at 12 times the price.

**Method.** Aeron ping-pong between two hosts in one cluster placement group,
us-east-1, Debian 13, Linux 7.2.6, Aeron 1.53.3, isolated CPUs, 5 reps. Idle:
32 B, one in flight, 300,000 round trips a rep. Under load: 288 B at 1 M
msgs/s, latency from each message's intended send time. Each cell is the
median of the reps, in µs; max is the slowest single round trip.

## Latency

"Kernel" is the best kernel path measured on that platform: busy polling
with a 4,096-entry ring on x86; interrupt moderation off with the NIC's
interrupts on a core of their own on Graviton (busy polling collapsed there
at 1 M msgs/s).

| instance | idle, kernel: p50 / p99.99 / max | idle, DPDK: p50 / p99.99 / max | 1 M msgs/s, kernel: p50 / max (lossy reps) | 1 M msgs/s, DPDK: p50 / max (lossy reps) |
|---|---|---|---|---|
| `c7i.4xlarge`, x86 VM | 28.22 / 56.48 / 1,938 | 21.28 / 43.71 / 109 | 43.74 / 599 (1 of 5) | 30.38 / 152 (0 of 5) |
| `c7i.metal-24xl`, x86 metal | 33.70 / 44.54 / 67 | 28.13 / 38.72 / 71 | 42.94 / 592 (0 of 5) | 34.30 / 82 (0 of 5) |
| `c9g.2xlarge`, Graviton5 VM | 18.54 / 38.14 / 93 | 13.69 / 28.67 / 75 | 44.9 / 122 (0 of 5) | 19.0 / 107 (0 of 5) |

Runs: `c7i.4xlarge` round 7, 2026-10-15, `aws-20261015-185813`
([data](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md#aws-c7i4xlarge-rounds-6-and-7-2026-10-15));
`c7i.metal-24xl` round 4, 2026-10-15, `aws-20261015-012400`
([data](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md#aws-c7imetal-24xl-round-4-no-hypervisor-2026-10-15));
`c9g.2xlarge` round G1, 2026-10-16, `aws-20261016-072005`, spot, built with
`-mcpu=native`
([data](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md#aws-graviton5-c9g2xlarge-round-g1-2026-10-16)).

- **Graviton5 against the x86 VM:** idle median 13.69 against 21.28 µs on
  DPDK, 18.54 against 28.22 on the kernel path. At 1 M msgs/s DPDK 19.0
  against 30.38; the kernel paths were level (43–45 µs).
- **Tail under load (DPDK, 288 B):** at 100 k msgs/s the max was 74–84 µs on
  all three Graviton pairs against 117 (109–123) on the x86 VM. At 1 M msgs/s
  it was lower on two of three pairs (G2 98, G3 106); G1's 107 (92–166)
  overlaps the x86 VM's 152 (139–155). Metal: 82 (77–103).
- **Bare metal:** the lowest idle kernel max (67 µs) but the highest medians.
  Its kernel rows predate the interrupt re-pinning fix, so they are
  pessimistic.
- **Treat the gap with care.** Each row is a different pair and round.
  Placement alone moved an x86 median by 7 µs, and an earlier `c9g.xlarge`
  pair's DPDK median was 19.9 µs, not 13.7. x86 had more tuning rounds.
- **Two more Graviton pairs reproduced the medians:** DPDK 13.78 and 13.97,
  kernel 18.83 and 18.93 µs (rounds
  [G2](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md#aws-graviton5-c9g2xlarge-round-g2-2026-10-16),
  `aws-20261016-135004`, and
  [G3](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md#aws-graviton5-c9g2xlarge-round-g3-2026-10-16),
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
- Azure, third-party East US list prices (the lab ran in North Central US):
  `D8s_v6` pair about $0.81, `D16s_v6` + `D8ds_v6` about $1.30.

## Bare metal

`c7i.metal-24xl`, round 4: no hypervisor, so the tails tighten. DPDK's idle
p99.99 was 38.72 µs against 56.06 on a `c7i.4xlarge` (round 3), and it lost
nothing up to 1.25 M msgs/s of 288 B with every max under 140 µs. Still, 3 of
10 DPDK reps had one round trip of 676–845 µs, not yet explained.

## Kubernetes: EKS on `c7i.4xlarge`

DPDK at both ends, the media driver as a DaemonSet pod, round trips between
the two nodes' pods, 2026-10-14, run `eks-20261014-195824`
([data](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md#eks-dpdk-pods-on-c7i4xlarge-2026-10-14)):
p50 28.37, p99.99 63.33, max 126 µs (112–148 across reps), no NAKs or
retransmits. Every rep's max was a stall of the client's own loop, not the
driver's. Setup: [Kubernetes](kubernetes.md).

## Platform limits

- **AWS caps one flow at 7.8–8.0 Gbit/s**, x86 and Graviton, kernel and DPDK
  alike: 15.17 M msgs/s at 32 B, 3.03 M at 288 B, 0.69–0.705 M at 1,344 B. An
  Aeron channel is one flow; spread a heavier stream over several channels
  ([data](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md#aws-one-flow-carries-about-8-gbits)).
- **Azure vCPUs stall once a minute**, at a second fixed per VM, for up to
  3.7 ms. Find them with a two-minute spin and keep busy threads off them
  ([data](https://github.com/mimran1980/ergon/blob/main/md-dpdk/BENCHMARKS.md#the-once-a-minute-stall)).
- **A lost frame on a quiet stream costs about 100 ms:** Aeron's C driver
  notices the gap only at the next heartbeat. One Azure round trip took
  134 ms this way. Give sparse, latency-critical streams a ~1 kHz keep-alive.
