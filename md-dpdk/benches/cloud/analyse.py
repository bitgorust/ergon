#!/usr/bin/env python3
"""Tables for md-dpdk/BENCHMARKS.md from lab.sh's results.csv, in rusteron's form:
each cell is the median across reps, the range across reps in brackets.

results.csv rows (lab.sh prefixes the rep):
  <rep>,rtt,<mode>,udp,p50,p90,p99,p99.9,p99.99,max,mean,samples   (ns)
  <rep>,tput,<mode>,udp,<median msgs/s>,<min>,<max>
  <rep>,rxhw,<mode>,<mean ns>,<max ns>,<frames>   (one per host; NIC RX timestamps only)
  <rep>,tputmax,<mode>,<size>,<median msgs/s>,<min>,<max>   (the load sweep's one-way maximum)
  <rep>,load,<mode>,udp-<fraction|burst>,<size>,<rate>,<burst>,p50,...,max,mean,samples,sent,received,
        backpressured,achieved msgs/s,max send lag ns
  <rep>,mix,<mode>,udp-<profile>/<stream>,<size>,<rate>,<burst>,... (as load: one row per stream of a mix)
  <rep>,loss,<mode>[:<size>:<fraction>|:mix:<profile>],<host>,<key>=<growth>;...

Next to results.csv, spikes-*.csv (stream,due_ns,latency_ns: round trips of 100 µs
or more) are summarised: whether spikes recur at one phase of each second, the
signature of once-a-second work.

analyse.py <results.csv> [<run dir>]
"""
import csv
import glob
import os
import re
import statistics
import sys
from collections import defaultdict

ORDER = ["kernel", "kernel-mono", "kernel-busy", "kernel-busy-mono", "dpdk", "dpdk-mono", "dpdk+idle",
         "dpdk-spin", "dpdk-spin+idle", "dpdk-adaptive+idle", "local-kernel", "local-dpdk",
         "local-dpdk-adaptive", "local-dpdk-spin"]
STATES = ["", "pinned", "isolated"]
NAMES = {
    "kernel": "kernel UDP",
    "kernel-busy": "kernel UDP, busy polling",
    "dpdk": "DPDK",
    "dpdk-spin": "DPDK, agents poll local sockets",
    "dpdk+idle": "DPDK, idle localhost subscription",
    "dpdk-spin+idle": "DPDK, idle localhost subscription, agents poll it",
    "local-kernel": "localhost, kernel UDP",
    # the build's default local-I/O: epoll up to sweep 2 (2026-10-14), adaptive since
    "local-dpdk": "localhost, DPDK driver",
    "local-dpdk-spin": "localhost, DPDK driver, agents poll",
    "local-dpdk-adaptive": "localhost, DPDK driver, adaptive local-I/O",
    "local-dpdk-epoll": "localhost, DPDK driver, epoll local-I/O",
    "dpdk-adaptive+idle": "DPDK, idle localhost subscription, adaptive local-I/O",
}
# a -mono mode: the transport and the clients clock with CLOCK_MONOTONIC, not TSC
NAMES.update({f"{m}-mono": f"{n}, monotonic clock" for m, n in list(NAMES.items())})
RTT_COLUMNS = ["p50", "p90", "p99", "p99.9", "p99.99", "max"]


def name(label):
    state, _, mode = label.rpartition(":")
    return f"{state}, {NAMES.get(mode, mode)}" if state else NAMES.get(mode, mode)


def cell(values, scale, digits):
    values = sorted(v / scale for v in values)
    return f"{statistics.median(values):.{digits}f} ({values[0]:.{digits}f}–{values[-1]:.{digits}f})"


def table(header, rows):
    widths = [max(len(r[i]) for r in [header] + rows) for i in range(len(header))]
    line = lambda r: "| " + " | ".join(c.ljust(w) for c, w in zip(r, widths)) + " |"
    return "\n".join([line(header), "| " + " | ".join("-" * w for w in widths) + " |"] + [line(r) for r in rows])


