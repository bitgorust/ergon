//! Minimal RFC 6455 codec: client handshake, server handshake (for the lab replay
//! server), frame header parsing and frame encoding.
//!
//! Server-to-client frames are unmasked, so a parsed payload is just a sub-slice of
//! the decrypted receive buffer.

use crate::error::{Error, Result};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Length marker: a 16-bit length follows.
const LEN16: u8 = 0x7E;
/// Length marker: a 64-bit length follows.
const LEN64: u8 = 0x7F;

/// Continuation frame.
pub const OP_CONT: u8 = 0x0;
/// Text frame.
pub const OP_TEXT: u8 = 0x1;
/// Binary frame.
pub const OP_BINARY: u8 = 0x2;
/// Close frame.
pub const OP_CLOSE: u8 = 0x8;
/// Ping frame.
pub const OP_PING: u8 = 0x9;
/// Pong frame.
pub const OP_PONG: u8 = 0xA;

/// A parsed frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// FIN bit.
    pub fin: bool,
    /// Opcode (low four bits).
    pub opcode: u8,
    /// Masking key, when the frame is masked.
    pub mask: Option<[u8; 4]>,
    /// Header length in bytes, including the masking key.
    pub header_len: usize,
    /// Payload length in bytes.
    pub payload_len: usize,
}

impl FrameHeader {
    /// Total frame length (header plus payload).
    #[must_use]
    pub const fn frame_len(&self) -> usize {
        self.header_len + self.payload_len
    }
}

/// Parses a frame header. Returns `Ok(None)` when more bytes are needed.
///
/// # Errors
/// Reserved bits set, an unknown opcode, an oversized or fragmented control frame,
/// or a 64-bit length with the top bit set.
pub fn parse_header(buf: &[u8]) -> Result<Option<FrameHeader>> {
    if buf.len() < 2 {
        return Ok(None);
    }
    let b0 = buf[0];
    let b1 = buf[1];
    if b0 & 0x70 != 0 {
        return Err(Error::Ws(
            "reserved bits set (no extensions were negotiated)",
        ));
    }
    let fin = b0 & 0x80 != 0;
    let opcode = b0 & 0x0F;
    match opcode {
        OP_CONT | OP_TEXT | OP_BINARY => {}
        OP_CLOSE | OP_PING | OP_PONG => {
            if !fin {
                return Err(Error::Ws("fragmented control frame"));
            }
            if b1 & 0x7F > 125 {
                return Err(Error::Ws("control frame longer than 125 bytes"));
            }
        }
        _ => return Err(Error::Ws("unknown opcode")),
    }
    let masked = b1 & 0x80 != 0;
    let (payload_len, mut header_len) = match b1 & 0x7F {
        126 => {
            if buf.len() < 4 {
                return Ok(None);
            }
            (usize::from(u16::from_be_bytes([buf[2], buf[3]])), 4)
        }
        127 => {
            if buf.len() < 10 {
                return Ok(None);
            }
            let mut be = [0u8; 8];
            be.copy_from_slice(&buf[2..10]);
            let len = u64::from_be_bytes(be);
            if len >> 63 != 0 {
                return Err(Error::Ws("64-bit length with the top bit set"));
            }
            let len = usize::try_from(len).map_err(|_| Error::Ws("frame too large"))?;
            (len, 10)
        }
        n => (usize::from(n), 2),
    };
    let mask = if masked {
        if buf.len() < header_len + 4 {
            return Ok(None);
        }
        let mut m = [0u8; 4];
        m.copy_from_slice(&buf[header_len..header_len + 4]);
        header_len += 4;
        Some(m)
    } else {
        None
    };
    Ok(Some(FrameHeader {
        fin,
        opcode,
        mask,
        header_len,
        payload_len,
    }))
}

/// XORs `data` with the masking key, starting at key offset 0.
pub fn apply_mask(data: &mut [u8], mask: [u8; 4]) {
    let word = u32::from_ne_bytes(mask);
    let (chunks, rest) = data.as_chunks_mut::<4>();
    for c in chunks {
        *c = (u32::from_ne_bytes(*c) ^ word).to_ne_bytes();
    }
    for (i, b) in rest.iter_mut().enumerate() {
        *b ^= mask[i];
    }
}

/// Length of a frame header for `payload_len` bytes.
#[must_use]
pub const fn header_len(payload_len: usize, masked: bool) -> usize {
    let base = if payload_len < 126 {
        2
    } else if payload_len <= 0xFFFF {
        4
    } else {
        10
    };
    if masked { base + 4 } else { base }
}

