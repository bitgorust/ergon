//! What a `Driver` drives: connection ids, the sealed endpoint traits and the
//! application callbacks. Platform-independent.

use crate::error::Error;

/// Identifies a connection within one `Driver`.
///
/// A slot index plus a generation: once a connection is closed its id never matches again, even when
/// the slot is reused, so a stale id cannot reach a newer connection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnId {
    idx: u32,
    generation: u32,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl ConnId {
    pub(crate) const fn new(idx: u32, generation: u32) -> Self {
        Self { idx, generation }
    }

    pub(crate) const fn generation(self) -> u32 {
        self.generation
    }

    /// The slot index (dense, reused after close; for per-connection arrays).
    #[must_use]
    pub const fn index(self) -> usize {
        self.idx as usize
    }
}

/// The driver-facing side of endpoints and dials. The module is private, so
/// code outside this crate can neither implement these traits nor call their
/// methods: only the driver moves bytes for a connection it owns.
pub(crate) mod sealed {
    use std::net::SocketAddr;
    use std::os::fd::RawFd;

    use super::ConnId;
    use crate::RxMeta;
    use crate::error::Result;

    /// Socket, receive buffer and send queue of a connection.
    pub trait Io {
        /// Whether [`Io::expire`] must be called (request timeouts). The
        /// driver's periodic deadline check is compiled out when `false`.
        const DEADLINES: bool = false;
        /// Fails the connection if something it waits for is overdue.
        fn expire(&mut self, now_ns: i64) -> Result<()> {
            let _ = now_ns;
            Ok(())
        }
        /// The socket.
        fn fd(&self) -> RawFd;
        /// The free part of the receive buffer. With `io_uring` the kernel
        /// writes into it after the call returns, so it must stay at the same
        /// address until `on_received`.
        fn rx_space(&mut self) -> &mut [u8];
        /// Bytes waiting to be sent; the driver drains from the front.
        fn tx_queue(&mut self) -> &mut Vec<u8>;
        /// Input received before the endpoint was added, not yet decoded.
        fn has_buffered_input(&self) -> bool;
    }

    /// Decoding received bytes for handler `H`.
    pub trait Deliver<H: ?Sized>: Io {
        /// Accounts for `n` bytes written into `rx_space`, decodes and delivers
        /// every complete message. `n == 0` decodes buffered input.
        fn on_received(
            &mut self,
            id: ConnId,
            n: usize,
            meta: &RxMeta,
            handler: &mut H,
        ) -> Result<usize>;
    }

    /// A connection being set up.
    pub trait DialIo {
        /// The connection it produces.
        type Conn: Io;
        /// Host to resolve.
        fn host(&self) -> &str;
        /// TCP port.
        fn port(&self) -> u16;
        /// A fixed address, if DNS should be skipped.
        fn addr(&self) -> Option<SocketAddr>;
        /// Starts the non-blocking TCP connect.
        fn start(&mut self, addr: SocketAddr) -> Result<()>;
        /// Advances as far as the socket allows; `Some` once ready.
        fn step(&mut self) -> Result<Option<Self::Conn>>;
    }
}

/// A connection a driver can own: `WsConn`, `HttpConn` or `AnyConn`.
///
/// Sealed: with `io_uring` the kernel writes into the receive buffer after the
/// submitting call returns, so only this crate's types, whose buffers never move
/// while owned by a driver, can be endpoints.
#[allow(private_bounds)] // sealed on purpose: the bound is unnameable outside the crate
pub trait Endpoint: sealed::Io {}
impl<T: sealed::Io> Endpoint for T {}

/// An endpoint that can decode for application handler `H`.
#[allow(private_bounds)]
pub trait Receive<H: ?Sized>: Endpoint + sealed::Deliver<H> {}
impl<H: ?Sized, T: sealed::Deliver<H>> Receive<H> for T {}

/// A connection being set up without blocking (`WsDial`, `HttpDial`,
/// `AnyDial`): DNS is the driver's job; the dial does TCP, TLS and any
/// protocol upgrade.
#[allow(private_bounds)]
pub trait Dial: sealed::DialIo {}
impl<T: sealed::DialIo> Dial for T {}

/// Connection lifecycle callbacks.
pub trait Handler {
    /// A connection started with `Driver::connect` is ready.
    fn on_open(&mut self, id: ConnId) {
        let _ = id;
    }
    /// The driver closed `id` because of `error` (including [`Error::Closed`] for
    /// a peer close). The connection is gone when this is called.
    fn on_closed(&mut self, id: ConnId, error: Error) {
        let _ = (id, error);
    }
}

/// Kernel-interface counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DriverStats {
    /// `poll` calls.
    pub polls: u64,
    /// `recvmsg` calls.
    pub recvmsg_calls: u64,
    /// `recvmsg` calls that returned `EAGAIN`.
    pub recvmsg_eagain: u64,
    /// `epoll_wait` / `epoll_pwait2` calls.
    pub epoll_wait_calls: u64,
    /// `send` calls.
    pub send_calls: u64,
    /// `io_uring_enter` calls.
    pub enter_calls: u64,
    /// Receive completions with data.
    pub completions: u64,
    /// Messages delivered.
    pub messages: u64,
    /// Connections closed because of an error or peer close.
    pub closed: u64,
    /// Connections opened by `Driver::connect`.
    pub connects: u64,
    /// `Driver::connect` attempts that failed or timed out.
    pub connect_failures: u64,
    /// Longest single connection-setup step on the polling thread, in
    /// nanoseconds (the TLS handshake's crypto lands here).
    pub connect_step_max_ns: u64,
}

impl DriverStats {
    /// Total syscalls made by the driver.
    #[must_use]
    pub const fn syscalls(&self) -> u64 {
        self.recvmsg_calls + self.epoll_wait_calls + self.send_calls + self.enter_calls
    }
}
