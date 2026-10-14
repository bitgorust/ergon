//! TLS 1.3: rustls does the handshake; this module then owns the record layer and
//! decrypts each record **in place** in the caller's receive buffer.
//!
//! rustls 0.23's own read paths (buffered and unbuffered) both pop every decrypted
//! record out of an internal `ChunkVecBuffer` (one allocation and one copy per
//! record). To keep exactly one copy per byte (kernel to our buffer) we use the
//! kTLS-style API: drive `UnbufferedClientConnection` until the handshake reaches
//! `WriteTraffic`, call `dangerous_into_kernel_connection`, and run AES-GCM /
//! ChaCha20-Poly1305 ourselves with ring. The returned `KernelConnection` is still
//! used for post-handshake messages (session tickets and key updates).

use std::io::{Read, Write};
use std::sync::Arc;

use ring::aead::{
    AES_128_GCM, AES_256_GCM, Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey,
};
use rustls::client::{ClientConnectionData, UnbufferedClientConnection};
use rustls::kernel::KernelConnection;
use rustls::pki_types::ServerName;
use rustls::unbuffered::{ConnectionState, UnbufferedStatus};
use rustls::{ClientConfig, ConnectionTrafficSecrets, RootCertStore};

use crate::error::{Error, Result};

/// TLS record header length.
pub const HEADER_LEN: usize = 5;
/// AEAD tag length for every TLS 1.3 suite.
pub const TAG_LEN: usize = 16;
/// Largest TLS 1.3 record on the wire: 2^14 plaintext + content type + padding + tag.
pub const MAX_RECORD: usize = HEADER_LEN + (1 << 14) + 256;

const CT_ALERT: u8 = 21;
const CT_HANDSHAKE: u8 = 22;
/// Application-data content type.
pub const CT_APPDATA: u8 = 23;
const HS_NEW_SESSION_TICKET: u8 = 4;
const HS_KEY_UPDATE: u8 = 24;

/// A rustls client config restricted to TLS 1.3, with secret extraction enabled.
///
/// # Errors
/// The ring provider rejects the protocol selection (it does not).
pub fn client_config(roots: RootCertStore) -> Result<Arc<ClientConfig>> {
    client_config_with(
        roots,
        Arc::new(rustls::crypto::ring::default_provider()),
        true,
    )
}

/// A TLS 1.3 client config with an explicit crypto provider.
///
/// # Errors
/// The provider rejects TLS 1.3.
pub fn client_config_with(
    roots: RootCertStore,
    provider: Arc<rustls::crypto::CryptoProvider>,
    extract_secrets: bool,
) -> Result<Arc<ClientConfig>> {
    let mut cfg = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots)
        .with_no_client_auth();
    cfg.enable_secret_extraction = extract_secrets;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(cfg))
}

/// Mozilla's root store (feature `webpki-roots`).
#[cfg(feature = "webpki-roots")]
#[must_use]
pub fn webpki_roots() -> RootCertStore {
    RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    }
}

struct Direction {
    key: LessSafeKey,
    iv: [u8; 12],
    seq: u64,
}

impl Direction {
    fn new(seq: u64, secrets: &ConnectionTrafficSecrets) -> Result<Self> {
        let (alg, key, iv) = match secrets {
            ConnectionTrafficSecrets::Aes128Gcm { key, iv } => {
                (&AES_128_GCM, key.as_ref(), iv.as_ref())
            }
            ConnectionTrafficSecrets::Aes256Gcm { key, iv } => {
                (&AES_256_GCM, key.as_ref(), iv.as_ref())
            }
            ConnectionTrafficSecrets::Chacha20Poly1305 { key, iv } => {
                (&CHACHA20_POLY1305, key.as_ref(), iv.as_ref())
            }
            _ => return Err(Error::Record("unsupported cipher suite")),
        };
        let key =
            LessSafeKey::new(UnboundKey::new(alg, key).map_err(|_| Error::Record("bad key"))?);
        let mut ivb = [0u8; 12];
        if iv.len() != 12 {
            return Err(Error::Record("unexpected IV length"));
        }
        ivb.copy_from_slice(iv);
        Ok(Self { key, iv: ivb, seq })
    }

    fn next_nonce(&mut self) -> Nonce {
        let mut n = self.iv;
        for (b, s) in n[4..].iter_mut().zip(self.seq.to_be_bytes()) {
            *b ^= s;
        }
        self.seq += 1;
        Nonce::assume_unique_for_key(n)
    }
}

