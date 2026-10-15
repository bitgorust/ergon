//! Connection set-up on a helper thread: DNS (the driver's non-blocking
//! resolver, so one slow name never holds up other connects or their
//! deadlines), TCP, TLS and any upgrade run there, so the polling thread never
//! spends the handshake's CPU. Finished connections come back over a channel
//! that `poll` checks only while background connects are outstanding.
//! Connections the driver closes are dropped here too, so freeing their
//! buffers never lands on the polling thread.
//!
//! The thread's CPU mask is set before it starts (`sys::spawn_pinned`): a
//! thread inherits its creator's mask, and one that moved itself would first
//! run on the polling thread's CPU. It never blocks in the channel: idle, it
//! checks every millisecond, so handing it work never has to wake it (a futex
//! call on the polling thread).

use std::net::{IpAddr, SocketAddr};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::Duration;

use crate::clock::monotonic_ns;
use crate::dns::{DnsConfig, Lookup, Resolver};
use crate::endpoint::sealed::DialIo;
use crate::error::{Error, Result};

/// A dial as the helper thread sees it (no bound on the connection type).
pub(super) trait BgDial<E>: Send {
    fn host(&self) -> &str;
    fn port(&self) -> u16;
    fn addr(&self) -> Option<SocketAddr>;
    fn start(&mut self, addr: SocketAddr) -> Result<()>;
    fn step(&mut self) -> Result<Option<E>>;
}

impl<E, D: DialIo<Conn = E> + Send> BgDial<E> for D {
    fn host(&self) -> &str {
        DialIo::host(self)
    }
    fn port(&self) -> u16 {
        DialIo::port(self)
    }
    fn addr(&self) -> Option<SocketAddr> {
        DialIo::addr(self)
    }
    fn start(&mut self, addr: SocketAddr) -> Result<()> {
        DialIo::start(self, addr)
    }
    fn step(&mut self) -> Result<Option<E>> {
        DialIo::step(self)
    }
}

pub(super) type BoxDial<E> = Box<dyn BgDial<E>>;

/// Applies socket options to a finished connection (on the helper thread).
pub(super) type Prepare<E> = Box<dyn Fn(&E) -> Result<()> + Send>;

struct Job<E> {
    slot: usize,
    generation: u32,
    dial: BoxDial<E>,
    prepare: Prepare<E>,
    /// Monotonic nanoseconds.
    deadline_ns: i64,
    started: bool,
}

enum Msg<E> {
    Dial(Job<E>),
    /// A closed connection to free.
    Free(E),
}

/// A finished background connect.
pub(super) struct Done<E> {
    pub(super) slot: usize,
    pub(super) generation: u32,
    pub(super) result: Result<E>,
}

/// The helper thread's mailbox. Dropping it stops the thread once its jobs are
/// done; results nobody receives are dropped there (their sockets close).
pub(super) struct Background<E> {
    msgs: Sender<Msg<E>>,
    done: Receiver<Done<E>>,
    /// Connects submitted and not yet received: (slot, generation).
    outstanding: Vec<(usize, u32)>,
}

impl<E: Send + 'static> Background<E> {
    /// Starts the helper thread on `cpu`, or else on the housekeeping CPUs
    /// (online, not isolated, not the calling thread's).
    pub(super) fn start(cpu: Option<u32>, dns: DnsConfig) -> Result<Self> {
        let (msgs, msg_rx) = mpsc::channel::<Msg<E>>();
        let (done_tx, done) = mpsc::channel::<Done<E>>();
        let cpus = cpu.map_or_else(
            || crate::sys::housekeeping_cpus(crate::sys::current_cpu()),
            |c| vec![c as usize],
        );
        crate::sys::spawn_pinned("ergon-io-dial", &cpus, move || {
            run(&msg_rx, &done_tx, Resolver::new(dns));
        })?;
        Ok(Self {
            msgs,
            done,
            outstanding: Vec::new(),
        })
    }

    pub(super) fn submit(
        &mut self,
        slot: usize,
        generation: u32,
        dial: BoxDial<E>,
        prepare: Prepare<E>,
        timeout: Duration,
    ) -> Result<()> {
        self.msgs
            .send(Msg::Dial(Job {
                slot,
                generation,
                dial,
                prepare,
                deadline_ns: monotonic_ns()
                    .saturating_add(i64::try_from(timeout.as_nanos()).unwrap_or(i64::MAX)),
                started: false,
            }))
            .map_err(|_| Error::Config("background connect thread stopped".into()))?;
        self.outstanding.push((slot, generation));
        Ok(())
    }
}

