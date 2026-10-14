//! FFI to the native DPDK transport and the install/lifetime path.
// The only unsafe code in the crate: calls into the native transport the
// build script compiles, and the `Send`/`Sync` of its opaque handle.
#![allow(unsafe_code)]

use crate::config::DpdkTransportConfig;
use crate::error::DpdkError;
use rusteron_media_driver::AeronDriverContext;
use std::sync::Arc;

#[cfg(feature = "dpdk")]
use std::ffi::CStr;
#[cfg(feature = "dpdk")]
use std::os::raw::{c_char, c_int, c_void};

/// Owning handle to the native DPDK transport installed into an
/// [`AeronDriverContext`].
///
/// Cloning shares the same native instance; the native runtime (EAL, both
/// ports, mempools) is closed exactly once, when the last clone is dropped.
///
/// The bindings installed into the context point into that runtime, and
/// rusteron 0.2.11's context cannot hold a Rust dependency for it, so the
/// caller owns its lifetime: keep a clone alive until every driver started
/// from the context has stopped.
#[derive(Clone, Debug)]
#[must_use = "dropping the transport while a driver uses it is a use-after-free in the driver's agents"]
pub struct DpdkTransport {
    // Read only in the `dpdk` install path.
    #[cfg_attr(not(feature = "dpdk"), allow(dead_code))]
    inner: Arc<NativeTransport>,
}

/// Opaque native transport state. Empty without the `dpdk` feature.
#[derive(Debug)]
struct NativeTransport {
    #[cfg(feature = "dpdk")]
    ptr: *mut c_void,
}

impl DpdkTransport {
    /// Validate the configuration and the existing Aeron context, create the
    /// native DPDK runtime (EAL and both ENA ports), and install its sender and
    /// receiver bindings into `context`.
    ///
    /// Call it after the context is configured and before the driver is
    /// created. The returned handle must outlive every driver started from
    /// `context`.
    ///
    /// # Errors
    ///
    /// [`DpdkError::FeatureDisabled`] without the `dpdk` feature, before the
    /// config or context is inspected; otherwise
    /// [`DpdkError::InvalidConfiguration`] for a config or context that breaks
    /// the transport's requirements, [`DpdkError::Native`] when DPDK fails to
    /// initialise, and [`DpdkError::Aeron`] when installation fails.
    #[cfg_attr(not(feature = "dpdk"), allow(clippy::missing_const_for_fn))]
    pub fn install(context: &AeronDriverContext, config: &DpdkTransportConfig) -> Result<Self, DpdkError> {
        #[cfg(feature = "dpdk")]
        {
            config.validate()?;
            validate_context(context, config)?;
            pin_agents_to_configured_cpus(context)?;
            let transport = Self {
                inner: Arc::new(NativeTransport::create(config)?),
            };
            transport.inner.install(context)?;
            Ok(transport)
        }
        #[cfg(not(feature = "dpdk"))]
        {
            let _ = (context, config);
            Err(DpdkError::FeatureDisabled)
        }
    }
}

#[cfg(feature = "dpdk")]
impl NativeTransport {
    fn create(config: &DpdkTransportConfig) -> Result<Self, DpdkError> {
        let native_config = native_config(config)?;
        let mut ptr: *mut c_void = std::ptr::null_mut();
        // SAFETY: both pointers are valid for the call; the C side copies the
        // config and writes `ptr` only on success.
        let rc = unsafe { rusteron_dpdk_transport_create(&raw const native_config, &raw mut ptr) };
        if rc != 0 {
            return Err(DpdkError::Native(last_error()));
        }
        Ok(Self { ptr })
    }

    fn install(&self, context: &AeronDriverContext) -> Result<(), DpdkError> {
        // SAFETY: `ptr` came from a successful create and is not yet closed;
        // the context pointer is live for the borrow of `context`.
        let rc = unsafe { rusteron_dpdk_transport_install(self.ptr, context.get_inner()) };
        if rc != 0 {
            return Err(DpdkError::Aeron(last_error()));
        }
        Ok(())
    }
}

// SAFETY: the native runtime is built for the multi-threaded media driver,
// whose sender and receiver agents both use it from their own threads; Rust
// only holds the opaque pointer and passes it back to close, once. Mirrors
// rusteron's own `unsafe impl Send/Sync for AeronDriverContext`.
#[cfg(feature = "dpdk")]
unsafe impl Send for NativeTransport {}
// SAFETY: as for `Send`; no Rust method dereferences the pointer.
#[cfg(feature = "dpdk")]
unsafe impl Sync for NativeTransport {}

