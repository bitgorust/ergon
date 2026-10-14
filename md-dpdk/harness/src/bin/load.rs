//! Round-trip latency under a fixed offered load, open loop, through an
//! external media driver (`AERON_DIR`). The far end echoes each message; this
//! side publishes on a fixed schedule regardless of replies and stamps each
//! message with its *intended* send time, so time spent back-pressured or
//! queued counts against the latency (no coordinated omission).
//!
//! load xping <ping endpoint> <pong endpoint> <msgs/s> <seconds> <size> <cpu|-> [burst]
//!     one stream, echoed by `rtt xpong` (streams 1002 -> 1003)
//! load xmix <ping endpoint> <pong endpoint> <seconds> <cpu|-> <name:stream:msgs/s:size[:burst]>...
//!     several streams at once on the same channels, each on its own schedule
//!     (a mix of rates, sizes and bursts), echoed by `load xecho` on stream + 1000
//! load xecho <ping endpoint> <pong endpoint> <cpu|-> <stream>...
//!     echoes each stream back on stream + 1000 until every ping image goes
//!
//! `burst` messages (default 1) are due together, `burst / rate` seconds
//! apart. The first second is warm-up, not recorded. Prints one line per stream:
//! <load|mix>,<label>,<udp|udp-name>,<size>,<rate>,<burst>,p50,p90,p99,p99.9,p99.99,max,mean (ns),
//! <samples>,<sent>,<received>,<back-pressured messages>,<achieved msgs/s>,<max send lag ns>
//! `achieved` below `rate` means the generator or the path could not keep the
//! schedule; `max send lag` is the longest a message waited past its due time
//! to be offered. `received < sent` means replies never came back within two
//! seconds of the end (a stall: Aeron itself does not lose messages), and a
//! run is cut short at twice its schedule if the peer stalls or goes away. With
//! `RTT_HISTOGRAM=<path>` the first stream's full histogram is written there.
//!
//! With `LOAD_SPIKES=<path>`, every round trip of at least `LOAD_SPIKE_NS`
//! (default 100000) after warm-up is written there as
//! `stream,due_ns,latency_ns` (`due_ns` from the start of the schedule, whose
//! `CLOCK_REALTIME` the first line gives as `# start_realtime_ns=`), up to
//! 1,000,000 of them, so spikes can be lined up against periodic work.

#[path = "../lab.rs"]
mod lab;

use hdrhistogram::Histogram;
use lab::{Clock, aeron_dir, cpu_arg, label, pin, wait_until};
use rusteron_client::*;
use std::ffi::CString;
use std::hint::spin_loop;
use std::time::Duration;

// rtt's streams, so `rtt xpong` echoes for xping
const PING_STREAM_ID: i32 = 1002;
const PONG_STREAM_ID: i32 = 1003;
// xmix/xecho: the echo of stream s is on s + ECHO_OFFSET
const ECHO_OFFSET: i32 = 1000;
const HEADER: usize = 16; // intended send tick, sequence
const FRAGMENT_COUNT_LIMIT: usize = 64;
const SPIKE_CAPACITY: usize = 1_000_000;

fn udp(endpoint: Option<String>) -> CString {
    CString::new(format!("aeron:udp?endpoint={}", endpoint.expect("host:port"))).unwrap()
}

fn client() -> Aeron {
    let context = AeronContext::new().unwrap();
    context.set_dir(&aeron_dir()).unwrap();
    let aeron = Aeron::new(&context).unwrap();
    aeron.start().unwrap();
    aeron
}

/// One offered stream: what to send, and on which streams.
struct Spec {
    name: Option<String>,
    ping_stream: i32,
    pong_stream: i32,
    rate: u64,
    size: usize,
    burst: u64,
}

