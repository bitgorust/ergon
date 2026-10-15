//! Linux socket options and `recvmsg` / `send` with `SO_TIMESTAMPING` control
//! messages.
#![allow(unsafe_code)]

use std::io;
use std::os::fd::RawFd;

use crate::RxMeta;

/// Control-message buffer size: room for one `scm_timestamping` (3 timespecs).
pub const CTRL_LEN: usize = 128;

/// Requests software and (where the NIC supports it) hardware receive timestamps.
///
/// # Errors
/// `setsockopt` fails.
pub fn enable_rx_timestamps(fd: RawFd) -> io::Result<()> {
    let flags = libc::SOF_TIMESTAMPING_RX_SOFTWARE
        | libc::SOF_TIMESTAMPING_SOFTWARE
        | libc::SOF_TIMESTAMPING_RX_HARDWARE
        | libc::SOF_TIMESTAMPING_RAW_HARDWARE;
    setsockopt_int(fd, libc::SOL_SOCKET, libc::SO_TIMESTAMPING, to_int(flags)?)
}

/// Sets `SO_BUSY_POLL` (microseconds; no privilege needed).
///
/// # Errors
/// `setsockopt` fails.
pub fn set_busy_poll(fd: RawFd, usec: u32) -> io::Result<()> {
    setsockopt_int(fd, libc::SOL_SOCKET, libc::SO_BUSY_POLL, to_int(usec)?)
}

const SO_PREFER_BUSY_POLL: libc::c_int = 69;
const SO_BUSY_POLL_BUDGET: libc::c_int = 70;

/// Sets `SO_PREFER_BUSY_POLL` (enabling it needs `CAP_NET_ADMIN`).
///
/// # Errors
/// `setsockopt` fails (`EPERM` without the capability).
pub fn set_prefer_busy_poll(fd: RawFd, on: bool) -> io::Result<()> {
    setsockopt_int(fd, libc::SOL_SOCKET, SO_PREFER_BUSY_POLL, i32::from(on))
}

/// Sets `SO_BUSY_POLL_BUDGET`, packets per busy-poll pass (raising it needs
/// `CAP_NET_ADMIN`).
///
/// # Errors
/// `setsockopt` fails (`EPERM` without the capability).
pub fn set_busy_poll_budget(fd: RawFd, packets: u16) -> io::Result<()> {
    setsockopt_int(
        fd,
        libc::SOL_SOCKET,
        SO_BUSY_POLL_BUDGET,
        i32::from(packets),
    )
}

/// A socket's busy-poll settings (the kernel does not report
/// `SO_BUSY_POLL_BUDGET` back).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusyPoll {
    /// `SO_BUSY_POLL`, microseconds.
    pub usec: u32,
    /// `SO_PREFER_BUSY_POLL`.
    pub prefer: bool,
}

/// Reads `SO_BUSY_POLL` and `SO_PREFER_BUSY_POLL`.
///
/// # Errors
/// `getsockopt` fails.
pub fn busy_poll_settings(fd: RawFd) -> io::Result<BusyPoll> {
    let get = |opt| getsockopt_int(fd, libc::SOL_SOCKET, opt);
    Ok(BusyPoll {
        usec: u32::try_from(get(libc::SO_BUSY_POLL)?)
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?,
        prefer: get(SO_PREFER_BUSY_POLL)? != 0,
    })
}

/// Sets `SO_RCVBUF`.
///
/// # Errors
/// `setsockopt` fails.
pub fn set_rcvbuf(fd: RawFd, bytes: u32) -> io::Result<()> {
    setsockopt_int(fd, libc::SOL_SOCKET, libc::SO_RCVBUF, to_int(bytes)?)
}

/// `SO_INCOMING_NAPI_ID`: the NAPI id (NIC queue) of the socket's last packet; 0
/// when none is known yet.
///
/// # Errors
/// `getsockopt` fails.
pub fn incoming_napi_id(fd: RawFd) -> io::Result<u32> {
    const SO_INCOMING_NAPI_ID: libc::c_int = 56;
    getsockopt_int(fd, libc::SOL_SOCKET, SO_INCOMING_NAPI_ID).map(i32::cast_unsigned)
}

/// `SO_INCOMING_CPU`: the CPU that processed the socket's last packet.
///
/// # Errors
/// `getsockopt` fails.
pub fn incoming_cpu(fd: RawFd) -> io::Result<i32> {
    getsockopt_int(fd, libc::SOL_SOCKET, libc::SO_INCOMING_CPU)
}

/// The CPU the calling thread is running on, if known.
#[must_use]
pub fn current_cpu() -> Option<usize> {
    // SAFETY: plain syscall without arguments.
    usize::try_from(unsafe { libc::sched_getcpu() }).ok()
}

