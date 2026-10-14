# ergon-io benchmarks: busy-spin loops, HTTP clients and connection set-up

All runs are on x86 in Azure: `Standard_D8ds_v6` receiver and `Standard_D2ds_v6` sender, Intel Xeon Platinum 8573C, MANA NIC, kernel `6.17.0-1022-azure`. Busy-spin and confirmation runs are in East Asia (Hong Kong), the nearest region to Binance (AWS Tokyo) where this subscription can run x86; the HTTP comparison is lab-only and ran in West US 3.

| run | what | tables |
|---|---|---|
| `20261014T060634Z` | busy-spin loops: paired lab replay, ping-pong, live Binance | [`results/20261014T060634Z/tables.md`](results/20261014T060634Z/tables.md) |
| `http-20261014T074036Z` | HTTP clients: ergon-io, libcurl (three builds), reqwest | [`results/http-20261014T074036Z/tables.md`](results/http-20261014T074036Z/tables.md) |
| `confirm-20261014T085509Z` | the final `ergon-io` library: ping-pong, HTTP, reconnect cost | [`results/confirm-20261014T085509Z/tables.md`](results/confirm-20261014T085509Z/tables.md) |
| `confirm-20261014T093250Z` | `HttpClient`: native against libcurl through one API; reconnect cost after the SQPOLL fix | [`results/confirm-20261014T093250Z/tables.md`](results/confirm-20261014T093250Z/tables.md) |
| `tls-20261014T091942Z` | request signing cost (`sign_bench`) | [`results/tls-20261014T091942Z/sign_bench.md`](results/tls-20261014T091942Z/sign_bench.md) |
| `20261014T005221Z`, `…-sens` | earlier run: poll-then-sleep loops (`epoll-busy`, `uring-napi`) | [`results/20261014T005221Z/tables.md`](results/20261014T005221Z/tables.md) |
| `tls-20261014T034037Z` | TLS record decryption, in place vs stock | [`results/tls-20261014T034037Z/tls_bench.md`](results/tls-20261014T034037Z/tls_bench.md) |

## Conclusions