impl Spec {
    /// `name:stream:msgs/s:size[:burst]`
    fn parse(arg: &str) -> Self {
        let parts: Vec<&str> = arg.split(':').collect();
        assert!(
            (4..=5).contains(&parts.len()),
            "stream spec {arg}: name:stream:msgs/s:size[:burst]"
        );
        let stream: i32 = parts[1].parse().expect("stream id");
        Self {
            name: Some(parts[0].to_owned()),
            ping_stream: stream,
            pong_stream: stream + ECHO_OFFSET,
            rate: parts[2].parse().expect("msgs/s"),
            size: parts[3].parse().expect("size"),
            burst: parts.get(4).map_or(1, |b| b.parse::<u64>().expect("burst")).max(1),
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "load xping <ping> <pong> <rate> <seconds> <size> <cpu> [burst] | \
                 load xmix <ping> <pong> <seconds> <cpu> <name:stream:rate:size[:burst]>... | \
                 load xecho <ping> <pong> <cpu> <stream>...";
    let mode = args.next().unwrap_or_else(|| panic!("{usage}"));
    let (ping_channel, pong_channel) = (udp(args.next()), udp(args.next()));
    let number = |a: Option<String>, what: &str| -> u64 { a.unwrap_or_else(|| panic!("{what}")).parse().unwrap() };
    match mode.as_str() {
        "xping" => {
            let rate = number(args.next(), "msgs/s");
            let seconds = number(args.next(), "seconds");
            let size = usize::try_from(number(args.next(), "size")).unwrap();
            let cpu = cpu_arg(args.next());
            let burst = args.next().map_or(1, |b| b.parse::<u64>().unwrap()).max(1);
            let spec = Spec {
                name: None,
                ping_stream: PING_STREAM_ID,
                pong_stream: PONG_STREAM_ID,
                rate,
                size,
                burst,
            };
            run(&ping_channel, &pong_channel, vec![spec], seconds, cpu);
        }
        "xmix" => {
            let seconds = number(args.next(), "seconds");
            let cpu = cpu_arg(args.next());
            let specs: Vec<Spec> = args.map(|a| Spec::parse(&a)).collect();
            assert!(!specs.is_empty(), "{usage}");
            run(&ping_channel, &pong_channel, specs, seconds, cpu);
        }
        "xecho" => {
            let cpu = cpu_arg(args.next());
            let streams: Vec<i32> = args.map(|a| a.parse().expect("stream id")).collect();
            assert!(!streams.is_empty(), "{usage}");
            echo(&ping_channel, &pong_channel, &streams, cpu);
        }
        other => panic!("unknown mode {other}: {usage}"),
    }
}

/// A stream being offered: its schedule and what came back.
struct Offered {
    spec: Spec,
    publication: AeronExclusivePublication,
    image: AeronImage,
    message: Vec<u8>,
    interval_fp: u128,
    total: u64,
    warmup: u64,
    sent: u64,
    backpressured: u64,
    stalled: bool,
    max_lag: u64,
    histogram: Histogram<u64>,
    received: u64,
}

impl Offered {
    #[inline]
    fn due(&self, start: u64, n: u64) -> u64 {
        start + ((self.interval_fp * u128::from(n / self.spec.burst)) >> 32) as u64
    }
}

/// Where spikes go: preallocated, so recording never allocates.
struct Spikes {
    threshold_ns: u64,
    rows: Vec<(i32, u64, u64)>,
    dropped: u64,
}

fn run(ping_channel: &CString, pong_channel: &CString, specs: Vec<Spec>, seconds: u64, cpu: Option<usize>) {
    for spec in &specs {
        assert!(spec.rate > 0 && spec.size >= HEADER, "rate > 0, size >= {HEADER}");
    }
    let aeron = client();
    let mut pending = Vec::new();
    for spec in specs {
        let publication = aeron
            .async_add_exclusive_publication(ping_channel, spec.ping_stream)
            .unwrap()
            .poll_blocking(Duration::from_secs(5))
            .unwrap();
        let subscription = aeron
            .async_add_subscription(pong_channel, spec.pong_stream, Handlers::NONE, Handlers::NONE)
            .unwrap()
            .poll_blocking(Duration::from_secs(5))
            .unwrap();
        pending.push((spec, publication, subscription));
    }
    wait_until("pong", 30, || {
        pending
            .iter()
            .all(|(_, p, s)| p.is_connected() && s.image_at_index(0).is_some())
    });
    pin(cpu);
    let clock = Clock::calibrate();
    if !clock.is_tsc() {
        eprintln!("load: no invariant TSC, stamping with CLOCK_MONOTONIC");
    }
    // Ticks per burst interval, in 32.32 fixed point so long runs do not drift.
    let ticks_per_second = ticks_per_second(&clock);
    let mut streams: Vec<Offered> = pending
        .into_iter()
        .map(|(spec, publication, subscription)| {
            let image = subscription.image_at_index(0).unwrap();
            let max_payload = publication.max_payload_length().unwrap();
            assert!(
                spec.size <= max_payload,
                "size {} > max payload {max_payload}: it would fragment",
                spec.size
            );
            let interval_fp = (u128::from(ticks_per_second) << 32) * u128::from(spec.burst) / u128::from(spec.rate);
            Offered {
                message: vec![0u8; spec.size],
                interval_fp,
                total: spec.rate * (seconds + 1), // the first second is warm-up
                warmup: spec.rate,
                sent: 0,
                backpressured: 0,
                stalled: false,
                max_lag: 0,
                histogram: Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).unwrap(),
                received: 0,
                spec,
                publication,
                image,
            }
        })
        .collect();
    let mut spikes = std::env::var("LOAD_SPIKES").ok().filter(|p| !p.is_empty()).map(|path| {
        let threshold_ns = std::env::var("LOAD_SPIKE_NS").map_or(100_000, |v| v.parse().expect("LOAD_SPIKE_NS"));
        (
            path,
            Spikes {
                threshold_ns,
                rows: Vec::with_capacity(SPIKE_CAPACITY),
                dropped: 0,
            },
        )
    });

    let start = clock.ticks();
    let start_realtime = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    // A hard bound: the schedule plus as long again, so a stalled or lost
    // peer ends the run (reported as sent < total) instead of hanging it.
    let send_deadline = start + 2 * (seconds + 1) * ticks_per_second;
    let mut gone = false;
    'send: while streams.iter().any(|s| s.sent < s.total) {
        let now = clock.ticks();
        if now > send_deadline {
            break;
        }
        for s in &mut streams {
            // the messages now due, each stamped with when it was due; at
            // most a poll's worth per pass, so replies are read between them
            let mut budget = FRAGMENT_COUNT_LIMIT;
            while budget > 0 && s.sent < s.total && s.due(start, s.sent) <= now {
                let intended = s.due(start, s.sent);
                s.message[..8].copy_from_slice(&intended.to_le_bytes());
                s.message[8..16].copy_from_slice(&s.sent.to_le_bytes());
                match s.publication.offer(&s.message) {
                    Ok(_) => {
                        s.max_lag = s.max_lag.max(clock.ticks().saturating_sub(intended));
                        s.sent += 1;
                        budget -= 1;
                        s.stalled = false;
                    }
                    Err(e) if e.is_retryable() => {
                        if !s.publication.is_connected() || s.image.is_closed() {
                            eprintln!(
                                "load: stream {}'s pong went away after {} messages",
                                s.spec.ping_stream, s.sent
                            );
                            gone = true;
                            break 'send;
                        }
                        // messages that met back-pressure at least once
                        if !s.stalled {
                            s.backpressured += 1;
                            s.stalled = true;
                        }
                        break;
                    }
                    Err(e) => panic!("offer: {e:?}"),
                }
            }
        }
        if poll_all(&mut streams, &clock, start, spikes.as_mut().map(|(_, s)| s)) == 0 {
            spin_loop();
        }
    }
    let send_end = clock.ticks();
    // the rate actually offered after warm-up: below `rate`, the generator or
    // the path could not keep the schedule
    let achieved: Vec<f64> = streams
        .iter()
        .map(|s| {
            if s.sent > s.warmup {
                (s.sent - s.warmup) as f64 * 1e9
                    / clock.ns(send_end.saturating_sub(s.due(start, s.warmup)).max(1)) as f64
            } else {
                0.0
            }
        })
        .collect();
    // drain: replies still in flight, for up to two seconds
    let drain_until = clock.ticks() + 2 * ticks_per_second;
    while !gone && streams.iter().any(|s| s.received < s.sent) && clock.ticks() < drain_until {
        if poll_all(&mut streams, &clock, start, spikes.as_mut().map(|(_, s)| s)) == 0 {
            spin_loop();
        }
    }