/// CPUs for background work: online and not isolated (`isolcpus`), without
/// `avoid`. Falls back to every online CPU but `avoid`, then to all online
/// CPUs. Reads sysfs, so call it off the polling thread.
#[must_use]
pub fn housekeeping_cpus(avoid: Option<usize>) -> Vec<usize> {
    let read = |p: &str| {
        std::fs::read_to_string(p)
            .map(|s| parse_cpu_list(&s))
            .unwrap_or_default()
    };
    let online = read("/sys/devices/system/cpu/online");
    let isolated = read("/sys/devices/system/cpu/isolated");
    let pick = |skip_isolated: bool| -> Vec<usize> {
        online
            .iter()
            .copied()
            .filter(|c| Some(*c) != avoid && !(skip_isolated && isolated.contains(c)))
            .collect()
    };
    let hk = pick(true);
    if !hk.is_empty() {
        return hk;
    }
    let others = pick(false);
    if others.is_empty() { online } else { others }
}

/// Parses a kernel CPU list such as `0-3,6,8-9`.
#[must_use]
pub fn parse_cpu_list(s: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for part in s.trim().split(',').filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
                    out.extend(a..=b);
                }
            }
            None => {
                if let Ok(c) = part.trim().parse() {
                    out.push(c);
                }
            }
        }
    }
    out
}

/// Restricts the calling thread to `cpus` (ignored when empty).
///
/// # Errors
/// `sched_setaffinity` fails.
pub fn set_affinity(cpus: &[usize]) -> io::Result<()> {
    if cpus.is_empty() {
        return Ok(());
    }
    // SAFETY: zeroed cpu_set_t is valid; CPU_SET is bounds-checked below.
    let rc = unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        for &c in cpus.iter().filter(|&&c| c < libc::CPU_SETSIZE as usize) {
            libc::CPU_SET(c, &mut set);
        }
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &raw const set)
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Starts a detached thread that runs only on `cpus`, from its first
/// instruction.
///
/// The mask is set in the thread attributes, so unlike a thread that moves
/// itself it never runs on its creator's (often isolated, busy-spinning) CPU.
/// An empty `cpus` keeps the creator's affinity. The thread is named `name`
/// (at most 15 bytes are kept); a panic in `f` ends the thread only.
///
/// # Errors
/// The attributes are rejected or the thread cannot be created.
pub fn spawn_pinned(
    name: &str,
    cpus: &[usize],
    f: impl FnOnce() + Send + 'static,
) -> io::Result<()> {
    type Job = Box<dyn FnOnce() + Send>;
    extern "C" fn start(arg: *mut libc::c_void) -> *mut libc::c_void {
        // SAFETY: `arg` is the `Box<Job>` leaked by `spawn_pinned`, taken back
        // exactly once, here.
        let job = unsafe { Box::from_raw(arg.cast::<Job>()) };
        // A panic must not unwind into the C caller.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
        std::ptr::null_mut()
    }
    let check = |rc: libc::c_int| {
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(rc))
        }
    };
    let cname = std::ffi::CString::new(
        name.as_bytes()
            .iter()
            .take(15)
            .copied()
            .collect::<Vec<u8>>(),
    )
    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "thread name contains NUL"))?;
    let job: Job = Box::new(move || {
        // SAFETY: PR_SET_NAME reads a NUL-terminated string of at most 16 bytes.
        unsafe { libc::prctl(libc::PR_SET_NAME, cname.as_ptr()) };
        f();
    });
    // SAFETY: the attribute object is initialised before use and destroyed on
    // every path; the cpu set is zeroed and filled within its bounds.
    unsafe {
        let mut attr: libc::pthread_attr_t = std::mem::zeroed();
        check(libc::pthread_attr_init(&raw mut attr))?;
        let result = (|| {
            if !cpus.is_empty() {
                let mut set: libc::cpu_set_t = std::mem::zeroed();
                for &c in cpus.iter().filter(|&&c| c < libc::CPU_SETSIZE as usize) {
                    libc::CPU_SET(c, &mut set);
                }
                check(libc::pthread_attr_setaffinity_np(
                    &raw mut attr,
                    std::mem::size_of::<libc::cpu_set_t>(),
                    &raw const set,
                ))?;
            }
            check(libc::pthread_attr_setdetachstate(
                &raw mut attr,
                libc::PTHREAD_CREATE_DETACHED,
            ))?;
            let arg = Box::into_raw(Box::new(job)).cast::<libc::c_void>();
            let mut tid: libc::pthread_t = 0;
            let rc = libc::pthread_create(&raw mut tid, &raw const attr, start, arg);
            if rc != 0 {
                // The thread never started: take the job back.
                drop(Box::from_raw(arg.cast::<Job>()));
            }
            check(rc)
        })();
        libc::pthread_attr_destroy(&raw mut attr);
        result
    }
}

fn to_int<T: TryInto<libc::c_int>>(v: T) -> io::Result<libc::c_int> {
    v.try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "socket option out of range"))
}

#[allow(clippy::cast_possible_truncation)] // 4 bytes
const INT_LEN: libc::socklen_t = std::mem::size_of::<libc::c_int>() as libc::socklen_t;

