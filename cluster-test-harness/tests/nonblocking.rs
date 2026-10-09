#![allow(missing_docs)]
//! No poll waits on the media driver, and a session follows its own egress
//! image rather than whichever images the subscription holds.
//!
//! The client runs on a Java media driver in its own process, which the tests
//! freeze with `SIGSTOP`: an add that waited for the driver would hang until
//! it resumes, while a polled add keeps every poll short. A fake leader on an
//! in-process driver plays the cluster, so each step happens on cue.

#[expect(
    dead_code,
    reason = "these tests drive a fake leader, not the shared echo helpers"
)]
mod common;

use std::error::Error;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use ergo_aeron_cluster::cluster_codec_types::{
    EventCode, NewLeaderEventEncoder, NewLeaderEventFixedFields, SessionEventEncoder,
    SessionEventFixedFields, SessionMessageHeaderDecoder,
};
use ergo_aeron_cluster::egress::EgressAdapter;
use ergo_aeron_cluster::{AeronCluster, ClusterError, ConnectStep, SessionBuilder, SessionState};
use rusteron_client::{
    Aeron, AeronContext, AeronExclusivePublication, AeronSubscription, Handlers, cformat,
};
use serial_test::serial;

use common::{Capture, launch_own_driver};

/// How long a test keeps the client's driver frozen.
const FROZEN_FOR: Duration = Duration::from_millis(1500);
/// Far below [`FROZEN_FOR`], so a poll that waited on the driver fails.
const POLL_BUDGET: Duration = Duration::from_millis(250);
const CLUSTER_SESSION_ID: i64 = 7;
const MEMBER_INGRESS: [&str; 2] = [
    "aeron:udp?endpoint=localhost:19231",
    "aeron:udp?endpoint=localhost:19232",
];
const CLIENT_EGRESS: &str = "aeron:udp?endpoint=localhost:19233";
/// Differ from the driver defaults, so a rebuilt publication that dropped
/// them would report other limits.
const INGRESS_SETTINGS: &str = "|term-length=1m|mtu=4096";
const LIMITS: (usize, usize) = (1024 * 1024 / 8, 4096 - 32);

/// A Java media driver in its own process, so a test can freeze it without
/// freezing itself.
struct JavaDriver {
    child: Child,
    dir: String,
}

