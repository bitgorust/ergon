//! Lab sender: a TLS WebSocket server for the two-host benchmark.
//!
//! ```text
//! replay-server --listen 0.0.0.0:9443 --cert-out cert.pem --clients 2 --mode replay --capture capture.bin --speed 1 --duration 600
//! replay-server --listen 0.0.0.0:9443 --cert-out cert.pem --clients 2 --mode burst --burst 50 --gap-us 1000 --duration 600
//! replay-server --listen 0.0.0.0:9443 --cert-out cert.pem --clients 100 --mode mixed --duration 600 \
//!     [--streams 1024 --trickle-min-us 1000 --trickle-max-us 10000 --burst-every-us 2000 --burst-slots 10 --burst 10 --seed 42]
//! replay-server --listen 0.0.0.0:9443 --cert-out cert.pem --mode echo
//! replay-server --listen 0.0.0.0:9445 --cert-out cert.pem --mode http
//! ```
//!
//! `replay` and `burst` send **every message to every client**, prefixed with
//! `seq u64 | send_ns i64 | capture_ns i64`, alternating which client is written
//! first on even and odd sequence numbers so that write order cancels out in the
//! paired comparison. `mixed` gives each connection its own stream: a client
//! names its slot in its upgrade path (`/slot/<n>`), and every slot gets a
//! trickle at its own random interval plus, every few milliseconds, bursts to a
//! random subset of slots (`io_bench::mixed`, the same for a seed); a message
//! goes to every client of its slot. With `--streams N` the schedule has N
//! streams sharing the connections as on a combined-stream socket (stream s on
//! connection s mod connections); the stream number is sent in place of the
//! capture time. `echo` answers each client frame with the same payload.
//! `http` is a keep-alive HTTP/1.1 server answering every request with a fixed
//! JSON body the size of a Binance order acknowledgement.
//! The server side is not what is being measured; it busy-spins to keep its own
//! jitter low and identical for every receiver. `--busy-poll US` sets
//! `SO_BUSY_POLL` on every accepted socket, so each read polls the NIC queue
//! itself instead of waiting for an interrupt on another CPU (which may be
//! waking from a deep idle state). With `--stalls-dir DIR` the
//! echo and HTTP loops log their slow iterations (`DIR/server-<mode>-<port>.txt`
//! per connection), so a slow round trip can be traced to the server.

use std::error::Error;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::ExitCode;
use std::sync::Arc;

