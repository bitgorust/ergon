//! Cross-host round trip through an external media driver (`AERON_DIR`): the
//! kernel-UDP driver or the DPDK one, the client code identical. Ported from
//! rusteron's x86 lab (`scripts/x86-lab/harness/src/bin/rtt.rs`), cross-host
//! modes only, so the results compare with rusteron's published UDP table.
//!
//! Pong runs on the peer and ping here (endpoints are host:port, ping's on the
//! pong host and pong's on the ping host):
//! rtt xpong <ping endpoint> <pong endpoint> <pong cpu|->
//! rtt xping <ping endpoint> <pong endpoint> <messages> <warmup> <ping cpu|->
//! xping prints: rtt,<label>,udp,p50,p90,p99,p99.9,p99.99,max,mean (ns),samples
//!
//! Endpoints on 127.0.0.1 with both on one host measure a localhost round trip.
//! `rtt idle <endpoint> <seconds>` holds a subscription that receives nothing,
//! to measure what an idle host-local channel costs the driver's agents.
//!
//! A slow configuration stops early: warmup after a quarter of `RTT_SECONDS`
//! (default 20) and the measured run after all of it.
//!
//! Round trips are stamped with the calibrated TSC (`lab::Clock`). With
//! `RTT_HISTOGRAM=<path>` xping also writes the full histogram there, one
//! `value_ns,count` row per recorded value. With `LOAD_SPIKES=<path>` it writes
//! every measured round trip of at least `LOAD_SPIKE_NS` (default 100000) as
//! `stream,due_ns,latency_ns` (`due_ns`: when it was sent, from the start of the
//! measured run, whose `CLOCK_REALTIME` the first line gives as
//! `# start_realtime_ns=`), the first 100,000 of them with `# spikes_total=`,
//! as `load` does.
//! With `LOOP_STALLS_DIR=<dir>`, xping and xpong also write there every stall
//! of their own polling loop of at least `LOOP_STALL_NS` (default 20000), as
//! `stalls-ping.csv` / `stalls-pong.csv` (`end_ns,gap_ns` from the line
//! `# start_realtime_ns=`), so a spike can be told from a client that did not
//! run. xpong stops on SIGTERM, writing its stalls.

#[path = "../lab.rs"]
mod lab;

use hdrhistogram::Histogram;
use lab::{Clock, aeron_dir, cpu_arg, label, pin, retry_closing, sub_channel, wait_until};
use rusteron_client::*;
use std::ffi::{CStr, CString};
use std::hint::spin_loop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const PING_STREAM_ID: i32 = 1002;
const PONG_STREAM_ID: i32 = 1003;
const MESSAGE_LENGTH: usize = 32;
const FRAGMENT_COUNT_LIMIT: usize = 10;
const SPIKE_CAPACITY: usize = 100_000;
const STALL_CAPACITY: usize = 1_000_000;

/// Gaps in the calling loop of at least a threshold, each with when it ended:
/// time this client did not run. Preallocated, so recording never allocates.
struct Stalls {
    threshold: u64, // ticks
    last: u64,
    t0: u64,
    t0_realtime: u128,
    rows: Vec<(u64, u64)>, // (end, gap) in ticks
}

impl Stalls {
    /// `None` unless `LOOP_STALLS_DIR` is set.
    fn new(clock: &Clock) -> Option<Self> {
        std::env::var("LOOP_STALLS_DIR").ok().filter(|d| !d.is_empty())?;
        let ns: u64 = std::env::var("LOOP_STALL_NS").map_or(20_000, |v| v.parse().expect("LOOP_STALL_NS"));
        // ticks for `ns`, from how many ns a billion ticks are
        let threshold =
            u64::try_from(u128::from(ns) * 1_000_000_000 / u128::from(clock.ns(1_000_000_000).max(1))).unwrap();
        let t0 = clock.ticks();
        Some(Self {
            threshold,
            last: t0,
            t0,
            t0_realtime: realtime_ns(),
            rows: Vec::with_capacity(STALL_CAPACITY),
        })
    }

