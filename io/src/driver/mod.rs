//! The driver: owns a set of connections and moves bytes between them and the
//! kernel with the configured [`Backend`]. Everything above the socket (TLS,
//! framing, the application) is the same whichever backend is chosen.
//!
//! [`Driver::poll`] does one non-blocking iteration and returns, so the caller's
//! own loop stays in control:
//!
//! ```rust,no_run
//! # use ergon_io::{Config, Driver, ConnId, Error, Handler, WsConn, WsHandler, RxMeta};
//! # struct App;
//! # impl Handler for App {}
//! # impl WsHandler for App {
//! #     fn on_message(&mut self, _: ConnId, _: u8, _: &[u8], _: &RxMeta, _: u16) {}
//! # }
//! # fn conn() -> WsConn { unimplemented!() }
//! # fn main() -> ergon_io::Result<()> {
//! let mut driver: Driver<WsConn> = Driver::new(Config::parse("recv-busy")?)?;
//! let id = driver.add(conn())?;
//! let mut app = App;
//! loop {
//!     driver.poll(&mut app)?; // receive, decode, deliver, flush
//!     // ... the rest of the duty cycle; sends queued here go out on the next
//!     // poll, or call driver.flush() to send now.
//! #   break;
//! }
//! # Ok(()) }
//! ```
//!
//! A connection that fails (protocol error, reset, peer close) is closed and
//! reported through [`Handler::on_closed`]; the others keep running. `poll`
//! itself returns an error only when the backend fails.

use std::net::{IpAddr, SocketAddr};

use crate::backend::{Backend, Config};
use crate::clock::{monotonic_ns, rdtsc, ticks_per_us};
use crate::dns::{DnsConfig, Lookup, Resolver};
use crate::endpoint::{ConnId, ConnMut, Dial, DriverStats, Endpoint, Handler, Receive};
use crate::error::{Error, Result};
use crate::{RxMeta, sys};

mod background;
mod epoll;
#[cfg(feature = "uring")]
mod uring;

/// The connection slots: endpoints plus a generation per slot.
struct Conns<E> {
    slots: Vec<Option<E>>,
    gens: Vec<u32>,
    /// Closed but still referenced by in-flight `io_uring` operations: hidden from
    /// the API, not reusable, dropped once the kernel lets go.
    closing: Vec<bool>,
    /// Reserved for a connection still being set up (no endpoint yet).
    reserved: Vec<bool>,
    /// Closed endpoints the kernel no longer references, waiting to be freed
    /// (on the helper thread when there is one: `Driver::free_closed`).
    closed: Vec<E>,
}

impl<E> Conns<E> {
    const fn new() -> Self {
        Self {
            slots: Vec::new(),
            gens: Vec::new(),
            closing: Vec::new(),
            reserved: Vec::new(),
            closed: Vec::new(),
        }
    }

    const fn len_slots(&self) -> usize {
        self.slots.len()
    }

    /// The current id of slot `i` (valid whether or not it is occupied).
    fn id(&self, i: usize) -> ConnId {
        ConnId::new(
            u32::try_from(i).unwrap_or(u32::MAX),
            self.gens.get(i).copied().unwrap_or(0),
        )
    }

    fn at(&mut self, i: usize) -> Option<&mut E> {
        self.slots.get_mut(i).and_then(Option::as_mut)
    }

    fn lookup(&self, id: ConnId) -> Option<usize> {
        let i = id.index();
        (self.gens.get(i) == Some(&id.generation())
            && self.closing.get(i) == Some(&false)
            && self.slots.get(i).is_some_and(Option::is_some))
        .then_some(i)
    }

    fn is_open(&self, i: usize) -> bool {
        self.closing.get(i) == Some(&false) && self.slots.get(i).is_some_and(Option::is_some)
    }

