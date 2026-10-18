# WebSocket

`WsConn` is a TLS 1.3 WebSocket client that a `Driver` owns.

- Frames are parsed in place: `WsHandler::on_message` gets a slice of the
  buffer the kernel wrote into (one copy per byte).
- `RxMeta` carries the read's kernel receive timestamp and, where the NIC
  has one, its hardware timestamp.
- Timers (Pings, timeouts, the close wait) run on the driver's deadline
  check (every 250 µs), never on the read or send path.

## A complete client

Connect, subscribe, print each message, reconnect and subscribe again after
a drop, then close cleanly:

```rust,ignore
{{#include ../../../io/examples/websocket.rs:example}}
```
*(From `io/examples/websocket.rs`. `cargo test -p ergon-io` builds it, and
`io/tests/example_websocket.rs` runs it against a local server on every
backend: a server close, the reconnect, the second subscribe and the clean
close.)*

Run it (Linux):

```sh
cargo run -p ergon-io --features webpki-roots --example websocket -- 20 recv-busy
```

- Arguments: messages to print before closing, then the backend
  configuration.
- The feed is Binance's public spot stream; any public feed will do.

## Options

`WsOptions`, on `WsDial::with_options` or `WsConn::connect_with`:

| option | default | what it does |
|---|---|---|
| `buf_size` | 1 MiB | receive buffer; every frame must fit |
| `max_message` | 0 (what the buffer holds) | a larger message closes the connection; a fragmented message may exceed the buffer |
| `ping_interval_ms` | 10,000 | Ping after this long with nothing received; a busy connection sends none; 0: off |
| `pong_timeout_ms` | 5,000 | nothing received this long after a Ping: `Error::Timeout("websocket pong")`; 0: off |
| `idle_timeout_ms` | 0 (off) | nothing received this long: `Error::Timeout("websocket idle")` |
| `close_timeout_ms` | 1,000 | longest wait for the other side of a close handshake; 0: no wait |
| `validate_utf8` | off | invalid text closes the connection; costs one more pass over each text message |
| `protocols` | none | subprotocols offered; `WsConn::protocol` is the server's pick |

- Any byte received after a Ping counts as the answer, not only a Pong.
- Server Pings are answered at once.

## Close

| who starts | what happens | `on_closed` gets |
|---|---|---|
| the server | its Close is answered with the same code; then the client waits up to `close_timeout_ms` for the server to close TCP | `Error::WsClose`, `by_server: true` |
| the client: `close(code, reason)` | sends a Close; nothing more can be sent; waits up to `close_timeout_ms` for the server's Close | `Error::WsClose`, `by_server: false`; or `Error::Timeout("websocket close")` |
| the client: `Driver::close(id)` | drops the socket at once, no Close | nothing |

- Data that arrives after a server Close is dropped.
- A protocol error drops the connection (RFC 6455 allows it): no Close 1002.

## Reconnect

`Reconnect` is optional. You own it (in your handler, as in the example)
and feed it every open and close.

| `Retry` field | default | meaning |
|---|---|---|
| `initial_ms` | 250 | first delay after a close |
| `multiplier` | 2 | each failed attempt multiplies the delay |
| `max_ms` | 30,000 | longest delay |
| `jitter` | 0.2 | each delay cut by up to 20 %, at random |
| `seed` | 0 (random) | a fixed seed repeats the delays (tests) |
| `max_attempts` | 0 (never give up) | failed attempts in a row before `GaveUp` |
| `stable_ms` | 60,000 | open this long resets the backoff |
| `background` | true | connect with `connect_background` (handshake off the loop) |

- `on_open(id)` returns `Opened { link, reconnect }`: subscribe either way;
  with `reconnect`, resync any state kept from the old connection.
- `on_closed(id)` returns the next step: `Retry`, `GaveUp` or `Stopped`.
- `stop(link)` vetoes reconnects; `resume(link)` starts again.
- `Driver::close` reports nothing: call `on_closed(id)` yourself after it.
- `poll` starts due attempts. It costs one branch while nothing waits.

### Connect limits and the server's wait

Venues limit connect attempts per IP (Binance: 300 per 5 minutes; OKX: 3
per second). One `Budget` bucket is the gate of every link on that IP:

