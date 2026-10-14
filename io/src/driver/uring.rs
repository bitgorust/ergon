//! `uring-spin`, `uring-napi` and `uring-sqpoll`.
//!
//! Every connection keeps one single-shot `IORING_OP_RECVMSG` armed, pointed at
//! the free tail of its receive buffer, so the kernel copies straight into the
//! buffer the TLS layer decrypts in place. A provided-buffer ring with multishot
//! receive would save the re-arm but forces a copy to stitch TLS records that
//! straddle ring buffers.
//!
//! - `uring-spin`: `SINGLE_ISSUER | DEFER_TASKRUN | TASKRUN_FLAG`. `poll` reads
//!   the completion ring and the `IORING_SQ_TASKRUN` flag in shared memory and
//!   enters the kernel only when it has submissions or the kernel flagged
//!   deferred work. Idle polls make no syscalls.
//! - `uring-napi`: the same ring plus NAPI busy poll (static NAPI ids). An idle
//!   poll waits in `io_uring_enter(GETEVENTS, 1, wait_us)`: the kernel polls the
//!   NIC, then sleeps until a completion or the timeout. Even with `wait_us = 0`
//!   it schedules on an already-expired timer, so this backend can sleep; with
//!   `wait_us = 0` the NAPI budget is 0 (one pass).
//! - `uring-sqpoll`: an `SQPOLL` kernel thread (optionally pinned) submits,
//!   completes and polls the NIC; `poll` only reads the completion ring.
//!
//! Lifetimes: the kernel holds pointers into each connection's `MsgSlot`, receive
//! buffer and send buffer until the matching completion is reaped. Closing a
//! connection cancels its requests and keeps it alive until both completions
//! arrive; dropping the driver cancels everything and reaps before freeing.
#![allow(unsafe_code)]

use std::os::fd::RawFd;

use io_uring::types::{CancelBuilder, Napi, NapiTracking};
use io_uring::{IoUring, opcode, squeue, types};

use super::Conns;
use crate::backend::{Backend, Config};
use crate::endpoint::sealed::Io;
use crate::endpoint::{DriverStats, Endpoint, Handler, Receive};
use crate::error::{Error, Result};
use crate::sys::{self, MsgSlot};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Spin,
    Napi,
    Sqpoll,
}

const RECV: u64 = 0;
const SEND: u64 = 1;
const CANCEL: u64 = 2;
const MAX_CQES: usize = 64;

struct Slot {
    msg: Box<MsgSlot>,
    send_buf: Vec<u8>,
    send_off: usize,
    recv_armed: bool,
    send_active: bool,
    /// A cancel that did not fit in the submission queue yet.
    cancel_fd: Option<RawFd>,
}

pub(super) struct Uring {
    // Dropped first: closing the ring fd before the buffers below.
    ring: IoUring,
    mode: Mode,
    slots: Vec<Slot>,
    pending: usize,
    wait: types::Timespec,
    napi_ids: Vec<u32>,
    napi_static: bool,
    pending_cancels: bool,
}

impl Uring {
    pub(super) fn new(cfg: &Config) -> Result<Self> {
        let mode = match cfg.backend {
            Backend::UringNapi => Mode::Napi,
            Backend::UringSqpoll => Mode::Sqpoll,
            _ => Mode::Spin,
        };
        let mut b = IoUring::builder();
        if mode == Mode::Sqpoll {
            // NAPI passes do not count as SQPOLL work, so keep the idle window
            // long or the thread sleeps through quiet periods.
            b.setup_sqpoll(cfg.sqpoll_idle_ms);
            // The SQPOLL thread inherits its creator's affinity: with a pinned
            // polling thread it would spin on the poller's own core and steal
            // whole time slices. Without an explicit CPU, put it on a
            // housekeeping CPU instead.
            let cpu = cfg.sqpoll_cpu.or_else(|| {
                sys::housekeeping_cpus(sys::current_cpu())
                    .first()
                    .and_then(|&c| u32::try_from(c).ok())
            });
            if let Some(cpu) = cpu {
                b.setup_sqpoll_cpu(cpu);
            }
            b.setup_single_issuer();
        } else {
            b.setup_single_issuer()
                .setup_defer_taskrun()
                .setup_taskrun_flag();
        }
        let ring = b.build(cfg.ring_entries).map_err(|e| {
            if e.raw_os_error() == Some(libc::ENOSYS) || e.raw_os_error() == Some(libc::EPERM) {
                Error::Unsupported("io_uring is disabled or unavailable on this kernel")
            } else {
                Error::Io(e)
            }
        })?;
        let mut napi_static = false;
        if mode != Mode::Spin {
            // With a zero wait the GETEVENTS busy loop is capped only by the NAPI
            // budget, not by the expired timeout: budget 0 makes it one pass.
            let budget = if mode == Mode::Napi && cfg.wait_us == 0 {
                0
            } else {
                cfg.busy_poll_us.max(1)
            };
            // Static tracking (6.13+) polls exactly the queues of our sockets;
            // older kernels (6.9+) track the queues of sockets with requests.
            let register = |tracking| {
                let mut napi = Napi::new()
                    .set_busy_poll_timeout(budget)
                    .set_prefer_busy_poll(false)
                    .set_tracking(tracking);
                ring.submitter().register_napi(&mut napi)
            };
            napi_static = register(NapiTracking::Static).is_ok();
            if !napi_static {
                register(NapiTracking::Dynamic).map_err(|_| {
                    Error::Unsupported("IORING_REGISTER_NAPI needs Linux 6.9 or newer")
                })?;
            }
        }
        Ok(Self {
            ring,
            mode,
            slots: Vec::new(),
            pending: 0,
            wait: types::Timespec::new()
                .sec(u64::from(cfg.wait_us / 1_000_000))
                .nsec((cfg.wait_us % 1_000_000) * 1000),
            napi_ids: Vec::new(),
            napi_static,
            pending_cancels: false,
        })
    }