    /// Reserves a free slot.
    fn reserve(&mut self) -> usize {
        let i = (0..self.slots.len())
            .find(|&i| self.slots[i].is_none() && !self.closing[i] && !self.reserved[i])
            .unwrap_or_else(|| {
                self.slots.push(None);
                // Generation 1 first, so `ConnId::default()` never matches.
                self.gens.push(1);
                self.closing.push(false);
                self.reserved.push(false);
                self.slots.len() - 1
            });
        self.reserved[i] = true;
        i
    }

    /// Fills a reserved slot.
    fn install(&mut self, i: usize, e: E) {
        self.slots[i] = Some(e);
        self.reserved[i] = false;
    }

    /// Frees a reserved slot without filling it, invalidating its id.
    fn release(&mut self, i: usize) {
        if self.reserved.get(i) == Some(&true) {
            self.reserved[i] = false;
            self.gens[i] = self.gens[i].wrapping_add(1);
        }
    }

    /// Slot of a connection still being set up.
    fn lookup_reserved(&self, id: ConnId) -> Option<usize> {
        let i = id.index();
        (self.gens.get(i) == Some(&id.generation()) && self.reserved.get(i) == Some(&true))
            .then_some(i)
    }

    /// Invalidates slot `i`'s id but keeps the endpoint alive until
    /// [`Conns::finish_close`].
    #[cfg_attr(not(feature = "uring"), allow(dead_code))]
    fn begin_close(&mut self, i: usize) {
        if self.is_open(i) {
            self.gens[i] = self.gens[i].wrapping_add(1);
            self.closing[i] = true;
        }
    }

    fn is_closing(&self, i: usize) -> bool {
        self.closing.get(i) == Some(&true)
    }

    /// Retires a closing endpoint (freed later, see `closed`).
    #[cfg_attr(not(feature = "uring"), allow(dead_code))]
    fn finish_close(&mut self, i: usize) {
        if self.is_closing(i) {
            if let Some(e) = self.slots[i].take() {
                self.closed.push(e);
            }
            self.closing[i] = false;
        }
    }

    /// Never frees the endpoints again (last resort when the kernel may still
    /// write into their buffers).
    #[cfg_attr(not(feature = "uring"), allow(dead_code))]
    fn leak(&mut self) {
        std::mem::forget(std::mem::take(&mut self.slots));
    }

    /// Removes slot `i` at once, invalidating its id; the endpoint is freed
    /// later (see `closed`).
    fn remove(&mut self, i: usize) {
        if self.is_closing(i) {
            return;
        }
        if let Some(e) = self.slots.get_mut(i).and_then(Option::take) {
            self.gens[i] = self.gens[i].wrapping_add(1);
            self.closed.push(e);
        }
    }
}

// One per driver, so the size does not matter; boxing would add a pointer
// chase to every poll.
#[allow(clippy::large_enum_variant)]
enum Imp {
    Epoll(epoll::Epoll),
    #[cfg(feature = "uring")]
    Uring(uring::Uring),
}

/// How often request deadlines are checked, in microseconds (one counter read
/// per `poll`, only for endpoints with deadlines).
const EXPIRY_CHECK_US: u32 = 250;

/// How often connections being set up are advanced, in microseconds.
const SETUP_STEP_US: u32 = 20;

/// Per-socket options every backend needs.
#[derive(Clone, Copy)]
struct SockOpts {
    timestamps: bool,
    rcvbuf: Option<u32>,
    busy_poll_us: u32,
    /// `recv-busy` only: preferred busy polling and the packet budget (epoll
    /// and `io_uring` take theirs on the instance).
    prefer_busy_poll: bool,
    busy_poll_budget: Option<u16>,
}

impl SockOpts {
    fn of(cfg: &Config) -> Self {
        Self {
            timestamps: cfg.rx_timestamps,
            rcvbuf: cfg.rcvbuf,
            // Only the backends that poll through the socket set SO_BUSY_POLL;
            // the others set 0 so a sysctl default cannot leak in.
            busy_poll_us: match cfg.backend {
                Backend::EpollBusy | Backend::RecvBusy => cfg.busy_poll_us.max(1),
                _ => 0,
            },
            prefer_busy_poll: cfg.backend == Backend::RecvBusy && cfg.prefer_busy_poll,
            busy_poll_budget: cfg
                .busy_poll_budget
                .filter(|_| cfg.backend == Backend::RecvBusy),
        }
    }

