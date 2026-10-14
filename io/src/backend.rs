//! Backend selection: which kernel interface the `Driver` uses, chosen at
//! run time from configuration.
//!
//! | backend | how it waits | NIC polling | sleeps in the kernel | extra core |
//! |---|---|---|---|---|
//! | `epoll-spin` | `epoll_wait(0)` every poll, then `recvmsg` on ready sockets | no (interrupts) | never | no |
//! | `recv-busy` | non-blocking `recvmsg` on every socket every poll, `SO_BUSY_POLL` set: an empty read runs one NAPI pass | yes, one pass per socket per poll | never | no |
//! | `uring-spin` | `IORING_OP_RECVMSG`, `DEFER_TASKRUN`; enters the kernel only when it flags work | no (interrupts) | never | no |
//! | `uring-sqpoll` | `SQPOLL` kernel thread submits, completes and polls the NIC (NAPI) | yes, on the kernel thread | never (the application thread) | yes |
//! | `epoll-busy` | `epoll_pwait2(wait_us)` with epoll busy poll for `busy_poll_us` | yes, inside the wait | up to `wait_us` | no |
//! | `uring-napi` | `io_uring_enter(GETEVENTS, 1, wait_us)` with NAPI busy poll | yes, inside the wait | up to `wait_us`, and on every idle poll even with `wait_us = 0` | no |
//!
//! Use cases:
//!
//! - **Latency-critical busy-spin loop** (returns to the application every poll,
//!   NIC polled): `recv-busy`, or `uring-sqpoll` if a core can be spared for the
//!   kernel thread.
//! - **Busy-spin loop, interrupt-driven** (never blocks; sees a packet once the
//!   NIC interrupt has delivered it): `epoll-spin`, or `uring-spin`, which makes
//!   far fewer syscalls for the same latency. `epoll_wait(0)` never blocks, but
//!   with a zero timeout the kernel also skips its NIC busy-poll step.
//! - **Not latency-critical, CPU matters** (the thread may nap in the kernel):
//!   `epoll-busy` and `uring-napi` poll the NIC only inside a timed wait.
//!
//! Configuration strings are `backend[,key=value...]`, for example
//! `uring-sqpoll,sqpoll_cpu=3` or `epoll-busy,busy_poll_us=50,wait_us=100`.

use std::fmt;
use std::str::FromStr;

use crate::error::{Error, Result};

/// The kernel interface a `Driver` uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    /// `epoll_wait(0)` spin, interrupt-driven.
    EpollSpin,
    /// Non-blocking `recvmsg` spin with `SO_BUSY_POLL`: one NAPI pass per empty read.
    RecvBusy,
    /// `io_uring`, `DEFER_TASKRUN`; enters the kernel only when it flags work.
    UringSpin,
    /// `io_uring` with an `SQPOLL` kernel thread that also polls the NIC.
    UringSqpoll,
    /// `epoll_pwait2` with epoll busy poll; sleeps up to `wait_us`.
    EpollBusy,
    /// `io_uring` waiting in `io_uring_enter` with NAPI busy poll; sleeps.
    UringNapi,
}

impl Backend {
    /// Every backend, never-sleeping ones first.
    pub const ALL: [Self; 6] = [
        Self::EpollSpin,
        Self::RecvBusy,
        Self::UringSpin,
        Self::UringSqpoll,
        Self::EpollBusy,
        Self::UringNapi,
    ];

    /// The configuration name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::EpollSpin => "epoll-spin",
            Self::RecvBusy => "recv-busy",
            Self::UringSpin => "uring-spin",
            Self::UringSqpoll => "uring-sqpoll",
            Self::EpollBusy => "epoll-busy",
            Self::UringNapi => "uring-napi",
        }
    }

    /// Whether `Driver::poll` can sleep in the kernel with this backend.
    #[must_use]
    pub const fn may_sleep(self) -> bool {
        matches!(self, Self::EpollBusy | Self::UringNapi)
    }

    /// Whether the backend polls the NIC instead of waiting for interrupts.
    #[must_use]
    pub const fn polls_nic(self) -> bool {
        matches!(
            self,
            Self::RecvBusy | Self::UringSqpoll | Self::EpollBusy | Self::UringNapi
        )
    }

    /// Whether the backend is built on `io_uring`.
    #[must_use]
    pub const fn is_uring(self) -> bool {
        matches!(self, Self::UringSpin | Self::UringSqpoll | Self::UringNapi)
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Backend {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|b| b.name() == s)
            .ok_or_else(|| Error::Config(format!("unknown backend {s:?}")))
    }
}