impl JavaDriver {
    fn launch(tag: &str) -> Result<Self, Box<dyn Error>> {
        let dir = std::env::temp_dir().join(format!("{tag}-{}", std::process::id()));
        let dir = dir
            .to_str()
            .ok_or("aeron dir is not valid UTF-8")?
            .to_string();
        let jar = ergo_aeron_cluster_test_harness::jar::try_find_jar("aeron-all-")?;
        let child = Command::new("java")
            .args([
                "--add-opens",
                "java.base/jdk.internal.misc=ALL-UNNAMED",
                "-Xmx128m",
                "-cp",
            ])
            .arg(jar)
            .arg(format!("-Daeron.dir={dir}"))
            .args([
                "-Daeron.dir.delete.on.start=true",
                "-Daeron.dir.delete.on.shutdown=true",
                "io.aeron.driver.MediaDriver",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let driver = Self { child, dir };
        // Waits for the driver's CnC file and first heartbeat.
        connect(&driver.dir)?;
        Ok(driver)
    }

    /// Freeze the driver; another thread resumes it after [`FROZEN_FOR`], so
    /// a poll that waits on the driver ends rather than hangs.
    fn freeze(&self) -> Result<thread::JoinHandle<Result<(), String>>, Box<dyn Error>> {
        let pid = self.child.id();
        signal(pid, "-STOP")?;
        Ok(thread::spawn(move || {
            thread::sleep(FROZEN_FOR);
            signal(pid, "-CONT").map_err(|e| e.to_string())
        }))
    }
}

impl Drop for JavaDriver {
    fn drop(&mut self) {
        let pid = self.child.id();
        let _ = signal(pid, "-CONT");
        // TERM runs the shutdown hook that deletes the driver directory.
        let _ = signal(pid, "-TERM");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && matches!(self.child.try_wait(), Ok(None)) {
            thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn signal(pid: u32, signal: &str) -> Result<(), Box<dyn Error>> {
    let status = Command::new("kill")
        .arg(signal)
        .arg(pid.to_string())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("kill {signal} {pid}: {status}").into())
    }
}

fn connect(dir: &str) -> Result<Aeron, Box<dyn Error>> {
    let ctx = AeronContext::new()?;
    ctx.set_dir(&cformat!("{dir}"))?;
    let aeron = Aeron::new(&ctx)?;
    aeron.start()?;
    Ok(aeron)
}

fn wait_until(mut done: impl FnMut() -> bool, what: &str) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {what}").into());
        }
        thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

fn join(resume: thread::JoinHandle<Result<(), String>>) -> Result<(), Box<dyn Error>> {
    Ok(resume.join().map_err(|_| "resume thread panicked")??)
}

/// A fake cluster member's egress publication to the client: each one is a
/// separate image on the client's egress subscription.
fn egress_publication(leader: &Aeron) -> Result<AeronExclusivePublication, Box<dyn Error>> {
    Ok(leader.add_exclusive_publication(
        &cformat!("{CLIENT_EGRESS}|term-length=64k"),
        102,
        Duration::from_secs(5),
    )?)
}

fn offer(publication: &AeronExclusivePublication, message: &[u8]) -> Result<(), Box<dyn Error>> {
    wait_until(
        || publication.offer_raw(message, Handlers::NONE) > 0,
        "a fake leader's offer",
    )
}

fn session_id(publication: &AeronExclusivePublication) -> Result<i32, Box<dyn Error>> {
    Ok(publication.get_constants()?.session_id)
}

fn poll_any(subscription: &AeronSubscription) -> Result<bool, Box<dyn Error>> {
    let mut any = false;
    subscription.poll_fn(|_, _| any = true, 10)?;
    Ok(any)
}

fn session_event_ok() -> Result<Vec<u8>, Box<dyn Error>> {
    let mut buf = vec![0u8; SessionEventEncoder::compute_encoded_length_with_message_header(0)];
    let len = SessionEventEncoder::wrap_and_apply_header(&mut buf, 0)
        .fixed(&SessionEventFixedFields {
            cluster_session_id: CLUSTER_SESSION_ID,
            correlation_id: 0,
            leadership_term_id: 1,
            leader_member_id: 0,
            code: EventCode::OK,
            version: Some(1),
            leader_heartbeat_timeout_ns: None,
        })
        .detail(b"")?
        .encoded_length_with_header();
    assert_eq!(len, buf.len());
    Ok(buf)
}

fn new_leader_event() -> Result<Vec<u8>, Box<dyn Error>> {
    let endpoints = b"0=localhost:19231,1=localhost:19232";
    let mut buf =
        vec![
            0u8;
            NewLeaderEventEncoder::compute_encoded_length_with_message_header(endpoints.len())
        ];
    let len = NewLeaderEventEncoder::wrap_and_apply_header(&mut buf, 0)
        .fixed(&NewLeaderEventFixedFields {
            leadership_term_id: 2,
            cluster_session_id: CLUSTER_SESSION_ID,
            leader_member_id: 1,
        })
        .ingress_endpoints(endpoints)?
        .encoded_length_with_header();
    assert_eq!(len, buf.len());
    Ok(buf)
}

/// Poll `client` until it reaches `state`.
fn poll_until(
    client: &mut AeronCluster,
    adapter: &mut EgressAdapter<Capture>,
    state: SessionState,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while client.state() != state {
        client.poll_egress(adapter, 10)?;
        if Instant::now() >= deadline {
            return Err(format!("still {:?}, expected {state:?}", client.state()).into());
        }
        thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

#[test]
#[serial]
fn async_connect_adds_its_transport_without_waiting() -> Result<(), Box<dyn Error>> {
    let driver = JavaDriver::launch("nonblocking-connect")?;
    let builder = SessionBuilder::default()
        .ingress_channel("aeron:udp?endpoint=localhost:19221")?
        .egress_channel("aeron:udp?endpoint=localhost:19222")?
        .message_timeout(Duration::from_secs(30))?;
    let mut connecting = builder.connect_async(driver.dir.clone());

    let resume = driver.freeze()?;
    let frozen_at = Instant::now();
    while frozen_at.elapsed() < FROZEN_FOR - Duration::from_millis(300) {
        let started = Instant::now();
        connecting.poll()?;
        assert!(
            started.elapsed() < POLL_BUDGET,
            "a poll took {:?} while the driver was frozen",
            started.elapsed()
        );
        assert_eq!(
            connecting.step(),
            ConnectStep::CreateTransport,
            "a frozen driver cannot have added the transport"
        );
        thread::sleep(Duration::from_millis(1));
    }
    join(resume)?;

    let deadline = Instant::now() + Duration::from_secs(10);
    while connecting.step() == ConnectStep::CreateTransport {
        connecting.poll()?;
        if Instant::now() >= deadline {
            return Err("the transport was not added after the driver resumed".into());
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(connecting.step(), ConnectStep::SendConnect);
    Ok(())
}

#[test]
#[serial]
fn failover_follows_the_session_egress_image_without_waiting() -> Result<(), Box<dyn Error>> {
    let client_driver = JavaDriver::launch("nonblocking-failover")?;
    let leader_driver = launch_own_driver("nonblocking-leader")?;
    let leader = connect(&leader_driver.dir)?;
    let mut member_ingress = Vec::with_capacity(MEMBER_INGRESS.len());
    for channel in MEMBER_INGRESS {
        member_ingress.push(leader.add_subscription(
            &cformat!("{channel}"),
            101,
            Handlers::NONE,
            Handlers::NONE,
            Duration::from_secs(5),
        )?);
    }

    // Member 0 answers the connect over the first egress image.
    let builder = SessionBuilder::default()
        .ingress_channel(format!("{}{INGRESS_SETTINGS}", MEMBER_INGRESS[0]))?
        .egress_channel(CLIENT_EGRESS)?
        .message_timeout(Duration::from_secs(10))?
        .new_leader_timeout(Duration::from_secs(60))?;
    let mut connecting = builder.connect_async(client_driver.dir.clone());
    let mut old_egress = None;
    let deadline = Instant::now() + Duration::from_secs(20);
    while !connecting.is_complete() {
        connecting.poll()?;
        if old_egress.is_none() && poll_any(&member_ingress[0])? {
            let publication = egress_publication(&leader)?;
            offer(&publication, &session_event_ok()?)?;
            old_egress = Some(publication);
        }
        if Instant::now() >= deadline {
            return Err(format!("connect stuck at {:?}", connecting.step()).into());
        }
        thread::sleep(Duration::from_millis(1));
    }
    let old_egress = old_egress.ok_or("connected without the fake leader answering")?;
    let mut client = connecting.finish()?;
    let first_image = client
        .egress_image_correlation_id()
        .ok_or("no egress image bound at connect")?;
    assert_eq!(
        client.egress_image_session_id(),
        Some(session_id(&old_egress)?)
    );
    assert_eq!(
        (
            client.ingress_max_message_length(),
            client.ingress_max_payload_length()
        ),
        LIMITS
    );

    // Member 1 is elected and sends NewLeaderEvent over a second egress image
    // while member 0's image stays open, as a failed leader's image lingers.
    let new_egress = egress_publication(&leader)?;
    wait_until(|| new_egress.is_connected(), "the second egress image")?;
    offer(&new_egress, &new_leader_event()?)?;
    // Let the event reach the client's image before freezing its driver.
    thread::sleep(Duration::from_millis(500));

    let mut adapter = EgressAdapter::new(Capture::new());
    let resume = client_driver.freeze()?;
    let frozen_at = Instant::now();
    while frozen_at.elapsed() < FROZEN_FOR - Duration::from_millis(300) {
        let started = Instant::now();
        client.poll_egress(&mut adapter, 10)?;
        assert!(
            started.elapsed() < POLL_BUDGET,
            "a poll took {:?} while the driver was frozen",
            started.elapsed()
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        adapter.listener().new_leader_member_id,
        Some(1),
        "the NewLeaderEvent had not reached the client before its driver froze"
    );
    // The session moves to the new image at once, but keeps the old term
    // until the new leader's publication exists.
    assert_eq!(client.state(), SessionState::AwaitingNewLeader);
    assert_eq!(
        client.egress_image_session_id(),
        Some(session_id(&new_egress)?)
    );
    assert_ne!(client.egress_image_correlation_id(), Some(first_image));
    assert_eq!(client.leadership_term_id(), 1);
    assert!(matches!(
        client.offer(b"too early"),
        Err(ClusterError::NotConnected)
    ));
    join(resume)?;

    // Once the driver adds the publication, the session is member 1's, with
    // the channel settings and so the limits it started with.
    poll_until(&mut client, &mut adapter, SessionState::Connected)?;
    assert_eq!(
        (client.leadership_term_id(), client.leader_member_id()),
        (2, 1)
    );
    assert_eq!(
        (
            client.ingress_max_message_length(),
            client.ingress_max_payload_length()
        ),
        LIMITS
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while let Err(error) = client.offer(b"to member 1") {
        if !error.is_retryable() || Instant::now() >= deadline {
            return Err(error.into());
        }
        client.poll_egress(&mut adapter, 10)?;
        thread::sleep(Duration::from_millis(1));
    }
    let mut received = None;
    wait_until(
        || {
            let _ = member_ingress[1].poll_fn(
                |data, _| {
                    if let Ok(header) = SessionMessageHeaderDecoder::decode(data, 0)
                        && data.get(AeronCluster::SESSION_HEADER_LENGTH..) == Some(b"to member 1")
                    {
                        received = Some((header.leadership_term_id(), header.cluster_session_id()));
                    }
                },
                10,
            );
            received.is_some()
        },
        "the message at member 1",
    )?;
    assert_eq!(received, Some((2, CLUSTER_SESSION_ID)));

    // The bound image closes while member 0's image is still open: a leader
    // loss that counting the subscription's images would miss.
    drop(new_egress);
    poll_until(&mut client, &mut adapter, SessionState::AwaitingNewLeader)?;
    assert!(
        old_egress.is_connected(),
        "member 0's egress image must still be open"
    );
    Ok(())
}