    fn apply(self, fd: std::os::fd::RawFd) -> Result<()> {
        if self.timestamps {
            sys::enable_rx_timestamps(fd)?;
        }
        if let Some(b) = self.rcvbuf {
            sys::set_rcvbuf(fd, b)?;
        }
        sys::set_busy_poll(fd, self.busy_poll_us)?;
        let privileged = |what: &str, e: std::io::Error| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                Error::Config(format!("{what} needs CAP_NET_ADMIN"))
            } else {
                Error::Io(e)
            }
        };
        if self.prefer_busy_poll {
            sys::set_prefer_busy_poll(fd, true)
                .map_err(|e| privileged("prefer_busy_poll (SO_PREFER_BUSY_POLL)", e))?;
        }
        if let Some(b) = self.busy_poll_budget {
            sys::set_busy_poll_budget(fd, b)
                .map_err(|e| privileged("busy_poll_budget (SO_BUSY_POLL_BUDGET)", e))?;
        }
        Ok(())
    }
}

/// A connection being set up.
struct Pending<E> {
    slot: usize,
    dial: Box<dyn Dial<Conn = E>>,
    started: bool,
    deadline_ns: i64,
}

/// Owns connections of type `E` and drives them with the configured backend.
///
/// Dropping the driver closes every connection; with `io_uring` it first cancels
/// in-flight operations and waits until the kernel has released every buffer.
pub struct Driver<E> {
    cfg: Config,
    imp: Imp,
    conns: Conns<E>,
    stats: DriverStats,
    read_seq: u64,
    /// An added endpoint holds input that arrived during its handshake; the next
    /// `poll` delivers it before reading the socket.
    buffered: bool,
    pending: Vec<Pending<E>>,
    /// Connections `flush` closed (it has no handler): reported by the next
    /// `poll`.
    unreported: Vec<(ConnId, Error)>,
    resolver: Resolver,
    ticks_per_us: f64,
    /// Counter value at which deadlines are next checked (`E::DEADLINES`).
    next_expiry: u64,
    expiry_ticks: u64,
    /// Counter value at which connection set-up is next advanced.
    next_setup: u64,
    setup_ticks: u64,
    /// The background connect thread, started by the first `connect_background`.
    /// It also frees closed connections.
    background: Option<background::Background<E>>,
    /// Resolver settings for the background thread's own resolver.
    dns: DnsConfig,
    /// A socket with receive timestamps enabled for the driver's lifetime. The
    /// first timestamping socket in a process enables a kernel static key (code
    /// patched on every CPU) and the last one to close disables it again; this
    /// one keeps it enabled, so connections opened later never flip it.
    _timestamp_keeper: Option<socket2::Socket>,
}

impl<E: Endpoint> Driver<E> {
    /// A driver with no connections.
    ///
    /// # Errors
    /// The backend is unavailable (`io_uring` without the `uring` feature, or a
    /// kernel without the needed features) or fails to initialise.
    pub fn new(cfg: Config) -> Result<Self> {
        Self::with_dns(cfg, DnsConfig::system())
    }

