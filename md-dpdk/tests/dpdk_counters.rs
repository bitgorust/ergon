//! Aeron DPDK counters and ENA statistics (plan §9).
//!
//! Registers the per-role-port counter sets into a real
//! `aeron_counters_manager_t` (aligned metadata + values buffers, real clock
//! cache) reached through a fake driver context, then drives the genuine
//! binding callbacks (send on the sender ENA, poll on the receiver ENA) and
//! asserts the counters read back from the manager buffers:
//!   * every fixed counter exists with its plan §9 type ID and a label carrying
//!     role / BDF / DPDK port / queue / direction (port-info also driver + MAC),
//!   * TX packet/byte totals are monotonic and attribute exactly the accepted
//!     prefix (plan §7.4: rejected datagrams are errors, not TX bytes),
//!   * every approved TX error path bumps its counter (nobufs, tx-eagain, ARP
//!     miss, validation error incl. oversized), and every RX reject class bumps
//!     its own counter (checksum, fragmented, unsupported ethertype/protocol,
//!     discard, queue-drop),
//!   * ENA SRD/Express extended statistics appear as type-92 counters with the
//!     PMD's names, are re-read at most once per second (no metrics thread),
//!     and a *missed* xstat mirrors the plan §9 type-84 counter.
//!
//! Linux x86_64 only; links the same archives as the other DPDK integration
//! tests plus the counters manager/clock compiled into the core archive.
#![allow(
    missing_docs,
    unsafe_code,
    unsafe_op_in_unsafe_fn,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction
)]
#![cfg(all(feature = "dpdk", target_os = "linux", target_arch = "x86_64"))]

mod common;
use common::transport::*;
use common::*;

use serial_test::serial;

use std::os::raw::{c_char, c_int, c_void};

// The Aeron types and functions of the driver this crate installs into.
use rusteron_media_driver::bindings::*;

// ---------------------------------------------------------------------------
// Registration and labels
// ---------------------------------------------------------------------------

/// Both role ports register their counter sets with the plan §9 type IDs and
/// labels carrying role / BDF / DPDK port / queue / direction; port-info also
/// carries the PMD driver name and MAC; transport counts the open transports.
#[test]
#[serial]
fn counters_registered_with_type_ids_and_labels() {
    let mut h = Harness::new();
    h.init_tx(SENDER_IP, 40000, None, 1472);
    h.init_rx(RECEIVER_IP, 60000, 1472);

    // Sender port-info: type 75, value 1, driver + MAC in the label.
    let (v, t, label) = h
        .counters
        .find(TYPE_PORT_INFO, "role=sender")
        .expect("sender port-info");
    assert_eq!(t, TYPE_PORT_INFO);
    assert_eq!(v, 1);
    assert!(label.contains("rusteron-dpdk role=sender bdf=0000:00:01.0 port=0 q=0 dir=- port-info"));
    assert!(
        label.contains(" driver=net_ena mac=02:00:00:00:00:01"),
        "label: {label}"
    );

    // Receiver port-info: distinct BDF/port/MAC.
    let (v, _, label) = h
        .counters
        .find(TYPE_PORT_INFO, "role=receiver")
        .expect("receiver port-info");
    assert_eq!(v, 1);
    assert!(label.contains("role=receiver bdf=0000:00:02.0 port=1"));
    assert!(label.contains("mac=02:00:00:00:00:02"), "label: {label}");

    // Each port carries the full counter set for both directions, so the
    // needle must pin role AND direction to disambiguate the match.
    h.counters
        .find(TYPE_PKTS, "role=sender bdf=0000:00:01.0 port=0 q=0 dir=tx pkts")
        .expect("sender tx pkts");
    h.counters
        .find(TYPE_BYTES, "role=sender bdf=0000:00:01.0 port=0 q=0 dir=tx bytes")
        .expect("sender tx bytes");

    h.counters
        .find(TYPE_PKTS, "role=receiver bdf=0000:00:02.0 port=1 q=0 dir=rx pkts")
        .expect("receiver rx pkts");
    h.counters
        .find(TYPE_BYTES, "role=receiver bdf=0000:00:02.0 port=1 q=0 dir=rx bytes")
        .expect("receiver rx bytes");

    // Transport counters: one open transport per role port.
    assert_eq!(h.counter(TYPE_TRANSPORT, "role=sender"), 1);
    assert_eq!(h.counter(TYPE_TRANSPORT, "role=receiver"), 1);

    // Mempool availability is populated at registration (fake pool holds 64).
    assert_eq!(h.counter(TYPE_MEMPOOL_AVAILABLE, "role=sender"), 64);
    assert_eq!(h.counter(TYPE_MEMPOOL_AVAILABLE, "role=receiver"), 64);

    assert_no_leak();
}

/// A port's counters outlive its transports: when the last one closes (on
/// the conductor, as the driver closes endpoints) they stay allocated and
/// dormant, still reading their totals, so a reading taken between channels
/// sees them (a reader that found them gone would count a drop to zero); the
/// next transport on the port reuses them, with no duplicates.
#[test]
#[serial]
fn counters_outlive_the_last_transport_and_are_reused() {
    let mut h = Harness::new();
    h.init_rx(RECEIVER_IP, 60000, 1472);
    let mut second = h.init_extra_rx(60001);
    assert_eq!(h.counter(TYPE_TRANSPORT, "role=receiver"), 2);

    close_transport(h.bindings, &mut second);
    assert_eq!(h.counter(TYPE_TRANSPORT, "role=receiver"), 1, "still one open");

    unsafe { (*h.bindings).close_func.unwrap()(&mut *h.rx) };
    h.rx_inited = false;
    assert_eq!(
        h.counters.find_all(TYPE_TRANSPORT, "role=receiver"),
        vec![0],
        "kept, dormant, with the last transport"
    );
    assert_eq!(h.counters.find_all(TYPE_PORT_INFO, "role=receiver").len(), 1);

    h.init_rx(RECEIVER_IP, 60000, 1472);
    assert_eq!(
        h.counters.find_all(TYPE_TRANSPORT, "role=receiver"),
        vec![1],
        "reused, no duplicate"
    );
    assert_eq!(h.counters.find_all(TYPE_PORT_INFO, "role=receiver").len(), 1);
    assert_no_leak();
}

