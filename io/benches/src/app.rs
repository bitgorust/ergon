//! Applications driven by the I/O loops: recording Binance or lab messages, the
//! ping-pong round-trip client, and the capture writer.

use ergon_io::{ConnId, Driver, Error, Handler, Result, RxMeta, WsConn, WsHandler, ws};

use crate::clock::realtime_ns;
use crate::json::{self, Kind};
use crate::record::{Record, read_i64};
use crate::runner::App;

fn conn_u8(id: ConnId) -> u8 {
    u8::try_from(id.index()).unwrap_or(u8::MAX)
}

/// Lab frame prefix: `seq u64 | send_ns i64 | capture_ns i64`.
pub const LAB_PREFIX: usize = 24;

/// What the recorder expects in each message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Binance combined-stream JSON.
    Binance,
    /// Lab replay frames.
    Lab,
}

/// Records one [`Record`] per message into a preallocated vector.
pub struct Recorder {
    variant: u8,
    mode: Mode,
    streams: Vec<(Vec<u8>, Kind)>,
    /// Records captured so far.
    pub records: Vec<Record>,
    /// Messages not recorded because the vector was full.
    pub dropped: u64,
    /// Messages that could not be parsed.
    pub unparsed: u64,
    closed: Option<Error>,
}

impl Recorder {
    /// A recorder with room for `capacity` records (allocated up front).
    #[must_use]
    pub fn new(variant: u8, mode: Mode, streams: &[String], capacity: usize) -> Self {
        Self {
            variant,
            mode,
            streams: streams
                .iter()
                .map(|s| (s.as_bytes().to_vec(), Kind::of(s).unwrap_or(Kind::Trade)))
                .collect(),
            records: Vec::with_capacity(capacity),
            dropped: 0,
            unparsed: 0,
            closed: None,
        }
    }

    fn push(&mut self, r: Record) {
        if self.records.len() < self.records.capacity() {
            self.records.push(r);
        } else {
            self.dropped += 1;
        }
    }
}

impl Handler for Recorder {
    fn on_closed(&mut self, _id: ConnId, error: Error) {
        self.closed.get_or_insert(error);
    }
}

impl WsHandler for Recorder {
    fn on_message(&mut self, conn: ConnId, _opcode: u8, payload: &[u8], meta: &RxMeta, pos: u16) {
        let mut r = Record {
            variant: self.variant,
            conn: conn_u8(conn),
            pos,
            hw: u8::from(meta.rx_hw_ns != 0),
            rx_sw_ns: meta.rx_sw_ns,
            rx_hw_ns: meta.rx_hw_ns,
            read_seq: meta.read_seq,
            ..Record::default()
        };
        match self.mode {
            Mode::Binance => {
                let parsed = json::combined(payload).and_then(|(name, data)| {
                    let idx = self
                        .streams
                        .iter()
                        .position(|(s, _)| s.as_slice() == name)?;
                    let f = json::fields(data, self.streams[idx].1)?;
                    Some((idx, f))
                });
                let Some((idx, f)) = parsed else {
                    self.unparsed += 1;
                    return;
                };
                r.stream = idx as u16;
                r.id = f.id;
                r.src_ns = json::binance_ts_to_ns(f.event_time);
                r.src2_ns = if f.trade_time > 0 {
                    json::binance_ts_to_ns(f.trade_time)
                } else {
                    0
                };
            }
            Mode::Lab => {
                if payload.len() < LAB_PREFIX {
                    self.unparsed += 1;
                    return;
                }
                r.id = read_i64(payload, 0);
                r.src_ns = read_i64(payload, 8);
                r.src2_ns = read_i64(payload, 16);
            }
        }
        r.user_ns = realtime_ns();
        self.push(r);
    }
}

impl App for Recorder {
    fn take_closed(&mut self) -> Option<Error> {
        self.closed.take()
    }
}