    /// Like [`Driver::new`] with explicit resolver settings (`Driver::new` reads
    /// `/etc/resolv.conf` and `/etc/hosts`).
    ///
    /// # Errors
    /// As [`Driver::new`].
    pub fn with_dns(cfg: Config, dns: DnsConfig) -> Result<Self> {
        cfg.validate()?;
        let tpu = ticks_per_us();
        let imp = match cfg.backend {
            Backend::EpollSpin | Backend::EpollBusy | Backend::RecvBusy => {
                Imp::Epoll(epoll::Epoll::new(&cfg)?)
            }
            #[cfg(feature = "uring")]
            Backend::UringSpin | Backend::UringNapi | Backend::UringSqpoll => {
                Imp::Uring(uring::Uring::new(&cfg)?)
            }
            #[cfg(not(feature = "uring"))]
            Backend::UringSpin | Backend::UringNapi | Backend::UringSqpoll => {
                return Err(Error::Unsupported(
                    "io_uring backends need the `uring` feature",
                ));
            }
        };
        Ok(Self {
            imp,
            conns: Conns::new(),
            stats: DriverStats::default(),
            read_seq: 0,
            buffered: false,
            pending: Vec::new(),
            unreported: Vec::new(),
            resolver: Resolver::new(dns.clone()),
            dns,
            _timestamp_keeper: cfg.rx_timestamps.then(timestamp_keeper).flatten(),
            // Calibrated once here, not on the first connect.
            ticks_per_us: tpu,
            next_expiry: 0,
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            expiry_ticks: (f64::from(EXPIRY_CHECK_US) * tpu) as u64,
            next_setup: 0,
            background: None,
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            setup_ticks: (f64::from(SETUP_STEP_US) * tpu) as u64,
            cfg,
        })
    }

    /// The configuration.
    #[must_use]
    pub const fn config(&self) -> &Config {
        &self.cfg
    }

    /// Counters.
    #[must_use]
    pub const fn stats(&self) -> &DriverStats {
        &self.stats
    }

    /// Takes over a connected, non-blocking endpoint. Sets its socket options
    /// (receive timestamps, `SO_RCVBUF`, `SO_BUSY_POLL`) and starts receiving.
    ///
    /// # Errors
    /// A socket option or the backend registration fails; the endpoint is
    /// dropped (its socket closed).
    pub fn add(&mut self, ep: E) -> Result<ConnId> {
        let i = self.conns.reserve();
        let id = self.conns.id(i);
        if let Err(e) = self.install(i, ep) {
            self.conns.release(i);
            return Err(e);
        }
        Ok(id)
    }

