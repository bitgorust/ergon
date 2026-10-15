//! Integration tests: typed configuration, environment parsing, and lifetime
//! ownership (plan §5.1/§5.2/§6).
//!
//! Pure config/env/error behaviour runs on every platform with the default
//! features; the full install/lifetime path is `#[cfg(feature = "dpdk")]`
//! (Linux x86_64 in the Docker verification image).
//!
//! These tests mutate process-global env vars, so every test is `#[serial]`
//! (file-locked across the whole workspace, as with the other DPDK test files).
#![allow(
    missing_docs,
    unsafe_code,
    unsafe_op_in_unsafe_fn,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction
)]

use serial_test::serial;

use ergon_md_dpdk::config::DpdkTransportConfig;
use ergon_md_dpdk::env;
use ergon_md_dpdk::error::DpdkError;
use ergon_md_dpdk::{DpdkPortConfig, DpdkTransport, configure_media_transport_from_env};
use rusteron_media_driver::AeronDriverContext;

// Native libraries and the DPDK-free fakes are declared in `common/mod.rs`
// (cargo forwards build-script link-libs to the lib, not to same-package
// integration tests). Feature-gated install tests drive the transport under
// the skip-EAL test mode.
mod common;
#[cfg(feature = "dpdk")]
use common::TestEnv;

// SAFETY (both helpers): every test in this file is `#[serial]` and the lane
// runs with `--test-threads=1`, so no other thread reads the environment while
// it changes.
fn set_var(key: &str, value: &str) {
    unsafe { std::env::set_var(key, value) }
}

fn remove_var(key: &str) {
    unsafe { std::env::remove_var(key) }
}

const ENV_VARS: &[&str] = &[
    "RUSTERON_MEDIA_DRIVER_TRANSPORT",
    "RUSTERON_DPDK_FILE_PREFIX",
    "RUSTERON_DPDK_SENDER_PCI",
    "RUSTERON_DPDK_SENDER_IPV4_CIDR",
    "RUSTERON_DPDK_SENDER_GATEWAY",
    "RUSTERON_DPDK_RECEIVER_PCI",
    "RUSTERON_DPDK_RECEIVER_IPV4_CIDR",
    "RUSTERON_DPDK_RECEIVER_GATEWAY",
    "RUSTERON_DPDK_HUGE_DIR",
    "RUSTERON_DPDK_RX_DESCRIPTORS",
    "RUSTERON_DPDK_TX_DESCRIPTORS",
    "RUSTERON_DPDK_MBUFS_PER_PORT",
    "RUSTERON_DPDK_MEMPOOL_CACHE",
    "RUSTERON_DPDK_BURST_SIZE",
    "RUSTERON_DPDK_MAX_AERON_MTU",
    "RUSTERON_DPDK_SENDER_MAC",
    "RUSTERON_DPDK_RECEIVER_MAC",
    "RUSTERON_DPDK_RX_HW_TIMESTAMP",
];

fn clear_env() {
    for var in ENV_VARS {
        remove_var(var);
    }
}

/// A valid configuration matching the plan's recommended standalone settings.
fn canonical() -> DpdkTransportConfig {
    DpdkTransportConfig {
        sender: DpdkPortConfig {
            pci_address: "0000:00:01.0".into(),
            mac: None,
            local_ipv4: "10.0.0.1".parse().unwrap(),
            prefix_len: 24,
            gateway_ipv4: "10.0.0.254".parse().unwrap(),
        },
        receiver: DpdkPortConfig {
            pci_address: "0000:00:02.0".into(),
            mac: None,
            local_ipv4: "10.0.1.1".parse().unwrap(),
            prefix_len: 24,
            gateway_ipv4: "10.0.1.254".parse().unwrap(),
        },
        file_prefix: "rusteron-ena".into(),
        test_vdev: false,
        hugepage_dir: "/dev/hugepages".into(),
        rx_descriptors: 1024,
        tx_descriptors: 1024,
        mbufs_per_port: 65536,
        mempool_cache: 256,
        burst_size: 32,
        max_aeron_mtu: 1408,
        rx_hw_timestamp: false,
    }
}

