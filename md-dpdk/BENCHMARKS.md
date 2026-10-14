**Note:** These benchmarks are environment-sensitive; rerun them on your own hardware. Everything below was measured on 2026-10-13 on two Azure `Standard_D8s_v6` VMs (Intel Xeon Platinum 8573C, 4 cores × 2 SMT threads, 32 GiB) in one proximity placement group in North Central US. Each VM had three accelerated NICs (the MANA NIC): one for management and two on a data subnet for the media driver's sender and receiver. They ran Debian 13 with Linux 7.2.6 (the `trixie-backports` cloud kernel), DPDK 24.11.4 and rdma-core 56.1 from Debian, rusteron-media-driver 0.2.11 (Aeron 1.53.3) and rustc 1.99.0. UDP is measured between the two VMs, never over loopback.

# Kernel UDP against DPDK on an Azure D8s_v6 pair (2026-10-13)

## Summary

- **Round trip, 32-byte messages:**
  - On the stock kernel, DPDK took 20.1 µs at p50 against 35.1 µs for kernel UDP, 23.0 against 46.9 µs at p99, and a max of 216 µs (69–663 across reps) against 336 µs (274–1,032).
  - Kernel socket busy polling (`busy_read`) closed most of the median gap: 21.5 µs, max 280 µs (233–295). DPDK stayed ahead at every percentile, and the ranges never overlap up to p99.9.
  - The tail is where DPDK pulls away. On isolated CPUs its p99.99 was 30.3 µs (28.4–31.5) and its max 71 µs (65–447), against 33.9 (32.4–34.6) and 82 µs (75–449) with busy polling, and 115 µs and 289 µs (244–2,740) for plain kernel UDP. The max is one round trip per rep, so its range is wide in every mode.
- **Throughput, 32-byte messages:** DPDK carried 19.8 M msgs/s, against 13.5 M for kernel UDP, 47% more. Busy polling cost kernel UDP some throughput: 13.1 M on the stock kernel, 12.4 M isolated.
  - DPDK delivered 19.83 M msgs/s in all ten reps, to four significant figures. That is not the VM's bandwidth limit: a later VM pair reached 22.8 M (see [Clocks](#clocks-tsc-against-clock_monotonic)).