    pub(super) fn add<E: Endpoint>(&mut self, ep: &mut E, i: usize) -> Result<()> {
        if self.napi_static {
            let id = sys::incoming_napi_id(ep.fd()).unwrap_or(0);
            if id != 0 && !self.napi_ids.contains(&id) {
                self.ring.submitter().register_napi_add_id(id)?;
                self.napi_ids.push(id);
            }
        }
        while self.slots.len() <= i {
            self.slots.push(Slot {
                msg: MsgSlot::new(),
                send_buf: Vec::with_capacity(1 << 16),
                send_off: 0,
                recv_armed: false,
                send_active: false,
                cancel_fd: None,
            });
        }
        // Input left from the handshake is decoded first (the driver's next
        // poll), which may move the buffer: arm after that (`arm_initial`).
        if ep.has_buffered_input() {
            return Ok(());
        }
        self.arm_recv(ep, i)
    }

    /// Arms the first receive of a connection added with buffered input.
    pub(super) fn arm_initial<E: Io>(&mut self, ep: &mut E, i: usize) -> Result<()> {
        if self.slots.get(i).is_some_and(|s| !s.recv_armed) {
            self.arm_recv(ep, i)?;
        }
        Ok(())
    }

    /// Starts closing slot `i`: cancels its requests, or drops it at once when
    /// nothing is in flight.
    pub(super) fn close<E: Endpoint>(&mut self, conns: &mut Conns<E>, i: usize) {
        let Some(fd) = conns.at(i).map(|c| c.fd()) else {
            return;
        };
        conns.begin_close(i);
        self.cancel_fd(fd, i);
        self.maybe_finish(conns, i);
    }

    fn cancel_fd(&mut self, fd: RawFd, i: usize) {
        let Some(s) = self.slots.get(i) else {
            return;
        };
        if !s.recv_armed && !s.send_active {
            return;
        }
        let e = opcode::AsyncCancel2::new(CancelBuilder::fd(types::Fd(fd)).all())
            .build()
            .user_data(tag(i, CANCEL));
        // A full queue: retried on every poll until it fits.
        if self.push(&e).is_err() {
            self.slots[i].cancel_fd = Some(fd);
            self.pending_cancels = true;
        }
    }

    #[cold]
    fn retry_cancels(&mut self) {
        for i in 0..self.slots.len() {
            if let Some(fd) = self.slots[i].cancel_fd.take() {
                self.cancel_fd(fd, i);
            }
        }
    }

    fn maybe_finish<E>(&mut self, conns: &mut Conns<E>, i: usize) {
        if conns.is_closing(i)
            && self
                .slots
                .get(i)
                .is_none_or(|s| !s.recv_armed && !s.send_active)
        {
            if let Some(s) = self.slots.get_mut(i) {
                s.send_buf.clear();
                s.send_off = 0;
            }
            conns.finish_close(i);
        }
    }