fn set_env_from_canonical() {
    set_var("RUSTERON_DPDK_FILE_PREFIX", "rusteron-ena");
    set_var("RUSTERON_DPDK_SENDER_PCI", "0000:00:01.0");
    set_var("RUSTERON_DPDK_SENDER_IPV4_CIDR", "10.0.0.1/24");
    set_var("RUSTERON_DPDK_SENDER_GATEWAY", "10.0.0.254");
    set_var("RUSTERON_DPDK_RECEIVER_PCI", "0000:00:02.0");
    set_var("RUSTERON_DPDK_RECEIVER_IPV4_CIDR", "10.0.1.1/24");
    set_var("RUSTERON_DPDK_RECEIVER_GATEWAY", "10.0.1.254");
    set_var("RUSTERON_DPDK_HUGE_DIR", "/dev/hugepages");
    set_var("RUSTERON_DPDK_RX_DESCRIPTORS", "1024");
    set_var("RUSTERON_DPDK_TX_DESCRIPTORS", "1024");
    set_var("RUSTERON_DPDK_MBUFS_PER_PORT", "65536");
    set_var("RUSTERON_DPDK_MEMPOOL_CACHE", "256");
    set_var("RUSTERON_DPDK_BURST_SIZE", "32");
    set_var("RUSTERON_DPDK_MAX_AERON_MTU", "1408");
}

/// Assert a validation error that names `needle` (the offending field).
/// `RUSTERON_MEDIA_DRIVER_MLOCKALL` takes 0 or 1 (unset: 0, nothing locked);
/// anything else is refused, naming the variable, before the driver starts.
#[serial]
#[test]
fn mlockall_setting_is_validated() {
    remove_var("RUSTERON_MEDIA_DRIVER_MLOCKALL");
    assert!(!ergon_md_dpdk::lock_memory_from_env().unwrap(), "unset: nothing locked");
    set_var("RUSTERON_MEDIA_DRIVER_MLOCKALL", "0");
    assert!(!ergon_md_dpdk::lock_memory_from_env().unwrap(), "0: nothing locked");
    for bad in ["yes", "2", "true"] {
        set_var("RUSTERON_MEDIA_DRIVER_MLOCKALL", bad);
        let e = ergon_md_dpdk::lock_memory_from_env().unwrap_err();
        assert!(e.to_string().contains("RUSTERON_MEDIA_DRIVER_MLOCKALL"), "{bad}: {e}");
    }
    remove_var("RUSTERON_MEDIA_DRIVER_MLOCKALL");
}

fn assert_invalid_config(config: &DpdkTransportConfig, needle: &str) {
    let err = config.validate().unwrap_err();
    assert!(
        matches!(err, DpdkError::InvalidConfiguration(_)),
        "expected InvalidConfiguration, got {err:?}"
    );
    let msg = err.to_string();
    assert!(msg.contains(needle), "message {msg:?} must name {needle:?}");
}

// --- Acceptance 1: typed and environment configurations resolve identically ---

#[serial]
#[test]
fn typed_and_env_configs_resolve_identical() {
    clear_env();
    set_env_from_canonical();
    assert_eq!(env::config_from_env().unwrap(), canonical());
}

// --- Validation rules (plan §6.5): each error names the offending field ------

#[serial]
#[test]
fn validation_rejects_bad_sender_pci() {
    let mut c = canonical();
    c.sender.pci_address = "00:01.0".into(); // missing domain
    assert_invalid_config(&c, "sender.pci_address");
}

// --- Test-only vdev selectors (plan §11.2): production stays PCI-only --------