/// Result of opening one record in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opened {
    /// Application data at `record[HEADER_LEN..HEADER_LEN + len]`.
    AppData(usize),
    /// A post-handshake message was consumed; `reply` bytes (if any) were queued.
    Control,
}

/// The TLS 1.3 record layer after the handshake.
pub struct RecordLayer {
    rx: Direction,
    tx: Direction,
    kconn: KernelConnection<ClientConnectionData>,
    /// Number of records opened.
    pub records_in: u64,
}

impl RecordLayer {
    /// Length of the complete record at the front of `buf`, if it has fully arrived.
    ///
    /// # Errors
    /// The header is not a TLS 1.3 application-data record header.
    pub fn complete_record_len(buf: &[u8]) -> Result<Option<usize>> {
        if buf.len() < HEADER_LEN {
            return Ok(None);
        }
        if buf[0] != CT_APPDATA && buf[0] != CT_ALERT {
            return Err(Error::Record("unexpected outer content type"));
        }
        let len = usize::from(u16::from_be_bytes([buf[3], buf[4]]));
        if len > MAX_RECORD - HEADER_LEN {
            return Err(Error::Record("record too long"));
        }
        let total = HEADER_LEN + len;
        Ok((buf.len() >= total).then_some(total))
    }

    /// Decrypts one complete record in place. On `AppData(n)` the plaintext is at
    /// `record[HEADER_LEN..HEADER_LEN + n]`. Post-handshake handshake messages are
    /// handled here; any reply (a key update) is appended to `tx_out` already
    /// encrypted.
    ///
    /// # Errors
    /// AEAD failure, an alert, or a malformed post-handshake message.
    pub fn open_in_place(&mut self, record: &mut [u8], tx_out: &mut Vec<u8>) -> Result<Opened> {
        let mut aad = [0u8; HEADER_LEN];
        aad.copy_from_slice(&record[..HEADER_LEN]);
        let nonce = self.rx.next_nonce();
        let plain = self
            .rx
            .key
            .open_in_place(nonce, Aad::from(aad), &mut record[HEADER_LEN..])
            .map_err(|_| Error::Record("AEAD open failed"))?;
        self.records_in += 1;
        // Strip zero padding; the last non-zero byte is the real content type.
        let mut end = plain.len();
        while end > 0 && plain[end - 1] == 0 {
            end -= 1;
        }
        if end == 0 {
            return Err(Error::Record("record has no content type"));
        }
        let inner = plain[end - 1];
        let n = end - 1;
        match inner {
            CT_APPDATA => Ok(Opened::AppData(n)),
            CT_HANDSHAKE => {
                let body = &plain[..n];
                self.handle_handshake(body, tx_out)?;
                Ok(Opened::Control)
            }
            CT_ALERT => Err(Error::Closed),
            _ => Err(Error::Record("unexpected inner content type")),
        }
    }

    fn handle_handshake(&mut self, mut msgs: &[u8], tx_out: &mut Vec<u8>) -> Result<()> {
        while !msgs.is_empty() {
            if msgs.len() < 4 {
                return Err(Error::Record("truncated handshake message"));
            }
            let typ = msgs[0];
            let len = usize::from(msgs[1]) << 16 | usize::from(msgs[2]) << 8 | usize::from(msgs[3]);
            if msgs.len() < 4 + len {
                return Err(Error::Record("handshake message split across records"));
            }
            let body = &msgs[4..4 + len];
            match typ {
                HS_NEW_SESSION_TICKET => self.kconn.handle_new_session_ticket(body)?,
                HS_KEY_UPDATE => {
                    let (seq, secrets) = self.kconn.update_rx_secret()?;
                    self.rx = Direction::new(seq, &secrets)?;
                    if body.first() == Some(&1) {
                        // update_requested: answer with our own KeyUpdate under the
                        // old key, then switch our sending key.
                        self.seal(CT_HANDSHAKE, &[HS_KEY_UPDATE, 0, 0, 1, 0], tx_out)?;
                        let (seq, secrets) = self.kconn.update_tx_secret()?;
                        self.tx = Direction::new(seq, &secrets)?;
                    }
                }
                _ => return Err(Error::Record("unexpected post-handshake message")),
            }
            msgs = &msgs[4 + len..];
        }
        Ok(())
    }