/// The driver's counters manager belongs to its conductor: a port's last
/// transport closed on another thread (the receiver removing a destination)
/// leaves its counters allocated but dormant, and the next transport on the
/// port reuses them: no duplicates, no leak. Reuse needs the manager to
/// still hold them under their registration id; a set the manager no longer
/// holds (as after a driver restart, even at the same address) is replaced.
#[test]
#[serial]
fn counters_closed_off_the_conductor_are_dormant_then_reused_or_replaced() {
    struct SendPtr(
        *mut aeron_udp_channel_transport_stct,
        *mut aeron_udp_channel_transport_bindings_stct,
    );
    unsafe impl Send for SendPtr {}
    let close_elsewhere = |h: &mut Harness| {
        let ptrs = SendPtr(&mut *h.rx, h.bindings);
        std::thread::spawn(move || {
            let ptrs = ptrs;
            unsafe { (*ptrs.1).close_func.unwrap()(ptrs.0) };
        })
        .join()
        .unwrap();
        h.rx_inited = false;
    };

    let mut h = Harness::new();
    h.init_rx(RECEIVER_IP, 60000, 1472);
    close_elsewhere(&mut h);
    assert_eq!(
        h.counters.find_all(TYPE_TRANSPORT, "role=receiver"),
        vec![0],
        "dormant, not freed"
    );

    h.init_rx(RECEIVER_IP, 60000, 1472);
    assert_eq!(
        h.counters.find_all(TYPE_TRANSPORT, "role=receiver"),
        vec![1],
        "reused, no duplicate"
    );

    // The manager lets them go (a new driver's manager would not hold them).
    close_elsewhere(&mut h);
    for id in h.counters.ids_labelled("role=receiver") {
        unsafe { aeron_counters_manager_free(&mut *h.counters.manager, id) };
    }
    assert!(h.counters.find_all(TYPE_TRANSPORT, "role=receiver").is_empty());
    h.init_rx(RECEIVER_IP, 60000, 1472);
    assert_eq!(
        h.counters.find_all(TYPE_TRANSPORT, "role=receiver"),
        vec![1],
        "registered afresh"
    );
    assert_no_leak();
}

/// Counts are running totals for the runtime's life: a channel that comes
/// and goes never resets them, nor do they vanish in between.
#[test]
#[serial]
fn counts_carry_over_when_a_port_reregisters() {
    let mut h = Harness::new();
    h.init_tx(SENDER_IP, 40000, None, 1472);
    h.arp_seed(PEER_IP, PEER_MAC);
    for _ in 0..3 {
        assert_eq!(h.send_to((PEER_IP, 40123), &[b"x"]).0, 1, "{}", last_error());
    }
    const TX: &str = "role=sender bdf=0000:00:01.0 port=0 q=0 dir=tx pkts";
    assert_eq!(h.counter(TYPE_PKTS, TX), 3);

    unsafe { (*h.bindings).close_func.unwrap()(&mut *h.tx) };
    h.tx_inited = false;
    assert_eq!(h.counter(TYPE_PKTS, TX), 3, "still readable between channels");

    h.init_tx(SENDER_IP, 40000, None, 1472);
    assert_eq!(h.counter(TYPE_PKTS, TX), 3, "the total carried over");
    assert_eq!(h.send_to((PEER_IP, 40123), &[b"x"]).0, 1);
    assert_eq!(h.counter(TYPE_PKTS, TX), 4);
    assert_eq!(h.counter(TYPE_TRANSPORT, "role=sender"), 1, "gauges are not carried");
    assert_no_leak();
}

/// Each port publishes which clock the transport reads (type 80): 1 for the
/// calibrated invariant TSC, 0 for the `CLOCK_MONOTONIC` fallback.
#[test]
#[serial]
fn clock_counter_reports_the_clock_in_use() {
    let mut h = Harness::new();
    h.init_tx(SENDER_IP, 40000, None, 1472);
    h.init_rx(RECEIVER_IP, 60000, 1472);

    let expected = i64::from(cpu_has_invariant_tsc());
    assert_eq!(i64::from(unsafe { rusteron_dpdk_clock_source() }), expected);
    let (v, _, label) = h
        .counters
        .find(TYPE_CLOCK, "role=sender")
        .expect("sender clock counter");
    assert_eq!(v, expected);
    assert!(label.contains("dir=- clock"), "label: {label}");
    assert_eq!(h.counter(TYPE_CLOCK, "role=receiver"), expected);

    assert_no_leak();
}

// ---------------------------------------------------------------------------
// TX counters: monotonic and correctly attributed
// ---------------------------------------------------------------------------