#[serial]
#[test]
fn validation_rejects_vdev_name_outside_test_mode() {
    let mut c = canonical();
    c.sender.pci_address = "net_tap0".into();
    c.receiver.pci_address = "net_tap1".into();
    assert_invalid_config(&c, "sender.pci_address");
}

#[serial]
#[test]
fn validation_accepts_vdev_names_in_test_mode() {
    let mut c = canonical();
    c.test_vdev = true;
    c.sender.pci_address = "net_tap0".into();
    c.receiver.pci_address = "net_tap1".into();
    c.validate().unwrap();
}

#[serial]
#[test]
fn validation_rejects_bad_vdev_name_in_test_mode() {
    let mut c = canonical();
    c.test_vdev = true;
    c.sender.pci_address = "0000:00:01.0".into(); // PCI is always valid
    c.receiver.pci_address = "net_tap-with:colon".into(); // ':' would misdetect as PCI
    assert_invalid_config(&c, "receiver.pci_address");
}

#[serial]
#[test]
fn validation_rejects_duplicate_vdev_in_test_mode() {
    let mut c = canonical();
    c.test_vdev = true;
    c.sender.pci_address = "net_tap0".into();
    c.receiver.pci_address = "net_tap0".into();
    assert_invalid_config(&c, "must differ");
}

#[serial]
#[test]
fn validation_rejects_duplicate_pci() {
    let mut c = canonical();
    c.receiver.pci_address = c.sender.pci_address.clone();
    assert_invalid_config(&c, "must differ");
}

// --- Azure MANA: one PCI function, ports selected by MAC ----------------------

/// MANA exposes every accelerated NIC of a VM on one PCI function, so both
/// roles share the PCI address and are told apart by MAC.
fn mana(c: &mut DpdkTransportConfig) {
    c.receiver.pci_address = "7870:00:00.0".into();
    c.sender.pci_address = "7870:00:00.0".into();
    c.sender.mac = Some("00:0d:3a:76:3b:d0".parse().unwrap());
    c.receiver.mac = Some("00:0d:3a:76:3b:d1".parse().unwrap());
}

#[serial]
#[test]
fn validation_accepts_shared_pci_with_distinct_macs() {
    let mut c = canonical();
    mana(&mut c);
    c.validate().unwrap();
}

#[serial]
#[test]
fn validation_rejects_shared_pci_with_identical_macs() {
    let mut c = canonical();
    mana(&mut c);
    c.receiver.mac = c.sender.mac;
    assert_invalid_config(&c, "must differ");
}

#[serial]
#[test]
fn validation_rejects_shared_pci_when_a_mac_is_missing() {
    let mut c = canonical();
    mana(&mut c);
    c.receiver.mac = None;
    assert_invalid_config(&c, "must differ");
}

#[serial]
#[test]
fn validation_rejects_multicast_port_mac() {
    let mut c = canonical();
    mana(&mut c);
    c.sender.mac = Some("01:00:5e:00:00:01".parse().unwrap());
    assert_invalid_config(&c, "sender.mac");
}

#[serial]
#[test]
fn mac_parses_either_case_and_displays_lowercase() {
    let mac: ergon_md_dpdk::MacAddr = "00:0D:3A:76:3B:D0".parse().unwrap();
    assert_eq!(mac.0, [0x00, 0x0d, 0x3a, 0x76, 0x3b, 0xd0]);
    assert_eq!(mac.to_string(), "00:0d:3a:76:3b:d0");
    assert!("00:0d:3a:76:3b".parse::<ergon_md_dpdk::MacAddr>().is_err());
    assert!("00-0d-3a-76-3b-d0".parse::<ergon_md_dpdk::MacAddr>().is_err());
    assert!("00:0d:3a:76:3b:zz".parse::<ergon_md_dpdk::MacAddr>().is_err());
}

