# ergon-md-dpdk

An experimental DPDK kernel-bypass UDP transport for the
[rusteron](https://crates.io/crates/rusteron-media-driver) Aeron media driver,
on AWS (ENA) and Azure (MANA), Linux x86_64. Benchmarks against the kernel UDP
path: [BENCHMARKS.md](BENCHMARKS.md).

> **AI assistance.** Like the rest of ergon, this crate was written **with heavy
> AI assistance**. See
> [AI-ASSISTANCE.md](https://github.com/mimran1980/ergon/blob/main/AI-ASSISTANCE.md).
> It is not production-ready.

Aeron's C media driver sends and receives through a table of function
pointers, `aeron_udp_channel_transport_bindings_t`. This crate's native C fills
in that table on DPDK, with one dedicated NIC for the sender agent and one for
the receiver, and installs it into an `AeronDriverContext`. The conductor keeps
resolving on the kernel socket, and the VM's primary NIC stays kernel-owned.

| Cloud | DPDK driver | Port selected by |
|---|---|---|
| AWS | `net_ena`, the ENA bound to `vfio-pci` | its PCI address |
| Azure | `net_netvsc` over the MANA VF (Microsoft's recommended path), or `net_mana` alone | its MAC (`RUSTERON_DPDK_*_MAC`): every accelerated NIC of a VM is a vport on one PCI function |

It needs rusteron-media-driver 0.2.11 or newer, which exports the headers of
the Aeron it builds (`DEP_AERON_DRIVER_*`) so this crate's C compiles against
exactly the driver it is installed into.

## Requirements

- The `dpdk` feature, on Linux x86_64 with `libdpdk >= 23.11` found through
  pkg-config (`libdpdk-dev` on Ubuntu 24.04, or DPDK built from source on
  Amazon Linux 2023, as `deploy/aws-eks/Dockerfile.media-driver` does).
- Without the feature the crate still builds on every platform: the
  configuration, environment parsing and transport selection compile, and
  selecting `dpdk-ena` returns `DpdkError::FeatureDisabled`.
- `static` links the Aeron C driver statically (rusteron's `static`).

## Selecting the transport

The bundled `media_driver` binary selects the transport from one environment
variable:

```sh
RUSTERON_MEDIA_DRIVER_TRANSPORT=dpdk-ena cargo run -p ergon-md-dpdk --features dpdk --bin media_driver
```

- Absent or `default`: the socket transport, unchanged.
- `dpdk-ena`: the DPDK transport. A configuration, validation or native
  failure exits non-zero; it never falls back to the socket driver.
- Anything else: invalid, exits non-zero.

The variable names are rusteron's, so deployments written for rusteron's DPDK
branch work unchanged. `RUSTERON_DPDK_EAL_ARGS` appends EAL options (log
levels, lcore lists such as `-l 0`), at most 1 KiB with the device arguments. The DPDK configuration is read from the environment
(`src/env.rs`). The required values are `RUSTERON_DPDK_FILE_PREFIX`,
`RUSTERON_DPDK_SENDER_PCI`, `RUSTERON_DPDK_SENDER_IPV4_CIDR` and
`RUSTERON_DPDK_SENDER_GATEWAY`, plus their `RUSTERON_DPDK_RECEIVER_*` twins.
The Aeron context must be a dedicated-thread driver with distinct sender and
receiver CPUs, spin idle strategies, disjoint wildcard port ranges, and an MTU
no larger than `RUSTERON_DPDK_MAX_AERON_MTU`. These are set through the
standard `AERON_*` variables (`AERON_SENDER_CPU_AFFINITY`,
`AERON_SENDER_IDLE_STRATEGY`, `AERON_SENDER_WILDCARD_PORT_RANGE`,
`AERON_MTU_LENGTH`, ...).

An embedded driver can install the transport with a typed
`DpdkTransportConfig` instead; see
[`examples/embedded_dpdk.rs`](examples/embedded_dpdk.rs).

## Azure

Each data NIC's synthetic VMBus device must belong to `uio_hv_generic` (its
kernel interface and VF down) so the netvsc PMD can take it and keep the VF data
path switched in; `benches/cloud/vm.sh driver dpdk` shows the steps. MANA queues
hold at most 256 descriptors (`RUSTERON_DPDK_RX_DESCRIPTORS` and
`RUSTERON_DPDK_TX_DESCRIPTORS`), and the transport stops DPDK's automatic
vdev_netvsc fail-safe device, which would otherwise take the ports (and the
primary NIC's).

## CPU pinning

Aeron's C driver only parses `AERON_{CONDUCTOR,SENDER,RECEIVER}_CPU_AFFINITY`;
`aeronmd` applies them with an agent start hook that rusteron's driver lacks.
`pin_agents_to_configured_cpus` installs that hook, `DpdkTransport::install`
calls it, and so does the bundled `media_driver` in every mode. Without it the
agents float, or, under DPDK, inherit the EAL's CPU 0 pin, which puts the sender
and receiver on one core (milliseconds per round trip).

## No pauses on the hot path

The driver's sender and receiver agents only send, receive and bump counters.
Nothing periodic runs on them:

- **ARP.** Only the first send to a next hop waits for its resolution (Aeron
  retries the zero-byte result); a next hop that stays silent for 3 s fails
  one send with `EHOSTUNREACH`, as Linux reports a failed neighbour once, and
  later sends to it are dropped silently (as an unconnected `sendto`
  succeeds) while it is re-probed once a second. Each NIC keeps up to 1024
  next hops; a lookup examines at most 16 slots, and a new next hop finding
  none free replaces the least recently used one idle for 60 s (a next hop
  in use is never replaced). If none is idle, its datagrams are dropped and
  `ENOBUFS` is reported at most once a minute. Requests for the NIC's
  addresses are answered at most 1000 times a second. After resolution the cached MAC is used for every
  send: at 30 s a refresh request goes out alongside the traffic, retried at
  most once a second until answered, and an unanswered refresh keeps the MAC.
  A frame from an on-link peer's IP arriving from another MAC (a pod
  rescheduled, a failover) makes the next send refresh at once. Only a solicited answer
  moves the next hop to a new MAC; unsolicited ARP never does. Each NIC keeps
  its own table, touched only by its own agent thread.
- **Statistics.** The 1 Hz PMD extended statistics, the NIC's basic drop
  counters (`nic-rx-errors`, `nic-rx-missed`, `nic-rx-nombuf`) and the mempool
  sample (on ENA, some of those are admin commands to the device) run on a
  housekeeping thread. It and the local-I/O thread (see Host-local channels)
  share the conductor's core, the driver's housekeeping core: they run on
  `RUSTERON_DPDK_HOUSEKEEPING_CPU`; else on the conductor's CPU, unless the
  conductor busy-spins; else on the EAL's main lcore CPU; never on a core a
  busy-spinning agent owns (SMT siblings included), else on the process's
  CPUs minus those. The agents' CPUs and idle strategies are the driver
  context's (env or setters), read when the transport is installed.
- **Nothing else on the agents' cores.** The EAL's own threads (interrupts
  and alarms, multi-process messages) would be allowed on every CPU the
  creating thread could use except the lcores, the agents' among them: the
  transport confines each thread `rte_eal_init` starts to the main lcore's
  CPUs. Threads that are not the transport's (a signal handler, Aeron's own)
  inherit the process's CPUs, so start the driver confined to its
  housekeeping CPUs (`taskset -c 0 media_driver`); the agents are pinned
  explicitly and leave that mask.
- **Receive-ring health.** Each port counts the longest gap between receive
  polls that found frames waiting (`rx-poll-gap-max-ns`), how many reached
  100 µs, and polls that drained a full burst (`rx-burst-full`), so a stall
  long enough to overflow the NIC's ring shows up. Every poll also times the
  gap since the port's last one, frames or not (`poll-cycle-gap-max-ns`, and
  how many fell in 20–50, 50–100 and over 100 µs): the agent's own loop, so a
  latency spike can be matched to the agent not running. The clock is read
  there anyway.

## Kernel socket semantics

The transport behaves as the kernel UDP sockets Aeron's own transport uses,
so DPDK peers and kernel peers interoperate:

- **Ports.** An explicit port already bound on the NIC fails init with
  `EADDRINUSE` at once. A wildcard bind (port 0, as every plain `endpoint=`
  publication has) gets an ephemeral port, unique on the NIC, reported by
  `bind_addr_and_port`; port 0 never goes on the wire. The range is
  `RUSTERON_DPDK_EPHEMERAL_PORT_RANGE="<low> <high>"`, else the network
  namespace's `net.ipv4.ip_local_port_range`, skipping
  `net.ipv4.ip_local_reserved_ports` and Aeron's configured wildcard ranges,
  starting at a random offset. For a fixed, firewall-friendly source port use
  Aeron's `interface=<nic-ip>:<port>` on the channel. If a kernel interface
  holds a DPDK NIC's IP, start fails unless the transport's range avoids the
  kernel's or is reserved there.
- **Addresses.** A bind must be `0.0.0.0` or the NIC's address (or one of
  `RUSTERON_DPDK_{SENDER,RECEIVER}_EXTRA_IPV4`, e.g. a secondary IP), else
  `EADDRNOTAVAIL`. Each address holds its own ports: the NIC's address and an
  extra one may both bind a port, while a `0.0.0.0` bind of it conflicts with
  either. An extra address is answered in ARP and used as the source of what
  its channels send; a `0.0.0.0` channel answers each peer from the address
  that peer last reached it on. Multicast fails at creation. Sends to `0.0.0.0`, a
  broadcast address, port 0 or the NIC's own address are errors.