/// Driver configuration. [`Config::new`] gives the defaults for a backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The kernel interface.
    pub backend: Backend,
    /// NIC busy-poll budget in microseconds. `epoll-busy` and `uring-napi` poll
    /// this long before sleeping; `recv-busy` and `uring-sqpoll` only need it
    /// non-zero (they make one pass per call).
    pub busy_poll_us: u32,
    /// Longest kernel wait for `epoll-busy` and `uring-napi`, in microseconds.
    pub wait_us: u32,
    /// CPU for the `SQPOLL` kernel thread: ideally an isolated core of its own.
    /// `None` uses the first housekeeping CPU (online, not isolated, not the
    /// calling thread's), never the polling thread's core.
    pub sqpoll_cpu: Option<u32>,
    /// Idle time after which the `SQPOLL` thread sleeps, in milliseconds. NAPI
    /// passes do not count as work, so keep it long.
    pub sqpoll_idle_ms: u32,
    /// `io_uring` submission queue entries.
    pub ring_entries: u32,
    /// `SO_RCVBUF` in bytes for every socket (`None` keeps the kernel default).
    pub rcvbuf: Option<u32>,
    /// Request kernel software (and, where supported, NIC) receive timestamps.
    pub rx_timestamps: bool,
    /// Deadline for `Driver::connect` (DNS, TCP, TLS, upgrade), in milliseconds.
    pub connect_timeout_ms: u32,
    /// CPU for the `Driver::connect_background` helper thread. `None` runs it
    /// on the housekeeping CPUs (online, not isolated, not the polling CPU), so
    /// the handshake never competes with the polling thread for its core.
    pub connect_cpu: Option<u32>,
}

impl Config {
    /// Defaults for `backend`.
    #[must_use]
    pub const fn new(backend: Backend) -> Self {
        let (busy_poll_us, wait_us) = match backend {
            Backend::EpollBusy | Backend::UringNapi => (50, 100),
            Backend::RecvBusy | Backend::UringSqpoll => (1, 0),
            Backend::EpollSpin | Backend::UringSpin => (0, 0),
        };
        Self {
            backend,
            busy_poll_us,
            wait_us,
            sqpoll_cpu: None,
            sqpoll_idle_ms: 600_000,
            ring_entries: 256,
            rcvbuf: Some(4 << 20),
            rx_timestamps: true,
            connect_timeout_ms: 10_000,
            connect_cpu: None,
        }
    }

    /// Parses `backend[,key=value...]`. Keys: `busy_poll_us`, `wait_us`,
    /// `sqpoll_cpu`, `sqpoll_idle_ms`, `ring_entries`, `connect_timeout_ms`, `connect_cpu`, `rcvbuf` (0 keeps the
    /// kernel default), `rx_timestamps` (`true`/`false`).
    ///
    /// # Errors
    /// Unknown backend or key, or a malformed value.
    pub fn parse(s: &str) -> Result<Self> {
        let mut parts = s.split(',').map(str::trim);
        let mut cfg = Self::new(parts.next().unwrap_or_default().parse()?);
        for kv in parts.filter(|p| !p.is_empty()) {
            let (k, v) = kv
                .split_once('=')
                .ok_or_else(|| Error::Config(format!("expected key=value, got {kv:?}")))?;
            let num = || -> Result<u32> {
                v.parse()
                    .map_err(|_| Error::Config(format!("{k}: bad number {v:?}")))
            };
            match k {
                "busy_poll_us" => cfg.busy_poll_us = num()?,
                "wait_us" => cfg.wait_us = num()?,
                "sqpoll_cpu" => cfg.sqpoll_cpu = Some(num()?),
                "sqpoll_idle_ms" => cfg.sqpoll_idle_ms = num()?,
                "ring_entries" => cfg.ring_entries = num()?,
                "connect_timeout_ms" => cfg.connect_timeout_ms = num()?,
                "connect_cpu" => cfg.connect_cpu = Some(num()?),
                "rcvbuf" => cfg.rcvbuf = Some(num()?).filter(|&b| b > 0),
                "rx_timestamps" => {
                    cfg.rx_timestamps = v
                        .parse()
                        .map_err(|_| Error::Config(format!("rx_timestamps: bad bool {v:?}")))?;
                }
                _ => return Err(Error::Config(format!("unknown key {k:?}"))),
            }
        }
        Ok(cfg)
    }
}

impl FromStr for Config {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}
