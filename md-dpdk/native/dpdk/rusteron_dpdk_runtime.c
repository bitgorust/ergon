/*
 * ergon-md-dpdk DPDK transport — runtime orchestration (plan §7.2).
 *
 * Owns the process-lifetime EAL singleton and drives both role ports through
 * the injectable port-ops seam: probe -> dev_info (net_ena + offloads) ->
 * dev_configure -> mempool -> rx/tx queue -> set_mtu -> start -> link. Any
 * failure cleans up prior resources in reverse initialization order and the
 * error is recorded for rusteron_dpdk_last_error()/last_error_code().
 *
 * This translation unit references no libdpdk symbols directly: EAL goes
 * through the rusteron_dpdk_eal_* seam and ports through the port-ops table, so
 * it can be linked and deterministically tested without libdpdk.
 */

/* clock_gettime/CLOCK_MONOTONIC need POSIX 199309; the strict -std=c11 build
 * hides them unless this is declared before any libc header is included. */
#define _GNU_SOURCE /* pthread_setaffinity_np, cpu_set_t: the housekeeping thread */
#define _POSIX_C_SOURCE 200809L

#include "rusteron_dpdk_internal.h"

#include <arpa/inet.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sched.h>
#include <ifaddrs.h>
#include <netinet/in.h>
#include <sys/random.h>
#include <errno.h>
#include <time.h>

#define RUSTERON_DPDK_IP_UDP_OVERHEAD 28 /* IPv4 + UDP headers above the payload */
#define RUSTERON_DPDK_LINK_TIMEOUT_MS 5000

/* Process-lifetime state. EAL cannot be reinitialized (plan §7.2), so the
 * ever-initialized flag is never cleared outside the test reset hook. */
static int rusteron_dpdk_test_eal_mode = RUSTERON_DPDK_EAL_REAL;
static int rusteron_dpdk_eal_ever_initialized = 0;
static const rusteron_dpdk_port_ops_t *rusteron_dpdk_port_ops_override = NULL;

void rusteron_dpdk_test_set_eal_mode(int mode)
{
    rusteron_dpdk_test_eal_mode = mode;
}

/* Transport clock. The hot path reads the invariant TSC and scales it to
 * nanoseconds with a 32.32 fixed-point factor calibrated once against
 * CLOCK_MONOTONIC, anchored so the two clocks agree at calibration. Without an
 * invariant TSC (constant_tsc + nonstop_tsc) it falls back to CLOCK_MONOTONIC.
 * Tests pin it, which routes every read through the slow path. */
#define RUSTERON_DPDK_CLOCK_CALIBRATION_NS 50000000ULL /* 50 ms, once per process */
#ifndef CLOCK_MONOTONIC_RAW
#define CLOCK_MONOTONIC_RAW 4 /* Linux */
#endif

rusteron_dpdk_clock_t rusteron_dpdk_clock = { .mode = RUSTERON_DPDK_CLOCK_UNCALIBRATED };
static int rusteron_dpdk_clock_calibrated_source = RUSTERON_DPDK_CLOCK_MONOTONIC;
static int rusteron_dpdk_clock_allow_tsc = 1; /* the setting the calibration used */
static uint64_t rusteron_dpdk_test_clock_ns_value = 0;
/* How far each of this thread's reads moves a pinned clock on: the test's
 * own, so the local-I/O thread's reads never move it under the test. */
static _Thread_local uint64_t rusteron_dpdk_test_clock_step_ns = 0;

static uint64_t rusteron_dpdk_clock_read_ns(clockid_t clock);

static uint64_t rusteron_dpdk_monotonic_ns(void)
{
    return rusteron_dpdk_clock_read_ns(CLOCK_MONOTONIC);
}

static inline uint64_t rusteron_dpdk_tsc(void)
{
#if defined(__x86_64__)
    return __rdtsc();
#else
    return 0;
#endif
}

/* Whether the first "flags" line of /proc/cpuinfo lists both constant_tsc
 * and nonstop_tsc: the TSC then ticks at a fixed rate through frequency and
 * idle-state changes, so it can stand in for CLOCK_MONOTONIC. */
static int rusteron_dpdk_cpu_has_invariant_tsc(void)
{
#if defined(__x86_64__)
    FILE *f = fopen("/proc/cpuinfo", "r");
    if (NULL == f)
    {
        return 0;
    }
    char line[8192];
    int invariant = 0;
    while (NULL != fgets(line, sizeof(line), f))
    {
        if (0 == strncmp(line, "flags", 5))
        {
            invariant = NULL != strstr(line, " constant_tsc") && NULL != strstr(line, " nonstop_tsc");
            break;
        }
    }
    fclose(f);
    return invariant;
#else
    return 0;
#endif
}

