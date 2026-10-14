//! `epoll-spin`, `epoll-busy` and `recv-busy`.
//!
//! - `epoll-spin`: `epoll_wait(timeout = 0)` every poll, then one `recvmsg` per
//!   ready socket (level-triggered, so data left behind is reported again).
//! - `epoll-busy`: `epoll_pwait2(wait_us)` with busy poll enabled on the epoll
//!   instance (`EPIOCSPARAMS`, `prefer_busy_poll` off): when nothing is ready the
//!   kernel polls the NIC queue for up to `busy_poll_us`, then sleeps until an
//!   event or the timeout. A zero timeout returns before the busy loop, so this
//!   backend sleeps by design. An epoll instance tracks one NAPI id, so with
//!   sockets on several RX queues only one is polled.
//! - `recv-busy`: no epoll; a non-blocking `recvmsg` on every socket every poll,
//!   with `SO_BUSY_POLL` set (by [`super::Driver::add`]). An empty read runs
//!   `sk_busy_loop(sk, nonblock)`: one NAPI pass, then `EAGAIN`. Never sleeps.
#![allow(unsafe_code)]

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use super::{Conns, recv_one};
use crate::backend::{Backend, Config};
use crate::endpoint::{ConnId, DriverStats, Handler, Receive};
use crate::error::{Error, Result};
use crate::sys::MsgSlot;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Spin,
    Busy,
    Recv,
}

const MAX_EVENTS: usize = 64;

pub(super) struct Epoll {
    mode: Mode,
    ep: Option<OwnedFd>,
    slot: Box<MsgSlot>,
    events: Box<[libc::epoll_event; MAX_EVENTS]>,
    wait: libc::timespec,
}

impl Epoll {
    pub(super) fn new(cfg: &Config) -> Result<Self> {
        let mode = match cfg.backend {
            Backend::EpollBusy => Mode::Busy,
            Backend::RecvBusy => Mode::Recv,
            _ => Mode::Spin,
        };
        let ep = if mode == Mode::Recv {
            None
        } else {
            // SAFETY: plain syscall.
            let fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            // SAFETY: `fd` is a fresh descriptor we own.
            Some(unsafe { OwnedFd::from_raw_fd(fd) })
        };
        if let (Mode::Busy, Some(ep)) = (mode, &ep) {
            set_epoll_busy_poll(ep.as_raw_fd(), cfg.busy_poll_us)?;
        }
        Ok(Self {
            mode,
            ep,
            slot: MsgSlot::new(),
            events: Box::new([libc::epoll_event { events: 0, u64: 0 }; MAX_EVENTS]),
            wait: libc::timespec {
                tv_sec: libc::time_t::from(cfg.wait_us / 1_000_000),
                tv_nsec: libc::c_long::from(cfg.wait_us % 1_000_000) * 1000,
            },
        })
    }

    pub(super) fn add(&self, fd: RawFd, id: ConnId) -> Result<()> {
        let Some(ep) = &self.ep else {
            return Ok(());
        };
        let mut ev = libc::epoll_event {
            events: libc::EPOLLIN.cast_unsigned(),
            u64: id.index() as u64,
        };
        // SAFETY: valid epoll fd, socket fd and event.
        if unsafe { libc::epoll_ctl(ep.as_raw_fd(), libc::EPOLL_CTL_ADD, fd, &raw mut ev) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }

    pub(super) fn remove(&self, fd: RawFd) {
        if let Some(ep) = &self.ep {
            // SAFETY: valid fds; a null event is allowed for DEL.
            unsafe {
                libc::epoll_ctl(
                    ep.as_raw_fd(),
                    libc::EPOLL_CTL_DEL,
                    fd,
                    std::ptr::null_mut(),
                )
            };
        }
    }

    #[inline]
    pub(super) fn poll<E, H>(
        &mut self,
        conns: &mut Conns<E>,
        handler: &mut H,
        stats: &mut DriverStats,
        read_seq: &mut u64,
    ) -> Result<()>
    where
        E: Receive<H>,
        H: Handler,
    {
        if self.mode == Mode::Recv {
            for i in 0..conns.len_slots() {
                self.recv_index(conns, i, handler, stats, read_seq);
            }
        } else {
            let n = self.wait(stats)?;
            for k in 0..n {
                let i = usize::try_from(self.events[k].u64).unwrap_or(usize::MAX);
                self.recv_index(conns, i, handler, stats, read_seq);
            }
        }
        Ok(())
    }

    #[inline]
    fn recv_index<E, H>(
        &mut self,
        conns: &mut Conns<E>,
        i: usize,
        handler: &mut H,
        stats: &mut DriverStats,
        read_seq: &mut u64,
    ) where
        E: Receive<H>,
        H: Handler,
    {
        let id = conns.id(i);
        let Some(conn) = conns.at(i) else {
            return;
        };
        if let Err(e) = recv_one(conn, id, &mut self.slot, handler, stats, read_seq) {
            self.remove(conn.fd());
            conns.take(i);
            stats.closed += 1;
            handler.on_closed(id, e);
        }
    }

    fn wait(&mut self, stats: &mut DriverStats) -> Result<usize> {
        let Some(ep) = &self.ep else {
            return Ok(0);
        };
        stats.epoll_wait_calls += 1;
        #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
        let max = MAX_EVENTS as libc::c_int;
        let n = if self.mode == Mode::Busy {
            // SAFETY: valid fd, event buffer of MAX_EVENTS, timeout pointer, no sigmask.
            unsafe {
                libc::syscall(
                    libc::SYS_epoll_pwait2,
                    ep.as_raw_fd(),
                    self.events.as_mut_ptr(),
                    max,
                    &raw const self.wait,
                    std::ptr::null::<libc::sigset_t>(),
                    0usize,
                )
            }
        } else {
            // SAFETY: `events` has room for MAX_EVENTS entries.
            libc::c_long::from(unsafe {
                libc::epoll_wait(ep.as_raw_fd(), self.events.as_mut_ptr(), max, 0)
            })
        };
        if let Ok(n) = usize::try_from(n) {
            return Ok(n);
        }
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EINTR) {
            Ok(0)
        } else {
            Err(Error::Io(e))
        }
    }
}

/// `struct epoll_params` (Linux 6.9+).
#[repr(C)]
struct EpollParams {
    busy_poll_usecs: u32,
    busy_poll_budget: u16,
    prefer_busy_poll: u8,
    pad: u8,
}

/// `EPIOCSPARAMS = _IOW(0x8A, 0x01, struct epoll_params)`.
const EPIOCSPARAMS: libc::c_ulong = (1 << 30) | (8 << 16) | (0x8A << 8) | 0x01;

fn set_epoll_busy_poll(ep: RawFd, usec: u32) -> Result<()> {
    let p = EpollParams {
        busy_poll_usecs: usec,
        busy_poll_budget: 8,
        prefer_busy_poll: 0,
        pad: 0,
    };
    // SAFETY: `p` is a valid epoll_params for the duration of the call.
    if unsafe { libc::ioctl(ep, EPIOCSPARAMS as _, &raw const p) } != 0 {
        return Err(Error::Unsupported(
            "EPIOCSPARAMS (epoll busy poll) needs Linux 6.9 or newer",
        ));
    }
    Ok(())
}
