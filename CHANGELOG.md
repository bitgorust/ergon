# Changelog

## [Unreleased]

### Added
- `k8s/nri-shared-cpus` (experimental, unpublished): a containerd NRI plugin
  and kubelet device plugin. A Guaranteed container asks
  `ergon.io/shared-millicpus` and gets its exclusive CPUs plus shared
  housekeeping CPUs; every thread starts on the shared CPUs;
  `ERGON_ISOLATED_CPUS`/`ERGON_SHARED_CPUS` let the app pin by index;
  `ergon-cpus` turns `isolated:<i>` into real CPU numbers (e.g. for the
  Aeron media driver). Tested on kind, not yet on EKS.
- `ergon-md-dpdk`: an experimental DPDK ENA kernel-bypass UDP transport for
  the rusteron Aeron media driver (Linux x86_64 or aarch64,
  `libdpdk >= 23.11`, `dpdk` feature), installed through
  rusteron-media-driver 0.2.11's custom transport bindings. Selected by
  `RUSTERON_MEDIA_DRIVER_TRANSPORT=dpdk-ena` in the bundled `media_driver` or
  with `DpdkTransport::install`. Ships with an
  interop harness, a kubelet device plugin and AWS EKS deployment assets
  (unpublished). Runs on AWS (`net_ena`) and Azure (`net_netvsc` over MANA,
  ports selected by `RUSTERON_DPDK_{SENDER,RECEIVER}_MAC`); pins the driver's
  agents to `AERON_*_CPU_AFFINITY` (`pin_agents_to_configured_cpus`).
  Benchmarks against kernel UDP on Azure: `md-dpdk/BENCHMARKS.md`.
  Times in nanoseconds with `ergon-clock`'s calibration, made once a process
  by `DpdkTransport::install` (`ergon_md_dpdk::clock`, cross-check trust,
  monotonic domain) and handed to the native transport in its configuration
  (ergon-clock's `Params`, checked at create): the C hot path reads the
  counter inline (`rdtsc`, or `mrs CNTVCT_EL0` on aarch64, the same
  instructions as before) and converts it with ergon-clock's arithmetic to the
  bit, with no call into Rust and no added branch; where ergon-clock falls
  back, `CLOCK_MONOTONIC` through the same formula (counter type 80 reports
  which; `RUSTERON_DPDK_CLOCK=counter`, or `tsc` as before, or `monotonic`,
  read by the install, `DpdkError::InvalidEnvironment` for anything else).
  The native code's own calibration and its test hooks are gone, and
  `DpdkTransport::clock` gives callers the clock the transport adopted;
  and with `RUSTERON_DPDK_RX_HW_TIMESTAMP=1` takes the NIC's hardware RX
  timestamps (AWS ENA in a precision-time placement group, DPDK >= 25.11),
  reporting NIC arrival to the receive burst that carried the frame per port
  (type 81: max, min, sum and count, against `CLOCK_REALTIME` read once a
  receive burst after the NIC handed the burst over and before any frame of it
  is dispatched, so a frame after the burst's first is measured before its own
  dispatch; a frame stamped after that read, skew between the NIC's PTP clock
  and the host's, is counted apart, never in the sum).
  Nothing periodic runs on the driver's agents: a learned ARP next hop is
  refreshed in the background while sends keep using it, and the 1 Hz statistics
  sample runs on a housekeeping thread (`RUSTERON_DPDK_NIC_STATS_MS` sets how
  often it reads the NIC's own statistics, admin commands on ENA; 0 never).
  An agent's loop-gap counters time only the agent: a port's absence from a
  poller (its channels closed and reopened) is no longer counted as a gap.
  A send is all or nothing: what a full TX ring refuses waits, built, in the
  NIC's queue (`tx-queued`) and goes first at the agent's next send or poll,
  and the whole vector is reported sent. Reporting the accepted part made
  Aeron send the whole vector again, that part twice: on AWS ENA an estimated
  third to half of flat-out frames were duplicates. Out of mbufs part-way, a
  send now sends nothing instead of a prefix.
  A receive poll is no longer capped by Aeron's vlen (frames are dispatched
  one at a time, zero copy, so vlen sized nothing): it takes up to the burst
  size a burst and keeps draining while bursts come back full, up to
  `RUSTERON_DPDK_RX_MAX_BURSTS` (default: as many as fill the ring, at most
  8), with `rx-backlog-max` and, on Azure's ports, `rx-ring-full-suspected`
  counters (an ENA counts a full ring's drops itself, `nic-rx-missed`, and
  has no suspicion counter). The
  sender's control poll, which Aeron calls with vlen 1, took one frame a duty
  cycle before. `RUSTERON_DPDK_MBUFS_PER_PORT` defaults to 16384 (what the
  lab measured), and a mempool cache of 0 is honoured (no per-core cache)
  instead of being replaced by 256, with more than 512 rejected.
  The bundled `media_driver` sets receiver IO vectors to 16 under `dpdk-ena`
  unless `AERON_RECEIVER_IO_VECTOR_CAPACITY` is set (never under the socket
  transport), locks its memory with `RUSTERON_MEDIA_DRIVER_MLOCKALL=1`, and
  stops cleanly on SIGTERM and SIGHUP as well as SIGINT; the device plugin
  also stops on SIGTERM without waiting for kubelet's open stream. The EKS
  node setup leaves `vm.min_free_kbytes` and the primary ENA's coalescing and
  busy polling at the kernel's values, caps C-states only on virtualised
  instances, turns transparent hugepages off from the kernel command line,
  and accepts bare-metal nodes with the IOMMU in passthrough.
  Behaves as the kernel UDP sockets Aeron's own transport uses, so it
  interoperates with kernel peers: wildcard binds get an ephemeral port
  (previously port 0 went on the wire and kernel peers could never answer),
  duplicate binds fail with `EADDRINUSE`, foreign bind addresses with
  `EADDRNOTAVAIL`, connected transports hear only their peer, RFC 768
  checksums (`0xFFFF` was wrongly rejected), IP options, ICMP echo and port
  unreachable, software checksums without offload, unanswered ARP reported
  as `EHOSTUNREACH`, jumbo `max_aeron_mtu` up to 8960, and peers with a larger
  MTU rejected at SETUP. Loopback and host-local channels run on kernel
  sockets in the same driver, woken by an epoll thread so idle ones cost the
  DPDK duty cycle nothing (and opened by that thread, never by an agent); a
  `0.0.0.0` subscription also hears localhost. The ARP table replaces next
  hops idle for 60 s instead of filling for good. Bandwidth allowances keep
  sends below a VM's cap (`RUSTERON_DPDK_TX_RATE_MBPS`) and a slower link's
  (`RUSTERON_DPDK_TX_ROUTE_MBPS`), and `RUSTERON_DPDK_TX_LOW_STREAMS` holds
  streams that can wait below the rest under them, so a latency-critical
  stream never queues behind bulk; frames a send does not send or queue (out
  of mbufs, an oversized datagram) are not charged to them, and frames a full
  ring refuses are charged once, when queued. Each NIC's allowance defaults
  to a backstop of 25000 Mbit/s (`0` turns it off), AWS's highest documented
  rate for one flow and above every per-NIC rate measured: it bounds a
  sustained rate and leaves bursts alone, with a 1 MiB bucket (more than a
  full ENA ring of 1,450-byte frames) unless `RUSTERON_DPDK_TX_BUCKET_BYTES`
  is set, and never makes the agent wait (a send it would delay sends
  nothing, and Aeron retries it), so it holds back only a runaway sender. It
  is per NIC: where one NIC carries more than 25 Gbit/s over several flows
  (c7i.metal-24xl, c7i.48xlarge), raise it or set `0`.
  `RUSTERON_DPDK_TX_LOW_STREAMS` still needs an allowance the operator set.
  AWS holds one flow (one channel) to about 8 Gbit/s of frames whatever the
  allowance. A host-local socket whose agent reads it every cycle comes off
  the local-I/O thread's epoll, so busy localhost senders no longer wake that
  thread with every datagram, and the EAL's own threads are confined to the
  main lcore's CPUs, never the agents'. Malformed or overlong
  `RUSTERON_DPDK_*` values (`RUSTERON_DPDK_WILDCARD_LOCAL=off`,
  `RUSTERON_DPDK_EAL_ARGS` past the EAL's 1 KiB argument buffer) fail
  initialisation, naming the variable, instead of being read loosely or cut
  short.
  Runs under every Aeron threading mode, not only `DEDICATED`: under
  `SHARED_NETWORK` the network agent busy-spins on the sender's CPU
  (`AERON_SHAREDNETWORK_IDLE_STRATEGY`), under `SHARED` and `INVOKER` the one
  thread that runs everything on the conductor's (`AERON_SHARED_IDLE_STRATEGY`),
  and the install refuses a mode whose network agent has no CPU or would
  sleep. Housekeeping and local I/O keep off that core, and the install
  refuses one they could not keep off (a creating thread allowed no other
  CPU) or the EAL's main lcore on it; since one thread
  also receives there, a send never waits out a pacing debt
  (`RUSTERON_DPDK_TX_PACE_SPIN_US` is 0 unless set, and a non-zero value is
  refused). `driver::ConfiguredDriver` and `EnvMediaDriver` are the bundled
  `media_driver`'s configuration and main loop, for a process that embeds the
  driver; `EnvMediaDriver::abandon` leaves an `INVOKER` or `SHARED` driver
  unclosed at exit, so its `CnC` keeps every counter. The lab clients (`rtt`,
  `load`, `tput`) embed one with `HARNESS_DRIVER=invoker`, its agents and
  their client's conductor run by their measuring thread, append
  `PUB_CHANNEL_PARAMS` to every publication's channel, and `counters` reads
  the `CnC` file directly, so a stopped driver's counters too. A lab client
  never outlives its run: a back-pressured echo (`rtt xpong`, `load xecho`)
  or ping retries only while it can make progress, giving up on SIGTERM, on
  its image closing, or on its publication staying not connected for over a
  second (an AWS run's pong spun 47 minutes in its echo, its ping gone, past
  the lab's SIGTERM); the cloud lab gives each client a deadline its VM
  enforces, bounds every wait on one (a subscriber piped into the results
  included), never runs a command again over SSH once it has started, and
  ends a client by its program's path, so on VM a its own SSH session to a
  localhost client is never what it kills.
  The ENA PMD's watchdog (device keep-alive, admin queue, missing transmit
  completions), a DPDK timer that never ran, now runs from the housekeeping
  thread once a second, and a device reset a PMD asks for
  (`RTE_ETH_EVENT_INTR_RESET`) is counted per port (`nic-reset-requested`,
  type 79), logged at error level and recorded in the driver's error log;
  the transport does not reset the device. Start checks that each port's
  mbufs hold a frame of `max_aeron_mtu` whole (8960 on a 9001-byte AWS VPC,
  8448 on an 8500-byte path), and validation refuses a `max_aeron_mtu` of 0.
  A clock read behind a bandwidth allowance's last refill no longer refills
  its bucket.
  Unset, `RUSTERON_DPDK_RX_DESCRIPTORS` now gives each port min(4096, the
  NIC's maximum) receive descriptors instead of 1024 (256 on MANA), as AWS
  measured: no first-point `nic-rx-missed` loss, loss-free 1,344 B from 400 k
  to 600 k msgs/s, and no NAKs with two flows, at the same idle latency. An
  explicit count is kept and checked as before.
  `DpdkTransportConfig::rx_descriptors` is now an `Option<u16>` (`None` for
  that default), and validation and start size the mbuf pool for the ring the
  port takes, its transmit ring, four bursts and two lcores' mempool caches,
  naming the variables when it falls short (with the ring unset and a pool
  for MANA's 256 descriptors: `RUSTERON_DPDK_RX_DESCRIPTORS=256`, or a larger
  `RUSTERON_DPDK_MBUFS_PER_PORT`, each with its count). The EKS ConfigMap no
  longer pins `RUSTERON_DPDK_RX_DESCRIPTORS` to 1024. A channel asking for media
  receive timestamps (`media-rcv-ts-offset`), which did nothing over DPDK,
  now gets them as on Aeron's kernel transport: the NIC's receive time with
  `RUSTERON_DPDK_RX_HW_TIMESTAMP=1`, else `CLOCK_REALTIME` read once a
  receive burst, and the kernel's `SO_TIMESTAMPNS` on host-local sockets; a
  NIC without such a channel reads no clock for them. Every ENA port asks for
  its low-latency queue (`llq_policy=1` from DPDK 24.11, whose 24.11.0 and .1
  otherwise leave it off; `enable_llq=1` before), takes the operator's
  `RUSTERON_DPDK_ENA_DEVARGS`, and fails start, naming the port, unless its
  LLQ BAR is mapped write-combined (`resource2_wc` in `/proc/self/maps`) and
  LLQ is not turned off, or it is bound to `vfio-pci` (whose mapping the
  check cannot see) and `RUSTERON_DPDK_VFIO_WC=1` confirms a write-combining
  one, as the EKS preflight requires and the EKS ConfigMap sets;
  `RUSTERON_DPDK_ENA_LLQ_REQUIRED=0` starts it anyway with a warning, for
  diagnosis. Each port's `port-info` label reports its
  receive ring (`rx-desc=`) and an ENA's LLQ finding (`llq=`).
- `ergon-clock`, published (`ergon-md-dpdk` depends on it): a
  hardware-counter clock for low-latency threads. `calibrate` is one
  explicit start-up call (40–70 ms, plus up to 50 ms for the time-sync
  probe) that returns the owner `Clock` and a `Report` of what it chose:
  `rdtsc` (x86-64, invariant TSC with `rdtscp`) or `CNTVCT_EL0` (AArch64)
  scaled to UTC nanoseconds by one 32.32 multiply (the DPDK transport's
  formula), its rate from Arm's `CNTFRQ_EL0` or an x86 nominal rate only
  where a measurement against `CLOCK_MONOTONIC_RAW` cannot tell them apart
  (its error bound from the step that clock's reads advance by, not
  `clock_getres`, and never beyond 50 ppm), else the measurement, then
  verified. The counter is used where the kernel keeps time with it
  (`Trust::CrossCheck` also accepts a paravirtual clock source or macOS);
  otherwise `CLOCK_MONOTONIC` plus an offset that keeps the stamps on UTC,
  with the reason reported (`Policy::RequireCounter` makes it an error).
  Unordered and ordered reads by name (the ordered one `rdtscp`, or on Arm
  `CNTVCTSS_EL0` where the CPU has `FEAT_ECV` and `isb; mrs cntvct_el0`
  where it does not, chosen once at calibration), in a UTC or a since-boot
  domain. The owner keeps it on `CLOCK_REALTIME` with `Clock::update` from
  its loop's idle time (±500 ppm, never back; `Health` with `warning()` and
  `sampled`), other threads read it through `Follower`, and
  `Report::recheck` says from an idle-timer callback whether the kernel
  still keeps time with the counter. `calibrate` prints one line per
  problem (`ERGON_HOST_CHECKS=off` or `0` silences them; `report.warnings()`
  keeps them). `WallOffset` maps a since-boot clock to `CLOCK_REALTIME` with
  its error bound; `ManualClock` and `TimeSource` cover tests. It depends on
  `libc` alone: quanta and minstant were measured against it on c7i, c8g and
  c9g and are not used (`clock/BENCHMARKS.md` has the numbers and the
  reasons; with chrony settled quanta's calibration is 2.7–19.1 ppm off the
  counter and differs per process, and right after a boot with
  systemd-timesyncd it reached 978 ppm).
  `ergon-clock-bench` compares every clock read on a host
  (`clockbench`: cost, gaps, backwards steps across cores, drift against
  `CLOCK_MONOTONIC_RAW` and the NTP-steered `CLOCK_MONOTONIC`, calibration in
  fresh processes; and Criterion); the cloud lab runs it in
  every state (`vm.sh clockbench`), and `LAB_CLOCK_ONLY=1` (AWS) runs only
  it, summarised by `md-dpdk/benches/cloud/clockbench.py`.
  The lab clients (`rtt`, `load`, `tput`, `clockcheck`) stamp with
  ergon-clock (cross-check trust; its ordered read for every latency stamp)
  instead of a clock of their own, in nanoseconds as before. The AWS lab's
  `LAB_DISCIPLINE_SECONDS` runs `clockbench --discipline` on both hosts after
  each state's chrony gate and reports the residual against `CLOCK_REALTIME`
  per host with chrony's tracking around it; the kernel modes name the data
  NICs to the media driver (`RUSTERON_MEDIA_DRIVER_DATA_NICS`) so its NIC
  checks run; ENA's allowance counters are read on both hosts before and
  after every load point, with a warning when one grew; results carry each host's
  media-driver start-up host checks per mode (`hostcheck` rows, a table in
  `analyse.py`); a subscription takes `media-rcv-ts-offset` (`reserved`, or a
  payload offset from 16 to 24, past what the clients read); and
  `LAB_ESTIMATE=1` prices spot runs at the lab's spot prices beside the
  on-demand ceiling.
  The calibration report has a time-sync section (`Report::time_sync`,
  `TimeSyncPolicy`): how `CLOCK_REALTIME` is kept, from `adjtimex` (read
  only), `/sys/class/ptp/*/clock_name`, the time daemons in `/proc/*/comm`,
  chronyd's tracking report over its command port (UDP 127.0.0.1:323, 50 ms
  timeout; the layout checked against chrony 4.6.1) and ClockBound 2.0's
  segment. Its verdict is `ptp-grade`, `ntp-disciplined` (with an error
  bound where one is known), `unsynchronised` or `unknown`. `Report::findings`
  says of each thing it looked at; `Report::warnings` is the smaller list
  `calibrate` prints (a fallback, an unsynchronised or unsettled host clock,
  a pending leap second). It never fails unless `TimeSyncPolicy::RequirePtp`
  asks it to; `Report::recheck_findings` and `time_sync()` re-run the checks
  from an idle-timer callback. `advanced::Clock::fallback` and
  `MonotonicClock::always` always give a clock, with the error beside it,
  so a holder needs no `Option` (the counter's read path is unchanged).
  Under `advanced`, the owner's discipline to `CLOCK_REALTIME`
  (`advanced::discipline`) samples once a second (`Sampler`: the tightest of
  up to 8 brackets of the wall clock, and on Linux of
  `CLOCK_REALTIME - CLOCK_MONOTONIC`, which moves only on steps) and
  re-anchors (`Discipline::rebase` and `update_at`) so the rate follows the
  wall clock's two-point slope and the residual decays at θ/2 s, within a
  ±500 ppm cap (50 to 500) enforced exactly on the factor, never going
  back. It steps forward only at start (or after it, opted in), slews a
  wall-clock step either way, holds a sample taken across interrupts at the
  frequency estimate, never takes a slope over 1,000 ppm for a rate, and is
  pure and integer, with no global state. The `Copy` clock there is a fixed
  segment (`advanced::Clock`, `segment`); other threads follow through a
  seqlock in safe atomics (`share::ClockFollower`: one acquire load and a
  compare a read). `MonotonicClock` converts by the counter's formula (base,
  factor; unretuned it reads bit for bit as before and `Clock` stays five
  words), its `params()` carry its real base and factor, and
  `MonotonicClock::from_params` adopts them. `KernelSync::read` reads
  `adjtimex` without allocating; `realtime_res_ns` gives the wall clock's
  resolution. `clockbench --discipline SECS` runs it live with a 100 Hz
  verifier, both threads busy-waiting (the verifier on `--cpu2`); the
  counter rows read a clock re-anchored 10,000 times, and a follower's read
  (`follower_now_ns`, within 1.5 ns of the owner's) and the fallback's
  (within 1.5 ns of `clock_gettime` of the clock it wraps) are gated.
- `ergon-tune`, published (`ergon-md-dpdk` depends on it): checks of a
  host's low-latency settings for start-up and housekeeping, as pure functions that return findings (ok,
  a finding with its measured effect and exact fix, or unknown with the
  reason; unreadable data is never a warning). Host checks: busy-spinning
  CPUs isolated, `nohz_full` (except a CPU holding only ergon-io's SQPOLL
  thread, where it made no measured difference) and outside the default
  IRQ affinity (T1); a
  NIC's interrupts, its Azure VF's and a DPDK port's by PCI address, off
  those CPUs (T2); transparent hugepages off at every size (T3); the Aeron
  directory on tmpfs or hugetlbfs (T4); NUMA balancing and the NIC's node
  on multi-node hosts (T5, T6); no `SCHED_FIFO`/`SCHED_RR` hot thread
  (T7); no two busy threads on one core (T8); no CPU quota (CFS bandwidth
  limit, cgroup v2 `cpu.max` or v1 `cpu.cfs_quota_us`) on the process's
  cgroup or a parent visible from it, with the Kubernetes fix (T9), and
  `ThrottleWatch`, which reads the cgroup's `nr_throttled`,
  `throttled_usec` and `cpu.stat.local` from files opened once (`cpu.stat`
  only after `cpu.stat.local` grew: its read takes a kernel lock) and warns
  when they grow; ergon-io's `Driver::new` and the media driver run T9 at
  start. The EKS node bootstrap leaves `cpuCFSQuota` at its default `true`
  (every pod keeps its CPU limit) and relies on kubelet 1.33+'s own
  behaviour: no quota on a container with exclusive CPUs. Never
  `cpuCFSQuota: false`: it removes every pod's CPU limit on the node. With `nic`: ENA adaptive RX
  moderation (N1), deferred interrupts without a busy poller (N2), the
  Azure accelerated-networking VF (N3) and the kernel RX ring (N4), N1 and
  N4 over ethtool's generic netlink, unprivileged, cached by the caller
  per interface index and `carrier_changes`. Everything is read through a
  root path (fixture trees in tests), the thread's policy and affinity
  from `/proc/thread-self`; no threads, statics, atomics, logging or
  `unsafe` (rustix, already in the lock, for `statfs` and the socket).
- ergo-sbe: generated mappings between SBE types and application types.
  `with_enum_mapping(EnumMapping::new("Side", "app::Side").rename("Buy",
  "Bid"))` generates `TryFromSbe` / `TryToSbe` as exhaustive `match`es, so an
  application enum missing a schema variant, or with one the schema lacks,
  does not compile. `with_composite_mapping(CompositeMapping::new("Leg",
  "app::Leg"))` does the same member by member with a struct literal:
  constant members are skipped, optional members are `Option` (`None` writes
  the null value, `Some` of the null value is refused), and a member whose
  enum or composite has its own mapping uses it. Each mapping also registers
  the domain type, so accessors and DTOs take the application type, and a
  constant field of a mapped enum gets the domain getter beside its `*_wire`
  getter (decoder, entry decoder, memoized decoder, ordered fixed view), as a
  constant field under any `with_domain_type` or `with_conversion` now does.
  Both use the new `sbe_rt::MappingError`, which converts into
  `EncodeError` (`Conversion`), so `?` on a mapped setter works in a `send`
  closure: `NullVal` decodes as `NullEnum`, never a
  default, and so does an undeclared discriminant, which the decoder already
  folds into `NullVal` (under `with_null_as_option`, both are `None`).
  Mismatches the schema reveals (unknown type, member or variant, two mapped
  to one) are `GenerateError::InvalidConfiguration`, and so is an enum
  mapping without `with_null_as_option` when a `presence="optional"` field
  uses the enum: there `NullVal` means absent, and every valid frame without
  the field would otherwise fail to decode. The last rename given for a
  variant or member wins, whether it names it by its schema or generated
  name.
- ergo-sbe: send helpers that claim, encode, check and commit one frame.
  Every `sbe_rt` gains a `Claim` trait (implement it for your transport's
  claim; ergo-sbe has no Aeron dependency), `OfferClaim` for transports that
  copy (Aeron `offer`), `send_frame`, and `SendError`. Every encoder gains
  `send(claim, |enc| …)`, whose closure must return the terminal stage, and a
  fixed-only one also `send_fixed(claim, &fixed)`. A frame that does not fill
  its claim exactly (`LengthMismatch`) or does not start with the message's
  header (`HeaderMismatch`) is not committed, and the claim is aborted, as it
  is on an encoder error or a panic. The variable-length recipe no longer
  relies on `debug_assert`. A message whose own field, or first group or
  var-data, is named `send` or `send_fixed` keeps that accessor exactly as
  before (nothing is renamed) and has no helper of that name (both would be
  methods on the encoder); it sends through `sbe_rt::send_frame`. No encoder
  writes a block's padding (the bytes past its last field), so it goes out
  as the claimed region held it: an Aeron `try_claim` region starts zeroed,
  and `OfferClaim::new` zeroes its buffer, so a scratch buffer reused across
  frames never puts the previous frame's bytes on the wire; the try-claim
  path is unchanged.
- ergo-sbe: `with_source_traits(true)` generates a `{Message}Source` trait per
  message and an `…EntrySource` per repeating group: getters an application
  type implements (fixed fields, `impl ExactSizeIterator` per group, `&[u8]`
  per var-data). `encode_from(&src)` writes it in wire order through the
  encoder's own stages and returns the terminal stage, so it composes with
  `send`; `encoded_length_with_header_from(&src)` sizes it. No allocation;
  absent optional fields write their null value; a miscounted group is an
  error, and so is `Some` of an optional field's null value (NaN for a float
  without another `nullValue`), which would read back as `None`:
  `EncodeError::SomeNullValue`, as the composite mapping refuses it. A
  refused domain conversion is `EncodeError::Conversion`, with the field's
  path and the index of its entry in each group on the way
  (`fills[1].allocs[0].px`, `sbe_rt::FieldPath`) and the conversion's own
  error (`sbe_rt::ConversionError`: a `MappingError` or `&'static str`
  kept whole, any other type named). A field, or first group or var-data,
  named `encode_from` or `encoded_length_with_header_from` keeps its
  accessor exactly as without source traits, and that message has no helper
  of that name.
- `ergon-io`: a TCP quick-ACK route check on every connection
  a `Driver` takes (`connect`, `connect_background`, `add`) and every
  libcurl connection of `HttpClient` (libcurl 7.80's pre-request callback).
  Once per connection it asks the kernel over rtnetlink for the routing-table
  entry the connection uses (`ip route get fibmatch <peer> from <local>`,
  with the socket's interface, mark, TOS or traffic class, and ports) and
  reads `quickack` and `rto_min` from its metrics: never on the read or
  write path, and once per libcurl connection whichever handle uses it
  (`route::lookups` counts the lookups). `Config::quickack_check`
  (`quickack_check=` in the configuration string): `warn`, the default,
  warns once per route without quickack with the exact
  `ip route change ... quickack 1` that fixes it (none for routes kept from
  router advertisements, which the network configuration must set);
  `require` fails such a connection with `Error::QuickAck`; `off` skips it.
  Routes to this host are exempt; off Linux the check reports unsupported
  and fails only under `require`. What it found is on the connection
  (`WsConn::route`, `HttpConn::route`, `AnyConn::route`,
  `HttpClient::route_checks`). The check is kept for the mechanism (a peer
  that uses Nagle's algorithm waits for our delayed ACK, tcp(7)); the
  benchmarks did not measure what quickack is worth (the AWS matrix's 180 to
  245 µs from p99 up came from a run whose sender idled in C6).
- `ergon-io`: under `quickack_check=warn` the route lookup of
  `Driver::connect` and `Driver::add` runs on ergon-io's warning thread, not
  the polling thread, which only reads the connection's addresses and
  queues it; the connection opens at once and its `RouteCheck` reads
  `Pending` until the lookup is made (`warnings::flush` waits for it,
  `route::lookups_here` counts a thread's lookups). Under `require`,
  `Driver::connect` (and `connect_background`, on its helper thread) check
  the route as soon as TCP is connected, before the TLS handshake, so a
  refused route costs no handshake; `DriverStats::route_lookups`,
  `route_check_ns` and `route_check_max_ns` count the polling thread's
  share.
- `ergon-io`: a complete WebSocket client.
  - Close handshake both ways: a server Close is answered with its code;
    `WsConn::close(code, reason)` waits up to `close_timeout_ms` for the
    server's. `on_closed` gets `Error::WsClose` (code, reason, who started).
  - `WsOptions`: Pings after `ping_interval_ms` with nothing received (10 s;
    a busy connection sends none), `pong_timeout_ms` (5 s), `idle_timeout_ms`
    (off), `max_message`, `validate_utf8` (off), `protocols`
    (subprotocols). Timers run on the driver's deadline check, never on the
    read or send path.
  - Handshake checks `Upgrade` and `Connection`; a refusal is
    `Error::Upgrade { status, body, .. }`. Messages larger than one TLS record
    are sent over several; `send_text`, `send_binary`, `ping`.
  - A complete data frame inside a fragmented message now closes the
    connection (RFC 6455 section 5.4); it used to be delivered as a message.
  - `Reconnect` (optional): backoff with jitter (a fixed `seed` repeats it),
    `max_attempts`, a reset after `stable_ms`; the app resubscribes on
    `on_open`, vetoes with `stop`. Every backend.
  - `io/examples/websocket.rs`: a complete client (connect, subscribe,
    print, reconnect and subscribe again, clean close).
    `io/tests/example_websocket.rs` runs it against a local server on every
    backend.
- `ergon-io`: TCP keepalive on by default on every connection (`Config`:
  `keepalive_idle_s` 10, `keepalive_interval_s` 5, `keepalive_count` 3,
  `tcp_user_timeout_ms` 25,000), including `Driver::add` and `HttpClient`'s
  libcurl connections (set as libcurl opens each socket; the client refuses
  settings `Config::validate` refuses).
- `ergon-io`: the driver reads the counter for its deadline check every
  64th poll on backends that never sleep (every poll before): an idle
  `uring-spin` poll was 6.6 ns and would have been 21 ns with the
  WebSocket timers.
- `ergon-io`: `ergon-tune`'s checks, wired in (`ergon_io::checks`).
  `Driver::new` checks the polling loop's CPU (`Config::loop_cpu`,
  `loop_cpu=`, else the CPU the creating thread is pinned to): isolation
  and `nohz_full` (T1, with `sqpoll_cpu` for `uring-sqpoll`), busy SMT
  siblings (T8), the thread's scheduling policy (T7) and transparent
  hugepages as information (T3), and whether the kernel grants the
  configured `SO_RCVBUF` (one probe socket); an unpinned loop gets one
  information line instead of the CPU checks (`Driver::host_report`).
  ergon-io's warning thread checks each connection's egress device: its
  interrupts off the loop and SQPOLL CPUs (T2), ENA adaptive RX moderation
  (N1), deferred interrupts for interrupt-driven backends (N2) and an Azure
  VF (N3), again after each reset of the device (its `carrier_changes`,
  read once a second while a device is watched, moved and then held still
  for two reads, so a link-event re-pin can act first); one information
  line when the device is not visible (a pod's veth), which is then not
  watched. It also reports a NIC-polling backend whose socket has no NAPI
  id after the handshake. `Driver::add` sets `TCP_NODELAY` on a TCP socket
  and reports `TCP_CORK`. Each line is printed once per process, the
  device findings again after each reset. The warning thread starts outside
  the housekeeping CPUs when the process's cpuset excludes them (a pod with
  isolated CPUs only), saying so; if it is not running at all, the work
  queued to it is skipped, never done on the polling thread.
- `ergon-io`: what became of a failed request (`ergon_io::outcome`).
  - `ClientHandler::on_error(request, sent, error)`: `Sent::No` when the
    request's TLS record never wholly reached the kernel (the server cannot
    have seen it), else `Sent::Maybe` (it may have run). `HttpConn` users get
    `Error::Unanswered` (the cause, and the first request not handed) when a
    connection closes with requests in flight. Send-path cost: one add per
    `send`, one store per request.
  - `Verdict` (done, rejected, not run, throttled, banned, clock skew,
    unknown) and `outcome::generic` (status and `Retry-After` only);
    `retry_after_ms` (seconds, or an HTTP date in all three RFC 9110 formats,
    measured against the response's `Date`), `header_u64`,
    `ms_until_epoch`. A malformed header is an error, never a default.
  - `Error::Upgrade` keeps the refusal's `Retry-After` (`retry_after_ms`,
    `retry_after_invalid`).
  - `HttpClient::signed_request_at`: the caller's timestamp.
  - libcurl: a timeout is `Error::Timeout("http request")`, as on the native
    client; a response whose headers cannot be read (more than 64) is an
    error, no longer an empty header list. POST, PUT, PATCH and any method
    but GET, HEAD, OPTIONS and DELETE are refused (`Error::CurlResend`)
    unless `HttpClientConfig::curl_allow_non_idempotent_methods` is set:
    libcurl sends a request again, up to 5 times, when the reused connection
    it went out on dies before any reply byte (a test shows it).
- `ergon-io`: connect discipline for venue limits.
  - `Reconnect::poll_gated` (and `poll_at_gated`): each attempt, the first
    after `add` included, starts only when the caller's gate passes it
    (typically one `Budget` bucket per IP). A refusal is not a failed
    attempt; the link asks again after the gate's wait, at most `max_ms`
    later. Each pass starts after the last link the gate passed, so none
    starves.
  - `Reconnect::defer` (and `defer_at`): no attempt for a link before a
    time, kept across later closes and `resume`.
  - `outcome::refused_upgrade`: a refused WebSocket handshake on the
    connect bucket, 429 held for `Retry-After` (or the backoff), 418
    halted until a person clears it (`Budget::clear_halt`). An unreadable
    `Retry-After` is an error, after the hold.
  - `HttpClient`: a dropped pool connection reopens after a random delay of
    up to 100 ms, the limit doubling per failed connect up to 5 s (it was
    at once, then 100 ms doubling); `HttpClientConfig::reconnect_seed`.
    `HttpClientConfig::connect_gate` and `HttpClient::poll_gated`: every
    pool connect through the caller's gate, the first ones included (`new`
    opens nothing; `poll` refuses with `Error::Config`). Native only.
  - `WsStats::auto_frames`: frames the connection sent by itself (Pongs,
    Pings on a quiet connection, the echo of a server Close), to charge to
    a per-connection message limit.
- `ergon-io`: a send budget for venue rate limits (`ergon_io::limit`).
  `Budget::try_charge` checks up to 8 buckets (rolling window, draining
  counter or gate), all or none, and never sleeps or queues; a reserve
  for cancels; `observe_used` and `set_limit` from the venue's headers;
  `throttled` holds a bucket, `banned` halts it until a person calls
  `clear_halt`; `try_resend` sends again only inside a request's deadline.
  No allocation after `Budget::new` (`limit_bench`). The repository's
  `io/examples/venues.rs` adapts Binance, Bybit and OKX (limits from
  Binance's `exchangeInfo`), tested against responses written from their
  docs; the book's new "Rate Limits and Errors" page has the venue table,
  the error policy, the rules and the cost.
- `ergon-md-dpdk`: `ergon-tune`'s checks for the media driver
  (`ergon_md_dpdk::checks`), run by `ConfiguredDriver::from_env` (so the
  bundled `media_driver` too) after the agents' pins and the transport,
  before start, one stderr line each: the network agents busy-spin on pinned
  CPUs of their own, off the conductor's (M1), non-sparse term buffers (M2),
  for every driver; the socket buffers granted as asked, by a probe socket
  (M3); kernel busy polling for the socket transport (M4; information on
  Graviton); T1, T8, T7, T3, T4 (its own directory) and T5 on the agents'
  CPUs; T2 and T6 on the data NICs, with N1, N2 and N4 for the socket
  transport, whose data NICs `RUSTERON_MEDIA_DRIVER_DATA_NICS` names (one
  information line without it). md's re-check thread (`md-host-checks`,
  Linux, off every spinning agent's core) re-runs N1 and T2 once such a
  NIC's `carrier_changes` moved and settled. `RUSTERON_MEDIA_DRIVER_HOST_CHECK`
  is the one switch: `warn` (default), `off` (one information line saying
  so) or `require`, the strict opt-in, which refuses to start on a warning
  (`DpdkError::HostCheck`); any other value refuses to start, naming it. The DPDK install now also refuses a
  network agent on the conductor's CPU (`DEDICATED`, `SHARED_NETWORK`) and,
  under `DEDICATED`, the EAL's main lcore on the sender's or receiver's core.
  `ergon-tune`'s `CheckId` gains `M1`–`M4`.
- The book has a part for each of `ergon-io`, `ergon-clock`, `ergon-tune` and
  `ergon-md-dpdk` (when to use it, best practices, configuration, and code
  `{{#include}}`d from each crate's own tests and binaries), and a Deployment
  part: AWS and Azure hosts on x86 and Graviton, Kubernetes (the EKS
  manifests included as they are), and the key measurements per platform,
  each with its run, date and `BENCHMARKS.md` section. The book is now titled
  "ergon book".
- `ergon-md-dpdk`'s README and book chapter open with a notice: a proof of
  concept, not recommended for production; Aeron Premium recommended for
  kernel bypass with Aeron; no gain over the open-source C driver on
  localhost, with the data.
- The `ergon-md-dpdk` and Deployment chapters and `md-dpdk/README.md` are
  rewritten short and plain: what kernel bypass costs, which transport to use
  when, the cross-host results per platform (Azure D v6, AWS `c7i`, AWS
  `c9g`), and x86 against Graviton in latency and cost per hour.
  `md-dpdk/BENCHMARKS.md` gains Graviton rounds G2 (`aws-20261016-135004`)
  and G3 (`aws-20261016-223251`), and round 7's 500 k and 1 M msgs/s points.
- `scripts/check-book-content.sh` fails a book `{{#include}}` whose file or
  anchor is missing (mdbook renders it as an empty block and exits 0), with a
  fixture test in `scripts/tests/test-book-content.sh`.
- The book's "Java for Archive and Cluster" page (`book/src/deploy/java.md`):
  which Java to run and build on, the recommended JVM options and Aeron
  latency properties with the reason and source of each, pinning the hot
  threads, small hosts, and options from older guides that Java 25 refuses.
  Its two files, `cluster-test-harness/jvm/aeron.jvm` (a java argument file)
  and `cluster-test-harness/jvm/low-latency.properties`, are tested by
  `cluster-test-harness/tests/jvm_settings.rs`: the harness's JDK takes every
  option without a warning, every property is named in Aeron's main sources
  (Aeron ignores a misspelt one), and a cluster node started with both serves
  a client.

### Changed
- The workspace toolchain is Rust 1.99.0.
- `ergon-clock`: `now_ns` is UTC nanoseconds by default (`Domain::Realtime`;
  `Domain::Monotonic` for since boot, which `ergon-md-dpdk` keeps). The
  owner calls `Clock::update` from its loop's idle time to keep it on
  `CLOCK_REALTIME` (±500 ppm, never back); it returns a `Health`
  (`Health::warning`, `Warning::line`) and never prints. The front API is
  `calibrate`, `Config`, `Clock`, `Follower`, `Health`, `Warning`,
  `Report`, `Domain`, `TimeSource`, `elapsed_ns`, `ManualClock` and
  `Error`; everything else is `ergon_clock::advanced` (the `Copy` clock is
  `advanced::Clock`, with a silent `advanced::calibrate`). `calibrate`
  prints one line to stderr per problem: a fallback, a host clock not
  synchronised or not settled, a loose UTC bound, a pending leap second
  (`ERGON_HOST_CHECKS=off` or `0` silences them). The book's clock chapter
  is rewritten short, with a glossary, ordered reads and references;
  "Choosing a Clock" is folded into it.
- `ergon-clock`, `ergon-tune` and `ergon-io` each have a README that says
  what the crate is for, when to use it, its guarantees and limits, and how
  to use it, with compiled examples (`io/README.md` is new). Each README is
  its crate's rustdoc front page (`#![doc = include_str!("../README.md")]`),
  so its examples run as doctests, and CI builds the three crates' docs with
  `-D warnings`.
- `ergon-io`'s README, module docs and book chapter are rewritten short:
  bullets and tables. The book's "WebSocket, HTTP and Signing" page is now
  two pages, "WebSocket" (the complete example, options, close, reconnect)
  and "HTTP client" (native against libcurl, an example of each, signing,
  results). Benchmarks give p50 / p99 / max with run ids; the AWS tables in
  `io/benches/BENCHMARK.md` gain their maxima.
- `ergon-io` is published (it was `publish = false`): crates.io metadata,
  docs.rs with `webpki-roots`, and a package of `src/` and the README only
  (no tests, example or benches). The release publishes it after
  `ergon-tune`, in the one multi-package `cargo publish` of `just release`
  and the release workflow, and `just release-check` dry-runs it in the
  same invocation as `ergon-clock`, `ergon-tune` and `ergon-md-dpdk`. It
  is in `api/public-api-baseline.toml` (first release 0.1.30).
  Install: `cargo add ergon-io --features webpki-roots`.
- The release publishes `ergon-clock` and `ergon-tune` before
  `ergon-md-dpdk` (one multi-package `cargo publish`, in `just release` and
  the release workflow, which did not publish `ergon-md-dpdk` before), and
  `just release-check` dry-runs the three together, so `ergon-md-dpdk`
  verifies against the packaged two: alone, its dry run could not resolve
  them. `scripts/check-publish-set.sh`, in `just policy`, `just preflight`
  and `release-check`, fails when a published crate depends on an
  unpublished one, on a path without a version or on an older version than
  a published sibling's own (a version bump that missed the line), or when
  the release recipe, the release workflow, the dry runs or
  `api/public-api-baseline.toml` leave a published crate out or publish it
  before its dependencies. `scripts/check-public-api.sh` runs
  cargo-semver-checks on every crate in that manifest's `packages` (now
  with `ergon-clock`, `ergon-tune` and `ergon-md-dpdk`;
  `ergo-aeron-cluster` still only with `CLUSTER_SEMVER_CHECKS=1`); a crate whose
  first release (`first_release`) is newer than the baseline is compared
  once the baseline reaches it, and `--plan` prints what it compares.
- Breaking (generated API): a domain DTO's `encode` reports a value its
  domain conversion refused as `EncodeError::Conversion` (the field's path
  with its entry indices, and the conversion's own error) instead of
  `DomainConversionFailed`, which no encoder returns any more, and refuses
  `Some` of an optional field's null value (NaN for a float without another
  `nullValue`) with `EncodeError::SomeNullValue`: it wrote the null value,
  which read back as `None`.
- `just build`, `just preflight` and CI run `scripts/check-aeron-version.sh`,
  which fails unless the Aeron each rusteron crate bundles (in `Cargo.lock`
  and in the samples' own lock files), the submodule gitlink (pinned in
  `scripts/aeron-pin.txt`) and, when the submodule is checked out, its HEAD,
  its version and the version in each built jar's name are one release, and
  it also fails when the checkout has local changes to tracked files.
  That pin and check replace `cluster-test-harness/test-jars.sha256`, which
  still named 1.52.2, `just hash-aeron-jars` / `just check-aeron-jars` and
  the harness's `jar::sha256` / `jar::try_sha256`:
  the jars' byte hashes cannot hold, because the pinned commit built with
  JDK 17.0.20.1 and with 17.0.6 gives different jars (30 of the 316 class
  files in `aeron-cluster` differ) and CI installs whichever Temurin 17 is
  current.
- Aeron Archive and Cluster run on Java 25 (Temurin 25), the newest LTS in
  Aeron 1.53.3's own CI matrix (17, 21, 25); Java 27, the newest release, is
  neither LTS nor in that matrix. CI's cluster integration job and the
  release workflow install Temurin 25, which also builds the jars (Aeron's
  and SBE's Gradle 9.7.1 run on JDK 17 to 26; the classes stay Java 17
  bytecode). The Aeron 1.46.8 compatibility job stays on 17: that release's
  Gradle 8.10 predates Java 25. The cluster harness runs
  `$JAVA_HOME/bin/java`, else `java` on `PATH`, checks its version before
  the first launch (older than 17, or a `java` that will not run, fails with
  the binary's path and the fix: `jvm::try_java_major`), and starts every
  Java process with `cluster-test-harness/jvm/test.jvm`, Aeron's own test
  options: both `--add-opens` Aeron needs (`jdk.internal.misc`, and
  `java.util.zip` for archive checksums), a 256 MiB fixed heap, Parallel GC,
  Agrona's bounds and alignment checks on, IPv4 sockets.
  `TestCluster::single_node_with_jvm_args` starts a node with options of the
  caller's.
- `ClusterLauncher` is compiled with `javac --release 17` from `$JAVA_HOME`
  (else `PATH`): compiled by JDK 27 without it, it ran only on 27 or newer.

### Fixed
- ergo-sbe: a domain DTO's `encode` writes `NullVal` for a `None`
  domain-typed enum field (`sinceVersion` or `with_null_as_option`), at
  message level and in group entries, instead of leaving whatever the buffer
  held, which a reused buffer turned into the previous frame's value.

## [0.1.30] — 2026-10-13

### Added
- Generate an opt-in cross-schema `AnySchemaMessage` dispatcher
  (`Generator::generate_schema_dispatch`) with typed errors and borrowed
  frames, including unknown schemas and templates.
- `SessionBuilder::aeron`, `client_name` and `error_handler`.
- `AeronCluster::offer_vectors` gathers a session header plus up to seven
  payload slices with no heap allocation.
- `IngressSessionDecorator` prepends a session header on a raw publication.
  `ClusterEvent` is the logged cluster event. Egress listeners gain
  `on_message_with_reserved_value` and `on_extension_message`.
- Report the cluster client's ingress limits, so a client can refuse to start
  when its largest message would not fit:
  `AeronCluster::ingress_max_message_length()`, `ingress_max_payload_length()`
  and `AeronCluster::SESSION_HEADER_LENGTH`.
- Report the egress image a cluster session is bound to:
  `AeronCluster::egress_image_correlation_id()` and `egress_image_session_id()`.
  As in Java, the session binds to the image that delivered its
  `SessionEvent(OK)` or latest `NewLeaderEvent`.
- Read a fixed text array (any `char` array, or a `uint8` array whose
  `characterEncoding` is ASCII or UTF-8) as text: `<field>_as_str()` returns
  `Result<&str, DecodeError>` with trailing NUL padding removed, at message
  and group-entry level. Group-entry encoders gain the `<field>_str(&str)`
  setter message encoders already had. Both use the new public
  `sbe_rt::encode_fixed_text` and `sbe_rt::decode_fixed_text`.

### Changed
- Breaking (generated API): drop `try_` from the per-field and closure
  methods.
  - A domain-typed field's getter and setter take the plain field name
    (`price()?`, `price(v)?`); the raw accessor stays `price_wire`. A field
    named after a reserved method takes the same `_field` rename its raw
    accessor would.
  - The decoder's boolean getter is `{field}_bool()` again, reversing
    0.1.24's `try_{field}_bool()`.
  - The `Ordered` lane's `try_fixed`, `try_<group>`, `try_<data>` and
    `try_<data>_as_str` twins are removed; their plain forms return
    `DecodeError`, so map a custom error into it inside the closure.
  - The raw stages' `try_<data>(f)` and `try_<data>_as_message(f)`
    callbacks are removed; use `into_<data>()` / `into_<data>_as_message()`,
    or `.ordered()`.
- Breaking (cluster API): `SessionBuilder` is no longer `Send` or `Sync`,
  because it can hold a shared Aeron client (`SessionBuilder::aeron`), and
  `EgressEvent::SessionEvent` gains `leader_heartbeat_timeout_ns`, so code
  that builds or destructures that variant must name the field.
- Depend on rusteron 0.2.11 from crates.io, which bundles Aeron 1.53.3.
- Pin the `aeron` submodule and the vendored cluster session schema to 1.53.3.
  Session schema id 111 is SBE version 17. `ServiceTerminationPosition` gains
  `ackRequired`.
- Check ergo-sbe's wire bytes against sbe-tool 1.40.2, up from a pre-1.40.0
  snapshot, and add its `issue1116`, `issue1118`,
  `floating-point-infinity-schema` and five more test schemas to the parity
  suite.
- Reject a float or double `nullValue`, `minValue`, `maxValue` or constant
  spelled `inf`, `infinity` or `nan` in any case other than `Infinity` and
  `NaN` (with an optional sign), as sbe-tool does. A malformed float constant
  now fails at parse time, not in the generated module.
- Reject `presence="constant"` on a set or composite field, and a constant
  enum field given by `constantValue` rather than `valueRef`, as sbe-tool
  does for the enum form.
- Stop blocking in the cluster client's polls. `AsyncClusterConnect::poll`
  and `poll_egress` add publications and subscriptions with Aeron's async add
  and pick them up on later polls; each add used to wait up to 5 s. Only the
  first `poll`, which creates the Aeron client, can still wait.
- Follow a new cluster leader as Java's client does: after a `NewLeaderEvent`
  the session stays `AwaitingNewLeader` until the new leader's ingress
  publication has connected, within `message_timeout`. The `Disconnected`
  reason names what the session was waiting for.
- Detect cluster leader loss from the session's own egress image:
  `AeronCluster::is_egress_connected()` is false once that image closes, even
  while a replacement image is open.
- Return `PayloadTooLarge` from `AeronCluster::try_claim` for a claim over the
  ingress publication's max payload length. `connect` and `finish` fail when
  the publication's limits cannot be read.
- Align the cluster client's connect, failover and egress handling with the
  Java client: open an ingress publication to every member when ingress
  endpoints are set, send protocol version 0.3.0, accept a `SessionEvent` or
  `Challenge` only for the request in flight, and wait for a new leader on
  `NOT_CONNECTED` and `CLOSED`. `MAX_POSITION_EXCEEDED` closes the session.
  An unset new-leader timeout is twice the leader heartbeat, or 20 s. UDP
  egress subscriptions set `rejoin=false`, and an egress channel of
  `endpoint=localhost:0` is resolved to the bound port before connecting.
  `poll_egress` no longer sends keep-alives.
  `EgressEvent::SessionEvent` carries `leader_heartbeat_timeout_ns`.

### Fixed
- Leave constant fields out of a group entry's `blockLength`. Frames ergo-sbe
  encoded for schemas with a constant ending an entry change width to match
  sbe-tool.
- Return a constant enum in a group entry from its `valueRef`, as message
  level does; its wire-reading entry accessors and setters are removed.
- Count a group entry's DTO `encoded_length()` from the stride the encoder
  writes, including a group's declared `blockLength` padding.
- Take an enum's null from its `encodingType` typedef's `nullValue` when the
  enum declares none, as sbe-tool does. `NullVal`, the `From` conversions and
  an optional enum's `None` now write that value (254 for `nullValue="254"`),
  where they wrote the primitive default.
- Generate float and double constants written `Infinity`, `-Infinity`,
  `+Infinity` or `NaN`; they did not compile. A negative constant such as
  `-1.5` is no longer mistaken for an `Enum.value` reference.
- Decode an optional float or double whose `nullValue` is not NaN as `None`
  when it holds that value, at every level.
- Let fixed-text decoder helpers yield to schema field names at every depth.
- Emit generated doc comments one `#[doc]` line at a time. A multi-paragraph
  doc was rendered as a `/** … */` block whose indented prose rustdoc ran as a
  failing doctest when the generated module was checked into `src/`.
- Stop the cluster crate enabling ergo-sbe's default `fancy` feature, which
  pulled miette's graphical renderer into the cluster build.
- Walk `verify` at the dimension header's block length and the frame's acting
  version, so an extended group block is accepted and a short nested block is
  rejected.
- Reject a nested group block shorter than its fields when reached through a
  warm parent entry; `bulk_decode_into` could panic or read past the buffer.
- Rescale a `rust_decimal::Decimal` to a decimal composite's constant
  exponent instead of passing its mantissa through. A value that cannot be
  represented exactly is an error, never rounded.
- Honour a `UTCTimestamp` field's `timeUnit` in the built-in
  `chrono::DateTime<Utc>` conversion, at every level and in domain DTOs. A
  value more precise than the field holds is an error.
- Reject an invalid `valueRef` at parse time, as sbe-tool does.
- Reject an included file whose root is `<message>`; it was dropped silently.
- Keep the ingress channel's settings when the cluster client follows a
  REDIRECT or a new leader; only a unicast endpoint is replaced.
- Apply a cluster `SessionEvent(CLOSED)` that arrives with or after a
  `NewLeaderEvent`; it was dropped.
- Refuse cluster channel URIs over Aeron's 4096-byte limit; Aeron's printer
  wrote past a fixed 512- or 256-byte buffer.

### Removed
- Moved the runtime, its server, the wire and bridge crates, the log printer
  and the ClickHouse lab to a private repository.

## [0.1.29] — 2026-09-19

### Fixed
- **`acting_version()` / `acting_block_length()` still collided wherever a
  type owned tails, so those schemas did not compile.** The 0.1.28 fix covered
  the group-entry decoder only. The ordered lane emitted both names
  unconditionally on every wrapper, and the message decoder and memoized
  wrapper did the same, so a group or var-data named `actingVersion` /
  `actingBlockLength` (at message or entry level), or an entry field of that
  name beside entry tails, failed with E0592 or E0015. Every decoder, stage and
  ordered wrapper now emits them through one helper that yields to a schema
  name on that type. The header values stay on `get_metadata()`.
- **`with_external_sbe_rt` against a runtime owner without groups or var-data
  did not compile.** 0.1.27 and 0.1.28 sized `EntryInfo`, `Ordered` and
  `OrderedFixed` to the emitting schema, but a runtime owner cannot see the
  schemas of the modules that share it. A generated `sbe_rt` is a whole runtime
  again, so one shared runtime means one set of types:
  `consumer::sbe_rt::EntryInfo` *is* the owner's, as it was before 0.1.27.
- **`<field>_as_str` yields to a sibling tail of that name**, not only to a
  fixed field, through the same helper as `<group>_count` / `<field>_len`.
- Group-entry `acting_version()` / `acting_block_length()` yield when a field
  already owns that name, matching the count/len rule. A schema with
  `actingVersion` / `actingBlockLength` on an entry compiles; the field
  accessor keeps its name.
- Ordered `fixed` / `try_fixed` callbacks yield independently. A first tail
  named `tryFixed` keeps `fixed()`. A first tail named `fixed` also owns
  `try_fixed()` (the try-visit), so both callbacks yield and callers read
  fixed fields before `ordered()`.
- Memoized **and group-entry** var-data getters re-check the schema maximum
  even when `encoded_length()` / the iterator has already published a
  bounds-valid end. Over-max payloads are `InvalidVarDataLength` on the base
  lane, a cold getter, and a warmed one.
- Domain `None` for a `with_all_enums_as_option()` (or `with_null_as_option`)
  enum writes `NullVal` into a reused buffer, including nested-entry DTOs.
  Flat-group `bulk_add_domain` and `to_wire_entry` honour `Option<Enum>` the
  same way. A versioned required enum `None` writes `NullVal` rather than
  skipping the write.
- `*_as_str` / `*_as_str_unchecked` yield per name: a sibling `noteAsStr`
  does not drop `note_as_str_unchecked`.
- The gated `ergo-sbe_ordered` Car decode arm walks recursively with ordered
  callbacks at every tail and observes the same fields as the staged
  consuming arm. `fairness_policy_test` rejects staged `.into_*` anywhere in
  that arm.

## [0.1.28] — 2026-09-15

### Added
- **The ordered lane recurses.** `ordered()` is now generated on any decoder
  that owns tails, not only the message decoder: a group entry carrying its own
  groups or var-data has one too, so a walk keeps a single spelling all the way
  down instead of switching to the staged API at the entry boundary. A nested
  `ordered()` walk is the completion the parent visit owes. Entries with no
  tails of their own have nothing to order and get no lane.
- The staged `into_*` iterator is unchanged and still generated beside it. For a
  fixed-stride nested group it remains the better tool — an `ExactSizeIterator`
  you can `break` out of, still reaching the next tail by arithmetic. The lane
  adds a spelling rather than replacing one.

### Changed
- Nested `ordered()` walks return the parent visit's completion directly
  (`sbe_rt::IntoEntryComplete`); `.done()` is only to unwrap the staged
  complete. The message chain ends on `encoded_length_with_header()` /
  `as_bytes_with_header()`, like encode.
- The ordered lane is `sbe_rt::Ordered<S>` wrapping each staged stage, instead
  of a parallel `*DecoderOrdered*` type per tail. Message-level `fixed` takes a
  `{Name}DecoderFixedView` (fixed fields only) and consumes into
  `Ordered<OrderedFixed<Decoder>>`. Entry-level wrappers no longer emit `fixed`:
  the parent callback already holds the entry. Unprefixed group/var-data methods
  return `DecodeError` so closures infer; `try_*` keeps a custom `E`.
- `EntryInfo` for dynamic groups is filled from the attached group decoder the
  staged walk already opened. The discarded `__sbe_*_dim` wrap is gone.

### Fixed
- A group entry with a field literally named `ordered` no longer collides with
  the new entry-level lane. Group entries are not renamed against the static
  reserved list, unlike message and memoized decoders, so such a field already
  owns `ordered()`; the lane yields and is simply not generated for that entry,
  matching how `<group>_count` / `<field>_len` yield to a colliding sibling.
  A converted field named `ordered` (`ordered_wire` / `ordered_as`) no longer
  suppresses the lane. Covered by `ordered_lane_test`.
- A first tail named `fixed` no longer emits duplicate `fixed()` methods: the
  message-level callback yields, and the group/var-data visit keeps the name.
  Last tail named `done` is a different type from `done()` on the complete
  stage. Covered by `ordered_lane_test`.
- The entry-level lane's `group(&mut self, F)` / `try_group` no longer declared
  a default on their `R` type parameter. Function generic parameter defaults
  are not valid Rust (`invalid_type_param_default`, deny-by-default); every
  group whose entries carry their own tails failed to compile. `R` is always
  inferred from the callback's return expression, so the default was dropped
  rather than replaced.

## [0.1.27] — 2026-09-12

### Added
- **Ordered decode lane — `decoder.ordered()`.** One callback per tail, in
  wire order, with the same spelling at every *message-level* tail (nested
  groups inside an entry keep the staged spelling): `fixed(|&Decoder|)`, then
  `group(|entry, EntryInfo|)` for each group, then `field(|&[u8]|)` (or
  `field_as_str(|&str|)` where the schema declares a text encoding). `done()`
  returns the staged complete stage, so extent and full-frame helpers stay
  reachable. It is a façade over the staged stages and owns no cursor, so the
  entry traversal and compile-time tail ordering come from there rather than
  being re-implemented; entries carrying their own tails still return their
  completion from the callback, which is what keeps the walk one-pass. It is
  free over the staged lane for fixed-stride groups, and costs one extra
  dimension-header read per *dynamic* group per message — never per entry, and
  never a scan — because a visit closure exposes no handle to ask for the
  count. The staged lane stays the floor for all-dynamic messages.
- `sbe_rt::EntryInfo { index, count, block_length }`, handed to every ordered
  entry callback, with `is_first()`, `is_last()` and `remaining()`. Both
  `count` and `block_length` come from the group's dimension header, so neither
  costs a scan. A group's total *byte* length is deliberately absent: for
  entries with tails it is only settled by traversing them.
- **Fixed-stride groups decode as real iterators.** A group whose entries carry
  no tails of their own has a known stride, so `into_<group>()` hands back an
  iterator: `for entry in &mut iter`. It implements `Iterator` only for
  `&mut Iter`, never for `Iter`, so `for entry in iter` does not compile and a
  loop cannot silently drop the rest of the message. Also `ExactSizeIterator`
  and `FusedIterator`. The iterator does not have to be drained — `finish()`,
  or the next tail's `into_*` / `skip_*` called straight on it, skips the rest
  by arithmetic.
- Non-advancing `<group>_count()` and `<field>_len()` accessors, generated at
  every decode location: the base decoder, `.memoized()`, group entries, and
  each staged stage that precedes the tail. Both return `Result<usize>` and
  yield `0` when the tail is absent at the acting version, so sizing a `Vec` or
  logging a count no longer costs a stage.
- `iter.remaining_entries()` and `iter.entry_block_length()` on the staged
  fixed-stride iterator. The ordered lane fills `EntryInfo` from these for
  fixed-stride groups, so that shape costs nothing over the staged walk.

### Changed
- **`ordered` is a reserved decoder method name again.** 0.1.26 removed the
  mutable ordered lane and, with it, the rename that a schema field named
  `ordered` used to take; the new callback lane puts `ordered()` back on the
  base decoder, so such a field is once more generated as `ordered_field()` —
  the same rule that has always applied to `memoized`. The *mutable* lane
  (`{Name}OrderedDecoder`, runtime `OutOfOrder` checks) stays deleted; the new
  lane enforces order through types, not at runtime.
- The `ergo-sbe_ordered` benchmark arm now exercises the ordered lane. It and
  `ergo-sbe_consuming` were both measuring the staged API, so the two labels
  reported the same thing; `fairness_policy_test` now asserts each arm uses its
  own lane.
- Groups whose entries carry their own groups or var-data keep the visit
  closure, and now that is a deliberate split rather than an accident of
  history. Such entries have no stride, and `Iterator::next` cannot learn where
  an entry ended from the entry it already handed away, so an iterator would
  have to measure every entry before yielding it and the caller's own walk
  would traverse it again. The closure returns the entry's completion, and that
  *is* the next cursor. One pass, both shapes.
- `absent_tail_length` is now the single predicate for "this tail is not on the
  wire at this version", shared by all four locations that emit a count or
  length accessor; the memoized lane no longer carries its own copy.
- `sbe_rt::EntryInfo` is emitted only for schemas that declare at least one
  group (and unconditionally in shared-module mode, where one module's runtime
  serves its siblings). It is reachable only through the ordered lane's group
  callbacks, so a group-less schema was carrying it as dead code.
- `extended_optional_enum_nullify` now decodes the optional composite's counter
  alongside the two enums, on both arms. The enum-only form was not gateable:
  both codecs compile those two-byte loads to the same shape, so the true ratio
  is ~1.00 and the measured one was decided by code placement rather than codec
  work — a 2.6% change in unrelated generated code in the benchmarks crate moved
  ergon's arm 37% while the decoder's generated source stayed byte-identical.
  Reading the composite adds equal logical work to both arms and leaves the
  ratio near 0.78, with enough margin that placement drift cannot flip it.

### Fixed
- **`<group>_count` / `<field>_len` could collide with a sibling accessor and
  produce a module that did not compile.** A schema declaring a group `orders`
  beside a field `ordersCount` emitted `orders_count()` twice (E0592), at
  message, memoized and group-entry level alike. Sibling *tails* collided the
  same way: a group `fills` beside a group `fillsCount`, or a var-data `blob`
  beside a var-data `blobLen`. These names are derived from the schema, so they
  cannot live in the static reserved list that drives field renaming. The
  existing accessor now wins and keeps its name — renaming it would change an
  accessor that worked before these convenience methods existed — and the
  convenience accessor is not generated for that tail. Consuming stages carry
  no fields or sibling tails, so they still get it. Covered by
  `reserved_name_clash_test::tail_accessors_yield_to_colliding_field_names`,
  which compiles a schema containing all three collision shapes.
- **`finish()` / `skip_*` on a fixed-stride group stepped over each unread
  entry.** Every entry occupies the acting block length, so the group's end is
  `count * block_length`; the loop was doing a bounds check per entry for a
  result that one multiply gives. Groups whose entries carry tails still walk,
  because without a stride there is nothing to compute.
- The fixed-stride group decoder gained the `size_hint` that its
  `ExactSizeIterator` implementation already implied; it was returning the
  default `(0, None)`.
- The codegen matrix's iterator-surface probe bound its message walk to `_`,
  so a decoding failure in the walk could not fail the test. It now propagates
  with `?`; verified by injecting a truncated buffer, which the bound-to-`_`
  form passed and the fixed form fails.
- `EntryInfo::remaining()` underflowed for a zero-`count` value. `EntryInfo` is
  a public struct, so an independently constructed one could panic in debug
  builds; it now saturates. Its three queries also carry `#[must_use]`, since
  discarding a pure position query is always a bug.

## [0.1.26] — 2026-09-07

### Added
- Var-data declared with `characterEncoding="UTF-8"` or `"ASCII"` now gets a
  `*_as_str(&str)` encode-side setter, at both message and group-entry level,
  alongside the existing `*_as_str` decode-side accessors — writes through
  the checked byte setter after validating ASCII where the schema requires it.

### Changed
- Sequential decode is the staged `into_*(|entry|)` / `skip_*` chain — the
  encoder dual, compile-time order, no `&mut`.

### Fixed
- `characterEncoding` spelling variants legal under the SBE spec —
  `US-ASCII` and `UTF8` (no hyphen), used by real schemas including this
  repo's own Aeron cluster codecs — were silently dropped by message-level
  decode, the memoized decode lane, and group-entry decode, though the
  staged-consuming lane already recognised them. Message-level decode,
  group-entry decode, the memoized lane, and the new encoder now classify
  `characterEncoding` through one shared function so they can't drift again.

### Removed
- The mutable ordered decoder lane: `{Name}Decoder::ordered()`,
  `{Name}OrderedDecoder`, `AnyMessage::into_<name>_ordered()`, and
  `DecodeError::OutOfOrder`. Sequential decode is the staged
  `into_*(|entry|)` / `skip_*` chain; random-access and `.memoized()` remain.

## [0.1.25] — 2026-09-06

### Added
- **Four decoder lanes, generated for every message with groups or var-data:**
  random access (`try_decode`, any order, `Sync`), staged (`into_*(|entry|)` /
  `skip_*`, compile-time order), mutable ordered (`.ordered()`, one
  cursor, runtime order checks), and memoized (`.ordered()`'s sibling —
  `decoder.memoized()` consumes the base decoder and returns a progressive
  `Cell`-backed tail-boundary cache, so repeated or out-of-order tail reads
  walk each boundary at most once). A message with no groups or var-data gets
  only the base decoder — there is no tail to memoize or order, so
  `.memoized()` / `.ordered()` are not generated and `AnyMessage` exposes only
  `into_<name>()` for it.
- `AnyMessage::into_<name>_memoized()` / `into_<name>_ordered()` land directly
  in a lane without an intermediate base-decoder conversion.
- Var-data declared with `characterEncoding="UTF-8"` or `"ASCII"` now gets
  `*_as_str` / `*_as_str_unchecked` text accessors on **group entries**, not
  only at message level — the same helper that already existed for message
  var-data, shared rather than duplicated.
- `GenerationConfig::with_encode_version(version)`: the encoder writes
  `version` in the header and omits members introduced after it, while the
  decoder still reads every version in the schema. Useful for producing older
  wire versions for peers that have not upgraded. It is rejected at generate
  time when it would drop members alongside domain objects or conversions.
  Covered by a versioned-L3 fixture (v0–v3) that is byte-compared against
  sbe-tool at every acting version.
- Cluster Callgrind instruction probes (`just bench-cluster-instructions`) —
  mechanism-level evidence for cluster session codec hot paths, alongside the
  existing SBE probe lane.

### Fixed
- **`<data characterEncoding="...">` was silently dropped by the schema
  parser.** SBE allows `characterEncoding` directly on `<data>`, not only on
  the referenced composite's `varData` member; real schemas use the element
  form. The parser only read the composite-member form, so a declared-text
  field silently downgraded to raw `&[u8]` with no `*_as_str` accessor at all
  — a wire-compatible but API-degrading bug with no error to signal it.
- **A truncated or undersized fixed-stride group could be read out of bounds
  through the mutable ordered decoder.** The fixed-stride branch of
  `.ordered()`'s group entry validated `count * blockLength` but never
  `blockLength` against the fixed fields' minimum readable extent — a
  `blockLength` smaller than a required field's offset let a safe getter read
  past its entry. Both the truncation and the undersized-stride case are now
  rejected before any entry reaches a caller callback, matching the
  random-access lane's existing checks.

### Removed
- `GenerationConfig::with_memoized_tail_offsets` and
  `with_compact_tail_offsets` — memoization is now a runtime lane
  (`decoder.memoized()`) rather than a generation-time, module-wide knob, so
  the choice moves to each call site instead of being fixed for the whole
  module. Compact `u32` tail-offset storage was measured and dropped: it saved
  a little decoder memory but cost more instructions on the cache's hot
  primitives and was materially slower under LTO.

## [0.1.24] — 2026-08-31

### Fixed
- **Domain objects + domain types generate compilable code again.** Fifteen
  codegen defects made `with_domain_type` / `with_manual_domain_type` unusable
  alongside `with_domain_objects` for most field shapes. Every one produced a
  generated module that failed to compile, so no wire format changed and no
  working code can have depended on the old behaviour.

  Reported against 0.1.22 and 0.1.23, then found while root-causing it:
  - A `sinceVersion > 0` **composite** wrapped `try_*` in `Some(..)`. `try_*`
    on a versioned field already returns `Result<Option<T>, E>`, so the DTO
    built `Option<Option<T>>` for an `Option<T>` field.
  - The same double-wrap on a `sinceVersion > 0` **primitive scalar**.
  - A **fixed array** matched by a selector generated converters against the
    element type (`u8`) instead of the array type (`[u8; 4]`).
  - An **array or optional scalar** with a conversion read the raw flyweight
    getter under its pre-rename name, though a conversion renames it `*_wire`.
  - An **array or optional scalar** DTO kept the wire type but wrote through
    the domain `try_*` setter, which takes the domain type.

  Found by the combination matrix below:
  - A **versioned array** was treated as `Option`-returning, though an array
    accessor returns a zero-filled array, never `None`.
  - A **constant** field matched by a selector generated conversion accessors
    against a `*_wire` setter that constants do not have.
  - `Display` for a **group-entry composite** with a domain type re-derived the
    conversion and fed `Option<T>` to `TryFromSbe<T>`; it now goes through the
    generated `try_*` accessor.
  - `Display` for a **converted primitive** read the raw getter under its
    pre-rename name, at message and group-entry level.
  - Conversion-only `*_as` accessors did not `Option`-unwrap a versioned or
    optional raw getter, at message and group-entry level.
  - `with_all_enums_as_option` makes enum accessors return `Option<T>`, which
    neither the `*_bool` accessors nor the domain DTO accounted for.
  - An **optional bool enum** produced a group-entry accessor that matched
    `Option` against a plain `BooleanType`.
  - An **optional enum with a domain type** was treated as `Option`-returning;
    an optional enum carries `NullVal` as a variant, so its accessor is plain.
  - `null_as_option` **combined with an enum domain type** left `try_*`
    disagreeing with the accessor it delegates to, in both directions.
  - Group-entry and message-level boolean accessors disagreed on when they
    return `Option`; both now do so only when version-gated.

  Most shared one root cause: the same decision — "is the domain type applied
  to this field?", "is this accessor `Option`-returning?" — was made
  independently in several places and drifted apart. Those now live in single
  predicates (`conversion_helpers::dto_domain_type`,
  `converter_impls::is_optional_domain_field`).
- `ConversionSelector::FieldPath` selects fields again. It is documented as the
  primary selector form and validation accepted it, but codegen matched only
  `NamedType` and `SemanticType`, so every `FieldPath` selector was a silent
  no-op. Group and nested-group fields extend the path with their group names
  (`"Message.group.field"`).
- Documented selector precedence is enforced: `FieldPath` beats `SemanticType`
  beats `NamedType`. Resolution was "first selector in registration order", so
  precedence silently depended on call order.
- Domain types configured for **enum** and **set** fields reach the DTO.
  `try_*` accessors were generated for them all along, but the domain-DTO
  generator ignored them and emitted the raw generated type. Boolean enums are
  unchanged — they still materialise as `bool`.
- Scratch-crate test helpers no longer force `--offline` / `CARGO_NET_OFFLINE`,
  and they strip that flag when inherited from `cargo llvm-cov`. Nested
  compile-and-run crates can download extra transitives (`bit-set`, `ahash`)
  that are not in the workspace lock — the cause of red `test`/`coverage`
  jobs on 0.1.19–0.1.22 and Dependabot PRs.
- The coverage job timeout is 120 minutes (was 30). Instrumented
  compile-and-run tests cannot finish in half an hour; the 0.1.23 coverage
  job was cancelled mid-run.
- Nightly `cargo fuzz` pins `x86_64-unknown-linux-gnu`. cargo-fuzz's Linux
  default is musl+ASAN, which fails on a minimal nightly without that target.
- Group-dimension IR extraction matches the required `blockLength` and
  `numInGroup` members by exact name. Padding fields such as `account`,
  `countPadding`, or `blockLengthPadding` no longer overwrite the layout.
- Generated group-entry `Display` formats a domain-mapped set through
  `try_*` and `{:?}`. The raw accessor is `*_wire`; calling `self.<field>()`
  failed to compile.
- `poll_connect_until_done` idles with work-count 0 while the handshake is
  still in progress. `AsyncClusterConnect::poll`'s `true` means more polling
  is needed, not that Aeron performed work; a positive count reset backoff
  and pinned a core.
- `parse_ingress_endpoints` rejects empty comma-separated entries (leading,
  trailing, or repeated commas) instead of skipping them.
- `cluster_decode_session_event` carries a documented no-LTO-only allowance of
  1.05 (LTO stays at 1.01, where ergon measures 0.99). Its no-LTO arm is a
  memory-bound decode that walks either side of parity — 1.0403 and 1.0444 on
  consecutive quiet-machine runs, matching the 1.0406 already recorded in the
  pair's history. This is an allowance for a placement-sensitive tie, not a
  proof that nothing regressed; above 1.05 the gate fails as before.
- `write_module_set` restore after a mid-commit failure replaces an
  already-promoted destination (Windows-safe) and surfaces restore errors
  rather than discarding them. Successful publication fails if a `.bak`
  backup cannot be removed.

### Added
- **Codegen combination matrix** (`sbe/tests/codegen_matrix_test.rs`). Every
  field shape x `sinceVersion` x mapped/unmapped cell, at all three codegen
  locations (message, group entry, nested group entry), crossed with every
  `GenerationConfig` combination that selects a distinct codegen path — and
  each case **compiles** the generated module rather than inspecting its
  source. Source-string assertions were what let the defects above ship: they
  stay green while the module fails to compile.

  The fixture is generated by `scripts/regenerate-codegen-matrix-fixture.py`,
  whose `SHAPES` list is the single source of truth; the test recomputes the
  same cross product and fails when the fixture falls behind, and `--check`
  (wired into `just preflight`, `just test`, and CI) fails on drift.

### Changed
- **Breaking (generated API):** the decoder's boolean getter is `try_{field}_bool`
  at every shape and location. Message-level code named the `Option`-returning
  one `{field}_bool` while group entries named both shapes `try_*`, so the same
  schema shape had two different accessor names depending on where it sat —
  which is what let the domain DTO call an accessor that did not exist.
  `{field}_bool` is now unambiguously the *encoder setter*; `try_` marks the
  fallible decode. The only remaining shape difference is the return type:
  `Result<bool, _>`, or `Result<Option<bool>, _>` when `sinceVersion > 0` makes
  the field absent on older wire versions.
- **Breaking (generated API):** conversion-only `{field}_as::<T>()` returns
  `Result<Option<T>, _>` for a `sinceVersion > 0` or optional field, matching
  the domain-typed `try_*` accessors. It previously generated a body that fed
  `Option<W>` to `TryFromSbe<W>`, so no compiling code can depend on the old
  signature.
- Hook `FieldInfo::rust_type` for groups and var-data is a real type
  (`LevelsDecoder`, `&[u8]`) instead of the sentinels `"group"` / `"data"`.
  `FieldInfo::kind()` returns `FieldKind` (`Fixed`, `Group`, or `Data`).
- Consumers are advised to build with `lto = true` / `codegen-units = 1`.
  Generated codecs are many small `#[inline]` methods whose inlining only fully
  collapses across codegen units; `sbe/BENCHMARKS.md` and the book's benchmark
  chapter now document the recommendation. Benchmark methodology no longer
  describes the LTO profile as informational — both profiles have been blocking
  gates with the same literal `1.00` ceiling.
- Hook `FieldInfo` carries a `kind: FieldKind` field recorded when the field
  is built. `kind()` returns it instead of inferring from `rust_type` suffix
  and substring checks, so renaming a generated type cannot silently change a
  field's meaning.
- `just check-local`, `just check-products`, and `just test` no longer run the
  `ergo-aeron-cluster` suite twice: the crate is excluded from the workspace
  run that already covers it explicitly, and its doctests run once via
  `cargo test -p ergo-aeron-cluster`. Same for CI's workspace doctest job.
- CI, `just test`, and the local product gates run
  `cargo test -p ergo-aeron-cluster` (integration tests, compile-fail API
  suite) rather than `--lib` only.
- Generated public-API freeze records trait associated-type bounds,
  defaults, generics, where-clauses, and semver attributes.

## [0.1.23] — 2026-08-28

### Fixed
- Generated `Display`/`Debug` bodies no longer assume a `with_domain_type`
  target implements `Display`. Domain values format with `{:?}`, so a domain
  type only has to be `Debug`.
- `with_display_debug(false)` now also omits the group entry decoder's
  `Display` impl, which was emitted unconditionally.

### Changed
- **Breaking:** optional var-data accessors (`into_*_as_compact_str`,
  `into_*_as_smol_str`, `into_*_as_bytes`) are emitted only when the
  *generator* was built with the matching `compact_str` / `smol_str` / `bytes`
  feature — reverting the 0.1.x switch to gating them on the consumer's own
  features. That gate was a `#[cfg(feature = "…")]` in the emitted source,
  which resolved against the consumer crate's feature table; consumer crates do
  not declare features by those names, so the accessors were unreachable dead
  code in every generated module. Enable the feature on `ergo-sbe` in both
  `[build-dependencies]` and `[dependencies]`.
- The generated public-API snapshots no longer record those accessors, since
  their presence now depends on the generator's feature set.

## [0.1.22] — 2026-08-27

### Added
- Generated observers named by T-1 (`after_this_message`, message/group
  `min_readable_fixed_extent`, `MessageHeader` peeks, `schema_id_from_header`,
  domain `to_wire_entry`) carry specific `#[must_use]` diagnostics.
- `StaticCredentials::new` accepts `impl Into<Vec<u8>>` (moved `Vec`, slice,
  array, embedded NUL, invalid UTF-8) with byte-identical connect and challenge
  credentials. `from_utf8` remains the text convenience.
- Poll-driven connect recipe in rustdoc, `cluster/README.md`, and the session
  builder book chapter: `SessionBuilder` → `connect_async` → `default_idle` →
  `poll_connect_until_done` → `finish`. `AsyncClusterConnect` is a poll-driven
  Aeron state machine, not a Rust `Future`.
- Group-dimension composites accept unsigned `u8`/`u16`/`u32`/`u64` members
  (both endians, reordered offsets, padded composites over 32 bytes). Parser
  rejects signed/optional/array/missing/overlapping/out-of-bounds members.
  Over-limit counts and non-representable 32-bit `u64` wire values are typed
  range errors.
- Unpublished workspace crate `ergo-aeron-cluster-test-harness` owns the Java
  ClusterLauncher, harness tests, and examples. Release automation unpacks
  `ergo-aeron-cluster` with every advertised feature and fails closed.

### Changed
- **Breaking:** `ergo-aeron-cluster` no longer advertises a `test-harness`
  feature or `test_support` module. Depend on `ergo-aeron-cluster-test-harness`
  in this repository.
- `AnyMessage::decode_frame` rejects every `frame_len` in `0..HEADER_LENGTH`
  before reading header fields, even when the backing slice contains a later
  valid header.

## [0.1.21] — 2026-08-24

### Added
- `GenerationConfig::with_manual_domain_type` and `with_domain_type` now emit
  correctly `Option`-wrapped `try_*` accessors for optional (or
  `sinceVersion`-gated) domain-typed fields inside repeating groups, matching
  the message-level behaviour. Generated `Display`/`Debug` for group-entry
  domain-typed `Primitive`/`Enum` fields now shows the converted domain value
  instead of failing to compile.

### Changed
- **Breaking:** `BuildError` is `#[non_exhaustive]`. `Io(std::io::Error)` is
  now `Io { action: &'static str, path: PathBuf, source: std::io::Error }`,
  naming the exact attempted step and destination instead of a bare I/O
  message.
- `generate_multi_to_dir` / `write_module_set` publish a complete generated
  module set as one all-or-nothing unit: every module is staged to a unique
  temp file before any destination is touched, and a mid-commit failure
  restores every pre-existing destination and leaves no temp/backup debris.
  A cleanup step that itself fails is reported in preference to the failure
  that triggered it, so a rollback never reports itself clean while leaving
  debris. Deleting the backups of a *successful* commit is past the point of
  no return and does not roll back: every backup that can be removed is
  removed, then the publication fails naming the first that could not.
- `GenerationConfig::with_keyword_append_token` is validated at `generate()`:
  an empty or invalid token now returns
  `GenerateError::InvalidConfiguration` instead of generating uncompilable
  Rust.
- **Breaking:** `SessionBuilder::ingress_endpoints` returns
  `Result<Self, ClusterError>` and validates the endpoint map (grammar,
  duplicate IDs, empty entries) at the call site instead of at `validate()`
  or the first `poll()`.
- **Breaking:** `poller::parse_event` returns `Result<EgressEvent, ClusterError>`
  (was `Result<Option<EgressEvent>, ClusterError>` — every decoded branch
  was already `Some`, so `None` was unreachable).
  `poller::parse_leader_endpoint` returns `Result<Option<String>, ClusterError>`
  (was `Option<String>`) — a malformed endpoint map is now `Err` with the
  parser's specific reason, distinct from `Ok(None)` for a well-formed map
  that simply lacks the requested leader.
- Message-bearing `#[must_use]` on pure cluster decision observers:
  `PublicationFailure::is_retryable`/`raw_code`, `ClusterError::is_retryable`,
  `AeronCluster::{cluster_session_id, leadership_term_id, leader_member_id,
  is_ingress_connected, is_ingress_closed, ingress_position,
  is_egress_connected, state}`, `ClusterClaim::position`, and
  `AsyncClusterConnect::{step, is_complete}`.
- **Breaking:** `PublicationFailure` is `#[non_exhaustive]` and gains
  `TooManyParts`, mapped directly from `AeronOfferError::TooManyParts`
  instead of the fabricated sentinel `Other(-100)`. `raw()` is replaced by
  `raw_code() -> Option<i64>` (`None` for `TooManyParts`, which has no real
  Aeron wire code).

- The generated public-API freeze gate (`api/generated/*.txt`) now snapshots
  the canonical semver-relevant surface — struct fields, enum variant
  payloads/discriminants, full fn/method signatures (receiver, generics,
  args, return type, where-clause), associated types/consts, type aliases,
  and cfg/non_exhaustive/repr/deprecated/must_use attributes — instead of
  item names only. A cfg-gated item is now recorded (with its condition)
  instead of being silently excluded from the freeze.
- The `SbeMessage` sealing-trait path is now explicit per-schema state
  (`GenerationContext`, passed by reference into decoder/encoder generation)
  instead of a `thread_local!`. A hook that invokes a nested `Generator`
  (e.g. to emit a companion crate) can no longer leak its sealed path into
  the outer generation's remaining messages.
- `sbe/BENCHMARKS.md` and `book/src/sbe/benchmarks.md`'s "Group encode: LTO
  on and off" section no longer quotes unprovenanced point estimates
  (`414.1 ns`, etc., with no run-id/commit/host and duplicated verbatim
  across both files) as if they were current — it states the qualitative
  ranking and the reproduction command instead.
- Generated domain-object `encoded_length()` / `encoded_length_with_header()`
  rustdoc no longer contradicts `encode()`: `encoded_length()` is documented
  as body-only, and `encoded_length_with_header()` as the exact buffer size
  and return value of `encode()` for both fixed and dynamic messages.
- Generated `{Group}Encoder::wrap`, `ENTRY_BLOCK_LENGTH`, and
  `GROUP_DIM_TEMPLATE` now carry authored rustdoc explaining the standalone
  group framing contract (offset is the first entry, the dimension header is
  the caller's responsibility) instead of a placeholder.

### Fixed
- Domain-object generation for a required (non-`sinceVersion`) boolean field
  no longer panics on an unknown wire discriminant; `try_from_decoder`
  propagates `DecodeError::InvalidBoolean` instead.
- Single-line schema `description` text containing `&`, `<`, or `>` (e.g.
  `Option<u32>`) is now escaped before emission, so it can no longer break a
  downstream `-D warnings` rustdoc build with `rustdoc::invalid_html_tags`.

## [0.1.20] — 2026-08-20

### Added
- `GeneratedModuleSet::into_parts` consumes the set and returns owned modules
  and warnings without cloning generated source.
- `SchemaFile` plus `generate_multi_to_dir` / `generate_multi_to_out_dir`
  generate a shared schema and its consumers in one transaction, using
  supplied module names and watching every resolved include.
- `EncodeError::InvalidAscii { field }` for ASCII fixed-array `*_str` writes.
- Generated public-API snapshots under `api/generated/` (`car_lean`,
  `car_domain`, `multi_schema_shared`) and
  `scripts/check-generated-public-api.sh`, invoked from `just preflight`.
- `GenerationConfig::with_manual_domain_type` for additive Manual
  `TryFromSbe`/`TryToSbe` mappings. `with_domain_type(selector, path)` is
  the two-argument Generated path.

### Changed
- `GenerationConfig::with_domain_type` stays the two-argument
  `(selector, path)` Generated path it has always been. Use
  [`with_manual_domain_type`] for additive Manual impls.
- **Breaking:** `ParseError` is `#[non_exhaustive]`. Root file reads are
  `Io { path, source }`. Include failures are `Include { href, attempted,
  cause }` with typed `IncludeCause` (`Cycle` / `Io` / `NotFound`).
- **Breaking:** `SessionBuilder::ingress_channel`, `egress_channel`,
  `message_timeout`, and `new_leader_timeout` return `Result` and store only
  validated values. `is_ingress_exclusive` and `owns_aeron` setters are
  removed.
- **Breaking:** generated `*_str` exists only for default-ASCII `char` and
  encodings `ASCII` / `US-ASCII` / `UTF-8` / `UTF8`. Unencoded numeric arrays
  and encodings such as GB18030 keep the raw array setter only — schemas
  relying on the old setter for those types need the raw accessor instead.
- Optional var-data accessors (`into_*_as_compact_str`, `into_*_as_smol_str`,
  `into_*_as_bytes`) are always emitted and gated on the *consumer's*
  `compact_str` / `smol_str` / `bytes` feature, not the generator's. Fixes a
  feature leak where the generator's own feature flags decided what a
  consumer crate could see, independent of that crate's own Cargo features.
- Generated rustdoc snippets referencing a `DomainImpl::Manual` field render
  as plain text instead of an ignored `rust,ignore` fence.
- Side-effect-free generated observers (`as_option` / `as_bool`, completed
  tail views/lengths, encoder metadata, group `written`, exact-length
  terminals) carry message-bearing `#[must_use]`.

### Fixed
- One-byte fixed-array getters (`char` / `uint8`) return the bulk-read
  `[u8; N]` instead of reconstructing each element with `from_le_bytes`.
- `parse_with_xsd_validation` accepts schema-declared enum `nullValue`
  (signed and unsigned) and still rejects unknown non-namespaced enum
  attributes.
- `generate_to_dir` emits `cargo::rerun-if-changed` for the root schema and
  every nested or transitive include, so an include-only edit rebuilds.
- Unresolved-type diagnostics name the containing field as well as the
  invalid type.

### Deprecated
- `GenerationConfig::with_error_from_impls` — it converts generated encode/decode
  errors through `format!` and `From<String>`, dropping fields such as `needed`
  and `available`. Implement `From<generated::sbe_rt::EncodeError>` /
  `From<generated::sbe_rt::DecodeError>` instead. Removal is scheduled for 1.0.

## [0.1.19] — 2026-08-19

### Fixed
- A composite member with `presence="optional"` and a schema `nullValue`
  (e.g. a `PriceNull9`-style Decimal's `mantissa`) now decodes as `Option<T>`,
  checked against the wire null sentinel, instead of silently misreading the
  null image as a real value. `with_domain_type` mappings onto such a
  composite (e.g. `rust_decimal::Decimal`) now fail closed with a typed error
  on the null image rather than decoding the sentinel as a huge/wrong number.

## [0.1.18] — 2026-08-18

### Added
- Reject unknown attributes on `<messageSchema>`, `<message>`, `<field>`,
  `<group>`, and `<data>` at parse time, so an authoring typo (`presense`,
  `semanticTpye`) is an error instead of being silently ignored. Namespaced
  attributes (`xsi:*`, `xi:*`, and vendor namespaces such as Binance's
  `mbx:*`) are outside the SBE grammar and still pass.
- Multiple distinct decimal composites (`Decimal64`, `Decimal128`, …) can each
  map to `rust_decimal::Decimal` via `with_domain_type`; each gets its own
  `TryFromSbe`/`TryToSbe` impl.

### Fixed
- `validate_against_sbe_xsd` no longer rejects valid real-world schemas. It
  dropped every namespaced vendor attribute check (`mbx:exponent`) on the
  floor, and its allow-lists omitted `characterEncoding` on `<data>`, `unit`
  on `<type>`, `jsonValue` on `<validValue>` / `<choice>`, and `package` on
  `<types>` — enough to reject checked-in `l3-book` and `binance-spot`
  schemas. Attribute allow-lists are now shared with the parser so the two
  cannot drift apart.
- Domain-typed optional fields no longer produce a type mismatch. An optional
  `rust_decimal::Decimal` composite decodes to a plain `Decimal` (a composite
  has no null image), an optional `chrono::DateTime<Utc>` decodes to
  `Option<DateTime>`, and the encoder setters take the plain domain value.
- A boolean enum mapped to `bool` via `with_domain_type` now emits its
  `TryFromSbe`/`TryToSbe` impl once per shared module instead of in every
  consumer, which caused a "conflicting implementation" across multi-schema
  modules.

### Changed
- Fixed-only encoders now carry `FieldsState`: `as_bytes_with_header`,
  `as_body_bytes`, `encoded_length*`, and `into_remaining_mut` exist only
  after `fixed(&FixedFields)`, so a reused buffer cannot publish or pack
  unwritten required fields.
- Cluster bench gate uses the same literal `1.00` ceiling as SBE and requires
  `--run-id` provenance (`just bench-cluster` stamps per-estimate run ids).
- `check-public-api.sh` strips a leading `v` from `baseline_tag` before
  passing `--baseline-version` to cargo-semver-checks.

### Fixed
- Schema `nullValue` / `minValue` / `maxValue` that do not fit the declared
  primitive width are rejected at parse time (no more `256_u64 as u8`).
  Signed enum `validValue`s are compared as signed, so `int8` `-1` is
  accepted when `minValue="-5"` and `maxValue="5"`.
- Maintained cluster decode benches wrap both arms at the body offset without
  extra template/schema/version work on the sbe-tool side.
- Docs-validation temp crates `cargo fetch` before `CARGO_NET_OFFLINE=true`,
  so a clean CI runner is not missing optional transitive crates.

## [0.1.17] — 2026-08-14

### Changed
- **Breaking:** writing a message's tail now requires the fixed block first.
  `wrap*` returns `{Msg}UnfixedEncoder`; groups and var-data are reachable only
  after `.fixed(&{Msg}FixedFields { … })`. "Tails before the fixed block" is now
  unrepresentable rather than a runtime hazard.
- Gate the fixed-stride group proof APIs (`add_checked`, `start_entry`,
  `complete`, `EntryComplete`) on groups that actually have a fixed stride. A
  dynamic entry with a var-data tail can no longer claim completion.
- `ClusterError` is `#[non_exhaustive]`, so adding a variant is not a breaking
  change for downstream matches.
- Decode errors keep their UTF-8 cause in the error chain instead of flattening it.
- Malformed XML and schema-invalid SBE now surface as distinct errors.

### Added
- Schema `description` text is emitted as rustdoc on messages, fields,
  composite members, enum variants, group entries, and var-data.
- `#[must_use]` on pure generated observers — getters, set predicates, raw
  enum/set values, and metadata queries — so discarding them is a warning.
- Multi-schema generation validates that same-named shared types have identical
  wire fingerprints, and rejects duplicate or non-identifier module names.
- Fail-closed benchmark evidence packaging (`scripts/package-bench-artifacts.sh`)
  wired into both the release workflow and `just release`.
- sbe-tool parity benchmarks for optional-enum nullification and
  group-with-data, both gated at the strict `1.00` ceiling.

### Fixed
- Optional primitive fields write the schema null image when set to `None`,
  including fixed arrays and nested optional composite members.
- Cluster egress decoding runs through one canonical fail-closed path; short,
  truncated, and invalid-text frames report the real error instead of a
  misleading timeout.
- `parse_event` reports the real template id for decoded-but-unprojected frames
  instead of `0`.
- Session builder timeouts reject zero, sub-millisecond, and overflow values,
  and each timeout field tracks its own error so one cannot mask another.
- `StaticCredentials` and `SessionBuilder` redact credential material from
  `Debug` output.
- Duplicate member IDs in an ingress endpoint map are rejected.
- Generated multi-paragraph rustdoc is emitted one line per `#[doc]`, so it is
  no longer parsed as an indented Markdown code block and compiled as a doctest.

## [0.1.16] — 2026-08-10

### Changed
- Decoder structs use `offset` field (matching sbe-tool naming), replacing internal `base_addr` cache. `read_addr_unchecked` helper removed.
- Generated encoder cursors renamed `offset` for consistency across all decoder/encoder types.

### Added
- Historic ergo regression benchmarks — null-as-option, optional fields, Decimal converters. Gate: `just bench-historic`, `scripts/check-bench-historic.sh`.
- Multi-message/framing documentation: `AnyMessage` dispatch, back-to-back and `remaining()` framing approaches.
- miette error diagnostics page with rendered examples of schema parse errors.

### Fixed
- toml 1.x `FromStr` compat: `toml::Value::from_str` replaced with `toml::from_str` for full-document parsing.
- Matrix test crate missing feature flags (`compact_str`, `smol_str`, `bytes`).
- Book: removed incorrect "Java-style" label from parent-hopping sections.

## [0.1.15] — 2026-08-09

### Added
- **`with_null_as_option(ConversionSelector::named_type("EventCode"))`** — enum fields become `Option<EventCode>`. `NullVal` wire byte → `None`.
- **`with_all_enums_as_option()`** — every enum in the schema → `Option<Enum>`.
- **`as_option()` on every generated enum** — `event_code.as_option()` → `Option<EventCode>`.
- **`nullValue` on `<enum>` elements** now parsed from XML. `<enum encodingType="uint8" nullValue="99">` → `NullVal = 99`.

### Changed
- Crate description: `"Simple Binary Encoding (SBE)..."` for crates.io visibility.
- Group-entry enum fields support `with_null_as_option`.

## [0.1.14] — 2026-08-08

### Breaking
- `SessionState::AwaitingNewLeaderConnection` removed (merged into `AwaitingNewLeader`).
- `ClusterError` now `#[non_exhaustive]`, gains `InvalidTimeout` and `PayloadTooLarge`.
- `AsyncClusterConnect::step()` returns `ConnectStep` (non-exhaustive, wildcard required).
- `GenerationConfig::profile(Lean)` no longer clears explicit conversions/domain-types. Use `.lean()` for a clean baseline.

### Added
- `#[must_use]` on `SessionBuilder`, `AsyncClusterConnect`, `ClusterClaim`.
- `ConnectStep` re-exported at crate root.
- `checked_deadline` helper — zero/overflow timeouts return `InvalidTimeout`.
- `PayloadTooLarge` error with `operation`, `requested`, `maximum` fields.
- Zero-alloc fragmented path via `offer_parts`.
- Feature-gated integrations: `compact_str`, `smol_str`, `bytes`, `chrono`.
- `DomainVarData::CompactStrings`, `SmolStrings`, `BytesCrate` variants.
- `lean()` constructor on `GenerationConfig`.
- Module name validation + path containment.
- `#[must_use]` on codec tail stages.
- FIX SBE conformance suite documented.

### Fixed
- Malformed egress frames (< 8 bytes) → `ProtocolError`, not silently unknown.
- Decode errors propagated on poll path.
- `checked_add` overflow in `offer()`.
- Stale channel parse errors cleared on re-set.
- Benchmark docs match gate: SBE zero tolerance, cluster 0.005.

## [0.1.13] — 2026-08-07

### Breaking
- `SbeMessage` is a sealed trait — only generated types can implement it.
- Staged `EncodedLength` types renamed for stage-just-completed convention.
- Group decoders no longer expose `wrap_with_parent`.
- Safe constructors prove fixed extent and panic if short; `unsafe *_unchecked` skips.
- `From<BooleanType> for bool` removed — use `as_bool()`.
- `EncodeError::BufferTooShort` carries `field`.
- `DomainConversionFailed` carries `reason`.

### Added
- `add_checked` — group entries proven complete at compile time.
- Group decoders poison themselves on malformed entry.
- `#{inline}` on `bulk_decode`, staged finish helpers, domain DTO methods.
- `Error::source` on `EncodeError::Decode` and `VerifyError::DecodeError`.
- `#[must_use]` on consuming stages and `EncodedLength` stages.

### Fixed
- Constant-presence fields no longer count toward readable fixed extent.
- Aeron `try_claim` recipe rewritten against real API.
- Bench batch-decode uses `wrap_unchecked` for equal work.
- Deduped reserved method-name lists.