```rust,ignore
{{#include ../../../io/tests/reconnect.rs:gate}}
```

```rust,ignore
{{#include ../../../io/tests/reconnect.rs:gate_poll}}
```
*(From `io/tests/reconnect.rs`, which `cargo test -p ergon-io` runs on
every platform, with a stand-in connector and a fake clock.)*

- `poll_gated(driver, gate)` starts an attempt only when the gate passes
  it, the first attempt after `add` included: 50 links added at once start
  at the gate's pace, not in one burst.
- A refusal is not a failed attempt: the backoff does not grow. The link
  asks again after the gate's wait, at most `max_ms` later.
- Each pass starts after the last link the gate passed, so links that keep
  failing cannot starve the others.
- `defer(link, wait_ns)`: no attempt before then. It stays with the link
  across later closes and `resume`.

A refused handshake, in `on_closed`:

```rust,ignore
{{#include ../../../io/tests/reconnect.rs:refused}}
```

- 429: the bucket is held for `Retry-After` (or its backoff without one).
- 418: the bucket is halted. No link on that IP connects again until a
  person calls `Budget::clear_halt`, even after the server's wait.
- Give the HTTP pool a bucket of its own ([HTTP client](http-client.md)):
  venues count WebSocket connects.

## Sending

- `send_text`, `send_binary`, `ping`, or `send_frame(opcode, payload)`.
- A message larger than one TLS record goes out over several.
- On a driver-owned connection: `driver.get_mut(id)`, then the same methods.
- Sends go out on the next `poll`, or at once with `Driver::flush`.
- Handlers cannot send (they have no driver): remember the id, send after
  `poll` returns (as the example does).
- `stats().auto_frames` counts the frames the connection sends by itself:
  Pongs, Pings on a quiet connection, the echo of a server Close. A venue
  that limits messages per connection may count them (Binance spot does):
  charge the change each pass with `Budget::force`.

## Handshake