    /// One pass of the loop, at `now`.
    #[inline(always)]
    fn pass(&mut self, now: u64) {
        let gap = now.wrapping_sub(self.last);
        if gap >= self.threshold && self.rows.len() < self.rows.capacity() {
            self.rows.push((now.wrapping_sub(self.t0), gap));
        }
        self.last = now;
    }

    fn write(&self, clock: &Clock, name: &str) {
        let Some(dir) = std::env::var("LOOP_STALLS_DIR").ok() else {
            return;
        };
        let mut rows = format!("# start_realtime_ns={}\nend_ns,gap_ns\n", self.t0_realtime);
        for (end, gap) in &self.rows {
            rows.push_str(&format!("{},{}\n", clock.ns(*end), clock.ns(*gap)));
        }
        let path = format!("{dir}/{name}");
        std::fs::write(&path, rows).unwrap_or_else(|e| panic!("LOOP_STALLS_DIR {path}: {e}"));
    }
}

/// Set by SIGTERM, so pong stops its loop and writes what it recorded.
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigterm(_signal: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

fn client() -> Aeron {
    let context = AeronContext::new().unwrap();
    context.set_dir(&aeron_dir()).unwrap();
    let aeron = Aeron::new(&context).unwrap();
    aeron.start().unwrap();
    aeron
}

fn udp(endpoint: Option<String>) -> CString {
    CString::new(format!("aeron:udp?endpoint={}", endpoint.expect("host:port"))).unwrap()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().expect("xping|xpong|idle");
    if mode == "idle" {
        run_idle(&udp(args.next()), args.next().expect("seconds").parse().unwrap());
        return;
    }
    let (ping_channel, pong_channel) = (udp(args.next()), udp(args.next()));
    match mode.as_str() {
        "xpong" => run_pong(&ping_channel, &pong_channel, cpu_arg(args.next())),
        "xping" => {
            let number = |a: Option<String>| -> usize { a.expect("messages and warmup").parse().unwrap() };
            let (messages, warmup) = (number(args.next()), number(args.next()));
            run_ping(&ping_channel, &pong_channel, messages, warmup, cpu_arg(args.next()));
        }
        other => panic!("unknown mode {other}: xping, xpong or idle"),
    }
}

fn run_ping(ping_channel: &CStr, pong_channel: &CStr, messages: usize, warmup: usize, cpu: Option<usize>) {
    let aeron = client();
    let publication = retry_closing("publication", || {
        aeron
            .async_add_exclusive_publication(ping_channel, PING_STREAM_ID)
            .and_then(|a| a.poll_blocking(Duration::from_secs(5)))
    });
    let subscription = retry_closing("subscription", || {
        aeron
            .async_add_subscription(
                &sub_channel(pong_channel),
                PONG_STREAM_ID,
                Handlers::NONE,
                Handlers::NONE,
            )
            .and_then(|a| a.poll_blocking(Duration::from_secs(5)))
    });
    wait_until("pong", 30, || {
        publication.is_connected() && subscription.image_at_index(0).is_some()
    });
    let image = subscription.image_at_index(0).unwrap();
    pin(cpu);
    let clock = Clock::calibrate();
    if !clock.is_tsc() {
        eprintln!("rtt: no invariant TSC, stamping with CLOCK_MONOTONIC");
    }

    let budget = Duration::from_secs(std::env::var("RTT_SECONDS").map_or(20, |s| s.parse().unwrap()));
    let mut histogram = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).unwrap();
    let mut buffer = [0u8; MESSAGE_LENGTH];
    // RTT_LOST_TIMEOUT_US: on an unreliable channel a lost ping or echo is
    // never repaired, so after this long it is counted lost and the next sent
    let timeout_ns: u64 =
        std::env::var("RTT_LOST_TIMEOUT_US").map_or(0, |v| v.parse::<u64>().expect("RTT_LOST_TIMEOUT_US") * 1000);
    let mut seq = 0_u64;
    let start = Instant::now();
    for i in 0..warmup {
        seq += 1;
        round_trip(
            &clock,
            &publication,
            &image,
            &mut buffer,
            &mut None,
            seq,
            timeout_ns,
            |_| {},
        );
        if i % 64 == 0 && start.elapsed() > budget / 4 {
            break;
        }
    }
    // spikes, preallocated so recording never allocates
    let spikes_path = std::env::var("LOAD_SPIKES").ok().filter(|p| !p.is_empty());
    let spike_ns: u64 = std::env::var("LOAD_SPIKE_NS").map_or(100_000, |v| v.parse().expect("LOAD_SPIKE_NS"));
    let mut spikes: Vec<(u64, u64)> = Vec::with_capacity(if spikes_path.is_some() { SPIKE_CAPACITY } else { 0 });
    let mut spikes_total = 0_u64;
    let start = Instant::now();
    let t0 = clock.ticks();
    let t0_realtime = realtime_ns();
    let mut stalls = Stalls::new(&clock);
    let mut lost = 0_u64;
    for i in 0..messages {
        seq += 1;
        let echoed = round_trip(
            &clock,
            &publication,
            &image,
            &mut buffer,
            &mut stalls,
            seq,
            timeout_ns,
            |rtt| {
                histogram.saturating_record(rtt);
                if rtt >= spike_ns {
                    spikes_total += 1;
                    if spikes.len() < spikes.capacity() {
                        spikes.push((clock.ns(clock.ticks().wrapping_sub(t0)).saturating_sub(rtt), rtt));
                    }
                }
            },
        );
        if !echoed {
            lost += 1;
        }
        if i % 64 == 0 && start.elapsed() > budget {
            break;
        }
    }

    let q = |quantile| histogram.value_at_quantile(quantile);
    println!(
        "rtt,{},udp,{},{},{},{},{},{},{:.0},{},{lost}",
        label(),
        q(0.5),
        q(0.9),
        q(0.99),
        q(0.999),
        q(0.9999),
        histogram.max(),
        histogram.mean(),
        histogram.len()
    );

    if let Some(path) = std::env::var("RTT_HISTOGRAM").ok().filter(|p| !p.is_empty()) {
        let mut rows = String::from("value_ns,count\n");
        for v in histogram.iter_recorded() {
            rows.push_str(&format!("{},{}\n", v.value_iterated_to(), v.count_at_value()));
        }
        std::fs::write(&path, rows).unwrap_or_else(|e| panic!("RTT_HISTOGRAM {path}: {e}"));
    }
    if let Some(path) = spikes_path {
        // the wall-clock time due_ns counts from, to line spikes up with traces
        let mut rows =
            format!("# start_realtime_ns={t0_realtime}\n# spikes_total={spikes_total}\nstream,due_ns,latency_ns\n");
        for (due, latency) in &spikes {
            rows.push_str(&format!("{PING_STREAM_ID},{due},{latency}\n"));
        }
        std::fs::write(&path, rows).unwrap_or_else(|e| panic!("LOAD_SPIKES {path}: {e}"));
    }
    if let Some(stalls) = &stalls {
        stalls.write(&clock, "stalls-ping.csv");
    }
}

