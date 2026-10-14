//! Lab sender: a TLS WebSocket server for the two-host benchmark.
//!
//! ```text
//! replay-server --listen 0.0.0.0:9443 --cert-out cert.pem --clients 2 --mode replay --capture capture.bin --speed 1 --duration 600
//! replay-server --listen 0.0.0.0:9443 --cert-out cert.pem --clients 2 --mode burst --burst 50 --gap-us 1000 --duration 600
//! replay-server --listen 0.0.0.0:9443 --cert-out cert.pem --mode echo
//! replay-server --listen 0.0.0.0:9445 --cert-out cert.pem --mode http
//! ```
//!
//! `replay` and `burst` send **every message to every client**, prefixed with
//! `seq u64 | send_ns i64 | capture_ns i64`, alternating which client is written
//! first on even and odd sequence numbers so that write order cancels out in the
//! paired comparison. `echo` answers each client frame with the same payload.
//! `http` is a keep-alive HTTP/1.1 server answering every request with a fixed
//! JSON body the size of a Binance order acknowledgement.
//! The server side is not what is being measured; it busy-spins to keep its own
//! jitter low and identical for every receiver.

use std::error::Error;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::ExitCode;
use std::sync::Arc;

use io_bench::args::Args;
use io_bench::clock::{monotonic_ns, pin_current_thread, realtime_ns};
use io_bench::record::parse_capture;
use io_bench::ws;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection};

type BoxResult<T> = Result<T, Box<dyn Error>>;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("replay-server: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Client {
    sock: TcpStream,
    tls: ServerConnection,
}