- Checks status 101, `Upgrade`, `Connection` and `Sec-WebSocket-Accept`.
- Any other status is `Error::Upgrade { status, retry_after_ms, .. }`: a
  429 or 418 can be told apart, and `retry_after_ms` is the server's
  `Retry-After` (seconds or a date). Wait at least that long before
  connecting again: `outcome::refused_upgrade` and `Reconnect::defer`
  (see [Reconnect](#connect-limits-and-the-servers-wait)).
- Refuses a subprotocol it did not offer.
- Offers no extension and refuses a server that picks one.
- Extra headers (auth, say) go on the `Target`:
  `Target::new(host, port, path).with_header(name, value)`.

## Not supported

- Compression (`permessage-deflate`) and other extensions: decompressing
  breaks in-place parsing and costs CPU per message.
- Proxies.
- Sending fragmented messages (receiving them works).

## Measured results

Order round trip: a 64-byte echo, one message in flight, **1 connection**.
p50 / p99 / max, µs. More sockets on the same loop: not measured (see the
last bullet).

‡ **Measured with the NIC's adaptive interrupt moderation ON** (ENA's
default, `rx-usecs` 20; not a tuned host), and the NIC's interrupts on the
shared housekeeping CPUs, deep idle states on. These interrupt-driven rows
include the moderation delay, so they are not a fair comparison with the
polling rows (`recv-busy`, `uring-sqpoll`). A fair rerun is planned.

**AWS Tokyo**, `c7i.2xlarge` VM, adaptive RX on
(`flags-aws-20261015T045527Z`, 3–4 baselines):

| backend | back to back | 10,000/s |
|---|---|---|
| `recv-busy` | 39.5–42.0 / 45.5–49.7 / 130–1,038 | 41.0–41.8 / 45.4–48.4 / 128–255 |
| `uring-sqpoll` | 40.0–41.0 / 45.5–47.0 / 176–862 | 41.7–42.9 / 47.3–50.6 / 127–206 |
| `uring-spin` ‡ | 47.7–48.4 / 55–91 / 197–1,035 | 45.4–48.0 / 51–94 / 432–982 |
| `epoll-spin` ‡ | 47.1–49.0 / 55–98 / 188–369 | 47.2–48.3 / 53–91 / 127–982 |

**AWS Tokyo**, another VM pair, with mio and the SQPOLL thread on the
loop's sibling (`flags-aws-20261015T124649Z`, 2 baselines):

| backend | back to back | 10,000/s |
|---|---|---|
| `recv-busy` | 36.3–37.4 / 51.7–51.8 / 902–1,016 | 38.3–38.8 / 50.5–54.0 / 640–961 |
| `uring-sqpoll`, its own core | 38.7–38.8 / 53.0–55.2 / 165–180 | 38.9–39.6 / 53.5–53.6 / 233–1,124 |
| `uring-sqpoll`, the loop's sibling | 38.3–38.5 / 51.5–52.6 / 281–432 | 40.7–40.8 / 55.9–56.7 / 700–913 |
| `epoll-spin` ‡ | 43.2–45.3 / 68.1 / 230–1,018 | 43.4–49.9 / 89–94 / 210–243 |
| mio + rustls ‡ | 43.7–48.7 / 61–93 / 207–304 | 45.6–47.8 / 88–90 / 225–519 |

**AWS bare metal**, `c7i.metal-24xl` receiver, adaptive RX on too
(`flags-aws-20261015T142240Z`, 2 baselines):

| backend | back to back | 10,000/s |
|---|---|---|
| `recv-busy` | 35.3–35.7 / 43.3–43.4 / 160–206 | 37.1–37.8 / 43.9–44.8 / 113–963 |
| `uring-sqpoll` | 38.1–39.7 / 45.5–53.8 / 332–429 | 38.2–40.2 / 50.2–52.7 / 169–852 |
| `uring-sqpoll`, the loop's sibling | 37.8–38.4 / 45.3–46.4 / 312–859 | 41.0–41.3 / 48–74 / 766–881 |
| `epoll-spin` ‡ | 39.4–41.1 / 56–67 / 193–380 | 40.0–41.2 / 51 / 149–928 |

**Azure East Asia**, `D8ds_v6` (`confirm-20261014T085509Z`, 1 run; the
NIC's interrupt settings were not recorded):

| backend | 1,000/s | back to back |
|---|---|---|
| `recv-busy` | 57.3 / 64.8 / 143 | 54.5 / 68.0 / 1,081 |
| `uring-sqpoll` | 58.0 / 65.1 / 441 | 54.6 / 68.5 / 642 |
| `uring-spin` | 63.1 / 73.9 / 1,061 | 57.9 / 72.2 / 890 |
| `epoll-spin` | 62.9 / 73.2 / 279 | 59.2 / 73.4 / 612 |

- **AWS, polling against interrupt-driven: no conclusion.** The ‡ rows
  had adaptive moderation on; a fair rerun is planned.
- **Azure** (`confirm-20261014T085509Z`, 1 connection): NIC polling was
  4–6 µs ahead of `epoll-spin` at the median and 5–8 µs at p99, with no
  lower max. That run did not record the NIC's interrupt settings.
- **`epoll-spin`'s p99 varied** from 53 to 98 µs between AWS baselines (‡);
  the cause was not traced.
- **The max is not lower with polling.** On AWS about 1 round trip in 1.8
  million takes about 1 ms, on every backend and on bare metal too. Neither
  loop stalled, so it is the network path or the sender VM
  ([worst cases on AWS](https://github.com/mimran1980/ergon/blob/main/io/benches/BENCHMARK.md#worst-cases-on-aws)). Azure's traced runs put
  every maximum in a once-a-minute hypervisor stall of 0.5–3.7 ms
  ([settings matrix](https://github.com/mimran1980/ergon/blob/main/io/benches/BENCHMARK.md#settings-matrix)).
- **mio** (the usual Rust way): level with `epoll-spin` at the median
  (`124649Z`, both ‡).
- **The SQPOLL thread on the loop's sibling** costs −0.4 to +1.8 µs at the
  median against a core of its own.
- **More sockets on the loop: not measured.** With plain `recv-busy` each
  extra socket adds about 0.6 µs to every poll
  ([cost per poll](backends.md#cost-per-poll)). With N other sockets,
  expect about N × 0.3 µs more at the median and up to N × 0.6 µs at p99,
  plus the time spent on their messages. Past 4 sockets in all, use
  `recv-busy,per_queue=true` or `uring-sqpoll`.
- **Feeds** (market data, 1 to 256 connections):
  [by number of connections](overview.md#by-number-of-connections).