- **CPU isolation** (`isolcpus`, `nohz_full`, `rcu_nocbs`) mattered for the tail and left the median alone. For DPDK it cut p99.9 from 29.5 to 23.3 µs, p99.99 from 69 to 30 µs and the max from 216 µs (69–663) to 71 µs (65–447). Plain kernel UDP did not change: its tail comes from interrupts, not from scheduling noise.
- **Clocks** (a later VM pair, [below](#clocks-tsc-against-clock_monotonic)): the transport's TSC clock reads in 10–12 ns against 19–21 ns for `CLOCK_MONOTONIC` and 21–24 ns for the millisecond clock it replaced, and takes 9–10 ns (16–17%) off each send and 4 ns (5%) off each poll. Round trips and kernel throughput did not change measurably with either clock; a round trip is 30,000 ns and swings by more than that between reps. Calibrated against `CLOCK_MONOTONIC_RAW`, the TSC tracks it to within 0.01 ppm.
- **Pin the driver's agents.** Aeron's C driver parses `AERON_SENDER_CPU_AFFINITY` and the like, but only Aeron's own `aeronmd` binary applies them. rusteron's driver did not, so the agents floated. Under DPDK they also inherited the CPU the EAL pins its main thread to, which put the sender and receiver on one core and made a round trip take 2.8 ms. `ergon-md-dpdk` now pins them (`pin_agents_to_configured_cpus`), in every transport mode. Every number here was measured with the fix.

## Method

- **Machines.** Both VMs are created and deleted by `md-dpdk/benches/cloud/azure/lab.sh`; each VM's side is `md-dpdk/benches/cloud/azure/vm.sh`. Both hosts go through every state together.
  - The first VM (a) pings and publishes, the second (b) pongs and subscribes.
  - Each VM has a management NIC on 10.20.0.0/24 (SSH only from the operator's address) and two data NICs on 10.20.1.0/24: the driver's sender port and receiver port. All three are vports of one MANA PCI function, told apart by MAC.
  - SMT twins are adjacent: CPUs 0–1, 2–3, 4–5, 6–7.
- **CPU layout.**
  - CPU 0 does housekeeping alone: every IRQ (irqbalance stopped), the driver's conductor, the clients' conductor threads (each client is started with `taskset -c 0`) and, under DPDK, the EAL's only lcore (`RUSTERON_DPDK_EAL_ARGS='-l 0'`).
  - The app thread (ping, pong, publisher or subscriber) on CPU 2, the driver's sender on 4 and its receiver on 6, each pinned once connected.
- **Transport modes**, on the same data NICs:

| Mode | What runs |
| --- | --- |
| kernel UDP | rusteron's socket transport, the kernel's MANA driver |
| kernel UDP, busy polling | the same, with `net.core.busy_read=50` and `busy_poll=50`, and `napi_defer_hard_irqs=2` and `gro_flush_timeout=200000` on the data VFs, on both hosts |
| DPDK (netvsc + MANA) | `ergon-md-dpdk` on DPDK's netvsc PMD over the MANA VF: each data NIC's synthetic VMBus device moved to `uio_hv_generic`, its kernel interfaces down, 256 RX and TX descriptors |

- **Kernel states**, applied one after the other to both VMs:

| State | What it adds |
| --- | --- |
| pinned | the stock kernel, with the CPU layout above |
| isolated | `isolcpus=nohz,domain,managed_irq,1-7 nohz_full=1-7 rcu_nocbs=1-7 irqaffinity=0` on the kernel command line |

- **Aeron settings throughout.**
  - Dedicated threading; sender and receiver with `noop` idle; the conductor with the default backoff.
  - Non-sparse term buffers (`AERON_TERM_BUFFER_SPARSE_FILE=false`) and pre-touched client mappings (`AERON_CLIENT_PRE_TOUCH_MAPPED_MEMORY=true`).
  - `AERON_DIR` on 2 MiB `hugetlbfs` (`AERON_FILE_PAGE_SIZE=2097152`).
  - MTU 1408; `AERON_SOCKET_SO_SNDBUF`, `AERON_SOCKET_SO_RCVBUF` and `AERON_RCV_INITIAL_WINDOW_LENGTH` all 2 MiB, with `net.core.rmem_max` and `wmem_max` at 16 MiB.
  - Exclusive publications; pong echoes with `try_claim`.
  - Two data NICs on one subnet: each NIC's source address selects its own routing table, so the kernel never sends from one NIC's address through the other (Azure drops those).
- **Workloads.** The clients are rusteron's x86-lab `rtt` and `tput`, ported to `md-dpdk/harness` and run against the bundled `media_driver`, so kernel and DPDK use identical client code. These runs stamped round trips with `Aeron::nano_clock` (`CLOCK_MONOTONIC` through the vDSO). The clients now stamp with `rdtscp`, calibrated against `CLOCK_MONOTONIC_RAW`, and record each run's full histogram; the clock comparison below shows the round trips are the same either way.
  - Round trip: 32-byte messages, one in flight, 300,000 round trips after 50,000 of warm-up, stopping after 20 s.
  - Throughput: 32-byte messages offered flat out. The figure is the median of five one-second samples after a one-second warm-up.
  - Each state runs 5 reps, with the modes in a different random order each rep.
- **Provenance.** ergon `db3f9129` (`feat/0.1.30`) with the uncommitted `ergon-md-dpdk` working tree. The pinned state comes from run `20261013-100610` and the isolated state from run `20261013-102659`. A first combined run failed in its second state on a lab-script bug (re-adding a routing rule); its pinned state had completed and is used as is.
- **Reading the tables.**
  - Each cell is the median across reps, with the range across reps in brackets.
  - A difference counts only when the ranges do not overlap.
  - Max is the slowest single round trip of a rep, one sample, so it varies most.
- **Validity checks.**
  - Azure can move a VM's traffic from the NIC's VF to its software (synthetic) path. Each run recorded each host's count of "data path switched" kernel messages before and after: none of the 52 host-runs checked saw a switch.
  - Kernel runs recorded netvsc's VF and synthetic-path packet counters: no packet took the synthetic path.
  - DPDK runs recorded the netvsc PMD's statistics, mirrored as Aeron counters: in every checked host-run, the VF's packet counts equalled the totals, so all traffic used the VF.
  - The isolated state's kernel command line and `/sys/devices/system/cpu/isolated` (`1-7`) were recorded on both hosts. The clock source was `tsc`.

## UDP round trip between the two hosts (µs)

| state, transport | reps | p50 | p90 | p99 | p99.9 | p99.99 | max |
| --- | --- | --- | --- | --- | --- | --- | --- |
| pinned, kernel UDP | 5 | 35.10 (31.65–35.23) | 43.52 (42.88–43.71) | 46.94 (46.46–49.66) | 60.90 (59.58–63.71) | 109.12 (105.28–128.13) | 336.13 (273.66–1032.19) |
| pinned, kernel UDP, busy polling | 5 | 21.54 (21.45–21.63) | 22.48 (22.45–22.64) | 27.05 (24.45–28.53) | 35.90 (31.23–36.51) | 96.32 (91.01–103.10) | 279.81 (232.83–295.42) |
| pinned, DPDK (netvsc + MANA) | 5 | 20.13 (20.09–20.19) | 20.51 (20.50–20.59) | 22.98 (22.27–23.77) | 29.47 (29.07–29.76) | 68.73 (33.41–84.09) | 215.94 (69.38–663.04) |
| isolated, kernel UDP | 5 | 35.26 (32.13–35.36) | 43.65 (43.13–43.65) | 46.05 (46.05–46.72) | 57.28 (50.59–57.47) | 115.20 (98.50–127.94) | 289.28 (243.84–2740.22) |
| isolated, kernel UDP, busy polling | 5 | 21.57 (21.49–22.03) | 22.54 (22.41–23.20) | 23.82 (23.41–25.17) | 27.02 (26.62–27.74) | 33.92 (32.35–34.59) | 82.43 (74.69–448.77) |
| isolated, DPDK (netvsc + MANA) | 5 | 20.11 (20.02–20.13) | 20.48 (20.35–20.50) | 21.33 (21.10–21.34) | 23.25 (23.17–23.44) | 30.29 (28.35–31.50) | 71.10 (64.93–447.49) |

Every rep completed its 300,000 round trips.

- **DPDK against kernel UDP:** 15 µs off the median (35.1 → 20.1), and at most two-thirds of kernel UDP's latency at every percentile above it, in both states: half at p99 and p99.9, and down to a quarter at p99.99 when isolated (30.3 against 115.2). The max: 216 against 336 µs on the stock kernel, 71 against 289 µs isolated, every range overlapping.
- **DPDK against busy polling:** 1.4 µs off the median, and ahead at every percentile up to p99.9 in both states, the ranges apart. At p99.99 it won in the isolated state (30.3 against 33.9, ranges apart); the max ranges overlap in both states (isolated 71 µs, 65–447, against 82 µs, 75–449). On the stock kernel the p99.99 and max ranges overlap.
- **Isolation:** DPDK's and busy polling's p99.9, p99.99 and median max fell (DPDK 216 → 71 µs, busy polling 280 → 82 µs); plain kernel UDP's did not (max 336 → 289 µs, with one 2.7 ms rep).

## UDP throughput, 32-byte messages (M msgs/s)

| state, transport | reps | msgs/s |
| --- | --- | --- |
| pinned, kernel UDP | 5 | 13.45 (13.15–13.67) |
| pinned, kernel UDP, busy polling | 5 | 13.13 (12.96–13.18) |
| pinned, DPDK (netvsc + MANA) | 5 | 19.83 (19.83–20.05) |
| isolated, kernel UDP | 5 | 13.57 (13.48–13.72) |
| isolated, kernel UDP, busy polling | 5 | 12.39 (12.38–12.42) |
| isolated, DPDK (netvsc + MANA) | 5 | 19.83 (19.83–19.83) |

- DPDK carried 47% more than kernel UDP. Its figure barely moved across ten reps and two states (about 22 messages per 1408-byte frame, so about 10.6 Gbit/s on the wire). It is not the VM's bandwidth limit: on a later VM pair, DPDK reached 22.8 M msgs/s in its good reps.

## Clocks: TSC against CLOCK_MONOTONIC

Measured on 2026-10-13 on a second pair of the same VMs, after the transport and clients moved from `CLOCK_MONOTONIC` to the TSC. Each transport mode ran twice: as is (TSC), and with `RUSTERON_DPDK_CLOCK=monotonic` and `BENCH_CLOCK=monotonic` (the `-mono` modes), which put the transport and clients back on `CLOCK_MONOTONIC`. Both VMs report an invariant TSC (`constant_tsc`, `nonstop_tsc`) at 2.3 GHz, and the transport's clock counter (type 80) read 1 in the TSC runs. Run `20261013-161020`; calibration rechecked in run `20261013-173144`.

### What one clock read costs

Criterion (`cargo bench -p ergon-md-dpdk --features static,dpdk --bench clock`, pinned to the app CPU), on the two VMs:

| clock read | VM a | VM b |
| --- | --- | --- |
| millisecond `CLOCK_MONOTONIC` (the transport before) | 23.6 ns | 21.1 ns |
| `CLOCK_MONOTONIC` in ns (`RUSTERON_DPDK_CLOCK=monotonic`) | 20.5 ns | 18.4 ns |
| **TSC in ns (the transport now)** | **11.7 ns** | **10.3 ns** |
| `std::time::Instant` | 20.5 ns | 18.3 ns |
| `minstant::Instant::now` (raw cycles, converted later) | 9.7 ns | 8.7 ns |

The transport's own paths, one 32-byte datagram through the genuine send and poll callbacks against the fake ports (so the transport's cost, not a wire latency):

| path | `CLOCK_MONOTONIC` | TSC | saved |
| --- | --- | --- | --- |
| send, VM a | 60.3 ns | 50.3 ns | 10.0 ns (17%) |
| send, VM b | 54.1 ns | 45.3 ns | 8.8 ns (16%) |
| poll, VM a | 89.4 ns | 84.9 ns | 4.5 ns (5%) |
| poll, VM b | 79.8 ns | 75.8 ns | 4.0 ns (5%) |

Criterion's confidence intervals were within ±0.3 ns, and the isolated state gave the same figures. `clockcheck` (`md-dpdk/harness`) measured the same from the clients' side: `rdtsc` 8–9.5 ns, `rdtscp` 14–16 ns, the clients' scaled `rdtscp` clock 16.5–19 ns, `clock_gettime(CLOCK_MONOTONIC)` 20.5–23.4 ns.

### Round trip, TSC against CLOCK_MONOTONIC (µs)

| state, transport | reps | p50 | p90 | p99 | p99.9 | p99.99 | max |
| --- | --- | --- | --- | --- | --- | --- | --- |
| pinned, kernel UDP | 5 | 38.98 (38.88–39.30) | 44.48 (43.77–44.80) | 52.54 (51.49–52.64) | 64.09 (60.83–64.48) | 146.94 (144.25–156.29) | 1328.13 (409.86–1671.17) |
| pinned, kernel UDP, monotonic clock | 5 | 38.94 (38.85–39.26) | 43.77 (43.55–45.44) | 51.49 (51.20–53.85) | 61.22 (59.10–64.77) | 153.85 (139.78–157.31) | 289.28 (253.18–2246.66) |
| pinned, kernel UDP, busy polling | 5 | 32.06 (31.93–32.09) | 33.76 (33.66–34.05) | 39.90 (38.81–40.67) | 47.71 (46.43–56.06) | 140.67 (56.93–147.07) | 414.72 (125.69–2963.45) |
| pinned, kernel UDP, busy polling, monotonic clock | 5 | 32.08 (32.00–32.14) | 33.79 (33.73–33.85) | 39.74 (38.98–39.97) | 47.23 (45.66–48.19) | 144.64 (59.58–156.29) | 422.40 (122.81–1437.69) |
| pinned, DPDK | 5 | 29.98 (29.97–30.02) | 31.28 (31.25–31.41) | 35.55 (35.01–36.32) | 40.00 (39.74–40.61) | 52.64 (48.26–141.57) | 634.88 (137.60–665.60) |
| pinned, DPDK, monotonic clock | 5 | 30.03 (30.02–30.11) | 31.34 (31.31–31.42) | 36.00 (35.55–36.03) | 40.58 (40.09–40.93) | 135.94 (46.24–147.46) | 459.77 (356.86–878.59) |
| isolated, kernel UDP | 5 | 38.85 (38.72–38.88) | 43.49 (42.98–43.52) | 50.75 (50.11–52.26) | 57.31 (55.97–82.11) | 133.25 (121.15–143.74) | 730.11 (331.77–15499.26) |
| isolated, kernel UDP, monotonic clock | 5 | 38.98 (37.82–39.13) | 43.42 (40.67–44.83) | 50.62 (44.45–52.06) | 56.45 (50.98–62.30) | 133.12 (118.46–146.43) | 762.37 (355.07–1461.25) |
| isolated, kernel UDP, busy polling | 5 | 32.05 (31.97–32.11) | 33.57 (33.47–33.70) | 36.93 (36.48–37.41) | 41.18 (41.09–41.76) | 46.56 (45.79–47.30) | 126.08 (82.81–142.85) |
| isolated, kernel UDP, busy polling, monotonic clock | 5 | 32.05 (32.00–32.16) | 33.57 (33.50–33.76) | 37.34 (36.51–37.50) | 41.66 (40.77–41.89) | 47.45 (46.81–54.94) | 132.74 (71.87–2566.14) |
| isolated, DPDK | 5 | 29.98 (29.97–30.03) | 31.18 (31.09–31.28) | 34.37 (33.47–35.23) | 38.66 (37.25–40.13) | 42.78 (41.79–51.39) | 438.78 (86.91–868.86) |
| isolated, DPDK, monotonic clock | 5 | 30.03 (30.00–30.09) | 31.30 (31.23–31.36) | 34.75 (33.66–35.74) | 38.91 (37.57–47.42) | 42.88 (41.79–60.86) | 78.40 (64.25–1457.15) |

- **No measurable difference.** Every TSC/monotonic pair's ranges overlap at every percentile in both states. DPDK's p50 was 0.05 µs lower with the TSC in both states, the ranges just apart (the max ranges overlap: 635 µs, 138–666, against 460 µs, 357–879, pinned), which matches the 10–14 ns the two clocks save per round trip (four transport clock reads, two client stamps); everywhere else the noise is larger than the clock.
- **This pair was slower than the first** in every mode, kernel UDP included, which never runs the transport: DPDK 30.0 against 20.1 µs at p50 (max 635 against 216 µs pinned), busy polling 32.1 against 21.5 (max 415 against 280 µs). The software, kernel and CPU model were the same, so it was the placement. Compare modes within one pair, not across pairs.

Kernel throughput did not change with the clock either (pinned 12.82 M against 12.83 M, busy polling 11.44 against 11.33; isolated 12.69 against 12.81, and 10.83 against 10.82, all ranges overlapping).

### DPDK throughput on this pair: frame loss, not the clocks

DPDK throughput on this pair fell to 0.5–0.6 M msgs/s, some reps carrying nothing, with either clock. An A/B on a third VM pair ran this morning's build (before the clock work) and today's build alternately, three reps each, with every transport and NIC counter read before and after:

| build, rep | frames sent by a | received by b | lost | msgs/s |
| --- | --- | --- | --- | --- |
| today, 1 | 5,541,752 | 5,541,024 | 728 (0.013%) | 22.79 M |
| morning, 1 | 5,980,294 | 5,979,915 | 379 (0.006%) | 22.77 M |
| today, 2 | 5,167,968 | 5,166,079 | 1,889 (0.037%) | 19.80 M |
| morning, 2 | 682,821 | 676,323 | 6,498 (0.95%) | 2.42 M |
| today, 3 | 2,569,338 | 2,564,594 | 4,744 (0.18%) | 11.69 M |
| morning, 3 | 5,984,546 | 5,984,367 | 179 (0.003%) | 22.78 M |

(Frames as counted by the transport: a's sender port transmitted, b's receiver port dispatched.)

- Every rep of both builds lost frames. Up to about 0.01% cost nothing; beyond that throughput fell, to 2.4 M msgs/s at 0.95%. Both builds reached 22.8 M msgs/s in their good reps. The clock work did not cause it.
- The frames went missing between VM a's NIC and VM b's transport, and neither side counted them: b's NIC counted as good exactly what its transport dispatched. Aeron then throttles on NAKs and retransmits, which is the collapse.
- The likely place is b's receive ring, which MANA caps at 256 descriptors (about 250 µs at 1 M frames/s), overflowing whenever the receiver agent stalls; the alternative is the Azure fabric. Not yet confirmed.
- Kernel UDP over the same VF NICs lost nothing measurable, and the first pair never showed it. Until it is resolved, DPDK throughput on Azure varies by placement.

### Accuracy

`clockcheck` read the clients' calibrated TSC, `minstant` and `CLOCK_MONOTONIC_RAW` against `CLOCK_MONOTONIC` for 120 s, on the app CPU:

| calibration | state | TSC against `CLOCK_MONOTONIC` | TSC against `CLOCK_MONOTONIC_RAW` | `minstant` against `CLOCK_MONOTONIC` |
| --- | --- | --- | --- | --- |
| first: 50 ms against `CLOCK_MONOTONIC`, one read at each end | after boot | −0.4 to −3.6 ppm | not measured | +0.0 to −5.6 ppm |
| | isolated | −115 to −124 ppm | not measured | −0.5 to +1.4 ppm |
| now: 100 ms against `CLOCK_MONOTONIC_RAW`, tightest of 16 reads | after boot | +43 to +48 ppm | −0.02 to +0.003 ppm (after 10 s) | −0.5 to +5.4 ppm |
| | isolated | +42 to +46 ppm | −0.003 to +0.011 ppm (after 10 s) | −2.2 to +2.3 ppm |

- `CLOCK_MONOTONIC` ran 42–48 ppm slower than `CLOCK_MONOTONIC_RAW` on every VM: chrony's frequency correction for this host's TSC rate, which the kernel estimated that far off. The first calibration measured against a clock that chrony was correcting, inside a 50 ms window, and caught whatever the correction was then: anything from −0.4 to −124 ppm, run to run.
- The current calibration is deterministic: the TSC matches `CLOCK_MONOTONIC_RAW` to 0.01 ppm. Its absolute rate carries the kernel's 44 ppm estimate error, which is 1.3 ns on a 30 µs round trip and 44 µs a second. The transport uses the same calibration (50 ms); its `CLOCK_MONOTONIC`-anchored nanoseconds feed only ARP and statistics timers.
- `minstant` calibrates against `CLOCK_MONOTONIC`, so it follows chrony's correction (±5 ppm here) instead. Its `now()` is cheaper because it returns raw cycles; a nanosecond reading (`elapsed()`) took 18–20.5 ns, against 16.5–19 ns for the clients' clock. It brings no advantage here, and the C transport cannot use it.

## How to run

```sh
az login
md-dpdk/benches/cloud/azure/lab.sh                          # both states, 5 reps of every mode, about 45 minutes
LAB_STATES=isolated md-dpdk/benches/cloud/azure/lab.sh      # one state
LAB_KEEP=1 LAB_REPS=0 md-dpdk/benches/cloud/azure/lab.sh    # build the VMs and keep them, for work by hand
LAB_MODES="kernel kernel-mono kernel-busy kernel-busy-mono dpdk dpdk-mono" md-dpdk/benches/cloud/azure/lab.sh   # the clock comparison
```

Every run also records `clockcheck` and the Criterion clock bench on both VMs, after boot and after isolating. VM a runs each state's reps itself over the private network (`benches/cloud/remote.sh`), so a dropped connection on the operator's side does not stop a run; `LAB_SSH_FROM` takes every public address the operator's connection may use.

`lab.sh` refuses to start if its resource group exists and deletes the group on any exit, success or failure. With `LAB_KEEP=1`, delete it yourself (`az group delete -n ergon-lab-md-dpdk --yes`). Results land in `target/md-dpdk-bench/<stamp>/`, and `analyse.py <results.csv>` prints the tables.

## What got DPDK working on Azure

These are recorded so a rerun does not have to rediscover them:

- **Use the netvsc PMD, not MANA alone.** With the kernel interfaces down, Azure moves the NIC's traffic to the synthetic path, and DPDK's MANA PMD on its own received nothing. Moving each data NIC's VMBus device to `uio_hv_generic` lets DPDK's netvsc PMD take it: it attaches the MANA VF and keeps the data path on it.
- **Select ports by MAC.** Every accelerated NIC of the VM is a vport on one MANA PCI function (`RUSTERON_DPDK_SENDER_MAC`, `RUSTERON_DPDK_RECEIVER_MAC`).
- **Stop the automatic fail-safe device.** DPDK probes `vdev_netvsc` on Hyper-V by itself, and its fail-safe device took ownership of the MANA ports; the transport passes `--vdev=net_vdev_netvsc0,ignore=1` whenever ports are selected by MAC.
- **256 descriptors.** MANA queues hold at most 256.
- **Pin the agents** (see the summary): without it, DPDK's round trip was 2.8 ms.

## Experiment log

One change per experiment, A/B against the current best in random order within each rep, 5 reps; kept only if the medians win, the tail does not widen, and a second run reproduces it. Loss counters were read before and after every run on both hosts.

| date | change | result | decision |
| --- | --- | --- | --- |
| 2026-10-13 | DPDK against kernel UDP and busy polling (D8s_v6 pair, runs `20261013-100610`, `-102659`) | DPDK fastest at every percentile; see the tables above | baseline |
| 2026-10-13 | TSC clock in the transport and clients, against `CLOCK_MONOTONIC` (run `20261013-161020`) | 9–10 ns off each send, 4 ns off each poll; round trips unchanged within noise at every percentile and the max (isolated DPDK max 439 µs, 87–869, against 78 µs, 64–1,457) | kept: cheaper and calibrated to 0.01 ppm |
| 2026-10-14 | host-local channels read every cycle for 1 ms after data (`RUSTERON_DPDK_LOCAL_IO=adaptive`) against the epoll wake-up (run `20261014-003737`, D16s_v6 + D8ds_v6) | localhost round trip p50 14.1 → 6.4 µs and max 260 (203–1,117) → 61 µs (50–116) pinned; p50 13.6 → 6.3 µs, p99.99 73.7 → 11.1 µs and max 1,177 (289–2,271) → 57 µs (54–71) isolated; cross-host with an idle localhost subscription unchanged | kept, now the default |
| 2026-10-14 | re-offer a refused transmit tail for up to 50 µs (`RUSTERON_DPDK_TX_RETRY_US=50`), same run | round trips unchanged (every range overlaps, both states; max 541 µs, 242–939, against 596 µs, 86–758, isolated; 590 µs, 105–2,511, against 465 µs, 108–2,263, pinned); flat-out short sends down 99% (31 M → 0.26 M) and NAKs 8 → 0 pinned; 1,344-byte flat-out throughput still collapsed | open: retested once the bandwidth allowance keeps the ring from filling |
| 2026-10-14 | (finding) 1,344-byte frames flat out over DPDK, same run | collapsed to 0.03 M msgs/s in 4 of 5 reps per state: about 11 Gbit/s exceeded the VMs' allowance, frames were dropped unseen and Aeron fell into NAKs and retransmits | led to the bandwidth allowance (`RUSTERON_DPDK_TX_RATE_MBPS`) and stream priority (`RUSTERON_DPDK_TX_LOW_STREAMS`), under test |
| 2026-10-14 | (measurement bug) per-run DPDK loss counters | a port's counters were freed when its last channel closed, so a reading taken between runs saw them absent (0) and per-run deltas swung by ± the whole total; Aeron's own counters (NAKs, retransmits) were unaffected | fixed: counters stay registered across channels; sweep 2's and round 3's per-point DPDK loss columns are void |
| 2026-10-14 | (finding) where the agents are preempted (round 3, kernel tracing on the agent CPUs, a TSC-gap spinner on a spare CPU) | the EAL's interrupt and multi-process threads and Aeron's `aeron-md-nra` were allowed on the agents' CPUs (now confined to the housekeeping CPU); the agents' only remaining guest preemptions are 8–22 µs kworker LRU drains from khugepaged every 10.24 s; VM b also stalled for 0.3–0.76 ms at second :57 of every minute with nothing in the guest (the host); kernel busy polling spiked to 1.48 ms too, so the millisecond spikes are not DPDK's alone | THP off goes to round 4; spikes now carry the wall clock, matched against the spinner and trace |
| 2026-10-14 | round 3, isolated, run `20261014-095043`, stopped after rep 2 (its build had the counter bug) | 8 Gbit/s cap: 288 B sustained 4.58 → 3.03 M msgs/s, 1,344 B 1.04 → 0.71 M (the cap binds by design), no tail gain; TX_LOW_STREAMS and PACE_SPIN_US=0 under it: no change at one rep; 128 KiB receiver window: 288 B 4.58 → 0.39 M; sender io vectors and messages per send 16: 1,344 B collapsed to 0.03 M; receiver io vectors 16: round trip max 250 against 1,260 µs at one rep, throughput unchanged | cap kept off by default (it stays for slow links); window and sender io vectors rejected; receiver io vectors to round 4 |
| 2026-10-14 | (finding) 1,344 B collapse, round 3 rep 1 | uncapped DPDK ran clean at 1 M msgs/s (11.3 Gbit/s) yet collapsed at 800 k: not the VM's bandwidth allowance but stochastic, consistent with a receiver stall overflowing MANA's 256-descriptor receive ring at about 1 M frames/s | the bandwidth allowance is not the fix; receive headroom (io vectors) and the stalls are |
| 2026-10-14 | localhost throughput gap, `perf` on the pinned VM (run `20261014-093330`) | 1,344 B localhost: kernel 526 k, DPDK 324 k msgs/s; 13% of all samples in the sender's `sock_def_readable`: every datagram to a host-local socket woke the local-I/O thread (124 k switches in 3 s) | fixed: a socket comes off that thread's epoll while its agent reads it every cycle (`RUSTERON_DPDK_LOCAL_UNWATCH`, A/B in round 4) |