fn getsockopt_int(fd: RawFd, level: libc::c_int, name: libc::c_int) -> io::Result<i32> {
    let mut v: libc::c_int = 0;
    let mut len = INT_LEN;
    // SAFETY: `v` and `len` are valid for the call.
    let rc = unsafe { libc::getsockopt(fd, level, name, (&raw mut v).cast(), &raw mut len) };
    if rc == 0 {
        Ok(v)
    } else {
        Err(io::Error::last_os_error())
    }
}

fn setsockopt_int(
    fd: RawFd,
    level: libc::c_int,
    name: libc::c_int,
    v: libc::c_int,
) -> io::Result<()> {
    // SAFETY: `v` outlives the call and its size is passed.
    let rc = unsafe { libc::setsockopt(fd, level, name, (&raw const v).cast(), INT_LEN) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// A `msghdr` with one iovec and a control buffer at a stable address, as
/// `io_uring` needs (it reads the header after submission).
#[repr(C)]
pub struct MsgSlot {
    /// The header passed to `recvmsg` / `IORING_OP_RECVMSG`.
    pub hdr: libc::msghdr,
    iov: libc::iovec,
    ctrl: [u8; CTRL_LEN],
}

impl MsgSlot {
    /// A boxed slot (stable address).
    #[must_use]
    pub fn new() -> Box<Self> {
        // SAFETY: all-zero is a valid msghdr, iovec and byte array.
        let mut s: Box<Self> = Box::new(unsafe { std::mem::zeroed() });
        s.hdr.msg_iovlen = 1;
        s
    }

    /// Points the slot at `buf` and resets the control length. Call before every
    /// receive.
    pub const fn arm(&mut self, buf: &mut [u8]) {
        self.iov.iov_base = buf.as_mut_ptr().cast();
        self.iov.iov_len = buf.len();
        self.hdr.msg_iov = &raw mut self.iov;
        self.hdr.msg_iovlen = 1;
        self.hdr.msg_control = self.ctrl.as_mut_ptr().cast();
        #[allow(clippy::cast_possible_truncation)]
        {
            self.hdr.msg_controllen = CTRL_LEN as _;
        }
        self.hdr.msg_flags = 0;
    }

    /// Software and hardware receive timestamps after a completed receive (0 when
    /// absent).
    #[must_use]
    pub fn timestamps(&self) -> (i64, i64) {
        let mut sw = 0i64;
        let mut hw = 0i64;
        // SAFETY: the kernel filled `msg_controllen` bytes of well-formed cmsgs
        // into `ctrl`; the CMSG_* helpers stay within that length.
        unsafe {
            let mut c = libc::CMSG_FIRSTHDR(&raw const self.hdr);
            while !c.is_null() {
                if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SO_TIMESTAMPING {
                    let ts = libc::CMSG_DATA(c)
                        .cast::<[libc::timespec; 3]>()
                        .read_unaligned();
                    sw = ts_ns(&ts[0]);
                    hw = ts_ns(&ts[2]);
                }
                c = libc::CMSG_NXTHDR(&raw const self.hdr, c);
            }
        }
        (sw, hw)
    }

    /// [`RxMeta`] for a completed receive.
    #[must_use]
    pub fn meta(&self, read_seq: u64) -> RxMeta {
        let (rx_sw_ns, rx_hw_ns) = self.timestamps();
        RxMeta {
            rx_sw_ns,
            rx_hw_ns,
            read_seq,
        }
    }
}

const fn ts_ns(t: &libc::timespec) -> i64 {
    t.tv_sec * 1_000_000_000 + t.tv_nsec
}

/// Non-blocking `recvmsg` through `slot`. `Ok(None)` on `EAGAIN`.
///
/// # Errors
/// Any other `recvmsg` error.
pub fn recvmsg(fd: RawFd, slot: &mut MsgSlot) -> io::Result<Option<usize>> {
    // SAFETY: `slot.hdr` was armed with buffers that outlive the call.
    let n = unsafe { libc::recvmsg(fd, &raw mut slot.hdr, libc::MSG_DONTWAIT) };
    if let Ok(n) = usize::try_from(n) {
        return Ok(Some(n));
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::EAGAIN) {
        Ok(None)
    } else {
        Err(e)
    }
}

/// Non-blocking `send`. Returns bytes sent (0 on `EAGAIN`).
///
/// # Errors
/// Any other `send` error.
pub fn send(fd: RawFd, buf: &[u8]) -> io::Result<usize> {
    // SAFETY: `buf` is valid for `buf.len()` bytes.
    let n = unsafe {
        libc::send(
            fd,
            buf.as_ptr().cast(),
            buf.len(),
            libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
        )
    };
    if let Ok(n) = usize::try_from(n) {
        return Ok(n);
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::EAGAIN) {
        Ok(0)
    } else {
        Err(e)
    }
}