impl<E> Background<E> {
    /// Frees `conn` on the helper thread (here, if the thread has stopped).
    pub(super) fn free(&self, conn: E) {
        let _ = self.msgs.send(Msg::Free(conn));
    }

    /// Whether connects are still to be taken.
    pub(super) const fn has_outstanding(&self) -> bool {
        !self.outstanding.is_empty()
    }

    /// A finished connect, if any, without blocking. If the helper thread has
    /// stopped, every outstanding connect is reported as failed.
    pub(super) fn try_take(&mut self) -> Option<Done<E>> {
        match self.done.try_recv() {
            Ok(d) => {
                self.outstanding.retain(|&o| o != (d.slot, d.generation));
                Some(d)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.outstanding.pop().map(|(slot, generation)| Done {
                    slot,
                    generation,
                    result: Err(Error::Config("background connect thread stopped".into())),
                })
            }
        }
    }
}

fn run<E>(msgs: &Receiver<Msg<E>>, done: &Sender<Done<E>>, mut resolver: Resolver) {
    let mut active: Vec<Job<E>> = Vec::new();
    let take = |m: Msg<E>, active: &mut Vec<Job<E>>| match m {
        Msg::Dial(j) => active.push(j),
        Msg::Free(c) => drop(c),
    };
    loop {
        loop {
            match msgs.try_recv() {
                Ok(m) => take(m, &mut active),
                // The driver is gone: finish what is in hand (the results are
                // dropped, closing their sockets), then stop.
                Err(TryRecvError::Disconnected) if active.is_empty() => return,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if active.is_empty() {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        let now = monotonic_ns();
        resolver.poll(now);
        let mut i = 0;
        while i < active.len() {
            match advance(&mut active[i], &mut resolver, now) {
                Ok(None) => i += 1,
                result => {
                    let job = active.swap_remove(i);
                    // Socket options are set here too: the first
                    // SO_TIMESTAMPING in a process flips a kernel static key
                    // (code patched on every CPU, ~100 µs), which must not
                    // land on the polling thread.
                    let result = result
                        .and_then(|c| c.ok_or(Error::Closed))
                        .and_then(|c| (job.prepare)(&c).map(|()| c));
                    // The driver is gone: dropping the result closes the socket.
                    let _ = done.send(Done {
                        slot: job.slot,
                        generation: job.generation,
                        result,
                    });
                }
            }
        }
        std::thread::sleep(Duration::from_micros(50));
    }
}

fn advance<E>(job: &mut Job<E>, resolver: &mut Resolver, now: i64) -> Result<Option<E>> {
    if now > job.deadline_ns {
        return Err(Error::Timeout("connect"));
    }
    if !job.started {
        let addr = match job.dial.addr() {
            Some(a) => a,
            None => match resolver.lookup(job.dial.host(), now)? {
                Lookup::Ready(ip) => SocketAddr::new(IpAddr::V4(ip), job.dial.port()),
                Lookup::Pending => return Ok(None),
            },
        };
        job.dial.start(addr)?;
        job.started = true;
    }
    job.dial.step()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    /// A dial whose first step panics, killing the helper thread.
    struct Boom;

    impl BgDial<u8> for Boom {
        fn host(&self) -> &'static str {
            "boom.test"
        }
        fn port(&self) -> u16 {
            1
        }
        fn addr(&self) -> Option<SocketAddr> {
            Some(SocketAddr::from(([127, 0, 0, 1], 1)))
        }
        fn start(&mut self, _: SocketAddr) -> Result<()> {
            Ok(())
        }
        #[allow(clippy::panic)]
        fn step(&mut self) -> Result<Option<u8>> {
            panic!("dial step panicked");
        }
    }

    #[test]
    fn a_stopped_helper_fails_every_outstanding_connect() -> Result<()> {
        let mut bg = Background::<u8>::start(None, DnsConfig::system())?;
        bg.submit(
            3,
            7,
            Box::new(Boom),
            Box::new(|_| Ok(())),
            Duration::from_secs(5),
        )?;
        bg.submit(
            4,
            9,
            Box::new(Boom),
            Box::new(|_| Ok(())),
            Duration::from_secs(5),
        )?;
        let end = Instant::now() + Duration::from_secs(5);
        let mut failed = Vec::new();
        while bg.has_outstanding() {
            if let Some(d) = bg.try_take() {
                assert!(d.result.is_err());
                failed.push((d.slot, d.generation));
            }
            assert!(Instant::now() < end, "outstanding connects never reported");
        }
        failed.sort_unstable();
        assert_eq!(failed, vec![(3, 7), (4, 9)]);
        Ok(())
    }
}