/// `CLOCK_REALTIME` now, in nanoseconds since the epoch.
fn realtime_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

/// Sends one stamped ping and polls until pong's echo of it is back.
#[inline]
/// One round trip of message `seq`: `true` once its echo is back, `false` if
/// none came within `timeout_ns` (0: wait for ever, as a reliable channel
/// repairs any loss). An echo of an earlier, timed-out message is skipped.
#[allow(clippy::too_many_arguments)]
fn round_trip(
    clock: &Clock,
    publication: &AeronExclusivePublication,
    image: &AeronImage,
    buffer: &mut [u8; MESSAGE_LENGTH],
    stalls: &mut Option<Stalls>,
    seq: u64,
    timeout_ns: u64,
    mut record: impl FnMut(u64),
) -> bool {
    if let Some(s) = stalls.as_mut() {
        s.pass(clock.ticks());
    }
    buffer[8..16].copy_from_slice(&seq.to_le_bytes());
    let started = loop {
        // stamped on each attempt, so time back-pressured is not counted
        let now = clock.ticks();
        buffer[..8].copy_from_slice(&now.to_le_bytes());
        match publication.offer(buffer) {
            Ok(_) => break now,
            Err(e) if e.is_retryable() => spin_loop(),
            Err(e) => panic!("offer: {e:?}"),
        }
    };
    let mut echoed = false;
    while !echoed {
        if let Some(s) = stalls.as_mut() {
            s.pass(clock.ticks());
        }
        let fragments = image
            .poll_fn(
                |reply, _header| {
                    if u64::from_le_bytes(reply[8..16].try_into().unwrap()) == seq {
                        let sent = u64::from_le_bytes(reply[..8].try_into().unwrap());
                        record(clock.ns(clock.ticks().wrapping_sub(sent)));
                        echoed = true;
                    }
                },
                FRAGMENT_COUNT_LIMIT,
            )
            .unwrap();
        if fragments == 0 {
            assert!(!image.is_closed(), "pong went away");
            if timeout_ns != 0 && clock.ns(clock.ticks().wrapping_sub(started)) > timeout_ns {
                return false;
            }
            spin_loop();
        }
    }
    true
}

