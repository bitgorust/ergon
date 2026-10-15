//! A TLS WebSocket client connection.
//!
//! [`WsDial`] sets it up without blocking,
//! [`WsConn`] parses server frames in place in the shared receive buffer
//! (see [`crate::stream`]), so a payload is a slice of the bytes the kernel wrote.

use std::io::{ErrorKind, Read};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;

use rustls::ClientConfig;

use crate::RxMeta;
use crate::endpoint::sealed::{Deliver, DialIo, Io};
use crate::endpoint::{ConnId, ConnMut, Handler};
use crate::error::{Error, Result};
pub use crate::net::Target;
use crate::net::{TlsDial, flush_nonblocking};
use crate::stream::TlsStream;
use crate::tls::{self, MAX_RECORD, Opened, RecordLayer};
use crate::ws;

/// Receives decoded WebSocket messages.
pub trait WsHandler: Handler {
    /// One complete data message (`opcode` is [`ws::OP_TEXT`] or
    /// [`ws::OP_BINARY`]). `payload` borrows the receive buffer: it is valid only
    /// during the call. `pos` is its index among the messages decoded from the
    /// same read.
    fn on_message(&mut self, conn: ConnId, opcode: u8, payload: &[u8], meta: &RxMeta, pos: u16);
}

/// Per-connection counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WsStats {
    /// Bytes received from the kernel.
    pub bytes_in: u64,
    /// TLS records opened.
    pub records: u64,
    /// Data messages delivered.
    pub messages: u64,
    /// Plaintext bytes moved because a frame spanned two records.
    pub span_copy_bytes: u64,
    /// Bytes moved by buffer compaction.
    pub compact_bytes: u64,
    /// Bytes copied to reassemble fragmented messages.
    pub frag_copy_bytes: u64,
    /// Pings answered.
    pub pings: u64,
}

/// A connected, upgraded TLS WebSocket. Hand it to a `Driver`; the
/// socket closes when it is dropped.
pub struct WsConn {
    stream: TlsStream,
    frag: Vec<u8>,
    /// Opcode of the fragmented message in progress, 0 when none.
    frag_op: u8,
    /// Largest frame or reassembled message accepted.
    max_frame: usize,
    rng: u64,
    messages: u64,
    frag_copy_bytes: u64,
    pings: u64,
}

/// Smallest receive buffer [`WsConn::connect`] accepts.
pub const MIN_BUF: usize = 8 * MAX_RECORD;

impl WsConn {
    /// Connects, runs the TLS handshake and the WebSocket upgrade, waiting until
    /// done: a convenience for tools and tests. In a busy-spin loop use
    /// [`WsDial`] with `Driver::connect`, which never waits. Without
    /// [`Target::addr`] the host is resolved with the system resolver (blocking).
    ///
    /// # Errors
    /// Resolution, connect, TLS or upgrade failure, or no progress for 30 s.
    pub fn connect(target: &Target, cfg: Arc<ClientConfig>, buf_size: usize) -> Result<Self> {
        let addr = match target.addr {
            Some(a) => a,
            None => (target.host.as_str(), target.port)
                .to_socket_addrs()?
                .find(SocketAddr::is_ipv4)
                .ok_or_else(|| Error::Dns(format!("{}: no IPv4 address", target.host)))?,
        };
        let mut dial = WsDial::new(target.clone(), cfg, buf_size)?;
        dial.start(addr)?;
        let start = std::time::Instant::now();
        loop {
            if let Some(c) = dial.step()? {
                return Ok(c);
            }
            if start.elapsed() > std::time::Duration::from_secs(30) {
                return Err(Error::Timeout("websocket connect"));
            }
            std::thread::yield_now();
        }
    }

    /// Counters.
    #[must_use]
    pub const fn stats(&self) -> WsStats {
        let s = self.stream.stats();
        WsStats {
            bytes_in: s.bytes_in,
            records: s.records,
            messages: self.messages,
            span_copy_bytes: s.span_copy_bytes,
            compact_bytes: s.compact_bytes,
            frag_copy_bytes: self.frag_copy_bytes,
            pings: self.pings,
        }
    }

