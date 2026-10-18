# UTC and Time Sync

`now_ns` is UTC by default. This page says how `update` keeps it on UTC,
how close it stays, and how to set up the host's time sync, which it
follows. It can be no closer to UTC than the host's own clock.

## How `update` keeps UTC

- **One owner**, the thread holding the `Clock`, calls `update`. Once a
  second it samples the wall clock (the tightest of up to 8 brackets — each
  bracket is an ordered read, a `CLOCK_REALTIME` read, an ordered read, and
  its width bounds the error) and re-anchors the clock to follow it.
- **Run the owner on a thread that never idles.** The slow updates below
  (c7i, 18.7 µs) came after deep idle and are not confirmed; on Graviton
  the slowest is the start update.
- **Rate cap:** ±500 ppm, NTP's and Linux's own limit.
- **Never backwards:** a clock behind the wall clock runs faster until it
  catches up; one ahead runs slower.
- **Bad samples:** a sample taken across an interrupt (too wide a bracket)
  is rejected, and the clock keeps its rate.
- **Followers:** the owner publishes each update through a seqlock (a
  lock-free publish; readers never wait). A follower's read costs what the
  owner's does, and can go back by about 1 ns at an update; the owner's
  never does.
- **The fallback** is kept the same way: `CLOCK_MONOTONIC` plus a
  disciplined offset, never the raw `CLOCK_REALTIME`.

### Measured

30 minutes per node, isolated CPU, LTO, chrony against the Amazon Time Sync
Service; runs as on [the overview](overview.md#measured). Residual: how far
the clock was from the host's wall clock, ns, as a follower thread saw it
100 times a second (brackets cut by an interrupt left out). Update: the
owner's whole once-a-second update, ns.

| host, node | residual p50 / p99 / max | update p50 / p99 / max |
|---|---|---|
| c7i.2xlarge a | 134 / 2,641 / 9,404 | 3,478 / 4,460 / 18,737 |
| c7i.2xlarge b | 133 / 692 / 3,821 | 3,480 / 4,426 / 6,064 |
| c8g.xlarge a | 4 / 523 / 2,144 | 334 / 572 / 2,894 |
| c8g.xlarge b | 3 / 303 / 1,465 | 329 / 435 / 2,895 |
| c9g.2xlarge a | 12 / 240 / 2,662 | 295 / 373 / 1,759 |
| c9g.2xlarge b | 7 / 360 / 2,662 | 278 / 365 / 1,840 |

- No sample was rejected, and no clock stepped after the start.
- **The largest residuals follow chrony's own frequency changes.** A
  change of `f` ppm holds the clock off by about `f` ns for one second
  (measured 0.8–1.0 of that), not an ergon-clock error. c7i's p50 of
  133–134 ns is its wide brackets (about 350 ns); the same hosts at the
  owner's own samples read 2–3 ns.
- **c7i's updates cost ten times Graviton's.** The bench's owner slept
  between updates, so each ran with cold caches, likely after deep idle
  (not confirmed; the platform is a vCPU). It now busy-waits, as a runtime
  thread does; the sleeping figures are what is measured. c7i's slowest
  (18.7 µs) is unattributed and is not the start update. On Graviton the
  slowest is the start (16 brackets, not 8).
- This is the distance to the host's wall clock. The wall clock's own
  distance to UTC is chrony's bound: 87–166 µs here.
- [Full data](https://github.com/mimran1980/ergon/blob/main/clock/BENCHMARKS.md#disciplined-clock-residual).

## How the host keeps the wall clock

`calibrate` reports it, as far as the process can see
(`report.time_sync`), and warns at start-up when it is not synchronised or
not settled ([warnings](overview.md#start-up-warnings)):

| verdict | meaning |
|---|---|
| `PtpGrade` | chronyd locked to a PTP hardware clock, or phc2sys with the kernel synchronised |
| `NtpDisciplined` | chronyd or ntpd to an NTP server, or systemd-timesyncd; with its error bound where known |
| `Unsynchronised` | nothing keeps it; no error bound |
| `Unknown` | not Linux, or the answer cannot be read (daemon in another namespace, `/proc` with `hidepid`) |

`advanced::TimeSyncPolicy::RequirePtp` fails calibration unless PTP-grade;
`Off` skips the probe. `cargo run -p ergon-clock --example timesync` prints
this host's verdict; `report.recheck_findings()` re-runs it from an
idle-timer callback.

## Set up time sync

- **AWS:** chrony on the Amazon Time Sync Service. Not systemd-timesyncd:
  it keeps no error bound, and its slews after boot reached 533 ppm in the
  [reference runs](https://github.com/mimran1980/ergon/blob/main/clock/BENCHMARKS.md#time-sync-in-these-runs).

  ```text
  server 169.254.169.123 prefer iburst minpoll 4 maxpoll 4
  makestep 0.0001 3
  ```

  For PTP-grade time: an instance type with a PTP hardware clock (EC2
  `PhcSupport`), the ENA PTP clock enabled (in-tree ENA from Linux 6.17:
  devlink `enable_phc`; Amazon's driver: `phc_enable=1`), then:

  ```text
  refclock PHC /dev/ptp_ena poll 0 delay 0.000010 prefer
  server 169.254.169.123 iburst minpoll 4 maxpoll 4
  makestep 0.0001 3
  ```

  The service smears leap seconds: remove any `leapseclist` or `leapsectz`
  line.
- **Azure:** chrony on the Hyper-V PTP clock:
  `refclock PHC /dev/ptp_hyperv poll 3 dpoll -2 offset 0`.
- **x86 and Graviton** set up the same way.

## Since-boot clocks

A `Domain::Monotonic` clock, such as the
[DPDK transport's](../md-dpdk/overview.md), converts its stamps to and from
wall-clock time with `advanced::WallOffset::measure(&clock)`: `CLOCK_REALTIME`
less the clock, with its error bound. It drifts with the hardware's rate:
remeasure it from an idle-timer callback.