#[serial]
#[test]
fn env_reads_optional_port_macs() {
    clear_env();
    set_env_from_canonical();
    set_var("RUSTERON_DPDK_SENDER_PCI", "7870:00:00.0");
    set_var("RUSTERON_DPDK_RECEIVER_PCI", "7870:00:00.0");
    set_var("RUSTERON_DPDK_SENDER_MAC", "00:0d:3a:76:3b:d0");
    set_var("RUSTERON_DPDK_RECEIVER_MAC", "00:0D:3A:76:3B:D1");
    let c = env::config_from_env().unwrap();
    assert_eq!(c.sender.mac.unwrap().to_string(), "00:0d:3a:76:3b:d0");
    assert_eq!(c.receiver.mac.unwrap().to_string(), "00:0d:3a:76:3b:d1");
    c.validate().unwrap();
    clear_env();
}

#[serial]
#[test]
fn env_leaves_port_macs_unset_by_default() {
    clear_env();
    set_env_from_canonical();
    let c = env::config_from_env().unwrap();
    assert_eq!(c.sender.mac, None);
    assert_eq!(c.receiver.mac, None);
    clear_env();
}

/// `RUSTERON_DPDK_RX_HW_TIMESTAMP` turns on NIC receive timestamps; it is
/// off by default and accepts only 0/1/true/false.
#[serial]
#[test]
fn env_reads_rx_hw_timestamp_flag() {
    clear_env();
    set_env_from_canonical();
    assert!(!env::config_from_env().unwrap().rx_hw_timestamp, "off by default");
    for (value, expected) in [
        ("1", true),
        ("true", true),
        ("TRUE", true),
        ("0", false),
        ("false", false),
    ] {
        set_var("RUSTERON_DPDK_RX_HW_TIMESTAMP", value);
        assert_eq!(env::config_from_env().unwrap().rx_hw_timestamp, expected, "{value}");
    }
    clear_env();
}

#[serial]
#[test]
fn env_rejects_malformed_rx_hw_timestamp() {
    clear_env();
    set_env_from_canonical();
    set_var("RUSTERON_DPDK_RX_HW_TIMESTAMP", "yes");
    let err = env::config_from_env().unwrap_err();
    assert!(matches!(err, DpdkError::InvalidEnvironment(_)), "got {err:?}");
    assert!(err.to_string().contains("RUSTERON_DPDK_RX_HW_TIMESTAMP"), "got {err}");
    clear_env();
}

#[serial]
#[test]
fn env_rejects_malformed_port_mac() {
    clear_env();
    set_env_from_canonical();
    set_var("RUSTERON_DPDK_SENDER_MAC", "not-a-mac");
    let err = env::config_from_env().unwrap_err();
    assert!(matches!(err, DpdkError::InvalidEnvironment(_)), "got {err:?}");
    assert!(err.to_string().contains("RUSTERON_DPDK_SENDER_MAC"), "got {err}");
    clear_env();
}

#[serial]
#[test]
fn validation_rejects_multicast_local_ip() {
    let mut c = canonical();
    c.sender.local_ipv4 = "224.0.0.1".parse().unwrap();
    assert_invalid_config(&c, "sender.local_ipv4");
}

#[serial]
#[test]
fn validation_rejects_broadcast_gateway() {
    let mut c = canonical();
    c.receiver.gateway_ipv4 = "255.255.255.255".parse().unwrap();
    assert_invalid_config(&c, "receiver.gateway_ipv4");
}

#[serial]
#[test]
fn validation_rejects_zero_prefix_len() {
    let mut c = canonical();
    c.receiver.prefix_len = 0;
    assert_invalid_config(&c, "receiver.prefix_len");
}

#[serial]
#[test]
fn validation_rejects_prefix_len_above_32() {
    let mut c = canonical();
    c.sender.prefix_len = 33;
    assert_invalid_config(&c, "sender.prefix_len");
}

#[serial]
#[test]
fn validation_rejects_gateway_outside_subnet() {
    let mut c = canonical();
    c.sender.gateway_ipv4 = "10.0.1.254".parse().unwrap(); // receiver's subnet
    assert_invalid_config(&c, "sender.gateway_ipv4");
}

