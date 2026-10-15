//! Backend configuration strings.
#![allow(missing_docs)]

use ergon_io::{Backend, Config};

type R = Result<(), Box<dyn std::error::Error>>;

#[test]
fn every_backend_name_round_trips() -> R {
    for b in Backend::ALL {
        assert_eq!(b.name().parse::<Backend>()?, b);
        assert_eq!(Config::parse(b.name())?, Config::new(b));
    }
    Ok(())
}

#[test]
fn keys_override_defaults() -> R {
    let c =
        Config::parse("uring-sqpoll, sqpoll_cpu=3, busy_poll_us=2, rcvbuf=0, rx_timestamps=false")?;
    assert_eq!(c.backend, Backend::UringSqpoll);
    assert_eq!(c.sqpoll_cpu, Some(3));
    assert_eq!(c.busy_poll_us, 2);
    assert_eq!(c.rcvbuf, None);
    assert!(!c.rx_timestamps);
    Ok(())
}

#[test]
fn busy_poll_preference_and_budget_parse() -> R {
    let d = Config::new(Backend::RecvBusy);
    assert!(!d.prefer_busy_poll);
    assert_eq!(d.busy_poll_budget, None);
    let c = Config::parse("recv-busy, prefer_busy_poll=true, busy_poll_budget=64")?;
    assert!(c.prefer_busy_poll);
    assert_eq!(c.busy_poll_budget, Some(64));
    assert_eq!(
        Config::parse("recv-busy,busy_poll_budget=0")?.busy_poll_budget,
        None
    );
    for bad in [
        "recv-busy,prefer_busy_poll=1",
        "recv-busy,busy_poll_budget=65536",
        // Only backends that poll the NIC can prefer busy polling, and only the
        // socket-level ones take a packet budget.
        "epoll-spin,prefer_busy_poll=true",
        "uring-spin,prefer_busy_poll=true",
        "uring-sqpoll,busy_poll_budget=16",
        // The kernel takes the busy-poll time as a signed int.
        "recv-busy,busy_poll_us=2147483648",
    ] {
        assert!(Config::parse(bad).is_err(), "{bad:?} accepted");
    }
    let mut c = Config::new(Backend::UringSpin);
    c.prefer_busy_poll = true;
    assert!(c.validate().is_err(), "struct-built config not validated");
    assert!(Config::parse("uring-sqpoll,prefer_busy_poll=true")?.prefer_busy_poll);
    assert!(Config::parse("epoll-busy,prefer_busy_poll=true,busy_poll_budget=32").is_ok());
    assert!(Config::parse("recv-busy,busy_poll_us=2147483647").is_ok());
    Ok(())
}

#[test]
fn malformed_input_is_rejected() {
    for bad in [
        "",
        "epoll",
        "epoll-spin,wait_us",
        "epoll-spin,wait_us=x",
        "epoll-spin,nope=1",
    ] {
        assert!(Config::parse(bad).is_err(), "{bad:?} accepted");
    }
}

#[test]
fn only_busy_poll_wait_backends_may_sleep() {
    for b in Backend::ALL {
        assert_eq!(
            b.may_sleep(),
            matches!(b, Backend::EpollBusy | Backend::UringNapi),
            "{b}"
        );
    }
}

#[test]
fn per_queue_applies_to_recv_busy_only() -> R {
    assert!(!Config::new(Backend::RecvBusy).per_queue);
    assert!(Config::parse("recv-busy,per_queue=true")?.per_queue);
    for bad in [
        "recv-busy,per_queue=1",
        "epoll-spin,per_queue=true",
        "uring-sqpoll,per_queue=true",
    ] {
        assert!(Config::parse(bad).is_err(), "{bad:?} accepted");
    }
    Ok(())
}