    /// The socket's descriptor, for socket options and diagnostics. Do not read
    /// from or write to it: the driver owns the byte stream.
    #[must_use]
    pub fn raw_fd(&self) -> std::os::fd::RawFd {
        self.stream.fd()
    }

    /// Accounts for `n` received bytes, decrypts every complete record and
    /// delivers every complete message. `n == 0` parses what is already buffered
    /// (frames that arrived with the upgrade).
    #[inline]
    fn deliver<H: WsHandler>(
        &mut self,
        id: ConnId,
        n: usize,
        meta: &RxMeta,
        handler: &mut H,
    ) -> Result<usize> {
        self.stream.received(n);
        let mut delivered = 0usize;
        if n == 0 {
            delivered += self.parse_frames(id, meta, handler, 0)?;
        }
        while self.stream.open_next()? {
            delivered += self.parse_frames(id, meta, handler, delivered)?;
        }
        self.stream.compact();
        Ok(delivered)
    }

    #[inline]
    fn parse_frames<H: WsHandler>(
        &mut self,
        id: ConnId,
        meta: &RxMeta,
        handler: &mut H,
        already: usize,
    ) -> Result<usize> {
        let mut pos = already;
        let mut count = 0;
        loop {
            let window = self.stream.window();
            let Some(h) = ws::parse_header(window)? else {
                break;
            };
            if h.mask.is_some() {
                return Err(Error::Ws("server frame is masked"));
            }
            if window.len() < h.frame_len() {
                // Never complete in this buffer: fail rather than stall.
                if h.frame_len() > self.max_frame {
                    return Err(Error::Ws("frame larger than the receive buffer"));
                }
                break;
            }
            let payload = &window[h.header_len..h.frame_len()];
            match h.opcode {
                ws::OP_TEXT | ws::OP_BINARY if h.fin => {
                    handler.on_message(id, h.opcode, payload, meta, pos16(pos));
                    pos += 1;
                    count += 1;
                    self.messages += 1;
                }
                ws::OP_TEXT | ws::OP_BINARY => {
                    if self.frag_op != 0 {
                        return Err(Error::Ws("new message inside a fragmented one"));
                    }
                    if payload.len() > self.max_frame {
                        return Err(Error::Ws("message too large"));
                    }
                    self.frag.clear();
                    self.frag.extend_from_slice(payload);
                    self.frag_op = h.opcode;
                    self.frag_copy_bytes += payload.len() as u64;
                }
                ws::OP_CONT => {
                    if self.frag_op == 0 {
                        return Err(Error::Ws("continuation without a first fragment"));
                    }
                    if self.frag.len() + payload.len() > self.max_frame {
                        return Err(Error::Ws("message too large"));
                    }
                    self.frag.extend_from_slice(payload);
                    self.frag_copy_bytes += payload.len() as u64;
                    if h.fin {
                        handler.on_message(id, self.frag_op, &self.frag, meta, pos16(pos));
                        self.frag_op = 0;
                        pos += 1;
                        count += 1;
                        self.messages += 1;
                    }
                }
                ws::OP_PING => {
                    let mut body = [0u8; 125];
                    let len = payload.len();
                    body[..len].copy_from_slice(payload);
                    self.stream.consume(h.frame_len());
                    self.send_frame(ws::OP_PONG, &body[..len])?;
                    self.pings += 1;
                    continue;
                }
                ws::OP_PONG => {}
                ws::OP_CLOSE => return Err(Error::Closed),
                _ => return Err(Error::Ws("unexpected opcode")),
            }
            self.stream.consume(h.frame_len());
        }
        Ok(count)
    }

    /// Queues one masked client frame, encrypted, for sending on the driver's
    /// next `poll` or `flush`. The payload is written once, straight into the
    /// record being built.
    ///
    /// # Errors
    /// The frame is larger than one TLS record.
    pub fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<()> {
        let mask = xorshift(&mut self.rng).to_ne_bytes();
        let mask = [mask[0], mask[1], mask[2], mask[3]];
        self.stream.seal_build(|out| {
            let start = out.len();
            let hl = ws::header_len(payload.len(), true);
            out.resize(start + hl + payload.len(), 0);
            ws::encode_frame(opcode, payload, Some(mask), &mut out[start..])?;
            Ok(())
        })
    }