    for (s, achieved) in streams.iter().zip(&achieved) {
        let q = |quantile| s.histogram.value_at_quantile(quantile);
        let (kind, name) = match &s.spec.name {
            Some(n) => ("mix", format!("udp-{n}")),
            None => ("load", "udp".to_owned()),
        };
        println!(
            "{kind},{},{name},{},{},{},{},{},{},{},{},{},{:.0},{},{},{},{},{achieved:.0},{}",
            label(),
            s.spec.size,
            s.spec.rate,
            s.spec.burst,
            q(0.5),
            q(0.9),
            q(0.99),
            q(0.999),
            q(0.9999),
            s.histogram.max(),
            s.histogram.mean(),
            s.histogram.len(),
            s.sent,
            s.received,
            s.backpressured,
            clock.ns(s.max_lag)
        );
    }
    if let Some(path) = std::env::var("RTT_HISTOGRAM").ok().filter(|p| !p.is_empty()) {
        let mut rows = String::from("value_ns,count\n");
        for v in streams[0].histogram.iter_recorded() {
            rows.push_str(&format!("{},{}\n", v.value_iterated_to(), v.count_at_value()));
        }
        std::fs::write(&path, rows).unwrap_or_else(|e| panic!("RTT_HISTOGRAM {path}: {e}"));
    }
    if let Some((path, spikes)) = spikes {
        // the wall-clock time due_ns counts from, to line spikes up with traces
        let mut rows = format!("# start_realtime_ns={start_realtime}\nstream,due_ns,latency_ns\n");
        for (stream, due, latency) in &spikes.rows {
            rows.push_str(&format!("{stream},{due},{latency}\n"));
        }
        std::fs::write(&path, rows).unwrap_or_else(|e| panic!("LOAD_SPIKES {path}: {e}"));
        if spikes.dropped > 0 {
            eprintln!(
                "load: {} spikes past the first {SPIKE_CAPACITY} not written",
                spikes.dropped
            );
        }
    }
}