    /// Starts connecting without blocking and returns the connection's id at
    /// once. Each later `poll` advances it (DNS, TCP, TLS, protocol upgrade);
    /// [`Handler::on_open`] reports success, [`Handler::on_closed`] failure or
    /// the `connect_timeout_ms` deadline. Sends queued before it opens are
    /// not possible (`get_mut` returns `None` until then).
    ///
    /// # Errors
    /// The connect could not even be started (socket creation failed).
    pub fn connect<D: Dial<Conn = E> + 'static>(&mut self, dial: D) -> Result<ConnId> {
        let i = self.conns.reserve();
        let id = self.conns.id(i);
        let mut p = Pending {
            slot: i,
            dial: Box::new(dial),
            started: false,
            deadline_ns: monotonic_ns() + i64::from(self.cfg.connect_timeout_ms) * 1_000_000,
        };
        if let Some(addr) = p.dial.addr() {
            if let Err(e) = p.dial.start(addr) {
                self.conns.release(i);
                return Err(e);
            }
            p.started = true;
        }
        self.pending.push(p);
        Ok(id)
    }

    /// Like [`Driver::connect`], but DNS (the helper's own non-blocking
    /// resolver, built from the driver's `DnsConfig`), TCP, TLS and any
    /// upgrade run on a helper thread the driver starts on first use, so the
    /// polling thread spends no handshake CPU. `poll` only takes the finished
    /// connection; [`Handler::on_open`] / [`Handler::on_closed`] report it as
    /// for `connect`. Closing the id first, or dropping the driver, discards
    /// the result and closes its socket.
    ///
    /// # Errors
    /// The helper thread could not be started.
    pub fn connect_background<D>(&mut self, dial: D) -> Result<ConnId>
    where
        D: Dial<Conn = E> + Send + 'static,
        E: Send + 'static,
    {
        if self.background.is_none() {
            self.background = Some(background::Background::start(
                self.cfg.connect_cpu,
                self.dns.clone(),
            )?);
        }
        let i = self.conns.reserve();
        let id = self.conns.id(i);
        let timeout = std::time::Duration::from_millis(u64::from(self.cfg.connect_timeout_ms));
        let opts = SockOpts::of(&self.cfg);
        if let Some(bg) = self.background.as_mut()
            && let Err(e) = bg.submit(
                i,
                id.generation(),
                Box::new(dial),
                Box::new(move |c: &E| opts.apply(c.fd())),
                timeout,
            )
        {
            self.conns.release(i);
            return Err(e);
        }
        Ok(id)
    }

    /// Frees the connections closed during the current call: on the helper
    /// thread when there is one (their sockets close moments later, there),
    /// otherwise here before the call returns. The helper takes the `munmap`
    /// calls and the wait for their TLB shootdowns; the shootdown IPI itself
    /// still reaches the polling CPU, which runs the same address space.
    #[cold]
    fn free_closed(&mut self) {
        let Self {
            conns, background, ..
        } = self;
        for c in conns.closed.drain(..) {
            match background {
                Some(b) => b.free(c),
                None => drop(c),
            }
        }
    }

    /// Installs connections finished on the helper thread.
    #[cold]
    fn take_background<H: Handler>(&mut self, handler: &mut H) {
        while let Some(done) = self
            .background
            .as_mut()
            .and_then(background::Background::try_take)
        {
            let id = ConnId::new(
                u32::try_from(done.slot).unwrap_or(u32::MAX),
                done.generation,
            );
            // Closed while connecting: the slot was released; free the result
            // with the other closed connections.
            if self.conns.lookup_reserved(id).is_none() {
                if let Ok(ep) = done.result {
                    self.conns.closed.push(ep);
                }
                continue;
            }
            match done
                .result
                .and_then(|ep| self.install_prepared(done.slot, ep))
            {
                Ok(()) => {
                    self.stats.connects += 1;
                    handler.on_open(id);
                }
                Err(e) => {
                    self.conns.release(done.slot);
                    self.stats.connect_failures += 1;
                    handler.on_closed(id, e);
                }
            }
        }
    }

    /// Starts resolving `host` now so a later [`Driver::connect`] finds it
    /// cached. Answers are kept for their TTL.
    ///
    /// # Errors
    /// The name is invalid or no name server is configured.
    pub fn prefetch(&mut self, host: &str) -> Result<()> {
        self.resolver.prefetch(host, monotonic_ns())
    }

    /// Whether connections are still being set up.
    #[must_use]
    pub const fn connecting(&self) -> usize {
        self.pending.len()
    }

    /// Socket options, backend registration and the slot.
    fn install(&mut self, i: usize, ep: E) -> Result<()> {
        if let Err(e) = SockOpts::of(&self.cfg).apply(ep.fd()) {
            self.conns.closed.push(ep);
            return Err(e);
        }
        self.install_prepared(i, ep)
    }

    /// Backend registration and the slot, for an endpoint whose socket options
    /// are already set.
    #[cfg_attr(not(feature = "uring"), allow(unused_mut))]
    fn install_prepared(&mut self, i: usize, mut ep: E) -> Result<()> {
        let fd = ep.fd();
        let id = self.conns.id(i);
        let added = match &mut self.imp {
            Imp::Epoll(e) => e.add(fd, id),
            #[cfg(feature = "uring")]
            Imp::Uring(u) => u.add(&mut ep, i),
        };
        if let Err(e) = added {
            // Nothing was armed: freed with the other closed connections.
            self.conns.closed.push(ep);
            return Err(e);
        }
        self.buffered |= ep.has_buffered_input();
        self.conns.install(i, ep);
        Ok(())
    }

    /// Advances connections being set up. Each dial step's CPU time is tracked
    /// in [`DriverStats::connect_step_max_ns`].
    #[cold]
    fn advance_pending<H: Handler>(&mut self, handler: &mut H) {
        let now = monotonic_ns();
        self.resolver.poll(now);
        let mut k = 0;
        while k < self.pending.len() {
            let p = &mut self.pending[k];
            let outcome = advance_one(
                p,
                &mut self.resolver,
                &mut self.stats,
                now,
                self.ticks_per_us,
            );
            let ep = match outcome {
                Ok(None) => {
                    k += 1;
                    continue;
                }
                Ok(Some(ep)) => Ok(ep),
                Err(e) => Err(e),
            };
            let p = self.pending.swap_remove(k);
            let id = self.conns.id(p.slot);
            let installed = ep.and_then(|ep| self.install(p.slot, ep));
            match installed {
                Ok(()) => {
                    self.stats.connects += 1;
                    handler.on_open(id);
                }
                Err(e) => {
                    self.conns.release(p.slot);
                    self.stats.connect_failures += 1;
                    handler.on_closed(id, e);
                }
            }
        }
    }

    /// The connection, if `id` is still open.
    #[must_use]
    pub fn get(&self, id: ConnId) -> Option<&E> {
        self.conns
            .lookup(id)
            .and_then(|i| self.conns.slots[i].as_ref())
    }

    /// The connection, if `id` is still open, for queuing sends. The handle
    /// offers the connection's methods but cannot replace or move it.
    pub fn get_mut(&mut self, id: ConnId) -> Option<ConnMut<'_, E>> {
        self.conns
            .lookup(id)
            .and_then(|i| self.conns.at(i))
            .map(ConnMut::new)
    }

    /// Open connections.
    pub fn iter(&self) -> impl Iterator<Item = (ConnId, &E)> {
        self.conns
            .slots
            .iter()
            .enumerate()
            .filter(|&(i, _)| !self.conns.is_closing(i))
            .filter_map(|(i, c)| c.as_ref().map(|c| (self.conns.id(i), c)))
    }

    /// Number of open connections.
    #[must_use]
    pub fn len(&self) -> usize {
        (0..self.conns.len_slots())
            .filter(|&i| self.conns.is_open(i))
            .count()
    }

    /// Whether there are no open connections.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Closes a connection; stale or unknown ids are ignored. With `io_uring` the
    /// endpoint is dropped once the kernel has released its buffers (on a later
    /// `poll`, or when the driver is dropped).
    pub fn close(&mut self, id: ConnId) {
        if let Some(i) = self.conns.lookup(id) {
            close_index(&mut self.imp, &mut self.conns, i);
            self.free_closed();
        } else if let Some(i) = self.conns.lookup_reserved(id) {
            // Still connecting: dropping the dial closes its socket (a
            // background result for the released slot is dropped on arrival).
            self.pending.retain(|p| p.slot != i);
            self.conns.release(i);
        }
    }

    /// Whether every connection's NIC queue can be polled: the backend polls
    /// the NIC and every socket has a NAPI id (known once it has received a
    /// packet).
    #[must_use]
    pub fn nic_polling_active(&self) -> bool {
        self.cfg.backend.polls_nic()
            && self
                .iter()
                .all(|(_, c)| sys::incoming_napi_id(c.fd()).unwrap_or(0) != 0)
    }

    /// One iteration: receive whatever the kernel has, decode and deliver it,
    /// then send queued bytes. Returns messages delivered. Never sleeps unless
    /// the backend is [`Backend::EpollBusy`] or [`Backend::UringNapi`].
    ///
    /// # Errors
    /// The backend itself failed. Connection errors close that connection and
    /// go to [`Handler::on_closed`].
    #[inline]
    pub fn poll<H: Handler>(&mut self, handler: &mut H) -> Result<usize>
    where
        E: Receive<H>,
    {
        self.stats.polls += 1;
        let before = self.stats.messages;
        if !self.unreported.is_empty() {
            for (id, e) in self.unreported.drain(..) {
                handler.on_closed(id, e);
            }
        }
        if self
            .background
            .as_ref()
            .is_some_and(background::Background::has_outstanding)
        {
            self.take_background(handler);
        }
        if !self.pending.is_empty() || self.resolver.busy() {
            // Set-up steps cost syscalls; take them at most every few µs.
            let t = rdtsc();
            if t >= self.next_setup {
                self.next_setup = t.wrapping_add(self.setup_ticks);
                self.advance_pending(handler);
            }
        }
        if self.buffered {
            self.buffered = false;
            self.deliver_buffered(handler);
        }
        let mut r = match &mut self.imp {
            Imp::Epoll(e) => e.poll(
                &mut self.conns,
                handler,
                &mut self.stats,
                &mut self.read_seq,
            ),
            // Receives, sends and the kernel entry in one pass, so re-arms and
            // sends share one io_uring_enter.
            #[cfg(feature = "uring")]
            Imp::Uring(u) => u.poll(
                &mut self.conns,
                handler,
                &mut self.stats,
                &mut self.read_seq,
            ),
        };
        if r.is_ok() && matches!(self.imp, Imp::Epoll(_)) {
            r = self.send_queued(handler);
        }
        // After receiving, so a response already in the socket is not timed out.
        self.check_deadlines(handler);
        if !self.conns.closed.is_empty() {
            self.free_closed();
        }
        r?;
        Ok(usize::try_from(self.stats.messages - before).unwrap_or(usize::MAX))
    }

    #[inline]
    fn check_deadlines<H: Handler>(&mut self, handler: &mut H) {
        if E::DEADLINES {
            let t = rdtsc();
            if t >= self.next_expiry {
                self.next_expiry = t.wrapping_add(self.expiry_ticks);
                self.expire_all(handler);
            }
        }
    }

    /// Closes connections whose deadlines passed.
    #[cold]
    fn expire_all<H: Handler>(&mut self, handler: &mut H) {
        let now = monotonic_ns();
        for i in 0..self.conns.len_slots() {
            let id = self.conns.id(i);
            if !self.conns.is_open(i) {
                continue;
            }
            let Some(c) = self.conns.at(i) else {
                continue;
            };
            if let Err(e) = c.expire(now) {
                close_index(&mut self.imp, &mut self.conns, i);
                self.stats.closed += 1;
                handler.on_closed(id, e);
            }
        }
    }

    /// Delivers input that endpoints already held when they were added.
    #[cold]
    fn deliver_buffered<H: Handler>(&mut self, handler: &mut H)
    where
        E: Receive<H>,
    {
        let meta = RxMeta::default();
        for i in 0..self.conns.len_slots() {
            let id = self.conns.id(i);
            if !self.conns.is_open(i) {
                continue;
            }
            let Some(c) = self.conns.at(i) else {
                continue;
            };
            if !c.has_buffered_input() {
                continue;
            }
            // io_uring arms this connection's receive only now, after the
            // buffer layout has settled.
            let r = c.on_received(id, 0, &meta, handler);
            #[cfg(feature = "uring")]
            if let (Ok(_), Imp::Uring(u)) = (&r, &mut self.imp) {
                u.arm_initial(c, i);
            }
            match r {
                Ok(n) => self.stats.messages += n as u64,
                Err(e) => {
                    close_index(&mut self.imp, &mut self.conns, i);
                    self.stats.closed += 1;
                    handler.on_closed(id, e);
                }
            }
        }
    }

    /// Sends queued bytes now (`poll` also does this). Connections that fail are
    /// closed and reported to the handler of the next `poll`;
    /// [`Driver::flush_with`] reports them at once.
    ///
    /// # Errors
    /// The backend itself failed.
    pub fn flush(&mut self) -> Result<()> {
        struct Defer(Vec<(ConnId, Error)>);
        impl Handler for Defer {
            fn on_closed(&mut self, id: ConnId, error: Error) {
                self.0.push((id, error));
            }
        }
        let mut d = Defer(std::mem::take(&mut self.unreported));
        let r = self.flush_with(&mut d);
        self.unreported = d.0;
        r
    }

    /// Sends queued bytes now, reporting connections that fail to `handler`.
    ///
    /// # Errors
    /// The backend itself failed.
    #[inline]
    pub fn flush_with<H: Handler>(&mut self, handler: &mut H) -> Result<()> {
        let r = self.send_queued(handler);
        if !self.conns.closed.is_empty() {
            self.free_closed();
        }
        r
    }

    #[inline]
    #[cfg_attr(not(feature = "uring"), allow(clippy::unnecessary_wraps))]
    fn send_queued<H: Handler>(&mut self, handler: &mut H) -> Result<()> {
        match &mut self.imp {
            Imp::Epoll(e) => {
                for i in 0..self.conns.len_slots() {
                    let Some(c) = self.conns.at(i) else {
                        continue;
                    };
                    if c.tx_queue().is_empty() {
                        continue;
                    }
                    self.stats.send_calls += 1;
                    self.stats.sends += 1;
                    let fd = c.fd();
                    let q = c.tx_queue();
                    match sys::send(fd, q) {
                        Ok(n) => {
                            q.drain(..n);
                        }
                        Err(err) => {
                            let id = self.conns.id(i);
                            e.remove(fd);
                            self.conns.remove(i);
                            self.stats.closed += 1;
                            handler.on_closed(id, err.into());
                        }
                    }
                }
                Ok(())
            }
            #[cfg(feature = "uring")]
            Imp::Uring(u) => u.flush(&mut self.conns, &mut self.stats),
        }
    }
}

