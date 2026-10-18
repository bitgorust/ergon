**Note:** Environment-sensitive; rerun on your own hardware. Azure 2026-10-13 to 10-15, AWS x86 10-14 to 10-16, AWS Graviton 10-16. UDP between two hosts, never loopback, except rows marked localhost.

# ergon-md-dpdk: kernel UDP against DPDK on Azure and AWS

µs: median of 5 reps, range across reps in brackets. Max is each rep's slowest round trip (one sample). Load figures are open loop, from each message's intended send time. Every percentile keeps its max.

## Conclusions

- **In production, run kernel UDP, tuned for the platform** ([recommended configuration](#recommended-configuration)). `ergon-md-dpdk` is a proof of concept, not for production ([README](README.md)). The DPDK rows show what kernel bypass is worth.
- **Kernel bypass pays off most under load.** c9g.2xlarge, round G3, DPDK against the best kernel configuration (p50 / p99.99 / max, max's range in brackets):

  | point | DPDK | kernel UDP |
  | --- | --- | --- |
  | idle, 32 B | 13.97 / 33.66 / 75 (73–79) | 18.93 / 44.09 / 79 (76–168) |
  | 288 B at 100 k/s | 14.34 / 41.34 / 84 (83–101) | 19.55 / 91.71 / 177 (116–193) |
  | 288 B at 1 M/s | 19.36 / 62.11 / 106 (93–114) | 44.83 / 176.00 / 204 (122–211) |
  | 1,344 B at 500 k/s | 18.62 / 59.30 / 104 (95–111) | 31.38 / 131.71 / 188 (112–742) |
  | 1,344 B, bursts of 1,000 at 500 k/s mean | 245.89 / 468.99 / 501 (491–1,819) | 606.72 / 1,131.52 / 1,159 (1,103–1,176) |

  Idle: DPDK saves 5 µs at the median; max about the same. From 100 k/s: DPDK's p99.99 and max are 0.35–0.55 of the kernel's. From 500 k/s: its median is 0.4–0.6 as well.
- **Graviton5 (c9g.2xlarge) had the lowest latency measured,** on three pairs (G1–G3):
  - DPDK idle p50 13.69–13.97, max 66–75; 288 B at 1 M/s p50 19.0–19.4, max 98–107.
  - Best x86 VM, c7i.4xlarge (round 7): idle p50 21.28, max 109 (52–116); 288 B at 1 M/s p50 30.38, max 152 (139–155).
  - The median lead (7 µs) is no more than placement alone moved c7i.4xlarge between rounds 6 and 7. Not proven.
  - The tail lead is clearer. At 100 k/s the max was lower on all three pairs (74–84 against 117, 109–123). At 1 M/s lower on two of three (G2 98, 95–105; G3 106, 93–114); G1's (107, 92–166) overlaps.
  - Only x86 bare metal matched that tail (288 B at 1 M/s max 82, 77–103), at 12 times the price.
  - A c9g.2xlarge pair costs $0.696 an hour on demand ($0.306 spot): the 8 cores of a c7i.4xlarge pair ($1.428) at half the price.
- **Settings that matter most:**
  - Kernel UDP on AWS: ENA's interrupt moderation off. On by default since Linux 6.9; added about 60 µs to the median at 100 k/s on c7i.2xlarge (p50 106 against 47, max 606 against 538; 3 reps).
  - With moderation off, give the data NICs' interrupts a core of their own. On the housekeeping core they fall behind: c7i.2xlarge p99.9 12 ms at 500 k/s; c9g lost at 1 M/s in 5 of 5 reps (max 10.8 ms). Tested on Graviton only.
  - Every platform: isolated CPUs with pinned agents (Azure DPDK max 216 → 71 µs), transparent hugepages off (Azure idle max 546 → 106 µs), busy threads only on cores the pre-flight finds quiet.
  - Azure DPDK: receiver IO vectors 16, and a receiver window that MANA's 256-descriptor ring can hold.
- **Zero loss comes first.** One lost frame on a sparse stream costs about 100 ms, Aeron's heartbeat (one lost ping cost 134 ms). Send a keep-alive at about 1 kHz on sparse, latency-critical streams.
- **One AWS flow carries at most 7.8–8.0 Gbit/s,** on x86 and Graviton, kernel or DPDK. A channel is one flow: spread a heavier stream over several channels.
- **What did not help:**
  - Graviton: sender and receiver on one thread (1,344 B bursts p50 522 against 234, max 748 against 493), the conductor on its own core, a mempool cache of 512, sender IO vectors 2, jumbo frames (for latency), transmit moderation kept at 64 µs, kernel busy polling (288 B at 1 M/s: p50 4,973 µs, max 9,175 (8,544–102,826), 61,488 NAKs).
  - x86 and Azure: `mlockall`, no NIC statistics reads, C-states capped at C1 on metal, a 1 MiB receive window, an RX burst of 64, re-offering refused transmits, pacing to 7.9 Gbit/s.
- **CPU isolation is the biggest single win on Azure.** Round 11 (2026-10-18), same binary, same pair, `pinned` against `isolated`: DPDK idle max 91.84 (58.02–102.02) → **57.89 (49.47–64.22)**, p99.99 34.40 → 26.35. Round trips of 200 µs or more: **0** in the whole isolated-DPDK idle run, against 39 in isolated kernel UDP. Of 100–250 µs: 0 in 4 of 5 isolated-DPDK reps, against 10–40 in every kernel-UDP rep. The full table is under [round 11](#azure-round-11-2026-10-18-d8s_v6-pair--idle-round-trip-p50--p90--p99--p999--p9999--max).
- **Media receive timestamps cost nothing measurable** on Graviton DPDK (G3): p50 within 0.07 µs at idle and every steady rate; idle max 74 (62–153) against 75 (73–79). Repeat once before making them a default: one rep a point reached 152–183 µs.

## Recommended configuration

> **Not for production.** `ergon-md-dpdk` is a proof of concept ([README](README.md)). Its rows record the lab's best settings for it.

Each item won an A/B and was reproduced on a second pair, unless the row says otherwise. All recommendations must work inside a Kubernetes pod: cpuset via the kubelet's static CPU manager (whole physical cores), cpu quotas sized so the busy-spin threads are not throttled, host settings (`isolcpus`, `nohz_full`, `rcu_nocbs`, `irqaffinity`, THP, hugetlbfs) on the node; pod settings (agent CPU pinning, `RUSTERON_*` env) in the pod spec. Never assume host housekeeping CPUs are available to the pod.

### Which transport, per platform

| platform | kernel UDP: best measured | DPDK (lab only) |
| --- | --- | --- |
| AWS Graviton5, c9g.2xlarge (G1–G3) | interrupt moderation off, data NIC interrupts on a spare core, no busy polling: idle p50 18.93, max 79 (76–168); 288 B at 1 M/s p50 44.83, max 204 (122–211). No lossy rep in G1 or G3; 1 of 5 in G2 (63 NAKs) | 4,096 RX descriptors: idle p50 13.97, max 75 (73–79); 288 B at 1 M/s p50 19.36, max 106 (93–114). No NAK or retransmit below the flow ceiling in G1–G3 |
| AWS x86, c7i.4xlarge (rounds 5–7) | busy polling, 4,096-entry ring: idle p50 28.22, max 1,938 (349–4,846); 288 B at 1 M/s p50 43.74, max 599, 1 lossy rep of 5 (round 7). Not converged: moderation off with its interrupts on a core of their own is untested on x86 | 4,096 RX descriptors, receiver IO vectors 16: idle p50 21.28, max 109 (52–116); 288 B at 1 M/s p50 30.38, max 152 (139–155), no lossy rep (round 7) |
| AWS x86 bare metal, c7i.metal-24xl (round 4) | busy polling: idle p50 33.70, max 67 (55–307); 288 B at 1 M/s p50 42.94, max 592 (114–1,150) | idle p50 28.13, max 71 (48–845); 288 B at 1 M/s p50 34.30, max 82 (77–103) |
| Azure D16s_v6 + D8ds_v6 (rounds 4–10) | busy polling, 4,096-entry ring: idle p50 25.82, max 167 (100–1,246) (round 8); no NIC drop or NAK in round 5 | receiver IO vectors 16; the conductor's park capped at 20 µs with a ring-sized window: idle p50 23.73, max 63 (59–796) (round 8); 288 B at 1 M/s p50 72.70, max 129 (127–205) (round 5, a farther pair) |
| Azure D8s_v6 pair, **isolated** (round 11) | busy polling: idle p50 21.15, p99.99 31.79, max 99.52 (73.86–209.41). Plain kernel UDP: idle p50 32.03, max 354.81 (272.13–2,203.65), 39 spikes of 200 µs or more and 10–40 of 100–250 µs a rep — **do not use** | **idle p50 19.90, p99.99 26.35, max 57.89 (49.47–64.22)**, **0 spikes of 200 µs or more in the whole round**, 0 of 100–250 µs in 4 of 5 reps. 32 B at 50% of flat-out: p50 26.51, max 90.50. Loss under load at 288 B is unresolved: see [round 11 loss counters](#round-11-loss-counters) |

### Every platform

| setting | why |
| --- | --- |
| CPU isolation (`isolcpus`, `nohz_full`, `rcu_nocbs`, `irqaffinity` to housekeeping), agents pinned (`pin_agents_to_configured_cpus`), EAL threads on housekeeping | Azure DPDK max 216 (69–663) → 71 µs (65–447); the agents never share a core. In k8s: static CPU manager, whole physical cores, no SMT sibling shared |
| busy-spinning threads on cores the pre-flight finds quiet | some vCPUs stop for up to 3.7 ms once a minute. Map the gaps at pod start; pin to quiet cores |
| transparent hugepages off (`/sys/kernel/mm/transparent_hugepage/enabled` = never) | Azure rounds 4 and 6: idle p99.99 67 → 53 µs, idle max 546 → 106 µs. Node-level setting |
| Aeron directory and term buffers on 2 MiB hugetlbfs, non-sparse, with a sized mount | no 4 KiB page faults; an unsized hugetlbfs fails Aeron's storage check. Mount into the pod |
| a keep-alive (about 1 kHz) from the application on sparse latency-critical streams | a frame lost on a quiet stream is noticed only at Aeron's next heartbeat, 100 ms in the C driver: one lost ping cost 134 ms (Azure round 5) |
| AWS: each channel under about 7.8 Gbit/s; a heavier stream over several channels on different ports | AWS caps one UDP flow on every size and both architectures, and holds the sender back rather than dropping |
| DPDK: `RUSTERON_DPDK_TX_RATE_MBPS` unset (no cap: the send path skips the whole pacer); set one, with `RUSTERON_DPDK_TX_LOW_STREAMS`, only on links slower than the NIC | a cap costs throughput where the link is not the limit (Azure round 3: 8 Gbit/s took 288 B from 4.58 to 3.03 M msgs/s). It also costs a refill, a sum and a charge on every send |
| DPDK: hot host-local sockets off the local-I/O epoll (the default) | localhost throughput 8.5 → 14.5 M msgs/s |
| ENA: 512 TX descriptors, `igb_uio` with write-combining | ENA's limit, and its low-latency queues |

### AWS Graviton5 (c9g)

| setting | why |
| --- | --- |
| kernel: interrupt moderation off on the data NICs (`ethtool -C <if> adaptive-rx off rx-usecs 0 tx-usecs 0`) | G1–G3: no lossy rep to 1 M msgs/s in G1 and G3 (one in G2, 63 NAKs). Transmit moderation kept at 64 µs (`COALESCE=0/64`) is not kept: G2's one lossy rep without it did not recur in G3, and it added its own tails (288 B at 1 M/s max 289, 178–569) |
| kernel: the data NICs' interrupts on a core of their own | G1, one A/B: on the housekeeping core, 1 M/s lost in 5 of 5 reps (max 10.8 ms); on the receiver's core it collapsed from 500 k/s. Graviton's idle is WFI, so the spare core never sleeps deep |
| kernel: no busy polling | G1, one A/B: the best kernel idle (p50 16.98, max 94, 76–127), but at 288 B, 1 M/s p50 4,973 µs, max 9,175 (8,544–102,826) and 61,488 NAKs. Threaded NAPI busy polling is untested: G2's setup was wrong |
| DPDK: dedicated threading (sender and receiver on cores of their own) | G1, one A/B: SHARED_NETWORK's 1,344 B bursts p50 522 against 234, max 748 (723–886) against 493 (465–745) |
| DPDK: 4,096 RX descriptors (the default: min(4,096, the NIC's limit)) | no measurable effect on c9g (G1: idle max 73, 60–88, against 75, 68–155); kept for c7i (round 6) |
| Aeron MTU 1,408; jumbo frames only on bulk or bursty channels, after a second run | G2, one run: DPDK 1,344 B bursts p50 242 → 210, max 664 (466–954) → 561 (455–1,533), ceiling +2.6%; idle unchanged (max 68, 61–100) |

### AWS x86 (c7i)

| setting | why |
| --- | --- |
| DPDK: receiver IO vectors 16 | round 3: the one loss-free DPDK mode at 1.25 M msgs/s of 288 B, max 165 (126–213) against 179 (155–1,167) with 10 NAKs |
| DPDK: 4,096 RX descriptors | round 6: first-point NIC misses in 0 of 5 reps against 4 of 5; loss-free 1,344 B rate 400 k → 600 k msgs/s; idle max 107 (88–517) against 125 (94–142) |
| kernel: busy polling with a 4,096-entry ring | the best kernel mode measured on c7i (rounds 5–7): round 7 idle p50 28.22, max 1,938 (349–4,846) |
| kernel, interrupt-driven: no ENA adaptive moderation, and no deep C-state on the core that takes the interrupts | moderation added about 60 µs at 100 k/s (c7i.2xlarge, 3 reps); c7i.4xlarge's C6 (190 µs exit) made plain kernel UDP's idle p50 235 µs (round 3) |
| bare metal (`c7i.metal-24xl`, `iommu=pt`) where the tail matters | round 4: DPDK idle max 71 (48–845) against 675 (118–852) on c7i.4xlarge (round 3) |
| not Azure's window fix (the conductor's 20 µs park with 256 KiB) | round 7: loss-free, but p99.9 of 1.8–5.2 ms from 500 k/s |

### Azure (MANA)

| setting | why |
| --- | --- |
| DPDK on the netvsc PMD, ports selected by MAC, `vdev_netvsc` ignored, 256 descriptors | see [what got DPDK working on Azure](#what-got-dpdk-working-on-azure) |
| DPDK: receiver IO vectors 16 (`AERON_RECEIVER_IO_VECTOR_CAPACITY=16`) | rounds 4–5: no NAK at 288 B up to 4.5 M msgs/s; bursts' max 2,808 → 1,000 µs (round 4) |
| DPDK: the conductor's park capped at 20 µs (`AERON_CONDUCTOR_IDLE_STRATEGY_INIT_ARGS=100-10-1us-20us`) with a receiver window the ring can hold, per channel (`rcv-wnd`): 256 × the channel's smallest datagram where loss matters more than peak rate (80 KiB for 288 B messages) | rounds 9–10: with 80 KiB no lossy rep at any point (idle max 104, 95–119); with 256 KiB, sustainable 1,344 B 400 k → 600 k msgs/s. Never use the faster park without the small window: it lost more than the default (round 9). A window carries at most about window ÷ (round trip + status-message delay) |
| kernel: busy polling (`net.core.busy_read`/`busy_poll` 50) with a 4,096-entry receive ring | round 5: no NIC drop or NAK in 5 reps, where the default ring dropped at 1 M msgs/s of 288 B; idle max 166 (138–2,851) |

### Not recommended

The transmit re-offer, `mlockall`, stopping the guest agents, `min_free_kbytes` at 2%, sender IO vectors or messages per send of 16 (and of 2 on Graviton), a 128 KiB or 1 MiB receiver window without the conductor fix, an RX burst of 64, pacing, C-states at C1 on metal, and on Graviton: SHARED_NETWORK, the conductor on its own core, a mempool cache of 512, `COALESCE=0/64` and kernel busy polling.

### Rates to run at

Run at the sustainable rate, below both the loss-free and the keeps-up rate, never at the flat-out maximum, which loses. It depends on the pair.

- **Graviton c9g.2xlarge:** DPDK had no NAK or retransmit below the flow ceiling in G1–G3: 288 B to 1 M msgs/s and 1,344 B to 500 k. Short sends hit 4 of 50 reps of 1,344 B bursts (host a's NIC taking frames slowly; every message echoed). The kernel path had no lossy rep in G1 and G3 (G2: one rep of five, 63 NAKs, at 1 M/s of 288 B).
- **Azure** (`Standard_D16s_v6` with `Standard_D8ds_v6`), the highest load with no loss (NAKs, retransmits, drops and short sends) in any of five reps: DPDK 3 M msgs/s at 288 B and 400 k msgs/s at 1,344 B in round 7, 1 M and 800 k in round 8, against kernel busy polling's 1 M and 400 k in round 7, and 2 M and 400 k it kept up with in round 8. Flat-out, DPDK carried about 4.5 M and 1.03 M with NAKs, and on round 6's and round 8's pairs 1,344 B collapsed to about 0.1 M in most reps.

## Environment

| | Azure | AWS x86 | AWS Graviton |
| --- | --- | --- | --- |
| Instances | `Standard_D8s_v6` / `D16s_v6` + `D8ds_v6` (Xeon 8573C) | `c7i.2xlarge`, `c7i.4xlarge`, `c7i.metal-24xl` (Sapphire Rapids) | `c9g.2xlarge` (Neoverse V3) |
| NIC | MANA, 256-descriptor rings | ENA, 512 TX / 1,024–4,096 RX | ENA |
| OS / kernel | Debian 13, Linux 7.2.6 | Debian 13, Linux 7.2.6 | Debian 13, Linux 7.2.6 |
| Clock source | `tsc` | `tsc` | `arch_sys_counter` (`CNTVCT_EL0`) |
| Isolation | `isolcpus=nohz,domain,managed_irq,1-N`, `nohz_full`, `rcu_nocbs`, `irqaffinity=0`; agents pinned | the same, plus `mitigations=off` in the lab profile | the same; no OS-controlled C-states (idle is WFI) |
| Aeron | rusteron-media-driver 0.2.11 (Aeron 1.53.3), MTU 1408, hugetlbfs 2 MiB, exclusive publications | the same | the same, `-mcpu=native` |
| Placement | one proximity placement group per round | cluster placement group, us-east-1 | cluster placement group, us-east-1, spot |

Round-trip and load numbers are median of 5 reps with the range across reps in brackets; max is one sample per rep and varies most. Compare modes within one round, never across pairs: placement alone moved DPDK's idle p50 by about 7 µs on c7i.4xlarge (rounds 6 and 7).

## Measured latency at each load level

Every figure keeps its max. Full tables are in git history; this is the record.

### Graviton5 (c9g.2xlarge), round G3 — best measured (p50 / p99.99 / max)

| point | DPDK | DPDK, media timestamps | kernel UDP, moderation off, spare core |
| --- | --- | --- | --- |
| idle, 32 B | 13.97 / 33.66 / 75 (73–79) | 13.97 / 32.59 / 74 (62–153) | 18.93 / 44.09 / 79 (76–168) |
| 288 B at 100 k/s | 14.34 / 41.34 / 84 (83–101) | 14.32 / 44.13 / 77 (76–167) | 19.55 / 91.71 / 177 (116–193) |
| 288 B at 1 M/s | 19.36 / 62.11 / 106 (93–114) | 19.34 / 58.72 / 107 (93–179) | 44.83 / 176.00 / 204 (122–211) |
| 288 B, bursts of 1,000 at 100 k/s mean | 122.43 / 222.97 / 231 (218–314) | 122.75 / 225.92 / 237 (229–241) | 198.40 / 359.68 / 372 (366–375) |
| 1,344 B at 500 k/s | 18.62 / 59.30 / 104 (95–111) | 18.69 / 64.99 / 97 (90–183) | 31.38 / 131.71 / 188 (112–742) |
| 1,344 B, bursts of 1,000 at 500 k/s mean | 245.89 / 468.99 / 501 (491–1,819) | 245.12 / 475.39 / 1,134 (484–1,595) | 606.72 / 1,131.52 / 1,159 (1,103–1,176) |

Lossy reps: DPDK none below the flow ceiling; timestamps mode 1 of 5 at 1,344 B bursts (short sends only, 0 NAKs); kernel none. G3's 288 B bursts average 100 k/s; G1 and G2 offered 500 k/s — do not compare bursts across rounds.

### Azure, rounds 4–10 — DPDK and kernel (p50 / p99.99 / max)

| point | DPDK (best) | kernel busy polling (best) |
| --- | --- | --- |
| idle, 32 B (round 8, close pair) | 23.73 / 35.55 / 63 (59–796) | 25.82 / 41.47 / 167 (100–1,246) |
| idle, 32 B (round 5) | 45.28 / 58.37 / 529 (86–1,122) | 46.72 / 68.86 / 1,099 (235–3,424) |
| 288 B at 1 M/s (round 5) | 72.70 / 100.35 / 129 (127–205) | 101.44 / 14,787 / 15,049 (6,730–33,161) |
| 288 B at 3 M/s (round 5) | 86.14 / 108.80 / 149 (137–571) | past capacity |
| 1,344 B at 600 k/s (round 4) | 87.36 / 107.26 / 142 (134–259) | past capacity from 600 k/s |
| 288 B, bursts of 1,000 (round 4) | 119.49 / 520.45 / 1,000 (326–4,559) | 122.11 / 18,268 / 18,383 (16,622–28,443) at 1,344 B 400 k/s |

### Azure round 11 (2026-10-18), D8s_v6 pair — idle round trip (p50 / p90 / p99 / p99.9 / p99.99 / max)

First round with both `pinned` and `isolated` states, 4 modes × 5 reps, 300,000
round trips a rep. `isolated` boots with `isolcpus=nohz,domain,managed_irq`,
`nohz_full`, `rcu_nocbs` and `irqaffinity` on the housekeeping CPUs. Every
percentile keeps its max; the range across reps is in brackets.

| state, transport | p50 | p90 | p99 | p99.9 | p99.99 | max |
| --- | --- | --- | --- | --- | --- | --- |
| **isolated, DPDK** | 19.90 (19.89–19.93) | 20.24 (20.21–20.27) | 20.54 (20.53–20.61) | 22.70 (22.50–22.80) | 26.35 (25.84–27.02) | **57.89 (49.47–64.22)** |
| isolated, DPDK, agents poll local sockets | 19.90 (19.89–20.03) | 20.24 (20.21–20.37) | 20.54 (20.50–20.91) | 22.67 (22.35–23.21) | 26.67 (26.02–53.73) | 67.26 (46.81–917.50) |
| isolated, kernel UDP, busy polling | 21.15 (21.14–21.25) | 22.08 (22.05–22.18) | 22.99 (22.94–23.07) | 26.05 (25.86–26.21) | 31.79 (29.14–33.98) | 99.52 (73.86–209.41) |
| isolated, kernel UDP | 32.03 (30.46–39.52) | 45.18 (34.62–46.34) | 48.61 (46.75–50.66) | 53.82 (50.56–60.48) | 100.99 (90.62–131.71) | 354.81 (272.13–2,203.65) |
| pinned, DPDK | 19.95 (19.92–19.95) | 20.27 (20.24–20.27) | 20.69 (20.66–20.82) | 26.73 (26.46–27.15) | 34.40 (31.33–35.77) | 91.84 (58.02–102.02) |
| pinned, DPDK, agents poll local sockets | 19.97 (19.93–19.98) | 20.30 (20.25–20.34) | 21.04 (20.70–22.24) | 27.45 (26.91–27.73) | 34.43 (31.05–37.98) | 120.38 (71.94–2,295.81) |
| pinned, kernel UDP, busy polling | 21.10 (20.98–21.15) | 22.00 (21.86–22.06) | 23.20 (23.07–23.76) | 28.96 (28.66–31.92) | 43.01 (36.29–70.72) | 102.40 (71.87–860.16) |
| pinned, kernel UDP | 39.23 (39.20–39.39) | 45.57 (45.53–45.63) | 47.77 (47.62–48.13) | 52.41 (51.97–53.44) | 77.44 (67.33–117.69) | 1,495.04 (106.88–2,029.57) |

**Best Azure figure measured: isolated DPDK max 57.89 µs (49.47–64.22),** against
round 8's 63 (59–796). Isolation is what did it — the same DPDK binary on the
same pair without isolation is 91.84 (58.02–102.02).

#### Spikes: 200 µs and over, and 100–250 µs, isolated

The goal's second condition is no spikes. Two counts on the idle run (300,000
round trips a rep). Round trips of 200 µs or more, and where the analysis could
place them:

| transport | spikes ≥ 200 µs | host stall | unexplained |
| --- | --- | --- | --- |
| DPDK | **0** | 0 | 0 |
| DPDK, agents poll local sockets | 3 | 2 | 1 |
| kernel UDP, busy polling | 1 | 0 | 1 |
| kernel UDP | 39 | 1 | 38 |

Round trips between 100 and 250 µs per rep:

| transport | rep 1 | rep 2 | rep 3 | rep 4 | rep 5 |
| --- | --- | --- | --- | --- | --- |
| DPDK | 0 | 0 | 0 | 0 | 0 |
| DPDK, agents poll local sockets | 0 | 11 | 0 | 0 | 0 |
| kernel UDP, busy polling | 0 | 0 | 1 | 0 | 1 |
| kernel UDP | 30 | 34 | 18 | 10 | 40 |

DPDK has no round trip of 200 µs or more in the whole round, and none of
100–250 µs in four of five reps (11 in one). Plain kernel UDP has 39 at
200 µs or more and 10–40 at 100–250 µs in every rep. **A better p50 with these
tails is a regression**, and plain kernel UDP on Azure fails that test: its
median is 32 µs against DPDK's 20, and its p99.99 is four times worse.

Unexplained per-rep maxima (round trips the analysis could not attribute to a
known slow-path episode), isolated — these are the `max` column above:

| transport | per-rep max (µs), all unexplained |
| --- | --- |
| DPDK | none: every rep under 100 µs (range 49.47–64.22) |
| DPDK, agents poll local sockets | four reps under 100 µs; rep 2 **917.50**, which also had agent loop gaps on both agents (a 18/3/7, b 23/1/4 counts at 20–50/50–100/100+ µs). Range 46.81–917.50 |
| kernel UDP, busy polling | three reps under 100 µs; rep 3 **194**, rep 5 **209.41**. Range 73.86–209.41 |
| kernel UDP | 272 / 273 / 396 / 355 / **2,203.65** |

The largest is still the kernel UDP 2,203 µs outlier. **None of these four
maxima is traced to a cause yet.** Nothing based on any of them is recommended
until it is.

#### What round 11 did not measure

`localhost` runs were skipped: "no fifth core on VM a". The goal's matrix wants
both the cross-host and the localhost path. **The localhost path is not measured
on Azure this round.** Run it on a larger pair, or with a fifth core reserved.

### Azure round 11 under open-loop load (p50 / p99.99 / max)

`achieved` below 98% of `offered` is marked `*`: the rate generator could not
keep its own schedule, and the figures beside it include that queueing — they
are **not** path latency. `lag` is how far the generator itself fell behind
(median of the reps). Loads are 10 / 50 / 80 / 95 / 100% of each mode's
flat-out rate, plus bursts of 1,000 at the 50% mean. Every percentile keeps
its max; the range across the 5 reps is in the full analysis.

**isolated, DPDK** — the configuration the idle table recommends:

| size | load | offered | achieved | p50 | p99.99 | max | lag |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 32 B | 10% | 2.01 M/s | 2.01 M/s | 18.98 | 30.16 | 72.77 | 45 |
| 32 B | 50% | 10.03 M/s | 10.03 M/s | 26.51 | 59.77 | 90.50 | 49 |
| 32 B | 80% | 16.04 M/s | 12.66 M/s * | 681,574 | 1,334,837 | 1,334,837 | 1,334,526 |
| 32 B | 100% | 20.05 M/s | 16.29 M/s * | 678,953 | 1,163,919 | 1,163,919 | 1,154,227 |
| 32 B | bursts 1,000 | 10.03 M/s | 10.03 M/s | 62.62 | 113.73 | 202.75 | 108 |
| 288 B | 10% | 394 k/s | 394 k/s | 20.89 | 33.92 | 74.43 | 22 |
| 288 B | 50% | 1.97 M/s | 1.97 M/s | 20.37 | 2,140.16 | 2,551.81 | 23 |
| 288 B | 80% | 3.15 M/s | 3.15 M/s | 20.94 | 1,854.46 | 2,199.55 | 25 |
| 288 B | 100% | 3.94 M/s | 3.94 M/s | 2,809.86 | 4,689.92 | 4,706.30 | 25 |
| 288 B | bursts 1,000 | 1.97 M/s | 1.97 M/s | 78.78 | 142.21 | 185.85 | 24 |
| 1,344 B | 10% | 91 k/s | 91 k/s | 22.21 | 31.28 | 71.10 | 20 |
| 1,344 B | 50% | 456 k/s | 456 k/s | 23.50 | 32.37 | 71.55 | 22 |
| 1,344 B | 80% | 729 k/s | 729 k/s | 22.06 | 33.95 | 88.70 | 23 |
| 1,344 B | 95% | 866 k/s | 866 k/s | 21.77 | 70.59 | 324.86 | 21 |
| 1,344 B | 100% | 911 k/s | 911 k/s | 21.61 | 1,225.73 | 1,558.53 | 22 |
| 1,344 B | bursts 1,000 | 456 k/s | 456 k/s | 253.18 | 4,325.38 | 4,362.24 | 24 |

What this says:

- **Up to 50% of flat-out, and at every 1,344 B rate up to 95%, DPDK holds its
  idle latency.** p50 19–27 µs, max 71–91 µs. The idle figures are the right
  ones to quote for a working load.
- **32 B from 80% up is the generator, not the network.** `lag` is 1.1–1.3 ms,
  the same order as the latency beside it. The generator cannot offer 16 M
  messages a second on one core, so the intended-send-time clock runs away.
  The true path latency at those rates is not measured this round.
- **288 B and 1,344 B show a tail the idle run does not.** 288 B at 50% has
  p99.99 2,140 µs while its p50 stays at 20. 1,344 B at 100% has p99.99 1,226.
  Both are past the point where loss starts (below), so the tail is the same
  event seen late: a lost frame, then Aeron's recovery. **Not a transport
  latency; a loss symptom.**
- **Bursts of 1,000 at 32 B and 288 B stay clean** (max 203 and 186). At
  1,344 B bursts the max is 4,362 µs and 5 of 5 reps lost 13,925 NAKs between
  them — the same MANA ring overflow that IO vectors 16 was meant to fix, not
  fixed for 1,344 B bursts on this pair.

**isolated, kernel UDP** for contrast, 32 B at 10% / 50% / 80%:
p50 114.75 / 58.40 / 207.10 µs, max 2,425 / 2,320 / 6,652 µs. It is 6× worse
than DPDK at the median and 30–70× worse at the max, at 10% of its own
flat-out. **Kernel UDP on Azure is not the path to run.**




### AWS c7i.4xlarge, round 7 (p50 / p99.9 / max, median across reps)

| 288 B | 500 k msgs/s | 1 M msgs/s | highest loss-free 288 B / 1,344 B |
| --- | --- | --- | --- |
| DPDK | 23.63 / 38.11 / 135, 0 lossy | 30.38 / 54.02 / 152, 0 lossy | 1 M / 100 k¹ |
| kernel UDP, busy polling, 4,096 ring | 35.71 / 172.16 / 427, 0 | 43.74 / 242.18 / 599, 1 lossy (4 NAKs) | 500 k / 100 k |
| kernel UDP | 84.35 / 179.46 / 492, 0 | 85.89 / 653.31 / 2,574, 2 lossy (5 NAKs) | 500 k / 500 k |

¹ With ENA's default 1,024 RX descriptors as this round ran; with 4,096 (round 6) the loss-free 1,344 B rate was 600 k msgs/s. 1,344 B at 1 M msgs/s is past one flow's ceiling (0.705 M msgs/s) for every transport.

Round 7 at Aeron's 100 k/s point, 288 B (p50 / p99 / p99.9 / max): DPDK 21.82 / 26.56 / 29.30 / 117 (109–123); kernel busy polling 27.79 / 63.81 / 126.78 / 382 (372–389); kernel UDP 94.02 / 129.60 / 150.53 / 469 (448–578). Aeron's published open-source C driver figure (kernel UDP, different instance and harness): 22 / 43 / 60, no max published. Not a like-for-like comparison.

## Sustained maximum throughput (M msgs/s)

| platform, transport | 32 B flat-out | 288 B flat-out | 1,344 B flat-out | 288 B loss-free | 1,344 B loss-free |
| --- | --- | --- | --- | --- | --- |
| Azure DPDK (rounds 5, 7) | 22.76 | 4.57 | 1.03–1.04 | 3.0 | 0.4–0.6 |
| Azure DPDK, isolated (round 11) | 20.05 | 3.94 | 0.91 | none | 0.46 |
| Azure kernel busy polling (round 5) | 13.16 | 2.49 | 0.64 | 1.0 | 0.4 |
| Azure kernel busy polling, isolated (round 11) | 13.97 | 2.62 | 0.68 | 0.26 | 0.54 |
| c7i.4xlarge DPDK (round 5) | 15.17 | 3.03 | 0.705 | 3.03 (one flow cap) | 0.4 |
| c7i.2xlarge kernel UDP (round 2) | 15.17 | 2.94 | — | — | — |
| c7i.metal DPDK (round 4) | 6.75 | 1.82 | — | — | — |
| c7i.metal kernel UDP (round 4) | 15.17 | 3.03 | — | — | — |
| c9g.2xlarge DPDK (G1–G3) | — | — | 0.6855 (flow cap) | 1.0 (288 B) | 0.5 |

`—` is a point not yet measured. `loss-free` is the highest ladder rate below the first point that lost anything in any rep. Flat-out is a ceiling, not a rate to run at. One AWS flow never carried more than 8.0 Gbit/s of frames (15.17 M / 3.03 M / 0.705 M msgs/s at 32 / 288 / 1,344 B): the sender is held back, nothing is dropped. ENA's allowance-exceeded counters stay 0 at the ceiling.

**Round 11's `none` is a real result, not a gap.** Isolated DPDK at 288 B lost in
2 of 5 reps at the *lowest* ladder point (394 k/s, 10% of flat-out, 18 NAKs
between them), so there is no loss-free rate to name on that pair. Rounds 5 and
7 named 3.0 M msgs/s on a different, larger pair. Two things differ and neither
is ruled out as the cause: the pair (D8s_v6 against D16s_v6 + D8ds_v6), and
round 11's stricter definition, which counts short sends as loss. **Do not
quote 3.0 M msgs/s as the Azure loss-free rate for 288 B.** Re-run round 11 on
the round 7 pair before the two are compared.

## Loss counters

Read before and after every run on both hosts. What they showed:

| counter | meaning | finding |
| --- | --- | --- |
| NAKs, retransmits | Aeron's recovery | one lost frame on a sparse stream costs about 100 ms (C driver heartbeat). Zero loss is the first condition of low latency |
| `dpdk-nic-rx-missed` / `rx-ring-full-suspected` | MANA's 256-descriptor ring overflowing while the receiver stalls | the cause of Azure's 1,344 B ladder NAKs; fixed by IO vectors 16 and a ring-sized window |
| `noport-` (was `dpdk-queue-drop`) | frames to a closed port (the previous test's endpoints) | not loss |
| `dpdk-arp-miss` | 20 short sends per mode at start-up | startup ARP, not loss |
| short sends, refused transmits | the send path refused a frame | counted as loss on purpose; Aeron does not advance after a short send. Host a's egress noise (4 of 50 DPDK 1,344 B burst reps over G1–G3) showed this signature with 0 NAKs and every message echoed |
| kernel `RcvbufErrors` / `InErrors`, NIC drops | socket and NIC receive overflow | the default receive ring dropped at 1 M msgs/s of 288 B on Azure; a 4,096-entry ring removed it |
| ENA `bw_out`/`pps_allowance_exceeded` | instance network allowance | stayed 0 at the per-flow ceiling |
| `sw-checksum-verified` | software UDP checksum | 0 of 350,000 frames on c7i ENA: the NIC verifies every frame. MANA verified 7–10 of 56 M: same story |

### Round 11 loss counters

Over the **idle** round trip alone, both hosts, all reps:

| state, transport | grew |
| --- | --- |
| pinned and isolated, kernel UDP | none |
| pinned and isolated, kernel UDP, busy polling | none |
| pinned and isolated, DPDK, and DPDK with local sockets | `dpdk-arp-miss` 20, `short-sends` 20 |

20 ARP short sends per DPDK mode are start-up ARP, not loss. Idle is clean.

Over the round-trip and throughput runs together the same DPDK modes also grew
`dpdk-tx-eagain` about 40 M and `short-sends` about 40 M — those are the
flat-out ceilings refusing frames, which is what flat-out means, not an idle
path loss. The real recovery traffic is NAKs and retransmits:

| state, transport | NAKs sent+received | retransmits | where |
| --- | --- | --- | --- |
| isolated, DPDK | 51 | 51 | scattered across the load ladder; worst 288 B at 95% and 100% |
| isolated, DPDK, local sockets | 121 | 121 | 288 B at 95%: 5 of 5 reps lost (152 NAKs) |
| isolated, kernel UDP, busy polling | 1 | 1 | 32 B at 10%: 4 of 5 reps |
| isolated, kernel UDP | none over the pair of runs | — | but see the load table: its latency is unusable anyway |

`isolated-nic-irqs` — the data NICs' interrupts landing on a CPU other than the
housekeeping one — grew in **0 of 920 runs**. The IRQ pinning held every time.
That is the check on the run, and it passed.

## How to run

```sh
az login
md-dpdk/benches/cloud/azure/lab.sh                          # both states, 5 reps of every mode, about 45 minutes
LAB_STATES=isolated md-dpdk/benches/cloud/azure/lab.sh      # one state
LAB_KEEP=1 LAB_REPS=0 md-dpdk/benches/cloud/azure/lab.sh    # build the VMs and keep them
LAB_MODES="kernel kernel-mono kernel-busy kernel-busy-mono dpdk dpdk-mono" md-dpdk/benches/cloud/azure/lab.sh
LAB_MODES="dpdk dpdk@AERON_RECEIVER_IO_VECTOR_CAPACITY=4 kernel-busy@RX_RING=4096" md-dpdk/benches/cloud/aws/lab.sh
LAB_LOAD_LADDER="288:250000,500000,1000000" LAB_LOAD_SIZES=288 md-dpdk/benches/cloud/aws/lab.sh
LAB_BASE=HEAD~1 LAB_MODES="dpdk dpdk@BIN=bin-base" md-dpdk/benches/cloud/aws/lab.sh   # this checkout against an earlier revision
md-dpdk/benches/cloud/analyse.py --ab dpdk dpdk@BIN=bin-base target/md-dpdk-bench/<stamp>/results.csv
LAB_TYPE=c7i.4xlarge LAB_TPUT_FLOWS=2 md-dpdk/benches/cloud/aws/lab.sh   # two flows at once
just test-dpdk-lab                                           # the analysis against its fixture, no cloud
```

A mode's `@KEY=value` is a driver environment variable or a `vm.sh` host setting: `CSTATE=<n>`, `COALESCE=off|<rx>/<tx>`, `RX_RING=<n>`, `MIN_FREE_PCT=<n>`, `THP=never`, `CLOCKSOURCE=<name>`, `AGENTS=off`, `RELIABLE=false`. One that cannot apply on that cloud fails the run.

Every mode runs on the lab's baseline (`vm.sh baseline`): transparent hugepages off and, for DPDK modes, receive IO vectors 16. `lab.sh` deletes its resource group on any exit; results land in `target/md-dpdk-bench/<stamp>/`. In Kubernetes, run the same driver settings in the pod env and the host settings on the node; use the static CPU manager with whole physical cores for the busy-spin threads.

## What got DPDK working on Azure

- Use the netvsc PMD, not MANA alone. With the kernel interfaces down, Azure moves traffic to the synthetic path and DPDK's MANA PMD receives nothing. Move each data NIC's VMBus device to `uio_hv_generic`.
- Select ports by MAC (`RUSTERON_DPDK_SENDER_MAC`, `RUSTERON_DPDK_RECEIVER_MAC`).
- Stop the automatic fail-safe device: `--vdev=net_vdev_netvsc0,ignore=1` when ports are selected by MAC.
- 256 descriptors. MANA queues hold at most 256.
- Pin the agents: without it, DPDK's round trip was 2.8 ms.

## Experiment log

One change per experiment, A/B against the current best, 5 reps; kept only if the medians win, the tail does not widen, and a second run reproduces it. Loss counters before and after every run on both hosts.

| date | change | result | decision |
| --- | --- | --- | --- |
| 2026-10-13 | DPDK against kernel UDP and busy polling (D8s_v6 pair) | DPDK fastest at every percentile | baseline |
| 2026-10-13 | TSC clock in the transport and clients | 9–10 ns off each send, 4 ns off each poll; round trips unchanged within noise at every percentile and the max | kept: cheaper and calibrated to 0.01 ppm |
| 2026-10-14 | host-local channels read every cycle after data (`LOCAL_IO=adaptive`) | localhost p50 14.1 → 6.4 µs, max 260 (203–1,117) → 61 (50–116) pinned | kept, now the default |
| 2026-10-14 | re-offer a refused transmit tail for 50 µs | round trips unchanged; flat-out short sends −99%; 1,344 B still collapsed | open |
| 2026-10-14 | (finding) 1,344 B frames flat out over DPDK collapsed | 0.03 M msgs/s in 4 of 5 reps: frames dropped unseen, Aeron fell into NAKs | led to TX rate and stream priority, under test |
| 2026-10-14 | (measurement bug) per-run DPDK loss counters freed on port close | per-run deltas swung by ± the whole total | fixed: counters stay registered; sweep 2 and round 3 DPDK loss columns void |
| 2026-10-14 | (finding) where the agents are preempted | EAL threads and `aeron-md-nra` on the agents' CPUs; khugepaged LRU drains 8–22 µs every 10.24 s; VM b stalled 0.3–0.76 ms at :57 | THP off to round 4 |
| 2026-10-14 | round 3, stopped after rep 2 (counter bug) | 8 Gbit/s cap: 288 B 4.58 → 3.03 M msgs/s; 128 KiB window collapsed; receiver IO vectors 16: max 250 against 1,260 µs | cap off by default; window and sender IO vectors rejected; receiver IO vectors to round 4 |
| 2026-10-14 | round 4, receive IO vectors 16 | NAKs 16–27 k → 0 at 1,344 B 600–800 k/s; p99.99/max 885/1,130 → 107/142 µs; bursts' max 2,808 → 1,000 µs | candidate; round 5 must reproduce |
| 2026-10-14 | round 4, THP off | idle p99.99 67 → 53 µs, max 546 → 106 µs (round 6) | not kept alone; retried on top of IO vectors 16 |
| 2026-10-14 | round 4, hot sockets on the local-I/O epoll | localhost 8.49 against 14.45 M msgs/s | the default (unwatch) kept |
| 2026-10-14 | (finding) the once-a-minute stall | per VM, at a fixed second, some vCPUs stop for up to 3.7 ms (Azure) or about 400 µs (AWS), nothing in the guest | pre-flight records noisy vCPUs; `spikes.py` labels it |
| 2026-10-14 | (bug) DPDK on ENA crashed at start | 1,024 TX descriptors exceeded ENA's 512 | fixed: queue sizes checked against the NIC's limits; TX default 512 |
| 2026-10-14 | (measurement) `dpdk-queue-drop` | about 140 per load point: frames to a closed port | reported as `noport-`, not loss |
| 2026-10-14 | round 5, IO vectors 16 reproduced | NAKs 10,832 → 132 at 1,344 B 800 k/s; sustained 1.04 M msgs/s every rep | **kept**: DPDK's baseline from round 6 |
| 2026-10-14 | round 5, 4,096-entry kernel receive ring | no NAKs or NIC drops at 288 B 1 M/s (default: 3 NAKs, 64 drops) | kept for the kernel path |
| 2026-10-14 | round 5, `min_free_kbytes` 2% | one rep stormed at 200 k/s | not kept |
| 2026-10-14 | (finding) a 134 ms round trip in round 5 | one frame lost, found only at the next heartbeat (100 ms) | documented: zero loss first; keep-alive about 1 kHz |
| 2026-10-14 | EKS, DPDK pods on two c7i.4xlarge | p50 28.4 µs, p99.99 63 (58–72), max 126 (112–148), no loss | baseline for EKS |
| 2026-10-14 | (finding) host-local channels counted no agent loop gaps | port counters never registered | fixed |
| 2026-10-14 | AWS round 1–2, c7i.2xlarge | DPDK p50 23.6 against busy polling 31.4; no loss to 1 M/s of 288 B | DPDK for latency on AWS |
| 2026-10-14 | (finding) kernel errors were `ENETUNREACH` | cloud-init NIC hotplug re-ran DHCP and dropped data NIC routes | fixed in `vm.sh` |
| 2026-10-14 | (method) loss counters around each sustained-maximum run | round 6's collapse left no counters | added |
| 2026-10-15 | AWS round 3, c7i.4xlarge | IO vectors 16 the only loss-free DPDK mode at 1.25 M/s; NIC statistics reads and `mlockall` no gain | IO vectors 16 kept |
| 2026-10-15 | round 6, THP off on top of IO vectors 16 | idle max 546 → 106 µs; flat-out collapsed on this pair (1.03 M → 50 k) | THP off kept (reproduced); flat-out not counted |
| 2026-10-15 | round 7, new pair on the round 6 baseline | DPDK idle max 94 (80–97), every rep under 100 µs; RX burst 64 lossier; 1 MiB window cut every rate by a third | burst 64 and 1 MiB window rejected |
| 2026-10-15 | round 8, sends all or nothing and the receive drain | DPDK idle p50 23.7; 32 B 22.9 M msgs/s; 1,344 B flat-out collapsed 4 of 5 | drain kept; collapse to round 9 |
| 2026-10-15 | (measurement bug, fixed) ENA IRQs on isolated CPUs | the ENA driver re-pins queue IRQs on every interface up | lab re-pins after every hand-back; AWS kernel results rounds 2–5 pessimistic, to be re-run |
| 2026-10-15 | round 9, conductor park 20 µs + ring-sized window | C with 256 KiB lost nothing at 1,344 B at any rate; C alone lost more than the default | C with a ring-sized window is the candidate; C never without it |
| 2026-10-15 | round 10, C with 256 KiB and 80 KiB on a farther pair | 256 KiB: sustainable 1,344 B 400 k → 600 k; 80 KiB: no lossy rep anywhere, flat-out capped by window ÷ round trip | C with a ring-sized window kept for Azure DPDK, sized per channel |
| 2026-10-15 | AWS round 5, sends all or nothing | DPDK at the per-flow ceiling like kernel UDP (15.17 M at 32 B); no NAK at any ladder rate | transmit fix confirmed |
| 2026-10-15 | (finding) one AWS flow carries about 8 Gbit/s | same on every size and architecture; holds the sender back, drops nothing | documented; heavier streams over several channels |
| 2026-10-15 | AWS round 4, c7i.metal-24xl | DPDK idle max 71 (48–845); loss-free to 1.25 M/s with max under 140 µs; C1 not kept on metal | IO vectors 16 kept |
| 2026-10-15 | (finding) stalls on metal | two all-CPU pauses of 265–272 µs (SMI-like); 3 of 10 DPDK reps still had a 676–845 µs round trip | untraced; the c7i.4xlarge maxima are mostly, but not only, the hypervisor |
| 2026-10-15 | (finding, fixed) the DPDK receive poll was capped by Aeron's vlen | the receiver drained 4 frames a duty cycle by default | fixed: up to the burst size a burst whatever the vlen |
| 2026-10-15 | (finding, fixed) DPDK flat-out throughput below the kernel on AWS | partial vector sends caused duplicates (34–56% of frames) | fixed: sends are all or nothing |
| 2026-10-15 | AWS round 6–7 | 4,096 RX descriptors: first-point NIC misses 0 of 5 against 4 of 5; Aeron load points: DPDK 21.8/26.6/29.3 at 100 k/s of 288 B | 4,096 RX candidate |
| 2026-10-15 | (finding) ENA adaptive interrupt moderation is kernel UDP's gap to Aeron's published figure | at 100 k/s of 288 B p50 106 with it, 47 with moderation off (equal to idle); off loses at 500 k/s with every interrupt on the housekeeping CPU | repeat with interrupts on a core of their own |
| 2026-10-15 | (default) `RUSTERON_DPDK_TX_RATE_MBPS` from 0 to a 25,000 Mbit/s backstop | a high default, 1 MiB bucket, defers rather than spinning | shipped with tests |
| 2026-10-16 | AWS Graviton G1 | DPDK p50 14.0 / max 82 µs at 100 k/s of 288 B; kernel moderation off + spare core loss-free to 1 M/s | kernel on Graviton: moderation off plus interrupts on their own core |
| 2026-10-16 | (finding) round A's comparison stopped | x86 tuned over many rounds, Graviton not; tune each platform before comparing | comparisons only after each side converges |
| 2026-10-16 | (lab bug, fixed) round G2 froze twice, 47 and 16 minutes | the cross pong retried a back-pressured claim for ever; its SSH session ended 255, which `on()` read as a dropped connection and ran the pong again | fixed test-first; bounded waits; `lab-faults.log` |
| 2026-10-16 | AWS Graviton G2 | DPDK idle p50 13.78, max 66 (61–77); conductor own core, mempool cache 512, sender IO vectors 2: no gain; `COALESCE=0/64` added tails (max 289, 178–569) | none kept; jumbo a candidate for bulk channels |
| 2026-10-16 | (lab fault) `NAPI_BUSY=spare` put all 16 NAPI kthreads on one core | the flow's NAPI got a sixteenth of the core: 50–80 ms waits. Measures the setup, not threaded NAPI busy polling | retest with `ethtool -L combined 1` and a core per kthread |
| 2026-10-16 | AWS Graviton G3 | a third pair, the same medians; media timestamps no measurable cost (one rep a point max 152–183 µs, 4 of 20 against 0 of 20, p ≈ 0.1) | timestamps: repeat once before making them a default |
| 2026-10-16 | (finding) the one lossy rep in G3 | host a's egress: 630,837 short sends, 646,280 refused transmits, 0 NAKs, every message echoed. Same signature in 4 of 50 DPDK 1,344 B burst reps | AWS egress noise, not the mode |
| 2026-10-17 | (correction) G1's "4,096 RX descriptors removed the lossy rep" | that rep was a stall of host b (max 2,564 µs), not receive loss | no measurable effect on c9g; 4,096 stays the default for c7i |
| 2026-10-17 | (correction) SHARED_NETWORK on Graviton | G1's write-up missed the burst cost: 1,344 B bursts p50 522 against 234, max 748 (723–886) against 493 (465–745) | not the default |
| 2026-10-17 | (finding) the per-flow cap on Graviton | 1,344 B: 685.5 k msgs/s in every rep of G1 and G2, both transports | one cap on bytes per flow, same on both architectures |
| 2026-10-17 | (lab and analysis faults) found in G3 | ping's 43–44 µs minimum exceeds DPDK's whole round trip on c9g; the spike file keeps only the first spikes (lists 459–482 µs where histograms show 491–1,819); every load max of 100 µs or more unexplained; no diagnostics ran during the 30-minute discipline | drop or replace the ping baseline; take maxima from the histograms; start diagnostics before the discipline |
| 2026-10-18 | (default) `RUSTERON_DPDK_TX_RATE_MBPS` unset is now no cap, not a 25,000 Mbit/s backstop | the backstop put a refill, a compare and a charge on every send whatever the run was doing, and a send it held back sent nothing rather than waiting. Unset now skips the whole pacer block; a rate the operator set paces and waits up to `RUSTERON_DPDK_TX_PACE_SPIN_US` (`0` refuses instead of waiting, which is the behaviour the backstop had); `RUSTERON_DPDK_TX_LOW_STREAMS` still needs a rate or route the operator set. Cost of the change: not yet measured | kept (the simplification is the point; the live Azure round 1 will show whether the paced-sends counter falls away) |
| 2026-10-18 | round 11, D8s_v6 pair, `pinned` against `isolated`, 4 modes × 5 reps | isolated DPDK idle p50 19.90, p99.99 26.35, **max 57.89 (49.47–64.22)** — the best Azure figure measured, against pinned's 91.84 (58.02–102.02). Spikes of 100–250 µs: 0 in 4 of 5 isolated-DPDK reps (11 in one), 10–40 in every kernel-UDP rep | **isolation kept.** It is what takes the max down |
| 2026-10-18 | round 11, 32 B throughput | DPDK 20.05 (20.05–20.05) M msgs/s in every mode and state; kernel medians 14.10–14.58 (reps 13.66–14.66); zero drop at the throughput point in every mode | no change |
| 2026-10-18 | round 11, loss under load | isolated DPDK 288 B lost in 2 of 5 reps at the *lowest* ladder point (394 k/s, 18 NAKs). Round 5/7 named 3.0 M msgs/s loss-free on a larger pair | **not a contradiction to hide.** Pair and loss definition both differ; re-run round 11 on the round 7 pair before comparing |
| 2026-10-18 | round 11, 32 B from 80% load | p50 of 681 ms with `lag` of 1,334 ms | the rate generator, not the network. A generator that cannot offer 16 M/s on one core makes the intended-send-time clock run away. True path latency at those rates **not measured** |
| 2026-10-18 | round 11, 288 B and 1,344 B tails under load | 288 B at 50%: p50 20.37 but p99.99 2,140. 1,344 B at 100%: p99.99 1,226 | the same event as the loss above, seen late: a lost frame then Aeron's recovery. Not transport latency |
| 2026-10-18 | round 11, unexplained idle maxima | kernel UDP 272 / 273 / 396 / 355 / **2,203.65**; kernel UDP busy polling 194 and 209.41 on reps 3 and 5; DPDK local sockets **917.50** on rep 2 (agent loop gaps on both agents). DPDK alone: every rep under 100 µs | **not yet traced to a cause.** Nothing based on any of them is recommended until it is |
| 2026-10-18 | round 11, spikes of 200 µs and over (idle) | DPDK **0**; DPDK local sockets 3 (2 host stall, 1 unexplained); kernel UDP busy polling 1; kernel UDP 39 (1 host stall, 38 unexplained) | isolation + DPDK is the only mode with a clean run at this threshold |
| 2026-10-18 | round 11, `localhost` | **skipped**: "no fifth core on VM a" | the matrix's localhost path is **not measured** this round. Run it on a larger pair or with a fifth core |
| 2026-10-18 | round 11, `isolated-nic-irqs` | grew in 0 of 920 runs | IRQ pinning held every time. The check on the run passed |