/// Encodes one FIN frame into `out`. Returns the number of bytes written.
///
/// # Errors
/// `out` is too small.
pub fn encode_frame(
    opcode: u8,
    payload: &[u8],
    mask: Option<[u8; 4]>,
    out: &mut [u8],
) -> Result<usize> {
    let n = payload.len();
    let hl = header_len(n, mask.is_some());
    if out.len() < hl + n {
        return Err(Error::Ws("output buffer too small for frame"));
    }
    out[0] = 0x80 | opcode;
    let mbit = if mask.is_some() { 0x80 } else { 0x00 };
    let mut pos = 2;
    if let Ok(small @ 0..126) = u8::try_from(n) {
        out[1] = mbit | small;
    } else if let Ok(medium) = u16::try_from(n) {
        out[1] = mbit | LEN16;
        out[2..4].copy_from_slice(&medium.to_be_bytes());
        pos = 4;
    } else {
        out[1] = mbit | LEN64;
        out[2..10].copy_from_slice(&(n as u64).to_be_bytes());
        pos = 10;
    }
    if let Some(m) = mask {
        out[pos..pos + 4].copy_from_slice(&m);
        pos += 4;
    }
    out[pos..pos + n].copy_from_slice(payload);
    if let Some(m) = mask {
        apply_mask(&mut out[pos..pos + n], m);
    }
    Ok(pos + n)
}

/// `Sec-WebSocket-Accept` value for a client key.
#[must_use]
pub fn accept_key(client_key: &str) -> String {
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
    ctx.update(client_key.as_bytes());
    ctx.update(GUID.as_bytes());
    B64.encode(ctx.finish().as_ref())
}

/// Base64 encoding of a 16-byte client nonce.
#[must_use]
pub fn client_key(nonce: [u8; 16]) -> String {
    B64.encode(nonce)
}

/// Builds the client upgrade request. No extensions are offered, so the server
/// never compresses.
#[must_use]
pub fn client_request(
    host_header: &str,
    path: &str,
    key: &str,
    extra_headers: &[(&str, &str)],
) -> Vec<u8> {
    let mut s = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_header}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nUser-Agent: ergon-io-bench\r\n"
    );
    for (k, v) in extra_headers {
        s.push_str(k);
        s.push_str(": ");
        s.push_str(v);
        s.push_str("\r\n");
    }
    s.push_str("\r\n");
    s.into_bytes()
}

/// Parses the server's upgrade response. Returns the header length once the full
/// header block has arrived, or `Ok(None)` if more bytes are needed.
///
/// # Errors
/// Not a 101, a wrong accept key, or a negotiated extension.
pub fn parse_client_response(buf: &[u8], key: &str) -> Result<Option<usize>> {
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut resp = httparse::Response::new(&mut headers);
    let n = match resp.parse(buf).map_err(|e| Error::Http(e.to_string()))? {
        httparse::Status::Partial => return Ok(None),
        httparse::Status::Complete(n) => n,
    };
    if resp.code != Some(101) {
        let body = String::from_utf8_lossy(&buf[n..buf.len().min(n + 300)]).into_owned();
        return Err(Error::Http(format!(
            "status {:?} {:?}: {body}",
            resp.code, resp.reason
        )));
    }
    let want = accept_key(key);
    let mut accepted = false;
    for h in resp.headers.iter() {
        if h.name.eq_ignore_ascii_case("sec-websocket-accept") {
            accepted = h.value == want.as_bytes();
        }
        if h.name.eq_ignore_ascii_case("sec-websocket-extensions") && !h.value.is_empty() {
            return Err(Error::Http(
                "server negotiated an extension we did not offer".into(),
            ));
        }
    }
    if !accepted {
        return Err(Error::Http("missing or wrong Sec-WebSocket-Accept".into()));
    }
    Ok(Some(n))
}

/// Parses a client upgrade request (lab replay server). Returns
/// `(header_len, sec_websocket_key, path)` once complete.
///
/// # Errors
/// Malformed request or missing key.
pub fn parse_server_request(buf: &[u8]) -> Result<Option<(usize, String, String)>> {
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut req = httparse::Request::new(&mut headers);
    let n = match req.parse(buf).map_err(|e| Error::Http(e.to_string()))? {
        httparse::Status::Partial => return Ok(None),
        httparse::Status::Complete(n) => n,
    };
    let key = req
        .headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("sec-websocket-key"))
        .map(|h| String::from_utf8_lossy(h.value).into_owned())
        .ok_or_else(|| Error::Http("missing Sec-WebSocket-Key".into()))?;
    Ok(Some((n, key, req.path.unwrap_or("/").to_owned())))
}

/// The server's 101 response for a client key.
#[must_use]
pub fn server_response(client_key: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(client_key)
    )
    .into_bytes()
}