    /// Encrypts `payload` as one record of `inner_type` and appends it to `out`.
    ///
    /// # Errors
    /// The payload is larger than one record.
    pub fn seal(&mut self, inner_type: u8, payload: &[u8], out: &mut Vec<u8>) -> Result<()> {
        if payload.len() > 1 << 14 {
            return Err(Error::Record("payload larger than one record"));
        }
        let start = out.len();
        let ct_len = payload.len() + 1 + TAG_LEN;
        let len = u16::try_from(ct_len).map_err(|_| Error::Record("record too long"))?;
        out.extend_from_slice(&[CT_APPDATA, 0x03, 0x03]);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(payload);
        out.push(inner_type);
        let mut aad = [0u8; HEADER_LEN];
        aad.copy_from_slice(&out[start..start + HEADER_LEN]);
        let nonce = self.tx.next_nonce();
        let tag = self
            .tx
            .key
            .seal_in_place_separate_tag(nonce, Aad::from(aad), &mut out[start + HEADER_LEN..])
            .map_err(|_| Error::Record("AEAD seal failed"))?;
        out.extend_from_slice(tag.as_ref());
        Ok(())
    }

    /// Builds a record in place at the end of `out`: `build` appends the plaintext
    /// directly after the header, so the payload is written exactly once.
    ///
    /// # Errors
    /// `build` fails or produces more than one record of plaintext.
    pub fn seal_build(
        &mut self,
        out: &mut Vec<u8>,
        build: impl FnOnce(&mut Vec<u8>) -> Result<()>,
    ) -> Result<()> {
        let start = out.len();
        out.extend_from_slice(&[CT_APPDATA, 0x03, 0x03, 0, 0]);
        if let Err(e) = build(out) {
            // Nothing half-built may reach the wire.
            out.truncate(start);
            return Err(e);
        }
        let plain_len = out.len() - start - HEADER_LEN;
        if plain_len > 1 << 14 {
            out.truncate(start);
            return Err(Error::Record("payload larger than one record"));
        }
        out.push(CT_APPDATA);
        let len =
            u16::try_from(plain_len + 1 + TAG_LEN).map_err(|_| Error::Record("record too long"))?;
        out[start + 3..start + 5].copy_from_slice(&len.to_be_bytes());
        let mut aad = [0u8; HEADER_LEN];
        aad.copy_from_slice(&out[start..start + HEADER_LEN]);
        let nonce = self.tx.next_nonce();
        let tag = self
            .tx
            .key
            .seal_in_place_separate_tag(nonce, Aad::from(aad), &mut out[start + HEADER_LEN..])
            .map_err(|_| Error::Record("AEAD seal failed"))?;
        out.extend_from_slice(tag.as_ref());
        Ok(())
    }

    /// Encrypts application data.
    ///
    /// # Errors
    /// See [`RecordLayer::seal`].
    pub fn seal_app(&mut self, payload: &[u8], out: &mut Vec<u8>) -> Result<()> {
        self.seal(CT_APPDATA, payload, out)
    }
}

/// Runs the TLS 1.3 handshake over a blocking stream and returns the record layer
/// plus any bytes already read that belong to records after the handshake.
///
/// # Errors
/// I/O failure, a TLS error, or a peer that closes during the handshake.
pub fn handshake<S: Read + Write>(
    stream: &mut S,
    config: Arc<ClientConfig>,
    server_name: &str,
) -> Result<(RecordLayer, Vec<u8>)> {
    let mut hs = TlsHandshake::new(config, server_name)?;
    while !hs.step(stream)? {}
    hs.finish()
}

/// Like [`handshake`] but returns the raw traffic secrets, so callers (the TLS
/// micro-benchmark) can drive a different AEAD implementation.
///
/// # Errors
/// I/O failure, a TLS error, or a peer that closes during the handshake.
pub fn handshake_raw<S: Read + Write>(
    stream: &mut S,
    config: Arc<ClientConfig>,
    server_name: &str,
) -> Result<(
    rustls::ExtractedSecrets,
    KernelConnection<ClientConnectionData>,
    Vec<u8>,
)> {
    let mut hs = TlsHandshake::new(config, server_name)?;
    while !hs.step(stream)? {}
    hs.finish_raw()
}

/// A client handshake advanced without blocking: [`TlsHandshake::step`] reads
/// and writes what the socket accepts and returns at the first `WouldBlock`.
pub struct TlsHandshake {
    conn: UnbufferedClientConnection,
    inc: Vec<u8>,
    inc_len: usize,
    out: Vec<u8>,
    out_len: usize,
    out_sent: usize,
    done: bool,
}

