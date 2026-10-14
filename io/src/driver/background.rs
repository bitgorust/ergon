//! Connection set-up on a helper thread: DNS (system resolver), TCP, TLS and
//! any upgrade run there, so the polling thread never spends the handshake's
//! CPU. Finished connections come back over a channel that `poll` checks only
//! while background connects are outstanding.

use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

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
    deadline: Instant,
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
    jobs: Sender<Job<E>>,
    done: Receiver<Done<E>>,
    /// Connects submitted and not yet received.
    pub(super) outstanding: usize,
}

impl<E: Send + 'static> Background<E> {
    /// Starts the helper thread. It inherits the caller's affinity (often one
    /// pinned CPU), so it first moves itself to `cpu`, or else to the
    /// housekeeping CPUs, away from `poller_cpu`.
    pub(super) fn start(cpu: Option<u32>) -> Result<Self> {
        let (jobs, job_rx) = mpsc::channel::<Job<E>>();
        let (done_tx, done) = mpsc::channel::<Done<E>>();
        let poller_cpu = crate::sys::current_cpu();
        std::thread::Builder::new()
            // At most 15 bytes: the kernel truncates thread names beyond that.
            .name("ergon-io-dial".into())
            .spawn(move || {
                let cpus = cpu.map_or_else(
                    || crate::sys::housekeeping_cpus(poller_cpu),
                    |c| vec![c as usize],
                );
                // Best effort: on failure the thread keeps the inherited set.
                let _ = crate::sys::set_affinity(&cpus);
                run(&job_rx, &done_tx);
            })?;
        Ok(Self {
            jobs,
            done,
            outstanding: 0,
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
        self.jobs
            .send(Job {
                slot,
                generation,
                dial,
                prepare,
                deadline: Instant::now() + timeout,
            })
            .map_err(|_| Error::Config("background connect thread stopped".into()))?;
        self.outstanding += 1;
        Ok(())
    }
}

impl<E> Background<E> {
    /// A finished connect, if any, without blocking.
    pub(super) fn try_take(&mut self) -> Option<Done<E>> {
        match self.done.try_recv() {
            Ok(d) => {
                self.outstanding -= 1;
                Some(d)
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }
}

fn run<E>(jobs: &Receiver<Job<E>>, done: &Sender<Done<E>>) {
    let mut active: Vec<(Job<E>, bool)> = Vec::new();
    loop {
        // Idle: wait for work (this thread may block; the poller never does).
        if active.is_empty() {
            match jobs.recv() {
                Ok(j) => active.push((j, false)),
                Err(_) => return,
            }
        }
        while let Ok(j) = jobs.try_recv() {
            active.push((j, false));
        }
        let mut i = 0;
        while i < active.len() {
            let (job, started) = &mut active[i];
            let outcome = advance(job, started);
            match outcome {
                Ok(None) => i += 1,
                result => {
                    let (job, _) = active.swap_remove(i);
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
        if !active.is_empty() {
            std::thread::sleep(Duration::from_micros(50));
        }
    }
}

fn advance<E>(job: &mut Job<E>, started: &mut bool) -> Result<Option<E>> {
    if Instant::now() > job.deadline {
        return Err(Error::Timeout("connect"));
    }
    if !*started {
        let addr = match job.dial.addr() {
            Some(a) => a,
            None => (job.dial.host(), job.dial.port())
                .to_socket_addrs()?
                .find(SocketAddr::is_ipv4)
                .ok_or_else(|| Error::Dns(format!("{}: no IPv4 address", job.dial.host())))?,
        };
        job.dial.start(addr)?;
        *started = true;
    }
    job.dial.step()
}