    /// Bytes still waiting in the receive buffer (diagnostics).
    #[must_use]
    pub const fn buffered(&self) -> usize {
        self.stream.buffered()
    }
}

/// A driver-owned [`WsConn`]: the same send method.
impl ConnMut<'_, WsConn> {
    /// [`WsConn::send_frame`].
    ///
    /// # Errors
    /// As [`WsConn::send_frame`].
    pub fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<()> {
        self.inner().send_frame(opcode, payload)
    }
}

/// A WebSocket connection being set up without blocking: TCP connect, TLS
/// handshake and upgrade, each advanced by the driver's `poll`. Hand it to
/// `Driver::connect`.
pub struct WsDial {
    target: Target,
    buf_size: usize,
    tls: TlsDial,
    up: Option<Box<Upgrade>>,
}

impl WsDial {
    /// A dial to `target`. `buf_size` is the receive buffer, at least [`MIN_BUF`].
    ///
    /// # Errors
    /// The buffer is too small.
    pub fn new(target: Target, cfg: Arc<ClientConfig>, buf_size: usize) -> Result<Self> {
        if buf_size < MIN_BUF {
            return Err(Error::Config(
                "receive buffer must hold at least 8 records".into(),
            ));
        }
        // The path and headers go into the upgrade request verbatim: no CR/LF,
        // valid tokens, and none of the fields the handshake sets itself.
        let headers: Vec<(&str, &str)> = target
            .headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        crate::http::validate_request("GET", &target.path, &headers)?;
        if let Some((k, _)) = headers
            .iter()
            .find(|(k, _)| k.len() >= 14 && k[..14].eq_ignore_ascii_case("sec-websocket-"))
        {
            return Err(Error::Http(format!(
                "header {k} is set by the WebSocket handshake"
            )));
        }
        let sni = target.host.clone();
        Ok(Self {
            target,
            buf_size,
            tls: TlsDial::new(cfg, sni)?,
            up: None,
        })
    }
}

impl DialIo for WsDial {
    type Conn = WsConn;

    fn host(&self) -> &str {
        &self.target.host
    }

    fn port(&self) -> u16 {
        self.target.port
    }

    fn addr(&self) -> Option<SocketAddr> {
        self.target.addr
    }

    fn start(&mut self, addr: SocketAddr) -> Result<()> {
        self.tls.start(addr)
    }

    fn step(&mut self) -> Result<Option<WsConn>> {
        if self.up.is_none() {
            let Some((sock, tls, rest)) = self.tls.step()? else {
                return Ok(None);
            };
            self.up = Some(Box::new(Upgrade::new(
                &self.target,
                sock,
                tls,
                &rest,
                self.buf_size,
            )?));
        }
        let Some(up) = self.up.as_mut() else {
            return Ok(None);
        };
        if !up.step()? {
            return Ok(None);
        }
        self.up.take().map_or(Ok(None), |u| u.finish().map(Some))
    }
}

/// The upgrade request in flight and the response being read.
struct Upgrade {
    sock: TcpStream,
    buf: Box<[u8]>,
    head: usize,
    tail: usize,
    tls: RecordLayer,
    txq: Vec<u8>,
    http: Vec<u8>,
    key: String,
    rng: u64,
    header_len: Option<usize>,
}