enum Next {
    Read,
    Done,
    Continue,
}

impl TlsHandshake {
    /// Starts a handshake for `server_name`.
    ///
    /// # Errors
    /// Invalid server name or configuration.
    pub fn new(config: Arc<ClientConfig>, server_name: &str) -> Result<Self> {
        let name = ServerName::try_from(server_name.to_owned())
            .map_err(|e| Error::Config(e.to_string()))?;
        Ok(Self {
            conn: UnbufferedClientConnection::new(config, name)?,
            inc: vec![0u8; 1 << 16],
            inc_len: 0,
            out: vec![0u8; 1 << 16],
            out_len: 0,
            out_sent: 0,
            done: false,
        })
    }

    /// Advances as far as the socket allows. Returns `true` once the handshake
    /// is complete and every handshake byte has been sent.
    ///
    /// # Errors
    /// I/O failure (other than `WouldBlock`), a TLS error, or a peer close.
    pub fn step<S: Read + Write>(&mut self, stream: &mut S) -> Result<bool> {
        loop {
            if !self.flush(stream)? {
                return Ok(false);
            }
            if self.done {
                return Ok(true);
            }
            let UnbufferedStatus { discard, state } =
                self.conn.process_tls_records(&mut self.inc[..self.inc_len]);
            let next = match state? {
                ConnectionState::EncodeTlsData(mut e) => {
                    self.out_len += e
                        .encode(&mut self.out[self.out_len..])
                        .map_err(|e| Error::Http(format!("encode: {e}")))?;
                    Next::Continue
                }
                ConnectionState::TransmitTlsData(t) => {
                    // The bytes are ours now; `flush` sends them.
                    t.done();
                    Next::Continue
                }
                ConnectionState::BlockedHandshake => Next::Read,
                ConnectionState::WriteTraffic(_) => Next::Done,
                ConnectionState::PeerClosed | ConnectionState::Closed => {
                    return Err(Error::Closed);
                }
                other => {
                    return Err(Error::Http(format!("unexpected handshake state {other:?}")));
                }
            };
            if discard > 0 {
                self.inc.copy_within(discard..self.inc_len, 0);
                self.inc_len -= discard;
            }
            match next {
                Next::Continue => {}
                Next::Done => self.done = true,
                Next::Read => {
                    if self.inc_len == self.inc.len() {
                        return Err(Error::Record("handshake buffer full"));
                    }
                    match stream.read(&mut self.inc[self.inc_len..]) {
                        Ok(0) => return Err(Error::Closed),
                        Ok(n) => self.inc_len += n,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
                        Err(e) => return Err(e.into()),
                    }
                }
            }
        }
    }

    /// Sends pending handshake bytes; `false` if the socket would block first.
    fn flush<S: Write>(&mut self, stream: &mut S) -> Result<bool> {
        while self.out_sent < self.out_len {
            match stream.write(&self.out[self.out_sent..self.out_len]) {
                Ok(0) => return Err(Error::Closed),
                Ok(n) => self.out_sent += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
                Err(e) => return Err(e.into()),
            }
        }
        self.out_len = 0;
        self.out_sent = 0;
        Ok(true)
    }

    /// The record layer and any bytes received after the handshake, once
    /// [`TlsHandshake::step`] has returned `true`.
    ///
    /// # Errors
    /// The handshake is not complete or negotiated an unsupported suite.
    pub fn finish(self) -> Result<(RecordLayer, Vec<u8>)> {
        let (secrets, kconn, rest) = self.finish_raw()?;
        let layer = RecordLayer {
            rx: Direction::new(secrets.rx.0, &secrets.rx.1)?,
            tx: Direction::new(secrets.tx.0, &secrets.tx.1)?,
            kconn,
            records_in: 0,
        };
        Ok((layer, rest))
    }

    /// Like [`TlsHandshake::finish`] with the raw secrets.
    ///
    /// # Errors
    /// The handshake is not complete.
    pub fn finish_raw(
        mut self,
    ) -> Result<(
        rustls::ExtractedSecrets,
        KernelConnection<ClientConnectionData>,
        Vec<u8>,
    )> {
        if !self.done {
            return Err(Error::Record("handshake not complete"));
        }
        let (secrets, kconn) = self.conn.dangerous_into_kernel_connection()?;
        self.inc.truncate(self.inc_len);
        Ok((secrets, kconn, self.inc))
    }
}