/// Echoes every ping until the ping side's image goes away.
fn run_pong(ping_channel: &CStr, pong_channel: &CStr, cpu: Option<usize>) {
    let aeron = client();
    let publication = retry_closing("publication", || {
        aeron
            .async_add_exclusive_publication(pong_channel, PONG_STREAM_ID)
            .and_then(|a| a.poll_blocking(Duration::from_secs(5)))
    });
    let subscription = retry_closing("subscription", || {
        aeron
            .async_add_subscription(
                &sub_channel(ping_channel),
                PING_STREAM_ID,
                Handlers::NONE,
                Handlers::NONE,
            )
            .and_then(|a| a.poll_blocking(Duration::from_secs(5)))
    });
    wait_until("ping", 60, || subscription.image_at_index(0).is_some());
    let image = subscription.image_at_index(0).unwrap();
    pin(cpu);
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe { libc::signal(libc::SIGTERM, on_sigterm as *const () as libc::sighandler_t) };
    let clock = Clock::calibrate();
    let mut stalls = Stalls::new(&clock);
    while !image.is_closed() && !STOP.load(Ordering::Relaxed) {
        if let Some(s) = stalls.as_mut() {
            s.pass(clock.ticks());
        }
        let fragments = image
            .poll_fn(|ping, _header| echo(&publication, ping), FRAGMENT_COUNT_LIMIT)
            .unwrap();
        if fragments == 0 {
            spin_loop();
        }
    }
    if let Some(stalls) = &stalls {
        stalls.write(&clock, "stalls-pong.csv");
    }
}

#[inline]
fn echo(publication: &AeronExclusivePublication, ping: &[u8]) {
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

/// Holds a subscription on `channel` that nobody publishes to, for `seconds`.
fn run_idle(channel: &CStr, seconds: u64) {
    let aeron = client();
    let _subscription = retry_closing("subscription", || {
        aeron
            .async_add_subscription(channel, PONG_STREAM_ID + 100, Handlers::NONE, Handlers::NONE)
            .and_then(|a| a.poll_blocking(Duration::from_secs(5)))
    });
    eprintln!("rtt: idle subscription on {}", channel.to_string_lossy());
    std::thread::sleep(Duration::from_secs(seconds));
}