#[serial]
#[test]
fn validation_rejects_bad_file_prefix() {
    let mut c = canonical();
    c.file_prefix = "rusteron/ena".into(); // '/' not allowed
    assert_invalid_config(&c, "file_prefix");
}

#[serial]
#[test]
fn validation_rejects_empty_file_prefix() {
    let mut c = canonical();
    c.file_prefix = String::new();
    assert_invalid_config(&c, "file_prefix");
}

#[serial]
#[test]
fn validation_rejects_relative_hugepage_dir() {
    let mut c = canonical();
    c.hugepage_dir = "hugepages".into();
    assert_invalid_config(&c, "hugepage_dir");
}

#[serial]
#[test]
fn validation_rejects_rx_descriptors_out_of_range() {
    let mut c = canonical();
    c.rx_descriptors = 63;
    assert_invalid_config(&c, "rx_descriptors");
}

#[serial]
#[test]
fn validation_rejects_tx_descriptors_out_of_range() {
    let mut c = canonical();
    c.tx_descriptors = 8193;
    assert_invalid_config(&c, "tx_descriptors");
}

#[serial]
#[test]
fn validation_rejects_zero_burst() {
    let mut c = canonical();
    c.burst_size = 0;
    assert_invalid_config(&c, "burst_size");
}

#[serial]
#[test]
fn validation_rejects_burst_above_256() {
    let mut c = canonical();
    c.burst_size = 257;
    assert_invalid_config(&c, "burst_size");
}

/// DPDK takes a per-core mempool cache of 0 (none) to 512 entries: 0 is
/// honoured, not replaced, and more than 512 is reported here instead of
/// failing inside the pool's creation.
#[serial]
#[test]
fn validation_bounds_the_mempool_cache() {
    let mut c = canonical();
    c.mempool_cache = 0;
    assert!(c.validate().is_ok(), "no cache is a valid choice");
    c.mempool_cache = 512;
    assert!(c.validate().is_ok());
    c.mempool_cache = 513;
    assert_invalid_config(&c, "mempool_cache");
}

#[serial]
#[test]
fn validation_rejects_undersized_mbuf_pool() {
    let mut c = canonical();
    // rx + tx + 4*burst = 1024 + 1024 + 128 = 2176; one below is too small.
    c.mbufs_per_port = 2175;
    assert_invalid_config(&c, "mbufs_per_port");
}

#[serial]
#[test]
fn validation_rejects_unaligned_mtu() {
    let mut c = canonical();
    c.max_aeron_mtu = 1400; // not a multiple of 32
    assert_invalid_config(&c, "max_aeron_mtu");
}

#[serial]
#[test]
fn validation_rejects_mtu_above_8960_and_accepts_jumbo() {
    let mut c = canonical();
    c.max_aeron_mtu = 8992;
    assert_invalid_config(&c, "max_aeron_mtu");
    c.max_aeron_mtu = 8960; // an AWS 9001-byte VPC MTU less IPv4+UDP
    c.validate().unwrap();
}

// --- Selector behaviour (plan §6.1) ------------------------------------------

#[serial]
#[test]
fn absent_selector_preserves_default() {
    clear_env();
    let ctx = AeronDriverContext::new().unwrap();
    let result = configure_media_transport_from_env(&ctx).unwrap();
    assert!(result.is_none(), "absent selector must preserve default behaviour");
}

#[serial]
#[test]
fn default_selector_preserves_default() {
    clear_env();
    set_var("RUSTERON_MEDIA_DRIVER_TRANSPORT", "default");
    let ctx = AeronDriverContext::new().unwrap();
    let result = configure_media_transport_from_env(&ctx).unwrap();
    assert!(result.is_none(), "`default` selector must preserve default behaviour");
}