- **Connected transports** receive only from their peer, and `reconnect`
  re-targets only a connected transport.
- **Receive.** IP options are skipped; UDP checksums are verified per RFC 768
  (a zero checksum is accepted even when the NIC calls it bad, and `0xFFFF` is
  valid); IPv4 fragments and frames larger than one mbuf are dropped and
  counted. Datagrams to a port nobody listens on get an ICMP port unreachable
  (at most 100 a second per NIC); ICMP echo is answered (at most 100 a second,
  payloads up to 512 bytes); an ICMP fragmentation-needed is counted and
  reported at most once a second.
- **Bandwidth allowance.** Clouds cap each VM's network rate and drop what
  exceeds it before any counter here sees it; Aeron then retransmits, and
  throughput collapses (measured on Azure: about 11 Gbps of 1,344-byte frames
  fell to 40 thousand messages a second). `RUSTERON_DPDK_TX_RATE_MBPS` caps
  each NIC's transmit rate in megabits per second of frames (default 0: no
  cap) with a token bucket of `RUSTERON_DPDK_TX_BUCKET_BYTES` (default 64 KiB):
  a send that finds it overdrawn waits the debt out if that takes at most
  `RUSTERON_DPDK_TX_PACE_SPIN_US` (default 20), else sends nothing, which Aeron
  retries whole (counter `tx-paced`). Set it below the VM's allowance, shared
  between the two NICs. A slower link on the way to some destinations (a
  direct link to another site, say) gets its own allowance with
  `RUSTERON_DPDK_TX_ROUTE_MBPS="<a.b.c.d>/<len>=<Mbit/s>,..."` (up to 16
  prefixes; the longest match applies, on top of the NIC's), shared by every
  channel crossing that link. Size each channel's receiver window (`rcv-wnd`)
  to its link's bandwidth-delay product too, so one stream cannot burst past
  it. Kernel UDP has no congestion control either: the same holds for the
  kernel driver, which only lacks a pacer.
- **Stream priority under the allowance.** Every stream on a NIC draws on its
  one allowance, so near the cap a latency-critical stream could wait behind
  bulk. `RUSTERON_DPDK_TX_LOW_STREAMS="<stream>|<first>-<last>,..."` (up to
  16, non-negative stream ids) holds streams that can wait below the rest:
  they send only from what leaves `RUSTERON_DPDK_TX_LOW_RESERVE_BYTES`
  (default half the bucket) for the others, or from a full bucket, never
  overdraw it and never wait for it (a zero result Aeron retries, counted as
  `tx-paced`). Everything else, control frames included whatever stream they
  name, then never queues behind them. It needs an allowance
  (`RUSTERON_DPDK_TX_RATE_MBPS` or `RUSTERON_DPDK_TX_ROUTE_MBPS`): without
  one nothing here queues, so there is nothing to reorder. Priority keys on
  the stream id each data frame carries; a subscription's `reliable=false`
  never reaches the publishing side, so it cannot.
- **Full transmit ring.** A send the NIC's ring cannot take whole returns the
  accepted prefix (a short send). Aeron then resends the whole vector, the
  datagrams already sent included. `RUSTERON_DPDK_TX_RETRY_US` (default 0)
  offers the refused tail again for up to that long while the NIC drains its
  ring, trading a brief spin on the sender for no duplicates.
- **Checksums out.** The NIC completes them when it offers both IPv4 and UDP
  checksum offload; otherwise the transport computes them.
- **MTU.** `max_aeron_mtu` may be up to 8960 (jumbo frames, e.g. an AWS
  9001-byte VPC) and must fit the NIC's maximum, checked at start. Every peer's
  Aeron MTU must be at most `max_aeron_mtu`: a SETUP announcing more is dropped
  and reported (`peer-mtu-too-large`), so the publication shows as not
  connected instead of stalling.

## Host-local channels

A DPDK NIC cannot reach its own host. Channels bound on or connected to
loopback (`127.0.0.0/8`) or one of the host's kernel interfaces (Archive and
Cluster control channels on `localhost`, a co-located process) therefore run
on ordinary kernel UDP sockets, exactly as Aeron's kernel transport would
carry them, in the same driver. A DPDK publication with a host-local
destination among its remote ones (MDC) sends that one through a kernel
socket and the rest over DPDK. A local-I/O thread (pinned with housekeeping)
waits on those sockets with epoll and flags one when data is waiting, so the
DPDK duty cycle reads a kernel socket only when it has something and never
pays an idle syscall or touches epoll. A DPDK publication's first send to a
host-local destination asks that thread to open a socket bound on the
destination's own address (with the publication's port where free there);
the agent reports zero-length sends, which Aeron retries, until it is ready
within about 10 ms, and never makes the socket or bind calls itself. A failed
open is reported once and retried each second, its datagrams dropped
meanwhile. Replies on it reach the publication. A `0.0.0.0` subscription also
listens on the kernel's interfaces, as a kernel socket would
(`RUSTERON_DPDK_WILDCARD_LOCAL=0` or `false` opts out; any value but
`0`, `1`, `false` and `true` fails initialisation). These kernel sockets hear only
this host (and a connected transport only its peer): a remote peer reaching
one through a kernel interface would get replies over DPDK from another
address, so it is not heard. Host-local channels are not limited by `max_aeron_mtu`.
`RUSTERON_DPDK_HOST_LOCAL=loopback` limits host-local to `127/8`, for a test
rig whose DPDK peer is a kernel interface on the same host.
`RUSTERON_DPDK_LOCAL_IO` picks how the agents read their host-local sockets:
`adaptive` (the default) only when the local-I/O thread flags data while a
socket is idle, so idle local channels cost the DPDK path nothing, but every
duty cycle for `RUSTERON_DPDK_LOCAL_HOT_US` (default 1000) after it last had
data, so an active one is read as promptly as the kernel driver reads it.
While its agent reads a socket every cycle, the local-I/O thread takes it off
its epoll: a registered socket makes every sender's datagram wake that
thread, in the sender's own syscall over loopback (a third of a busy
localhost sender's time, measured). It goes back on within a millisecond of
the agent cooling, and going back on reports a datagram already waiting
(`RUSTERON_DPDK_LOCAL_UNWATCH=0` keeps them on).
`epoll` only ever reads on the flag, which adds a thread wake-up to every local
message (about twice the localhost round trip: see BENCHMARKS.md); `spin`
every duty cycle always (a `recvmmsg` per socket per cycle, idle or not), with
no socket on the epoll at all.