static uint64_t rusteron_dpdk_clock_read_ns(clockid_t clock)
{
    struct timespec ts;
    clock_gettime(clock, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

/* The tightest of a few bracketed samples: a read of `clock` and the TSC
 * midpoint around it. A sample the thread was interrupted in has a wide
 * bracket, so the narrowest one pins the read to within a few ns. */
static void rusteron_dpdk_clock_sample(clockid_t clock, uint64_t *tsc, uint64_t *ns)
{
    uint64_t best = UINT64_MAX;
    for (int i = 0; i < 16; i++)
    {
        const uint64_t t0 = rusteron_dpdk_tsc();
        const uint64_t at = rusteron_dpdk_clock_read_ns(clock);
        const uint64_t t1 = rusteron_dpdk_tsc();
        if (t1 - t0 < best)
        {
            best = t1 - t0;
            *tsc = t0 + (t1 - t0) / 2;
            *ns = at;
        }
    }
}

/* Measure the TSC rate; 0 when TSC is not to be used. The rate is measured
 * against CLOCK_MONOTONIC_RAW, the unslewed hardware rate: CLOCK_MONOTONIC
 * may be mid-slew (chrony after boot) and would bake that into the factor.
 * The ns base is anchored to CLOCK_MONOTONIC. */
static uint64_t rusteron_dpdk_clock_measure_tsc(void)
{
    uint64_t tsc0, raw0, tsc1, raw1;
    rusteron_dpdk_clock_sample(CLOCK_MONOTONIC_RAW, &tsc0, &raw0); /* warms the code and the vDSO page */
    rusteron_dpdk_clock_sample(CLOCK_MONOTONIC_RAW, &tsc0, &raw0);
    do
    {
        rusteron_dpdk_clock_sample(CLOCK_MONOTONIC_RAW, &tsc1, &raw1);
    }
    while (raw1 - raw0 < RUSTERON_DPDK_CLOCK_CALIBRATION_NS);
    if (tsc1 <= tsc0)
    {
        return 0;
    }
    rusteron_dpdk_clock_sample(CLOCK_MONOTONIC, &rusteron_dpdk_clock.tsc_base, &rusteron_dpdk_clock.ns_base);
    return (uint64_t)(((unsigned __int128)(raw1 - raw0) << 32) / (tsc1 - tsc0));
}

/* Choose and calibrate the clock. A test pin stays in force. */
static void rusteron_dpdk_clock_calibrate(int allow_tsc)
{
    rusteron_dpdk_clock_allow_tsc = allow_tsc;
    uint64_t mult = allow_tsc && rusteron_dpdk_cpu_has_invariant_tsc() ? rusteron_dpdk_clock_measure_tsc() : 0;
    int source = 0 != mult ? RUSTERON_DPDK_CLOCK_TSC : RUSTERON_DPDK_CLOCK_MONOTONIC;
    rusteron_dpdk_clock.tsc_mult = mult;
    rusteron_dpdk_clock_calibrated_source = source;
    if (RUSTERON_DPDK_CLOCK_PINNED != rusteron_dpdk_clock.mode)
    {
        rusteron_dpdk_clock.mode = source;
    }
}

uint64_t rusteron_dpdk_clock_ns_slow(void)
{
    if (RUSTERON_DPDK_CLOCK_PINNED == rusteron_dpdk_clock.mode)
    {
        const uint64_t now = rusteron_dpdk_test_clock_ns_value;
        if (0 != rusteron_dpdk_test_clock_step_ns)
        {
            rusteron_dpdk_test_clock_ns_value = now + rusteron_dpdk_test_clock_step_ns;
        }
        return now;
    }
    if (RUSTERON_DPDK_CLOCK_UNCALIBRATED == rusteron_dpdk_clock.mode)
    {
        rusteron_dpdk_clock_calibrate(1);
        return rusteron_dpdk_clock_now();
    }
    return rusteron_dpdk_monotonic_ns();
}

uint64_t rusteron_dpdk_clock_ns(void)
{
    return rusteron_dpdk_clock_now();
}

void rusteron_dpdk_clock_init(void)
{
    if (RUSTERON_DPDK_CLOCK_UNCALIBRATED == rusteron_dpdk_clock.mode)
    {
        rusteron_dpdk_clock_calibrate(1);
    }
}

int rusteron_dpdk_clock_source(void)
{
    rusteron_dpdk_clock_init();
    return rusteron_dpdk_clock_calibrated_source;
}

void rusteron_dpdk_test_set_clock_ns(uint64_t ns)
{
    rusteron_dpdk_clock_init();
    rusteron_dpdk_test_clock_ns_value = ns;
    rusteron_dpdk_clock.mode = RUSTERON_DPDK_CLOCK_PINNED;
}

void rusteron_dpdk_test_set_clock_step_ns(uint64_t ns)
{
    rusteron_dpdk_test_clock_step_ns = ns;
}

int rusteron_dpdk_test_clock_recalibrate(int allow_tsc)
{
    rusteron_dpdk_clock_calibrate(allow_tsc);
    return 0;
}

/* Whether runtime init starts the housekeeping thread: production yes; the
 * test reset turns it off so tests drive housekeeping with the tick hook. */
static int rusteron_dpdk_housekeeping_enabled = 1;

void rusteron_dpdk_test_set_housekeeping_thread(int enabled)
{
    rusteron_dpdk_housekeeping_enabled = enabled;
}

static uint64_t rusteron_dpdk_test_realtime_ns_value = 0;
static int rusteron_dpdk_test_realtime_pinned = 0;

uint64_t rusteron_dpdk_realtime_ns(void)
{
    if (rusteron_dpdk_test_realtime_pinned)
    {
        return rusteron_dpdk_test_realtime_ns_value;
    }
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

void rusteron_dpdk_test_set_realtime_ns(uint64_t ns)
{
    rusteron_dpdk_test_realtime_ns_value = ns;
    rusteron_dpdk_test_realtime_pinned = 1;
}

/* Clear a test pin, keeping the calibration (it is process-lifetime). */
static void rusteron_dpdk_test_clock_unpin(void)
{
    if (RUSTERON_DPDK_CLOCK_PINNED == rusteron_dpdk_clock.mode)
    {
        rusteron_dpdk_clock.mode = rusteron_dpdk_clock_calibrated_source;
    }
    rusteron_dpdk_test_clock_ns_value = 0;
    rusteron_dpdk_test_clock_step_ns = 0;
    rusteron_dpdk_test_realtime_ns_value = 0;
    rusteron_dpdk_test_realtime_pinned = 0;
}

void rusteron_dpdk_test_reset(void)
{
    rusteron_dpdk_test_eal_mode = RUSTERON_DPDK_EAL_REAL;
    rusteron_dpdk_eal_ever_initialized = 0;
    rusteron_dpdk_port_ops_override = NULL;
    rusteron_dpdk_test_clock_unpin();
    rusteron_dpdk_housekeeping_enabled = 0;
    rusteron_dpdk_transport_test_reset();
    /* Clear the thread-local error state so last_error()/last_error_code()
     * start from "no error" in the next test (tests run single-threaded). */
    rusteron_dpdk_set_error_code("", RUSTERON_DPDK_ERR_OK);
}

void rusteron_dpdk_set_port_ops(const rusteron_dpdk_port_ops_t *ops)
{
    rusteron_dpdk_port_ops_override = ops;
}

static const rusteron_dpdk_port_ops_t *rusteron_dpdk_active_ops(void)
{
    if (NULL != rusteron_dpdk_port_ops_override)
    {
        return rusteron_dpdk_port_ops_override;
    }
    return rusteron_dpdk_port_ops_real();
}

/* Parse a dotted-quad into a network-order uint32_t; 0 on success. */
static int rusteron_dpdk_parse_ipv4(const char *text, uint32_t *out)
{
    struct in_addr addr;
    if (NULL == text || 1 != inet_pton(AF_INET, text, &addr))
    {
        return -1;
    }
    *out = addr.s_addr;
    return 0;
}

/* Bring EAL up once per process (or skip it under the test hook). The EAL
 * singleton guard fires in every mode, including the test SKIP mode, so a
 * second transport in one process always reports ALREADY_INITIALIZED. */
static int rusteron_dpdk_runtime_eal_up(rusteron_dpdk_transport_t *native)
{
    if (rusteron_dpdk_eal_ever_initialized || rusteron_dpdk_eal_is_initialized())
    {
        rusteron_dpdk_set_error_code(
            "DPDK EAL is already initialized in this process (only one EAL runtime is permitted)",
            RUSTERON_DPDK_ERR_ALREADY_INITIALIZED);
        return -1;
    }

    if (RUSTERON_DPDK_EAL_SKIP == rusteron_dpdk_test_eal_mode)
    {
        native->eal_up = 1;
        rusteron_dpdk_eal_ever_initialized = 1;
        return 0;
    }

    rusteron_dpdk_eal_params_t params;
    memset(&params, 0, sizeof(params));
    params.config = &native->config;
    params.mode = rusteron_dpdk_test_eal_mode;

    char errbuf[512] = "";
    if (rusteron_dpdk_eal_init(&params, errbuf, sizeof(errbuf)) < 0)
    {
        rusteron_dpdk_set_error(errbuf[0] != '\0' ? errbuf : "DPDK EAL initialization failed");
        return -1;
    }

    native->eal_up = 1;
    rusteron_dpdk_eal_ever_initialized = 1;
    return 0;
}

static int rusteron_dpdk_init_port(rusteron_dpdk_transport_t *native, rusteron_dpdk_port_t *p)
{
    const rusteron_dpdk_port_ops_t *ops = native->ops;
    const rusteron_dpdk_config_t *cfg = &native->config;
    char message[512];

    if (ops->probe_port(p->pci, p->has_mac ? p->want_mac : NULL, &p->port_id) < 0)
    {
        if (p->has_mac)
        {
            snprintf(message, sizeof(message),
                     "no DPDK port on %s reports MAC %s (is the MANA interface down and the MAC its own?)",
                     p->pci, p->mac_text);
        }
        else
        {
            snprintf(message, sizeof(message),
                     "device %s is not probed by DPDK (allow-listed ENA must be bound via VFIO)", p->pci);
        }
        rusteron_dpdk_set_error(message);
        return -1;
    }

    char driver[64] = "";
    uint16_t desc_lim[4] = { 0, 0, 0, 0 };
    if (ops->dev_info(p->port_id, p->pci, p->mac, driver, sizeof(driver),
                      &p->csum_offload_ok, &p->ena_llq_available, &p->rx_timestamp_ok, &p->max_mtu, desc_lim) < 0)
    {
        snprintf(message, sizeof(message),
                 "cannot read capabilities of device %s (port %u)", p->pci, p->port_id);
        rusteron_dpdk_set_error(message);
        return -1;
    }
    if (0 == memcmp(p->mac, "\x00\x00\x00\x00\x00\x00", 6))
    {
        snprintf(message, sizeof(message),
                 "device %s (port %u) reports an all-zero MAC", p->pci, p->port_id);
        rusteron_dpdk_set_error(message);
        return -1;
    }
    /* Keep the PMD name for the port-info counter label (plan §9). */
    memcpy(p->driver, driver, sizeof(p->driver));
    /* The PCI-device guarantees (a supported PMD, IPv4/UDP checksum offload)
     * are required for a PCI NIC but not for a virtual/TAP device: vdev PMDs
     * report their own driver name and may software-checksum (plan §11.2). */
    if (rusteron_dpdk_selector_is_pci(p->pci))
    {
        int ena = 0 == strcmp(driver, "net_ena");
        /* Azure: net_netvsc (the recommended path) drives the NIC's synthetic
         * channel and the MANA VF beneath it; net_mana is the VF alone. */
        int azure = 0 == strcmp(driver, "net_mana") || 0 == strcmp(driver, "net_netvsc");
        if (!ena && !azure)
        {
            snprintf(message, sizeof(message),
                     "device %s reports driver %s, expected the net_ena (AWS) or net_mana / net_netvsc (Azure) PMD",
                     p->pci, driver[0] != '\0' ? driver : "<unknown>");
            rusteron_dpdk_set_error(message);
            return -1;
        }
        if (azure && !p->has_mac)
        {
            /* Without a MAC the MANA PMD takes every vport on the function,
             * the primary NIC included. */
            snprintf(message, sizeof(message),
                     "device %s is a %s port: set the port's MAC "
                     "(RUSTERON_DPDK_SENDER_MAC / RUSTERON_DPDK_RECEIVER_MAC)", p->pci, driver);
            rusteron_dpdk_set_error(message);
            return -1;
        }
        if (azure && (cfg->rx_descriptors > RUSTERON_DPDK_MANA_MAX_DESCRIPTORS ||
                      cfg->tx_descriptors > RUSTERON_DPDK_MANA_MAX_DESCRIPTORS))
        {
            /* netvsc sets up its MANA VF's queues with the same sizes. */
            snprintf(message, sizeof(message),
                     "%s queues (a MANA VF) hold at most %d descriptors (rx %u, tx %u): set "
                     "RUSTERON_DPDK_RX_DESCRIPTORS and RUSTERON_DPDK_TX_DESCRIPTORS to %d",
                     driver, RUSTERON_DPDK_MANA_MAX_DESCRIPTORS, cfg->rx_descriptors, cfg->tx_descriptors,
                     RUSTERON_DPDK_MANA_MAX_DESCRIPTORS);
            rusteron_dpdk_set_error(message);
            return -1;
        }
        if (p->has_mac && 0 != memcmp(p->mac, p->want_mac, RUSTERON_DPDK_ETH_ADDR_LEN))
        {
            snprintf(message, sizeof(message),
                     "port %u on %s does not report the configured MAC %s", p->port_id, p->pci, p->mac_text);
            rusteron_dpdk_set_error(message);
            return -1;
        }
        if (!p->csum_offload_ok)
        {
            snprintf(message, sizeof(message),
                     "device %s lacks the required IPv4/UDP checksum offloads", p->pci);
            rusteron_dpdk_set_error(message);
            return -1;
        }
    }

    if (cfg->rx_hw_timestamp && !p->rx_timestamp_ok)
    {
        snprintf(message, sizeof(message),
                 "device %s (driver %s) does not offer hardware receive timestamps "
                 "(RX_OFFLOAD_TIMESTAMP: AWS ENA in a precision-time placement group, DPDK >= 25.11); "
                 "unset RUSTERON_DPDK_RX_HW_TIMESTAMP",
                 p->pci, driver[0] != '\0' ? driver : "<unknown>");
        rusteron_dpdk_set_error(message);
        return -1;
    }

    uint16_t l3_mtu = (uint16_t)(cfg->max_aeron_mtu + RUSTERON_DPDK_IP_UDP_OVERHEAD);
    if (0 != p->max_mtu && l3_mtu > p->max_mtu)
    {
        snprintf(message, sizeof(message),
                 "max_aeron_mtu %zu needs an L3 MTU of %u, above %s's maximum %u (driver %s): lower "
                 "RUSTERON_DPDK_MAX_AERON_MTU to at most %u",
                 cfg->max_aeron_mtu, (unsigned)l3_mtu, p->pci, (unsigned)p->max_mtu, driver,
                 (unsigned)((p->max_mtu - RUSTERON_DPDK_IP_UDP_OVERHEAD) / 32 * 32));
        rusteron_dpdk_set_error(message);
        return -1;
    }
    /* Queue sizes the NIC takes (an ENA at most 512 TX descriptors), checked
     * before anything is configured: DPDK refuses a queue outside them, and
     * the port it leaves half set up cannot be closed (see port_teardown). */
    for (int tx = 0; tx < 2; tx++)
    {
        const unsigned n = tx ? cfg->tx_descriptors : cfg->rx_descriptors;
        const unsigned lo = desc_lim[2 * tx], hi = desc_lim[2 * tx + 1];
        if (n < lo || (0 != hi && n > hi))
        {
            snprintf(message, sizeof(message),
                     "%s (RUSTERON_DPDK_%s) %u is outside %s's limits %u..%u (driver %s): set it within them",
                     tx ? "tx_descriptors" : "rx_descriptors", tx ? "TX_DESCRIPTORS" : "RX_DESCRIPTORS", n, p->pci,
                     lo, 0 != hi ? hi : 65535U, driver);
            rusteron_dpdk_set_error(message);
            return -1;
        }
    }
    /* Checksum offloads only when the NIC offers both IPv4 and UDP (a vdev may
     * not: then both sides are done in software, never a half-done UDP sum). */
    p->tx_udp_csum_offload = p->csum_offload_ok;
    uint64_t rx_off = p->csum_offload_ok ? (RUSTERON_DPDK_RX_OFFLOAD_IPV4_CKSUM | RUSTERON_DPDK_RX_OFFLOAD_UDP_CKSUM) : 0;
    if (cfg->rx_hw_timestamp)
    {
        rx_off |= RUSTERON_DPDK_RX_OFFLOAD_TIMESTAMP;
    }
    uint64_t tx_off = p->csum_offload_ok ? (RUSTERON_DPDK_TX_OFFLOAD_IPV4_CKSUM | RUSTERON_DPDK_TX_OFFLOAD_UDP_CKSUM) : 0;
    if (ops->dev_configure(p->port_id, 1, 1, rx_off, tx_off) < 0)
    {
        snprintf(message, sizeof(message), "failed to configure queues/offloads on %s", p->pci);
        rusteron_dpdk_set_error(message);
        return -1;
    }
    p->configured = 1;

    /* Keep the mempool name short: when a pool is split across memzones DPDK
     * names them MP_<pool>_<n>, and RTE_MEMZONE_NAMESIZE is 32 — the old
     * "rusteron_dpdk_receiver_pool" (27) produced a 32-char memzone name and
     * failed with ENAMETOOLONG on the split path (rte_mempool_populate_default). */
    char pool_name[64];
    snprintf(pool_name, sizeof(pool_name), "rusteron_%s_pool",
             p->role == RUSTERON_DPDK_ROLE_SENDER ? "sender" : "receiver");
    /* Room for the largest frame in one segment: Ethernet + L3 MTU, never less
     * than DPDK's default 2048. */
    uint16_t data_room = (uint16_t)(RUSTERON_DPDK_ETH_HDR_LEN + l3_mtu);
    if (data_room < 2048)
    {
        data_room = 2048;
    }
    p->mempool = ops->mempool_create(pool_name, cfg->mbufs_per_port, cfg->mempool_cache, data_room);
    if (NULL == p->mempool)
    {
        /* The port op records the underlying rte error (rte_strerror); only
         * fall back to a generic message if the op did not set one. */
        if (RUSTERON_DPDK_ERR_OK == rusteron_dpdk_last_error_code())
        {
            snprintf(message, sizeof(message), "mempool creation failed for %s", p->pci);
            rusteron_dpdk_set_error(message);
        }
        return -1;
    }

    if (ops->rx_queue_setup(p->port_id, 0, cfg->rx_descriptors, p->mempool) < 0)
    {
        snprintf(message, sizeof(message), "rx queue setup failed on %s", p->pci);
        rusteron_dpdk_set_error(message);
        return -1;
    }
    p->rx_queue_id = 0;

    if (ops->tx_queue_setup(p->port_id, 0, cfg->tx_descriptors) < 0)
    {
        snprintf(message, sizeof(message), "tx queue setup failed on %s", p->pci);
        rusteron_dpdk_set_error(message);
        return -1;
    }
    p->tx_queue_id = 0;
    p->queues_ready = 1;

    if (ops->dev_set_mtu(p->port_id, l3_mtu) < 0)
    {
        snprintf(message, sizeof(message),
                 "device %s rejected L3 MTU %u (max_aeron_mtu %zu)", p->pci, l3_mtu, cfg->max_aeron_mtu);
        rusteron_dpdk_set_error(message);
        return -1;
    }

    if (ops->dev_start(p->port_id) < 0)
    {
        snprintf(message, sizeof(message), "device %s failed to start", p->pci);
        rusteron_dpdk_set_error(message);
        return -1;
    }
    p->started = 1;

    if (ops->link_wait_ready(p->port_id, RUSTERON_DPDK_LINK_TIMEOUT_MS) < 0)
    {
        snprintf(message, sizeof(message),
                 "link on %s (port %u) did not come up within %d ms",
                 p->pci, p->port_id, RUSTERON_DPDK_LINK_TIMEOUT_MS);
        rusteron_dpdk_set_error(message);
        return -1;
    }
    p->link_up = 1;

    return 0;
}

int rusteron_dpdk_parse_mac(const char *text, uint8_t mac[RUSTERON_DPDK_ETH_ADDR_LEN])
{
    if (NULL == text || 17 != strlen(text))
    {
        return -1;
    }
    for (int i = 0; i < RUSTERON_DPDK_ETH_ADDR_LEN; i++)
    {
        const char *octet = text + i * 3;
        if (i < RUSTERON_DPDK_ETH_ADDR_LEN - 1 && ':' != octet[2])
        {
            return -1;
        }
        unsigned value = 0;
        for (int j = 0; j < 2; j++)
        {
            char c = octet[j];
            unsigned nibble;
            if (c >= '0' && c <= '9')
            {
                nibble = (unsigned)(c - '0');
            }
            else if (c >= 'a' && c <= 'f')
            {
                nibble = (unsigned)(c - 'a' + 10);
            }
            else if (c >= 'A' && c <= 'F')
            {
                nibble = (unsigned)(c - 'A' + 10);
            }
            else
            {
                return -1;
            }
            value = (value << 4) | nibble;
        }
        mac[i] = (uint8_t)value;
    }
    return 0;
}

/* Append " <arg>" (no leading space for the first) to out; -1 if it overflows. */
static int rusteron_dpdk_append_arg(char *out, size_t len, size_t *used, const char *arg)
{
    int n = snprintf(out + *used, len - *used, "%s%s", 0 == *used ? "" : " ", arg);
    if (n < 0 || (size_t)n >= len - *used)
    {
        return -1;
    }
    *used += (size_t)n;
    return 0;
}

int rusteron_dpdk_eal_allow_args(const rusteron_dpdk_config_t *cfg, char *out, size_t len)
{
    if (NULL == cfg || NULL == out || 0 == len)
    {
        rusteron_dpdk_set_error("eal_allow_args requires a config and an output buffer");
        return -1;
    }
    out[0] = '\0';
    size_t used = 0;
    const char *sel[2] = { cfg->sender_pci, cfg->receiver_pci };
    const char *mac[2] = { cfg->sender_mac, cfg->receiver_mac };
    for (int i = 0; i < 2; i++)
    {
        char arg[96];
        if (!rusteron_dpdk_selector_is_pci(sel[i]))
        {
            snprintf(arg, sizeof(arg), "--vdev=%s", sel[i]);
        }
        else if ('\0' == mac[i][0])
        {
            snprintf(arg, sizeof(arg), "%s", sel[i]);
        }
        else if (1 == i && '\0' != mac[0][0] && 0 == strcmp(sel[0], sel[1]))
        {
            continue; /* the receiver's MAC went with the sender's -a below */
        }
        else if (0 == i && '\0' != mac[1][0] && 0 == strcmp(sel[0], sel[1]))
        {
            /* One MANA function serves both roles: one -a, both vports. */
            snprintf(arg, sizeof(arg), "%s,mac=%s,mac=%s", sel[0], mac[0], mac[1]);
        }
        else
        {
            snprintf(arg, sizeof(arg), "%s,mac=%s", sel[i], mac[i]);
        }
        if ((rusteron_dpdk_selector_is_pci(sel[i]) && rusteron_dpdk_append_arg(out, len, &used, "-a") < 0) ||
            rusteron_dpdk_append_arg(out, len, &used, arg) < 0)
        {
            rusteron_dpdk_set_error("EAL device arguments do not fit the buffer");
            return -1;
        }
    }
    /* MAC-selected ports are Azure MANA. There DPDK auto-probes vdev_netvsc,
     * whose fail-safe device takes ownership of the MANA ports (so they never
     * reach the transport) and could take the primary NIC's; an explicit
     * instance with ignore=1 stops the auto-probe. */
    if (('\0' != mac[0][0] || '\0' != mac[1][0]) &&
        rusteron_dpdk_append_arg(out, len, &used, "--vdev=net_vdev_netvsc0,ignore=1") < 0)
    {
        rusteron_dpdk_set_error("EAL device arguments do not fit the buffer");
        return -1;
    }
    /* Operator EAL options (log levels, lcore lists), whitespace-separated. */
    const char *extra = getenv("RUSTERON_DPDK_EAL_ARGS");
    if (NULL != extra)
    {
        char copy[1024]; /* as long as the EAL's whole argument buffer */
        if (strlen(extra) >= sizeof(copy))
        {
            rusteron_dpdk_set_error("RUSTERON_DPDK_EAL_ARGS does not fit the EAL argument buffer");
            return -1;
        }
        snprintf(copy, sizeof(copy), "%s", extra);
        char *save = NULL;
        for (char *tok = strtok_r(copy, " \t", &save); NULL != tok; tok = strtok_r(NULL, " \t", &save))
        {
            if (rusteron_dpdk_append_arg(out, len, &used, tok) < 0)
            {
                rusteron_dpdk_set_error("RUSTERON_DPDK_EAL_ARGS does not fit the EAL argument buffer");
                return -1;
            }
        }
    }
    return 0;
}

/* "lo hi" or "lo-hi" (1 <= lo <= hi <= 65535). -1 when malformed. */
static int rusteron_dpdk_parse_port_range(const char *text, uint16_t *lo, uint16_t *hi)
{
    unsigned long a, b;
    char *end = NULL;
    a = strtoul(text, &end, 10);
    if (end == text)
    {
        return -1;
    }
    while (' ' == *end || '\t' == *end || '-' == *end)
    {
        end++;
    }
    const char *second = end;
    b = strtoul(second, &end, 10);
    while (' ' == *end || '\t' == *end || '\n' == *end)
    {
        end++;
    }
    if (end == second || '\0' != *end || a < 1 || b > 65535 || a > b)
    {
        return -1;
    }
    *lo = (uint16_t)a;
    *hi = (uint16_t)b;
    return 0;
}

static int rusteron_dpdk_read_small_file(const char *path, char *buf, size_t len)
{
    FILE *f = fopen(path, "r");
    if (NULL == f)
    {
        return -1;
    }
    size_t n = fread(buf, 1, len - 1, f);
    fclose(f);
    buf[n] = '\0';
    return 0;
}

/* "8080,9000-9010": ports the OS never hands out as ephemeral. */
static void rusteron_dpdk_parse_reserved_ports(const char *text, uint64_t *bits)
{
    const char *p = text;
    while ('\0' != *p)
    {
        char *end = NULL;
        unsigned long a = strtoul(p, &end, 10);
        if (end == p)
        {
            p++;
            continue;
        }
        unsigned long b = a;
        if ('-' == *end)
        {
            const char *q = end + 1;
            b = strtoul(q, &end, 10);
            if (end == q)
            {
                b = a;
            }
        }
        for (unsigned long port = a; port <= b && port <= 65535; port++)
        {
            bits[port / 64] |= 1ULL << (port % 64);
        }
        p = end;
    }
}

/* "10.0.0.7,10.0.0.8": more local IPv4s a NIC answers for. */
static int rusteron_dpdk_parse_extra_ips(const char *var, rusteron_dpdk_port_t *port)
{
    const char *spec = getenv(var);
    port->extra_ip_count = 0;
    if (NULL == spec || '\0' == spec[0])
    {
        return 0;
    }
    char buf[512];
    char message[640];
    if (strlen(spec) >= sizeof(buf))
    {
        snprintf(message, sizeof(message), "%s is longer than %zu bytes", var, sizeof(buf) - 1);
        rusteron_dpdk_set_error(message);
        return -1;
    }
    snprintf(buf, sizeof(buf), "%s", spec);
    char *save = NULL;
    for (char *item = strtok_r(buf, ", ", &save); NULL != item; item = strtok_r(NULL, ", ", &save))
    {
        uint32_t ip;
        if (port->extra_ip_count >= RUSTERON_DPDK_MAX_EXTRA_IPS || rusteron_dpdk_parse_ipv4(item, &ip) < 0)
        {
            snprintf(message, sizeof(message), "%s: \"%s\" is not an IPv4 address (at most %d)",
                     var, item, RUSTERON_DPDK_MAX_EXTRA_IPS);
            rusteron_dpdk_set_error(message);
            return -1;
        }
        port->extra_ips[port->extra_ip_count++] = ip;
    }
    return 0;
}

/* Network facts the transport needs, read once before any agent runs. */
static int rusteron_dpdk_runtime_net_init(rusteron_dpdk_transport_t *native)
{
    char buf[4096];
    char message[512];

    /* The ephemeral range: explicit, else the netns's own (a pod's, under
     * Kubernetes), else Linux's default. */
    const char *range = getenv("RUSTERON_DPDK_EPHEMERAL_PORT_RANGE");
    uint16_t kernel_lo = 32768, kernel_hi = 60999;
    if (0 == rusteron_dpdk_read_small_file("/proc/sys/net/ipv4/ip_local_port_range", buf, sizeof(buf)))
    {
        rusteron_dpdk_parse_port_range(buf, &kernel_lo, &kernel_hi);
    }
    if (NULL != range && '\0' != range[0])
    {
        if (rusteron_dpdk_parse_port_range(range, &native->ephemeral_low, &native->ephemeral_high) < 0)
        {
            snprintf(message, sizeof(message),
                     "RUSTERON_DPDK_EPHEMERAL_PORT_RANGE=\"%s\" is not \"<low> <high>\" with 1 <= low <= high <= 65535",
                     range);
            rusteron_dpdk_set_error(message);
            return -1;
        }
    }
    else
    {
        native->ephemeral_low = kernel_lo;
        native->ephemeral_high = kernel_hi;
    }
    memset(native->reserved_ports, 0, sizeof(native->reserved_ports));
    if (0 == rusteron_dpdk_read_small_file("/proc/sys/net/ipv4/ip_local_reserved_ports", buf, sizeof(buf)))
    {
        rusteron_dpdk_parse_reserved_ports(buf, native->reserved_ports);
    }

    /* Start each NIC's search at a random offset, as Linux does. */
    uint32_t span = (uint32_t)(native->ephemeral_high - native->ephemeral_low) + 1;
    uint32_t r[2] = { 0, 0 };
    if (sizeof(r) != getrandom(r, sizeof(r), GRND_NONBLOCK))
    {
        r[0] = (uint32_t)rusteron_dpdk_clock_ns();
        r[1] = r[0] * 2654435761u;
    }
    native->sender_ports.ephemeral_next = (uint16_t)(native->ephemeral_low + r[0] % span);
    native->receiver_ports.ephemeral_next = (uint16_t)(native->ephemeral_low + r[1] % span);

    if (rusteron_dpdk_parse_extra_ips("RUSTERON_DPDK_SENDER_EXTRA_IPV4", &native->sender) < 0 ||
        rusteron_dpdk_parse_extra_ips("RUSTERON_DPDK_RECEIVER_EXTRA_IPV4", &native->receiver) < 0)
    {
        return -1;
    }

    /* Host-local addresses: every kernel interface's IPv4 except the DPDK
     * NICs'. A kernel interface that also holds a DPDK NIC's IP shares its
     * port space with the transport: then the transport's ephemeral range
     * must not overlap the kernel's unless every port of it is reserved. */
    /* RUSTERON_DPDK_HOST_LOCAL: "all" (default) treats loopback and every
     * kernel interface's IP as host-local; "loopback" only 127/8 (e.g. a test
     * rig whose DPDK peer is a kernel interface on the same host). */
    /* How agents read host-local kernel sockets: `adaptive` (default) only
     * when the local-I/O thread flags data while a socket is idle, but every duty cycle for RUSTERON_DPDK_LOCAL_HOT_US (default
     * 1000) after it last had data, so an active local channel does not wait
     * for a thread wake-up per message; `spin` every duty cycle always (a
     * recvmmsg per socket per cycle on the agent, idle or not); `epoll` only
     * ever on the flag (a thread wake-up per local message). Measured on
     * Azure: adaptive halves the localhost round trip of epoll (p50 6.4 vs
     * 14.1 us, max 61 vs 260 us) at no cost while idle. */
    const char *local_io = getenv("RUSTERON_DPDK_LOCAL_IO");
    native->local_io_mode = RUSTERON_DPDK_LOCAL_IO_ADAPTIVE;
    if (NULL != local_io && '\0' != local_io[0])
    {
        if (0 == strcmp(local_io, "spin"))
        {
            native->local_io_mode = RUSTERON_DPDK_LOCAL_IO_SPIN;
        }
        else if (0 == strcmp(local_io, "adaptive"))
        {
            native->local_io_mode = RUSTERON_DPDK_LOCAL_IO_ADAPTIVE;
        }
        else if (0 != strcmp(local_io, "epoll"))
        {
            rusteron_dpdk_set_error("RUSTERON_DPDK_LOCAL_IO must be epoll, adaptive or spin");
            return -1;
        }
    }
    /* How often housekeeping reads the NIC's own statistics (on ENA each read
     * is an admin command to the device): every second by default, 0 never. */
    native->nic_stats_ns = 1000000000ULL;
    const char *nic_stats = getenv("RUSTERON_DPDK_NIC_STATS_MS");
    if (NULL != nic_stats && '\0' != nic_stats[0])
    {
        char *end = NULL;
        const unsigned long long ms = strtoull(nic_stats, &end, 10);
        if ('\0' != *end || '-' == nic_stats[0] || (0 != ms && (ms < 1000ULL || ms > 3600000ULL)))
        {
            rusteron_dpdk_set_error("RUSTERON_DPDK_NIC_STATS_MS must be 0 (never) or milliseconds from 1000 to 3600000");
            return -1;
        }
        native->nic_stats_ns = ms * 1000000ULL;
    }
    /* How many bursts one poll drains while they come back full: by default
     * as many as fill the receive ring, at most 8, so a backlog a stall left
     * clears in one duty cycle. */
    native->rx_max_bursts = native->config.burst_size > 0 ? native->config.rx_descriptors / native->config.burst_size : 1;
    if (native->rx_max_bursts > 8)
    {
        native->rx_max_bursts = 8;
    }
    if (native->rx_max_bursts < 1)
    {
        native->rx_max_bursts = 1;
    }
    const char *max_bursts = getenv("RUSTERON_DPDK_RX_MAX_BURSTS");
    if (NULL != max_bursts && '\0' != max_bursts[0])
    {
        char *end = NULL;
        const unsigned long long n = strtoull(max_bursts, &end, 10);
        if ('\0' != *end || '-' == max_bursts[0] || n < 1 || n > 64)
        {
            rusteron_dpdk_set_error("RUSTERON_DPDK_RX_MAX_BURSTS must be a number of bursts from 1 to 64");
            return -1;
        }
        native->rx_max_bursts = (uint32_t)n;
    }
    native->tx_retry_ns = 0;
    const char *tx_retry = getenv("RUSTERON_DPDK_TX_RETRY_US");
    if (NULL != tx_retry && '\0' != tx_retry[0])
    {
        char *end = NULL;
        const unsigned long long us = strtoull(tx_retry, &end, 10);
        if ('\0' != *end || us > 1000000ULL)
        {
            rusteron_dpdk_set_error("RUSTERON_DPDK_TX_RETRY_US must be microseconds, at most 1000000");
            return -1;
        }
        native->tx_retry_ns = us * 1000ULL;
    }
    /* The bandwidth allowance per NIC, megabits per second of frames (0 =
     * none; unset, the backstop RUSTERON_DPDK_TX_RATE_MBPS_DEFAULT), its
     * bucket (RUSTERON_DPDK_TX_BUCKET_BYTES, default 64 KiB, the backstop's
     * RUSTERON_DPDK_TX_BACKSTOP_BUCKET_BYTES) and the longest wait to repay a
     * send's overdraft (default 20 us; the backstop never waits). */
    uint64_t rate = RUSTERON_DPDK_TX_RATE_MBPS_DEFAULT, bucket = 65536, pace_spin_us = 20;
    const struct { const char *name; uint64_t *value; uint64_t max; } pacing[] = {
        { "RUSTERON_DPDK_TX_RATE_MBPS", &rate, 400000ULL },
        { "RUSTERON_DPDK_TX_BUCKET_BYTES", &bucket, 1ULL << 30 },
        { "RUSTERON_DPDK_TX_PACE_SPIN_US", &pace_spin_us, 1000000ULL },
    };
    for (size_t i = 0; i < sizeof(pacing) / sizeof(pacing[0]); i++)
    {
        const char *v = getenv(pacing[i].name);
        if (NULL != v && '\0' != v[0])
        {
            char *end = NULL;
            const unsigned long long n = strtoull(v, &end, 10);
            if ('\0' != *end || n > pacing[i].max)
            {
                char msg[128];
                snprintf(msg, sizeof(msg), "%s must be a number, at most %llu", pacing[i].name,
                         (unsigned long long)pacing[i].max);
                rusteron_dpdk_set_error(msg);
                return -1;
            }
            *pacing[i].value = n;
        }
    }
    native->tx_pace_spin_ns = pace_spin_us * 1000ULL;
    /* An allowance the operator set: the default is a backstop above every
     * link measured, not a link's capacity to share out, so only this can put
     * low-priority streams below the rest. */
    const char *rate_env = getenv("RUSTERON_DPDK_TX_RATE_MBPS");
    const int rate_set = NULL != rate_env && '\0' != rate_env[0] && 0 != rate;
    /* No rate set: the NIC's allowance is the backstop. It bounds a sustained
     * rate and never shapes a burst: its bucket holds more than the NIC's
     * ring of 1,450-byte frames (not of jumbo frames) unless
     * RUSTERON_DPDK_TX_BUCKET_BYTES says otherwise, and it never makes the
     * agent wait. A route allowance keeps the bucket and the wait. */
    const int backstop = NULL == rate_env || '\0' == rate_env[0];
    const char *bucket_env = getenv("RUSTERON_DPDK_TX_BUCKET_BYTES");
    const uint64_t nic_bucket = backstop && (NULL == bucket_env || '\0' == bucket_env[0])
        ? RUSTERON_DPDK_TX_BACKSTOP_BUCKET_BYTES
        : bucket;
    /* RUSTERON_DPDK_TX_ROUTE_MBPS="<a.b.c.d>/<len>=<Mbit/s>,...": a slower link
     * on the way to those destinations (a direct link to another site, say),
     * each with its own allowance; the longest matching prefix applies. */
    uint64_t route_rate[RUSTERON_DPDK_MAX_TX_ROUTES];
    uint32_t route_len[RUSTERON_DPDK_MAX_TX_ROUTES];
    native->tx_route_count = 0;
    const char *routes = getenv("RUSTERON_DPDK_TX_ROUTE_MBPS");
    if (NULL != routes && '\0' != routes[0])
    {
        char list[1024];
        if (strlen(routes) >= sizeof(list))
        {
            rusteron_dpdk_set_error("RUSTERON_DPDK_TX_ROUTE_MBPS is too long");
            return -1;
        }
        strcpy(list, routes);
        char *save = NULL;
        for (char *item = strtok_r(list, ",", &save); NULL != item; item = strtok_r(NULL, ",", &save))
        {
            char ip_text[32];
            unsigned int len = 0;
            unsigned long long mbps = 0;
            int consumed = 0;
            uint32_t ip = 0;
            if (native->tx_route_count == RUSTERON_DPDK_MAX_TX_ROUTES ||
                3 != sscanf(item, " %31[0-9.]/%u=%llu %n", ip_text, &len, &mbps, &consumed) ||
                '\0' != item[consumed] || len > 32 || 0 == mbps || mbps > 400000ULL ||
                rusteron_dpdk_parse_ipv4(ip_text, &ip) < 0)
            {
                rusteron_dpdk_set_error(
                    "RUSTERON_DPDK_TX_ROUTE_MBPS must be up to 16 <a.b.c.d>/<len>=<Mbit/s>, comma-separated");
                return -1;
            }
            /* insert keeping the longest prefixes first */
            uint32_t at = native->tx_route_count;
            while (at > 0 && route_len[at - 1] < len)
            {
                native->tx_route_net[at] = native->tx_route_net[at - 1];
                native->tx_route_mask[at] = native->tx_route_mask[at - 1];
                route_rate[at] = route_rate[at - 1];
                route_len[at] = route_len[at - 1];
                at--;
            }
            const uint32_t mask = 0 == len ? 0 : rusteron_dpdk_ipv4_mask((uint8_t)len); /* /0: every destination */
            native->tx_route_mask[at] = mask;
            native->tx_route_net[at] = ip & mask;
            route_rate[at] = mbps;
            route_len[at] = len;
            native->tx_route_count++;
        }
    }
    /* RUSTERON_DPDK_TX_LOW_STREAMS="<stream>|<first>-<last>,...": streams that
     * can wait (bulk), held below the rest under the allowances; they leave
     * RUSTERON_DPDK_TX_LOW_RESERVE_BYTES (default half the bucket) to the rest. */
    native->tx_low_count = 0;
    native->tx_low_reserve = (int64_t)(bucket / 2);
    const char *reserve = getenv("RUSTERON_DPDK_TX_LOW_RESERVE_BYTES");
    if (NULL != reserve && '\0' != reserve[0])
    {
        char *end = NULL;
        const unsigned long long n = strtoull(reserve, &end, 10);
        if ('\0' != *end || '-' == reserve[0] || n >= bucket)
        {
            rusteron_dpdk_set_error("RUSTERON_DPDK_TX_LOW_RESERVE_BYTES must be bytes, less than the bucket "
                                    "(RUSTERON_DPDK_TX_BUCKET_BYTES)");
            return -1;
        }
        native->tx_low_reserve = (int64_t)n;
    }
    const char *low = getenv("RUSTERON_DPDK_TX_LOW_STREAMS");
    if (NULL != low && '\0' != low[0])
    {
        char list[1024];
        if (strlen(low) >= sizeof(list))
        {
            rusteron_dpdk_set_error("RUSTERON_DPDK_TX_LOW_STREAMS is too long");
            return -1;
        }
        strcpy(list, low);
        char *save = NULL;
        for (char *item = strtok_r(list, ",", &save); NULL != item; item = strtok_r(NULL, ",", &save))
        {
            unsigned long long first = 0, last = 0;
            int consumed = 0;
            const char *digits = item + strspn(item, " ");
            int ok = native->tx_low_count < RUSTERON_DPDK_MAX_TX_LOW && *digits >= '0' && *digits <= '9';
            if (ok && 2 == sscanf(item, " %llu - %llu %n", &first, &last, &consumed) && '\0' == item[consumed])
            {
                /* a range */
            }
            else if (consumed = 0, ok && 1 == sscanf(item, " %llu %n", &first, &consumed) && '\0' == item[consumed])
            {
                last = first;
            }
            else
            {
                ok = 0;
            }
            if (!ok || first > last || last > INT32_MAX)
            {
                rusteron_dpdk_set_error("RUSTERON_DPDK_TX_LOW_STREAMS must be up to 16 stream ids or <first>-<last> "
                                        "ranges (0 to 2147483647), comma-separated");
                return -1;
            }
            native->tx_low_first[native->tx_low_count] = (int32_t)first;
            native->tx_low_last[native->tx_low_count] = (int32_t)last;
            native->tx_low_count++;
        }
        if (!rate_set && 0 == native->tx_route_count)
        {
            rusteron_dpdk_set_error("RUSTERON_DPDK_TX_LOW_STREAMS needs an allowance to hold them back: set "
                                    "RUSTERON_DPDK_TX_RATE_MBPS (its default is only a backstop) or "
                                    "RUSTERON_DPDK_TX_ROUTE_MBPS");
            return -1;
        }
    }
    const uint64_t pacer_now = rusteron_dpdk_clock_now();
    rusteron_dpdk_port_t *paced[2] = { &native->sender, &native->receiver };
    for (int p = 0; p < 2; p++)
    {
        rusteron_dpdk_pacer_init(&paced[p]->tx_pacer, rate, (int64_t)nic_bucket, backstop, pacer_now);
        for (uint32_t r = 0; r < native->tx_route_count; r++)
        {
            rusteron_dpdk_pacer_init(&paced[p]->route_pacers[r], route_rate[r], (int64_t)bucket, 0, pacer_now);
        }
    }
    /* RUSTERON_DPDK_LOCAL_UNWATCH=0 keeps hot sockets on the local-I/O
     * thread's epoll (adaptive); 1, the default, takes them off. */
    native->local_unwatch = 1;
    const char *unwatch = getenv("RUSTERON_DPDK_LOCAL_UNWATCH");
    if (NULL != unwatch && '\0' != unwatch[0])
    {
        if (0 != strcmp(unwatch, "0") && 0 != strcmp(unwatch, "1"))
        {
            rusteron_dpdk_set_error("RUSTERON_DPDK_LOCAL_UNWATCH must be 0 or 1");
            return -1;
        }
        native->local_unwatch = '1' == unwatch[0];
    }
    native->local_hot_ns = 1000000ULL;
    const char *hot = getenv("RUSTERON_DPDK_LOCAL_HOT_US");
    if (NULL != hot && '\0' != hot[0])
    {
        char *end = NULL;
        const unsigned long long us = strtoull(hot, &end, 10);
        if ('\0' != *end || us > 60000000ULL)
        {
            rusteron_dpdk_set_error("RUSTERON_DPDK_LOCAL_HOT_US must be microseconds, at most 60000000");
            return -1;
        }
        native->local_hot_ns = us * 1000ULL;
    }
    const char *host_local = getenv("RUSTERON_DPDK_HOST_LOCAL");
    int loopback_only = 0;
    if (NULL != host_local && '\0' != host_local[0])
    {
        if (0 == strcmp(host_local, "loopback"))
        {
            loopback_only = 1;
        }
        else if (0 != strcmp(host_local, "all"))
        {
            rusteron_dpdk_set_error("RUSTERON_DPDK_HOST_LOCAL must be all or loopback");
            return -1;
        }
    }
    free(native->host_ips);
    native->host_ips = NULL;
    native->host_ip_count = 0;
    int kernel_shares_ip = 0;
    struct ifaddrs *ifs = NULL;
    if (0 == getifaddrs(&ifs))
    {
        uint32_t capacity = 0;
        for (struct ifaddrs *i = ifs; NULL != i; i = i->ifa_next)
        {
            capacity++;
        }
        native->host_ips = calloc(capacity + 1, sizeof(uint32_t));
        if (NULL == native->host_ips)
        {
            freeifaddrs(ifs);
            rusteron_dpdk_set_error("out of memory reading the host's addresses");
            return -1;
        }
        for (struct ifaddrs *i = ifs; NULL != i; i = i->ifa_next)
        {
            if (NULL == i->ifa_addr || AF_INET != i->ifa_addr->sa_family)
            {
                continue;
            }
            uint32_t ip = ((struct sockaddr_in *)i->ifa_addr)->sin_addr.s_addr;
            if (rusteron_dpdk_port_owns_ip(&native->sender, ip) || rusteron_dpdk_port_owns_ip(&native->receiver, ip))
            {
                kernel_shares_ip = 1;
                continue;
            }
            if (!loopback_only && 0x7f != ((const uint8_t *)&ip)[0])
            {
                native->host_ips[native->host_ip_count++] = __builtin_bswap32(ip);
            }
        }
        freeifaddrs(ifs);
        /* Sorted, for the binary search in rusteron_dpdk_is_host_local. */
        for (uint32_t a = 1; a < native->host_ip_count; a++)
        {
            uint32_t v = native->host_ips[a];
            uint32_t b = a;
            while (b > 0 && native->host_ips[b - 1] > v)
            {
                native->host_ips[b] = native->host_ips[b - 1];
                b--;
            }
            native->host_ips[b] = v;
        }
    }
    if (kernel_shares_ip && native->ephemeral_low <= kernel_hi && kernel_lo <= native->ephemeral_high)
    {
        for (uint32_t p = native->ephemeral_low; p <= native->ephemeral_high; p++)
        {
            if (p >= kernel_lo && p <= kernel_hi && 0 == (native->reserved_ports[p / 64] & (1ULL << (p % 64))))
            {
                snprintf(message, sizeof(message),
                         "a kernel interface shares a DPDK NIC's IP and the transport's ephemeral ports %u-%u overlap "
                         "the kernel's %u-%u: set RUSTERON_DPDK_EPHEMERAL_PORT_RANGE outside it, or reserve the range "
                         "in net.ipv4.ip_local_reserved_ports",
                         native->ephemeral_low, native->ephemeral_high, kernel_lo, kernel_hi);
                rusteron_dpdk_set_error(message);
                return -1;
            }
        }
    }

    /* A 0.0.0.0 receive bind also listens on the host's own interfaces unless
     * RUSTERON_DPDK_WILDCARD_LOCAL=0 (or false): it costs nothing while idle. */
    native->wildcard_local = 1;
    const char *wildcard = getenv("RUSTERON_DPDK_WILDCARD_LOCAL");
    if (NULL != wildcard && '\0' != wildcard[0])
    {
        const int on = 0 == strcmp(wildcard, "1") || 0 == strcmp(wildcard, "true");
        if (!on && 0 != strcmp(wildcard, "0") && 0 != strcmp(wildcard, "false"))
        {
            rusteron_dpdk_set_error("RUSTERON_DPDK_WILDCARD_LOCAL must be 0, 1, false or true");
            return -1;
        }
        native->wildcard_local = on;
    }
    return 0;
}

void rusteron_dpdk_housekeeping_tick(rusteron_dpdk_transport_t *native, uint64_t now_ns)
{
    /* Only ports with open transports: counters belong to transports' lifetimes. */
    pthread_mutex_lock(&native->housekeeping_lock);
    if (native->sender.open_transports > 0)
    {
        rusteron_dpdk_counters_sample(&native->sender.counters, &native->sender, native->ops, now_ns, native->nic_stats_ns);
    }
    if (native->receiver.open_transports > 0)
    {
        rusteron_dpdk_counters_sample(&native->receiver.counters, &native->receiver, native->ops, now_ns, native->nic_stats_ns);
    }
    pthread_mutex_unlock(&native->housekeeping_lock);
}

void rusteron_dpdk_test_housekeeping_tick(rusteron_dpdk_transport_t *native)
{
    rusteron_dpdk_housekeeping_tick(native, rusteron_dpdk_clock_now());
}

/* Every 100 ms: the 1 Hz gate inside the sample keeps the rate. Pinned to the
 * housekeeping CPU, away from the agents' cores. */
static void *rusteron_dpdk_housekeeping_main(void *arg)
{
    rusteron_dpdk_transport_t *native = arg;
    const struct timespec step = { .tv_sec = 0, .tv_nsec = 100000000L };
    while (!__atomic_load_n(&native->housekeeping_stop, __ATOMIC_ACQUIRE))
    {
        rusteron_dpdk_housekeeping_tick(native, rusteron_dpdk_clock_now());
        nanosleep(&step, NULL);
    }
    return NULL;
}

/* A CPU Aeron pins one of its agents to (AERON_*_CPU_AFFINITY), else -1. */
static int rusteron_dpdk_agent_cpu(const char *name)
{
    const char *env = getenv(name);
    if (NULL == env || '\0' == env[0])
    {
        return -1;
    }
    char *end = NULL;
    long v = strtol(env, &end, 10);
    return '\0' == *end && v >= 0 && v < CPU_SETSIZE ? (int)v : -1;
}

/* Whether an idle strategy name busy-spins (Aeron's names and aliases). */
static int rusteron_dpdk_idle_spins(const char *name)
{
    return NULL != name &&
        (0 == strcmp(name, "spin") || 0 == strcmp(name, "noop") ||
         0 == strcmp(name, "aeron_idle_strategy_busy_spinning") || 0 == strcmp(name, "aeron_idle_strategy_noop"));
}

/* Clear `cpu` and its SMT siblings from `set`: a busy-spinning agent owns
 * its whole core. */
static void rusteron_dpdk_clear_core(cpu_set_t *set, int cpu)
{
    if (cpu < 0 || cpu >= CPU_SETSIZE)
    {
        return;
    }
    CPU_CLR(cpu, set);
    char path[96];
    snprintf(path, sizeof(path), "/sys/devices/system/cpu/cpu%d/topology/thread_siblings_list", cpu);
    FILE *f = fopen(path, "r");
    if (NULL == f)
    {
        return;
    }
    char list[256] = "";
    const int read_ok = NULL != fgets(list, sizeof(list), f);
    fclose(f);
    for (char *p = list; read_ok && '\0' != *p && '\n' != *p;)
    {
        char *end = NULL;
        long lo = strtol(p, &end, 10);
        long hi = lo;
        if (end == p)
        {
            break;
        }
        if ('-' == *end)
        {
            p = end + 1;
            hi = strtol(p, &end, 10);
        }
        for (long c = lo; c <= hi && c < CPU_SETSIZE; c++)
        {
            if (c >= 0)
            {
                CPU_CLR((int)c, set);
            }
        }
        p = ',' == *end ? end + 1 : end;
    }
}

/* Where the housekeeping and local-I/O threads run: with the conductor, the
 * driver's housekeeping core, never on a core a busy-spinning agent owns.
 * RUSTERON_DPDK_HOUSEKEEPING_CPU when set; else the conductor's CPU when it is
 * pinned and does not busy-spin; else the EAL's main lcore CPU unless a
 * spinning agent is there; else the CPUs the creating thread may use (taken
 * at create; the calling thread's when `native` is NULL) minus the spinning
 * agents' cores, SMT siblings included. The agents' CPUs and the conductor's
 * idle strategy come from the driver context once installed, else from
 * AERON_*_CPU_AFFINITY and AERON_CONDUCTOR_IDLE_STRATEGY. Returns 1 with
 * `set` filled, 0 to leave the thread unpinned, -1 when the env is bad. */
int rusteron_dpdk_housekeeping_affinity(const rusteron_dpdk_transport_t *native, cpu_set_t *set)
{
    CPU_ZERO(set);
    const char *env = getenv("RUSTERON_DPDK_HOUSEKEEPING_CPU");
    if (NULL != env && '\0' != env[0])
    {
        char *end = NULL;
        long v = strtol(env, &end, 10);
        if ('\0' != *end || v < 0 || v >= CPU_SETSIZE)
        {
            rusteron_dpdk_set_error("RUSTERON_DPDK_HOUSEKEEPING_CPU must be a CPU number");
            return -1;
        }
        CPU_SET((int)v, set);
        return 1;
    }
    const int installed = NULL != native && native->agent_cpus_known;
    const int conductor = installed ? native->agent_cpus[0] : rusteron_dpdk_agent_cpu("AERON_CONDUCTOR_CPU_AFFINITY");
    const int conductor_spins =
        installed ? native->conductor_spins : rusteron_dpdk_idle_spins(getenv("AERON_CONDUCTOR_IDLE_STRATEGY"));
    const int spinners[] = {
        installed ? native->agent_cpus[1] : rusteron_dpdk_agent_cpu("AERON_SENDER_CPU_AFFINITY"),
        installed ? native->agent_cpus[2] : rusteron_dpdk_agent_cpu("AERON_RECEIVER_CPU_AFFINITY"),
        conductor_spins ? conductor : -1,
    };
    const size_t spinner_count = sizeof(spinners) / sizeof(spinners[0]);

    /* The candidates, minus every spinning agent's core. */
    if (NULL != native && native->process_cpus_known)
    {
        for (int cpu = 0; cpu < CPU_SETSIZE && cpu < 1024; cpu++)
        {
            if (native->process_cpus[cpu / 64] & (UINT64_C(1) << (cpu % 64)))
            {
                CPU_SET(cpu, set);
            }
        }
    }
    else if (0 != sched_getaffinity(0, sizeof(*set), set))
    {
        CPU_ZERO(set);
    }
    cpu_set_t free_cpus;
    CPU_ZERO(&free_cpus);
    for (int cpu = 0; cpu < CPU_SETSIZE; cpu++)
    {
        CPU_SET(cpu, &free_cpus);
    }
    for (size_t i = 0; i < spinner_count; i++)
    {
        rusteron_dpdk_clear_core(set, spinners[i]);
        rusteron_dpdk_clear_core(&free_cpus, spinners[i]);
    }

    /* The conductor's CPU, then the EAL main lcore's, when no spinner owns it. */
    const int preferred[] = { conductor, rusteron_dpdk_eal_main_cpu() };
    for (size_t i = 0; i < sizeof(preferred) / sizeof(preferred[0]); i++)
    {
        if (preferred[i] >= 0 && preferred[i] < CPU_SETSIZE && CPU_ISSET(preferred[i], &free_cpus))
        {
            CPU_ZERO(set);
            CPU_SET(preferred[i], set);
            return 1;
        }
    }
    return CPU_COUNT(set) > 0 ? 1 : 0;
}

int rusteron_dpdk_housekeeping_start(rusteron_dpdk_transport_t *native)
{
    cpu_set_t set;
    const int pin = rusteron_dpdk_housekeeping_affinity(native, &set);
    if (pin < 0)
    {
        return -1;
    }
    native->housekeeping_stop = 0;
    if (0 != pthread_create(&native->housekeeping_thread, NULL, rusteron_dpdk_housekeeping_main, native))
    {
        rusteron_dpdk_set_error("cannot start the housekeeping thread");
        return -1;
    }
    native->housekeeping_running = 1;
    pthread_setname_np(native->housekeeping_thread, "dpdk-housekeep");
    if (pin > 0)
    {
        pthread_setaffinity_np(native->housekeeping_thread, sizeof(set), &set);
    }
    return 0;
}

void rusteron_dpdk_housekeeping_stop(rusteron_dpdk_transport_t *native)
{
    if (!native->housekeeping_running)
    {
        return;
    }
    __atomic_store_n(&native->housekeeping_stop, 1, __ATOMIC_RELEASE);
    pthread_join(native->housekeeping_thread, NULL);
    native->housekeeping_running = 0;
}

static void rusteron_dpdk_port_select_mac(rusteron_dpdk_port_t *p, const char *mac_text)
{
    /* Validated by the config check: empty, or a well-formed MAC. */
    p->has_mac = '\0' != mac_text[0] && 0 == rusteron_dpdk_parse_mac(mac_text, p->want_mac);
    snprintf(p->mac_text, sizeof(p->mac_text), "%s", p->has_mac ? mac_text : "");
}

int rusteron_dpdk_runtime_init(rusteron_dpdk_transport_t *native)
{
    cpu_set_t creator_cpus;
    if (0 == sched_getaffinity(0, sizeof(creator_cpus), &creator_cpus))
    {
        for (int cpu = 0; cpu < CPU_SETSIZE && cpu < 1024; cpu++)
        {
            if (CPU_ISSET(cpu, &creator_cpus))
            {
                native->process_cpus[cpu / 64] |= UINT64_C(1) << (cpu % 64);
            }
        }
        native->process_cpus_known = 1;
    }

    native->ops = rusteron_dpdk_active_ops();
    if (!native->local_io_running)
    {
        native->local_epfd = -1;
    }
    if (!native->housekeeping_lock_ready)
    {
        pthread_mutex_init(&native->housekeeping_lock, NULL);
        pthread_mutex_init(&native->port_table_lock, NULL);
        native->housekeeping_lock_ready = 1;
    }

    native->sender.role = RUSTERON_DPDK_ROLE_SENDER;
    memcpy(native->sender.pci, native->config.sender_pci, sizeof(native->sender.pci));
    rusteron_dpdk_port_select_mac(&native->sender, native->config.sender_mac);
    memcpy(native->sender.local_ipv4, native->config.sender_ipv4, sizeof(native->sender.local_ipv4));
    native->sender.prefix_len = native->config.sender_prefix_len;
    memcpy(native->sender.gateway_ipv4, native->config.sender_gateway, sizeof(native->sender.gateway_ipv4));

    native->receiver.role = RUSTERON_DPDK_ROLE_RECEIVER;
    memcpy(native->receiver.pci, native->config.receiver_pci, sizeof(native->receiver.pci));
    rusteron_dpdk_port_select_mac(&native->receiver, native->config.receiver_mac);
    memcpy(native->receiver.local_ipv4, native->config.receiver_ipv4, sizeof(native->receiver.local_ipv4));
    native->receiver.prefix_len = native->config.receiver_prefix_len;
    memcpy(native->receiver.gateway_ipv4, native->config.receiver_gateway, sizeof(native->receiver.gateway_ipv4));

    if (rusteron_dpdk_parse_ipv4(native->config.sender_ipv4, &native->sender.local_ip) < 0 ||
        rusteron_dpdk_parse_ipv4(native->config.sender_gateway, &native->sender.gateway_ip) < 0)
    {
        rusteron_dpdk_set_error("sender IPv4 address or gateway is not a valid dotted quad");
        return -1;
    }
    if (rusteron_dpdk_parse_ipv4(native->config.receiver_ipv4, &native->receiver.local_ip) < 0 ||
        rusteron_dpdk_parse_ipv4(native->config.receiver_gateway, &native->receiver.gateway_ip) < 0)
    {
        rusteron_dpdk_set_error("receiver IPv4 address or gateway is not a valid dotted quad");
        return -1;
    }

    if (rusteron_dpdk_runtime_net_init(native) < 0)
    {
        return -1;
    }

    /* RUSTERON_DPDK_CLOCK: tsc (default) or monotonic, which forces the
     * CLOCK_MONOTONIC fallback (for comparing the two). */
    const char *clock = getenv("RUSTERON_DPDK_CLOCK");
    int allow_tsc = 1;
    if (NULL != clock && '\0' != clock[0])
    {
        if (0 == strcmp(clock, "monotonic"))
        {
            allow_tsc = 0;
        }
        else if (0 != strcmp(clock, "tsc"))
        {
            rusteron_dpdk_set_error("RUSTERON_DPDK_CLOCK must be tsc or monotonic");
            return -1;
        }
    }
    if (RUSTERON_DPDK_CLOCK_UNCALIBRATED == rusteron_dpdk_clock.mode || allow_tsc != rusteron_dpdk_clock_allow_tsc)
    {
        rusteron_dpdk_clock_calibrate(allow_tsc);
    }

    if (rusteron_dpdk_runtime_eal_up(native) < 0)
    {
        return -1;
    }

    if (rusteron_dpdk_init_port(native, &native->sender) < 0)
    {
        rusteron_dpdk_runtime_cleanup(native);
        return -1;
    }
    if (rusteron_dpdk_init_port(native, &native->receiver) < 0)
    {
        rusteron_dpdk_runtime_cleanup(native);
        return -1;
    }

    if (rusteron_dpdk_housekeeping_enabled && rusteron_dpdk_housekeeping_start(native) < 0)
    {
        rusteron_dpdk_runtime_cleanup(native);
        return -1;
    }

    cpu_set_t local_io_cpus;
    const int local_io_pin = rusteron_dpdk_housekeeping_affinity(native, &local_io_cpus);
    if (local_io_pin < 0 ||
        rusteron_dpdk_local_io_start(native, local_io_pin > 0 ? &local_io_cpus : NULL) < 0)
    {
        rusteron_dpdk_runtime_cleanup(native);
        return -1;
    }

    return 0;
}

/* Tear one role port down in reverse of its init order: stop, close, free pool.
 * A port configured but without both queues set up is left open: DPDK's ENA
 * close releases every configured queue without checking it was set up
 * (ena_tx_queue_release, DPDK 24.11) and would crash the process; the process
 * is failing to start anyway. */
static void rusteron_dpdk_port_teardown(rusteron_dpdk_transport_t *native, rusteron_dpdk_port_t *p)
{
    const rusteron_dpdk_port_ops_t *ops = native->ops;
    if (p->started)
    {
        ops->dev_stop(p->port_id);
    }
    if (p->configured && p->queues_ready)
    {
        ops->dev_close(p->port_id);
    }
    p->started = 0;
    p->configured = 0;
    p->queues_ready = 0;
    /* frames still queued are the mempool's: back before it goes */
    rusteron_dpdk_port_tx_drop_queued(native, p);
    if (NULL != p->mempool)
    {
        ops->mempool_free(p->mempool);
        p->mempool = NULL;
    }
}

/* Reverse of initialization order: the receiver is initialized second, so it is
 * torn down first, then the sender. */
void rusteron_dpdk_runtime_cleanup(rusteron_dpdk_transport_t *native)
{
    /* The housekeeping thread reads the counters and the ports: stop it first. */
    rusteron_dpdk_housekeeping_stop(native);
    rusteron_dpdk_local_io_stop(native);
    free(native->host_ips);
    native->host_ips = NULL;
    native->host_ip_count = 0;

    /* The counters went back to the driver's counters manager when each port's
     * last transport closed (close_func, on the conductor, while the manager
     * was alive). By now the driver may be gone with its manager and CnC, so
     * only local state is released here, never the manager touched. */
    rusteron_dpdk_counters_forget(&native->receiver.counters);
    rusteron_dpdk_counters_forget(&native->sender.counters);

    rusteron_dpdk_port_teardown(native, &native->receiver);
    rusteron_dpdk_port_teardown(native, &native->sender);
    native->eal_up = 0;
}

int rusteron_dpdk_runtime_probe_device(
    rusteron_dpdk_transport_t *native, const char *pci_bdf, uint16_t *port_id)
{
    const rusteron_dpdk_config_t *cfg = &native->config;
    char message[512];

    if (NULL == pci_bdf || NULL == port_id)
    {
        rusteron_dpdk_set_error("probe_device requires non-NULL bdf and port_id");
        return -1;
    }

    int configured = (0 == strcmp(pci_bdf, cfg->sender_pci)) ||
                     (0 == strcmp(pci_bdf, cfg->receiver_pci));
    if (!configured)
    {
        snprintf(message, sizeof(message),
                 "device %s is not a configured ENA (only %s and %s are allow-listed)",
                 pci_bdf, cfg->sender_pci, cfg->receiver_pci);
        rusteron_dpdk_set_error(message);
        return -1;
    }

    if (native->ops->probe_port(pci_bdf, NULL, port_id) < 0)
    {
        snprintf(message, sizeof(message),
                 "device %s is allow-listed but not probed by DPDK", pci_bdf);
        rusteron_dpdk_set_error(message);
        return -1;
    }

    return 0;
}

void rusteron_dpdk_transport_test_dump(
    const rusteron_dpdk_transport_t *native,
    uint16_t *sender_port, uintptr_t *sender_pool,
    uint16_t *receiver_port, uintptr_t *receiver_pool)
{
    if (NULL != sender_port)
    {
        *sender_port = native->sender.port_id;
    }
    if (NULL != sender_pool)
    {
        *sender_pool = (uintptr_t)native->sender.mempool;
    }
    if (NULL != receiver_port)
    {
        *receiver_port = native->receiver.port_id;
    }
    if (NULL != receiver_pool)
    {
        *receiver_pool = (uintptr_t)native->receiver.mempool;
    }
}

/* Test-only ARP hooks (see internal.h). */
int rusteron_dpdk_transport_test_arp_seed(
    rusteron_dpdk_transport_t *native, const char *ip_str, const uint8_t mac[6])
{
    if (NULL == native || NULL == ip_str || NULL == mac)
    {
        rusteron_dpdk_set_error("arp_seed requires non-NULL transport, ip and mac");
        return -1;
    }

    uint32_t ip;
    if (rusteron_dpdk_parse_ipv4(ip_str, &ip) < 0)
    {
        rusteron_dpdk_set_error("arp_seed ip must be a valid dotted quad");
        return -1;
    }

    /* Each NIC keeps its own table; a test peer is reachable from both. */
    rusteron_dpdk_port_t *ports[2] = { &native->sender, &native->receiver };
    for (int p = 0; p < 2; p++)
    {
        if (rusteron_dpdk_arp_seed(&ports[p]->arp, ip, mac, rusteron_dpdk_clock_ns()) < 0)
        {
            rusteron_dpdk_set_error("arp_seed: ARP table is full");
            return -1;
        }
    }
    return 0;
}

int rusteron_dpdk_transport_test_arp_rx(
    rusteron_dpdk_transport_t *native, int role, const uint8_t *frame, size_t frame_len)
{
    if (NULL == native)
    {
        rusteron_dpdk_set_error("arp_rx requires a non-NULL transport");
        return -1;
    }
    rusteron_dpdk_port_t *port =
        (RUSTERON_DPDK_ROLE_RECEIVER == role) ? &native->receiver : &native->sender;
    return rusteron_dpdk_arp_handle_frame(&port->arp, native, port, frame, frame_len);
}

void rusteron_dpdk_transport_test_rx_stats(
    const rusteron_dpdk_transport_t *native, rusteron_dpdk_rx_stats_t *out)
{
    if (NULL == native || NULL == out)
    {
        return;
    }
    /* Each NIC counts its own; the test view is their sum. */
    const uint64_t *a = (const uint64_t *)&native->sender.rx_stats;
    const uint64_t *b = (const uint64_t *)&native->receiver.rx_stats;
    uint64_t *o = (uint64_t *)out;
    for (size_t i = 0; i < sizeof(*out) / sizeof(uint64_t); i++)
    {
        o[i] = a[i] + b[i];
    }
}

/* The driver context is known (install): take its agents' CPUs, however
 * they were set, and move the housekeeping and local-I/O threads off them. */
void rusteron_dpdk_runtime_agents_known(
    rusteron_dpdk_transport_t *native, int conductor, int sender, int receiver, const char *conductor_idle)
{
    /* The context may still name its `backoff` default when the environment
     * chose the strategy (ffi.rs effective_idle_strategy): either counts. */
    native->conductor_spins =
        rusteron_dpdk_idle_spins(conductor_idle) || rusteron_dpdk_idle_spins(getenv("AERON_CONDUCTOR_IDLE_STRATEGY"));
    native->agent_cpus[0] = conductor;
    native->agent_cpus[1] = sender;
    native->agent_cpus[2] = receiver;
    native->agent_cpus_known = 1;
    cpu_set_t set;
    if (rusteron_dpdk_housekeeping_affinity(native, &set) <= 0)
    {
        return;
    }
    if (native->housekeeping_running)
    {
        pthread_setaffinity_np(native->housekeeping_thread, sizeof(set), &set);
    }
    if (native->local_io_running)
    {
        pthread_setaffinity_np(native->local_io_thread, sizeof(set), &set);
    }
}