#[serial]
#[test]
fn unknown_selector_is_invalid_environment() {
    clear_env();
    set_var("RUSTERON_MEDIA_DRIVER_TRANSPORT", "vanilla");
    let ctx = AeronDriverContext::new().unwrap();
    let err = configure_media_transport_from_env(&ctx).unwrap_err();
    assert!(matches!(err, DpdkError::InvalidEnvironment(_)), "got {err:?}");
    assert!(err.to_string().contains("RUSTERON_MEDIA_DRIVER_TRANSPORT"));
}

#[serial]
#[test]
fn empty_selector_is_invalid_environment() {
    clear_env();
    set_var("RUSTERON_MEDIA_DRIVER_TRANSPORT", "");
    let ctx = AeronDriverContext::new().unwrap();
    let err = configure_media_transport_from_env(&ctx).unwrap_err();
    assert!(matches!(err, DpdkError::InvalidEnvironment(_)), "got {err:?}");
}

// --- Environment parsing failures (plan §6.2/§6.3) ---------------------------

#[serial]
#[test]
fn config_from_env_reports_missing_variable() {
    clear_env();
    set_env_from_canonical();
    remove_var("RUSTERON_DPDK_SENDER_PCI");
    let err = env::config_from_env().unwrap_err();
    assert!(matches!(err, DpdkError::MissingEnvironment(_)), "got {err:?}");
    assert!(err.to_string().contains("RUSTERON_DPDK_SENDER_PCI"));
}

#[serial]
#[test]
fn config_from_env_reports_cidr_without_prefix() {
    clear_env();
    set_env_from_canonical();
    set_var("RUSTERON_DPDK_SENDER_IPV4_CIDR", "10.0.0.1");
    let err = env::config_from_env().unwrap_err();
    assert!(matches!(err, DpdkError::InvalidEnvironment(_)), "got {err:?}");
    assert!(err.to_string().contains("SENDER_IPV4_CIDR"));
}

#[serial]
#[test]
fn config_from_env_reports_bad_prefix_number() {
    clear_env();
    set_env_from_canonical();
    set_var("RUSTERON_DPDK_SENDER_IPV4_CIDR", "10.0.0.1/33");
    let err = env::config_from_env().unwrap_err();
    assert!(matches!(err, DpdkError::InvalidEnvironment(_)), "got {err:?}");
    assert!(err.to_string().contains("prefix"));
}

#[serial]
#[test]
fn config_from_env_reports_bad_gateway() {
    clear_env();
    set_env_from_canonical();
    set_var("RUSTERON_DPDK_RECEIVER_GATEWAY", "not-an-ip");
    let err = env::config_from_env().unwrap_err();
    assert!(matches!(err, DpdkError::InvalidEnvironment(_)), "got {err:?}");
    assert!(err.to_string().contains("RECEIVER_GATEWAY"));
}

#[serial]
#[test]
fn config_from_env_reports_bad_number_default() {
    clear_env();
    set_env_from_canonical();
    set_var("RUSTERON_DPDK_BURST_SIZE", "lots");
    let err = env::config_from_env().unwrap_err();
    assert!(matches!(err, DpdkError::InvalidEnvironment(_)), "got {err:?}");
    assert!(err.to_string().contains("RUSTERON_DPDK_BURST_SIZE"));
}

#[serial]
#[test]
fn env_defaults_match_plan() {
    clear_env();
    set_env_from_canonical();
    for var in [
        "RUSTERON_DPDK_HUGE_DIR",
        "RUSTERON_DPDK_RX_DESCRIPTORS",
        "RUSTERON_DPDK_TX_DESCRIPTORS",
        "RUSTERON_DPDK_MBUFS_PER_PORT",
        "RUSTERON_DPDK_MEMPOOL_CACHE",
        "RUSTERON_DPDK_BURST_SIZE",
        "RUSTERON_DPDK_MAX_AERON_MTU",
    ] {
        remove_var(var);
    }
    let c = env::config_from_env().unwrap();
    assert_eq!(c.hugepage_dir, std::path::PathBuf::from("/dev/hugepages"));
    assert_eq!(c.rx_descriptors, 1024);
    assert_eq!(c.tx_descriptors, 512); // an ENA's maximum
    assert_eq!(c.mbufs_per_port, 16384, "what the lab measured");
    assert_eq!(c.mempool_cache, 256);
    assert_eq!(c.burst_size, 32);
    assert_eq!(c.max_aeron_mtu, 1408);
}