impl<E> Drop for Driver<E> {
    fn drop(&mut self) {
        // io_uring may still reference endpoint buffers: cancel and reap before
        // the connections are freed (fields drop after this body).
        #[cfg(feature = "uring")]
        if let Imp::Uring(u) = &mut self.imp
            && !u.shutdown()
        {
            self.conns.leak();
        }
    }
}

/// A UDP socket with software receive timestamps enabled (see
/// `Driver::_timestamp_keeper`); `None` if the kernel refuses.
fn timestamp_keeper() -> Option<socket2::Socket> {
    use std::os::fd::AsRawFd;
    let s = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None).ok()?;
    sys::enable_rx_timestamps(s.as_raw_fd()).ok()?;
    Some(s)
}

fn close_index<E: Endpoint>(imp: &mut Imp, conns: &mut Conns<E>, i: usize) {
    match imp {
        Imp::Epoll(e) => {
            if let Some(fd) = conns.at(i).map(|c| c.fd()) {
                e.remove(fd);
                conns.remove(i);
            }
        }
        #[cfg(feature = "uring")]
        Imp::Uring(u) => u.close(conns, i),
    }
}

fn advance_one<E: Endpoint>(
    p: &mut Pending<E>,
    resolver: &mut Resolver,
    stats: &mut DriverStats,
    now: i64,
    tpu: f64,
) -> Result<Option<E>> {
    if now > p.deadline_ns {
        return Err(Error::Timeout("connect"));
    }
    if !p.started {
        match resolver.lookup(p.dial.host(), now)? {
            Lookup::Ready(ip) => {
                p.dial
                    .start(SocketAddr::new(IpAddr::V4(ip), p.dial.port()))?;
                p.started = true;
            }
            Lookup::Pending => return Ok(None),
        }
    }
    let t0 = rdtsc();
    let r = p.dial.step();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let ns = (rdtsc().wrapping_sub(t0) as f64 / tpu * 1000.0) as u64;
    stats.connect_step_max_ns = stats.connect_step_max_ns.max(ns);
    r
}

/// Reads once from `conn` through `slot` and delivers what arrived.
fn recv_one<E, H>(
    conn: &mut E,
    id: ConnId,
    slot: &mut sys::MsgSlot,
    handler: &mut H,
    stats: &mut DriverStats,
    read_seq: &mut u64,
) -> Result<()>
where
    E: Receive<H>,
    H: Handler,
{
    slot.arm(conn.rx_space());
    stats.recvmsg_calls += 1;
    match sys::recvmsg(conn.fd(), slot)? {
        None => {
            stats.recvmsg_eagain += 1;
            Ok(())
        }
        Some(0) => Err(Error::Closed),
        Some(n) => {
            *read_seq += 1;
            stats.completions += 1;
            let meta = slot.meta(*read_seq);
            stats.messages += conn.on_received(id, n, &meta, handler)? as u64;
            Ok(())
        }
    }
}