**For a busy-spin loop that must never sleep, poll the NIC.** Use `recv-busy`, or `uring-sqpoll` if a core can be spared for the kernel thread.
- Against the interrupt-driven loops, a NIC-polling loop decodes the same market-data message about 11 µs earlier at the median (lab replay at Binance's rate, both core orders, 88–92 % of messages first).
- On the order path (a round trip at 100–1,000 msg/s) it removes the interrupt path's tails: p99 67–99 µs against 260–630 µs, p99.9 95–154 µs against over 2 ms, in the run where the interrupt path was slow (one VM in three). On the other two the interrupt-driven loops were 5–6 µs slower at the median with ordinary tails; the polled loops were tight on all three.
- `recv-busy` and `uring-sqpoll` are equal within the A/A noise. `recv-busy` spends about 79 % of its core in the kernel (one non-blocking `recvmsg` per spin); `uring-sqpoll` makes no syscalls on the loop thread but its kernel thread occupies a second core.

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

**Loops (`ergon_io::Config`):**
- Latency-critical busy-spin thread (market data and orders): `recv-busy`. If a core can be spared, `uring-sqpoll` matches it and takes the syscalls off the loop thread.
- Busy-spin thread where CPU matters more than the last microseconds: `uring-spin` (equal latency to `epoll-spin`, about 1,000× fewer syscalls).
- A thread that may nap: `uring-napi` with a wait, rather than `epoll-busy`.
- Keep the polled loops' sockets on their own NIC queues where possible (the analysis reports shared queues).

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

## Setup

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
| `recv-busy` | non-blocking `recvmsg` on every socket every poll, `SO_BUSY_POLL` set: an empty read runs one NAPI pass, then `EAGAIN` | yes, on the loop core | never | 1 per socket per poll | latency-critical busy spin |
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

## Results

All values are in µs. In paired tables Δ is B − A, so negative means B decoded first; rows come in pairs (both core orders).

### 1. Interrupt-driven: `epoll-spin` vs `uring-spin`

| traffic | A | B | Δuser p50 | Δuser p99 | stack A p50 / p99 | stack B p50 / p99 | A syscalls/s | B syscalls/s | A kernel % | B kernel % |
|---|---|---|---|---|---|---|---|---|---|---|
| 1× replay | epoll-spin | uring-spin | 0.28 | 24.75 | 8.55 / 57.78 | 8.43 / 56.25 | 4,134,654 | 808 | 30.1 | 0.2 |
| 1× replay | uring-spin | epoll-spin | −0.17 | 29.54 | 6.66 / 42.41 | 6.77 / 42.13 | 700 | 4,317,183 | 0.2 | 31.2 |
| 1× A/A | epoll-spin | epoll-spin | −0.12 | 30.45 | | | | | | |
| burst | epoll-spin | uring-spin | −0.65 | 25.52 | 18.55 / 60.54 | 18.76 / 60.42 | 3,679,619 | 54,390 | 38.6 | 12.3 |
| burst | uring-spin | epoll-spin | −0.10 | 24.43 | 17.58 / 59.24 | 17.10 / 58.95 | 61,332 | 3,540,137 | 14.6 | 38.7 |

- Equal latency: the differences sit inside the A/A spread, and the earlier run (`20261014T005221Z`) found the same (0.22 / −0.18 µs).
- io_uring removes over 99.9 % of the loop's syscalls at 1× and cuts its kernel share from about 30–39 % to 0.2–15 %.

### 2. NIC polling without sleeping: `recv-busy` vs `uring-spin` and `uring-sqpoll`

| traffic | A | B | Δuser p50 | Δuser p99 | Δrx p50 | A first | stack A p50 / p99 | stack B p50 / p99 |
|---|---|---|---|---|---|---|---|---|
| 1× replay | recv-busy | uring-spin | 10.76 | 466.54 | 7.98 | 91.5 % | 3.85 / 17.21 | 7.45 / 39.42 |
| 1× replay | uring-spin | recv-busy | −10.90 | 22.44 | −7.94 | 11.9 % | 8.42 / 50.16 | 4.16 / 21.86 |
| burst | recv-busy | uring-spin | 9.16 | 261.58 | 6.28 | 74.5 % | 4.83 / 23.18 | 10.22 / 28.42 |
| burst | uring-spin | recv-busy | −8.60 | 19.13 | −5.76 | 18.5 % | 7.23 / 23.03 | 3.26 / 21.40 |
| 1× replay | recv-busy | uring-sqpoll | 0.86 | 27.32 | −0.32 | 54.8 % | 3.76 / 16.75 | 4.72 / 17.53 |
| 1× replay | uring-sqpoll | recv-busy | −0.88 | 34.79 | −0.21 | 48.0 % | 4.49 / 23.59 | 3.62 / 22.77 |
| burst | recv-busy | uring-sqpoll | 5.62 | 59.78 | 3.36 | 63.5 % | 4.98 / 28.35 | 10.03 / 30.54 |
| burst | uring-sqpoll | recv-busy | −3.70 | 43.77 | −2.90 | 40.0 % | 9.35 / 30.05 | 7.90 / 26.09 |
| 1× A/A | recv-busy | recv-busy | 0.13 | 41.79 | | | | |

- **NIC polling decodes about 11 µs earlier** than the interrupt-driven loop at 1× replay, in both core orders, on 88–92 % of messages; about 9 µs under bursts. Most of it is before the stack: the polled socket's packets enter the kernel 8 µs earlier at the median and the interrupt path's arrival tail reaches 429 µs at p99 (Δrx). The polled loop processes its packets on its own core; the interrupt-driven loop's arrive on the IRQ core.
- **`recv-busy` and `uring-sqpoll` are equal** at 1× (±0.9 µs, inside the A/A spread); under bursts `recv-busy` is 4–6 µs ahead in both orders.
- **Cost:** `recv-busy` makes about 1.2 million syscalls/s and spends about 79 % of its core in the kernel; `uring-sqpoll` makes none on the loop thread, and its kernel thread occupies CPU 6 the whole time.

### 3. Order-path round trip (WebSocket echo, p50 / p99 / p99.9)

Run `20261014T060634Z`:

| rate | recv-busy | uring-sqpoll | uring-spin | epoll-spin | uring-napi (wait 0) |
|---|---|---|---|---|---|
| 100/s | 64.9 / 99.2 / 135 | 60.8 / 93.6 / 118 | 79.6 / 261 / 2,689 | 78.7 / 630 / 3,023 | 65.7 / 106 / 287 |
| 1,000/s | 48.0 / 67.4 / 154 | 55.4 / 81.0 / 95 | 54.2 / 411 / 2,221 | 51.8 / 298 / 2,612 | 48.6 / 82.3 / 729 |
| 10,000/s | 45.5 / 61.1 / 78.5 | 47.0 / 62.5 / 79.7 | 51.3 / 81.2 / 371 | 49.6 / 79.8 / 347 | 47.1 / 66.2 / 90.4 |
| back to back | 44.6 / 64.6 / 72.3 | 44.8 / 63.5 / 73.6 | 48.9 / 73.6 / 159 | 59.3 / 82.2 / 257 | 48.0 / 69.7 / 112 |

Loop behaviour in those runs: 0 sleeps for the four busy-spin loops; `uring-napi` with a zero wait slept 72,339–253,016 times per 30 s and its longest iteration was 4.6–7.5 ms.

- **The interrupt path's tails differ between VMs.** In the earlier run (`20261014T005221Z`) and the confirmation run, on the same VM size and setup, `epoll-spin` and `uring-spin` had p99 71–74 µs at 1,000/s; in this run 298–411 µs, with p99.9 above 2 ms: one VM in three. The NIC-polling loops were tight on all three (p99 64–81 µs). Polling removes the exposure to whatever the interrupt path is doing on a given host.
- What the round trip contains: two TLS hops, Azure VM to VM in one placement group, and the echo server's own busy loop.

### 4. HTTP clients

One request in flight, `POST` with a 242-byte form body and a 300-byte JSON response, p50 / p99 / p99.9, two rounds each:

| client | 1,000 req/s round 1 | 1,000 req/s round 2 | back to back round 1 | back to back round 2 |
|---|---|---|---|---|
| ergon-io `recv-busy` | 60.6 / 69.0 / 82.4 | 60.7 / 67.6 / 86.0 | 55.5 / 69.4 / 80.3 | 55.5 / 68.9 / 77.2 |
| ergon-io `recv-busy`, background connect | 59.3 / 69.0 / 86.2 | 61.3 / 71.7 / 82.2 | 54.0 / 68.6 / 76.5 | 53.9 / 68.7 / 79.5 |
| ergon-io `uring-sqpoll` | 60.8 / 71.8 / 87.8 | 62.9 / 70.0 / 84.9 | 56.6 / 69.8 / 80.6 | 54.5 / 68.9 / 80.5 |
| ergon-io `epoll-spin` | 68.4 / 79.3 / 100.3 | 67.9 / 77.7 / 95.5 | 63.4 / 74.5 / 93.0 | 49.5 / 73.2 / 84.0 |
| ergon-io `uring-spin` | 69.6 / 79.0 / 95.8 | 69.3 / 77.8 / 94.9 | 58.4 / 73.4 / 86.5 | 61.7 / 76.1 / 94.6 |
| libcurl 8.23.0, source build (c-ares, `-O3 -march=native`, HTTP only) | 79.7 / 95.7 / 126.6 | 77.8 / 88.1 / 125.1 | 73.9 / 92.7 / 116.4 | 75.7 / 94.0 / 111.7 |
| libcurl 8.5.0, Ubuntu shared library | 80.3 / 91.3 / 120.6 | 79.1 / 92.6 / 143.3 | 81.3 / 96.4 / 120.2 | 81.0 / 95.2 / 112.8 |
| libcurl 8.22.0, curl-sys static (vendored OpenSSL 3.6.3) | 80.3 / 92.0 / 131.7 | 79.8 / 102.4 / 140.5 | 79.7 / 95.4 / 113.0 | 80.1 / 95.1 / 113.0 |
| reqwest 0.12 on tokio (current thread) | 86.4 / 97.5 / 131.5 | 87.1 / 98.5 / 137.4 | 85.5 / 99.4 / 119.0 | 87.8 / 104.9 / 134.5 |

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

Order-path round trip (WebSocket echo, p50 / p99 / p99.9):

| rate | recv-busy | uring-sqpoll | uring-spin | epoll-spin |
|---|---|---|---|---|
| 100/s | 59.0 / 69.2 / 85.9 | 57.3 / 70.0 / 85.9 | 65.7 / 77.0 / 99.6 | 63.4 / 74.2 / 84.4 |
| 1,000/s | 57.3 / 64.8 / 71.0 | 58.0 / 65.1 / 71.1 | 63.1 / 73.9 / 84.7 | 62.9 / 73.2 / 82.6 |
| back to back | 54.5 / 68.0 / 71.8 | 54.6 / 68.5 / 73.0 | 57.9 / 72.2 / 80.5 | 59.2 / 73.4 / 79.9 |

HTTP (p50 / p99 / p99.9):

| client | 1,000 req/s | back to back |
|---|---|---|
| ergon-io `recv-busy` | 58.4 / 69.7 / 73.9 | 55.4 / 68.6 / 72.6 |
| ergon-io `uring-sqpoll` | 60.7 / 64.9 / 69.5 | 41.6 / 65.8 / 71.4 |
| ergon-io `epoll-spin` | 65.3 / 71.5 / 80.0 | 59.5 / 72.7 / 81.2 |
| ergon-io `uring-spin` | 65.7 / 78.0 / 92.6 | 59.2 / 73.9 / 81.7 |
| libcurl 8.23.0, source build | 73.9 / 85.4 / 93.0 | 78.0 / 89.0 / 95.2 |
| libcurl 8.5.0, distro | 75.2 / 86.8 / 93.7 | 79.6 / 92.1 / 101.2 |

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

Run `confirm-20261014T093250Z` (East Asia, `ONLY=client`), tables in [`results/confirm-20261014T093250Z/tables.md`](results/confirm-20261014T093250Z/tables.md). One request in flight, the same order-sized `POST`, two rounds; p50 / p99 / p99.9:

| path | 1,000 req/s round 1 | 1,000 req/s round 2 | back to back round 1 | back to back round 2 |
|---|---|---|---|---|
| `HttpClient` native, `recv-busy` | 53.9 / 63.5 / 67.7 | 55.0 / 63.9 / 69.2 | 47.4 / 66.2 / 70.4 | 40.2 / 53.0 / 61.9 |
| `HttpClient` native, `uring-sqpoll` | 55.2 / 64.4 / 67.3 | 55.2 / 64.6 / 68.3 | 51.0 / 67.0 / 72.0 | 51.9 / 68.4 / 76.4 |
| `HttpConn` directly, `recv-busy` | 53.5 / 62.5 / 66.5 | 54.8 / 63.3 / 73.3 | 51.5 / 67.1 / 73.6 | 51.1 / 66.9 / 70.9 |
| `HttpClient` libcurl 8.5.0 (distro), `perform` | 73.7 / 85.1 / 93.3 | 74.7 / 84.2 / 94.4 | 76.8 / 91.2 / 96.9 | 76.9 / 90.6 / 96.3 |
| `HttpClient` libcurl 8.5.0 (distro), `socket_action` | 75.8 / 86.1 / 91.9 | 75.9 / 85.4 / 96.3 | 77.5 / 91.4 / 100.0 | 77.6 / 92.2 / 100.6 |
| `HttpClient` libcurl 8.23.0 (source), `perform` | 70.3 / 82.6 / 90.2 | 73.8 / 85.5 / 92.9 | 74.3 / 90.0 / 99.7 | 74.6 / 90.2 / 98.7 |
| `HttpClient` libcurl 8.23.0 (source), `socket_action` | 76.4 / 99.8 / 114.5 | 74.9 / 83.7 / 89.8 | 74.7 / 89.8 / 102.2 | 75.6 / 90.9 / 102.2 |
| libcurl 8.5.0 directly (`curl` crate, `perform`) | 71.7 / 83.5 / 93.8 | 71.1 / 82.2 / 87.4 | 76.6 / 90.5 / 98.1 | 55.0 / 67.0 / 75.8 |

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
| round trip p50 / p99 at 1,000/s | 71.0 / 84.2 | 57.0 / 79.4 | 57.8 / 64.1 |
| with budget = wait = 200 µs: stack share p50 | 11.9–18.1 | 3.7–4.2 | |
| with budget = wait = 200 µs: round trip p50 / p99 at 1,000/s | 70.8 / 79.2 | 56.2 / 63.3 | |

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

## Limits

- **One VM size, two regions, one kernel.** MANA behind netvsc and Azure's virtual network are in the path; the interrupt path's tails varied between VM instances (section 3).
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

# Azure runs (Azure CLI logged in; each creates and deletes one ergon-lab-* resource group)
io/benches/scripts/azure-run.sh        # busy-spin loops: paired lab, round trips, live Binance
io/benches/scripts/azure-http.sh       # HTTP clients (LOC=westus3 by default)
io/benches/scripts/azure-confirm.sh    # final library: round trips, HTTP, reconnect cost
ONLY=client io/benches/scripts/azure-confirm.sh   # HttpClient: native against libcurl
io/benches/scripts/azure-tls-bench.sh  # TLS record decryption and request signing (SKIP_TLS=1: signing only)

# analysis
cargo run --release --manifest-path io/benches/Cargo.toml --bin analyse -- results/<stamp>/res/<run>...
```

Every script snapshots the source at start, runs on the VMs detached, records the environment (`env-rcv.txt`), analyses on the receiver, downloads the tables and small raw records, and deletes the resource group on every exit path, then verifies it is gone.