// --- Feature disabled (runs everywhere without `dpdk`) ------------------------

#[cfg(not(feature = "dpdk"))]
#[serial]
#[test]
fn dpdk_ena_without_feature_is_feature_disabled() {
    clear_env();
    set_var("RUSTERON_MEDIA_DRIVER_TRANSPORT", "dpdk-ena");
    set_env_from_canonical(); // env is fully set; the feature must still win
    let ctx = AeronDriverContext::new().unwrap();
    let err = configure_media_transport_from_env(&ctx).unwrap_err();
    assert!(matches!(err, DpdkError::FeatureDisabled), "got {err:?}");
}

#[cfg(not(feature = "dpdk"))]
#[serial]
#[test]
fn install_without_feature_is_feature_disabled() {
    clear_env();
    let ctx = AeronDriverContext::new().unwrap();
    let err = DpdkTransport::install(&ctx, &canonical()).unwrap_err();
    assert!(matches!(err, DpdkError::FeatureDisabled), "got {err:?}");
}

// --- Install + lifetime (plan §5.2, feature-gated) ----------------------------

/// A context meeting the plan §6.4 required state (see §6.4 recommended values).
#[cfg(feature = "dpdk")]
fn configured_context() -> AeronDriverContext {
    use rusteron_media_driver::AeronIdleStrategyKind;
    let ctx = AeronDriverContext::new().unwrap();
    ctx.set_sender_cpu_affinity(1).unwrap();
    ctx.set_receiver_cpu_affinity(2).unwrap();
    ctx.set_sender_idle_strategy_kind(AeronIdleStrategyKind::BusySpin)
        .unwrap();
    ctx.set_receiver_idle_strategy_kind(AeronIdleStrategyKind::BusySpin)
        .unwrap();
    ctx.set_sender_wildcard_port_range(20000, 20999).unwrap();
    ctx.set_receiver_wildcard_port_range(21000, 21999).unwrap();
    ctx.set_mtu_length(1408).unwrap();
    ctx
}

/// The configured context, but with the idle strategies from the environment
/// (`value` for both), as the standalone `media_driver` gets them. Aeron loads
/// the strategy the variable names but leaves the stored name at `backoff`.
/// The variables stay set, as in the driver's process; `clear_idle_env` unsets them.
#[cfg(feature = "dpdk")]
fn context_with_env_idle(value: &str) -> AeronDriverContext {
    set_var("AERON_SENDER_IDLE_STRATEGY", value);
    set_var("AERON_RECEIVER_IDLE_STRATEGY", value);
    let ctx = AeronDriverContext::new().unwrap();
    ctx.set_sender_cpu_affinity(1).unwrap();
    ctx.set_receiver_cpu_affinity(2).unwrap();
    ctx.set_sender_wildcard_port_range(20000, 20999).unwrap();
    ctx.set_receiver_wildcard_port_range(21000, 21999).unwrap();
    ctx.set_mtu_length(1408).unwrap();
    ctx
}

#[cfg(feature = "dpdk")]
#[serial]
#[test]
fn install_accepts_busy_idle_strategies_set_by_the_environment() {
    for value in ["spin", "noop", "aeron_idle_strategy_busy_spinning"] {
        let env = TestEnv::new();
        env.eal_skip();
        let ctx = context_with_env_idle(value);
        let result = DpdkTransport::install(&ctx, &canonical());
        clear_idle_env();
        drop(result.unwrap_or_else(|e| panic!("{value}: {e}")));
    }
}

