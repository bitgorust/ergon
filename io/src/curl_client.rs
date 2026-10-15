//! The libcurl implementation behind `HttpClient` (feature `curl`): libcurl is
//! linked in-process through the `curl` crate (no separate program). One multi
//! handle, easy handles reused across requests so libcurl's connection cache
//! keeps connections warm, driven without blocking.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use curl::easy::{Easy2, Handler as CurlHandler, HttpVersion, List, WriteError};
use curl::multi::{Easy2Handle, Events, Multi};

use crate::client::{ClientHandler, CurlDrive, HttpClientConfig};
use crate::clock::monotonic_ns;
use crate::error::{Error, Result};
use crate::http::Response;
use crate::net::Target;

/// Collects one response.
#[derive(Default)]
struct Collector {
    head: Vec<u8>,
    body: Vec<u8>,
}

impl CurlHandler for Collector {
    fn write(&mut self, data: &[u8]) -> std::result::Result<usize, WriteError> {
        self.body.extend_from_slice(data);
        Ok(data.len())
    }

    fn header(&mut self, data: &[u8]) -> bool {
        // A new status line (after a 100 Continue or a redirect) starts over.
        if data.starts_with(b"HTTP/") {
            self.head.clear();
        }
        self.head.extend_from_slice(data);
        true
    }
}

/// Sockets libcurl asked to watch (socket-action mode).
struct Watch {
    /// Closed when the client drops, after `multi` (declared earlier), whose
    /// clean-up still removes sockets from it.
    ep: OwnedFd,
    /// Deadline for `curl_multi_socket_action(CURL_SOCKET_TIMEOUT)`, monotonic
    /// ns; `i64::MAX` when none.
    timer: Arc<AtomicI64>,
}

/// Fields drop in declaration order: `multi` (and with it libcurl's socket
/// callbacks) goes before `watch` closes the epoll descriptor.
pub struct CurlClient {
    multi: Multi,
    free: Vec<Easy2<Collector>>,
    active: Vec<(u64, Easy2Handle<Collector>)>,
    base: String,
    resolve: Option<String>,
    fixed: Vec<String>,
    ca_file: Option<String>,
    timeout: Duration,
    pool: usize,
    watch: Option<Watch>,
}

