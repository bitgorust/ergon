//! The lab measurements written the way most Rust code would: mio for
//! readiness (epoll on Linux, edge-triggered), rustls's standard buffered
//! `ClientConnection`, and a loop that busy-spins on `Poll::poll` with a zero
//! timeout. It is the baseline for ergon-io's own loops: same server, same TLS
//! configuration, same records.
//!
//! ```text
//! mio_bench lab --server ADDR --ca cert.pem --sni lab.local --conns 100 --cpu 3 --duration 60 --out DIR
//! mio_bench pingpong --server ADDR --ca cert.pem --sni lab.local --cpu 3 --rate 10000 --duration 120 --out DIR
//! ```
//!
//! `lab` connects `--conns` WebSockets (connection j asks for `/slot/j`, so
//! `replay_server --mode mixed` gives it its own stream) and records every
//! message: sequence, the sender's send time, decoded time. `pingpong` sends a
//! `--payload`-byte frame (sequence, send time) and waits for the echo, paced
//! at `--rate` on a fixed schedule (0: back to back). Writes `records.bin`,
//! `stalls-0.txt` (loop iterations over `--stall-us`) and `meta.txt`, variant
//! `mio`, as `io_bench` does.

use std::error::Error;
use std::fmt::Write as _;
use std::io::{ErrorKind, Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use io_bench::args::Args;
use io_bench::clock::{monotonic_ns, pin_current_thread, realtime_ns};
use io_bench::record::{Record, read_i64, write_records};
use io_bench::stalls::StallLog;
use io_bench::{tls, ws};
use mio::net::TcpStream;
use mio::{Events, Interest, Poll, Token};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection};

type BoxResult<T> = Result<T, Box<dyn Error>>;

/// One TLS WebSocket connection.
struct Conn {
    sock: TcpStream,
    tls: ClientConnection,
    /// Decrypted bytes not yet parsed into frames.
    buf: Vec<u8>,
    closed: bool,
}