impl Client {
    fn write_all_tls(&mut self) -> std::io::Result<()> {
        while self.tls.wants_write() {
            match self.tls.write_tls(&mut self.sock) {
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => std::hint::spin_loop(),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

fn run() -> BoxResult<()> {
    let a = Args::from_env();
    if let Some(cpu) = a.list("cpu", "").first() {
        pin_current_thread(cpu.parse()?)?;
    }
    let names = vec![a.str_or("sni", "lab.local")];
    let rcgen::CertifiedKey { cert, signing_key } = rcgen::generate_simple_self_signed(names)?;
    std::fs::write(a.str_or("cert-out", "cert.pem"), cert.pem())?;
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(signing_key.serialize_der()));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = Arc::new(
        ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(cert.der().to_vec())], key)?,
    );
    let listener = TcpListener::bind(a.str_or("listen", "0.0.0.0:9443"))?;
    let mode = a.str_or("mode", "replay");
    eprintln!("listening on {} mode={mode}", listener.local_addr()?);
    if mode == "http" {
        loop {
            let (sock, peer) = listener.accept()?;
            let cfg = cfg.clone();
            std::thread::spawn(move || {
                if let Err(e) = accept_tls(sock, &cfg).and_then(http) {
                    eprintln!("http {peer}: {e}");
                }
            });
        }
    }
    if mode == "echo" {
        loop {
            let (sock, peer) = listener.accept()?;
            let cfg = cfg.clone();
            std::thread::spawn(move || {
                if let Err(e) = accept(sock, &cfg).and_then(echo) {
                    eprintln!("echo {peer}: {e}");
                }
            });
        }
    }

    let n: usize = a.num_or("clients", 2)?;
    let mut clients = Vec::new();
    while clients.len() < n {
        let (sock, peer) = listener.accept()?;
        clients.push(accept(sock, &cfg)?);
        eprintln!("client {} connected from {peer}", clients.len() - 1);
    }
    for c in &clients {
        c.sock.set_nonblocking(true)?;
    }
    let duration_ns: i64 = a.num_or::<i64>("duration", 600)? * 1_000_000_000;
    let start = monotonic_ns();
    let end = start + duration_ns;
    let mut frame = Vec::with_capacity(1 << 20);
    let mut seq: u64 = 0;
    let mut send =
        |clients: &mut Vec<Client>, payload: &[u8], capture_ns: i64, seq: u64| -> BoxResult<()> {
            frame.clear();
            let send_ns = realtime_ns();
            let mut body = Vec::with_capacity(24 + payload.len());
            body.extend_from_slice(&seq.to_le_bytes());
            body.extend_from_slice(&send_ns.to_le_bytes());
            body.extend_from_slice(&capture_ns.to_le_bytes());
            body.extend_from_slice(payload);
            frame.resize(ws::header_len(body.len(), false) + body.len(), 0);
            ws::encode_frame(ws::OP_BINARY, &body, None, &mut frame)?;
            let order: Vec<usize> = if seq.is_multiple_of(2) {
                (0..clients.len()).collect()
            } else {
                (0..clients.len()).rev().collect()
            };
            for i in order {
                let c = &mut clients[i];
                c.tls.writer().write_all(&frame)?;
                if let Err(e) = c.write_all_tls() {
                    // A receiver finished its run; stop rather than skew the others.
                    return Err(format!("client {i} gone after {seq} messages: {e}").into());
                }
            }
            drain_inputs(clients);
            Ok(())
        };

    match mode.as_str() {
        "replay" => {
            let bytes = std::fs::read(a.req("capture")?)?;
            let msgs = parse_capture(&bytes);
            if msgs.is_empty() {
                return Err("empty capture".into());
            }
            let speed: f64 = a.num_or("speed", 1.0)?;
            'outer: loop {
                let t0 = msgs[0].0;
                let base = monotonic_ns();
                for &(t, payload) in &msgs {
                    let due = base + ((t - t0) as f64 / speed) as i64;
                    while monotonic_ns() < due {
                        std::hint::spin_loop();
                    }
                    if monotonic_ns() >= end {
                        break 'outer;
                    }
                    seq += 1;
                    if let Err(e) = send(&mut clients, payload, t, seq) {
                        eprintln!("stopping: {e}");
                        break 'outer;
                    }
                }
            }
        }
        "burst" => {
            let bytes = std::fs::read(a.req("capture")?)?;
            let msgs = parse_capture(&bytes);
            if msgs.is_empty() {
                return Err("empty capture".into());
            }
            let burst: usize = a.num_or("burst", 50)?;
            let gap_ns: i64 = a.num_or::<i64>("gap-us", 1000)? * 1000;
            let mut k = 0usize;
            let mut next = monotonic_ns();
            while monotonic_ns() < end {
                while monotonic_ns() < next {
                    std::hint::spin_loop();
                }
                for _ in 0..burst {
                    let (t, payload) = msgs[k % msgs.len()];
                    k += 1;
                    seq += 1;
                    if let Err(e) = send(&mut clients, payload, t, seq) {
                        eprintln!("stopping: {e}");
                        return Ok(());
                    }
                }
                next += gap_ns;
            }
        }
        other => return Err(format!("unknown mode {other}").into()),
    }
    eprintln!("sent {seq} messages to {} clients", clients.len());
    for c in &mut clients {
        c.tls.send_close_notify();
        let _ = c.write_all_tls();
    }
    Ok(())
}

/// Reads and discards whatever the clients sent (pongs), without blocking.
fn drain_inputs(clients: &mut [Client]) {
    for c in clients {
        loop {
            match c.tls.read_tls(&mut c.sock) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let _ = c.tls.process_new_packets();
                    let mut sink = [0u8; 4096];
                    while let Ok(n) = c.tls.reader().read(&mut sink) {
                        if n == 0 {
                            break;
                        }
                    }
                }
            }
        }
    }
}