    fn fail<E: Endpoint, H: Handler>(
        &mut self,
        conns: &mut Conns<E>,
        i: usize,
        err: Error,
        handler: &mut H,
        stats: &mut DriverStats,
    ) {
        let id = conns.id(i);
        if !conns.is_open(i) {
            return;
        }
        self.close(conns, i);
        stats.closed += 1;
        handler.on_closed(id, err);
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
        if self.pending_cancels {
            self.pending_cancels = false;
            self.retry_cancels();
            self.pending_cancels = self.slots.iter().any(|s| s.cancel_fd.is_some());
        }
        if self.ring.completion().is_empty() {
            // Idle fast path: nothing to reap.
            self.push_sends(conns, handler, stats);
            return self.enter(true, stats);
        }
        let mut cqes = [(0u64, 0i32); MAX_CQES];
        let mut n = 0;
        for cqe in self.ring.completion() {
            cqes[n] = (cqe.user_data(), cqe.result());
            n += 1;
            if n == MAX_CQES {
                break;
            }
        }
        for &(ud, res) in &cqes[..n] {
            let i = usize::try_from(ud >> 8).unwrap_or(usize::MAX);
            match ud & 0xFF {
                RECV => self.on_recv(conns, i, res, handler, stats, read_seq),
                SEND => self.on_send(conns, i, res, handler, stats),
                _ => {}
            }
        }
        self.push_sends(conns, handler, stats);
        self.enter(n == 0, stats)
    }

    #[inline]
    fn on_recv<E, H>(
        &mut self,
        conns: &mut Conns<E>,
        i: usize,
        res: i32,
        handler: &mut H,
        stats: &mut DriverStats,
        read_seq: &mut u64,
    ) where
        E: Receive<H>,
        H: Handler,
    {
        let Some(slot) = self.slots.get_mut(i) else {
            return;
        };
        slot.recv_armed = false;
        if !conns.is_open(i) {
            self.maybe_finish(conns, i);
            return;
        }
        let id = conns.id(i);
        let Some(conn) = conns.at(i) else {
            return;
        };
        let outcome = match usize::try_from(res) {
            Ok(0) => Err(Error::Closed),
            Ok(len) => {
                *read_seq += 1;
                stats.completions += 1;
                let meta = slot.msg.meta(*read_seq);
                conn.on_received(id, len, &meta, handler)
                    .map(|m| stats.messages += m as u64)
            }
            Err(_) if res == -libc::EAGAIN || res == -libc::EINTR => Ok(()),
            Err(_) => Err(Error::Io(std::io::Error::from_raw_os_error(-res))),
        };
        let outcome = outcome.and_then(|()| self.arm_recv(conn, i));
        if let Err(e) = outcome {
            self.fail(conns, i, e, handler, stats);
        }
    }

    fn on_send<E: Endpoint, H: Handler>(
        &mut self,
        conns: &mut Conns<E>,
        i: usize,
        res: i32,
        handler: &mut H,
        stats: &mut DriverStats,
    ) {
        let Some(slot) = self.slots.get_mut(i) else {
            return;
        };
        slot.send_active = false;
        if !conns.is_open(i) {
            self.maybe_finish(conns, i);
            return;
        }
        let Ok(sent) = usize::try_from(res) else {
            let e = Error::Io(std::io::Error::from_raw_os_error(-res));
            self.fail(conns, i, e, handler, stats);
            return;
        };
        slot.send_off += sent;
        if slot.send_off < slot.send_buf.len() {
            if let Err(e) = self.push_send(conns, i) {
                self.fail(conns, i, e, handler, stats);
            }
        } else {
            slot.send_buf.clear();
            slot.send_off = 0;
        }
    }

    /// Moves each idle connection's queued bytes into its in-flight send buffer
    /// (a swap, no copy) and queues the send.
    #[inline]
    fn push_sends<E: Endpoint, H: Handler>(
        &mut self,
        conns: &mut Conns<E>,
        handler: &mut H,
        stats: &mut DriverStats,
    ) {
        for i in 0..conns.len_slots().min(self.slots.len()) {
            if self.slots[i].send_active || !conns.is_open(i) {
                continue;
            }
            let Some(c) = conns.at(i) else {
                continue;
            };
            let q = c.tx_queue();
            if q.is_empty() {
                continue;
            }
            let s = &mut self.slots[i];
            std::mem::swap(&mut s.send_buf, q);
            s.send_off = 0;
            stats.send_calls += 1;
            if let Err(e) = self.push_send(conns, i) {
                self.fail(conns, i, e, handler, stats);
            }
        }
    }

    pub(super) fn flush<E: Endpoint, H: Handler>(
        &mut self,
        conns: &mut Conns<E>,
        handler: &mut H,
        stats: &mut DriverStats,
    ) -> Result<()> {
        self.push_sends(conns, handler, stats);
        self.enter(false, stats)
    }