#[cfg(feature = "dpdk")]
impl Drop for NativeTransport {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: the last owner closes the pointer exactly once.
            unsafe { rusteron_dpdk_transport_close(self.ptr) };
        }
    }
}

/// Mirror of the C `rusteron_dpdk_config_t` in `native/dpdk/rusteron_dpdk_transport.h`.
#[cfg(feature = "dpdk")]
#[repr(C)]
struct rusteron_dpdk_config_t {
    struct_size: u32,
    file_prefix: [c_char; 65],
    hugepage_dir: [c_char; 4096],
    sender_pci: [c_char; 16],
    sender_ipv4: [c_char; 16],
    sender_prefix_len: u8,
    sender_gateway: [c_char; 16],
    receiver_pci: [c_char; 16],
    receiver_ipv4: [c_char; 16],
    receiver_prefix_len: u8,
    receiver_gateway: [c_char; 16],
    rx_descriptors: u16,
    tx_descriptors: u16,
    mbufs_per_port: u32,
    mempool_cache: u16,
    burst_size: u16,
    max_aeron_mtu: usize,
    sender_mac: [c_char; 18],
    receiver_mac: [c_char; 18],
    rx_hw_timestamp: u8,
}

// The C layout, field by field (asserted in native/dpdk/rusteron_dpdk_transport.c
// too): a field moved, resized or swapped with one of its size fails the build.
#[cfg(feature = "dpdk")]
const _: () = {
    use std::mem::{offset_of, size_of};
    type C = rusteron_dpdk_config_t;
    assert!(offset_of!(C, struct_size) == 0);
    assert!(offset_of!(C, file_prefix) == 4);
    assert!(offset_of!(C, hugepage_dir) == 69);
    assert!(offset_of!(C, sender_pci) == 4165);
    assert!(offset_of!(C, sender_ipv4) == 4181);
    assert!(offset_of!(C, sender_prefix_len) == 4197);
    assert!(offset_of!(C, sender_gateway) == 4198);
    assert!(offset_of!(C, receiver_pci) == 4214);
    assert!(offset_of!(C, receiver_ipv4) == 4230);
    assert!(offset_of!(C, receiver_prefix_len) == 4246);
    assert!(offset_of!(C, receiver_gateway) == 4247);
    assert!(offset_of!(C, rx_descriptors) == 4264);
    assert!(offset_of!(C, tx_descriptors) == 4266);
    assert!(offset_of!(C, mbufs_per_port) == 4268);
    assert!(offset_of!(C, mempool_cache) == 4272);
    assert!(offset_of!(C, burst_size) == 4274);
    assert!(offset_of!(C, max_aeron_mtu) == 4280);
    assert!(offset_of!(C, sender_mac) == 4288);
    assert!(offset_of!(C, receiver_mac) == 4306);
    assert!(offset_of!(C, rx_hw_timestamp) == 4324);
    assert!(size_of::<C>() == 4328);
};

#[cfg(feature = "dpdk")]
#[link(name = "rusteron_dpdk", kind = "static")]
unsafe extern "C" {
    fn rusteron_dpdk_transport_create(config: *const rusteron_dpdk_config_t, transport: *mut *mut c_void) -> c_int;
    fn rusteron_dpdk_transport_install(
        transport: *mut c_void,
        context: *mut rusteron_media_driver::bindings::aeron_driver_context_t,
    ) -> c_int;
    fn rusteron_dpdk_transport_close(transport: *mut c_void) -> c_int;
    fn rusteron_dpdk_last_error() -> *const c_char;
}

