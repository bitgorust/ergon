//! A pooled HTTP client whose implementation is chosen by configuration.
//!
//! The implementations: ergon-io's own HTTP/1.1 client ([`HttpImpl::Native`], the fast path) or
//! libcurl ([`HttpImpl::Curl`], feature `curl`) when its extra features are
//! needed (redirects, proxies, HTTP/2, authentication schemes, compression).
//!
//! Both sit behind one API: [`HttpClient::request`] /
//! [`HttpClient::signed_request`] return a request id at once, and each
//! [`HttpClient::poll`] (one non-blocking iteration) reports finished requests
//! through [`ClientHandler`]. Switching implementations is a configuration
//! change, not a code change; the dispatch costs one predictable branch per call.
//!
//! - **Native:** a pool of warm connections on a [`Driver`]; each request goes
//!   to a connection with nothing in flight, and a connection that drops is
//!   replaced in the background (with back-off).
//! - **Curl:** one libcurl multi handle with reused easy handles (its connection
//!   cache keeps connections warm), driven without blocking by
//!   `curl_multi_perform` on every poll or by `curl_multi_socket_action` on
//!   libcurl's own sockets ([`CurlDrive`]).

use std::str::FromStr;
use std::sync::Arc;

use crate::RxMeta;
use crate::auth::Signer;
use crate::backend::{Backend, Config};
use crate::clock::monotonic_ns;
use crate::driver::Driver;
use crate::endpoint::{ConnId, Handler};
use crate::error::{Error, Result};
use crate::http::{HttpConn, HttpDial, HttpHandler, HttpOptions, Response};
use crate::net::Target;

/// Which HTTP implementation an [`HttpClient`] uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpImpl {
    /// ergon-io's HTTP/1.1 client on the [`Driver`].
    Native,
    /// libcurl (needs the `curl` feature).
    Curl,
}

impl FromStr for HttpImpl {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "native" => Ok(Self::Native),
            "curl" => Ok(Self::Curl),
            _ => Err(Error::Config(format!("unknown HTTP implementation {s:?}"))),
        }
    }
}

/// How the libcurl implementation is driven (both never block).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurlDrive {
    /// `curl_multi_perform` on every poll.
    Perform,
    /// `curl_multi_socket_action` on the sockets libcurl reports ready, from a
    /// zero-timeout `epoll_wait` on libcurl's own sockets.
    SocketAction,
}

/// [`HttpClient`] settings.
#[derive(Debug, Clone)]
pub struct HttpClientConfig {
    /// The implementation.
    pub implementation: HttpImpl,
    /// Native: the loop backend for the pool's driver.
    pub driver: Config,
    /// Connections kept warm (native) or the most libcurl may use at once.
    pub pool: usize,
    /// Time from sending a request to its complete response, in milliseconds.
    pub request_timeout_ms: u32,
    /// Native: receive buffer per connection (must hold the largest response).
    pub buf_size: usize,
    /// Native: open connections on the driver's helper thread
    /// (`Driver::connect_background`), keeping handshake CPU off the loop.
    pub background_connect: bool,
    /// Curl: how libcurl is driven.
    pub curl_drive: CurlDrive,
    /// Curl: a CA bundle file instead of the system store. (Native trust comes
    /// from the rustls configuration passed to [`HttpClient::new`].)
    pub curl_ca_file: Option<String>,
}

impl HttpClientConfig {
    /// Defaults: native, `recv-busy`, a pool of 2, background connects.
    #[must_use]
    pub const fn new(implementation: HttpImpl) -> Self {
        Self {
            implementation,
            driver: Config::new(Backend::RecvBusy),
            pool: 2,
            request_timeout_ms: 5_000,
            buf_size: 1 << 20,
            background_connect: true,
            curl_drive: CurlDrive::Perform,
            curl_ca_file: None,
        }
    }
}