def main():
    rtt = defaultdict(list)
    tput = defaultdict(list)
    rxhw = defaultdict(list)
    tputmax = defaultdict(list)
    load = defaultdict(list)
    load_failed = defaultdict(int)
    mix = defaultdict(list)
    loss = defaultdict(lambda: defaultdict(int))
    with open(sys.argv[1], newline="") as f:
        for row in csv.reader(f):
            if len(row) < 5:
                continue
            kind, mode = row[1], row[2]
            if kind == "rtt" and len(row) >= 12:
                rtt[mode].append([float(v) for v in row[4:10]] + [float(row[11])])
            elif kind == "tput" and len(row) >= 7:
                tput[mode].append(float(row[4]))
            elif kind == "rxhw" and len(row) >= 6:
                rxhw[mode].append((float(row[3]), float(row[4])))
            elif kind == "tputmax" and len(row) >= 5:
                tputmax[(mode, int(row[3]))].append(float(row[4]))
            elif kind == "load" and len(row) >= 20:
                point = (mode, int(row[4]), row[3].removeprefix("udp-"))
                if int(row[14]) == 0:  # no echo after warm-up: a failed rep, no percentiles
                    load_failed[point] += 1
                    continue
                load[point].append([float(v) for v in row[7:13]]
                                   + [int(row[15]), int(row[16]), int(row[17]), int(row[5]), float(row[18]), int(row[19])])
            elif kind == "mix" and len(row) >= 20 and int(row[14]) > 0:
                profile, _, stream = row[3].removeprefix("udp-").partition("/")
                mix[(mode, profile, stream, int(row[4]), int(row[5]), int(row[6]))].append(
                    [float(v) for v in row[7:13]] + [int(row[15]), int(row[16]), int(row[17]), float(row[18])])
            elif kind == "loss" and len(row) >= 5:
                for kv in row[4].split(";"):
                    if "=" in kv:
                        k, v = kv.split("=", 1)
                        loss[mode][k] += int(v)
    # labels are <state>:<mode> (lab.sh), or a bare mode
    def key(label):
        state, _, mode = label.rpartition(":")
        return (STATES.index(state) if state in STATES else len(STATES), state,
                ORDER.index(mode) if mode in ORDER else len(ORDER), mode)

    modes = sorted(set(rtt) | set(tput), key=key)

    print("## UDP round trip (µs): between the two hosts, or both ends on one (localhost)\n")
    rows = []
    for m in modes:
        reps = rtt.get(m, [])
        if not reps:
            continue
        rows.append([name(m), str(len(reps))] + [cell([r[i] for r in reps], 1000, 2) for i in range(6)])
    print(table(["state, transport", "reps"] + RTT_COLUMNS, rows))
    samples = {m: [int(r[6]) for r in rtt[m]] for m in rtt}
    print("\nRound trips measured per rep: " + ", ".join(f"{name(m)} {min(s):,}–{max(s):,}" for m, s in samples.items()) + ".\n")

    print("## UDP throughput, 32-byte messages (M msgs/s)\n")
    rows = [[name(m), str(len(tput[m])), cell(tput[m], 1e6, 2)] for m in modes if tput.get(m)]
    print(table(["state, transport", "reps", "msgs/s"], rows))

    if tputmax:
        print("\n## Sustained one-way maximum by message size (M msgs/s)\n")
        rows = [[name(m), f"{size} B", str(len(v)), cell(v, 1e6, 2)] for (m, size), v in sorted(tputmax.items(), key=lambda x: (key(x[0][0]), x[0][1]))]
        print(table(["state, transport", "size", "reps", "msgs/s"], rows))

    if load:
        print("\n## Round trip under open-loop load (µs, from each message's intended send time)\n")
        print("`offered` is the scheduled rate; `achieved` the rate actually offered after warm-up. "
              "Below 98% of offered (marked *), the generator or the path could not keep the schedule, "
              "and the latencies include that queueing. `loss counters` lists every loss counter that grew "
              "on either host (`sat-` ones are saturation, not loss).\n")
        order = {"burst": 10**12}
        rows = []
        rank = lambda f: order.get(f, 0) or int(f.removeprefix("r"))
        points = sorted(set(load) | set(load_failed), key=lambda p: (key(p[0]), p[1], rank(p[2])))
        for (m, size, frac) in points:
            reps = load.get((m, size, frac), [])
            if not reps:
                rows.append([name(m), f"{size} B", "burst 1000" if frac == "burst" else (f"{int(frac[1:]):,}/s" if frac.startswith("r") else f"{frac}%"), "", "",
                             f"0 (+{load_failed[(m, size, frac)]} failed)"] + ["no echo"] * 6 + ["", "", ""])
                continue
            sent = sum(r[6] for r in reps)
            received = sum(r[7] for r in reps)
            lost = loss.get(f"{m}:{size}:{frac}", {})
            bad = {k: v for k, v in lost.items() if v}
            load_name = "burst 1000" if frac == "burst" else (f"{int(frac[1:]):,}/s" if frac.startswith("r") else f"{frac}%")
            offered = statistics.median(r[9] for r in reps)
            achieved = statistics.median(r[10] for r in reps)
            limited = "*" if achieved < 0.98 * offered else ""
            failed = load_failed.get((m, size, frac), 0)
            rows.append([name(m), f"{size} B", load_name, f"{offered:,.0f}", f"{achieved:,.0f}{limited}",
                         str(len(reps)) + (f" (+{failed} failed)" if failed else "")]
                        + [cell([r[i] for r in reps], 1000, 2) for i in range(6)]
                        + [f"{received:,}/{sent:,}", f"{sum(r[8] for r in reps):,}",
                           ", ".join(f"{k} {v:,}" for k, v in sorted(bad.items())) or "none"])
        print(table(["state, transport", "size", "load", "offered", "achieved", "reps"] + RTT_COLUMNS
                    + ["echoed", "back-pressured", "loss counters"], rows))

    if mix:
        print("\n## Mixed streams at once, open loop (µs, from each message's intended send time)\n")
        print("Every stream of a profile runs at the same time on one pair of channels, each on its own "
              "schedule; one row per stream. `achieved` below 98% of `offered` is marked *.\n")
        rows = []
        for (m, profile, stream, size, rate, burst), reps in sorted(mix.items(), key=lambda x: (key(x[0][0]), x[0][1], x[0][2])):
            achieved = statistics.median(r[9] for r in reps)
            lost = {k: v for k, v in loss.get(f"{m}:mix:{profile}", {}).items() if v}
            rows.append([name(m), profile, stream, f"{size} B", f"{rate:,}" + (f" ×{burst}" if burst > 1 else ""),
                         f"{achieved:,.0f}" + ("*" if achieved < 0.98 * rate else ""), str(len(reps))]
                        + [cell([r[i] for r in reps], 1000, 2) for i in range(6)]
                        + [f"{sum(r[7] for r in reps):,}/{sum(r[6] for r in reps):,}",
                           ", ".join(f"{k} {v:,}" for k, v in sorted(lost.items())) or "none"])
        print(table(["state, transport", "profile", "stream", "size", "offered/s", "achieved", "reps"] + RTT_COLUMNS
                    + ["echoed", "loss counters (profile)"], rows))

    spikes(os.path.dirname(os.path.abspath(sys.argv[1])))

    if loss:
        flagged = {m: {k: v for k, v in ks.items() if v} for m, ks in loss.items() if ":" not in m.split(":", 1)[-1]}
        flagged = {m: ks for m, ks in flagged.items() if ks}
        print("\n## Loss counters over the round-trip and throughput runs (both hosts, all reps; none = every one 0; "
              "`sat-` counters are saturation, not loss; localhost base runs are not sampled)\n")
        rows = [[name(m), ", ".join(f"{k} {v:,}" for k, v in sorted(flagged.get(m, {}).items())) or "none"]
                for m in modes if m in loss]
        print(table(["state, transport", "grew"], rows))

    if rxhw:
        print("\n## NIC arrival to transport dispatch, receiver port (µs)\n")
        rows = [[name(m), str(len(rxhw[m])), cell([r[0] for r in rxhw[m]], 1000, 2), cell([r[1] for r in rxhw[m]], 1000, 2)]
                for m in sorted(rxhw, key=key)]
        print(table(["state, transport", "host-runs", "mean", "max"], rows))


