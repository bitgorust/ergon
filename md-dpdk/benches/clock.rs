//! What the transport's clock costs: each clock read on its own (the TSC
//! clock, its `CLOCK_MONOTONIC` fallback, the millisecond clock it replaced,
//! and for reference `minstant` and `std::time::Instant`), then one 32-byte
//! datagram through the genuine send and poll callbacks against the fake
//! ports, once with each transport clock. The fakes stand in for the NIC, so
//! the path numbers are the transport's own cost, not a wire latency.
//!
//! `cargo bench -p ergon-md-dpdk --features dpdk --bench clock`
//! (Linux x86_64; pin it, e.g. `taskset -c 2`). Elsewhere it does nothing.
#![allow(
    missing_docs,
    unsafe_code,
    unsafe_op_in_unsafe_fn,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction
)]

#[cfg(all(feature = "dpdk", target_os = "linux", target_arch = "x86_64"))]
#[path = "../tests/common/mod.rs"]
mod common;

#[cfg(all(feature = "dpdk", target_os = "linux", target_arch = "x86_64"))]
mod bench {
    use super::common::transport::*;
    use super::common::*;
    use criterion::Criterion;
    use rusteron_media_driver::bindings::iovec;
    use std::hint::black_box;
    use std::os::raw::c_void;
    use std::time::Instant;

    /// (label, allow_tsc) for each transport clock this host can run.
    fn clocks() -> Vec<(&'static str, i32)> {
        let mut clocks = vec![("monotonic_ns", 0)];
        if cpu_has_invariant_tsc() {
            clocks.push(("tsc_ns", 1));
        } else {
            eprintln!("clock bench: no invariant TSC, the TSC clock is not measured");
        }
        clocks
    }

    fn use_clock(allow_tsc: i32, expect: &str) {
        unsafe { rusteron_dpdk_test_clock_recalibrate(allow_tsc) };
        let source = unsafe { rusteron_dpdk_clock_source() };
        assert_eq!(source, allow_tsc, "{expect}: transport clock source {source}");
    }

    pub fn reads(c: &mut Criterion) {
        let _env = TestEnv::new();
        let mut g = c.benchmark_group("clock_read");
        g.bench_function("legacy_monotonic_ms", |b| {
            b.iter(|| black_box(unsafe { rusteron_dpdk_fake_legacy_clock_ms() }))
        });
        for (name, allow) in clocks() {
            use_clock(allow, name);
            g.bench_function(name, |b| b.iter(|| black_box(unsafe { rusteron_dpdk_clock_ns() })));
        }
        g.bench_function("std_instant", |b| b.iter(|| black_box(Instant::now())));
        g.bench_function("minstant", |b| b.iter(|| black_box(minstant::Instant::now())));
        g.finish();
        unsafe { rusteron_dpdk_test_clock_recalibrate(1) };
    }

    pub fn send(c: &mut Criterion) {
        let mut g = c.benchmark_group("send_32b");
        for (name, allow) in clocks() {
            let mut h = Harness::new();
            use_clock(allow, name);
            h.init_tx(SENDER_IP, 40000, None, 1472);
            h.arp_seed(PEER_IP, PEER_MAC);
            let payload = [7u8; 32];
            let mut dst = SockAddrIn::new(PEER_IP, 40123);
            let mut iov = [iovec {
                iov_base: payload.as_ptr() as *mut c_void,
                iov_len: payload.len(),
            }];
            let mut bytes = 0i64;
            assert_eq!(h.send_iov(&mut dst, &mut iov, &mut bytes), 1, "send: {}", last_error());
            g.bench_function(name, |b| {
                b.iter(|| black_box(h.send_iov(&mut dst, &mut iov, &mut bytes)))
            });
            assert_no_leak();
        }
        g.finish();
        unsafe { rusteron_dpdk_test_clock_recalibrate(1) };
    }

    pub fn poll(c: &mut Criterion) {
        let mut g = c.benchmark_group("poll_32b");
        for (name, allow) in clocks() {
            let mut h = Harness::new();
            use_clock(allow, name);
            h.init_rx(RECEIVER_IP, 60000, 1472);
            let mut p = h.poller();
            p.add(&mut *h.rx);
            let frame = build_udp_frame(RECEIVER_MAC, PEER_MAC, PEER_IP, RECEIVER_IP, 7000, 60000, &[7u8; 32]);
            let mut msgvec = vec![unsafe { std::mem::zeroed::<RealMmsghdr>() }; 16];
            let mut count = 0u64;
            let clientd = &mut count as *mut u64 as *mut c_void;
            let mut bytes = 0i64;
            let mut one = || {
                unsafe { rusteron_dpdk_fake_rx_inject(RECEIVER_PORT, frame.as_ptr(), frame.len(), RX_GOOD, 1) };
                p.poll_into(&mut msgvec, Some(count_cb), clientd, &mut bytes)
            };
            assert_eq!(one(), 1, "poll: {}", last_error());
            g.bench_function(name, |b| b.iter(|| black_box(one())));
            drop(p);
            assert_no_leak();
        }
        g.finish();
        unsafe { rusteron_dpdk_test_clock_recalibrate(1) };
    }
}

#[cfg(all(feature = "dpdk", target_os = "linux", target_arch = "x86_64"))]
criterion::criterion_group!(benches, bench::reads, bench::send, bench::poll);
#[cfg(all(feature = "dpdk", target_os = "linux", target_arch = "x86_64"))]
criterion::criterion_main!(benches);

#[cfg(not(all(feature = "dpdk", target_os = "linux", target_arch = "x86_64")))]
fn main() {}