impl CurlClient {
    pub(crate) fn new(cfg: &HttpClientConfig, target: &Target) -> Result<Self> {
        curl::init();
        let mut multi = Multi::new();
        multi
            .set_max_host_connections(cfg.pool)
            .map_err(|e| Error::Http(format!("libcurl: {e}")))?;
        let base = if target.port == 443 {
            format!("https://{}", target.host)
        } else {
            format!("https://{}:{}", target.host, target.port)
        };
        let watch = if cfg.curl_drive == CurlDrive::SocketAction {
            // SAFETY: plain syscall.
            let raw = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
            if raw < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            // SAFETY: a fresh descriptor we own; closed on every return path.
            let ep_owned = unsafe { OwnedFd::from_raw_fd(raw) };
            let ep = ep_owned.as_raw_fd();
            let timer = Arc::new(AtomicI64::new(i64::MAX));
            let t = timer.clone();
            multi
                .timer_function(move |d| {
                    let at = d.map_or(i64::MAX, |d| {
                        monotonic_ns()
                            .saturating_add(i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
                    });
                    t.store(at, Ordering::Relaxed);
                    true
                })
                .map_err(|e| Error::Http(format!("libcurl: {e}")))?;
            // Interest per socket, owned by the socket callback.
            let int: Mutex<HashMap<RawFd, u32>> = Mutex::new(HashMap::new());
            multi
                .socket_function(move |sock, events, _token| {
                    let mut flags = 0u32;
                    if events.input() {
                        flags |= libc::EPOLLIN.cast_unsigned();
                    }
                    if events.output() {
                        flags |= libc::EPOLLOUT.cast_unsigned();
                    }
                    let mut ev = libc::epoll_event {
                        events: flags,
                        u64: u64::try_from(sock).unwrap_or(0),
                    };
                    let Ok(mut known) = int.lock() else {
                        return;
                    };
                    // SAFETY: valid epoll fd and event; libcurl owns `sock`.
                    unsafe {
                        if events.remove() {
                            libc::epoll_ctl(ep, libc::EPOLL_CTL_DEL, sock, std::ptr::null_mut());
                            known.remove(&sock);
                        } else if known.insert(sock, flags).is_some() {
                            libc::epoll_ctl(ep, libc::EPOLL_CTL_MOD, sock, &raw mut ev);
                        } else {
                            libc::epoll_ctl(ep, libc::EPOLL_CTL_ADD, sock, &raw mut ev);
                        }
                    }
                })
                .map_err(|e| Error::Http(format!("libcurl: {e}")))?;
            Some(Watch {
                ep: ep_owned,
                timer,
            })
        } else {
            None
        };
        Ok(Self {
            multi,
            free: Vec::new(),
            active: Vec::new(),
            base,
            resolve: target
                .addr
                .map(|a| format!("{}:{}:{}", target.host, target.port, a.ip())),
            fixed: target
                .headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect(),
            ca_file: cfg.curl_ca_file.clone(),
            timeout: Duration::from_millis(u64::from(cfg.request_timeout_ms)),
            pool: cfg.pool,
            watch,
        })
    }

    pub(crate) const fn idle(&self) -> usize {
        self.pool.saturating_sub(self.active.len())
    }

    fn easy(&mut self) -> Result<Easy2<Collector>> {
        if let Some(e) = self.free.pop() {
            return Ok(e);
        }
        let map = |e: curl::Error| Error::Http(format!("libcurl: {e}"));
        let mut e = Easy2::new(Collector::default());
        e.http_version(HttpVersion::V11).map_err(map)?;
        e.signal(false).map_err(map)?;
        e.tcp_nodelay(true).map_err(map)?;
        e.timeout(self.timeout).map_err(map)?;
        if let Some(r) = &self.resolve {
            let mut l = List::new();
            l.append(r).map_err(map)?;
            e.resolve(l).map_err(map)?;
        }
        if let Some(ca) = &self.ca_file {
            e.cainfo(ca).map_err(map)?;
        }
        Ok(e)
    }

    pub(crate) fn request(
        &mut self,
        id: u64,
        method: &str,
        path: &str,
        query: Option<&str>,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<()> {
        if self.active.len() >= self.pool {
            return Err(Error::Busy("libcurl pool fully in use"));
        }
        let map = |e: curl::Error| Error::Http(format!("libcurl: {e}"));
        let mut e = self.easy()?;
        let mut url =
            String::with_capacity(self.base.len() + path.len() + 1 + query.map_or(0, str::len));
        url.push_str(&self.base);
        url.push_str(path);
        if let Some(q) = query {
            url.push('?');
            url.push_str(q);
        }
        e.url(&url).map_err(map)?;
        let mut list = List::new();
        for h in &self.fixed {
            list.append(h).map_err(map)?;
        }
        for (k, v) in headers {
            list.append(&format!("{k}: {v}")).map_err(map)?;
        }
        // Never wait for a 100-continue.
        list.append("Expect:").map_err(map)?;
        e.http_headers(list).map_err(map)?;
        // HEAD: libcurl must not wait for the announced body (NOBODY); reused
        // handles reset it for every other method.
        let head = method == "HEAD";
        e.nobody(head).map_err(map)?;
        if head {
        } else if !body.is_empty() || matches!(method, "POST" | "PUT" | "PATCH") {
            e.post(true).map_err(map)?;
            e.post_fields_copy(body).map_err(map)?;
        } else {
            e.get(true).map_err(map)?;
        }
        e.custom_request(method).map_err(map)?;
        e.get_mut().head.clear();
        e.get_mut().body.clear();
        let h = self
            .multi
            .add2(e)
            .map_err(|e| Error::Http(format!("libcurl: {e}")))?;
        self.active.push((id, h));
        Ok(())
    }

    pub(crate) fn poll<H: ClientHandler>(&mut self, handler: &mut H) -> Result<usize> {
        if self.active.is_empty() {
            return Ok(0);
        }
        let mmap = |e: curl::MultiError| Error::Http(format!("libcurl: {e}"));
        match &self.watch {
            None => {
                self.multi.perform().map_err(mmap)?;
            }
            Some(w) => {
                let mut evs = [libc::epoll_event { events: 0, u64: 0 }; 16];
                // SAFETY: valid epoll fd and event buffer.
                let n = unsafe { libc::epoll_wait(w.ep.as_raw_fd(), evs.as_mut_ptr(), 16, 0) };
                for ev in evs.iter().take(usize::try_from(n).unwrap_or(0)) {
                    let fd = RawFd::try_from(ev.u64).unwrap_or(-1);
                    let mut e = Events::new();
                    e.input(ev.events & libc::EPOLLIN.cast_unsigned() != 0);
                    e.output(ev.events & libc::EPOLLOUT.cast_unsigned() != 0);
                    self.multi.action(fd, &e).map_err(mmap)?;
                }
                if monotonic_ns() >= w.timer.load(Ordering::Relaxed) {
                    w.timer.store(i64::MAX, Ordering::Relaxed);
                    self.multi.timeout().map_err(mmap)?;
                }
            }
        }
        let mut finished: Vec<(usize, std::result::Result<(), curl::Error>)> = Vec::new();
        let active = &self.active;
        self.multi.messages(|m| {
            for (i, (_, h)) in active.iter().enumerate() {
                if let Some(r) = m.result_for2(h) {
                    finished.push((i, r));
                }
            }
        });
        if finished.is_empty() {
            return Ok(0);
        }
        // Remove from the back so indices stay valid.
        finished.sort_unstable_by_key(|f| std::cmp::Reverse(f.0));
        let done = finished.len();
        for (i, result) in finished {
            let (id, h) = self.active.swap_remove(i);
            let e = self.multi.remove2(h).map_err(mmap)?;
            match result {
                Ok(()) => deliver(id, &e, handler),
                Err(err) => handler.on_error(id, Error::Http(format!("libcurl: {err}"))),
            }
            self.free.push(e);
        }
        Ok(done)
    }
}

fn deliver<H: ClientHandler>(id: u64, e: &Easy2<Collector>, handler: &mut H) {
    let status = e
        .response_code()
        .ok()
        .and_then(|c| u16::try_from(c).ok())
        .unwrap_or(0);
    let c = e.get_ref();
    let mut hdrs = [httparse::EMPTY_HEADER; 64];
    let mut resp = httparse::Response::new(&mut hdrs);
    let headers: &[httparse::Header<'_>] = match resp.parse(&c.head) {
        Ok(httparse::Status::Complete(_)) => resp.headers,
        _ => &[],
    };
    handler.on_response(
        id,
        &Response {
            status,
            headers,
            body: &c.body,
        },
    );
}

impl Drop for CurlClient {
    fn drop(&mut self) {
        // Easy handles must leave the multi before it is cleaned up.
        for (_, h) in self.active.drain(..) {
            let _ = self.multi.remove2(h);
        }
        // The epoll descriptor closes with `watch`, after `multi`.
    }
}
