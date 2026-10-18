# HTTP client

`HttpClient` keeps a pool of warm HTTP/1.1 connections behind one API. The
implementation is configuration:

| `HttpImpl` | What it is | Use it when |
|---|---|---|
| `Native` (default) | ergon-io's own client on a `Driver`: each request is one TLS record, each response is parsed in place | latency matters: on the same interrupt-driven path (`epoll-spin`), 7–11 µs faster than the source-built libcurl at the median, paced, 1 connection; the p99 was not always lower ([measured results](#measured-results)) |
| `Curl` (feature `curl`) | libcurl, linked in-process, driven without blocking | the endpoint needs redirects, proxies, HTTP/2, other auth schemes or compression |

- Same code for both: switching is a configuration change.
- Linux only (like the `Driver`).
- One level down, `HttpConn` is a single keep-alive connection on your own
  `Driver` (the API reference has it).

## Example

The handler gets every result, keyed by request id:

```rust,ignore
{{#include ../../../io/tests/client.rs:client_handler}}
```

Native:

```rust,ignore
{{#include ../../../io/tests/client.rs:native}}
```

libcurl:

```rust,ignore
{{#include ../../../io/tests/client.rs:curl}}
```

One request, the same code for both:

```rust,ignore
{{#include ../../../io/tests/client.rs:get}}
```
*(From `io/tests/client.rs`. `cargo test -p ergon-io` runs `get` through
the native client against a local TLS server on Linux; with
`--features curl`, through libcurl too. `lab` is that server: in your code
use `Target::new("api.binance.com", 443, "")` and
`tls::client_config(tls::webpki_roots())?`.)*

## Rules