/// Receives the outcome of every request.
pub trait ClientHandler {
    /// The complete response to `request`; it borrows the client's buffers for
    /// the duration of the call.
    fn on_response(&mut self, request: u64, response: &Response<'_>);
    /// `request` failed (connection error, timeout, libcurl error). Every
    /// request ends in exactly one of `on_response` or `on_error`.
    fn on_error(&mut self, request: u64, error: Error) {
        let _ = (request, error);
    }
    /// A pooled connection could not be opened (native implementation). The
    /// pool keeps retrying with back-off (100 ms doubling to 5 s); `poll` itself
    /// does not fail, and [`HttpClient::connect_failures`] counts them.
    ///
    /// The default prints a warning to stderr, at most one every 10 s across
    /// the process (with the number held back). A thread started with the
    /// client, on the housekeeping CPUs, makes the write, so the polling
    /// thread only formats the line and queues it. Implement it to route
    /// failures to your own logging or alerting.
    fn on_connect_error(&mut self, error: Error) {
        unhandled::warn("HttpClient could not open a connection", &error);
    }
}

/// Warnings for failures the application does not handle itself.
mod unhandled {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
    use std::sync::mpsc::{self, Sender};

    use crate::clock::monotonic_ns;
    use crate::error::Error;

    const EVERY_NS: i64 = 10_000_000_000;
    static LAST_NS: AtomicI64 = AtomicI64::new(i64::MIN);
    static HELD_BACK: AtomicU64 = AtomicU64::new(0);
    static WRITER: OnceLock<Option<Sender<String>>> = OnceLock::new();

    /// Starts the writer thread (on the housekeeping CPUs: stderr may be a
    /// pipe that blocks). Called when a client is created, so neither the
    /// sysfs reads nor the thread start land on the polling thread.
    pub(super) fn prepare() {
        WRITER.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<String>();
            let cpus = crate::sys::housekeeping_cpus(crate::sys::current_cpu());
            crate::sys::spawn_pinned("ergon-io-warn", &cpus, move || {
                for m in rx {
                    eprintln!("{m}");
                }
            })
            .ok()
            .map(|()| tx)
        });
    }

    /// Queues `what: error` for the writer, at most once every 10 s; written
    /// here only if the writer could not start. There is one writer per
    /// process (`WRITER`), parked in `recv`: each queued line costs the polling
    /// thread an allocation and a futex wake, bounded by the rate limit.
    pub(super) fn warn(what: &str, error: &Error) {
        let now = monotonic_ns();
        let last = LAST_NS.load(Ordering::Relaxed);
        let due = last == i64::MIN || now - last >= EVERY_NS;
        // One winner per 10 s, even with clients on several threads.
        if !due
            || LAST_NS
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            HELD_BACK.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let held = HELD_BACK.swap(0, Ordering::Relaxed);
        let msg = format!(
            "ergon-io warning: {what}: {error}{}; retrying (implement \
             ClientHandler::on_connect_error to handle this yourself)",
            if held > 0 {
                format!(" ({held} more since the last warning)")
            } else {
                String::new()
            }
        );
        match WRITER.get().and_then(Option::as_ref) {
            Some(tx) => {
                if let Err(mpsc::SendError(m)) = tx.send(msg) {
                    eprintln!("{m}");
                }
            }
            None => eprintln!("{msg}"),
        }
    }
}

enum Imp {
    Native(Box<Native>),
    #[cfg(feature = "curl")]
    Curl(Box<crate::curl_client::CurlClient>),
}

/// A pooled HTTP client; see the module documentation.
pub struct HttpClient {
    imp: Imp,
    next_id: u64,
}

impl HttpClient {
    /// A client for `target` (its headers, such as the API key, go with every
    /// request). Native connections start opening at once; a connection that
    /// cannot be opened is reported to [`ClientHandler::on_connect_error`] from
    /// `poll` and retried.
    ///
    /// # Errors
    /// The implementation is not built in, the configuration is invalid
    /// (headers, TLS config), or the driver or libcurl fails to initialise.
    pub fn new(
        cfg: &HttpClientConfig,
        target: Target,
        tls: Arc<rustls::ClientConfig>,
    ) -> Result<Self> {
        if cfg.pool == 0 {
            return Err(Error::Config("pool must be at least 1".into()));
        }
        let imp = match cfg.implementation {
            HttpImpl::Native => {
                // The default `on_connect_error` warning's writer, started now
                // rather than on the polling thread at the first failure.
                unhandled::prepare();
                Imp::Native(Box::new(Native::new(cfg, target, tls)?))
            }
            #[cfg(feature = "curl")]
            HttpImpl::Curl => {
                let _ = tls;
                Imp::Curl(Box::new(crate::curl_client::CurlClient::new(cfg, &target)?))
            }
            #[cfg(not(feature = "curl"))]
            HttpImpl::Curl => {
                return Err(Error::Unsupported(
                    "the libcurl client needs the `curl` feature",
                ));
            }
        };
        Ok(Self { imp, next_id: 0 })
    }

