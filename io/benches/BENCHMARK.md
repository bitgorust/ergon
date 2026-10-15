# ergon-io benchmarks: busy-spin loops, HTTP clients and connection set-up

Azure runs: `Standard_D8ds_v6` receiver and `Standard_D2ds_v6` sender, Intel Xeon Platinum 8573C, MANA NIC, kernel `6.17.0-1022-azure`; busy-spin and confirmation runs in East Asia (Hong Kong), the HTTP comparison in West US 3. AWS runs: `c7i.2xlarge` receiver and `c7i.xlarge` sender in Tokyo (ap-northeast-1, where Binance's matching engine is), Intel Xeon Platinum 8488C, ENA, kernel `7.0.0-1014-aws` ([AWS Tokyo](#aws-tokyo)).

| run | what | tables |
|---|---|---|
| `20261014T060634Z` | busy-spin loops: paired lab replay, ping-pong, live Binance | [`results/20261014T060634Z/tables.md`](results/20261014T060634Z/tables.md) |
| `http-20261014T074036Z` | HTTP clients: ergon-io, libcurl (three builds), reqwest | [`results/http-20261014T074036Z/tables.md`](results/http-20261014T074036Z/tables.md) |
| `confirm-20261014T085509Z` | the final `ergon-io` library: ping-pong, HTTP, reconnect cost | [`results/confirm-20261014T085509Z/tables.md`](results/confirm-20261014T085509Z/tables.md) |
| `confirm-20261014T093250Z` | `HttpClient`: native against libcurl through one API; reconnect cost after the SQPOLL fix | [`results/confirm-20261014T093250Z/tables.md`](results/confirm-20261014T093250Z/tables.md) |
| `tls-20261014T091942Z` | request signing cost (`sign_bench`) | [`results/tls-20261014T091942Z/sign_bench.md`](results/tls-20261014T091942Z/sign_bench.md) |
| `20261014T005221Z`, `…-sens` | earlier run: poll-then-sleep loops (`epoll-busy`, `uring-napi`) | [`results/20261014T005221Z/tables.md`](results/20261014T005221Z/tables.md) |
| `tls-20261014T034037Z` | TLS record decryption, in place vs stock | [`results/tls-20261014T034037Z/tls_bench.md`](results/tls-20261014T034037Z/tls_bench.md) |
| `flags-azure-20261014T144146Z`, `flags-azure-20261015T045348Z` | Azure settings matrix; diagnosis of the once-a-minute stall | [`tables-local.md`](results/flags-azure-20261014T144146Z/tables-local.md) |
| `flags-aws-20261015T*` | AWS Tokyo: settings, NIC, HTTP clients, many connections, combined streams, instance types, diagnosis | each run's `tables.md` / `tables-local.md` ([AWS Tokyo](#aws-tokyo)) |

## Conclusions

**For a busy-spin loop that must never sleep, poll the NIC; which way depends on how many sockets the loop serves** (AWS Tokyo and Azure; [recommendations](#which-backend)).
- **A few sockets per loop:** `recv-busy`, or `uring-sqpoll` if a core can be spared; the two are equal. Against the interrupt-driven loops, NIC polling decodes the same market-data message 6–11 µs earlier at the median and removes the interrupt path's tails (round-trip p99 on AWS 45–50 against 55–98 µs).
- **Many sockets per loop:** `uring-sqpoll`, whose kernel thread polls every NIC queue once per pass: at 100 connections 29 µs one way at the median against 36–41 µs for `epoll-spin`, mio and `recv-busy` with `per_queue`, and 75 µs for plain `recv-busy`, which reads every socket every poll. With one physical core (a pod with two vCPUs), put the SQPOLL thread on the loop's hyperthread sibling: 30.6 µs at 100 connections, about 1.5 µs behind a core of its own. With a single vCPU, `recv-busy` with `per_queue` or `epoll-spin`.
- **Many lines on one exchange:** fewer, busier connections win. 1,024 streams on 1–4 combined-stream connections with `recv-busy` decode in 10–27 µs at the median, against 29–31 µs on 256 connections with `uring-sqpoll`, and need no extra core.
- **mio (and tokio, built on it) is the epoll path**: within about 2 µs of `epoll-spin`, never ahead of the polling backends.
- **On AWS, turn ENA's adaptive receive moderation off**: its tuning work otherwise pre-empts the busy-polling loop about 40 times a second; off, p99.99 falls by about 40 µs.

**Without NIC polling, epoll and io_uring have equal latency; io_uring is far cheaper.** `epoll-spin` and `uring-spin` match within the A/A noise (0.1–0.3 µs at the median) in two separate runs. io_uring makes about 800 syscalls/s at 1× replay against 4.1 million and spends 0.2 % of the loop in the kernel against 30 %. Use `uring-spin` for loops where CPU matters more than the last microseconds.

**`epoll_wait(0)` never blocks, but it never polls the NIC either**, and io_uring's NAPI polling only happens inside a timed wait: `uring-napi` with a zero wait slept 72,000–253,000 times per 30 s run, with stalls up to 7.5 ms. The poll-then-sleep loops (`epoll-busy`, `uring-napi`) suit threads that may nap; for those, io_uring's NAPI path beats epoll's busy poll (see [Poll-then-sleep loops](#poll-then-sleep-loops-not-for-busy-spin)).

**HTTP: the ergon-io client is 17–26 µs faster per round trip than every libcurl build** (distro shared library, curl-sys static, and a from-source build with c-ares and `-O3 -march=native`), and about 26 µs faster than reqwest on tokio. Among libcurl builds the source build is 1–6 µs faster; distro and static are equal. Never sleeping: ergon-io and libcurl driven by `curl_multi_perform`; reqwest sleeps once per request.

**Connecting costs the loop about 0.7 ms of CPU** for a full TLS handshake (about 0.1 ms when the session resumes). `Driver::connect` is non-blocking but spends that in one poll; `Driver::connect_background` runs DNS, TCP, TLS and the upgrade on a helper thread on housekeeping CPUs and sets socket options there, so a reconnect costs the loop 6–12 µs at the median (see [Connection set-up](#5-connection-set-up) and the [HttpClient run](#httpclient-native-against-libcurl)).

**A libcurl fallback is available behind the same API** (`HttpClient`, `HttpImpl::Curl`, feature `curl`): 16–21 µs slower than native per request at 1,000 req/s, so keep it for endpoints that need libcurl's extra features.

## How the ergon-io HTTP client differs from libcurl, and why it is faster

The gap has two parts, and the benchmark separates them. At 1,000 req/s, libcurl (all three builds) is at 78–80 µs p50; ergon-io on the *same* interrupt-driven NIC path (`epoll-spin` / `uring-spin`) is at 68–70 µs; ergon-io polling the NIC (`recv-busy` / `uring-sqpoll`) is at 60–63 µs. So about **10–11 µs is client-side software** and about **8 µs is NIC polling**, which libcurl's model does not do.

| | ergon-io HTTP client | libcurl (multi interface) |
|---|---|---|
| Who drives I/O | the application's loop calls `driver.poll()`; nothing runs otherwise | `curl_multi_perform` runs curl's state machine for every handle on each call |
| Per request | `request()` writes the request line, the connection's pre-serialised fixed headers and the body straight into one TLS record: one `send` | a transfer walks curl's state machine: connection-pool lookup and liveness check, header assembly from the header list into a growable buffer, `Expect`/auth handling, completion, handle removal |
| Header formatting | fixed headers built once per connection | rebuilt per transfer |
| TLS send | encrypted in place in the send queue | OpenSSL `SSL_write` copies into its record buffer, encrypts, writes through a BIO |
| TLS receive | kernel → one buffer; record decrypted in place | kernel → OpenSSL read buffer → decrypt → copy into curl's buffer |
| Response | `httparse` in place; the body handed to the application is a slice of the receive buffer | header lines parsed, then body bytes copied out through a write callback |
| Allocations per request | none in steady state | several (header buffers, transfer state) |
| Syscalls per request | 1 (`uring-sqpoll`) to 4 (`uring-spin`), measured | `send` + `recv`, `EAGAIN` reads, and a liveness check before reusing a pooled connection |
| NIC polling | `recv-busy` / `uring-sqpoll` poll the NIC from the loop | plain non-blocking `recv`, interrupt-driven |
| Threads, blocking | none; never sleeps; connect can run off the loop | never slept when driven by `curl_multi_perform`; DNS uses resolver threads unless built with c-ares |

Why that makes it faster:
1. **Less work per request** (most of the ~10 µs): libcurl is general (redirects, auth schemes, proxies, cookies, compression, HTTP/2, many protocols), and even unused features shape its per-transfer path. For a fixed endpoint, ergon-io writes bytes and parses one response. This split is inferred from the two designs, not from a profile of libcurl.
2. **Fewer copies and allocations:** one copy per byte and no allocation, against three or more copies and several allocations. For a 300-byte response this is a small share; TLS decryption itself differs by a few hundred nanoseconds (in place 105 ns against OpenSSL 460 ns at 200 B, see [TLS](#tls-in-place-record-layer-vs-stock-implementations)).
3. **NIC polling** (~8 µs): the response is picked up from the NIC by the loop instead of waiting for the interrupt path. libcurl could get part of this by setting `SO_BUSY_POLL` in a `CURLOPT_SOCKOPTFUNCTION` callback, since it reads non-blocking on each `perform`; not tested.

What libcurl offers that ergon-io's client does not: HTTP/2 and HTTP/3, proxies, redirects, authentication schemes, cookies, compression, close-delimited bodies, decades of edge-case fixes and a choice of TLS backends. ergon-io's client is deliberately narrow: HTTP/1.1 keep-alive to known endpoints, `Content-Length` or chunked bodies, one request in flight per connection by default.

## Recommendations

### Which backend

What decides it is **how many sockets one polling loop serves** and **whether it may have a second core**. Measured on AWS `c7i` (the [AWS sections](#aws-tokyo)), consistent with Azure.

| situation | use | measured (one way, sender → decoded, p50 / p99, µs) |
|---|---|---|
| one loop, a few sockets (up to about 10), one core | **`recv-busy`** | 1 socket: 21.7 / 57; 10: 34.6 / 88 |
| many lines on one exchange | **pack the streams onto a few combined-stream connections (about 4), `recv-busy`** | 1,024 streams on 1–4 connections: 10–27 / 25–70 (the fastest tested) |
| many sockets per loop (tens to hundreds), a second core to spare | **`uring-sqpoll`** | 100 sockets: 29.3 / 90; 256: 28.6–30.8 / 94–96 |
| many sockets per loop, one physical core (two vCPUs, e.g. a pod) | **`uring-sqpoll` with the SQPOLL thread on the loop's hyperthread sibling** (`sqpoll_cpu` = the sibling) | 100 sockets: 30.6 / 95 |
| many sockets per loop, a single vCPU | **`recv-busy` with `per_queue`**, or `epoll-spin` | 100 sockets: 39.1–41.3 / 101–107 and 36.3–38.4 / 111 |
| CPU matters more than the last microseconds | `uring-spin` | interrupt-driven like `epoll-spin`, about 1,000× fewer syscalls |
| a thread that may nap | `uring-napi` (or `epoll-busy`) | polls the NIC only inside a timed wait |

| backend | for | against |
|---|---|---|
| `recv-busy` | polls the NIC from the loop's own core: no interrupt, no hand-off, best or equal best up to about 10 sockets; one core | one `recvmsg` per socket per poll, so its cost grows with sockets (100 sockets: 67 µs per poll, worst of all); about 65–80 % of its core in the kernel |
| `recv-busy`, `per_queue` | one read per NIC queue plus the ready sockets: keeps `recv-busy`'s latency up to 10 sockets and scales to hundreds on one core | one `epoll_wait` per poll; behind `uring-sqpoll` with many sockets (41 against 29 µs at 100) |
| `uring-sqpoll` | the kernel thread polls every NIC queue and completes every socket into one ring, in parallel with the loop: best from 10 sockets up; no syscalls on the loop thread | a spinning kernel thread per ring: a second core, or the loop's hyperthread sibling at a cost of about 1.5 µs (rings in one process can share one thread, other processes cannot); 1–7 µs behind `recv-busy` at the median, and up to twice its p99, with one to four very busy sockets |
| `epoll-spin` (and mio, which is epoll) | cheap per socket, scales like a readiness API should | waits for the NIC interrupt: 6–30 µs slower than polling at the median with few sockets, up to 3× the p99 |
| `uring-spin` | the fewest syscalls of any spin loop | interrupt-driven, as `epoll-spin` |

### Host and network (AWS)

- **Instance:** `c7i` (Sapphire Rapids). `c7a` was 5 µs slower at the median, `r7iz` no better; `c7i.4xlarge` matches `c7i.2xlarge` with a quieter core in two baselines.
- **NIC:** turn ENA's adaptive receive moderation off, `ethtool -C <nic> adaptive-rx off`: p99.99 −40 µs and 4–8× fewer round trips over 100 µs. With it on, the driver's tuning work pre-empts the busy-polling loop about 40 times a second.
- **CPUs:** isolate the loop's core (`isolcpus`, `nohz_full`, `rcu_nocbs`, interrupts on housekeeping cores), put the loop on the quietest isolated core (a short spinner on each finds it), and leave its hyperthread sibling idle. On AWS the boot profile moves the median by less than 2 µs; what it buys is the worst case (the stock boot had a 5.2 ms maximum).
- **TCP:** routes with `quickack 1 rto_min 5ms`.
- **Not worth it on AWS:** NIC interrupts on the loop's CPU, one NIC queue, capping the receiver's idle states (2 µs slower), preferred busy polling, `SCHED_FIFO`, TCP autocorking off.

### HTTP, connections and configuration

**Order entry over HTTP:**
- Use ergon-io's `HttpConn`, not libcurl, on the latency path.
- Keep a small pool of warm connections, one request in flight each; send each order on an idle connection (`in_flight() == 0`). A timed-out request closes its connection.
- Sign with **HMAC-SHA256** (`Signer::hmac_sha256`, built once at start-up) and `HttpConn::signed_request`: the timestamp and signature are written into the request's TLS record without allocating. Ed25519 costs about 80× more per request on x86, 21.6 µs against 0.26 µs (see [Request signing](#8-request-signing)); keep it for once-per-session uses such as a WebSocket API log-on, where the private key never leaving the machine is worth it.
- Set the API key once as a connection header (`Target::with_header("X-MBX-APIKEY", key)`).
- Consider the exchange's WebSocket order API for many requests over one connection.

**Connections:**
- Open replacements with `Driver::connect_background`, so the loop never spends the ~0.7 ms of handshake CPU; prefetch DNS at start-up (`Driver::prefetch`) or pin addresses with `Target::with_addr`.
- Reuse one `rustls::ClientConfig` (it keeps session tickets, so reconnects can resume).

**Choosing implementations: config, with features gating what is compiled in.**
- The loop backend is chosen at run time (`Config::parse("recv-busy")`); the `uring` Cargo feature decides whether io_uring is built at all. Run-time selection costs one predictable branch per `poll`, below a nanosecond; a feature per backend would only remove that branch and would need a rebuild to switch.
- The signer is chosen the same way (`Signer` is an enum); the signature itself dominates its cost.
- The HTTP implementation is chosen the same way: `HttpClient` with `HttpImpl::Native` (default) or `HttpImpl::Curl`, the latter compiled in only with the `curl` feature. Use native on the order path; switch an endpoint to libcurl by configuration when it needs libcurl's extras (redirects, proxies, HTTP/2, other authentication schemes, compression). Application code does not change.
- If libcurl is used anyway: build it from source with c-ares, drive it with `curl_multi_socket_action` or `curl_multi_perform` (never `curl_multi_wait`/`curl_multi_poll`), set an empty `Expect:` header and pin addresses with `CURLOPT_RESOLVE`.

## Setup (Azure)

| | |
|---|---|
| Receiver | `Standard_D8ds_v6`, Intel Xeon Platinum 8573C (Emerald Rapids), 4 cores / 8 threads, MANA NIC (accelerated networking) |
| Sender | `Standard_D2ds_v6` in the same proximity placement group |
| Kernel | Ubuntu 24.04, `6.17.0-1022-azure`, clocksource `tsc` |
| CPU isolation | `isolcpus=managed_irq,domain,2-7 nohz_full=2-7 rcu_nocbs=2-7 irqaffinity=0,1`. Housekeeping and all IRQs on core 0 (CPUs 0,1). Loop A on CPU 2, loop B on CPU 4, the SQPOLL thread on CPU 6; hyperthread siblings idle |
| Network tuning | `ip route … quickack 1 rto_min 5ms`; global `net.core.busy_read` / `busy_poll` = 0; `SO_BUSY_POLL` = 0 except on the sockets of the loops that poll through the socket |
| Memory | `mlockall(MCL_CURRENT \| MCL_FUTURE)`; receive buffers pre-faulted |
| Clock reads | `clock_gettime(CLOCK_REALTIME)` 18.9 ns (vDSO), `rdtsc` 9.0 ns |
| TLS | TLS 1.3, AES-128-GCM |
| Binance | spot `stream.binance.com:9443` (`@trade` ×3, `timeUnit=MICROSECOND`); futures `fstream.binance.com` `/public` (`@bookTicker` ×2) and `/market` (`@aggTrade`). Reachable from Hong Kong; blocked (HTTP 451) from West US 3 |

## Method

### The loops

Everything above the socket (TLS, WebSocket, HTTP, the application) is shared; the `Driver` backend is the only variable.

| backend | how it waits | NIC polled | sleeps | syscalls while idle | use |
|---|---|---|---|---|---|
| `recv-busy` | non-blocking `recvmsg` on every socket every poll, `SO_BUSY_POLL` set: an empty read runs one NAPI pass, then `EAGAIN`; with `per_queue`, one read per NIC queue and then the sockets `epoll_wait(0)` reports ready | yes, on the loop core | never | 1 per socket (or queue) per poll | latency-critical busy spin, few sockets (`per_queue`: many) |
| `uring-sqpoll` | `SQPOLL` kernel thread on its own core submits, completes and polls the NIC (NAPI); the loop reads the completion ring | yes, on the extra core | never | 0 | latency-critical busy spin, +1 core |
| `uring-spin` | one `IORING_OP_RECVMSG` per socket; `SINGLE_ISSUER \| DEFER_TASKRUN \| TASKRUN_FLAG`; enters the kernel only when it flags work | no (interrupts) | never | 0 | busy spin where CPU matters |
| `epoll-spin` | `epoll_wait(0)` every poll, then `recvmsg` on ready sockets | no (interrupts) | never | 1 per poll | busy spin, baseline |
| `epoll-busy` | `epoll_pwait2(wait_us)` with `EPIOCSPARAMS` busy poll | yes, inside the wait | up to `wait_us` | 1 per wait | may nap |
| `uring-napi` | `io_uring_enter(GETEVENTS, 1, wait_us)` with NAPI | yes, inside the wait | yes, even with `wait_us = 0` | 1 per wait | may nap |

How these were established:
- The kernel paths were checked against the Linux 6.17 source: `tcp_recvmsg` with `MSG_DONTWAIT` and `SO_BUSY_POLL > 0` makes exactly one NAPI pass (`napi_busy_loop` with no loop-end callback); `ep_poll` with a zero timeout returns before `ep_busy_loop`; `io_cqring_wait` with a zero timeout still busy-polls for the whole NAPI budget and then `schedule()`s on an already-expired timer; the SQPOLL thread makes one NAPI pass per iteration and its idle window must be long, because NAPI passes do not count as work.
- Every loop counts its voluntary context switches (it slept) and its longest iteration; the busy-spin backends recorded 0 sleeps in every run.
- `prefer_busy_poll` is off everywhere. Polling is per NIC queue, so when the two loops of a pair share a queue one side's polling also delivers the other's packets; the analysis flags shared queues. In the runs quoted here the polled pairs did not share a queue.

### One copy per byte

- **TLS:** rustls 0.23 performs the handshake; the traffic keys are then taken with `dangerous_into_kernel_connection` and records are decrypted **in place** with ring in the one receive buffer the kernel wrote into. Session tickets and key updates still go through rustls's `KernelConnection`. See [TLS](#tls-in-place-record-layer-vs-stock-implementations).
- **WebSocket, HTTP and JSON:** frames and responses are parsed in place; a payload or `Content-Length` body is a slice of the receive buffer.
- **io_uring:** single-shot `RECVMSG` into the same buffer tail; a provided-buffer ring would need a copy to stitch TLS records that straddle buffers.
- **Measured copies** beyond kernel→buffer: 0 in the lab; 0.37–0.50 % of bytes on live Binance (WebSocket frames split across two TLS records).

### Timestamps

- **Arrival:** the kernel's software receive timestamp (`SO_TIMESTAMPING`). MANA's hardware timestamps are on the VF, not on the `eth0` (netvsc) device the sockets use.
- **Decoded:** `CLOCK_REALTIME` after decoding, through the vDSO, the same clock domain as the kernel timestamp.
- **Stack share** (`user − rx`) uses reads that delivered one message only (the kernel stamps a read with its newest segment). Interrupt-driven and polled loops take the arrival timestamp on different CPUs, so their stack shares are compared only as a diagnostic.

### Pairing, controls and traffic

- **Paired runs:** two loops run at the same time on their own isolated cores, each with its own connection, receiving the same messages. **Δuser** = decoded time of B − decoded time of A for the same message, on one clock, so the exchange's and the network's jitter cancel. Δrx is the arrival difference; Δstack = Δuser − Δrx on single-message reads.
- **Lab traffic:** the sender replays a 5-minute Binance capture at original speed (1×, about 800 msg/s) and as bursts of 50 messages every millisecond (50 k msg/s), writing each message to both connections back to back, alternating which goes first (the reported Δuser removes that offset).
- **Controls:** every pair runs in both core orders; A/A runs show the noise floor.
- **Live:** the same pairs on Binance. Each loop has its own Binance connection, so live Δuser contains Binance's per-connection send skew (hundreds of µs); only live Δstack is a loop comparison.
- **Order-path round trip:** a WebSocket echo server on the sender, one 64-byte message in flight, at 100, 1k and 10k msg/s and back to back; loops measured one after another.
- **HTTP:** `replay_server --mode http` answers every request with a 300-byte JSON body (the size of a Binance order acknowledgement), keep-alive, `Content-Length`. Every client sends the same `POST /api/v3/order` (form-encoded parameters, signature, `X-MBX-APIKEY`, `Content-Type`), one request in flight, 30 s per run, two rounds interleaved by client.
- **Warm-up:** the first 10 s of every lab and live run (1 s for round trips) are dropped.

## Results (Azure)

All values are in µs. In paired tables Δ is B − A, so negative means B decoded first; rows come in pairs (both core orders). Every latency column now carries its maximum too. These earlier runs logged no stalls and took no kernel trace, so their maxima (often several ms, on the polled and the interrupt-driven loops alike) are reported but not attributed; the [settings matrix](#settings-matrix) runs trace every loop and name the cause of each worst case.

### 1. Interrupt-driven: `epoll-spin` vs `uring-spin`

| traffic | A | B | Δuser p50 / p99 / p99.9 / max | stack A p50 / p99 / max | stack B p50 / p99 / max | A syscalls/s | B syscalls/s | A kernel % | B kernel % |
|---|---|---|---|---|---|---|---|---|---|
| 1× replay | epoll-spin | uring-spin | 0.28 / 24.75 / 53.41 / 1,413 | 8.55 / 57.78 / 4,962 | 8.43 / 56.25 / 4,960 | 4,134,654 | 808 | 30.1 | 0.2 |
| 1× replay | uring-spin | epoll-spin | −0.17 / 29.54 / 68.13 / 1,709 | 6.66 / 42.41 / 4,163 | 6.77 / 42.13 / 4,160 | 700 | 4,317,183 | 0.2 | 31.2 |
| 1× A/A | epoll-spin | epoll-spin | −0.12 / 30.45 / 70.92 / 9,081 | | | | | | |
| burst | epoll-spin | uring-spin | −0.65 / 25.52 / 43.35 / 5,828 | 18.55 / 60.54 / 6,060 | 18.76 / 60.42 / 6,049 | 3,679,619 | 54,390 | 38.6 | 12.3 |
| burst | uring-spin | epoll-spin | −0.10 / 24.43 / 46.52 / 6,211 | 17.58 / 59.24 / 6,217 | 17.10 / 58.95 / 6,639 | 61,332 | 3,540,137 | 14.6 | 38.7 |

- Equal latency: the differences sit inside the A/A spread, and the earlier run (`20261014T005221Z`) found the same (0.22 / −0.18 µs).
- io_uring removes over 99.9 % of the loop's syscalls at 1× and cuts its kernel share from about 30–39 % to 0.2–15 %.

### 2. NIC polling without sleeping: `recv-busy` vs `uring-spin` and `uring-sqpoll`

| traffic | A | B | Δuser p50 / p99 / p99.9 / max | Δrx p50 | A first | stack A p50 / p99 / max | stack B p50 / p99 / max |
|---|---|---|---|---|---|---|---|
| 1× replay | recv-busy | uring-spin | 10.76 / 466.54 / 2,678 / 8,180 | 7.98 | 91.5 % | 3.85 / 17.21 / 492 | 7.45 / 39.42 / 1,230 |
| 1× replay | uring-spin | recv-busy | −10.90 / 22.44 / 48.67 / 1,556 | −7.94 | 11.9 % | 8.42 / 50.16 / 4,331 | 4.16 / 21.86 / 382 |
| burst | recv-busy | uring-spin | 9.16 / 261.58 / 2,620 / 9,241 | 6.28 | 74.5 % | 4.83 / 23.18 / 6,016 | 10.22 / 28.42 / 5,337 |
| burst | uring-spin | recv-busy | −8.60 / 19.13 / 34.12 / 3,249 | −5.76 | 18.5 % | 7.23 / 23.03 / 6,354 | 3.26 / 21.40 / 9,510 |
| 1× replay | recv-busy | uring-sqpoll | 0.86 / 27.32 / 135.30 / 1,377 | −0.32 | 54.8 % | 3.76 / 16.75 / 386 | 4.72 / 17.53 / 421 |
| 1× replay | uring-sqpoll | recv-busy | −0.88 / 34.79 / 85.24 / 2,158 | −0.21 | 48.0 % | 4.49 / 23.59 / 1,158 | 3.62 / 22.77 / 780 |
| burst | recv-busy | uring-sqpoll | 5.62 / 59.78 / 252.50 / 9,271 | 3.36 | 63.5 % | 4.98 / 28.35 / 3,965 | 10.03 / 30.54 / 8,946 |
| burst | uring-sqpoll | recv-busy | −3.70 / 43.77 / 211.52 / 7,619 | −2.90 | 40.0 % | 9.35 / 30.05 / 9,317 | 7.90 / 26.09 / 6,127 |
| 1× A/A | recv-busy | recv-busy | 0.13 / 41.79 / 95.63 / 1,372 | | | | |

- **NIC polling decodes about 11 µs earlier** than the interrupt-driven loop at 1× replay, in both core orders, on 88–92 % of messages; about 9 µs under bursts. Most of it is before the stack: the polled socket's packets enter the kernel 8 µs earlier at the median and the interrupt path's arrival tail reaches 429 µs at p99 (Δrx). The polled loop processes its packets on its own core; the interrupt-driven loop's arrive on the IRQ core.
- **`recv-busy` and `uring-sqpoll` are equal** at 1× (±0.9 µs, inside the A/A spread); under bursts `recv-busy` is 4–6 µs ahead in both orders.
- **Cost:** `recv-busy` makes about 1.2 million syscalls/s and spends about 79 % of its core in the kernel; `uring-sqpoll` makes none on the loop thread, and its kernel thread occupies CPU 6 the whole time.

### 3. Order-path round trip (WebSocket echo, p50 / p99 / p99.9 / max)

Run `20261014T060634Z`:

| rate | recv-busy | uring-sqpoll | uring-spin | epoll-spin | uring-napi (wait 0) |
|---|---|---|---|---|---|
| 100/s | 64.9 / 99.2 / 135 / 291 | 60.8 / 93.6 / 118 / 208 | 79.6 / 261 / 2,689 / 7,195 | 78.7 / 630 / 3,023 / 6,545 | 65.7 / 106 / 287 / 1,183 |
| 1,000/s | 48.0 / 67.4 / 154 / 363 | 55.4 / 81.0 / 95 / 272 | 54.2 / 411 / 2,221 / 8,129 | 51.8 / 298 / 2,612 / 8,186 | 48.6 / 82.3 / 729 / 7,149 |
| 10,000/s | 45.5 / 61.1 / 78.5 / 1,101 | 47.0 / 62.5 / 79.7 / 1,214 | 51.3 / 81.2 / 371 / 12,161 | 49.6 / 79.8 / 347 / 6,615 | 47.1 / 66.2 / 90.4 / 4,605 |
| back to back | 44.6 / 64.6 / 72.3 / 3,592 | 44.8 / 63.5 / 73.6 / 4,994 | 48.9 / 73.6 / 159 / 6,204 | 59.3 / 82.2 / 257 / 8,246 | 48.0 / 69.7 / 112 / 6,959 |

Loop behaviour in those runs: 0 sleeps for the four busy-spin loops; `uring-napi` with a zero wait slept 72,339–253,016 times per 30 s and its longest iteration was 4.6–7.5 ms.

- **The interrupt path's tails differ between VMs.** In the earlier run (`20261014T005221Z`) and the confirmation run, on the same VM size and setup, `epoll-spin` and `uring-spin` had p99 71–74 µs at 1,000/s; in this run 298–411 µs, with p99.9 above 2 ms: one VM in three. The NIC-polling loops were tight on all three (p99 64–81 µs). Polling removes the exposure to whatever the interrupt path is doing on a given host.
- What the round trip contains: two TLS hops, Azure VM to VM in one placement group, and the echo server's own busy loop.

### 4. HTTP clients

One request in flight, `POST` with a 242-byte form body and a 300-byte JSON response, p50 / p99 / p99.9 / max, two rounds each:

| client | 1,000 req/s round 1 | 1,000 req/s round 2 | back to back round 1 | back to back round 2 |
|---|---|---|---|---|
| ergon-io `recv-busy` | 60.6 / 69.0 / 82.4 / 155 | 60.7 / 67.6 / 86.0 / 807 | 55.5 / 69.4 / 80.3 / 490 | 55.5 / 68.9 / 77.2 / 1,332 |
| ergon-io `recv-busy`, background connect | 59.3 / 69.0 / 86.2 / 301 | 61.3 / 71.7 / 82.2 / 530 | 54.0 / 68.6 / 76.5 / 226 | 53.9 / 68.7 / 79.5 / 1,107 |
| ergon-io `uring-sqpoll` | 60.8 / 71.8 / 87.8 / 135 | 62.9 / 70.0 / 84.9 / 150 | 56.6 / 69.8 / 80.6 / 493 | 54.5 / 68.9 / 80.5 / 2,171 |
| ergon-io `epoll-spin` | 68.4 / 79.3 / 100.3 / 254 | 67.9 / 77.7 / 95.5 / 212 | 63.4 / 74.5 / 93.0 / 476 | 49.5 / 73.2 / 84.0 / 514 |
| ergon-io `uring-spin` | 69.6 / 79.0 / 95.8 / 201 | 69.3 / 77.8 / 94.9 / 304 | 58.4 / 73.4 / 86.5 / 752 | 61.7 / 76.1 / 94.6 / 798 |
| libcurl 8.23.0, source build (c-ares, `-O3 -march=native`, HTTP only) | 79.7 / 95.7 / 126.6 / 269 | 77.8 / 88.1 / 125.1 / 658 | 73.9 / 92.7 / 116.4 / 408 | 75.7 / 94.0 / 111.7 / 987 |
| libcurl 8.5.0, Ubuntu shared library | 80.3 / 91.3 / 120.6 / 276 | 79.1 / 92.6 / 143.3 / 239 | 81.3 / 96.4 / 120.2 / 3,313 | 81.0 / 95.2 / 112.8 / 1,334 |
| libcurl 8.22.0, curl-sys static (vendored OpenSSL 3.6.3) | 80.3 / 92.0 / 131.7 / 606 | 79.8 / 102.4 / 140.5 / 268 | 79.7 / 95.4 / 113.0 / 317 | 80.1 / 95.1 / 113.0 / 696 |
| reqwest 0.12 on tokio (current thread) | 86.4 / 97.5 / 131.5 / 246 | 87.1 / 98.5 / 137.4 / 294 | 85.5 / 99.4 / 119.0 / 611 | 87.8 / 104.9 / 134.5 / 510 |

- **ergon-io (`recv-busy`) is 17–20 µs faster than libcurl at 1,000 req/s and 18–26 µs back to back**, and about 26 µs faster than reqwest. With identical network and server, the difference is client-side work: libcurl's request assembly, connection-cache lookup and multi-handle state machine, and OpenSSL's record path against one pre-built request written straight into one TLS record and a response parsed in place.
- **NIC polling helps here too:** `recv-busy` and `uring-sqpoll` are 7–9 µs faster than the interrupt-driven backends at 1,000 req/s.
- **libcurl builds:** the from-source build is 1–6 µs faster than the distro and static builds, which are equal. libcurl was driven by calling `curl_multi_perform` on every spin (non-blocking); all three never slept. reqwest slept once per request (a tokio runtime parks the thread).
- **Syscalls per request at 1,000/s:** `uring-sqpoll` 1, `uring-spin` 4, `recv-busy` about 69 (it spins on `recvmsg`), `epoll-spin` about 330.
- `epoll-spin` back to back differed between rounds (63.4 against 49.5 µs); the other clients agree across rounds within 2 µs.

Practical notes for order entry (also in the `ergon_io::http` docs):
- A connection carries **one request in flight** by default; a second `request()` returns `Error::Busy`. A timed-out request closes its connection (HTTP/1.1 cannot abandon one request). To send several orders at once, keep a small pool of warm connections, or use the exchange's WebSocket order API.
- libcurl, if used: drive it with `curl_multi_socket_action` or `curl_multi_perform` (never `curl_multi_wait`/`curl_multi_poll`, which block), set an empty `Expect:` header, pin IPs with `CURLOPT_RESOLVE`, and build from source with c-ares (`-DENABLE_ARES=ON -DENABLE_THREADED_RESOLVER=OFF`); the curl-sys static build always uses resolver threads.

### 5. Connection set-up

`Driver::connect` never blocks: DNS (cached, `/etc/hosts`, or a UDP query), TCP, TLS and the WebSocket upgrade are advanced a step per poll. The TLS handshake's crypto still runs on the loop:

| | inline `connect` | `connect_background` |
|---|---|---|
| longest set-up step on the loop, per connect (HTTP run, recv-busy) | 0.67–0.77 ms | 0 |
| longest single poll while connecting | 0.68–0.78 ms | 0.17–0.74 ms (before the fixes below) |

Two fixes followed from the background numbers, and the confirmation run measures the result:
- The helper thread inherited the polling thread's single-CPU affinity and ran the handshake on the loop's own core. It now moves itself to the housekeeping CPUs (online, not isolated, not the polling CPU), or to `Config::connect_cpu`.
- The first `SO_TIMESTAMPING` in a process flips a kernel static key (code patched on every CPU, about 100 µs). Socket options are now set on the helper thread before the connection is handed over.

### 6. Live Binance

- **Δuser** is dominated by Binance's per-connection send skew (the A/A run differs by 42 µs at the median and 1.3 ms at p90), so it is not a loop comparison.
- **Stack share** agrees with the lab: on live traffic `recv-busy` 3.8–5.9 µs p50 against `uring-spin` 5.4–6.7 µs and `epoll-spin` 4.9–5.9 µs; `uring-sqpoll` 6.1–6.3 µs.
- **Context:** Binance event time `E` to kernel arrival is about 41 ms at the median from Hong Kong, with an unsynchronised clock (chrony had not yet selected the Azure PTP source). Context only.

### 7. Syscall census

`perf trace -s`, 10 s, separate from the timed runs: `epoll-spin` 1.67 M `epoll_wait` + 4,238 `recvmsg`; `uring-spin` 8,547–9,184 `io_uring_enter`; `recv-busy` 1.45 M `recvmsg`, 99.7 % of them `EAGAIN`; `uring-sqpoll` none. These match the in-process counters.

### 8. Request signing

`sign_bench`: one signed request's parameters, `&timestamp=…` and `&signature=…` appended to a reused buffer, exactly as `HttpConn::signed_request` builds them (a 143-byte Binance new-order parameter string), best of 7 × 200,000.

| signer | ns per signed request, x86 (D8ds_v6) | ns, Apple M-series (indicative) |
|---|---|---|
| HMAC-SHA256 (ring), hex | 262 | 176 |
| Ed25519 (ring), base64 | 21,558 | 11,842 |

On x86 Ed25519 costs about 80× HMAC per request (21.6 µs against 0.26 µs). The HMAC signer reproduces Binance's documented example signature (`tests/auth.rs`). Run `tls-20261014T091942Z` (`sign_bench`, West US 3, same VM size and CPU model).

## Confirmation run (final library)

The runs above used the benchmark's own loops (busy-spin run) or the library before its review fixes (HTTP run). The confirmation run repeats the order-path round trip, the HTTP comparison and the reconnect test on the final `ergon-io` code, built from the same source tree as this document.

Run `confirm-20261014T085509Z` (East Asia), tables in [`results/confirm-20261014T085509Z/tables.md`](results/confirm-20261014T085509Z/tables.md). No loop slept (0 voluntary context switches in every run).

Order-path round trip (WebSocket echo, p50 / p99 / p99.9 / max):

| rate | recv-busy | uring-sqpoll | uring-spin | epoll-spin |
|---|---|---|---|---|
| 100/s | 59.0 / 69.2 / 85.9 / 97.2 | 57.3 / 70.0 / 85.9 / 88.7 | 65.7 / 77.0 / 99.6 / 118 | 63.4 / 74.2 / 84.4 / 94.0 |
| 1,000/s | 57.3 / 64.8 / 71.0 / 143 | 58.0 / 65.1 / 71.1 / 441 | 63.1 / 73.9 / 84.7 / 1,061 | 62.9 / 73.2 / 82.6 / 279 |
| back to back | 54.5 / 68.0 / 71.8 / 1,081 | 54.6 / 68.5 / 73.0 / 642 | 57.9 / 72.2 / 80.5 / 890 | 59.2 / 73.4 / 79.9 / 612 |

HTTP (p50 / p99 / p99.9 / max):

| client | 1,000 req/s | back to back |
|---|---|---|
| ergon-io `recv-busy` | 58.4 / 69.7 / 73.9 / 151 | 55.4 / 68.6 / 72.6 / 442 |
| ergon-io `uring-sqpoll` | 60.7 / 64.9 / 69.5 / 1,416 | 41.6 / 65.8 / 71.4 / 723 |
| ergon-io `epoll-spin` | 65.3 / 71.5 / 80.0 / 348 | 59.5 / 72.7 / 81.2 / 915 |
| ergon-io `uring-spin` | 65.7 / 78.0 / 92.6 / 333 | 59.2 / 73.9 / 81.7 / 463 |
| libcurl 8.23.0, source build | 73.9 / 85.4 / 93.0 / 471 | 78.0 / 89.0 / 95.2 / 1,258 |
| libcurl 8.5.0, distro | 75.2 / 86.8 / 93.7 / 249 | 79.6 / 92.1 / 101.2 / 667 |

- The final library reproduces the earlier findings: polled loops ahead of interrupt-driven ones (here by 5–6 µs at the median, with tighter tails; this VM's interrupt path was not slow), ergon-io HTTP 15–17 µs ahead of libcurl at 1,000 req/s and 22–38 µs back to back, and the source-built libcurl slightly ahead of the distro build.

Reconnect cost, worst single poll per connect over 50 connect/close cycles (`http_bench --connects 50`):

| backend | inline `connect`, p50 / p90 / max | `connect_background`, p50 / p90 / max |
|---|---|---|
| recv-busy | 95.9 / 105.8 / 746.9 µs | **10.5 / 12.1 / 89.0 µs** |
| uring-sqpoll | 3.2 / 3.8 / 4.0 ms | 3.5 / 3.5 / 3.5 ms (bug, fixed below) |

- **Background connect works:** the loop's worst poll per reconnect falls from about 96 µs (the first connect about 0.75 ms; later handshakes resume the TLS session) to about 10 µs.
- **The SQPOLL numbers exposed a bug:** without an explicit `sqpoll_cpu`, the SQPOLL kernel thread inherits its creator's affinity, so with a pinned polling thread it spun on the poller's own core and took whole scheduler time slices (3.5 ms). The benchmark runs above always set `sqpoll_cpu`, so they were not affected. The library now puts the SQPOLL thread on a housekeeping CPU when none is given (`tests/driver.rs` checks the thread's allowed CPUs); the [HttpClient run](#httpclient-native-against-libcurl) repeats the reconnect test after the fix.

## HttpClient: native against libcurl

`ergon_io::HttpClient` puts both implementations behind one API (`request`, `signed_request`, `poll` reporting `on_response` / `on_error`), chosen by `HttpClientConfig::implementation`; libcurl needs the `curl` feature and is linked in-process through the `curl` crate (no separate program). This run measures them through that API, both libcurl drive modes, and the wrapper's own cost against `HttpConn` and libcurl used directly.

Run `confirm-20261014T093250Z` (East Asia, `ONLY=client`), tables in [`results/confirm-20261014T093250Z/tables.md`](results/confirm-20261014T093250Z/tables.md). One request in flight, the same order-sized `POST`, two rounds; p50 / p99 / p99.9 / max:

| path | 1,000 req/s round 1 | 1,000 req/s round 2 | back to back round 1 | back to back round 2 |
|---|---|---|---|---|
| `HttpClient` native, `recv-busy` | 53.9 / 63.5 / 67.7 / 95.4 | 55.0 / 63.9 / 69.2 / 1,532 | 47.4 / 66.2 / 70.4 / 831 | 40.2 / 53.0 / 61.9 / 650 |
| `HttpClient` native, `uring-sqpoll` | 55.2 / 64.4 / 67.3 / 87.6 | 55.2 / 64.6 / 68.3 / 340 | 51.0 / 67.0 / 72.0 / 659 | 51.9 / 68.4 / 76.4 / 616 |
| `HttpConn` directly, `recv-busy` | 53.5 / 62.5 / 66.5 / 87.3 | 54.8 / 63.3 / 73.3 / 117 | 51.5 / 67.1 / 73.6 / 641 | 51.1 / 66.9 / 70.9 / 401 |
| `HttpClient` libcurl 8.5.0 (distro), `perform` | 73.7 / 85.1 / 93.3 / 280 | 74.7 / 84.2 / 94.4 / 203 | 76.8 / 91.2 / 96.9 / 975 | 76.9 / 90.6 / 96.3 / 712 |
| `HttpClient` libcurl 8.5.0 (distro), `socket_action` | 75.8 / 86.1 / 91.9 / 596 | 75.9 / 85.4 / 96.3 / 452 | 77.5 / 91.4 / 100.0 / 682 | 77.6 / 92.2 / 100.6 / 442 |
| `HttpClient` libcurl 8.23.0 (source), `perform` | 70.3 / 82.6 / 90.2 / 839 | 73.8 / 85.5 / 92.9 / 1,486 | 74.3 / 90.0 / 99.7 / 1,322 | 74.6 / 90.2 / 98.7 / 737 |
| `HttpClient` libcurl 8.23.0 (source), `socket_action` | 76.4 / 99.8 / 114.5 / 314 | 74.9 / 83.7 / 89.8 / 403 | 74.7 / 89.8 / 102.2 / 522 | 75.6 / 90.9 / 102.2 / 881 |
| libcurl 8.5.0 directly (`curl` crate, `perform`) | 71.7 / 83.5 / 93.8 / 700 | 71.1 / 82.2 / 87.4 / 1,304 | 76.6 / 90.5 / 98.1 / 1,357 | 55.0 / 67.0 / 75.8 / 438 |

- **Native against libcurl through the same API:** native is 16–21 µs faster at 1,000 req/s and 27–37 µs back to back.
- **The wrapper's cost:** native `HttpClient` against `HttpConn` directly is within 0.4 µs at 1,000 req/s (noise). The libcurl implementation adds about 2–3.5 µs over driving libcurl directly; it builds the URL and header list for every request. That is the price of a uniform API on the fallback path, and could be trimmed if the fallback's latency ever matters.
- **Driving libcurl:** `curl_multi_perform` on every poll was 1–6 µs faster than `curl_multi_socket_action` (which spends `epoll_wait` and `epoll_ctl` calls); both never block. With `perform`, the source build was 1–3 µs faster than the distro build.
- **Back to back** some interrupt-driven runs land in a faster mode on one round (direct libcurl 55.0 against 76.6 µs; earlier `epoll-spin` 49.5 against 63.4 µs); the paced 1,000 req/s rows are the stable comparison.
- **Reconnect cost after the SQPOLL fix** (worst single poll per connect, 50 cycles, no `sqpoll_cpu` given):

| backend | inline `connect`, p50 / p90 / max | `connect_background`, p50 / p90 / max |
|---|---|---|
| uring-sqpoll | 105.0 / 109.9 / 774.2 µs | **6.0 / 6.7 / 98.3 µs** |
| recv-busy | 104.5 / 109.2 / 774.6 µs | **11.5 / 13.1 / 102.2 µs** |

The SQPOLL stall (3.5 ms per poll) is gone; with background connects a reconnect costs the loop 6–12 µs at the median.

## Poll-then-sleep loops (not for busy spin)

From the earlier run `20261014T005221Z` and its sensitivity run `…-sens`. These loops wait in the kernel while it polls the NIC, then sleep, so they suit a thread with nothing else to do.

| | epoll-busy | uring-napi | uring-sqpoll |
|---|---|---|---|
| stack share p50, 1× replay | 12.6–13.0 | 4.6 | 4.3 |
| decoded vs epoll-busy, 1× replay p50 | — | about 3 µs earlier | about 9 µs earlier |
| round trip p50 / p99 / p99.9 / max at 1,000/s | 71.0 / 84.2 / 90.6 / 569 | 57.0 / 79.4 / 87.3 / 117 | 57.8 / 64.1 / 71.1 / 420 |
| with budget = wait = 200 µs: stack share p50 | 11.9–18.1 | 3.7–4.2 | |
| with budget = wait = 200 µs: round trip p50 / p99 / p99.9 / max at 1,000/s | 70.8 / 79.2 / 86.5 / 188 | 56.2 / 63.3 / 69.7 / 665 | |

- io_uring's NAPI wait path beats epoll's busy poll even when both poll continuously (`BusyPollRxPackets` confirmed epoll was polling). A plausible cause, not investigated: epoll returns readiness and needs a separate `recvmsg`, and its busy loop follows a single NAPI id.
- Settings: `busy_poll_us = 50`, `wait_us = 100` (sensitivity: 200/200), `prefer_busy_poll` off.

## TLS: in-place record layer vs stock implementations

`tls_bench` measures turning ciphertext already in memory into plaintext the application can read: a loopback rustls server sends single-record messages (TLS 1.3, AES-128-GCM); only the decrypt-and-deliver loop is timed, best of 5, 64 MiB per size, one pinned core, same VM size and CPU model (West US 3; no network involved).

| implementation | 200 B | 1,000 B | 4,000 B | 16,000 B | allocs/message |
|---|---|---|---|---|---|
| **in place, ring** (ergon-io) | **105** | **175** | **429** | **1,464** | 0 |
| in place, aws-lc-rs | 100 | 168 | 342 | 955 | 0 |
| rustls unbuffered (ring) | 181 | 257 | 557 | 1,670 | 1 |
| rustls buffered, `fill_buf` (ring) | 231 | 330 | 722 | 2,213 | 1 |
| rustls buffered, `read` (ring) | 231 | 333 | 774 | 2,607 | 1 |
| rustls buffered, `fill_buf` (aws-lc-rs) | 235 | 332 | 647 | 1,757 | 1 |
| OpenSSL 3.0.13 (`openssl` crate, `SslStream::read`) | 460 | 563 | 827 | 1,951 | 0 † |

Values are ns per message. † OpenSSL allocates through its own `malloc`, which the counting allocator does not see.

- In place saves 76–82 ns per typical Binance message (200 B–1 KB) against rustls's fastest API and removes its allocation and copy per record. The receive stack is several µs, so this is a CPU and allocation-free-steady-state win, not a latency lever.
- **Production caveat:** record encryption and decryption are then this crate's code, as with kTLS: per-direction sequence numbers and nonces, key updates in both directions, alerts, and the AES-GCM confidentiality limit (rotate keys or reconnect well before it). For orders this code needs an independent review, or replacement by kTLS or by rustls once its in-place decryption lands.

## AWS Tokyo

Runs on 15 October 2026 in ap-northeast-1, the region Binance's matching engine is in. Every table keeps the maximum; each run's tables list its five slowest samples with what the traces show for each, and the worst cases no trace explains are summarised under [worst cases](#worst-cases-on-aws).

| | |
|---|---|
| Receiver | `c7i.2xlarge`: Intel Xeon Platinum 8488C (Sapphire Rapids), 4 cores / 8 threads, ENA (8 combined queues; adaptive receive moderation on by default, `rx-usecs` 20) |
| Sender | `c7i.xlarge` in the same cluster placement group (apne1-az4; apne1-az1 once, when az4 had no capacity) |
| Kernel | Ubuntu 24.04, `7.0.0-1014-aws`, clocksource `tsc` |
| Isolation | boot profile `full`: `isolcpus=managed_irq,domain,1-3,5-7 nohz_full=1-3,5-7 rcu_nocbs=1-3,5-7 irqaffinity=0,4`; before measuring, a spinner on every isolated CPU for 130 s (the gap map) puts the loop on the quietest core, its hyperthread sibling idle |
| Network tuning | routes with `quickack 1 rto_min 5ms`; `SO_BUSY_POLL` 1 µs on the polling backends' sockets |
| Rig | the sender's idle states capped at C1 and its echo server on `SO_BUSY_POLL` 50 (see [lab notes](#lab-notes)) |

Runs and tables:

| run | what |
|---|---|
| `flags-aws-20261015T045527Z` | stage 1 settings matrix, fixed rig (baselines and boot profiles; see [limits](#limits)) |
| `flags-aws-20261015T051742Z` | HTTP clients |
| `flags-aws-20261015T051741Z` | `nohz_full` on the SQPOLL CPU, A/B |
| `flags-aws-20261015T062428Z`, `…T070339Z` | tail and NIC settings, 8 minutes per setting at 10,000/s, A/B/A/B |
| `flags-aws-20261015T083544Z` | market data, two backends paired per message |
| `flags-aws-20261015T084140Z` | many connections, first run: 100, four backends |
| `flags-aws-20261015T093248Z` | many connections: 1, 10 and 100, six backends |
| `flags-aws-20261015T101949Z` | 1,024 streams on 1 to 256 combined-stream connections |
| `flags-aws-20261015T122127Z` | many connections with the SQPOLL thread on the loop's hyperthread sibling (one physical core) |
| `flags-aws-20261015T124649Z` | order round trip: `uring-sqpoll` on the sibling, mio + rustls |
| `flags-aws-20261015T142240Z` | bare-metal receiver (`c7i.metal-24xl`): order round trip and many connections |
| `flags-aws-20261015T051813Z`, `…T055149Z`, `…T072746Z` | instance types: `c7a.2xlarge`, `r7iz.2xlarge`, `c7i.4xlarge` |
| `flags-aws-20261015T045347Z` | diagnosis: hwlat, slow syscalls and kernel functions, guest agents on/off |

### One connection: order round trip and market data

Settings-matrix baselines on the fixed rig (`flags-aws-20261015T045527Z`, four baselines), WebSocket echo round trip, p50 / p99 / p99.99, µs:

| backend | back to back | 10,000/s |
|---|---|---|
| `recv-busy` | 39.5–42.0 / 45.5–49.7 / 68–89 | 41.0–41.8 / 45.4–48.4 / 65–80 |
| `uring-sqpoll` | 40.0–41.0 / 45.5–47.0 / 66–69 | 41.7–42.9 / 47.3–50.6 / 67–91 |
| `uring-spin` | 47.7–48.4 / 55–91 / 83–204 | |
| `epoll-spin` | 47.1–49.0 / 55–98 / 81–245 | |

The same on another pair with the SQPOLL thread on the loop's hyperthread sibling and the mio baseline (`flags-aws-20261015T124649Z`, two baselines):

| backend | back to back | 10,000/s |
|---|---|---|
| `recv-busy` | 36.3–37.4 / 51.7–51.8 / 83–96 | 38.3–38.8 / 50.5–54.0 / 89–95 |
| `uring-sqpoll`, its own core | 38.7–38.8 / 53.0–55.2 / 97–99 | 38.9–39.6 / 53.5–53.6 / 93–112 |
| `uring-sqpoll`, the loop's hyperthread sibling | 38.3–38.5 / 51.5–52.6 / 94–95 | 40.7–40.8 / 55.9–56.7 / 96–103 |
| `epoll-spin` | 43.2–45.3 / 68.1 / 159–236 | 43.4–49.9 / 89–94 / 146–194 |
| mio + rustls | 43.7–48.7 / 61–93 / 146–175 | 45.6–47.8 / 88–90 / 150–174 |

Market data, two loops receiving the same messages at once on their own cores and connections, paired per message (`flags-aws-20261015T083544Z`; 50-message bursts every millisecond, and one message every millisecond), Δ = B − A at the median:

| A | B | Δ p50 |
|---|---|---|
| `recv-busy` | `uring-sqpoll` (both orders, two rounds) | −0.8 to +0.8 (A/A: ±1) |
| `recv-busy` | `epoll-spin` | +7 to +71 when on another NIC queue; about 0 when it shares `recv-busy`'s queue (whose polling then delivers its packets too) |
| `recv-busy` | `uring-spin` | +1 to +29 |

With one or a few connections the two NIC-polling backends are equal, and both beat the interrupt-driven ones: 6–8 µs at the median and about half the p99 on the round trip.

### Many connections

`replay_server --mode mixed` gives every connection its own stream: a trickle at the connection's own random interval (1–10 ms), and every 2 ms a burst of 10 messages to each of 10 random connections, about 68,000–75,000 messages a second in all, the same for every backend (a fixed seed). Each backend runs alone for 60 s, three rounds in alternating order (`flags-aws-20261015T093248Z`). The figure is **sender → decoded, one way**: it includes the NIC, the interrupt path and the wait for the loop to reach the socket, and the offset between the two hosts' clocks, which drifted by up to 20 µs between rounds (common to the runs within a round). Median of the three rounds' p50 / p99, µs:

| connections per loop | `recv-busy` | `recv-busy`, `per_queue` | `uring-sqpoll` | `epoll-spin` | mio + rustls | `uring-spin` |
|---|---|---|---|---|---|---|
| 1 | **21.7 / 57** | **20.0 / 49** | **21.4 / 55** | 41.6 / 155 | 58.0 / 170 | 44.6 / 211 |
| 10 | 34.6 / 88 | 36.8 / 91 | **31.0 / 85** | 61.2 / 115 | 63.3 / 119 | 60.8 / 117 |
| 100 | 74.7 / 169 | 41.3 / 107 | **29.3 / 90** | 36.3 / 111 | 37.9 / 113 | 41.4 / 116 |

- **`recv-busy` does not scale with sockets.** It makes one `recvmsg` per socket per poll: with 100 sockets a poll took about 67 µs (15,000 polls a second against `uring-sqpoll`'s 7 million), so a message waits on average half a poll for its socket's turn.
- **`per_queue`** (one busy-polled read per NIC queue, then only the sockets `epoll_wait(0)` reports ready) recovers most of it: 41 against 75 µs at 100 connections, and equal to plain `recv-busy` at 1 and 10.
- **`uring-sqpoll` scales best**: its kernel thread polls each NIC queue once per pass, on its own core, in parallel with the loop, and completes every socket into one ring.
- **mio + rustls is the epoll path**, within about 2 µs of `epoll-spin` at every count: interrupt-driven, so it can match neither polling backend with few connections. It is the reference for "the usual Rust way" (`mio_bench`).

**One physical core (two vCPUs, as a pod gets): the SQPOLL thread on the loop's hyperthread sibling.** The same test on another pair (`flags-aws-20261015T122127Z`, three rounds, median p50 / p99 one way, µs):

| connections per loop | `recv-busy` | `per_queue` | `uring-sqpoll`, its own core | `uring-sqpoll`, the loop's sibling | `epoll-spin` |
|---|---|---|---|---|---|
| 1 | 21.5 / 46 | 20.9 / 53 | 21.6 / 45 | 22.5 / 49 | 61 / 144 |
| 10 | 34.7 / 87 | 35.7 / 89 | 31.5 / 84 | 32.3 / 88 | 54.7 / 111 |
| 100 | 72.3 / 159 | 39.1 / 101 | 29.1 / 88 | **30.6 / 95** | 38.4 / 111 |

The sibling costs about 1.5 µs at the median and 7 µs at p99 against a core of its own, and still beats every single-vCPU option at 100 connections by about 8 µs. It needs both hyperthreads of the core (in Kubernetes, the static CPU manager with whole physical cores) and nothing else on the sibling.

### Many streams on combined connections

The same question from the other side: 1,024 streams (each with its own trickle of 5–50 ms, bursts as above, about 85,000 messages a second), spread over 1, 4, 16, 64 or 256 connections as on Binance's combined-stream sockets (up to 1,024 streams each). Two rounds (`flags-aws-20261015T101949Z`), one way, p50 / p99 per round, µs:

| 1,024 streams over | `recv-busy` | `per_queue` | `uring-sqpoll` | `epoll-spin` | mio |
|---|---|---|---|---|---|
| 1 connection | **10.2 / 25**, 21.1 / 60 | 13.6 / 29, 19.7 / 33 | 16.9 / 56, 22.3 / 61 | 38.9 / 82, 46.3 / 93 | 41.1 / 90, 43.5 / 92 |
| 4 | **12.9 / 54**, 26.5 / 70 | 13.1 / 55, 27.9 / 70 | 19.4 / 59, 27.2 / 67 | 35.0 / 91, 47.6 / 104 | 39.7 / 95, 50.8 / 108 |
| 16 | 20.3 / 76, 26.0 / 85 | 23.2 / 83, 21.7 / 80 | 20.0 / 77, 26.7 / 82 | 39.5 / 108, 47.6 / 115 | 36.4 / 106, 45.0 / 115 |
| 64 | 46.4 / 132, 60.9 / 146 | **23.3 / 89**, 33.8 / 99 | 30.5 / 92, 27.2 / 88 | 54.4 / 128, 40.0 / 114 | 38.6 / 113, 44.0 / 120 |
| 256 | 123 / 258, 130 / 265 | 33.1 / 100, 35.8 / 102 | **28.6 / 94**, 30.8 / 96 | 36.2 / 115, 40.9 / 117 | 31.6 / 109, 43.8 / 119 |

- **Fewer, busier connections are faster than many quiet ones**: the same 1,024 streams on 1–4 connections with `recv-busy` reach p50 10–27 µs, against 29–31 µs on 256 connections with `uring-sqpoll`, with no extra core.
- **The cost is shared fate**: one TCP connection carries every line on it, so a loss or stall delays them all (one round's single connection saw a 115 ms event). A few connections (about 4) keep the latency of one while limiting that.

### HTTP clients on AWS

Order-sized `POST`, one request in flight, two rounds in reversed order; p50 and p99 in µs (range over the rounds).

| client | p50, 10,000/s | p99, 10,000/s | p50, back to back | p99, back to back | p99.99, back to back |
|---|---|---|---|---|---|
| **ergon-io `HttpConn`, `recv-busy`** | **44.6–44.9** | 53–54 | **42.0–42.2** | **50.2–50.3** | **70–71** |
| ergon-io, `uring-sqpoll` | 45.3–46.0 | 53–54 | 43.9–44.3 | 51.5–51.9 | 78–83 |
| ergon-io `HttpClient` (pooled API), `recv-busy` | 46.5–46.9 | 56–58 | 42.8–43.0 | 50.3–50.8 | 72–89 |
| ergon-io, `uring-spin` / `epoll-spin` | 48.6–50.3 | 90–97 | 48.2–49.0 | 96–97 | 106–110 |
| libcurl 8.23.0, source build with c-ares | 56.0–56.5 | 74–101 | 54.3–55.2 | 68–69 | 102–103 |
| libcurl, distro | 58.5–61.5 | 77–103 | 58.1–61.9 | 75 | 108–130 |
| reqwest (tokio) | 245 | 353–409 | 245 | 349–351 | 425–430 |

- ergon-io's own client is 12–19 µs ahead of every libcurl build at the median, as on Azure; the pooled `HttpClient` API costs 1–2 µs over a raw `HttpConn`.
- reqwest parks its thread between requests: about 200 µs more per request, and at 10,000/s it fell behind its schedule (470,000 of 1.2 million requests).

### Host and NIC settings on AWS

`recv-busy` order round trip at 10,000/s, 8 minutes per setting, each setting twice in alternating order; change against the baseline median, µs.

| setting | p50 | p99 | p99.99 | round trips over 100 µs /min (baseline 73–117) |
|---|---|---|---|---|
| **adaptive interrupt moderation off** (`ethtool -C <nic> adaptive-rx off`) | **−3.0, −3.0** | **−6.5, −5.2** | **−47, −43** | **15–32** |
| interrupt moderation off (`adaptive-rx off rx-usecs 0`) | −2.0, −2.4 | −5.0, −4.9 | −40, −38 | 32–35 |
| `SCHED_FIFO` loop | +0.5, −1.8 | +0.4, −3.0 | +1, −45 | 19–130 |
| TCP autocorking off | +0.5, −0.6 | −4.7, +0.2 | −20, −14 | 46–71 |
| one NIC queue | −1.8, +1.3 | −1.9, −0.9 | −15, +15 | 64–174 |
| NIC interrupts on the loop CPU | +0.7, −0.2 | +1.5, −2.1 | +1, −20 | 44–110 |
| receiver idle states capped at C1 | +2.2, +2.1 | +1.9, +1.1 | +17, +12 | 156–181 |
| SQPOLL CPU out of `nohz_full` (`uring-sqpoll`) | no difference over 2 × 2 boots | | | |

- **Turn adaptive receive moderation off.** With it on (ENA's default), the driver re-tunes moderation in a kernel work item (`ena_dim_work`) on the CPU that polls the queue, which for a busy-polling loop is the loop's own isolated CPU: about 40 pre-emptions a second. Off, the loop had no involuntary context switch in 8 minutes (8,841 with it on) and 94% fewer iterations over 20 µs.
- **Do not** move NIC interrupts onto the loop's CPU (market-data bursts: p99 +12 to +18 µs), cut the NIC to one queue, or cap the receiver's idle states (2 µs slower at the median, most likely less turbo headroom for the busy core).

### Bare metal

`c7i.metal-24xl` receiver (no hypervisor; 48 cores), the same `c7i.xlarge` sender (`flags-aws-20261015T142240Z`, two baselines, NIC defaults). A spinner on each of its 94 isolated CPUs for 130 s saw nothing over 8 µs (the VM's quietest cores: up to 375 µs).

| backend | back to back, p50 / p99 / p99.99 | 10,000/s, p50 / p99 / p99.99 | round trips over 100 µs /min |
|---|---|---|---|
| `recv-busy` | 35.3–35.7 / 43.3–43.4 / 64 | 37.1–37.8 / 43.9–44.8 / 64–65 | 3–33 |
| `uring-sqpoll` | 38.1–39.7 / 45.5–53.8 / 66–89 | 38.2–40.2 / 50.2–52.7 / 73–83 | 9–112 |
| `uring-sqpoll`, the loop's sibling | 37.8–38.4 / 45.3–46.4 / 67 | 41.0–41.3 / 48–74 / 71–120 | 8–643 |
| `epoll-spin` | 39.4–41.1 / 56–67 / 99–108 | 40.0–41.2 / 51 / 77–99 | 5–512 |

- **What metal buys:** 1–3 µs at the median, a steadier p99 and p99.99 (`recv-busy` 64–65 against 65–96 µs on the VM) and fewer slow round trips; the interrupt path is cheap without a hypervisor (`epoll-spin` only 2–4 µs behind polling, against 6–10).
- **What it does not:** round trips of about 1 ms still occur (four runs of eight had one), so they are not the receiver's hypervisor.
- **Many connections** (the mixed traffic above, two rounds, one way p50 / p99, µs): at 100 connections `uring-sqpoll` 26.7–35.1 / 81–90, `epoll-spin` 32.4–34.8 / 84–89, `per_queue` 32.5–38.5 / 90–94, plain `recv-busy` 62–64 / 131–133, the same ranking as on the VM and no better at the median; at 10 connections every backend sat at 32–42 / 82–95. Its maxima were worse, though: 1.4–3.0 ms at 100 connections (0.2–0.4 ms on the VM), cause not found in this run.
- **Cost:** about $5 an hour against $0.45 for `c7i.2xlarge`. For this workload metal is not worth it: a little at the median and p99 with one connection, nothing with many, and a worse many-connection maximum.

### Instance types

Baselines on the same rig, ranges over two baselines, µs.

| instance | WebSocket round trip p50 / p99 (`recv-busy`) | market-data burst p50 (arrival → decoded) | longest gap on an idle spinning core | per hour |
|---|---|---|---|---|
| **`c7i.2xlarge`** (Sapphire Rapids) | **37–39 / 44–48** | 2.6–3.3 | up to about 375 µs | about $0.45 |
| `c7i.4xlarge` | 37.7–40.4 / 44–49 | 2.4–2.7 | 74–81 µs | about $0.89 |
| `r7iz.2xlarge` (high frequency) | 38.0–39.4 / 50–56 | 2.2–2.6 | 108–376 µs | about $0.98 |
| `c7a.2xlarge` (AMD Genoa) | 42.7–43.1 / 51–51 | 4.8–5.3 | | about $0.52 |

`c7i` is the choice; the larger `c7i.4xlarge` matches it with a quieter core in these two baselines.

### Worst cases on AWS

- **About one round trip in 1.8 million takes about 1 ms.** Neither loop stalls during it, hwlat (interrupts off, all isolated CPUs, 120 s) saw nothing over 57 µs, and it appears under every host and NIC setting tested, **and on a bare-metal receiver too** (see [metal](#bare-metal)), so it is not the receiver's hypervisor: the AWS network path or the sender (still a VM) remain. The md-dpdk lab saw round trips of 676–845 µs between two metal hosts with DPDK, which points at the network path.
- **Rare whole-VM pauses** of 0.16–4 ms (one of 3.8 ms in the first stage-1 run).
- **With 100 connections, two path-wide delays of 110–126 ms** in about 15 minutes of runs: every connection's messages held back at once while neither loop stalled, so TCP retransmission or a paused vCPU on the interrupt path; the runs since record ENA's and TCP's counters (drops, `pps_allowance_exceeded`, retransmissions) on both hosts to tell which.
- No once-a-minute stall (Azure's), and stopping the guest agents (SSM, CloudWatch) changes nothing.

### Lab notes

- **The sender must not sleep.** An echo server that waits for interrupts on a CPU in C6 (190 µs exit latency on c7i) made 0–16% of round trips about 170 µs slower, varying per run. The first AWS matrix run (`flags-aws-20261014T163114Z`) has this artefact; with the sender capped at C1 and busy polling, the p50 fell from 44–48 to 37–39 µs and the p99 from 55–95 to 44–49 µs.
- **`aws login` and parallel runs.** Two AWS CLI processes refreshing the login token at once corrupted its cache, and every later call failed, teardown included. The lab scripts now take one lock around every call and retry teardown.

## Settings matrix

Every host, kernel, NIC and socket setting under test, measured the same way (platform noise on the loop CPU, round trips per backend paced and back to back, HTTP, a market-data burst), with baselines bracketing every two settings and every run traced so each worst case is attributed (`scripts/lib-matrix.sh`; `analyse --matrix` writes the tables, including the maximum, the cause of the worst case, the five slowest samples, once-a-minute stalls and interrupt rates per CPU).

**Azure East Asia** (`flags-azure-20261014T144146Z`, D8ds_v6 / D2ds_v6):
- **No setting moves the median by more than 1–3 µs.** Baselines: round trip p50 47, p99 62, p99.99 80 µs.
- **The maximum is the hypervisor.** Every run's maximum (0.6–2.7 ms) comes from a stall that stops some of a VM's vCPUs for 0.5–3.7 ms at one fixed second of every minute, a different second per VM (receiver :51 or :52, sender :49 or :04). The guest sees no event during it; hwlat, with interrupts off, measured the same gaps (up to 1,054 µs, CPUs 2–4, all at :52); stopping the guest agents (walinuxagent, hv-kvp-daemon) changes nothing (`flags-azure-20261015T045348Z`). **Mitigation:** map the gaps per vCPU at start-up (a spinner on every isolated CPU for two minute marks) and run the loop on a vCPU the stall does not reach; each VM has its own subset.
- In about a quarter of runs p99.99 sat near 220 µs with the slow round trips spread over the run, on the polled and the interrupt-driven loops alike: a state of the network path, not of the host.

**AWS Tokyo:** see [host and NIC settings](#host-and-nic-settings-on-aws); no once-a-minute stall, hwlat clean.

## Limits

- **Two clouds, one VM size each, one kernel each** (Azure 6.17 with MANA, AWS 7.0 with ENA); the interrupt path's tails varied between VM instances (section 3). Bare metal was measured once (one host, two baselines and the many-connection set).
- **One-way numbers** (many connections, combined streams) use two hosts' clocks: their offset drifted by up to 20 µs between rounds, so compare backends within a round; the round-trip and paired numbers use one clock.
- **The AWS settings matrix** lost the raw records of its single-setting runs to a copy failure; the settings that mattered were re-measured with longer runs (8 minutes each, A/B/A/B), and the others (preferred busy polling, NAPI budget, busy-poll time, GRO, quick ACK, workqueue placement, guest agents, a busy sibling) rest on the first AWS run, whose tails the rig's sleeping sender inflated.
- **Software receive timestamps only.**
- **Round trips and HTTP clients** were measured one after another, not paired, so path drift between runs is included; HTTP ran two interleaved rounds to expose it.
- **SQPOLL** uses one more core than any other loop; comparing it with a single-core loop is a cost/benefit comparison.
- **Live Binance HTTP** was not measured: West US 3 is blocked (HTTP 451).
- **Bursts:** some polled-pair burst medians depend on the core slot; burst numbers are reported but the verdicts rest on 1× replay and round trips.

## Reproduce

```sh
# local checks (the curl feature needs libcurl and OpenSSL headers)
cargo test -p ergon-io --all-features -- --test-threads=1
cargo test --manifest-path io/benches/Cargo.toml

# AWS and Azure lab (a dedicated, tagged pair of VMs per run, deleted on every exit and verified gone):
#   MODE=matrix | base | confirm | diagnose | sqtick | http | md | many
io/benches/scripts/aws-flags.sh        # Tokyo by default; RCV_TYPE / SND_TYPE pick the instance types
io/benches/scripts/azure-flags.sh      # East Asia by default
MODE=many MANY_CONNS="1 10 100" io/benches/scripts/aws-flags.sh               # many connections
MODE=many MANY_STREAMS=1024 MANY_CONNS="1 4 16 64 256" io/benches/scripts/aws-flags.sh   # combined streams
# results come back as tables and logs; KEEP_RECORDS=1 also copies the raw records and traces

# Azure runs (Azure CLI logged in; each creates and deletes one ergon-lab-* resource group)
io/benches/scripts/azure-run.sh        # busy-spin loops: paired lab, round trips, live Binance
io/benches/scripts/azure-http.sh       # HTTP clients (LOC=westus3 by default)
io/benches/scripts/azure-confirm.sh    # final library: round trips, HTTP, reconnect cost
ONLY=client io/benches/scripts/azure-confirm.sh   # HttpClient: native against libcurl
io/benches/scripts/azure-tls-bench.sh  # TLS record decryption and request signing (SKIP_TLS=1: signing only)

# analysis
cargo run --release --manifest-path io/benches/Cargo.toml --bin analyse -- results/<stamp>/res/<run>...
cargo run --release --manifest-path io/benches/Cargo.toml --bin analyse -- --matrix results/<stamp>/res   # lab runs (reads .zst copies)
```

Every script snapshots the source at start, runs on the VMs detached, records the environment (`env-rcv.txt`), analyses on the receiver, downloads the tables (and, with `KEEP_RECORDS=1`, the raw records and traces, compressed), and deletes its cloud resources on every exit path, then verifies they are gone.
