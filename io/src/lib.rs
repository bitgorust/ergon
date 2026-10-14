//! Busy-spin network I/O for low-latency exchange clients.
//!
//! - `Driver` owns connections and moves bytes with a [`Backend`] chosen at run
//!   time, e.g. `Config::parse("recv-busy")` or `"uring-sqpoll,sqpoll_cpu=3"`
//!   (see [`Config::parse`]).
//!   `Driver::poll` does one non-blocking iteration and returns, so the
//!   caller's loop stays in control. See [`backend`] for which backend suits
//!   which use.
//! - [`WsConn`]: TLS 1.3 WebSocket client. rustls does the handshake, then
//!   [`tls::RecordLayer`] decrypts records in place, the WebSocket frames are
//!   parsed in place, and each payload reaches [`WsHandler::on_message`] as a
//!   slice of the one receive buffer the kernel wrote into: one copy per byte.
//! - [`HttpConn`]: HTTP/1.1 client for order entry: each request written into
//!   one TLS record, responses parsed in place, Binance-style request signing
//!   ([`Signer`]: HMAC-SHA256 or Ed25519) without allocation.
//! - `HttpClient`: a pool of warm connections behind one API, with the
//!   implementation chosen by configuration: native (the fast path) or libcurl
//!   (feature `curl`) when its extra features are needed.
//! - Connections open without blocking (`Driver::connect`: DNS, TCP, TLS and
//!   any upgrade advanced a step per poll), or entirely on a helper thread
//!   (`Driver::connect_background`) so the loop spends no handshake CPU.
//!
//! Resources are owned: sockets close when their connection drops, a
//! connection that fails is closed and reported through [`Handler::on_closed`],
//! and dropping a `Driver` cancels in-flight `io_uring` operations before
//! freeing the buffers they point at.
//!
//! The driver is Linux-only; the protocol layers build everywhere.

pub mod any;
pub mod auth;
pub mod backend;
pub mod clock;
pub mod dns;
pub mod endpoint;
pub mod error;
pub mod http;
pub mod net;
pub mod stream;
pub mod tls;
pub mod ws;
pub mod ws_conn;

#[cfg(target_os = "linux")]
pub mod client;
#[cfg(all(target_os = "linux", feature = "curl"))]
mod curl_client;
#[cfg(target_os = "linux")]
mod driver;
#[cfg(target_os = "linux")]
pub mod sys;

pub use any::{AnyConn, AnyDial};
pub use auth::{SignatureKind, Signer};
pub use backend::{Backend, Config};
#[cfg(target_os = "linux")]
pub use client::{ClientHandler, CurlDrive, HttpClient, HttpClientConfig, HttpImpl};
#[cfg(target_os = "linux")]
pub use driver::Driver;
pub use endpoint::{ConnId, Dial, DriverStats, Endpoint, Handler, Receive};
pub use error::{Error, Result};
pub use http::{HttpConn, HttpDial, HttpHandler, HttpOptions, HttpStats, Response};
pub use net::Target;
pub use ws_conn::{WsConn, WsDial, WsHandler, WsStats};

/// Receive metadata for one read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RxMeta {
    /// Kernel software receive timestamp (`CLOCK_REALTIME` ns), 0 if absent.
    pub rx_sw_ns: i64,
    /// NIC hardware timestamp (raw PHC ns), 0 if absent.
    pub rx_hw_ns: i64,
    /// Sequence number of this read within its driver.
    pub read_seq: u64,
}