    /// The implementation in use.
    #[must_use]
    pub const fn implementation(&self) -> HttpImpl {
        match self.imp {
            Imp::Native(_) => HttpImpl::Native,
            #[cfg(feature = "curl")]
            Imp::Curl(_) => HttpImpl::Curl,
        }
    }

    /// Connections the native pool failed to open since it was created (each
    /// also went to [`ClientHandler::on_connect_error`]); 0 for libcurl, which
    /// reports connect failures as request errors.
    #[must_use]
    pub fn connect_failures(&self) -> u64 {
        match &self.imp {
            Imp::Native(n) => n.connect_failures,
            #[cfg(feature = "curl")]
            Imp::Curl(_) => 0,
        }
    }

    /// Requests that can be sent now without [`Error::Busy`]: idle open
    /// connections (native) or free handles (libcurl).
    #[must_use]
    pub fn idle(&self) -> usize {
        match &self.imp {
            Imp::Native(n) => n.idle(),
            #[cfg(feature = "curl")]
            Imp::Curl(c) => c.idle(),
        }
    }

    /// Sends a request (`path` may carry a query). Returns its id.
    ///
    /// # Errors
    /// [`Error::Busy`] when no connection or handle is free, or an invalid
    /// request.
    pub fn request(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<u64> {
        let id = self.next_id;
        match &mut self.imp {
            Imp::Native(n) => n.send(id, |c| c.request(method, path, headers, body))?,
            #[cfg(feature = "curl")]
            Imp::Curl(c) => c.request(id, method, path, None, headers, body)?,
        }
        self.next_id += 1;
        Ok(id)
    }

    /// Sends a signed request: `params`, `timestamp` and `signature` in the
    /// query string, as [`HttpConn::signed_request`]. Returns its id.
    ///
    /// # Errors
    /// As [`HttpClient::request`], or `params` needs percent-encoding.
    pub fn signed_request(
        &mut self,
        method: &str,
        path: &str,
        params: &str,
        headers: &[(&str, &str)],
        signer: &Signer,
    ) -> Result<u64> {
        let id = self.next_id;
        match &mut self.imp {
            Imp::Native(n) => {
                n.send(id, |c| {
                    c.signed_request(method, path, params, headers, signer)
                })?;
            }
            #[cfg(feature = "curl")]
            Imp::Curl(c) => {
                crate::auth::validate_params(params)?;
                let now_ms = u64::try_from(crate::clock::realtime_ns() / 1_000_000).unwrap_or(0);
                let mut q = Vec::with_capacity(params.len() + 128);
                crate::auth::append_signed_params(&mut q, params, now_ms, signer);
                let q = String::from_utf8(q).map_err(|_| Error::Http("non-UTF-8 query".into()))?;
                c.request(id, method, path, Some(&q), headers, &[])?;
            }
        }
        self.next_id += 1;
        Ok(id)
    }

    /// One non-blocking iteration: moves bytes, reports finished requests, and
    /// replaces dropped connections. Returns requests finished.
    ///
    /// # Errors
    /// The driver or libcurl failed as a whole (per-request failures go to
    /// [`ClientHandler::on_error`]).
    pub fn poll<H: ClientHandler>(&mut self, handler: &mut H) -> Result<usize> {
        match &mut self.imp {
            Imp::Native(n) => n.poll(handler),
            #[cfg(feature = "curl")]
            Imp::Curl(c) => c.poll(handler),
        }
    }
}

struct Slot {
    id: Option<ConnId>,
    open: bool,
    request: Option<u64>,
    retry_at_ns: i64,
    backoff_ms: i64,
}

struct Native {
    driver: Driver<HttpConn>,
    target: Target,
    tls: Arc<rustls::ClientConfig>,
    opts: HttpOptions,
    background: bool,
    slots: Vec<Slot>,
    next: usize,
    connect_failures: u64,
}

impl Native {
    fn new(cfg: &HttpClientConfig, target: Target, tls: Arc<rustls::ClientConfig>) -> Result<Self> {
        let mut n = Self {
            driver: Driver::new(cfg.driver.clone())?,
            target,
            tls,
            opts: HttpOptions {
                request_timeout_ms: cfg.request_timeout_ms,
                max_in_flight: 1,
                buf_size: cfg.buf_size,
            },
            background: cfg.background_connect,
            slots: (0..cfg.pool)
                .map(|_| Slot {
                    id: None,
                    open: false,
                    request: None,
                    retry_at_ns: 0,
                    backoff_ms: 0,
                })
                .collect(),
            next: 0,
            connect_failures: 0,
        };
        // Configuration errors (headers, TLS config) fail here; connect
        // failures go to the handler from `poll`, which retries them.
        HttpDial::new(n.target.clone(), n.tls.clone(), n.opts)?;
        for i in 0..n.slots.len() {
            let _ = n.open(i);
        }
        Ok(n)
    }