#[cfg(feature = "dpdk")]
fn native_config(config: &DpdkTransportConfig) -> Result<rusteron_dpdk_config_t, DpdkError> {
    let mut c = rusteron_dpdk_config_t {
        struct_size: u32::try_from(std::mem::size_of::<rusteron_dpdk_config_t>())
            .map_err(|_| DpdkError::InvalidConfiguration("native config struct exceeds u32".to_string()))?,
        file_prefix: [0; 65],
        hugepage_dir: [0; 4096],
        sender_pci: [0; 16],
        sender_ipv4: [0; 16],
        sender_prefix_len: config.sender.prefix_len,
        sender_gateway: [0; 16],
        receiver_pci: [0; 16],
        receiver_ipv4: [0; 16],
        receiver_prefix_len: config.receiver.prefix_len,
        receiver_gateway: [0; 16],
        rx_descriptors: config.rx_descriptors,
        tx_descriptors: config.tx_descriptors,
        mbufs_per_port: config.mbufs_per_port,
        mempool_cache: config.mempool_cache,
        burst_size: config.burst_size,
        max_aeron_mtu: config.max_aeron_mtu,
        sender_mac: [0; 18],
        receiver_mac: [0; 18],
        rx_hw_timestamp: u8::from(config.rx_hw_timestamp),
    };
    if let Some(mac) = config.sender.mac {
        fill(&mut c.sender_mac, &mac.to_string(), "sender.mac")?;
    }
    if let Some(mac) = config.receiver.mac {
        fill(&mut c.receiver_mac, &mac.to_string(), "receiver.mac")?;
    }
    fill(&mut c.file_prefix, &config.file_prefix, "file_prefix")?;
    let huge_dir = config
        .hugepage_dir
        .to_str()
        .ok_or_else(|| DpdkError::InvalidConfiguration("hugepage_dir is not valid UTF-8".to_string()))?;
    fill(&mut c.hugepage_dir, huge_dir, "hugepage_dir")?;
    fill(&mut c.sender_pci, &config.sender.pci_address, "sender.pci_address")?;
    fill(
        &mut c.sender_ipv4,
        &config.sender.local_ipv4.to_string(),
        "sender.local_ipv4",
    )?;
    fill(
        &mut c.sender_gateway,
        &config.sender.gateway_ipv4.to_string(),
        "sender.gateway_ipv4",
    )?;
    fill(
        &mut c.receiver_pci,
        &config.receiver.pci_address,
        "receiver.pci_address",
    )?;
    fill(
        &mut c.receiver_ipv4,
        &config.receiver.local_ipv4.to_string(),
        "receiver.local_ipv4",
    )?;
    fill(
        &mut c.receiver_gateway,
        &config.receiver.gateway_ipv4.to_string(),
        "receiver.gateway_ipv4",
    )?;
    Ok(c)
}

/// Copy a NUL-terminated string into a fixed C buffer.
#[cfg(feature = "dpdk")]
fn fill(buf: &mut [c_char], value: &str, field: &str) -> Result<(), DpdkError> {
    let bytes = value.as_bytes();
    if bytes.len() >= buf.len() {
        return Err(DpdkError::InvalidConfiguration(format!(
            "{field} value {value:?} is too long for the native config buffer"
        )));
    }
    for (i, b) in bytes.iter().enumerate() {
        buf[i] = c_char::from_ne_bytes([*b]);
    }
    buf[bytes.len()] = 0;
    Ok(())
}

#[cfg(feature = "dpdk")]
fn last_error() -> String {
    // SAFETY: returns a thread-local NUL-terminated string or NULL.
    let ptr = unsafe { rusteron_dpdk_last_error() };
    if ptr.is_null() {
        "<null>".to_string()
    } else {
        // SAFETY: non-null and NUL-terminated, valid until the next native call.
        unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
    }
}

/// Verify the required `AeronDriverContext` state. The DPDK
/// transport replaces only the sender/receiver bindings; the conductor's UDP
/// resolver stays on the kernel socket, so the context must still be a sane
/// dedicated-thread driver configuration.
#[cfg(feature = "dpdk")]
fn validate_context(context: &AeronDriverContext, config: &DpdkTransportConfig) -> Result<(), DpdkError> {
    use rusteron_media_driver::bindings::aeron_threading_mode_t;

    if context.get_threading_mode() != aeron_threading_mode_t::AERON_THREADING_MODE_DEDICATED {
        return Err(DpdkError::InvalidConfiguration(
            "aeron context threading mode must be AERON_THREADING_MODE_DEDICATED".to_string(),
        ));
    }

    let sender_aff = context.get_sender_cpu_affinity();
    let receiver_aff = context.get_receiver_cpu_affinity();
    if sender_aff < 0 || receiver_aff < 0 {
        return Err(DpdkError::InvalidConfiguration(format!(
            "aeron context sender/receiver cpu affinity must be nonnegative (sender={sender_aff}, receiver={receiver_aff})"
        )));
    }
    if sender_aff == receiver_aff {
        return Err(DpdkError::InvalidConfiguration(format!(
            "aeron context sender and receiver cpu affinity must differ (both {sender_aff})"
        )));
    }

    let sender_idle = effective_idle_strategy(context.get_sender_idle_strategy(), "AERON_SENDER_IDLE_STRATEGY");
    let receiver_idle = effective_idle_strategy(context.get_receiver_idle_strategy(), "AERON_RECEIVER_IDLE_STRATEGY");
    if !is_busy_idle_strategy(&sender_idle) || !is_busy_idle_strategy(&receiver_idle) {
        return Err(DpdkError::InvalidConfiguration(format!(
            "aeron context sender and receiver idle strategies must busy-spin (`spin` or `noop`), not \
             {sender_idle} and {receiver_idle}"
        )));
    }

    let (s_low, s_high) = wildcard_port_range(context, "sender")?;
    let (r_low, r_high) = wildcard_port_range(context, "receiver")?;
    for (role, low, high) in [("sender", s_low, s_high), ("receiver", r_low, r_high)] {
        if low == 0 || high == 0 {
            return Err(DpdkError::InvalidConfiguration(format!(
                "aeron context {role} wildcard port range ({low}-{high}) must be nonzero"
            )));
        }
        if low >= high {
            return Err(DpdkError::InvalidConfiguration(format!(
                "aeron context {role} wildcard port range ({low}-{high}) must satisfy low < high"
            )));
        }
    }
    if !(s_high < r_low || r_high < s_low) {
        return Err(DpdkError::InvalidConfiguration(format!(
            "aeron context sender wildcard port range ({s_low}-{s_high}) and receiver ({r_low}-{r_high}) must be disjoint"
        )));
    }

    let mtu = context.get_mtu_length();
    if mtu > config.max_aeron_mtu {
        return Err(DpdkError::InvalidConfiguration(format!(
            "aeron context mtu {mtu} exceeds max_aeron_mtu {}",
            config.max_aeron_mtu
        )));
    }

    Ok(())
}

