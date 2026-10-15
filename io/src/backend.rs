//! Backend selection: which kernel interface the `Driver` uses, chosen at
//! run time from configuration.
//!
//! | backend | how it waits | NIC polling | sleeps in the kernel | extra core |
//! |---|---|---|---|---|
//! | `epoll-spin` | `epoll_wait(0)` every poll, then `recvmsg` on ready sockets | no (interrupts) | never | no |
//! | `recv-busy` | non-blocking `recvmsg` on every socket every poll (with `per_queue`: one per NIC queue, then the sockets `epoll_wait(0)` reports ready), `SO_BUSY_POLL` set: an empty read runs one NAPI pass | yes, one pass per socket (or queue) per poll | never | no |
//! | `uring-spin` | `IORING_OP_RECVMSG`, `DEFER_TASKRUN`; enters the kernel only when it flags work | no (interrupts) | never | no |
//! | `uring-sqpoll` | `SQPOLL` kernel thread submits, completes and polls the NIC (NAPI) | yes, on the kernel thread | never (the application thread) | yes |
//! | `epoll-busy` | `epoll_pwait2(wait_us)` with epoll busy poll for `busy_poll_us` | yes, inside the wait | up to `wait_us` | no |
//! | `uring-napi` | `io_uring_enter(GETEVENTS, 1, wait_us)` with NAPI busy poll | yes, inside the wait | up to `wait_us`, and on every idle poll even with `wait_us = 0` | no |
//!
//! Choosing one (measured on AWS `c7i` and Azure; `io/benches/BENCHMARK.md`):
//! what decides it is how many sockets one loop serves and whether it may
//! have a second core.
//!
//! - **A few sockets per loop (up to about 10), one core:** `recv-busy`. It
//!   polls the NIC itself, so a packet is read without waiting for an
//!   interrupt; with one socket it equals `uring-sqpoll`. Its cost grows with
//!   the sockets: one `recvmsg` per socket per poll.
//! - **Many sockets per loop, a second core to spare:** `uring-sqpoll`. Its
//!   kernel thread polls each NIC queue once per pass and completes every
//!   socket's reads into one ring; best measured from 10 sockets up. It costs
//!   a spinning kernel thread per ring (rings in one process can share one;
//!   other processes cannot).
//! - **Many sockets per loop, one physical core** (two hyperthreads, as a
//!   pod with two vCPUs gets): `uring-sqpoll` with `sqpoll_cpu` set to the
//!   loop CPU's hyperthread sibling, about 1.5 µs behind a core of its own and
//!   still ahead of the single-CPU options.
//! - **Many sockets per loop, a single CPU:** `recv-busy` with `per_queue` (one
//!   busy-polled read per NIC queue, then only the sockets epoll reports
//!   ready), or `epoll-spin`; plain `recv-busy` scans every socket every poll
//!   and falls behind both from a few tens of sockets.
//! - **Busy-spin loop where CPU matters more than the last microseconds:**
//!   `uring-spin` (interrupt-driven like `epoll-spin`, about 1,000x fewer
//!   syscalls). `epoll_wait(0)` never blocks, but with a zero timeout the
//!   kernel also skips its NIC busy-poll step; mio is this path.
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
    /// Preferred busy polling (`SO_PREFER_BUSY_POLL`, or the `io_uring` / epoll
    /// equivalent): while the application keeps polling, the kernel leaves the
    /// NIC queue to it instead of also processing it from interrupts. It only
    /// suppresses interrupts when the device has `napi_defer_hard_irqs` and
    /// `gro_flush_timeout` set (sysfs). Backends that poll the NIC only; the
    /// socket option needs `CAP_NET_ADMIN`.
    pub prefer_busy_poll: bool,
    /// Packets per NIC busy-poll pass (`SO_BUSY_POLL_BUDGET`, or the epoll
    /// budget); `None` keeps the kernel default (8). `recv-busy` and
    /// `epoll-busy` only; raising it needs `CAP_NET_ADMIN`.
    pub busy_poll_budget: Option<u16>,
    /// `recv-busy` with many sockets: read one socket per NIC queue every
    /// poll (which polls each queue once), then only the sockets
    /// `epoll_wait(0)` reports ready, instead of reading every socket every
    /// poll. Costs one `epoll_wait` per poll; pays off once sockets outnumber
    /// the queues they share.
    pub per_queue: bool,
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
            prefer_busy_poll: false,
            busy_poll_budget: None,
            per_queue: false,
        }
    }

    /// Checks that every setting applies to the backend.
    ///
    /// # Errors
    /// `busy_poll_us` above `i32::MAX`, `prefer_busy_poll` on a backend that
    /// does not poll the NIC, or `busy_poll_budget` on one that does not poll
    /// through the socket or epoll.
    pub fn validate(&self) -> Result<()> {
        // The kernel takes the busy-poll time as a signed int.
        if i32::try_from(self.busy_poll_us).is_err() {
            return Err(Error::Config(format!(
                "busy_poll_us {} is above {}",
                self.busy_poll_us,
                i32::MAX
            )));
        }
        if self.prefer_busy_poll && !self.backend.polls_nic() {
            return Err(Error::Config(format!(
                "prefer_busy_poll needs a backend that polls the NIC, not {}",
                self.backend
            )));
        }
        if self.per_queue && self.backend != Backend::RecvBusy {
            return Err(Error::Config(format!(
                "per_queue applies to recv-busy, not {}",
                self.backend
            )));
        }
        if self.busy_poll_budget.is_some()
            && !matches!(self.backend, Backend::RecvBusy | Backend::EpollBusy)
        {
            return Err(Error::Config(format!(
                "busy_poll_budget applies to recv-busy and epoll-busy, not {}",
                self.backend
            )));
        }
        Ok(())
    }

    /// Parses `backend[,key=value...]`. Keys: `busy_poll_us`, `wait_us`,
    /// `sqpoll_cpu`, `sqpoll_idle_ms`, `ring_entries`, `connect_timeout_ms`,
    /// `connect_cpu`, `rcvbuf` (0 keeps the kernel default), `rx_timestamps`
    /// `prefer_busy_poll` and `per_queue` (`true`/`false`), `busy_poll_budget`
    /// (0 keeps the kernel default).
    ///
    /// # Errors
    /// Unknown backend or key, a malformed value, or a setting the backend
    /// cannot use ([`Config::validate`]).
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
            let flag = || -> Result<bool> {
                v.parse()
                    .map_err(|_| Error::Config(format!("{k}: bad bool {v:?}")))
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
                "rx_timestamps" => cfg.rx_timestamps = flag()?,
                "prefer_busy_poll" => cfg.prefer_busy_poll = flag()?,
                "per_queue" => cfg.per_queue = flag()?,
                "busy_poll_budget" => {
                    let b = u16::try_from(num()?)
                        .map_err(|_| Error::Config(format!("{k}: above 65535: {v:?}")))?;
                    cfg.busy_poll_budget = Some(b).filter(|&b| b > 0);
                }
                _ => return Err(Error::Config(format!("unknown key {k:?}"))),
            }
        }
        cfg.validate()?;
        Ok(cfg)
    }
}

impl FromStr for Config {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}