- **One request in flight per connection.** All busy: `Error::Busy` at
  once, no hidden queue. Each pooled connection is one more socket the loop
  polls: read [many connections](#many-connections) before sizing `pool`.
- **A timed-out request closes its connection** (`request_timeout_ms`,
  5 s): HTTP/1.1 cannot drop one request on a live connection. The pool
  opens a replacement. Both implementations report it as
  `Error::Timeout("http request")`.
- **A failed request says whether it was sent:**
  `on_error(request, sent, error)`.
  - `Sent::No`: the request never reached the kernel whole, so the server
    cannot have seen it. Safe to send again.
  - `Sent::Maybe`: it may have run. Look an order up by its client order
    id; never send it again blindly.
- **libcurl takes no method but GET, HEAD, OPTIONS and DELETE**
  (`Error::CurlResend`) unless `curl_allow_non_idempotent_methods` is
  set. Why: libcurl sends a request again, up to 5 times, when the reused
  connection it went out on dies before any reply byte, so an order could
  be placed twice (`io/tests/client.rs` shows it happening). The native
  client never re-sends: send orders through it.
- **Connections open in the background** (`background_connect`, on by
  default): the handshake's CPU never lands on the loop. A failed connect
  goes to `ClientHandler::on_connect_error` and is retried with backoff.
- **Reconnects spread out:** a dropped connection reopens after a random
  delay of up to 100 ms (the limit doubles per failed connect, up to 5 s),
  so connections that drop together do not reconnect together.
  `reconnect_seed` fixes the delays (tests).
- **A venue's connect limit** (native only): with `connect_gate`, `new`
  opens nothing and every connect waits for the gate passed to
  `poll_gated`; `poll` then refuses. A refusal does not grow the backoff.
  Give the pool a `Budget` bucket of its own: venues' connect limits count
  WebSocket connects.
- **`buf_size`** (1 MiB) is each native connection's receive buffer: it
  must hold the largest response.
- **Responses:** `Content-Length` or chunked bodies. The body in
  `on_response` borrows the client's buffer: copy what you keep.
- **libcurl:** `curl_drive` picks `curl_multi_perform` on every poll
  (default, 1–6 µs faster) or `curl_multi_socket_action`; both never block.
  Trust comes from `curl_ca_file` or the system store, not from the rustls
  configuration.

## Signing

`Signer` signs request parameters as Binance's signed endpoints expect,
without allocating. `HttpClient::signed_request` appends the timestamp and
the signature itself; `signed_request_at` takes the timestamp from you
(your clock, or replay time).

| signer | cost per request (x86, `D8ds_v6`) | use it for |
|---|---|---|
| `Signer::hmac_sha256` | 0.26 µs | signed REST requests |
| `Signer::ed25519_pkcs8` | 21.6 µs | log-on, once per session; the private key never leaves the machine |

Checked against Binance's documented example:

```rust,ignore
{{#include ../../../io/tests/auth.rs:signing}}
```
*(From `io/tests/auth.rs`; `cargo test -p ergon-io` runs it on every
platform.)*

- Set the API key once as a header:
  `Target::with_header("X-MBX-APIKEY", key)`.

## Measured results

Round trip, **1 connection**, one request in flight, a `POST` with a
242-byte body and a 300-byte JSON answer. p50 / p99 / max, µs.
Environments as in [Measured results](overview.md#measured-results). More
connections: [not measured](#many-connections).

‡ **Interrupt-driven, measured with the NIC's adaptive interrupt
moderation ON** (ENA's default, `rx-usecs` 20; not a tuned host), and the
NIC's interrupts on the shared housekeeping CPUs, deep idle states on. These
rows include the moderation delay, so they are not a fair comparison with
the polling rows (`recv-busy`, `uring-sqpoll`). The ‡ rows share that
setting with each other. A fair rerun is planned.

**AWS Tokyo**, `c7i.2xlarge`, adaptive RX on, two rounds
(`flags-aws-20261015T051742Z`, 2026-10-15):

| client | 10,000/s | back to back |
|---|---|---|
| `HttpConn`, `recv-busy` | 44.6–44.9 / 53.1–53.6 / 226–454 | 42.0–42.2 / 50.2–50.3 / 154–288 |
| `HttpClient` (pool of 1), `recv-busy` | 46.5–46.9 / 56.0–58.0 / 239–321 | 42.8–43.0 / 50.3–50.8 / 350–1,028 |
| `HttpConn`, `uring-sqpoll` | 45.3–46.0 / 53.2–53.6 / 181–288 | 43.9–44.3 / 51.5–51.9 / 213–439 |
| `HttpConn`, `epoll-spin` ‡ | 48.6–49.4 / 90.4–96.2 / 340–1,014 | 48.9–49.0 / 96.5–96.9 / 194–226 |
| `HttpConn`, `uring-spin` ‡ | 49.5–50.3 / 92.6–97.0 / 977–1,055 | 48.2–48.4 / 95.8–96.1 / 238–239 |
| libcurl 8.23.0, source build with c-ares ‡ | 56.0–56.5 / 73.8–100.8 / 262–345 | 54.3–55.2 / 67.7–68.6 / 153–269 |
| libcurl, distro ‡ | 58.5–61.5 / 76.8–101.1 / 240–254 | 58.1–61.9 / 75.1–75.4 / 209–296 |
| reqwest on tokio ‡ | 245 / 353–409 / 483–584 | 245 / 349–351 / 485–497 |

**Azure East Asia**, `D8ds_v6`, one run (`confirm-20261014T085509Z`,
2026-10-14); the NIC's interrupt settings were not recorded:

| client | 1,000/s | back to back |
|---|---|---|
| `HttpConn`, `recv-busy` | 58.4 / 69.7 / 151 | 55.4 / 68.6 / 442 |
| `HttpConn`, `uring-sqpoll` | 60.7 / 64.9 / 1,416 | 41.6 / 65.8 / 723 |
| `HttpConn`, `epoll-spin` | 65.3 / 71.5 / 348 | 59.5 / 72.7 / 915 |
| `HttpConn`, `uring-spin` | 65.7 / 78.0 / 333 | 59.2 / 73.9 / 463 |
| libcurl 8.23.0, source build | 73.9 / 85.4 / 471 | 78.0 / 89.0 / 1,258 |
| libcurl 8.5.0, distro | 75.2 / 86.8 / 249 | 79.6 / 92.1 / 667 |

**Azure East Asia**, both implementations through `HttpClient` (pool of
1), 1,000 requests/s, two rounds (`confirm-20261014T093250Z`). Native polls
the NIC here and libcurl does not, so the gap mixes client work with NIC
polling; the NIC's interrupt settings were not recorded:

| implementation | p50 / p99 / max |
|---|---|
| native, `recv-busy` | 53.9–55.0 / 63.5–63.9 / 95–1,532 |
| native, `uring-sqpoll` | 55.2 / 64.4–64.6 / 88–340 |
| libcurl 8.5.0, distro | 73.7–74.7 / 84.2–85.1 / 203–280 |
| libcurl 8.23.0, source build | 70.3–73.8 / 82.6–85.5 / 839–1,486 |

- **NIC polling against libcurl:** on AWS, no conclusion (‡). On Azure
  (`confirm-20261014T085509Z`, 1,000/s) `recv-busy` was 15–17 µs ahead of
  both libcurl builds at the median (p99 69.7 against 85.4–86.8); that run
  did not record the NIC's interrupt settings.
- **The same interrupt-driven path as libcurl:** `epoll-spin` is about
  7 µs ahead of the source-built libcurl at the median on AWS (10,000/s)
  and about 9 µs on Azure (1,000/s, p99 71.5 against 85.4). But on AWS back
  to back its p99 was worse: 96.5–96.9 against 67.7–68.6.
- **`HttpClient` with a pool of 1** costs 1–2 µs over a raw `HttpConn` on
  AWS, within 0.4 µs on Azure. A larger pool was not measured.
- **reqwest parks its thread** between requests: about 200 µs more per
  request than `epoll-spin` (the same interrupt path) on AWS. Most likely
  (not traced) that is the receiver waking from a deep idle state: its
  idle states were left on, and a C6 exit takes about 190 µs on `c7i`. On
  Azure West US 3 the gap was 18–19 µs (`http-20261014T074036Z`, 1,000/s:
  reqwest 86.4–87.1 / 97.5–98.5 / 246–294 against `epoll-spin` 67.9–68.4 /
  77.7–79.3 / 212–254).
- **AWS max** (0.15–1.06 ms above): neither loop stalled for most of them
  (network path, hypervisor or untraced); a few were a timer softirq. The
  1 ms ones are the rare 1 ms round trip seen on every client and backend.
- **Azure max:** not traced in these runs. Azure's traced runs put every
  maximum in a once-a-minute hypervisor stall of 0.5–3.7 ms.
- More, with every round:
  [HTTP clients](https://github.com/mimran1980/ergon/blob/main/io/benches/BENCHMARK.md#4-http-clients),
  [HttpClient: native against libcurl](https://github.com/mimran1980/ergon/blob/main/io/benches/BENCHMARK.md#httpclient-native-against-libcurl),
  [HTTP clients on AWS](https://github.com/mimran1980/ergon/blob/main/io/benches/BENCHMARK.md#http-clients-on-aws),
  [why native is faster](https://github.com/mimran1980/ergon/blob/main/io/benches/BENCHMARK.md#how-the-ergon-io-http-client-differs-from-libcurl-and-why-it-is-faster).

## Many connections

**Not measured.** Every figure above is 1 connection. What the data
supports for 20 pooled connections on one loop, one request in flight,
10,000/s. The measured column is AWS `c7i.2xlarge`, 1 connection, adaptive
RX on (`flags-aws-20261015T051742Z`):

| backend | what 20 connections change | 1 connection (`HttpConn`), measured (p50 / p99 / max) | 20 connections, estimate (p50 / p99) |
|---|---|---|---|
| `recv-busy` (the default) | a poll reads all 20 sockets: about 12 µs (16 sockets: 9.7 µs, measured). The response waits half a poll on average, up to one | 44.6–44.9 / 53.1–53.6 / 226–454 | about 50 / 65 |
| `recv-busy,per_queue=true` | a poll reads one socket per NIC queue: 2–6 µs at 4–256 sockets | not measured for HTTP | about 47 / 58 |
| `uring-sqpoll` | none: its kernel thread completes every socket into one ring | 45.3–46.0 / 53.2–53.6 / 181–288 | about the same as 1 connection |
| `epoll-spin` | none: `epoll_wait` returns only the ready sockets | ‡ 48.6–49.4 / 90.4–96.2 / 340–1,014 | no scan cost added; not comparable with the rows above (‡) |

- **An estimate has no max.** Measure it (below). `HttpClient` adds 1–2 µs
  over `HttpConn` (pool of 1, AWS).
- **Of the polling backends, `per_queue` should be best on one core;**
  `uring-sqpoll` needs a spare core. Poll costs:
  [cost per poll](backends.md#cost-per-poll).
- **No estimate against `epoll-spin`:** its 1-connection figures had
  adaptive moderation on (‡).
- **Rule:** count the pool with every other socket the thread polls:
  feeds, order connections, other pools. Each `HttpClient` has its own
  `Driver`, so its poll adds to theirs. Plain `recv-busy` up to about 4
  sockets in all; above that `recv-busy,per_queue=true`, or `uring-sqpoll`
  with a spare core.
- **Set it** with `HttpClientConfig::driver`, for example
  `Config::parse("recv-busy,per_queue=true")?`.

Measure it on your own two hosts with `io/benches`: one run per backend,
and `--pool 1` as the reference.

```sh
cargo build --release --manifest-path io/benches/Cargo.toml --bin replay_server --bin http_bench --bin analyse
B=io/benches/target/release

# sender: a keep-alive HTTP server; copy cert-http.pem to the receiver.
# It spins one thread per connection: run it unpinned, with more free CPUs than connections.
sudo $B/replay_server --listen 0.0.0.0:9445 --cert-out cert-http.pem --mode http --busy-poll 50

# receiver, loop on isolated CPU 2: 20 pooled connections, one request in flight, 10,000/s
sudo $B/http_bench --client client --backend recv-busy --pool 20 --server SENDER:9445 --ca cert-http.pem \
  --sni lab.local --cpu 2 --rate 10000 --duration 60 --out res/http20-recv-busy
# then the same with: --backend recv-busy --per-queue (out res/http20-per-queue),
# --backend epoll-spin (res/http20-epoll-spin), --backend uring-sqpoll --sqpoll-cpu 3 (res/http20-sqpoll)

$B/analyse --warmup-s 2 res/http20-*
```

- On AWS, give each family its own best NIC setting. Polling runs:
  `ethtool -C <nic> adaptive-rx off`. The `epoll-spin` run:
  `adaptive-rx off rx-usecs 0 tx-usecs 0`, with the NIC's interrupts on a
  core of their own ([ergon-tune N1](../tune/overview.md)).
- Compare p50, p99 and max per backend; repeat in reversed order.