/// Pin the driver's agent threads to the CPUs the context names
/// (`AERON_CONDUCTOR_CPU_AFFINITY`, `AERON_SENDER_CPU_AFFINITY`,
/// `AERON_RECEIVER_CPU_AFFINITY` or their setters), as Aeron's `aeronmd` does.
///
/// Aeron's C driver only parses those values; `aeronmd` applies them by
/// installing `aeron_set_thread_affinity_on_start` as the agent start hook,
/// which rusteron's driver does not. Without it the agents float, and under
/// DPDK they inherit the CPU the EAL pinned its main thread to, which puts the
/// sender and receiver on one core. [`DpdkTransport::install`] calls this; call
/// it yourself for the socket transport. It replaces any agent start hook the
/// context already has. Call it before the driver is created.
///
/// # Errors
///
/// [`DpdkError::Aeron`] if the context rejects the hook.
pub fn pin_agents_to_configured_cpus(context: &AeronDriverContext) -> Result<(), DpdkError> {
    use rusteron_media_driver::bindings::{
        aeron_driver_context_set_agent_on_start_function, aeron_set_thread_affinity_on_start,
    };
    let inner = context.get_inner();
    // SAFETY: Aeron's hook reads the driver context it is given as state, and
    // the agents it runs on are started from, and joined before, that context.
    let rc = unsafe {
        aeron_driver_context_set_agent_on_start_function(inner, Some(aeron_set_thread_affinity_on_start), inner.cast())
    };
    if rc < 0 {
        return Err(DpdkError::Aeron(format!(
            "aeron_driver_context_set_agent_on_start_function failed ({rc})"
        )));
    }
    Ok(())
}

/// The idle strategy the agent actually runs. Aeron's context init loads the
/// strategy `AERON_*_IDLE_STRATEGY` names but leaves the stored name at its
/// `backoff` default, so the getter alone misreads a driver configured from the
/// environment (the standalone binary's way); a setter updates both.
#[cfg(feature = "dpdk")]
fn effective_idle_strategy(stored: &str, env_var: &str) -> String {
    match std::env::var(env_var) {
        Ok(from_env) if stored == "backoff" && !from_env.is_empty() => from_env,
        _ => stored.to_string(),
    }
}

/// `spin` (busy spin with a pause) and `noop` both keep the agent polling the
/// NIC; anything that sleeps or yields would stall a DPDK queue.
#[cfg(feature = "dpdk")]
fn is_busy_idle_strategy(name: &str) -> bool {
    matches!(
        name,
        "spin" | "noop" | "aeron_idle_strategy_busy_spinning" | "aeron_idle_strategy_noop"
    )
}

// ponytail: role is the C getter suffix in the error message — keep it
// byte-identical to the bindings' aeron_driver_context_get_{role}_wildcard_port_range.
#[cfg(feature = "dpdk")]
fn wildcard_port_range(context: &AeronDriverContext, role: &str) -> Result<(u16, u16), DpdkError> {
    let (mut low, mut high) = (0u16, 0u16);
    let result = if role == "sender" {
        context.get_sender_wildcard_port_range(&mut low, &mut high)
    } else {
        context.get_receiver_wildcard_port_range(&mut low, &mut high)
    };
    result.map_err(|e| {
        DpdkError::Aeron(format!(
            "aeron_driver_context_get_{role}_wildcard_port_range failed: {e}"
        ))
    })?;
    Ok((low, high))
}