/// Ping-pong client: one message outstanding, sent at a fixed rate (or back to
/// back when `interval_ns == 0`); each echo becomes a record whose
/// `user_ns - src_ns` is the round trip.
pub struct PingPong {
    variant: u8,
    interval_ns: i64,
    next_send: i64,
    seq: i64,
    outstanding: bool,
    payload_len: usize,
    /// Round-trip records.
    pub records: Vec<Record>,
    /// Round trips not recorded because the vector was full.
    pub dropped: u64,
    closed: Option<Error>,
}

impl PingPong {
    /// A client sending `payload_len`-byte messages every `interval_ns`.
    #[must_use]
    pub fn new(variant: u8, interval_ns: i64, payload_len: usize, capacity: usize) -> Self {
        Self {
            variant,
            interval_ns,
            next_send: 0,
            seq: 0,
            outstanding: false,
            payload_len: payload_len.max(16),
            records: Vec::with_capacity(capacity),
            dropped: 0,
            closed: None,
        }
    }
}

impl Handler for PingPong {
    fn on_closed(&mut self, _id: ConnId, error: Error) {
        self.closed.get_or_insert(error);
    }
}

impl WsHandler for PingPong {
    fn on_message(&mut self, conn: ConnId, _opcode: u8, payload: &[u8], meta: &RxMeta, pos: u16) {
        let now = realtime_ns();
        if payload.len() < 16 {
            return;
        }
        if self.records.len() < self.records.capacity() {
            self.records.push(Record {
                variant: self.variant,
                conn: conn_u8(conn),
                pos,
                id: read_i64(payload, 0),
                src_ns: read_i64(payload, 8),
                rx_sw_ns: meta.rx_sw_ns,
                rx_hw_ns: meta.rx_hw_ns,
                user_ns: now,
                read_seq: meta.read_seq,
                ..Record::default()
            });
        } else {
            self.dropped += 1;
        }
        self.outstanding = false;
    }
}

impl App for PingPong {
    fn needs_idle(&self) -> bool {
        true
    }

    fn take_closed(&mut self) -> Option<Error> {
        self.closed.take()
    }

    fn on_idle(&mut self, driver: &mut Driver<WsConn>) -> Result<()> {
        if self.outstanding {
            return Ok(());
        }
        let now = realtime_ns();
        if now < self.next_send {
            return Ok(());
        }
        let mut buf = [0u8; 1024];
        let len = self.payload_len.min(buf.len());
        self.seq += 1;
        buf[..8].copy_from_slice(&self.seq.to_le_bytes());
        let t = realtime_ns();
        buf[8..16].copy_from_slice(&t.to_le_bytes());
        let Some(conn) = driver.iter().next().map(|(id, _)| id) else {
            return Err(Error::Closed);
        };
        driver
            .get_mut(conn)
            .ok_or(Error::Closed)?
            .send_frame(ws::OP_BINARY, &buf[..len])?;
        self.outstanding = true;
        self.next_send = if self.interval_ns == 0 {
            0
        } else {
            now.max(self.next_send) + self.interval_ns
        };
        Ok(())
    }
}

/// Copies every message with its receive time into memory (for later replay).
pub struct Capture {
    /// `capture_ns i64 | len u32 | payload` entries.
    pub out: Vec<u8>,
    /// Messages captured.
    pub count: u64,
    /// The first connection error.
    pub closed: Option<Error>,
}

impl Handler for Capture {
    fn on_closed(&mut self, _id: ConnId, error: Error) {
        self.closed.get_or_insert(error);
    }
}

impl WsHandler for Capture {
    fn on_message(&mut self, _conn: ConnId, _opcode: u8, payload: &[u8], meta: &RxMeta, _pos: u16) {
        let t = if meta.rx_sw_ns != 0 {
            meta.rx_sw_ns
        } else {
            realtime_ns()
        };
        self.out.extend_from_slice(&t.to_le_bytes());
        self.out
            .extend_from_slice(&(payload.len() as u32).to_le_bytes());
        self.out.extend_from_slice(payload);
        self.count += 1;
    }
}

impl App for Capture {
    fn take_closed(&mut self) -> Option<Error> {
        self.closed.take()
    }
}
