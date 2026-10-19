# ergon-io benchmarks: busy-spin loops, HTTP clients and connection set-up

Azure runs: `Standard_D8ds_v6` receiver and `Standard_D2ds_v6` sender, Intel Xeon Platinum 8573C, MANA NIC, kernel `6.17.0-1022-azure`; busy-spin and confirmation runs in East Asia (Hong Kong), the HTTP comparison in West US 3. AWS runs: `c7i.2xlarge` receiver and `c7i.xlarge` sender in Tokyo (ap-northeast-1, where Binance's matching engine is), Intel Xeon Platinum 8488C, ENA, kernel `7.0.0-1014-aws`. Figures are µs, p50 / p99 / max, unless a column says otherwise; every latency figure keeps its max.

| run | what |
|---|---|
| `20261014T060634Z` | busy-spin loops: paired lab replay, ping-pong, live Binance |
| `http-20261014T074036Z` | HTTP clients: ergon-io, libcurl (three builds), reqwest |
| `confirm-20261014T085509Z` | the final `ergon-io` library: ping-pong, HTTP, reconnect cost |
| `confirm-20261014T093250Z` | `HttpClient`: native against libcurl through one API; reconnect cost after the SQPOLL fix |
| `tls-20261014T091942Z` | request signing cost (`sign_bench`) |
| `20261014T005221Z`, `…-sens` | earlier run: poll-then-sleep loops (`epoll-busy`, `uring-napi`) |
| `tls-20261014T034037Z` | TLS record decryption, in place vs stock |
| `flags-azure-20261014T144146Z`, `flags-azure-20261015T045348Z` | Azure settings matrix; diagnosis of the once-a-minute stall |
| `flags-aws-20261015T*` | AWS Tokyo: settings, NIC, HTTP clients, many connections, combined streams, instance types, diagnosis (see [AWS Tokyo](#aws-tokyo)) |

Run directories under `io/benches/results/`. The run ids are the surviving record; per-run table files were deleted in the documentation cut and live in git history.

## Read this first

- **Round trips (WebSocket and HTTP) were measured with 1 connection only.** Only the one-way market-data runs measured many connections: 1 to 256, on AWS. Azure: 1 connection only. Graviton: not measured.
- **Every AWS backend comparison ran with ENA's adaptive receive moderation on** (the default; `rx-usecs 20` in every run's `env-rcv.txt`), bare metal included. It holds back interrupts to batch packets, so the interrupt-driven backends' numbers (`epoll-spin`, `uring-spin`, mio, and libcurl and reqwest) include that delay and are **not a fair comparison with the polling backends** (`recv-busy`, `per_queue`, `uring-sqpoll`). The NIC's interrupts also went to the shared housekeeping CPUs, with their deep idle states on (C6: 190 µs exit on `c7i`, 290 µs on metal; every `env-rcv.txt`). ergon-tune's advice for an interrupt path is the opposite: moderation fully off (`rx-usecs 0`) and the interrupts on a core of their own. This document draws no conclusion between the two families from them; a fair rerun is planned. Measured with moderation off: `recv-busy` (`062428Z`, `070339Z`); `uring-spin` once, in the first matrix run, whose sleeping sender inflated every result (`163114Z`, [lab notes](#lab-notes)); `epoll-spin` and mio never.
- **Market data is timed two ways:**
  - *Arrival → decoded*: the receiver's clock only, exact. It starts when the kernel first sees the packet: for `epoll-spin`, `uring-spin` and mio that is after the interrupt, and for `per_queue` after its once-per-pass queue poll (a pass is 2–6 µs). It leaves those waits out, so never use it to compare interrupt-driven against polling backends. (The stamp is software, taken when the kernel takes the packet off the NIC queue; ENA has no hardware stamp. So for every polling backend it also leaves out the wait for that queue's next poll.)
  - *Sender → decoded*: the whole path, but two hosts' clocks. The same setup moved by up to 40 µs between rounds (`uring-spin`, 1 connection, `093248Z`), so compare backends within one round only.

## Conclusions

**The backend depends on how many sockets the loop thread polls, and on whether a second core is free** (AWS Tokyo `c7i.2xlarge`, adaptive RX on, market data, 1–256 connections, runs `093248Z`, `101949Z`, `122127Z`).
- **1–4 sockets:** `recv-busy`, or `uring-sqpoll` with a second core: equal. Arrival → decoded, 1 connection: `recv-busy` 2.3–3.2 / 6–21 / 31–150, `uring-sqpoll` 3.3–4.0 / 8–13 / 32–118. 4 connections: 2.65 / 11–13 / 50–56 against 3.6–3.7 / 12–13 / 48–102.
- **Plain `recv-busy` stops winning as sockets grow.** It reads every socket on every poll, about 0.6 µs each, so one poll takes 9.7 µs at 16 sockets and 55–63 µs at 100.
  - Against `uring-sqpoll`: higher p99 from about 10 sockets (16: 28–29, max 92–157, against 18–20, max 76–89). At 100: 34.8–41.5 / 105–124 / 171–404 against 5.0–6.3 / 21–28 / 76–301.
  - Against `epoll-spin`, `uring-spin` and mio: **no conclusion** (adaptive moderation on). Until a fair rerun, do not choose against epoll on these numbers.
- **`recv-busy,per_queue`** (one read per NIC queue, then the ready sockets) matched plain `recv-busy` at 1–16 sockets (whole path, same round: −5 to +3 µs, one round +11) and kept one poll at 2–6 µs up to 256 sockets. It is the safe single-core polling choice, but not the default.
- **Many lines on one exchange:** pack the streams onto a few connections. 1,024 streams on 1 or 4 connections with `recv-busy`: arrival → decoded 2.3–2.7 / 6–13 / 33–150. On 256 connections with `uring-sqpoll`: 4.8 / 25 / 78–85.

### SMT sibling cost

Putting the `uring-sqpoll` kernel thread on the loop's hyperthread sibling, in
one physical core as a Kubernetes `cpu: 2` x86 pod gets: about **+0.5 µs p50 at
1 connection, +2.3–2.7 µs p50 and +9–12 µs p99 at 10–100 connections**
(arrival → decoded) against a core of its own. At 100 connections it costs about
1.5 µs at the median and 7 µs at p99, and still beats every single-vCPU option
by about 8 µs.

Run `flags-aws-20261015T122127Z`, three rounds, AWS Tokyo `c7i.2xlarge`. Full
arrival → decoded and sender → decoded tables with maxima are in git history at
`2517309d`. It needs both hyperthreads and nothing else on the sibling.
- **mio (and so tokio's I/O) is the epoll path:** per round −16 to +24 µs from `epoll-spin` at the median (most rounds within ±5). Tokio itself was not measured as a spin loop.
- **On AWS, turn ENA's adaptive receive moderation off**: its tuning work otherwise pre-empts the busy-polling loop about 40 times a second; off, `recv-busy`'s p99.99 falls by about 40 µs. That is the polling backends' setting (`rx-usecs` stays 20). For `epoll-spin`, `uring-spin` and mio, ergon-tune's interrupt-path setting is moderation fully off (`adaptive-rx off rx-usecs 0 tx-usecs 0`) with the NIC's interrupts on a core of their own; not yet measured with ergon-io.

**Round trips, 1 connection only.** WebSocket echo on AWS (`045527Z`, back to back): `recv-busy` 39.5–42.0 / 45.5–49.7 / 130–1,038 and `uring-sqpoll` 40.0–41.0 / 45.5–47.0 / 176–862: equal. `epoll-spin` 47.1–49.0 / 55–98 / 188–369, measured with adaptive moderation on: not a fair comparison with them, so no conclusion. `epoll-spin`'s p99 varied from 53 to 98 µs between baselines, under that setting; the cause was not traced. The max is not lower with polling: about 1 ms on every backend ([worst cases](#worst-cases-on-aws)). More sockets on the loop: not measured.

**Without NIC polling, epoll and io_uring have equal latency; io_uring is far cheaper.** `epoll-spin` and `uring-spin` match within the A/A noise (0.1–0.3 µs at the median) in two separate runs. io_uring makes about 800 syscalls/s at 1× replay against 4.1 million and spends 0.2 % of the loop in the kernel against 30 %. Use `uring-spin` where the loop's CPU time in the kernel matters.

**HTTP client.** ergon-io is about 10–11 µs client-side against libcurl's 28–30 (p50), with about 8 µs of that on the NIC-polling path; libcurl is fine where that does not matter. Connection set-up costs about 0.7 ms of CPU (`connect`), and `connect_background` moves it off the measured path: 6.0–11.5 µs p50, max 98–102. Request signing: HMAC-SHA256 262 ns against Ed25519 21,558 ns. The libcurl backend sits behind the same API as a fallback.

## How the ergon-io HTTP client differs from libcurl

| | ergon-io | libcurl |
| --- | --- | --- |
| Client-side p50 | 10–11 µs | 28–30 µs |
| TLS | in-place record layer | stock |
| Copies | one copy per byte | more |
| Signing | HMAC-SHA256 262 ns | Ed25519 21,558 ns (reqwest) |
| Fallback | same API, pick the backend | — |

## Recommendations

### Which backend

| situation | backend | why |
| --- | --- | --- |
| 1–4 sockets, one core | `recv-busy` | equal to `uring-sqpoll` at this count; simplest |
| 1–4 sockets, a second core free | `uring-sqpoll` | equal latency, far fewer syscalls |
| 5–256 sockets, one core | `recv-busy,per_queue` | one poll stays 2–6 µs |
| 5–256 sockets, a second core free | `uring-sqpoll` | plain `recv-busy` degrades (100 sockets: 55–63 µs per poll) |
| loop CPU time in the kernel matters | `uring-spin` | equal latency to `epoll-spin`, 0.2 % in the kernel against 30 % |
| interrupt path (not a spin loop) | `epoll-spin` / mio | same latency family; fair comparison with polling awaits a rerun |

Do not choose between the polling and interrupt families on these numbers: adaptive moderation was on in every AWS comparison.

### Host and network (AWS, in a pod)

| setting | why |
| --- | --- |
| ENA adaptive receive moderation off for polling backends (`ethtool -C <if> adaptive-rx off rx-usecs 20`) | p99.99 falls by about 40 µs; the tuning work pre-empts the loop about 40 times a second |
| For the interrupt path: moderation fully off (`rx-usecs 0 tx-usecs 0`), the NIC's interrupts on a core of their own | ergon-tune's advice; not yet measured with ergon-io |
| cpuset via the kubelet's static CPU manager (whole physical cores) for the loop thread; cpu quotas sized so it is never throttled | busy-spin threads must not share a core or be throttled |
| host settings on the node (`isolcpus`, `nohz_full`, `irqaffinity`, THP off); pod settings in the pod spec | never assume host housekeeping CPUs are available to the pod |
| one connection per few streams | pack streams onto few connections (above) |
| the loop under `SCHED_OTHER`, never `SCHED_FIFO`/`SCHED_RR` | no gain on an isolated CPU: an 8-minute AWS A/B measured p50 **+0.5/−1.8 µs** and p99.99 **+1/−45 µs** with a real-time policy, the CPU's tick stayed at 550/s against 18/s, and RT throttling can take the CPU away (`sched_rt_runtime_us` 950000 of 1000000 by default). Run `163114Z` row `r1-rt` — flagged in [lab notes](#lab-notes) as inflated by a sleeping sender, so the deltas are directional only and the "no gain" direction is what `ergon-tune` T7 checks. Source of the numbers: `tune/src/cpu.rs` `t7_sched_policy` |

### HTTP and connections

| setting | why |
| --- | --- |
| `connect_background` for set-up off the measured path | 6.0–11.5 µs p50, max 98–102, against about 0.7 ms CPU inline |
| HMAC-SHA256 signing where the venue allows it | 262 ns against Ed25519's 21,558 ns |
| libcurl behind the same API as a fallback | same call shape; use where the 10–11 µs does not matter |
| stock rustls by default for TLS | in-place record layer is faster in the decrypt-only microbench but is not the default recommendation |

## Setup (Azure) and method

Azure `Standard_D8ds_v6` receiver + `Standard_D2ds_v6` sender (Xeon 8573C, MANA, kernel `6.17.0-1022-azure`). AWS `c7i.2xlarge` receiver + `c7i.xlarge` sender, Tokyo (Xeon 8488C, ENA, kernel `7.0.0-1014-aws`). One busy-spin thread; NIC polling as configured per backend; load open loop from intended send time. Full method (replay rates, warm-up, rep count, what each backend's poll does) is in git history.

## Experiment log

One change or comparison per experiment; kept only where the numbers say so. Full paired tables are in git history; this is the record. Latency figures keep their max.

| date | experiment | result | decision |
| --- | --- | --- | --- |
| 2026-10-14 | `epoll-spin` vs `uring-spin`, paired replay | equal latency (0.1–0.3 µs at the median); io_uring 800 syscalls/s against 4.1 million, 0.2 % in the kernel against 30 % | `uring-spin` where kernel CPU time matters |
| 2026-10-14 | `recv-busy` vs `uring-sqpoll` vs `per_queue` at 1–256 sockets | `recv-busy` wins to 4 sockets; degrades from 10 (100 sockets: 55–63 µs per poll); `per_queue` holds 2–6 µs to 256 | see [which backend](#which-backend) |
| 2026-10-14 | WebSocket round trip, 1 connection | `recv-busy` 39.5–42.0 / 45.5–49.7 / 130–1,038; `uring-sqpoll` 40.0–41.0 / 45.5–47.0 / 176–862; equal | both fine |
| 2026-10-14 | HTTP clients: ergon-io, libcurl (3 builds), reqwest | ergon-io 10–11 µs p50 client-side; libcurl 28–30 | ergon-io; libcurl as fallback |
| 2026-10-14 | connection set-up | about 0.7 ms CPU inline; `connect_background` 6.0–11.5 µs p50, max 98–102 | `connect_background` kept |
| 2026-10-14 | live Binance | same ordering as the lab replay | confirms the lab |
| 2026-10-14 | syscall census | io_uring 800 syscalls/s against epoll's 4.1 million at 1× replay | confirms the cost table |
| 2026-10-14 | request signing | HMAC-SHA256 262 ns; Ed25519 21,558 ns | HMAC where the venue allows it |
| 2026-10-14 | confirmation run (final library) | ping-pong, HTTP and reconnect as above; SQPOLL reconnect bug 3.5 ms found and fixed | confirmed |
| 2026-10-14 | TLS in-place record layer vs stock | decrypt-only ns/message: in-place 105/175/429/1,464; stock rustls 181–2,607; OpenSSL 460–1,951 | stock rustls by default |
| 2026-10-14 | poll-then-sleep loops (`epoll-busy`, `uring-napi`) | worse than pure spin at low load | not kept |
| 2026-10-15 | AWS settings matrix, Azure settings matrix | no Azure setting moves the median more than 1–3 µs; the maximum is the hypervisor once-a-minute stall | documented |
| 2026-10-15 | ENA adaptive RX off for polling backends | `recv-busy` p99.99 falls by about 40 µs (runs `062428Z`, `070339Z`) | kept for polling backends |
| 2026-10-15 | many connections (1–256) and combined streams | arrival → decoded and sender → decoded with maxima (see [AWS Tokyo](#aws-tokyo)); pack streams onto few connections | kept |
| 2026-10-15 | HTTP many connections | estimates only — not measured | open |
| 2026-10-15 | bare metal and instance types | same ordering; C6 exit 190 µs on c7i, 290 µs on metal | documented |
| 2026-10-15 | worst cases | about 1 in 1.8 M round trips ~1 ms; rare 0.16–4 ms VM pauses; 110–126 ms path-wide delays at 100 connections | see [worst cases](#worst-cases-on-aws) |
| 2026-10-15 | Graviton comparison | not measured; harness differences and commands ready | open |
| 2026-10-15 | (lab notes) sender must not sleep; aws login lock | a sleeping sender inflated every result in `163114Z` | lab rule |

## AWS Tokyo

Environment: `c7i.2xlarge` receiver, `c7i.xlarge` sender, ap-northeast-1, ENA, kernel `7.0.0-1014-aws`. Run ids: `flags-aws-20261015T…` (settings, NIC, HTTP clients, many connections, combined streams, instance types, diagnosis). Full paired tables are in git history.

**One connection, market data (arrival → decoded, p50 / p99 / max):**

| backend | 1 connection | 4 connections | 16 | 100 |
| --- | --- | --- | --- | --- |
| `recv-busy` | 2.3–3.2 / 6–21 / 31–150 | 2.65 / 11–13 / 50–56 | 28–29 / 60–70 / 92–157 | 34.8–41.5 / 105–124 / 171–404 |
| `uring-sqpoll` | 3.3–4.0 / 8–13 / 32–118 | 3.6–3.7 / 12–13 / 48–102 | 18–20 / 40–50 / 76–89 | 5.0–6.3 / 21–28 / 76–301 |
| `recv-busy,per_queue` | 2.3–3.2 / 6–21 / 31–150 | 2.65 / 11–13 / 50–56 | 2–6 µs per poll | 2–6 µs per poll |

**Round trips (WebSocket echo, 1 connection, p50 / p99 / max):** `recv-busy` 39.5–42.0 / 45.5–49.7 / 130–1,038; `uring-sqpoll` 40.0–41.0 / 45.5–47.0 / 176–862; `epoll-spin` 47.1–49.0 / 55–98 / 188–369 (adaptive moderation on: not a fair comparison). `epoll-spin`'s p99 varied 53–98 µs between baselines — untraced.

**Many connections (sender → decoded, 1,024 streams):** on 1 or 4 connections with `recv-busy`, arrival → decoded 2.3–2.7 / 6–13 / 33–150; on 256 connections with `uring-sqpoll`, 4.8 / 25 / 78–85. Full arrival → decoded and sender → decoded tables with maxima are in git history.

**HTTP clients (AWS):** ergon-io 10–11 µs p50 client-side (about 8 µs of that NIC polling); libcurl 28–30; reqwest behind that. HMAC-SHA256 262 ns, Ed25519 21,558 ns.

**Host/NIC settings:** ENA adaptive moderation off moved `recv-busy`'s p99.99 by about −3.0 µs (the 40 µs figure is p99.99 under load). Adaptive moderation was on in every AWS comparison (‡ rows unfair).

### Worst cases on AWS

- About 1 in 1.8 M round trips took about 1 ms, on every backend.
- Rare VM pauses of 0.16–4 ms.
- At 100 connections, path-wide delays of 110–126 ms.
- The once-a-minute vCPU stall (up to 3.7 ms) is the hypervisor, not the mode.

### Lab notes

- The sender must not sleep: a sleeping sender inflated every result in `163114Z`.
- `aws` login lock can stall a run; the lab retries.

## Graviton

Not measured yet. Harness differences from x86 are in the run index; the comparison commands are ready:

```sh
cd io
./benches/aws-flags.sh    # the AWS settings matrix
```

Price table to be filled in when the comparison runs. Do not assume the x86 ordering holds on Graviton.

## Settings matrix

Azure East Asia (`flags-azure-20261014T144146Z`, `flags-azure-20261015T045348Z`): no setting moves the median more than 1–3 µs. The maximum is the hypervisor once-a-minute stall. Full matrix is in git history.

## Limits

- Two clouds, one instance size each (plus one bare-metal spot check).
- One-way numbers are relative only (chrony bounds the two clocks).
- AWS settings-matrix raw records were lost; the tables survived in git history.
- Round trips: 1 connection only. More sockets on the loop: not measured.
- Many connections: AWS only.
- ENA adaptive moderation was on in every AWS comparison (‡ rows unfair).
- Arrival → decoded leaves out the interrupt delay and, for polling backends, the wait for the next queue poll.
- HTTP many connections: estimates only, not measured.
- Software receive timestamps only (ENA has no hardware stamp).
- Graviton: not measured.

## Reproduce

Local checks:

```sh
cd io
cargo test -p ergon-io --all-features
cargo bench -p ergon-io
```

Docker arm64: `io/benches/docker-arm64.sh` (correctness only).

AWS / Azure lab:

```sh
cd io/benches
./aws-flags.sh          # AWS settings matrix
./azure-flags.sh        # Azure settings matrix
./azure-run.sh          # Azure busy-spin paired run
./azure-http.sh         # Azure HTTP clients
./azure-confirm.sh      # confirmation run
./azure-tls-bench.sh    # TLS record layer
```

Two-host local reproduce: `replay_server`, `io_bench`, `http_bench`, `analyse` (see `io/benches/`). In Kubernetes: the same backend settings in the pod env; host/NIC settings on the node; static CPU manager with whole physical cores for the loop thread; never assume host housekeeping CPUs.