/// TX packet/byte totals are monotonic and attribute exactly the accepted
/// prefix — rejected datagrams (oversized) are errors, never TX bytes.
#[test]
#[serial]
fn tx_pkts_and_bytes_monotonic_and_attributed() {
    let mut h = Harness::new();
    h.init_tx(SENDER_IP, 40000, None, 1472);
    h.arp_seed(PEER_IP, PEER_MAC);

    assert_eq!(h.counter(TYPE_PKTS, "dir=tx"), 0);
    assert_eq!(h.counter(TYPE_BYTES, "dir=tx"), 0);

    let (rc, sent) = h.send_to((PEER_IP, 40123), &[b"hello", b"world!"]);
    assert_eq!((rc, sent), (2, 11));
    assert_eq!(h.counter(TYPE_PKTS, "dir=tx"), 2, "two datagrams");
    assert_eq!(h.counter(TYPE_BYTES, "dir=tx"), 11, "5 + 6 payload bytes");

    let (rc, sent) = h.send_to((PEER_IP, 40123), &[b"xyz"]);
    assert_eq!((rc, sent), (1, 3));
    assert_eq!(h.counter(TYPE_PKTS, "dir=tx"), 3, "monotonic");
    assert_eq!(h.counter(TYPE_BYTES, "dir=tx"), 14, "monotonic");
    assert_no_leak();
}

/// Oversized datagrams: the accepted prefix is still counted (pkts/bytes) and
/// the oversized datagram bumps the error counter.
#[test]
#[serial]
fn oversized_datagram_bumps_error_not_tx_bytes() {
    let mut h = Harness::new();
    h.init_tx(SENDER_IP, 40000, None, 32);
    h.arp_seed(PEER_IP, PEER_MAC);

    // A 32-byte channel MTU: the first iovec fits, the second (33 bytes) is
    // oversized -> prefix flushed, then a permanent validation error.
    let (rc, sent) = h.send_to((PEER_IP, 40123), &[&[1u8; 20], &[2u8; 33]]);
    assert_eq!(rc, -1, "oversized datagram is rejected");
    assert_eq!(sent, 20, "the accepted prefix is reported");
    assert!(last_error().contains("oversized"), "got: {}", last_error());
    assert_eq!(h.counter(TYPE_PKTS, "dir=tx"), 1, "only the prefix datagram");
    assert_eq!(h.counter(TYPE_BYTES, "dir=tx"), 20, "only prefix bytes");
    assert_eq!(
        h.counter(TYPE_ERROR, "role=sender"),
        1,
        "oversized bumps the error counter"
    );
    assert_no_leak();
}

/// Every TX error path bumps its own counter: no destination, non-IPv4,
/// multicast, unresolved ARP, mbuf exhaustion, and NIC backpressure.
#[test]
#[serial]
fn tx_error_paths_counted() {
    let mut h = Harness::new();
    h.init_tx(SENDER_IP, 40000, None, 1472);
    h.arp_seed(PEER_IP, PEER_MAC);

    // No destination address (unconnected transport, NULL addr).
    let (rc, sent) = h.send_null_addr(&[b"x"]);
    assert_eq!(rc, -1);
    assert_eq!(sent, 0);
    assert_eq!(h.counter(TYPE_ERROR, "role=sender"), 1, "no-destination error");

    // Non-IPv4 family: a sockaddr_in6 as the destination.
    let (rc, sent) = {
        let mut v6 = [0u8; 128];
        v6[0] = 10; // AF_INET6
        let iovs = vec![iovec {
            iov_base: b"x".as_ptr() as *mut c_void,
            iov_len: 1,
        }];
        let mut bytes_sent: i64 = 0;
        let rc = unsafe {
            let b = &*h.bindings;
            b.send_func.unwrap()(
                &mut *h.data_paths,
                &mut *h.tx,
                v6.as_mut_ptr() as *mut sockaddr_storage,
                iovs.as_ptr() as *mut iovec,
                iovs.len(),
                &mut bytes_sent,
            )
        };
        (rc, bytes_sent)
    };
    assert_eq!(rc, -1);
    assert_eq!(sent, 0);
    assert_eq!(h.counter(TYPE_ERROR, "role=sender"), 2, "non-IPv4 error");

    // Multicast destination.
    let (rc, sent) = h.send_to(([224, 0, 0, 1], 40123), &[b"x"]);
    assert_eq!(rc, -1);
    assert_eq!(sent, 0);
    assert_eq!(h.counter(TYPE_ERROR, "role=sender"), 3, "multicast error");

    // Unresolved ARP: retryable zero, ARP-miss counter bumps.
    let (rc, sent) = h.send_to(([10, 0, 0, 99], 40123), &[b"x"]);
    assert_eq!((rc, sent), (0, 0), "unresolved ARP is a retryable zero");
    assert_eq!(h.counter(TYPE_ARP_MISS, "role=sender"), 1, "ARP miss counted");

    // Mbuf exhaustion: with no mbufs available, the build fails immediately.
    unsafe { rusteron_dpdk_fake_set_pool_avail(0) };
    let (rc, sent) = h.send_to((PEER_IP, 40123), &[b"x"]);
    assert_eq!((rc, sent), (0, 0));
    assert_eq!(h.counter(TYPE_NOBUFS, "role=sender"), 1, "nobufs counted");
    unsafe { rusteron_dpdk_fake_set_pool_avail(64) };

    // NIC backpressure: the fake accepts nothing in the flush.
    unsafe { rusteron_dpdk_fake_set_tx_burst_cap(0) };
    let (rc, sent) = h.send_to((PEER_IP, 40123), &[b"x"]);
    assert_eq!((rc, sent), (0, 0), "backpressure stops the burst");
    assert_eq!(h.counter(TYPE_TX_EAGAIN, "role=sender"), 1, "tx-eagain counted");
    unsafe { rusteron_dpdk_fake_set_tx_burst_cap(64) };
    assert_no_leak();
}