use io_bench::args::Args;
use io_bench::clock::{monotonic_ns, pin_current_thread, realtime_ns};
use io_bench::mixed::{Schedule, Shape};
use io_bench::record::parse_capture;
use io_bench::stalls::StallLog;
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
    /// The slot named in the upgrade path (`/slot/<n>`), if any.
    slot: Option<usize>,
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
    let stalls_dir = a
        .list("stalls-dir", "")
        .first()
        .map(std::path::PathBuf::from);
    let stall_us: f64 = a.num_or("stall-us", 20.0)?;
    let busy_poll_us: i32 = a.num_or("busy-poll", 0)?;
    if mode == "http" || mode == "echo" {
        loop {
            let (sock, peer) = listener.accept()?;
            set_busy_poll(&sock, busy_poll_us)?;
            let cfg = cfg.clone();
            let dir = stalls_dir.clone();
            let mode = mode.clone();
            std::thread::spawn(move || {
                let mut log = StallLog::new(stall_us, 1 << 20);
                let r = if mode == "http" {
                    accept_tls(sock, &cfg).and_then(|c| http(c, &mut log))
                } else {
                    accept(sock, &cfg).and_then(|c| echo(c, &mut log))
                };
                if let Err(e) = r {
                    eprintln!("{mode} {peer}: {e}");
                }
                if let Some(d) = dir {
                    let path = d.join(format!("server-{mode}-{}.txt", peer.port()));
                    if let Err(e) = log.write(&path) {
                        eprintln!("{}: {e}", path.display());
                    }
                }
            });
        }
    }

    let n: usize = a.num_or("clients", 2)?;
    let mut clients = Vec::new();
    while clients.len() < n {
        let (sock, peer) = listener.accept()?;
        set_busy_poll(&sock, busy_poll_us)?;
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
    // To every client (`only` = None, then pongs are drained), or to the given ones.
    let mut send = |clients: &mut Vec<Client>,
                    only: Option<&[usize]>,
                    payload: &[u8],
                    capture_ns: i64,
                    seq: u64|
     -> BoxResult<()> {
        frame.clear();
        let send_ns = realtime_ns();
        let mut body = Vec::with_capacity(24 + payload.len());
        body.extend_from_slice(&seq.to_le_bytes());
        body.extend_from_slice(&send_ns.to_le_bytes());
        body.extend_from_slice(&capture_ns.to_le_bytes());
        body.extend_from_slice(payload);
        frame.resize(ws::header_len(body.len(), false) + body.len(), 0);
        ws::encode_frame(ws::OP_BINARY, &body, None, &mut frame)?;
        let mut order: Vec<usize> =
            only.map_or_else(|| (0..clients.len()).collect(), <[usize]>::to_vec);
        if !seq.is_multiple_of(2) {
            order.reverse();
        }
        for i in order {
            let c = &mut clients[i];
            c.tls.writer().write_all(&frame)?;
            if let Err(e) = c.write_all_tls() {
                // A receiver finished its run; stop rather than skew the others.
                return Err(format!("client {i} gone after {seq} messages: {e}").into());
            }
        }
        if only.is_none() {
            drain_inputs(clients);
        }
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
                    if let Err(e) = send(&mut clients, None, payload, t, seq) {
                        eprintln!("stopping: {e}");
                        break 'outer;
                    }
                }
            }
        }
        "burst" => {
            // Without a capture: one synthetic trade message of Binance's size.
            let bytes = match a.list("capture", "").first() {
                Some(p) => std::fs::read(p)?,
                None => synthetic_capture(),
            };
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
                    if let Err(e) = send(&mut clients, None, payload, t, seq) {
                        eprintln!("stopping: {e}");
                        return Ok(());
                    }
                }
                next += gap_ns;
            }
        }
        "mixed" => {
            let slots = clients
                .iter()
                .filter_map(|c| c.slot)
                .max()
                .map_or(0, |m| m + 1);
            if slots == 0 {
                return Err("mixed: no client named a slot (/slot/<n>)".into());
            }
            // Streams (one schedule slot each) share the connections like a
            // combined-stream socket: stream s goes to connection s mod slots.
            let streams: usize = a.num_or("streams", slots)?;
            let mut groups: Vec<Vec<usize>> = vec![Vec::new(); slots];
            for (i, c) in clients.iter().enumerate() {
                if let Some(s) = c.slot {
                    groups[s].push(i);
                }
            }
            let shape = Shape {
                slots: streams.max(1),
                trickle_min_us: a.num_or("trickle-min-us", 1_000)?,
                trickle_max_us: a.num_or("trickle-max-us", 10_000)?,
                burst_every_us: a.num_or("burst-every-us", 2_000)?,
                burst_slots: a.num_or("burst-slots", 10)?,
                burst_len: a.num_or("burst", 10)?,
                seed: a.num_or("seed", 42)?,
            };
            let bytes = synthetic_capture();
            let msgs = parse_capture(&bytes);
            let &(_, payload) = msgs.first().ok_or("empty capture")?;
            let mut last_drain = start;
            for m in Schedule::new(shape) {
                let due = start + m.at_ns;
                if due >= end {
                    break;
                }
                while monotonic_ns() < due {
                    std::hint::spin_loop();
                }
                seq += 1;
                // The stream rides in the capture-time field (`src2_ns`).
                let stream = i64::try_from(m.slot).unwrap_or(0);
                if let Err(e) = send(
                    &mut clients,
                    Some(&groups[m.slot % slots]),
                    payload,
                    stream,
                    seq,
                ) {
                    eprintln!("stopping: {e}");
                    break;
                }
                // The clients send nothing but pongs and closes: drain now and then.
                if monotonic_ns() - last_drain > 10_000_000 {
                    drain_inputs(&mut clients);
                    last_drain = monotonic_ns();
                }
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

/// A one-message capture: a Binance spot trade event (about 200 bytes) in the
/// capture format (`capture_ns i64 | len u32 | payload`).
fn synthetic_capture() -> Vec<u8> {
    let payload: &[u8] = br#"{"stream":"btcusdt@trade","data":{"e":"trade","E":1760400000000000,"s":"BTCUSDT","t":5123456789,"p":"62431.17000000","q":"0.00120000","T":1760400000000000,"m":true,"M":true}}"#;
    let mut out = Vec::with_capacity(12 + payload.len());
    out.extend_from_slice(&0i64.to_le_bytes());
    out.extend_from_slice(&u32::try_from(payload.len()).unwrap_or(0).to_le_bytes());
    out.extend_from_slice(payload);
    out
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
/// Sets `SO_BUSY_POLL` to `us` on `sock` (nothing when 0, or off Linux).
fn set_busy_poll(sock: &TcpStream, us: i32) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    if us > 0 {
        use std::os::fd::AsRawFd;
        let len = libc::socklen_t::try_from(std::mem::size_of::<i32>()).unwrap_or(4);
        // SAFETY: a valid descriptor, and an int option of the given length.
        let r = unsafe {
            libc::setsockopt(
                sock.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_BUSY_POLL,
                (&raw const us).cast(),
                len,
            )
        };
        if r != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (sock, us);
    Ok(())
}

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
        if let Some((_, key, path)) = ws::parse_server_request(&req)? {
            tls.writer().write_all(&ws::server_response(&key))?;
            while tls.wants_write() {
                tls.write_tls(&mut sock)?;
            }
            let slot = path.strip_prefix("/slot/").and_then(|n| n.parse().ok());
            return Ok(Client { sock, tls, slot });
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
    Ok(Client {
        sock,
        tls,
        slot: None,
    })
}

/// The response body: the size and shape of a Binance `newOrderRespType=ACK`
/// answer plus padding to about 300 bytes.
const ORDER_ACK: &[u8] = br#"{"symbol":"BTCUSDT","orderId":28457911234,"orderListId":-1,"clientOrderId":"x-ergon-bench-0000000001","transactTime":1760400000000,"price":"0.00000000","origQty":"0.00100000","executedQty":"0.00000000","status":"NEW","timeInForce":"GTC","type":"LIMIT","side":"BUY","workingTime":1760400000000}"#;

/// Answers every HTTP/1.1 request with [`ORDER_ACK`], busy-spinning.
fn http(mut c: Client, log: &mut StallLog) -> BoxResult<()> {
    c.sock.set_nonblocking(true)?;
    let mut plain: Vec<u8> = Vec::with_capacity(1 << 20);
    let mut tmp = vec![0u8; 1 << 16];
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json;charset=UTF-8\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
        ORDER_ACK.len()
    );
    let mut resp = head.into_bytes();
    resp.extend_from_slice(ORDER_ACK);
    let mut served = 0u64;
    log.start(0);
    loop {
        log.tick(served);
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
            served += 1;
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
fn echo(mut c: Client, log: &mut StallLog) -> BoxResult<()> {
    c.sock.set_nonblocking(true)?;
    let mut plain: Vec<u8> = Vec::with_capacity(1 << 20);
    let mut tmp = vec![0u8; 1 << 16];
    let mut out = vec![0u8; 1 << 16];
    let mut echoed = 0u64;
    log.start(0);
    loop {
        log.tick(echoed);
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
                    echoed += 1;
                }
                _ => {}
            }
            consumed = b;
        }
        plain.drain(..consumed);
    }
}
