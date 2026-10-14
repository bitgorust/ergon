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
