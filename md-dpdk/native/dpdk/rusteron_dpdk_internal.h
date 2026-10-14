/*
 * ergon-md-dpdk DPDK transport — internal shared definitions.
 *
 * Includes the opaque transport handle's full layout, the role-port state, and
 * the seams the runtime goes through for EAL initialization and port
 * operations. This header is internal to the native DPDK sources and the test
 * fakes; it is not part of the Rust/native ABI.
 */
#ifndef RUSTERON_DPDK_INTERNAL_H
#define RUSTERON_DPDK_INTERNAL_H

/* clock_gettime/CLOCK_MONOTONIC (runtime.c), inet_pton (transport.c) and
 * htonl/htons (packet.c) are POSIX-199309/200112 symbols glibc hides under
 * strict -std=c11 without a feature macro. Every native DPDK source includes
 * this header first, so declaring it here (before any system include) covers
 * them uniformly. The vendored Aeron util files compiled alongside declare
 * their own macro and must not receive a -D from the build script. */
#define _POSIX_C_SOURCE 200809L

#if defined(__x86_64__)
#include <x86intrin.h>
#endif
#include <netinet/in.h>
#include <pthread.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "rusteron_dpdk_transport.h"
#include "media/aeron_udp_channel_transport.h" /* aeron_udp_channel_transport_params_t */
#include "rusteron_dpdk_port_ops.h"
#include "rusteron_dpdk_counters.h"
#include "rusteron_dpdk_arp.h"
#include "rusteron_dpdk_endpoint_map.h"

/* Aeron types for the poller callbacks and the shared receive loop (poller.c):
 * the affinity enum, the transport struct, the recv/recvmmsg function typedefs
 * and the (forward-declared) poller struct. transport.c already pulls these
 * transitively via media/aeron_udp_channel_transport.h; declaring them here
 * keeps poller.c self-contained. */
#include "media/aeron_udp_channel_transport_bindings.h"