def spikes(folder):
    """Per run (reps pooled): round trips of 100 µs or more, and the share of them
    in the busiest 10 ms of the second (by intended send time). Spread at random
    that share is about 1%; work that runs once a second pushes it towards 100%."""
    groups = defaultdict(lambda: [0, [0] * 100, 0])
    for path in glob.glob(os.path.join(folder, "spikes-*.csv")):
        stem = os.path.basename(path)[len("spikes-"):-len(".csv")]
        # <state>-<mode>-rtt-<rep>, <state>-<mode>-mix-<profile>-<rep>, <state>-<mode>-<size>-<rep>-<point>
        m = re.match(r"^(.*-(?:rtt|mix-.*))-\d+$", stem) or re.match(r"^(.*-\d+)-\d+(-[^-]+)$", stem)
        run = "".join(m.groups()) if m else stem
        g = groups[run]
        g[2] += 1
        with open(path, newline="") as f:
            for row in csv.reader(f):
                if len(row) == 3 and row[1].isdigit():
                    g[0] += 1
                    g[1][int(row[1]) % 1_000_000_000 // 10_000_000] += 1
    if not groups:
        return
    print("\n## Round trips of 100 µs or more, by run (reps pooled)\n")
    rows = []
    for run, (count, phase, files) in sorted(groups.items()):
        share = f"{100 * max(phase) / count:.0f}%" if count >= 20 else "too few"
        rows.append([run, str(files), f"{count:,}", share])
    print(table(["run", "reps", "spikes", "in busiest 10 ms of the second"], rows))


if __name__ == "__main__":
    main()