// ---------------------------------------------------------------------------
// RX counters: per-reject-class attribution
// ---------------------------------------------------------------------------

/// A helper that injects a frame, polls, and returns the poll work count.
fn inject_and_poll(h: &mut Harness, p: &mut Poller, frame: &[u8]) -> c_int {
    assert_eq!(
        unsafe { rusteron_dpdk_fake_rx_inject(RECEIVER_PORT, frame.as_ptr(), frame.len(), RX_GOOD, 1) },
        0
    );
    let mut bytes = 0i64;
    let clientd = &mut h.rx_dgrams as *mut Vec<RxDatagram> as *mut c_void;
    p.poll(16, clientd, &mut bytes)
}

/// Every poll also times the gap since the port's previous one, frames or
/// not: the agent's own loop. A stall of the agent (preempted, or its vCPU
/// descheduled by the host) shows as a gap, which a spike can be matched
/// against: the longest, and how many fell in 20-50, 50-100 and over 100 µs.
/// Gaps under 20 µs are an ordinary duty cycle and are not counted.
#[test]
#[serial]
fn agent_loop_gaps_counted_with_or_without_frames() {
    const RX: &str = "role=receiver bdf=0000:00:02.0 port=1 q=0 dir=rx";
    let mut h = Harness::new();
    h.init_rx(RECEIVER_IP, 60000, 1472);
    let mut p = h.poller();
    p.add(&mut *h.rx);
    let mut bytes = 0i64;
    let clientd = &mut h.rx_dgrams as *mut Vec<RxDatagram> as *mut c_void;
    let read = |h: &Harness| {
        (
            h.counter(TYPE_POLLER, &format!("{RX} poll-cycle-gap-max-ns")),
            h.counter(TYPE_POLLER, &format!("{RX} poll-cycle-gaps-20-50us")),
            h.counter(TYPE_POLLER, &format!("{RX} poll-cycle-gaps-50-100us")),
            h.counter(TYPE_POLLER, &format!("{RX} poll-cycle-gaps-over-100us")),
        )
    };
    pin_clock_ns(1_000_000);
    p.poll(16, clientd, &mut bytes); // the reference
    pin_clock_ns(1_010_000);
    p.poll(16, clientd, &mut bytes);
    assert_eq!(read(&h), (0, 0, 0, 0), "10 µs: an ordinary cycle");
    pin_clock_ns(1_040_000);
    p.poll(16, clientd, &mut bytes);
    assert_eq!(
        read(&h),
        (30_000, 1, 0, 0),
        "30 µs, no frames waiting: still the agent's gap"
    );
    pin_clock_ns(1_110_000);
    p.poll(16, clientd, &mut bytes);
    assert_eq!(read(&h), (70_000, 1, 1, 0));
    pin_clock_ns(1_310_000);
    p.poll(16, clientd, &mut bytes);
    assert_eq!(read(&h), (200_000, 1, 1, 1));
    pin_clock_ns(1_320_000);
    p.poll(16, clientd, &mut bytes);
    assert_eq!(read(&h), (200_000, 1, 1, 1), "the max is kept");
    assert_no_leak();
}

/// The receive poller records the longest gap between consecutive polls that
/// found frames waiting, how many of those gaps reached 100 µs, and how many
/// polls came back with a full burst: the evidence for a receive ring
/// overflowing behind a stalled agent. Gaps before empty polls do not count.
#[test]
#[serial]
fn rx_poll_gaps_and_full_bursts_counted() {
    const RX: &str = "role=receiver bdf=0000:00:02.0 port=1 q=0 dir=rx";
    let mut h = Harness::new();
    h.init_rx(RECEIVER_IP, 60000, 1472);
    let mut p = h.poller();
    p.add(&mut *h.rx);
    let frame = frame_to_receiver(60000, b"gap");
    let inject = |n: usize| {
        for _ in 0..n {
            assert_eq!(
                unsafe { rusteron_dpdk_fake_rx_inject(RECEIVER_PORT, frame.as_ptr(), frame.len(), RX_GOOD, 1) },
                0
            );
        }
    };
    let mut bytes = 0i64;
    let clientd = &mut h.rx_dgrams as *mut Vec<RxDatagram> as *mut c_void;
    let gap_max = |h: &Harness| h.counter(TYPE_POLLER, &format!("{RX} rx-poll-gap-max-ns"));
    let gaps_100us = |h: &Harness| h.counter(TYPE_POLLER, &format!("{RX} rx-poll-gaps-over-100us"));
    let full = |h: &Harness| h.counter(TYPE_POLLER, &format!("{RX} rx-burst-full"));

    pin_clock_ns(1_000_000);
    assert_eq!(
        p.poll(16, clientd, &mut bytes),
        0,
        "the first poll only sets the reference"
    );

    pin_clock_ns(1_020_000);
    inject(1);
    assert_eq!(p.poll(16, clientd, &mut bytes), 1);
    assert_eq!((gap_max(&h), gaps_100us(&h)), (20_000, 0), "a 20 µs gap before a frame");

    pin_clock_ns(1_170_000);
    inject(1);
    assert_eq!(p.poll(16, clientd, &mut bytes), 1);
    assert_eq!((gap_max(&h), gaps_100us(&h)), (150_000, 1), "a 150 µs gap");

    pin_clock_ns(1_500_000);
    assert_eq!(p.poll(16, clientd, &mut bytes), 0);
    assert_eq!(
        (gap_max(&h), gaps_100us(&h)),
        (150_000, 1),
        "a gap before an empty poll is not counted"
    );
    assert_eq!(full(&h), 0);

    // A burst-sized backlog (vlen 16 < burst 64, so the limit is 16).
    pin_clock_ns(1_510_000);
    inject(16);
    assert_eq!(p.poll(16, clientd, &mut bytes), 16);
    assert_eq!(full(&h), 1, "the poll drained a full burst: a backlog");
    assert_no_leak();
}