## Clocks

The transport reads time on every send and poll (ARP freshness, receive-poll
gaps) from the invariant TSC, scaled to nanoseconds by a factor
calibrated once, over 50 ms, against `CLOCK_MONOTONIC_RAW` when the first
transport starts (unslewed, like the TSC), and anchored to `CLOCK_MONOTONIC`. A
CPU without `constant_tsc` and `nonstop_tsc` gets `CLOCK_MONOTONIC` instead.
Each port's `clock` counter (type 80) reads 1 for TSC, 0 for the fallback.
`RUSTERON_DPDK_CLOCK=monotonic` forces the fallback (`tsc`, the default, or
unset otherwise; anything else fails initialization), to compare the two.

## NIC receive timestamps

`RUSTERON_DPDK_RX_HW_TIMESTAMP=1` (`DpdkTransportConfig::rx_hw_timestamp`)
requests the NIC's hardware receive timestamps, and initialization fails on a
port whose driver does not offer them. Today that is AWS ENA on Gen7+
instances in a `precision-time` placement group (a cluster group can sit
inside one), with DPDK 25.11 or later. Azure MANA has none. Each port then
counts, for every timestamped datagram, the delay from NIC arrival to the
transport handing it to Aeron (type 81: `rx-hw-delay-max-ns`,
`rx-hw-delay-sum-ns`, `rx-hw-timestamped`). The NIC stamps with its PTP
hardware clock and the transport compares against `CLOCK_REALTIME`, so the
delays mean something only while chrony keeps the host on that clock: Amazon's
ENA driver built with `ENA_PHC_INCLUDE=1`, loaded with `phc_enable=1`, and
`refclock PHC /dev/ptp_ena poll 0 delay 0.000010 prefer` in chrony
(`benches/cloud/vm.sh build-ptp` does all three).