#[cfg(feature = "dpdk")]
fn clear_idle_env() {
    remove_var("AERON_SENDER_IDLE_STRATEGY");
    remove_var("AERON_RECEIVER_IDLE_STRATEGY");
}

#[cfg(feature = "dpdk")]
#[serial]
#[test]
fn install_rejects_a_sleeping_idle_strategy_set_by_the_environment() {
    let env = TestEnv::new();
    env.eal_skip();
    let ctx = context_with_env_idle("backoff");
    let result = DpdkTransport::install(&ctx, &canonical());
    clear_idle_env();
    let err = result.unwrap_err();
    assert!(err.to_string().contains("idle"), "got {err}");
}

#[cfg(feature = "dpdk")]
#[serial]
#[test]
fn install_succeeds_on_configured_context() {
    let env = TestEnv::new();
    env.eal_skip();
    let ctx = configured_context();
    let guard = DpdkTransport::install(&ctx, &canonical()).unwrap();
    drop(guard); // the last owner: closes the native transport
}

#[cfg(feature = "dpdk")]
#[serial]
#[test]
fn install_rejects_misconfigured_context() {
    let _env = TestEnv::new();
    // Default context: sender/receiver cpu affinity = -1 (§6.4 requirement fails).
    let ctx = AeronDriverContext::new().unwrap();
    let err = DpdkTransport::install(&ctx, &canonical()).unwrap_err();
    assert!(matches!(err, DpdkError::InvalidConfiguration(_)), "got {err:?}");
    assert!(err.to_string().contains("cpu affinity"), "got {err}");
}

#[cfg(feature = "dpdk")]
#[serial]
#[test]
fn native_transport_closes_only_when_the_last_clone_drops() {
    let env = TestEnv::new();
    env.eal_skip();
    let ctx = configured_context();
    let guard = DpdkTransport::install(&ctx, &canonical()).unwrap();
    let clone = guard.clone();
    let closed = |env: &TestEnv| (0..env.log_count()).any(|i| env.log_at(i).starts_with("close "));

    // rusteron 0.2.11's context cannot hold the transport, so the caller's
    // clones own it: dropping one of two must leave the native ports open.
    drop(guard);
    assert!(!closed(&env), "native transport closed while a clone was alive");

    drop(clone);
    assert!(closed(&env), "native transport not closed after the last clone dropped");
}

/// Aeron's C driver only parses `AERON_{SENDER,RECEIVER,CONDUCTOR}_CPU_AFFINITY`;
/// `aeronmd` applies them by installing `aeron_set_thread_affinity_on_start` as
/// the agent start hook, and rusteron's driver does not. Without it the agents
/// float (kernel UDP) or inherit the EAL main thread's CPU (DPDK).
#[serial]
#[test]
fn pinning_installs_aerons_affinity_hook_with_the_context_as_state() {
    use rusteron_media_driver::bindings::{
        aeron_driver_context_get_agent_on_start_function, aeron_driver_context_get_agent_on_start_state,
        aeron_set_thread_affinity_on_start,
    };
    let ctx = AeronDriverContext::new().unwrap();
    ergon_md_dpdk::pin_agents_to_configured_cpus(&ctx).unwrap();
    // SAFETY: the context is live for both getter calls.
    let (func, state) = unsafe {
        (
            aeron_driver_context_get_agent_on_start_function(ctx.get_inner()),
            aeron_driver_context_get_agent_on_start_state(ctx.get_inner()),
        )
    };
    let expected: unsafe extern "C" fn(*mut std::os::raw::c_void, *const std::os::raw::c_char) =
        aeron_set_thread_affinity_on_start;
    assert_eq!(func.map(|f| f as usize), Some(expected as usize));
    assert_eq!(state, ctx.get_inner().cast());
}