/// The housekeeping thread samples the PMD statistics on its own, once a
/// second, with no help from the hot path.
#[test]
#[serial]
fn housekeeping_thread_samples_by_itself() {
    let _env = TestEnv::new();
    unsafe { rusteron_dpdk_test_set_housekeeping_thread(1) };
    let mut h = Harness::new_after_env();
    install_xstats(&["rx_missed"], &[5]);
    h.init_rx(RECEIVER_IP, 60000, 1472);
    assert_eq!(h.counter(TYPE_EXTENDED_STATS, "rx_missed"), 5);
    install_xstats(&["rx_missed"], &[42]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while h.counter(TYPE_EXTENDED_STATS, "rx_missed") != 42 {
        assert!(
            std::time::Instant::now() < deadline,
            "the housekeeping thread never sampled"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    drop(h);
    assert_no_leak();
}

/// A peer whose MAC changed (a pod rescheduled onto another node, a failover)
/// is noticed from its own traffic on the same NIC (each NIC keeps its own
/// table, owned by its agent): a frame from a learned next hop's IP with a
/// different source MAC makes the next send refresh at once, not 30 s later,
/// while still sending to the cached MAC meanwhile. A frame from the known MAC
/// changes nothing.
#[test]
#[serial]
fn peer_mac_change_seen_on_receive_triggers_an_immediate_refresh() {
    const NEW_MAC: [u8; 6] = [0x02, 0, 0, 0, 0, 0x51];
    const SENDER_PORT: u16 = 0;
    const SENDER_MAC: [u8; 6] = [0x02, 0, 0, 0, 0, 0x01];
    let mut h = Harness::new();
    h.init_tx(SENDER_IP, 40000, None, 1472);
    let mut p = h.poller();
    p.add(&mut *h.tx); // the sender agent polls its NIC for status messages
    pin_clock_ms(1_000);
    h.arp_seed(PEER_IP, PEER_MAC);
    let arp_requests = || {
        (0..unsafe { rusteron_dpdk_fake_capture_count() })
            .filter(|&i| {
                let mut c = unsafe { std::mem::zeroed::<FakeCapture>() };
                unsafe { rusteron_dpdk_fake_capture_at(i, &mut c) };
                c.data[12] == 0x08 && c.data[13] == 0x06
            })
            .count()
    };
    let mut bytes = 0i64;
    let clientd = &mut h.rx_dgrams as *mut Vec<RxDatagram> as *mut c_void;
    let from = |mac: [u8; 6]| build_udp_frame(SENDER_MAC, mac, PEER_IP, SENDER_IP, 7000, 40000, b"hi");

    // Traffic from the known MAC: nothing to do.
    let frame = from(PEER_MAC);
    assert_eq!(
        unsafe { rusteron_dpdk_fake_rx_inject(SENDER_PORT, frame.as_ptr(), frame.len(), RX_GOOD, 1) },
        0
    );
    assert_eq!(p.poll(16, clientd, &mut bytes), 1);
    pin_clock_ms(2_000);
    assert_eq!(h.send_to((PEER_IP, 40123), &[b"x"]).0, 1);
    assert_eq!(arp_requests(), 0, "same MAC: no refresh");

    // Traffic from a new MAC: the next send still goes out (to the cached
    // MAC) and a refresh request goes out with it, 29 s early.
    let frame = from(NEW_MAC);
    assert_eq!(
        unsafe { rusteron_dpdk_fake_rx_inject(SENDER_PORT, frame.as_ptr(), frame.len(), RX_GOOD, 1) },
        0
    );
    assert_eq!(p.poll(16, clientd, &mut bytes), 1);
    assert_eq!(h.send_to((PEER_IP, 40123), &[b"y"]).0, 1, "never a zero send");
    assert_eq!(arp_requests(), 1, "an immediate refresh");
    assert_no_leak();
}

/// The NIC's own receive drops (rte_eth_stats ierrors/imissed/rx_nombuf:
/// frames the PMD never hands up, e.g. oversized or ring-full) are mirrored by
/// the housekeeping sample, never the hot path.
#[test]
#[serial]
fn nic_receive_drops_are_mirrored_by_housekeeping() {
    let mut h = Harness::new();
    pin_clock_ms(1_000);
    h.init_rx(RECEIVER_IP, 60000, 1472);
    unsafe { rusteron_dpdk_fake_set_basic_stats(3, 5, 7) };
    pin_clock_ms(2_000);
    unsafe { rusteron_dpdk_test_housekeeping_tick(h.native) };
    const RX: &str = "role=receiver bdf=0000:00:02.0 port=1 q=0 dir=rx";
    assert_eq!(h.counter(TYPE_MISSED_PACKETS, &format!("{RX} nic-rx-errors")), 3);
    assert_eq!(h.counter(TYPE_MISSED_PACKETS, &format!("{RX} nic-rx-missed")), 5);
    assert_eq!(h.counter(TYPE_MISSED_PACKETS, &format!("{RX} nic-rx-nombuf")), 7);
}

/// RUSTERON_DPDK_RECEIVER_EXTRA_IPV4 adds addresses the receiver NIC answers
/// for (a secondary IP, a VIP): datagrams to them are delivered, not dropped
/// as foreign, to a transport bound on them or on 0.0.0.0.
#[test]
#[serial]
fn extra_local_ips_are_delivered() {
    let _guard = EnvGuard::set("RUSTERON_DPDK_RECEIVER_EXTRA_IPV4", "10.0.1.9");
    let frame = build_udp_frame(RECEIVER_MAC, PEER_MAC, PEER_IP, [10, 0, 1, 9], 7000, 60000, b"vip");
    // As a kernel socket: a wildcard bind hears every local address, a bind
    // on the NIC's own address only that address.
    for (bind, delivered) in [([0, 0, 0, 0], 1), (RECEIVER_IP, 0)] {
        let mut h = Harness::new();
        h.init_rx(bind, 60000, 1472);
        let mut p = h.poller();
        p.add(&mut *h.rx);
        assert_eq!(
            unsafe { rusteron_dpdk_fake_rx_inject(RECEIVER_PORT, frame.as_ptr(), frame.len(), RX_GOOD, 1) },
            0
        );
        let mut bytes = 0i64;
        let clientd = &mut h.rx_dgrams as *mut Vec<RxDatagram> as *mut c_void;
        assert_eq!(p.poll(16, clientd, &mut bytes), delivered, "bound on {bind:?}");
        drop(p);
        assert_no_leak();
    }
}

/// A wildcard subscription answers each peer from the address that peer
/// reached it on, as the kernel picks a reply's source, and follows the peer
/// when it switches between the NIC's address and an extra one.
#[test]
#[serial]
fn wildcard_replies_leave_from_the_address_the_peer_used() {
    let _guard = EnvGuard::set("RUSTERON_DPDK_RECEIVER_EXTRA_IPV4", "10.0.1.9");
    let mut h = Harness::new();
    h.init_rx([0, 0, 0, 0], 60000, 1472);
    h.arp_seed([10, 0, 1, 254], PEER_MAC); // the receiver NIC's gateway towards the peer
    let mut p = h.poller();
    p.add(&mut *h.rx);
    let clientd = &mut h.rx_dgrams as *mut Vec<RxDatagram> as *mut c_void;
    let last_source = || {
        let n = unsafe { rusteron_dpdk_fake_capture_count() };
        let mut c = unsafe { std::mem::zeroed::<FakeCapture>() };
        unsafe { rusteron_dpdk_fake_capture_at(n - 1, &mut c) };
        [c.data[26], c.data[27], c.data[28], c.data[29]]
    };
    // Two peers in one /24 (neighbours must not share a cache slot), each
    // switching address; replies follow each independently.
    let other_peer = [10, 0, 0, 6];
    for (peer, to) in [
        (PEER_IP, [10, 0, 1, 9]),
        (other_peer, RECEIVER_IP),
        (PEER_IP, [10, 0, 1, 9]),
        (other_peer, RECEIVER_IP),
        (other_peer, [10, 0, 1, 9]),
        (PEER_IP, [10, 0, 1, 9]),
        (other_peer, RECEIVER_IP),
    ] {
        let frame = build_udp_frame(RECEIVER_MAC, PEER_MAC, peer, to, 7000, 60000, b"setup");
        assert_eq!(
            unsafe { rusteron_dpdk_fake_rx_inject(RECEIVER_PORT, frame.as_ptr(), frame.len(), RX_GOOD, 1) },
            0
        );
        let mut bytes = 0i64;
        assert_eq!(p.poll(16, clientd, &mut bytes), 1);

        let msg = b"status";
        let mut iov = [iovec {
            iov_base: msg.as_ptr() as *mut c_void,
            iov_len: msg.len(),
        }];
        let mut dst = SockAddrIn::new(peer, 7000);
        let mut sent = 0i64;
        let rc = unsafe {
            (*h.bindings).send_func.unwrap()(
                &mut *h.data_paths,
                &mut *h.rx,
                dst.storage_mut(),
                iov.as_mut_ptr(),
                1,
                &mut sent,
            )
        };
        assert_eq!(rc, 1, "{}", last_error());
        assert_eq!(
            last_source(),
            to,
            "the reply to {peer:?} leaves from the address it used"
        );
    }
    // Each peer's latest address is remembered, not just the last peer's.
    for (peer, to) in [(PEER_IP, [10, 0, 1, 9]), (other_peer, RECEIVER_IP)] {
        let msg = b"status";
        let mut iov = [iovec {
            iov_base: msg.as_ptr() as *mut c_void,
            iov_len: msg.len(),
        }];
        let mut dst = SockAddrIn::new(peer, 7000);
        let mut sent = 0i64;
        let rc = unsafe {
            (*h.bindings).send_func.unwrap()(
                &mut *h.data_paths,
                &mut *h.rx,
                dst.storage_mut(),
                iov.as_mut_ptr(),
                1,
                &mut sent,
            )
        };
        assert_eq!(rc, 1, "{}", last_error());
        assert_eq!(last_source(), to);
    }
    drop(p);
    assert_no_leak();
}

/// With NIC RX timestamps on, each timestamped datagram adds its delay from
/// NIC arrival to transport dispatch (host CLOCK_REALTIME minus the NIC stamp)
/// to the type-81 sum, count and max; frames without a stamp are not counted.
#[test]
#[serial]
fn rx_hw_delay_counted_for_timestamped_frames() {
    const RX: &str = "role=receiver bdf=0000:00:02.0 port=1 q=0 dir=rx";
    let mut h = Harness::with_rx_hw_timestamp();
    h.init_rx(RECEIVER_IP, 60000, 1472);
    let mut p = h.poller();
    p.add(&mut *h.rx);
    unsafe { rusteron_dpdk_test_set_realtime_ns(1_000_000) };

    let frame = frame_to_receiver(60000, b"stamped");
    for hw_rx_ns in [999_000u64, 999_500, 0] {
        assert_eq!(
            unsafe {
                rusteron_dpdk_fake_rx_inject_hw_ts(RECEIVER_PORT, frame.as_ptr(), frame.len(), RX_GOOD, 1, hw_rx_ns)
            },
            0
        );
    }
    let mut bytes = 0i64;
    let clientd = &mut h.rx_dgrams as *mut Vec<RxDatagram> as *mut c_void;
    assert_eq!(p.poll(16, clientd, &mut bytes), 3, "all three dispatched");

    assert_eq!(h.counter(TYPE_RX_HW_DELAY, &format!("{RX} rx-hw-delay-max-ns")), 1_000);
    assert_eq!(h.counter(TYPE_RX_HW_DELAY, &format!("{RX} rx-hw-delay-sum-ns")), 1_500);
    assert_eq!(h.counter(TYPE_RX_HW_DELAY, &format!("{RX} rx-hw-timestamped")), 2);

    // A later, smaller delay leaves the max alone.
    unsafe { rusteron_dpdk_test_set_realtime_ns(2_000_000) };
    assert_eq!(
        unsafe {
            rusteron_dpdk_fake_rx_inject_hw_ts(RECEIVER_PORT, frame.as_ptr(), frame.len(), RX_GOOD, 1, 1_999_900)
        },
        0
    );
    assert_eq!(p.poll(16, clientd, &mut bytes), 1);
    assert_eq!(h.counter(TYPE_RX_HW_DELAY, &format!("{RX} rx-hw-delay-max-ns")), 1_000);
    assert_eq!(h.counter(TYPE_RX_HW_DELAY, &format!("{RX} rx-hw-delay-sum-ns")), 1_600);
    assert_eq!(h.counter(TYPE_RX_HW_DELAY, &format!("{RX} rx-hw-timestamped")), 3);
    assert_no_leak();
}

/// Accepted frames bump RX pkts/bytes and the poller counter; rejected classes
/// bump exactly their own counters (plan §9).
#[test]
#[serial]
fn rx_pkts_bytes_and_reject_classes_counted() {
    let mut h = Harness::new();
    h.init_rx(RECEIVER_IP, 60000, 1472);
    let mut p = h.poller();
    p.add(&mut *h.rx);

    // Accepted: RX pkts/bytes + poller.
    assert_eq!(inject_and_poll(&mut h, &mut p, &frame_to_receiver(60000, b"accept")), 1);
    assert_eq!(h.rx_dgrams.len(), 1);
    assert_eq!(h.counter(TYPE_PKTS, "dir=rx"), 1);
    assert_eq!(h.counter(TYPE_BYTES, "dir=rx"), 6, "payload length, not the frame");
    assert_eq!(h.counter(TYPE_POLLER, "dir=rx"), 1, "one frame processed per poll");

    // IPv6 ethertype -> discard.
    let mut frame = frame_to_receiver(60000, b"x");
    frame[12] = 0x86;
    frame[13] = 0xDD;
    assert_eq!(inject_and_poll(&mut h, &mut p, &frame), 0);
    assert_eq!(h.counter(TYPE_RX_RECEIVER_DISCARD, "role=receiver"), 1);

    // VLAN -> discard.
    let mut frame = frame_to_receiver(60000, b"x");
    frame[12] = 0x81;
    frame[13] = 0x00;
    assert_eq!(inject_and_poll(&mut h, &mut p, &frame), 0);
    assert_eq!(h.counter(TYPE_RX_RECEIVER_DISCARD, "role=receiver"), 2);

    // Unknown ethertype -> unsupported-ethertype.
    let mut frame = frame_to_receiver(60000, b"x");
    frame[12] = 0x12;
    frame[13] = 0x34;
    assert_eq!(inject_and_poll(&mut h, &mut p, &frame), 0);
    assert_eq!(h.counter(TYPE_UNSUPPORTED_ETHERTYPE, "role=receiver"), 1);

    // IP fragment -> fragmented.
    let mut frame = frame_to_receiver(60000, b"x");
    frame[20] = 0x20; // MF set
    assert_eq!(inject_and_poll(&mut h, &mut p, &frame), 0);
    assert_eq!(h.counter(TYPE_FRAGMENTED, "role=receiver"), 1);

    // Non-UDP protocol -> unsupported-protocol.
    let mut frame = frame_to_receiver(60000, b"x");
    frame[23] = 6; // TCP
    assert_eq!(inject_and_poll(&mut h, &mut p, &frame), 0);
    assert_eq!(h.counter(TYPE_UNSUPPORTED_PROTOCOL, "role=receiver"), 1);

    // Bad checksum (NIC verdict) -> checksum-failure.
    let frame = frame_to_receiver(60000, b"x");
    assert_eq!(
        unsafe { rusteron_dpdk_fake_rx_inject(RECEIVER_PORT, frame.as_ptr(), frame.len(), 0b100, 1) },
        0
    );
    let mut bytes = 0i64;
    let clientd = &mut h.rx_dgrams as *mut Vec<RxDatagram> as *mut c_void;
    p.poll(16, clientd, &mut bytes);
    assert_eq!(h.counter(TYPE_CHECKSUM_FAILURE, "role=receiver"), 1);

    // Foreign destination / unregistered port -> discard; queue-drop for the
    // endpoint-map miss only.
    let foreign = build_udp_frame(RECEIVER_MAC, PEER_MAC, PEER_IP, [10, 0, 1, 99], 7000, 60000, b"f");
    assert_eq!(inject_and_poll(&mut h, &mut p, &foreign), 0);
    let unknown = frame_to_receiver(60099, b"u");
    assert_eq!(inject_and_poll(&mut h, &mut p, &unknown), 0);
    let discard = h.counter(TYPE_RX_RECEIVER_DISCARD, "role=receiver");
    assert_eq!(discard, 4, "ipv6+vlan+foreign+unknown all discarded");
    assert_eq!(
        h.counter(TYPE_QUEUE_DROP, "role=receiver"),
        1,
        "only the unknown-port misses the map"
    );

    // Accepted is unaffected by the rejects.
    assert_eq!(h.counter(TYPE_PKTS, "dir=rx"), 1);
    assert_eq!(h.counter(TYPE_BYTES, "dir=rx"), 6);
    assert_no_leak();
}

// ---------------------------------------------------------------------------
// ENA extended statistics (plan §9: type 92 mirrors, 1 Hz sample)
// ---------------------------------------------------------------------------

/// The fake PMD's xstat table: an ENA SRD-style counter plus a *missed* entry
/// (the ENA PMD exposes rx_missed variants), so both the type-92 mirrors and
/// the plan §9 type-84 missed-packets mirror are exercised.
fn install_xstats(names: &[&str], values: &[u64]) {
    assert_eq!(names.len(), values.len());
    let cnames: Vec<std::ffi::CString> = names.iter().map(|n| std::ffi::CString::new(*n).unwrap()).collect();
    let ptrs: Vec<*const c_char> = cnames.iter().map(|c| c.as_ptr()).collect();
    unsafe { rusteron_dpdk_fake_set_xstats(ptrs.as_ptr(), values.as_ptr(), names.len() as u32) };
}

/// ENA extended statistics appear as type-92 counters named by the PMD, are
/// re-read at most once per second by the housekeeping thread and never by
/// the hot path (reading them can be an admin command to the NIC), and a
/// *missed* xstat mirrors type 84.
#[test]
#[serial]
fn ena_xstats_mirrored_and_sampled_at_1hz() {
    // Construct the harness first: its TestEnv::new() resets the fakes, which
    // would wipe an earlier install_xstats. Install the PMD xstat table and pin
    // the clock before registration so the 1 Hz gate is deterministic.
    let mut h = Harness::new();
    install_xstats(
        &["rx_missed", "ena_srd_llq_rx_packets", "ena_express_rx_drops"],
        &[11, 2200, 3],
    );
    pin_clock_ms(1_000);
    h.init_rx(RECEIVER_IP, 60000, 1472);
    let mut p = h.poller();
    p.add(&mut *h.rx);

    // The three PMD xstats are mirrored as type-92 counters with their names.
    let (v, t, label) = h
        .counters
        .find(TYPE_EXTENDED_STATS, "rx_missed")
        .expect("rx_missed mirror");
    assert_eq!(t, TYPE_EXTENDED_STATS);
    assert_eq!(v, 11);
    assert!(label.starts_with("rx_missed"), "label is the PMD name: {label}");
    assert_eq!(h.counter(TYPE_EXTENDED_STATS, "ena_srd_llq_rx_packets"), 2200);
    assert_eq!(h.counter(TYPE_EXTENDED_STATS, "ena_express_rx_drops"), 3);

    // The *missed* mirror (type 84) is populated by the sample, not by
    // registration — it reads 0 until the first 1 Hz refresh.
    assert_eq!(
        h.counter(TYPE_MISSED_PACKETS, "role=receiver"),
        0,
        "mirror waits for the sample"
    );

    // Change the PMD values; within the 1 Hz window nothing is re-read.
    install_xstats(
        &["rx_missed", "ena_srd_llq_rx_packets", "ena_express_rx_drops"],
        &[99, 4400, 7],
    );
    let mut bytes = 0i64;
    let clientd = &mut h.rx_dgrams as *mut Vec<RxDatagram> as *mut c_void;
    unsafe { rusteron_dpdk_test_housekeeping_tick(h.native) };
    assert_eq!(h.counter(TYPE_EXTENDED_STATS, "rx_missed"), 11, "within 1 Hz: stale");
    assert_eq!(h.counter(TYPE_MISSED_PACKETS, "role=receiver"), 0, "within 1 Hz: stale");

    // A second on, the hot path still never reads them.
    pin_clock_ms(2_000);
    assert_eq!(p.poll(16, clientd, &mut bytes), 0);
    assert_eq!(
        h.counter(TYPE_EXTENDED_STATS, "rx_missed"),
        11,
        "the hot path never samples"
    );

    // The housekeeping tick does.
    unsafe { rusteron_dpdk_test_housekeeping_tick(h.native) };
    assert_eq!(h.counter(TYPE_EXTENDED_STATS, "rx_missed"), 99, "1 Hz sample re-reads");
    assert_eq!(h.counter(TYPE_EXTENDED_STATS, "ena_srd_llq_rx_packets"), 4400);
    assert_eq!(h.counter(TYPE_EXTENDED_STATS, "ena_express_rx_drops"), 7);
    assert_eq!(
        h.counter(TYPE_MISSED_PACKETS, "role=receiver"),
        99,
        "type-84 mirror follows"
    );

    // Mempool availability refreshes on the sample too.
    assert_eq!(h.counter(TYPE_MEMPOOL_AVAILABLE, "role=receiver"), 64);

    assert_no_leak();
}