#ifdef __cplusplus
extern "C" {
#endif

#define RUSTERON_DPDK_ROLE_SENDER 0
#define RUSTERON_DPDK_ROLE_RECEIVER 1

/* EAL modes selected by rusteron_dpdk_test_set_eal_mode (production = REAL). */
#define RUSTERON_DPDK_EAL_REAL 0    /* --huge-dir <configured path> */
#define RUSTERON_DPDK_EAL_NO_HUGE 1 /* --no-huge (tests without hugetlbfs) */
#define RUSTERON_DPDK_EAL_SKIP 2    /* skip the EAL seam entirely (tests) */

/* A canonical PCI BDF contains ':'; anything else is a virtual-device name
 * (test/TAP path, plan §11.2). Shared by the EAL argv builder and the port
 * bring-up checks so the two never disagree on the mode of a selector. */
static inline int rusteron_dpdk_selector_is_pci(const char *sel)
{
    return NULL != sel && NULL != strchr(sel, ':');
}

/* Transport-level offload bits passed to the port ops; the real port
 * implementation maps them onto the RTE_ETH_RX/TX_OFFLOAD_* masks. */
#define RUSTERON_DPDK_RX_OFFLOAD_IPV4_CKSUM (1ULL << 0)
#define RUSTERON_DPDK_RX_OFFLOAD_UDP_CKSUM (1ULL << 1)
#define RUSTERON_DPDK_RX_OFFLOAD_TIMESTAMP (1ULL << 2)
#define RUSTERON_DPDK_TX_OFFLOAD_IPV4_CKSUM (1ULL << 0)
#define RUSTERON_DPDK_TX_OFFLOAD_UDP_CKSUM (1ULL << 1)

/* Error codes are defined in rusteron_dpdk_transport.h (public ABI). */

/* Receive counters (plan §7.6): one bucket per reject class, incremented in the
 * poll hot path, and surfaced as Aeron counters (rusteron_dpdk_counters.c). */
typedef struct rusteron_dpdk_rx_stats_stct
{
    uint64_t accepted;
    uint64_t arp;
    uint64_t ipv6;
    uint64_t multicast;
    uint64_t ethertype;
    uint64_t vlan;
    uint64_t ip_options;
    uint64_t fragment;
    uint64_t truncated;
    uint64_t protocol;
    uint64_t checksum;
    uint64_t multi_segment;
    uint64_t foreign_dst;
    uint64_t unknown_port;
    uint64_t not_connected_peer;  /* connected transport: datagram from another peer */
    uint64_t icmp;                /* ICMP handled (echo answered, errors recorded) */
} rusteron_dpdk_rx_stats_t;

#define RUSTERON_DPDK_MAX_EXTRA_IPS 8
#define RUSTERON_DPDK_MAX_KSOCKS 1024
#define RUSTERON_DPDK_MAX_TX_ROUTES 16
#define RUSTERON_DPDK_MAX_TX_LOW 16 /* RUSTERON_DPDK_TX_LOW_STREAMS ranges */

/* A transmit rate allowance: a token bucket in bytes, owned by one NIC's
 * agent alone (plain arithmetic, no atomics); it may go negative (debt). */
typedef struct rusteron_dpdk_pacer_stct
{
    uint64_t rate_mbps;   /* megabits per second of frames; 0 = no allowance */
    int64_t bucket_bytes; /* the most it holds: the burst allowed at once */
    int64_t tokens;
    uint64_t last_ns;     /* last refill */
} rusteron_dpdk_pacer_t;

/* A kernel socket's readiness, raised by the local-I/O thread and cleared by
 * the agent that drains it; one per cache line, so flags of sockets drained by
 * different agents never share one. Adaptive local I/O also shares, on that
 * line, the agent's hot deadline and whether the socket is off the epoll. */
typedef struct rusteron_dpdk_ksock_stct
{
    volatile uint8_t ready;
    uint8_t used;
    volatile uint8_t unwatched;     /* adaptive: off the epoll; its agent reads it every cycle */
    uint8_t watched;                /* registered with the epoll (port_table_lock) */
    int fd;                         /* the socket, -1 when none (port_table_lock) */
    volatile uint64_t hot_until_ns; /* adaptive: its agent reads it every cycle until then */
} __attribute__((aligned(64))) rusteron_dpdk_ksock_t;

typedef struct rusteron_dpdk_port_stct
{
    uint8_t role;                 /* RUSTERON_DPDK_ROLE_* */
    char pci[16];                 /* dddd:bb:ss.f */
    int has_mac;                  /* selected by MAC on its PCI function (MANA) */
    uint8_t want_mac[RUSTERON_DPDK_ETH_ADDR_LEN]; /* that MAC, byte order */
    char mac_text[18];            /* that MAC as configured */
    char local_ipv4[16];          /* dotted quad */
    uint8_t prefix_len;           /* 1..=32 */
    char gateway_ipv4[16];        /* dotted quad */
    uint16_t port_id;             /* DPDK port id once probed */
    void *mempool;                /* opaque rte_mempool* (or fake handle) */
    uint16_t rx_queue_id;
    uint16_t tx_queue_id;
    int configured;               /* dev_configure succeeded */
    int started;                  /* dev_start succeeded */
    int link_up;                  /* link_wait_ready succeeded */
    int csum_offload_ok;          /* dev_info: IPv4/UDP checksum offloads */
    int ena_llq_available;        /* dev_info: ENA LLQ/write-combining */
    int rx_timestamp_ok;          /* dev_info: NIC RX hardware timestamps */
    uint16_t max_mtu;             /* dev_info: the NIC's largest L3 MTU (0: unknown) */
    uint8_t mac[RUSTERON_DPDK_ETH_ADDR_LEN]; /* dev_info MAC, byte order */
    uint32_t local_ip;            /* parsed local IPv4, network order */
    uint32_t gateway_ip;          /* parsed gateway IPv4, network order */
    char driver[64];              /* dev_info PMD name (labels, port-info) */
    uint64_t last_poll_ns;        /* clock at the end of the last receive poll, 0 before the first */
    int tx_udp_csum_offload;      /* NIC completes the UDP checksum; else software */
    int open_transports;          /* DPDK transports open on this NIC (housekeeping_lock) */
    uint64_t icmp_window_ns;      /* ICMP error rate limit: current one-second window */
    uint32_t icmp_sent_in_window;
    uint64_t icmp_echo_window_ns; /* ICMP echo rate limit: current one-second window */
    uint32_t icmp_echo_sent;
    uint64_t last_report_ns;      /* rate limit for formatted error reports */
    uint64_t arp_reply_window_ns; /* ARP reply rate limit: current one-second window */
    /* The bandwidth allowances: this NIC's (RUSTERON_DPDK_TX_RATE_MBPS, the
     * VM's allowance) and one per destination prefix
     * (RUSTERON_DPDK_TX_ROUTE_MBPS, a slower link), all owned by its agent. */
    rusteron_dpdk_pacer_t tx_pacer;
    rusteron_dpdk_pacer_t route_pacers[RUSTERON_DPDK_MAX_TX_ROUTES];
    uint32_t arp_replies_in_window;
    uint32_t extra_ips[RUSTERON_DPDK_MAX_EXTRA_IPS]; /* more local IPv4s (secondary IPs, VIPs) */
    uint32_t extra_ip_count;
    rusteron_dpdk_counters_t counters; /* Aeron counters (plan §9) */
    /* Owned by this NIC's agent thread alone (the sender agent for the sender
     * NIC, the receiver agent for the receiver NIC), so no lock is needed:
     * both its sends and its polls run there. */
    rusteron_dpdk_rx_stats_t rx_stats;
    rusteron_dpdk_arp_table_t arp;
} rusteron_dpdk_port_t;

/* The UDP ports bound on one NIC, exclusively as a kernel socket would. Cold:
 * touched only at transport init/close, under the runtime's port_table_lock. */
typedef struct rusteron_dpdk_port_table_stct
{
    /* [0] the wildcard (0.0.0.0) binds, [1] the NIC's address, [2 + i] its
     * extra address i: as the kernel, a wildcard bind conflicts with any bind
     * of that port, a specific one only with the wildcard and itself. */
    uint64_t bound[2 + RUSTERON_DPDK_MAX_EXTRA_IPS][65536 / 64];
    uint16_t ephemeral_next;      /* where the next ephemeral search starts */
} rusteron_dpdk_port_table_t;

/* Per-channel state attached to an Aeron transport via bindings_clientd
 * (transport.c). One instance per initialized Aeron transport/endpoint. */
#define RUSTERON_DPDK_REPLY_IPS 64

/* The local address a peer last reached a wildcard transport on. */
typedef struct rusteron_dpdk_reply_ip_stct
{
    uint32_t peer_ip;
    uint32_t local_ip;
} rusteron_dpdk_reply_ip_t;

_Static_assert(64 == RUSTERON_DPDK_REPLY_IPS, "reply_slot takes 6 bits");

/* Fibonacci hashing: the top bits of the product depend on every bit of the
 * address, so neighbouring peers spread. */
static inline uint32_t rusteron_dpdk_reply_slot(uint32_t peer_ip)
{
    return (peer_ip * 0x9e3779b1u) >> (32 - 6);
}

#define RUSTERON_DPDK_LOCAL_IO_EPOLL 0
#define RUSTERON_DPDK_LOCAL_IO_SPIN 1
#define RUSTERON_DPDK_LOCAL_IO_ADAPTIVE 2

#define RUSTERON_DPDK_LAZY_NONE 0
#define RUSTERON_DPDK_LAZY_REQUESTED 1
#define RUSTERON_DPDK_LAZY_READY 2
#define RUSTERON_DPDK_LAZY_FAILED 3

typedef struct rusteron_dpdk_client_stct
{
    /* Hot: read on every send and dispatched datagram, in the first 64 bytes. */
    rusteron_dpdk_transport_t *runtime; /* owning native runtime (singleton) */
    rusteron_dpdk_port_t *port;         /* sender or receiver ENA by affinity */
    size_t mtu;                         /* per-channel Aeron MTU cap (bytes) */
    uint16_t local_udp_port;            /* local bind port, host order */
    uint16_t connected_port;            /* host order */
    uint32_t bound_ip;                  /* its bind address (network order), 0 for the wildcard */
    int connected;                      /* connect()ed: receive only from the peer */
    uint32_t connected_ip;              /* its address, network order */
    /* The kernel path (rusteron_dpdk_kernel.c): `delegated` sends and
     * receives everything through kernel_fd (a host-local channel); otherwise
     * kernel_fd, when open, carries only host-local destinations of a DPDK
     * transport, from the same port. -1 when unused. */
    int delegated;
    int kernel_fd;
    int ksock;                          /* its readiness slot, -1 when none */
    uint64_t local_hot_until_ns;        /* adaptive: read kernel_fd every cycle until then */

    struct rusteron_dpdk_poller_state_stct *poller; /* the poller it is added to */
    int kernel_polled;                  /* kernel_fd is on its poller's list */
    int affinity;                       /* AERON_UDP_CHANNEL_TRANSPORT_AFFINITY_* */
    int bound_index;                    /* its row in the port table (0 wildcard, 1 NIC, 2+ extra) */
    uint32_t route_dst_ip;              /* the last destination looked up in the route allowances */
    int route_index;                    /* its route (-1 none, -2 not looked up yet) */
    struct sockaddr_storage connected_address; /* owned copy for NULL-address sends */
    aeron_udp_channel_transport_params_t params; /* socket buffers for a kernel socket */
    /* A DPDK transport's host-local send socket, opened by the local-I/O
     * thread on request (kernel.c), never by the agent: the agent writes
     * lazy_dst_ip, then publishes REQUESTED in lazy_state; the local-I/O
     * thread publishes READY with lazy_fd/lazy_ksock (or FAILED, retried each
     * second), and the agent adopts them as kernel_fd/ksock. */
    /* Its own cache line: the local-I/O thread writes it. */
    volatile int lazy_state __attribute__((aligned(64))); /* RUSTERON_DPDK_LAZY_* */
    uint32_t lazy_dst_ip;               /* the destination to bind on, network order */
    int lazy_fd;
    int lazy_ksock;
    int lazy_reported;                  /* the current FAILED episode was reported */
    uint64_t lazy_retry_ns;             /* local-I/O thread only */
    int lazy_linked;                    /* on the runtime's lazy_clients list */
    struct rusteron_dpdk_client_stct *lazy_next; /* port_table_lock */
    struct rusteron_dpdk_client_stct *lazy_prev;
    /* A wildcard transport answers each peer from the address it reached us
     * on, as the kernel picks the source for a reply: NICs with extra IPs only. */
    rusteron_dpdk_reply_ip_t reply[RUSTERON_DPDK_REPLY_IPS];
} __attribute__((aligned(64))) rusteron_dpdk_client_t;

_Static_assert(offsetof(rusteron_dpdk_client_t, local_hot_until_ns) + sizeof(uint64_t) <= 64,
               "the hot client fields share one cache line");
_Static_assert(offsetof(rusteron_dpdk_client_t, lazy_state) >= 64, "lazy_state, written off the agent, is not on it");

struct rusteron_dpdk_transport_stct
{
    rusteron_dpdk_config_t config;
    const rusteron_dpdk_port_ops_t *ops; /* active port ops (real or fake) */
    int eal_up;                          /* this transport owns EAL up */
    rusteron_dpdk_port_t sender;
    rusteron_dpdk_port_t receiver;

    /* Host-local IPv4s (127/8 implied): every address on a kernel interface
     * except the DPDK NICs'. Channels to or bound on these go through the
     * kernel (rusteron_dpdk_kernel.c), never DPDK. Read once at init. */
    uint32_t *host_ips;              /* sorted (host order values), owned */
    uint32_t host_ip_count;
    int wildcard_local;              /* a 0.0.0.0 receive bind also listens on the kernel */

    /* Housekeeping (runtime.c): the 1 Hz PMD statistics sample, on its own
     * thread so the agents' hot paths never do it. The lock serialises the
     * sample with counter registration and release, never the hot path. */
    pthread_mutex_t housekeeping_lock;
    int housekeeping_lock_ready;
    /* The driver's agents' CPUs (conductor, sender, receiver; -1 unpinned),
     * from its context at install: housekeeping keeps off them. */
    int agent_cpus[3];
    int agent_cpus_known;
    int conductor_spins;                 /* its idle strategy busy-spins */
    int local_io_mode;                   /* RUSTERON_DPDK_LOCAL_IO_* (RUSTERON_DPDK_LOCAL_IO) */
    uint64_t local_hot_ns;               /* adaptive: read every cycle this long after data */
    int local_unwatch;                   /* adaptive: hot sockets come off the epoll (RUSTERON_DPDK_LOCAL_UNWATCH) */
    uint64_t tx_retry_ns;                /* RUSTERON_DPDK_TX_RETRY_US: re-offer a refused tail this long */
    uint64_t tx_pace_spin_ns;            /* RUSTERON_DPDK_TX_PACE_SPIN_US: longest wait to repay pacing debt */
    /* RUSTERON_DPDK_TX_ROUTE_MBPS: destination prefixes with their own
     * allowance, longest first (network order); the pacers are per NIC. */
    uint32_t tx_route_net[RUSTERON_DPDK_MAX_TX_ROUTES];
    uint32_t tx_route_mask[RUSTERON_DPDK_MAX_TX_ROUTES];
    uint32_t tx_route_count;
    /* RUSTERON_DPDK_TX_LOW_STREAMS: stream ids (inclusive ranges) held below
     * the rest under the allowances, and the bucket bytes they leave them
     * (RUSTERON_DPDK_TX_LOW_RESERVE_BYTES). */
    int32_t tx_low_first[RUSTERON_DPDK_MAX_TX_LOW];
    int32_t tx_low_last[RUSTERON_DPDK_MAX_TX_LOW];
    uint32_t tx_low_count;
    int64_t tx_low_reserve;
    /* The CPUs the creating thread may run on, taken once at create (before
     * the EAL or any pinning): the pool housekeeping picks from. */
    uint64_t process_cpus[16];
    int process_cpus_known;
    pthread_t housekeeping_thread;
    int housekeeping_running;
    volatile int housekeeping_stop;

    /* Cold: the per-NIC UDP port tables and the ephemeral range, resolved at
     * runtime init (RUSTERON_DPDK_EPHEMERAL_PORT_RANGE, else the netns's
     * ip_local_port_range, minus ip_local_reserved_ports). */
    pthread_mutex_t port_table_lock;
    uint16_t ephemeral_low;
    uint16_t ephemeral_high;
    uint64_t reserved_ports[65536 / 64];
    rusteron_dpdk_port_table_t sender_ports;
    rusteron_dpdk_port_table_t receiver_ports;

    /* The local-I/O thread (kernel.c) and the kernel-socket readiness slots
     * it raises; the slots are never freed (port_table_lock guards `used`). */
    int local_epfd;
    pthread_t local_io_thread;
    int local_io_running;
    volatile int local_io_stop;
    uint32_t local_unwatched;            /* sockets off the epoll (port_table_lock; read by the local-I/O thread) */
    /* adaptive: bumped by an agent when one of its sockets turns hot, so the
     * local-I/O thread takes it off the epoll (alone on its line) */
    struct
    {
        volatile uint32_t value;
    } __attribute__((aligned(64))) local_hot_epoch;
    uint8_t local_hot_epoch_pad[60];
    rusteron_dpdk_client_t *lazy_clients; /* DPDK transports (port_table_lock) */
    rusteron_dpdk_ksock_t ksocks[RUSTERON_DPDK_MAX_KSOCKS];
};

/* Credit an allowance's bucket for the time since its last refill. */
static inline void rusteron_dpdk_pacer_refill(rusteron_dpdk_pacer_t *p, uint64_t now_ns)
{
    const uint64_t elapsed = now_ns - p->last_ns;
    p->last_ns = now_ns;
    /* bytes = ns * Mbit/s / 8000; elapsed is capped so the product cannot overflow */
    const uint64_t capped = elapsed > 1000000000ULL ? 1000000000ULL : elapsed;
    const int64_t tokens = p->tokens + (int64_t)(capped * p->rate_mbps / 8000ULL);
    p->tokens = tokens > p->bucket_bytes ? p->bucket_bytes : tokens;
}

/* Nanoseconds until an allowance's debt is repaid (0: none, or no allowance). */
static inline uint64_t rusteron_dpdk_pacer_wait(const rusteron_dpdk_pacer_t *p)
{
    return 0 == p->rate_mbps || p->tokens >= 0 ? 0 : (uint64_t)(-p->tokens) * 8000ULL / p->rate_mbps;
}

/* Whether a low-priority send of `wire` bytes may draw on an allowance: it
 * leaves `reserve` in the bucket, or the bucket is full (so a batch larger
 * than the bucket less the reserve is held back, never starved). It never
 * overdraws, so the others never wait for its debt. No allowance: always. */
static inline int rusteron_dpdk_pacer_spare(const rusteron_dpdk_pacer_t *p, int64_t wire, int64_t reserve)
{
    return 0 == p->rate_mbps || p->tokens - wire >= reserve || p->tokens >= p->bucket_bytes;
}

static inline void rusteron_dpdk_pacer_init(rusteron_dpdk_pacer_t *p, uint64_t rate_mbps, int64_t bucket, uint64_t now_ns)
{
    p->rate_mbps = rate_mbps;
    p->bucket_bytes = bucket;
    p->tokens = bucket;
    p->last_ns = now_ns;
}

/* Whether `ip` (network order) is host-local: loopback, or a kernel
 * interface's address. Hot-path safe: a compare and a short scan. */
static inline int rusteron_dpdk_is_host_local(const rusteron_dpdk_transport_t *native, uint32_t ip)
{
    if (0x7f == (((const uint8_t *)&ip)[0]))
    {
        return 1;
    }
    /* Binary search: a node can carry hundreds of addresses (kube-proxy IPVS). */
    const uint32_t key = __builtin_bswap32(ip);
    uint32_t lo = 0, hi = native->host_ip_count;
    while (lo < hi)
    {
        const uint32_t mid = lo + (hi - lo) / 2;
        if (native->host_ips[mid] < key)
        {
            lo = mid + 1;
        }
        else
        {
            hi = mid;
        }
    }
    return lo < native->host_ip_count && native->host_ips[lo] == key;
}

/* Whether `ip` (network order) is one of the port's own addresses. */
static inline int rusteron_dpdk_port_owns_ip(const rusteron_dpdk_port_t *port, uint32_t ip)
{
    if (ip == port->local_ip)
    {
        return 1;
    }
    for (uint32_t i = 0; i < port->extra_ip_count; i++)
    {
        if (port->extra_ips[i] == ip)
        {
            return 1;
        }
    }
    return 0;
}

/* The kernel path for host-local traffic (rusteron_dpdk_kernel.c). Open a
 * non-blocking UDP socket bound to `bind_addr` (port 0: the kernel picks),
 * optionally connected; *port_out is the bound port. */
struct mmsghdr;
struct iovec;
int rusteron_dpdk_kernel_open(
    int *fd_out,
    const struct sockaddr_in *bind_addr,
    const struct sockaddr_in *connect_addr,
    const aeron_udp_channel_transport_params_t *params,
    uint16_t *port_out);
void rusteron_dpdk_kernel_close(int *fd);
/* Datagrams sent (a prefix of iov), 0 on EAGAIN-like pressure, -1 on error.
 * `address` NULL on a connected socket. */
int rusteron_dpdk_kernel_send(
    int fd, const struct sockaddr_storage *address, struct iovec *iov, size_t iov_length, int64_t *bytes_sent);
/* Receive up to vlen datagrams into msgvec's buffers and dispatch each. */
/* `dpdk_client` non-NULL (a DPDK transport's kernel socket): datagrams from
 * sources that are not host-local, or not its connected peer, are read and
 * discarded. Returns the datagrams read, -1 on an error, or
 * RUSTERON_DPDK_KERNEL_RECV_AGAIN when a transient error (a pending ICMP
 * error, EINTR) was consumed and datagrams may still wait. */
#define RUSTERON_DPDK_KERNEL_RECV_AGAIN (-2)
int rusteron_dpdk_kernel_recv(
    int fd,
    const rusteron_dpdk_client_t *dpdk_client,
    aeron_udp_channel_transport_t *transport,
    struct mmsghdr *msgvec,
    size_t vlen,
    int64_t *bytes_rcved,
    aeron_udp_transport_recv_func_t recv_func,
    void *clientd);

/* Poller (poller.c): put a transport's kernel socket on a poller's list;
 * receive for a transport through the poller it is registered with (every
 * endpoint on the NIC dispatched, none dropped). */
int rusteron_dpdk_poller_kernel_add(struct rusteron_dpdk_poller_state_stct *state, aeron_udp_channel_transport_t *transport);
int rusteron_dpdk_poller_receive_registered(
    rusteron_dpdk_client_t *client,
    struct mmsghdr *msgvec,
    size_t vlen,
    int64_t *bytes_rcved,
    aeron_udp_transport_recv_func_t recv_func,
    void *clientd);

/* The local-I/O thread and kernel-socket readiness (kernel.c). */
void rusteron_dpdk_local_io_stop(rusteron_dpdk_transport_t *native);
int rusteron_dpdk_ksock_register(rusteron_dpdk_transport_t *native, int fd);
void rusteron_dpdk_ksock_unregister(rusteron_dpdk_transport_t *native, int slot, int fd);
uint16_t rusteron_dpdk_port_tx(
    rusteron_dpdk_transport_t *native, rusteron_dpdk_port_t *port, rusteron_dpdk_mbuf_t **pkts, uint16_t nb);
void rusteron_dpdk_runtime_agents_known(
    rusteron_dpdk_transport_t *native, int conductor, int sender, int receiver, const char *conductor_idle);
void rusteron_dpdk_local_io_open_requested(rusteron_dpdk_transport_t *native, uint64_t now_ns);

/* One housekeeping pass: sample both ports' statistics (1 Hz gated). */
void rusteron_dpdk_housekeeping_tick(rusteron_dpdk_transport_t *native, uint64_t now_ns);
/* Start/stop the housekeeping thread (runtime init/cleanup). */
int rusteron_dpdk_housekeeping_start(rusteron_dpdk_transport_t *native);
void rusteron_dpdk_housekeeping_stop(rusteron_dpdk_transport_t *native);
/* The CPU the EAL's main lcore runs on, or -1 (the fake EAL): where the
 * housekeeping thread goes unless RUSTERON_DPDK_HOUSEKEEPING_CPU says. */
int rusteron_dpdk_eal_main_cpu(void);
/* RUSTERON_DPDK_HOUSEKEEPING_CPU, else the EAL main lcore's CPU; -1 none, -2 bad env. */
#include <sched.h>
#ifdef CPU_SETSIZE /* cpu_set_t: only units built with _GNU_SOURCE + sched.h */
int rusteron_dpdk_housekeeping_affinity(const rusteron_dpdk_transport_t *native, cpu_set_t *set);
/* `cpus` NULL: unpinned. */
int rusteron_dpdk_local_io_start(rusteron_dpdk_transport_t *native, const cpu_set_t *cpus);
#endif

/* EAL seam — implemented by rusteron_dpdk_eal.c (real rte_eal) in production
 * and by test/rusteron_dpdk_fake_eal.c in test builds. */
typedef struct rusteron_dpdk_eal_params_stct
{
    const rusteron_dpdk_config_t *config;
    int mode; /* RUSTERON_DPDK_EAL_* */
} rusteron_dpdk_eal_params_t;

int rusteron_dpdk_eal_init(const rusteron_dpdk_eal_params_t *params, char *errbuf, size_t errlen);
int rusteron_dpdk_eal_is_initialized(void);

/* Register/unregister the calling thread with DPDK (plan §7.2: "Register Aeron
 * network threads with DPDK"). Real impl in rusteron_dpdk_eal.c, no-op in
 * test/rusteron_dpdk_fake_eal.c. Both are safe on already-registered and EAL
 * threads. */
int rusteron_dpdk_eal_thread_register(void);
int rusteron_dpdk_eal_thread_unregister(void);

/* MANA's per-queue ring limit (MAX_RECEIVE/SEND_BUFFERS_PER_QUEUE in the PMD). */
#define RUSTERON_DPDK_MANA_MAX_DESCRIPTORS 256

/* Parse aa:bb:cc:dd:ee:ff (either case) into bytes. 0 on success. */
int rusteron_dpdk_parse_mac(const char *text, uint8_t mac[RUSTERON_DPDK_ETH_ADDR_LEN]);

/* The EAL device arguments for the configured ports, space-separated, into
 * `out`: `-a <bdf>` per ENA, `-a <bdf>,mac=<m>[,mac=<m>]` once per MANA
 * function, `--vdev=<name>` per test vdev. DPDK-free so tests can assert it;
 * rusteron_dpdk_eal.c splits it into argv. 0 on success. */
int rusteron_dpdk_eal_allow_args(const rusteron_dpdk_config_t *cfg, char *out, size_t len);

/* Runtime orchestration (rusteron_dpdk_runtime.c). */
int rusteron_dpdk_runtime_init(rusteron_dpdk_transport_t *native);
void rusteron_dpdk_runtime_cleanup(rusteron_dpdk_transport_t *native);

/* Resolve a device BDF against the allow-list. Only the two configured ENA
 * devices may be opened; anything else is rejected (plan §7.2). */
int rusteron_dpdk_runtime_probe_device(
    rusteron_dpdk_transport_t *native, const char *pci_bdf, uint16_t *port_id);

/* Test hooks (runtime.c). Production behaviour is the default (REAL, no
 * override, reset is a no-op for a fresh process). */
void rusteron_dpdk_test_reset(void);
void rusteron_dpdk_test_set_eal_mode(int mode);
void rusteron_dpdk_set_port_ops(const rusteron_dpdk_port_ops_t *ops);

/* Test reset for the transport.c singleton (cleared by rusteron_dpdk_test_reset). */
void rusteron_dpdk_transport_test_reset(void);

/* Monotonic nanosecond clock (runtime.c): the invariant TSC calibrated against
 * CLOCK_MONOTONIC, or CLOCK_MONOTONIC itself when the TSC is not invariant.
 * Calibrated once per process by rusteron_dpdk_clock_init (runtime init), or
 * lazily on first use. Tests pin it with the setter so ARP rate limiting,
 * expiry and the counter sample gate are deterministic; reset clears the pin. */
#define RUSTERON_DPDK_CLOCK_MONOTONIC 0
#define RUSTERON_DPDK_CLOCK_TSC 1
#define RUSTERON_DPDK_CLOCK_UNCALIBRATED (-1)
#define RUSTERON_DPDK_CLOCK_PINNED 2

/* The clock's state (runtime.c). Hidden, so the inlined fast path below reads
 * it RIP-relative rather than through the GOT. */
typedef struct rusteron_dpdk_clock_stct
{
    int mode;          /* RUSTERON_DPDK_CLOCK_{TSC,MONOTONIC,UNCALIBRATED,PINNED} */
    uint64_t tsc_base; /* TSC at calibration */
    uint64_t ns_base;  /* CLOCK_MONOTONIC ns at calibration */
    uint64_t tsc_mult; /* ns per tick, 32.32 fixed point */
} rusteron_dpdk_clock_t;
extern rusteron_dpdk_clock_t rusteron_dpdk_clock __attribute__((visibility("hidden")));
uint64_t rusteron_dpdk_clock_ns_slow(void);

/* The transport's clock read, inlined into the hot paths: the TSC scaled to
 * ns, or the slow path (CLOCK_MONOTONIC, a test pin, first calibration). */
static inline uint64_t rusteron_dpdk_clock_now(void)
{
#if defined(__x86_64__)
    if (__builtin_expect(RUSTERON_DPDK_CLOCK_TSC == rusteron_dpdk_clock.mode, 1))
    {
        const uint64_t delta = __rdtsc() - rusteron_dpdk_clock.tsc_base;
        return rusteron_dpdk_clock.ns_base +
            (uint64_t)(((unsigned __int128)delta * rusteron_dpdk_clock.tsc_mult) >> 32);
    }
#endif
    return rusteron_dpdk_clock_ns_slow();
}

/* rusteron_dpdk_clock_now as an exported function (tests, benchmarks). */
uint64_t rusteron_dpdk_clock_ns(void);
void rusteron_dpdk_clock_init(void);
/* RUSTERON_DPDK_CLOCK_TSC or RUSTERON_DPDK_CLOCK_MONOTONIC. */
int rusteron_dpdk_clock_source(void);
void rusteron_dpdk_test_set_clock_ns(uint64_t ns);
/* Recalibrate; allow_tsc == 0 forces the CLOCK_MONOTONIC fallback. */
int rusteron_dpdk_test_clock_recalibrate(int allow_tsc);

/* CLOCK_REALTIME in ns, the clock NIC RX timestamps are compared against
 * (chrony disciplines it to the NIC's PTP hardware clock). Read only when a
 * timestamped frame arrives. Tests pin it; reset clears the pin. */
uint64_t rusteron_dpdk_realtime_ns(void);
void rusteron_dpdk_test_set_realtime_ns(uint64_t ns);

/* The process-lifetime native runtime singleton (transport.c), or NULL when no
 * transport has been created. EAL can be initialized once, so there is at most
 * one. */
rusteron_dpdk_transport_t *rusteron_dpdk_active_runtime(void);

/* Test inspection of the two role ports (distinct ports / mempools). */
void rusteron_dpdk_transport_test_dump(
    const rusteron_dpdk_transport_t *native,
    uint16_t *sender_port, uintptr_t *sender_pool,
    uint16_t *receiver_port, uintptr_t *receiver_pool);

/* Test-only ARP helpers (runtime.c) so the tx tests can drive the next-hop
 * cache directly: seed a reachable entry, or feed an incoming ARP frame through
 * the handler as if it arrived on the given role (0 = sender, 1 = receiver). */
int rusteron_dpdk_transport_test_arp_seed(
    rusteron_dpdk_transport_t *native,
    const char *ip_str, const uint8_t mac[6]);
int rusteron_dpdk_transport_test_arp_rx(
    rusteron_dpdk_transport_t *native,
    int role, const uint8_t *frame, size_t frame_len);

/* Thread-local error buffer (defined in rusteron_dpdk_transport.c). */
void rusteron_dpdk_set_error(const char *message);
void rusteron_dpdk_set_error_code(const char *message, int code);

/* Aeron poller callbacks (rusteron_dpdk_poller.c, plan §7.6/§7.7). */
int rusteron_dpdk_poller_init(
    aeron_udp_transport_poller_t *poller,
    aeron_driver_context_t *context,
    aeron_udp_channel_transport_affinity_t affinity);
int rusteron_dpdk_poller_close(aeron_udp_transport_poller_t *poller);
int rusteron_dpdk_poller_add(
    aeron_udp_transport_poller_t *poller, aeron_udp_channel_transport_t *transport);
int rusteron_dpdk_poller_remove(
    aeron_udp_transport_poller_t *poller, aeron_udp_channel_transport_t *transport);
int rusteron_dpdk_poller_poll(
    aeron_udp_transport_poller_t *poller,
    struct mmsghdr *msgvec,
    size_t vlen,
    int64_t *bytes_rcved,
    aeron_udp_transport_recv_func_t recv_func,
    aeron_udp_channel_transport_recvmmsg_func_t recvmmsg_func,
    void *clientd);

/* Shared receive loop (rusteron_dpdk_poller.c): poll `client`'s port and
 * dispatch each valid frame to recv_func. When `endpoints` is non-NULL the
 * target transport is chosen by the map; otherwise only frames addressed to
 * `only_transport`'s own local endpoint are dispatched. Returns the number of
 * datagrams dispatched; accumulates payload bytes in *bytes_rcved. */
int rusteron_dpdk_poller_receive(
    rusteron_dpdk_client_t *client,
    aeron_udp_channel_transport_t *only_transport,
    const rusteron_dpdk_endpoint_map_t *endpoints,
    struct mmsghdr *msgvec,
    size_t vlen,
    int64_t *bytes_rcved,
    aeron_udp_transport_recv_func_t recv_func,
    void *clientd);

/* Test inspection of the receive counters (runtime.c). */
void rusteron_dpdk_transport_test_local_watch(const void *transport, int *watched, int *unwatched);
void rusteron_dpdk_transport_test_rx_stats(
    const rusteron_dpdk_transport_t *native, rusteron_dpdk_rx_stats_t *out);

#ifdef __cplusplus
}
#endif

#endif /* RUSTERON_DPDK_INTERNAL_H */