    #[inline]
    fn enter(&mut self, idle: bool, stats: &mut DriverStats) -> Result<()> {
        match self.mode {
            Mode::Sqpoll => {
                if self.pending > 0 {
                    // A syscall happens only when the SQPOLL thread went to sleep.
                    if self.ring.submission().need_wakeup() {
                        stats.enter_calls += 1;
                    }
                    self.ring.submit()?;
                    self.pending = 0;
                }
            }
            Mode::Napi if idle => {
                stats.enter_calls += 1;
                let args = types::SubmitArgs::new().timespec(&self.wait);
                match self.ring.submitter().submit_with_args(1, &args) {
                    Ok(_) => {}
                    Err(e)
                        if matches!(
                            e.raw_os_error(),
                            Some(libc::ETIME | libc::EINTR | libc::EBUSY)
                        ) => {}
                    Err(e) => return Err(e.into()),
                }
                self.pending = 0;
            }
            _ => {
                if self.pending > 0 || self.ring.submission().taskrun() {
                    stats.enter_calls += 1;
                    self.ring.submitter().submit_and_wait(0)?;
                    self.pending = 0;
                }
            }
        }
        Ok(())
    }

    fn arm_recv<E: Endpoint>(&mut self, conn: &mut E, i: usize) -> Result<()> {
        let s = &mut self.slots[i];
        s.msg.arm(conn.rx_space());
        let e = opcode::RecvMsg::new(types::Fd(conn.fd()), &raw mut s.msg.hdr)
            .build()
            .user_data(tag(i, RECV));
        s.recv_armed = true;
        self.push(&e)
            .inspect_err(|_| self.slots[i].recv_armed = false)
    }

    fn push_send<E: Endpoint>(&mut self, conns: &mut Conns<E>, i: usize) -> Result<()> {
        let Some(fd) = conns.at(i).map(|c| c.fd()) else {
            return Ok(());
        };
        let s = &mut self.slots[i];
        let rest = &s.send_buf[s.send_off..];
        let len = u32::try_from(rest.len())
            .map_err(|_| Error::Config("send larger than 4 GiB".into()))?;
        let e = opcode::Send::new(types::Fd(fd), rest.as_ptr(), len)
            .flags(libc::MSG_NOSIGNAL)
            .build()
            .user_data(tag(i, SEND));
        s.send_active = true;
        self.push(&e)
            .inspect_err(|_| self.slots[i].send_active = false)
    }

    fn push(&mut self, e: &squeue::Entry) -> Result<()> {
        for _ in 0..2 {
            // SAFETY: every pointer in `e` (msghdr slot, receive buffer tail, send
            // buffer) stays valid and untouched until its completion is reaped:
            // slots are boxed and owned here, receive buffers belong to endpoints
            // that are only dropped after their completions (`maybe_finish`,
            // `shutdown`), and send buffers are only cleared after completion.
            if unsafe { self.ring.submission().push(e) }.is_ok() {
                self.pending += 1;
                return Ok(());
            }
            self.ring.submit()?;
            self.pending = 0;
        }
        Err(Error::Config("io_uring submission queue full".into()))
    }

    /// Cancels every request and reaps until the kernel holds no buffer, for at
    /// most one second. Returns whether it got there; if not, the caller must
    /// leak the endpoints.
    pub(super) fn shutdown(&mut self) -> bool {
        let inflight = |s: &[Slot]| s.iter().any(|s| s.recv_armed || s.send_active);
        if !inflight(&self.slots) {
            return true;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        // Every queued SQE must be issued before a cancel can see it (with
        // SQPOLL the kernel thread picks them up asynchronously).
        while !self.ring.submission().is_empty() && std::time::Instant::now() < deadline {
            let _ = self.ring.submit();
            std::hint::spin_loop();
        }
        while std::time::Instant::now() < deadline {
            let _ = self
                .ring
                .submitter()
                .register_sync_cancel(None, CancelBuilder::any());
            let _ = self.ring.submitter().submit_and_wait(0);
            for cqe in self.ring.completion() {
                let i = usize::try_from(cqe.user_data() >> 8).unwrap_or(usize::MAX);
                if let Some(s) = self.slots.get_mut(i) {
                    match cqe.user_data() & 0xFF {
                        RECV => s.recv_armed = false,
                        SEND => s.send_active = false,
                        _ => {}
                    }
                }
            }
            if !inflight(&self.slots) {
                return true;
            }
            std::hint::spin_loop();
        }
        // The kernel may still write into these: never free them.
        std::mem::forget(std::mem::take(&mut self.slots));
        false
    }
}

const fn tag(i: usize, op: u64) -> u64 {
    ((i as u64) << 8) | op
}