/// TLS handshake plus WebSocket upgrade (blocking).
fn accept(mut sock: TcpStream, cfg: &Arc<ServerConfig>) -> BoxResult<Client> {
    sock.set_nodelay(true)?;
    let mut tls = ServerConnection::new(cfg.clone())?;
    while tls.is_handshaking() {
        tls.complete_io(&mut sock)?;
    }
    let mut req = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        // Drain plaintext rustls already holds first: the upgrade request often
        // arrives in the same read as the client's Finished, inside complete_io.
        loop {
            match tls.reader().read(&mut buf) {
                Ok(0) => break,
                Ok(n) => req.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        if let Some((_, key, _)) = ws::parse_server_request(&req)? {
            tls.writer().write_all(&ws::server_response(&key))?;
            while tls.wants_write() {
                tls.write_tls(&mut sock)?;
            }
            return Ok(Client { sock, tls });
        }
        if tls.read_tls(&mut sock)? == 0 {
            return Err("client closed during upgrade".into());
        }
        tls.process_new_packets()?;
    }
}

/// TLS handshake only (HTTP mode).
fn accept_tls(mut sock: TcpStream, cfg: &Arc<ServerConfig>) -> BoxResult<Client> {
    sock.set_nodelay(true)?;
    let mut tls = ServerConnection::new(cfg.clone())?;
    while tls.is_handshaking() {
        tls.complete_io(&mut sock)?;
    }
    Ok(Client { sock, tls })
}

/// The response body: the size and shape of a Binance `newOrderRespType=ACK`
/// answer plus padding to about 300 bytes.
const ORDER_ACK: &[u8] = br#"{"symbol":"BTCUSDT","orderId":28457911234,"orderListId":-1,"clientOrderId":"x-ergon-bench-0000000001","transactTime":1760400000000,"price":"0.00000000","origQty":"0.00100000","executedQty":"0.00000000","status":"NEW","timeInForce":"GTC","type":"LIMIT","side":"BUY","workingTime":1760400000000}"#;

/// Answers every HTTP/1.1 request with [`ORDER_ACK`], busy-spinning.
fn http(mut c: Client) -> BoxResult<()> {
    c.sock.set_nonblocking(true)?;
    let mut plain: Vec<u8> = Vec::with_capacity(1 << 20);
    let mut tmp = vec![0u8; 1 << 16];
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json;charset=UTF-8\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
        ORDER_ACK.len()
    );
    let mut resp = head.into_bytes();
    resp.extend_from_slice(ORDER_ACK);
    loop {
        // Plaintext rustls already holds first (it can arrive with the handshake).
        loop {
            match c.tls.reader().read(&mut tmp) {
                Ok(0) => return Ok(()),
                Ok(n) => plain.extend_from_slice(&tmp[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        let mut answered = false;
        loop {
            let mut hdrs = [httparse::EMPTY_HEADER; 32];
            let mut req = httparse::Request::new(&mut hdrs);
            let httparse::Status::Complete(n) = req.parse(&plain)? else {
                break;
            };
            let len = req
                .headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case("content-length"))
                .and_then(|h| {
                    std::str::from_utf8(h.value)
                        .ok()?
                        .trim()
                        .parse::<usize>()
                        .ok()
                })
                .unwrap_or(0);
            if plain.len() < n + len {
                break;
            }
            plain.drain(..n + len);
            c.tls.writer().write_all(&resp)?;
            answered = true;
        }
        if answered {
            c.write_all_tls()?;
        }
        match c.tls.read_tls(&mut c.sock) {
            Ok(0) => return Ok(()),
            Ok(_) => {
                c.tls.process_new_packets()?;
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => std::hint::spin_loop(),
            Err(e) => return Err(e.into()),
        }
    }
}

/// Echoes every client frame back as a server frame, busy-spinning.
fn echo(mut c: Client) -> BoxResult<()> {
    c.sock.set_nonblocking(true)?;
    let mut plain: Vec<u8> = Vec::with_capacity(1 << 20);
    let mut tmp = vec![0u8; 1 << 16];
    let mut out = vec![0u8; 1 << 16];
    loop {
        match c.tls.read_tls(&mut c.sock) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::hint::spin_loop();
                continue;
            }
            Err(e) => return Err(e.into()),
        }
        c.tls.process_new_packets()?;
        loop {
            match c.tls.reader().read(&mut tmp) {
                Ok(0) => return Ok(()),
                Ok(n) => plain.extend_from_slice(&tmp[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        let mut consumed = 0;
        while let Some(h) = ws::parse_header(&plain[consumed..])? {
            if plain.len() - consumed < h.frame_len() {
                break;
            }
            let a = consumed + h.header_len;
            let b = consumed + h.frame_len();
            if let Some(m) = h.mask {
                ws::apply_mask(&mut plain[a..b], m);
            }
            match h.opcode {
                ws::OP_CLOSE => return Ok(()),
                ws::OP_TEXT | ws::OP_BINARY => {
                    let n = ws::encode_frame(h.opcode, &plain[a..b], None, &mut out)?;
                    c.tls.writer().write_all(&out[..n])?;
                    c.write_all_tls()?;
                }
                _ => {}
            }
            consumed = b;
        }
        plain.drain(..consumed);
    }
}