/// Reads every stream's echoes once; the fragments read.
#[inline]
fn poll_all(streams: &mut [Offered], clock: &Clock, start: u64, mut spikes: Option<&mut Spikes>) -> i32 {
    let mut fragments = 0;
    for s in streams.iter_mut() {
        let (histogram, received, warmup, stream) = (&mut s.histogram, &mut s.received, s.warmup, s.spec.ping_stream);
        let mut spikes = spikes.as_deref_mut();
        fragments += s
            .image
            .poll_fn(
                |reply, _header| {
                    let intended = u64::from_le_bytes(reply[..8].try_into().unwrap());
                    let sequence = u64::from_le_bytes(reply[8..16].try_into().unwrap());
                    if sequence >= warmup {
                        let latency = clock.ns(clock.ticks().wrapping_sub(intended));
                        histogram.saturating_record(latency);
                        if let Some(spikes) = spikes.as_deref_mut()
                            && latency >= spikes.threshold_ns
                        {
                            if spikes.rows.len() < SPIKE_CAPACITY {
                                spikes
                                    .rows
                                    .push((stream, clock.ns(intended.wrapping_sub(start)), latency));
                            } else {
                                spikes.dropped += 1;
                            }
                        }
                    }
                    *received += 1;
                },
                FRAGMENT_COUNT_LIMIT,
            )
            .unwrap();
    }
    fragments
}

/// Echoes each stream back on stream + ECHO_OFFSET until every ping image goes.
fn echo(ping_channel: &CString, pong_channel: &CString, streams: &[i32], cpu: Option<usize>) {
    let aeron = client();
    let mut pairs = Vec::new();
    for &stream in streams {
        let publication = aeron
            .async_add_exclusive_publication(pong_channel, stream + ECHO_OFFSET)
            .unwrap()
            .poll_blocking(Duration::from_secs(5))
            .unwrap();
        let subscription = aeron
            .async_add_subscription(ping_channel, stream, Handlers::NONE, Handlers::NONE)
            .unwrap()
            .poll_blocking(Duration::from_secs(5))
            .unwrap();
        pairs.push((publication, subscription));
    }
    wait_until("ping", 60, || pairs.iter().all(|(_, s)| s.image_at_index(0).is_some()));
    let pairs: Vec<(AeronExclusivePublication, AeronImage)> = pairs
        .into_iter()
        .map(|(p, s)| {
            let image = s.image_at_index(0).unwrap();
            (p, image)
        })
        .collect();
    pin(cpu);
    while pairs.iter().any(|(_, image)| !image.is_closed()) {
        let mut fragments = 0;
        for (publication, image) in &pairs {
            fragments += image
                .poll_fn(|ping, _header| echo_one(publication, ping), FRAGMENT_COUNT_LIMIT)
                .unwrap();
        }
        if fragments == 0 {
            spin_loop();
        }
    }
}

#[inline]
fn echo_one(publication: &AeronExclusivePublication, ping: &[u8]) {
    loop {
        match publication.try_claim_owned(ping.len()) {
            Ok(mut claim) => {
                claim.data().copy_from_slice(ping);
                claim.commit().unwrap();
                return;
            }
            Err(e) if e.is_retryable() => spin_loop(),
            Err(e) => panic!("claim: {e:?}"),
        }
    }
}

/// Clock ticks in one second, measured over 50 ms.
fn ticks_per_second(clock: &Clock) -> u64 {
    let t0 = clock.ticks();
    std::thread::sleep(Duration::from_millis(50));
    let ticks = clock.ticks() - t0;
    u64::try_from(u128::from(ticks) * 1_000_000_000 / u128::from(clock.ns(ticks).max(1))).unwrap()
}