impl Upgrade {
    fn new(
        target: &Target,
        sock: TcpStream,
        mut tls: RecordLayer,
        rest: &[u8],
        buf_size: usize,
    ) -> Result<Self> {
        let mut seed = [0u8; 8];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut seed)
            .map_err(|_| Error::Config("no system randomness".into()))?;
        let mut rng = u64::from_le_bytes(seed) | 1;
        let mut nonce = [0u8; 16];
        nonce[..8].copy_from_slice(&xorshift(&mut rng).to_le_bytes());
        nonce[8..].copy_from_slice(&xorshift(&mut rng).to_le_bytes());
        let key = ws::client_key(nonce);
        let headers: Vec<(&str, &str)> = target
            .headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let req = ws::client_request(&target.host_header(), &target.path, &key, &headers);
        let mut txq = Vec::with_capacity(1 << 16);
        tls.seal_app(&req, &mut txq)?;
        // Non-zero fill writes every page now (zeroed memory is mapped lazily),
        // so the first receives do not take page faults.
        let mut buf = vec![1u8; buf_size].into_boxed_slice();
        if rest.len() > buf.len() {
            return Err(Error::Record("handshake leftover larger than the buffer"));
        }
        buf[..rest.len()].copy_from_slice(rest);
        Ok(Self {
            sock,
            buf,
            head: 0,
            tail: rest.len(),
            tls,
            txq,
            http: Vec::new(),
            key,
            rng,
            header_len: None,
        })
    }

    /// `true` once the request is sent and the 101 response parsed.
    fn step(&mut self) -> Result<bool> {
        loop {
            let sent = flush_nonblocking(&mut self.sock, &mut self.txq)?;
            if self.header_len.is_none() {
                while let Some(len) =
                    RecordLayer::complete_record_len(&self.buf[self.head..self.tail])?
                {
                    let rec = &mut self.buf[self.head..self.head + len];
                    if let Opened::AppData(n) = self.tls.open_in_place(rec, &mut self.txq)? {
                        self.http
                            .extend_from_slice(&rec[tls::HEADER_LEN..tls::HEADER_LEN + n]);
                    }
                    self.head += len;
                }
                self.header_len = ws::parse_client_response(&self.http, &self.key)?;
            }
            if self.header_len.is_some() {
                // Control replies (key updates) queued while reading must go out.
                if sent && self.txq.is_empty() {
                    return Ok(true);
                }
                if !sent {
                    return Ok(false);
                }
                continue;
            }
            if self.tail == self.buf.len() {
                return Err(Error::Http("upgrade response too large".into()));
            }
            match self.sock.read(&mut self.buf[self.tail..]) {
                Ok(0) => return Err(Error::Closed),
                Ok(n) => self.tail += n,
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(false),
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn finish(self) -> Result<WsConn> {
        let hl = self
            .header_len
            .ok_or(Error::Http("upgrade not complete".into()))?;
        let Self {
            sock,
            mut buf,
            head,
            tail,
            tls,
            txq,
            http,
            rng,
            ..
        } = self;
        // Put any WebSocket bytes that arrived with the 101 in front of the
        // remaining ciphertext (one-time setup copy).
        let leftover = &http[hl..];
        let ct = tail - head;
        let l = leftover.len();
        if l + ct > buf.len() {
            return Err(Error::Http(
                "upgrade leftover larger than the buffer".into(),
            ));
        }
        buf.copy_within(head..tail, l);
        buf[..l].copy_from_slice(leftover);
        let buf_len = buf.len();
        Ok(WsConn {
            stream: TlsStream::new(sock, buf, l, ct, tls, txq),
            frag: Vec::with_capacity(1 << 20),
            frag_op: 0,
            max_frame: max_frame(buf_len),
            rng,
            messages: 0,
            frag_copy_bytes: 0,
            pings: 0,
        })
    }
}

impl Io for WsConn {
    #[inline]
    fn fd(&self) -> std::os::fd::RawFd {
        self.stream.fd()
    }
    #[inline]
    fn rx_space(&mut self) -> &mut [u8] {
        self.stream.rx_space()
    }
    #[inline]
    fn tx_queue(&mut self) -> &mut Vec<u8> {
        self.stream.tx_queue()
    }
    fn has_buffered_input(&self) -> bool {
        self.stream.has_buffered_input()
    }
}

impl<H: WsHandler> Deliver<H> for WsConn {
    #[inline]
    fn on_received(
        &mut self,
        id: ConnId,
        n: usize,
        meta: &RxMeta,
        handler: &mut H,
    ) -> Result<usize> {
        self.deliver(id, n, meta, handler)
    }
}

/// Frames and reassembled messages must fit the buffer with room for two
/// records of ciphertext behind them.
const fn max_frame(buf_len: usize) -> usize {
    buf_len.saturating_sub(2 * MAX_RECORD)
}

fn pos16(pos: usize) -> u16 {
    u16::try_from(pos).unwrap_or(u16::MAX)
}

const fn xorshift(s: &mut u64) -> u64 {
    let mut x = *s;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *s = x;
    x
}