## Lifetime

`DpdkTransport` owns the native runtime the installed bindings point into.
Keep it alive until every driver started from the context has stopped:
dropping it while a driver still runs is a use-after-free in the driver's
agents. rusteron 0.2.11's context cannot hold that dependency itself.

## Layout

| Path | What |
|---|---|
| `src/` | Rust API: config, environment, errors, FFI, and the `media_driver` binary |
| `native/dpdk/` | The transport in C, split so tests link DPDK-free fakes of the EAL and port seams |
| `tests/` | Config/environment and binary-selection tests (every platform); ABI, runtime, TX, RX, counters and interop tests against the fakes (`dpdk`) |
| `harness/` | `dpdk-harness`, the interop scenario runner `scripts/test-dpdk-vdev.sh` and `scripts/aws-dpdk-acceptance.sh` drive, and the `rtt`, `tput` and `counters` benchmark clients (unpublished) |
| `benches/cloud/` | The cloud labs behind BENCHMARKS.md: `azure/lab.sh` and `aws/lab.sh` (two VMs, created and deleted per run), `vm.sh` (each VM's side), `remote.sh`, `analyse.py` |
| `device-plugin/` | Kubelet device plugin advertising VFIO ENA pairs (unpublished) |
| `deploy/aws-eks/` | Runtime images, DaemonSets and the node bootstrap and preflight scripts |
| `docs/` | The AWS EKS rollout and rollback runbook |

## Checks

From the ergon root:

```sh
cargo test -p ergon-md-dpdk -- --test-threads=1   # every platform
just check-dpdk                                   # Linux x86_64 + libdpdk
just verify-dpdk-docker --build                   # the same in Docker amd64 emulation
just test-dpdk-bootstrap                          # node scripts against fixtures
just test-dpdk-vdev                               # tap-device interop (root)
```

## License

Apache-2.0.
