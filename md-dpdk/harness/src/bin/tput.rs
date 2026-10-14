//! Cross-host throughput through an external media driver (`AERON_DIR`): the
//! kernel-UDP driver or the DPDK one, the client code identical. Ported from
//! rusteron's x86 lab (`scripts/x86-lab/harness/src/bin/tput.rs`), cross-host
//! modes only.
//!
//! The subscriber binds <endpoint> (host:port) on its host; the publisher sends
//! 32-byte messages to it flat out for <seconds>:
//! tput xsub <endpoint> <seconds> <cpu|->
//! tput xpub <endpoint> <seconds> <cpu|-> [size]   (default 32 bytes)
//! xsub prints: tput,<label>,udp,<median msgs/s>,<min>,<max> over one-second
//! samples after a one-second warmup

#[path = "../lab.rs"]
mod lab;

use lab::{Clock, aeron_dir, cpu_arg, label, pin, wait_until};
use rusteron_client::*;
use std::ffi::{CStr, CString};
use std::hint::spin_loop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const STREAM_ID: i32 = 1001;
const MESSAGE_LENGTH: usize = 32;
const FRAGMENT_COUNT_LIMIT: usize = 256;

fn client() -> Aeron {
    let context = AeronContext::new().unwrap();
    context.set_dir(&aeron_dir()).unwrap();
    let aeron = Aeron::new(&context).unwrap();
    aeron.start().unwrap();
    aeron
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = args.next().expect("xsub or xpub");
    let channel = CString::new(format!("aeron:udp?endpoint={}", args.next().expect("host:port"))).unwrap();
    let seconds: usize = args.next().expect("seconds").parse().unwrap();
    let cpu = cpu_arg(args.next());
    let size: usize = args.next().map_or(MESSAGE_LENGTH, |s| s.parse().unwrap());
    match mode.as_str() {
        "xpub" => {
            let running = AtomicBool::new(true);
            std::thread::scope(|scope| {
                scope.spawn(|| publish(&running, &channel, cpu, size));
                std::thread::sleep(Duration::from_secs(seconds as u64));
                running.store(false, Ordering::Release);
            });
        }
        "xsub" => {
            let aeron = client();
            let subscription = aeron
                .async_add_subscription(&channel, STREAM_ID, Handlers::NONE, Handlers::NONE)
                .unwrap()
                .poll_blocking(Duration::from_secs(5))
                .unwrap();
            wait_until("publisher", 60, || subscription.image_at_index(0).is_some());
            pin(cpu);
            let samples = measure(&subscription, seconds);
            println!(
                "tput,{},udp,{:.0},{:.0},{:.0}",
                label(),
                samples[samples.len() / 2],
                samples[0],
                samples[samples.len() - 1]
            );
        }
        other => panic!("unknown mode {other}: xsub or xpub"),
    }
}

/// Messages per second over `seconds` one-second samples after a one-second warmup, sorted.
fn measure(subscription: &AeronSubscription, seconds: usize) -> Vec<f64> {
    const SECOND_NS: u64 = 1_000_000_000;
    let clock = Clock::calibrate();
    if !clock.is_tsc() {
        eprintln!("tput: no invariant TSC, timing with CLOCK_MONOTONIC");
    }
    let mut count = 0u64;
    let mut samples = Vec::with_capacity(seconds);
    let mut window_start = clock.ticks();
    let warmup_start = window_start;
    let mut warming = true;
    while samples.len() < seconds {
        let fragments = subscription
            .poll_fn(|_message, _header| count += 1, FRAGMENT_COUNT_LIMIT)
            .unwrap();
        if fragments == 0 {
            spin_loop();
        }
        let now = clock.ticks();
        if warming {
            if clock.ns(now - warmup_start) >= SECOND_NS {
                warming = false;
                count = 0;
                window_start = now;
            }
        } else if clock.ns(now - window_start) >= SECOND_NS {
            samples.push(count as f64 * 1e9 / clock.ns(now - window_start) as f64);
            count = 0;
            window_start = now;
        }
    }
    samples.sort_by(f64::total_cmp);
    samples
}

fn publish(running: &AtomicBool, channel: &CStr, cpu: Option<usize>, size: usize) {
    let aeron = client();
    let publication = aeron
        .async_add_exclusive_publication(channel, STREAM_ID)
        .unwrap()
        .poll_blocking(Duration::from_secs(5))
        .unwrap();
    wait_until("subscriber", 60, || publication.is_connected());
    let max_payload = publication.max_payload_length().unwrap();
    assert!(
        size <= max_payload,
        "size {size} > max payload {max_payload}: it would fragment"
    );
    pin(cpu);
    let message = vec![0u8; size];
    while running.load(Ordering::Relaxed) {
        match publication.offer(&message) {
            Ok(_) => {}
            Err(e) if e.is_retryable() => spin_loop(),
            Err(e) => panic!("offer: {e:?}"),
        }
    }
}