    fn open(&mut self, i: usize) -> Result<()> {
        let dial = HttpDial::new(self.target.clone(), self.tls.clone(), self.opts)?;
        let id = if self.background {
            self.driver.connect_background(dial)?
        } else {
            self.driver.connect(dial)?
        };
        self.slots[i].id = Some(id);
        Ok(())
    }

    fn idle(&self) -> usize {
        self.slots
            .iter()
            .filter(|s| s.open && s.request.is_none())
            .count()
    }

    fn send(&mut self, id: u64, f: impl FnOnce(&mut HttpConn) -> Result<u64>) -> Result<()> {
        let n = self.slots.len();
        let Some(i) = (0..n)
            .map(|k| (self.next + k) % n)
            .find(|&i| self.slots[i].open && self.slots[i].request.is_none())
        else {
            return Err(Error::Busy("no idle connection in the pool"));
        };
        let mut conn = self.slots[i]
            .id
            .and_then(|c| self.driver.get_mut(c))
            .ok_or(Error::Closed)?;
        f(conn.inner())?;
        self.slots[i].request = Some(id);
        self.next = (i + 1) % n;
        Ok(())
    }

    fn poll<H: ClientHandler>(&mut self, handler: &mut H) -> Result<usize> {
        let mut a = Adapter {
            h: handler,
            slots: &mut self.slots,
            done: 0,
            connect_failures: 0,
        };
        self.driver.poll(&mut a)?;
        let done = a.done;
        self.connect_failures += a.connect_failures;
        // Replace dropped connections (with back-off on repeated failure).
        if self.slots.iter().any(|s| s.id.is_none()) {
            let now = monotonic_ns();
            for i in 0..self.slots.len() {
                if self.slots[i].id.is_none() && now >= self.slots[i].retry_at_ns {
                    let s = &mut self.slots[i];
                    s.backoff_ms = (s.backoff_ms * 2).clamp(100, 5_000);
                    s.retry_at_ns = now + s.backoff_ms * 1_000_000;
                    // A refused or unroutable connect fails here at once: report
                    // it and retry later, as for one that fails while connecting.
                    if let Err(e) = self.open(i) {
                        self.connect_failures += 1;
                        handler.on_connect_error(e);
                    }
                }
            }
        }
        Ok(done)
    }
}

/// Maps driver callbacks onto pool slots and client request ids.
struct Adapter<'a, H> {
    h: &'a mut H,
    slots: &'a mut [Slot],
    done: usize,
    connect_failures: u64,
}

impl<H> Adapter<'_, H> {
    fn slot(&mut self, id: ConnId) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|s| s.id == Some(id))
    }
}

impl<H: ClientHandler> Handler for Adapter<'_, H> {
    fn on_open(&mut self, id: ConnId) {
        if let Some(s) = self.slot(id) {
            s.open = true;
            s.backoff_ms = 0;
        }
    }

    fn on_closed(&mut self, id: ConnId, error: Error) {
        let Some(s) = self.slot(id) else {
            return;
        };
        s.id = None;
        let was_open = std::mem::replace(&mut s.open, false);
        if let Some(r) = s.request.take() {
            self.done += 1;
            self.h.on_error(r, error);
        } else if !was_open {
            // It never opened: a failed connect.
            self.connect_failures += 1;
            self.h.on_connect_error(error);
        }
    }
}

impl<H: ClientHandler> HttpHandler for Adapter<'_, H> {
    fn on_response(&mut self, conn: ConnId, _request: u64, response: &Response<'_>, _: &RxMeta) {
        if let Some(r) = self.slot(conn).and_then(|s| s.request.take()) {
            self.done += 1;
            self.h.on_response(r, response);
        }
    }
}
