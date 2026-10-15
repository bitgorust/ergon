//! Standalone Aeron media driver with transport selection.
//!
//! `RUSTERON_MEDIA_DRIVER_TRANSPORT=dpdk-ena` installs the DPDK ENA transport
//! (configured from the `RUSTERON_DPDK_*` environment) before the driver
//! starts; absent or `default` keeps the socket transport. A selected DPDK
//! transport that fails exits non-zero; it never falls back to the socket
//! driver. Under `dpdk-ena` the receiver IO vector capacity is 16 unless
//! `AERON_RECEIVER_IO_VECTOR_CAPACITY` says otherwise.
//! `RUSTERON_MEDIA_DRIVER_DIR` pins the Aeron directory, and
//! `RUSTERON_MEDIA_DRIVER_MLOCKALL=1` locks the process's memory, present and
//! future, before the driver starts. SIGINT, SIGTERM and SIGHUP stop it
//! cleanly.

use ergon_md_dpdk::{configure_media_transport_from_env, lock_memory_from_env, pin_agents_to_configured_cpus};
use log::info;
use rusteron_media_driver::{AeronDriver, AeronDriverContext};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The receiver IO vector capacity under `dpdk-ena`, unless the environment
/// sets one: 16, Aeron's maximum, the setting every kept DPDK measurement used
/// (BENCHMARKS.md). The DPDK receive poll no longer depends on it, since it
/// drains whole bursts whatever Aeron's vlen; it still sizes each receive from
/// a host-local socket. Never applied to the socket transport, where 16 cost
/// NIC drops with kernel busy polling.
const DPDK_RECEIVER_IO_VECTOR_CAPACITY: u32 = 16;
const RECEIVER_IO_VECTOR_CAPACITY_ENV: &str = "AERON_RECEIVER_IO_VECTOR_CAPACITY";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Flag to indicate when the application should stop
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = Arc::clone(&running);

    // SIGINT, and with ctrlc's `termination` feature SIGTERM and SIGHUP:
    // kubelet stops a pod with SIGTERM, and as the container's PID 1 (the
    // entrypoint execs it) the driver would otherwise ignore it until the
    // grace period's SIGKILL, closing neither the driver nor the transport.
    ctrlc::set_handler(move || {
        running_clone.store(false, Ordering::SeqCst);
    })?;

    // Create Aeron context
    let aeron_context = AeronDriverContext::new()?;

    // Pin a reproducible driver dir for standalone acceptance runs.
    // The caller removes the dir first — Aeron refuses to start over a live cnc.
    if let Ok(dir) = std::env::var("RUSTERON_MEDIA_DRIVER_DIR") {
        let cdir = std::ffi::CString::new(dir)?;
        aeron_context.set_dir(&cdir)?;
    }

    // Select the transport from `RUSTERON_MEDIA_DRIVER_TRANSPORT` after the
    // context exists and before the driver is created. A selected
    // DPDK failure propagates as a nonzero exit; it never falls back to the
    // default socket driver.
    // Apply AERON_*_CPU_AFFINITY as aeronmd does, in every transport mode.
    pin_agents_to_configured_cpus(&aeron_context)?;
    let dpdk = configure_media_transport_from_env(&aeron_context)?;
    let backend = if dpdk.is_some() { "dpdk-ena" } else { "socket" };
    if dpdk.is_some() && std::env::var_os(RECEIVER_IO_VECTOR_CAPACITY_ENV).is_none() {
        aeron_context.set_receiver_io_vector_capacity(DPDK_RECEIVER_IO_VECTOR_CAPACITY)?;
    }
    println!("transport backend: {backend}");
    println!("aeron dir: {}", aeron_context.get_dir());
    aeron_context.print_configuration();

    // RUSTERON_MEDIA_DRIVER_MLOCKALL=1: every mapping faulted in by mmap, so
    // the agents never page-fault on a log buffer
    if lock_memory_from_env()? {
        println!("memory locked (mlockall current and future)");
    }
    // Create Aeron driver
    let aeron_driver = AeronDriver::new(&aeron_context)?;
    aeron_driver.start(true)?;
    // Start the Aeron driver
    println!("media driver started");
    info!("Aeron media driver started successfully. Stop it with SIGINT or SIGTERM.");

    // Poll for work until a stop signal arrives
    while running.load(Ordering::Acquire) {
        aeron_driver.main_idle_strategy(aeron_driver.main_do_work()?);
    }
    info!("Received signal to stop the media driver.");
    // The transport owns the native runtime the driver's bindings point into,
    // so it goes only after the driver.
    drop(aeron_driver);
    drop(aeron_context);
    drop(dpdk);
    info!("Aeron media driver stopped successfully.");
    Ok(())
}
