//! Standalone Aeron media driver with transport selection.
//!
//! `RUSTERON_MEDIA_DRIVER_TRANSPORT=dpdk-ena` installs the DPDK ENA transport
//! (configured from the `RUSTERON_DPDK_*` environment) before the driver
//! starts; absent or `default` keeps the socket transport. A selected DPDK
//! transport that fails exits non-zero; it never falls back to the socket
//! driver. `RUSTERON_MEDIA_DRIVER_DIR` pins the Aeron directory.

use ergon_md_dpdk::{configure_media_transport_from_env, pin_agents_to_configured_cpus};
use log::info;
use rusteron_media_driver::{AeronDriver, AeronDriverContext};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Flag to indicate when the application should stop (set on Ctrl+C)
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = Arc::clone(&running);

    // Register signal handler for SIGINT (Ctrl+C)
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
    println!("transport backend: {backend}");
    println!("aeron dir: {}", aeron_context.get_dir());
    aeron_context.print_configuration();

    // Create Aeron driver
    let aeron_driver = AeronDriver::new(&aeron_context)?;
    aeron_driver.start(true)?;
    // Start the Aeron driver
    println!("media driver started");
    info!("Aeron media driver started successfully. Press Ctrl+C to stop.");

    // Poll for work until Ctrl+C is pressed
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