impl Conn {
    /// Opens a TLS WebSocket to `server`, blocking until upgraded, then
    /// switches the socket to non-blocking.
    fn open(server: SocketAddr, sni: &str, path: &str, cfg: &Arc<ClientConfig>) -> BoxResult<Self> {
        let mut sock = std::net::TcpStream::connect(server)?;
        sock.set_nodelay(true)?;
        let name = ServerName::try_from(sni.to_owned())?;
        let mut tls = ClientConnection::new(cfg.clone(), name)?;
        while tls.is_handshaking() {
            tls.complete_io(&mut sock)?;
        }
        let key = ws::client_key(rand_nonce());
        let host = format!("{sni}:{}", server.port());
        tls.writer()
            .write_all(&ws::client_request(&host, path, &key, &[]))?;
        while tls.wants_write() {
            tls.write_tls(&mut sock)?;
        }
        let mut buf = Vec::with_capacity(1 << 16);
        let mut chunk = [0u8; 4096];
        let header = loop {
            loop {
                match tls.reader().read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                }
            }
            if let Some(n) = ws::parse_client_response(&buf, &key)? {
                break n;
            }
            if tls.read_tls(&mut sock)? == 0 {
                return Err("server closed during the upgrade".into());
            }
            tls.process_new_packets()?;
        };
        // Frames that arrived with the response stay in the buffer.
        buf.drain(..header);
        sock.set_nonblocking(true)?;
        Ok(Self {
            sock: TcpStream::from_std(sock),
            tls,
            buf,
            closed: false,
        })
    }

    /// Reads everything the socket has (edge-triggered: until `WouldBlock`)
    /// and decrypts it into `buf`. Returns the reads made.
    fn read_all(&mut self) -> BoxResult<u64> {
        let mut reads = 0;
        let mut chunk = [0u8; 16 * 1024];
        loop {
            reads += 1;
            match self.tls.read_tls(&mut self.sock) {
                Ok(0) => {
                    self.closed = true;
                    return Ok(reads);
                }
                Ok(_) => {
                    self.tls.process_new_packets()?;
                    loop {
                        match self.tls.reader().read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                            Err(e) => return Err(e.into()),
                        }
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(reads),
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Calls `f(opcode, payload)` for every complete frame in `buf`.
    fn frames(&mut self, mut f: impl FnMut(u8, &[u8])) -> BoxResult<()> {
        let mut at = 0;
        while let Some(h) = ws::parse_header(&self.buf[at..])? {
            if self.buf.len() - at < h.frame_len() {
                break;
            }
            let p = at + h.header_len..at + h.frame_len();
            f(h.opcode, &self.buf[p]);
            at += h.frame_len();
        }
        self.buf.drain(..at);
        Ok(())
    }

    /// Sends one masked binary frame.
    fn send(&mut self, payload: &[u8], frame: &mut Vec<u8>) -> BoxResult<()> {
        frame.resize(ws::header_len(payload.len(), true) + payload.len(), 0);
        let n = ws::encode_frame(ws::OP_BINARY, payload, Some(rand_mask()), frame)?;
        self.tls.writer().write_all(&frame[..n])?;
        while self.tls.wants_write() {
            match self.tls.write_tls(&mut self.sock) {
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => std::hint::spin_loop(),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

fn rand_nonce() -> [u8; 16] {
    let mut n = [0u8; 16];
    let t = realtime_ns().to_le_bytes();
    n[..8].copy_from_slice(&t);
    n[8..].copy_from_slice(&monotonic_ns().to_le_bytes());
    n
}

fn rand_mask() -> [u8; 4] {
    let t = monotonic_ns().to_le_bytes();
    [t[0], t[1], t[2], t[3]]
}

/// Opens `conns` connections and registers them with a new `Poll`.
fn connect(a: &Args, conns: usize) -> BoxResult<(Poll, Vec<Conn>)> {
    let server: SocketAddr = a.req("server")?.parse()?;
    let sni = a.str_or("sni", "lab.local");
    let ca = CertificateDer::from_pem_file(a.req("ca")?).map_err(|e| format!("ca: {e}"))?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca)?;
    // The same client configuration as ergon-io's (TLS 1.3, AES-128-GCM).
    let cfg = tls::client_config(roots)?;
    let poll = Poll::new()?;
    let mut out = Vec::with_capacity(conns);
    for j in 0..conns {
        let mut c = Conn::open(server, &sni, &format!("/slot/{j}"), &cfg)?;
        poll.registry()
            .register(&mut c.sock, Token(j), Interest::READABLE)?;
        out.push(c);
    }
    Ok((poll, out))
}

fn meta(mode: &str, duration_s: i64, extra: &str, spins: u64, reads: u64, polls: u64) -> String {
    let mut m = String::new();
    let _ = writeln!(m, "mode={mode}");
    let _ = writeln!(m, "duration_s={duration_s}");
    let _ = writeln!(m, "end_realtime_ns={}", realtime_ns());
    let _ = writeln!(
        m,
        "kernel={}",
        std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|s| s.trim().to_owned())
            .unwrap_or_default()
    );
    let _ = writeln!(m, "variants=mio");
    let _ = writeln!(m, "loop.0.variant=mio");
    let _ = writeln!(m, "loop.0.spins={spins}");
    let _ = writeln!(m, "loop.0.epoll_wait_calls={polls}");
    let _ = writeln!(m, "loop.0.recvmsg_calls={reads}");
    let _ = writeln!(m, "loop.0.syscalls={}", polls + reads);
    m.push_str(extra);
    m
}

fn lab(a: &Args) -> BoxResult<()> {
    let conns: usize = a.num_or("conns", 1)?;
    let duration_s: i64 = a.num_or("duration", 60)?;
    let capacity: usize = a.num_or("capacity", 4_000_000)?;
    let out = PathBuf::from(a.str_or("out", "results"));
    std::fs::create_dir_all(&out)?;
    let (mut poll, mut conns_v) = connect(a, conns)?;
    let mut events = Events::with_capacity(1024);
    let mut records: Vec<Record> = Vec::with_capacity(capacity);
    let mut dropped = 0u64;
    let mut stalls = StallLog::new(a.num_or("stall-us", 20.0)?, 1 << 20);
    let (mut spins, mut reads, mut polls) = (0u64, 0u64, 0u64);
    // Frames that came with the upgrade responses.
    for c in &mut conns_v {
        c.frames(|_, _| {})?;
    }
    let end = monotonic_ns() + duration_s * 1_000_000_000;
    let mut ready: Vec<usize> = Vec::with_capacity(conns);
    while monotonic_ns() < end {
        spins += 1;
        polls += 1;
        poll.poll(&mut events, Some(Duration::ZERO))?;
        ready.clear();
        ready.extend(events.iter().map(|e| e.token().0));
        for &j in &ready {
            let c = &mut conns_v[j];
            reads += c.read_all()?;
            let conn = u8::try_from(j).unwrap_or(u8::MAX);
            c.frames(|op, p| {
                if op != ws::OP_BINARY || p.len() < 24 {
                    return;
                }
                let r = Record {
                    conn,
                    id: read_i64(p, 0),
                    src_ns: read_i64(p, 8),
                    src2_ns: read_i64(p, 16),
                    user_ns: realtime_ns(),
                    ..Record::default()
                };
                if records.len() < records.capacity() {
                    records.push(r);
                } else {
                    dropped += 1;
                }
            })?;
            if c.closed {
                return finish_lab(
                    &out, duration_s, &records, &stalls, spins, reads, polls, dropped,
                );
            }
        }
        stalls.tick(records.len() as u64);
    }
    finish_lab(
        &out, duration_s, &records, &stalls, spins, reads, polls, dropped,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_lab(
    out: &std::path::Path,
    duration_s: i64,
    records: &[Record],
    stalls: &StallLog,
    spins: u64,
    reads: u64,
    polls: u64,
    dropped: u64,
) -> BoxResult<()> {
    stalls.write(&out.join("stalls-0.txt"))?;
    write_records(&out.join("records.bin"), records)?;
    let extra = format!(
        "loop.0.messages={}\nloop.0.dropped={dropped}\n",
        records.len()
    );
    std::fs::write(
        out.join("meta.txt"),
        meta("lab", duration_s, &extra, spins, reads, polls),
    )?;
    println!(
        "mio lab: {} messages, {spins} spins, {} syscalls",
        records.len(),
        reads + polls
    );
    Ok(())
}

fn pingpong(a: &Args) -> BoxResult<()> {
    let rate: i64 = a.num_or("rate", 1000)?;
    let duration_s: i64 = a.num_or("duration", 30)?;
    let payload_len: usize = a.num_or::<usize>("payload", 64)?.max(16);
    let out = PathBuf::from(a.str_or("out", "results"));
    std::fs::create_dir_all(&out)?;
    let (mut poll, mut conns_v) = connect(a, 1)?;
    let mut events = Events::with_capacity(16);
    let interval = if rate <= 0 { 0 } else { 1_000_000_000 / rate };
    let per_s = if rate > 0 { rate } else { 100_000 };
    let cap = usize::try_from(per_s * duration_s + 1_000).unwrap_or(1 << 20);
    let mut records: Vec<Record> = Vec::with_capacity(cap);
    let mut stalls = StallLog::new(a.num_or("stall-us", 20.0)?, 1 << 20);
    let (mut spins, mut reads, mut polls) = (0u64, 0u64, 0u64);
    let mut payload = vec![0u8; payload_len];
    let mut frame = Vec::with_capacity(payload_len + 16);
    let (mut seq, mut next_send, mut due_ns, mut outstanding) = (0i64, 0i64, 0i64, false);
    let end = monotonic_ns() + duration_s * 1_000_000_000;
    while monotonic_ns() < end {
        spins += 1;
        polls += 1;
        poll.poll(&mut events, Some(Duration::ZERO))?;
        if !events.is_empty() {
            let c = &mut conns_v[0];
            reads += c.read_all()?;
            let now_rx = &mut outstanding;
            let due = due_ns;
            c.frames(|op, p| {
                if op != ws::OP_BINARY || p.len() < 16 {
                    return;
                }
                if records.len() < records.capacity() {
                    records.push(Record {
                        id: read_i64(p, 0),
                        src_ns: read_i64(p, 8),
                        src2_ns: due,
                        user_ns: realtime_ns(),
                        ..Record::default()
                    });
                }
                *now_rx = false;
            })?;
            if c.closed {
                break;
            }
        }
        // The next round trip: on a fixed schedule that never slips.
        if !outstanding {
            let now = realtime_ns();
            if interval == 0 || now >= next_send {
                seq += 1;
                payload[..8].copy_from_slice(&seq.to_le_bytes());
                payload[8..16].copy_from_slice(&now.to_le_bytes());
                conns_v[0].send(&payload, &mut frame)?;
                outstanding = true;
                if interval == 0 {
                    due_ns = 0;
                } else {
                    due_ns = if next_send == 0 { now } else { next_send };
                    next_send = due_ns + interval;
                }
            }
        }
        stalls.tick(records.len() as u64);
    }
    stalls.write(&out.join("stalls-0.txt"))?;
    write_records(&out.join("records.bin"), &records)?;
    let extra = format!("rate={rate}\npayload={payload_len}\nconns_per_variant=1\n");
    std::fs::write(
        out.join("meta.txt"),
        meta("pingpong", duration_s, &extra, spins, reads, polls),
    )?;
    println!("mio pingpong: {} round trips", records.len());
    Ok(())
}

fn main() -> BoxResult<()> {
    let a = Args::from_env();
    // As io_bench: memory locked (needs root), the counter calibrated, the
    // thread pinned.
    #[cfg(target_os = "linux")]
    // SAFETY: plain syscall with valid flags.
    let _ = unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) };
    let _ = io_bench::clock::ticks_per_us();
    if let Some(c) = a.list("cpu", "").first() {
        pin_current_thread(c.parse()?)?;
    }
    match a.command.as_str() {
        "lab" => lab(&a),
        "pingpong" => pingpong(&a),
        other => Err(format!("unknown command {other:?}: lab or pingpong").into()),
    }
}
