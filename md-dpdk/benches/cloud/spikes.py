#!/usr/bin/env python3
"""Where each latency spike, and each rep's max, came from, for one state of a lab run.

spikes.py <run dir>/<state> [<threshold us>]

Reads the spike files the clients wrote (spikes-*.csv: round trips of 100 us
or more, with their run's CLOCK_REALTIME start) and the diagnostics vm.sh
diag-fetch saved for each host (a-diag-<state>.log, b-diag-<state>.log: the gap
spinner on a spare CPU, a trace of the app and agent CPUs). A spike of at least
<threshold us> (default 200) overlapping:

- a spinner gap on either host is a host stall: the hypervisor paused the VM
  (the spinner's CPU is idle, so nothing in the guest can explain its gap);
- a traced preemption of a busy thread (an agent, ping or pong) is a guest
  preemption, named by what ran instead;
- a stall of the ping or pong client's own loop (stalls-ping/pong-*.csv) is
  a client stall;
- none of them is unexplained: the NIC, the fabric, or a stall of one vCPU
  the spinner does not share. For each round trip run the receive agents'
  poll gaps of 100 us or more (the jitter snapshots before and after) say
  whether an agent stalled: with none, an unexplained spike was in flight.
"""
import collections
import csv
import glob
import os
import re
import sys

BUSY = re.compile(r"^(sender|receiver|rtt|load|tput)")
EVENT = re.compile(r"\[(\d+)\].*?\s(\d+\.\d+): (\w+): (.*)")
SWITCH = re.compile(r"prev_comm=(\S+) prev_pid=\d+ prev_prio=\d+ prev_state=(\S+) ==> next_comm=(\S+)")


def read_diag(path):
    """(spinner gaps [(start_s, end_s)], preemptions [(start_s, end_s, cpu, who, by)]) on the wall clock."""
    gaps, lines, anchor, section = [], [], None, None
    for line in open(path, errors="replace"):
        line = line.rstrip("\n")
        if line.startswith("== "):
            section = line[3:]
            continue
        if section == "gapspin" and line and not line.startswith("#"):
            end, us = line.split()[:2]
            gaps.append((float(end) - float(us) / 1e6, float(end)))
        elif section == "trace":
            if line.startswith("UPTIME"):
                f = line.split()
                anchor = (float(f[1]), float(f[3]))
            else:
                lines.append(line)
    preempted, away, spinner_out = [], {}, []
    for line in lines if anchor else []:
        m = EVENT.search(line)
        if not m or m.group(3) != "sched_switch":
            continue
        s = SWITCH.search(m.group(4))
        if not s:
            continue
        cpu, ts = int(m.group(1)), anchor[1] - (anchor[0] - float(m.group(2)))
        prev, state, nxt = s.groups()
        # the spinner itself taken off its CPU by the guest: not a host stall
        if prev == "gapspin" and state.startswith("R"):
            away[("spin", cpu)] = (ts, prev, nxt)
        elif nxt == "gapspin" and ("spin", cpu) in away:
            spinner_out.append((away.pop(("spin", cpu))[0], ts))
        if BUSY.match(prev) and state.startswith("R"):
            away[cpu] = (ts, prev, nxt)
        elif BUSY.match(nxt) and cpu in away:
            t0, who, by = away.pop(cpu)
            preempted.append((t0, ts, cpu, who, by))
    # a spinner gap the trace explains (the guest ran something else there) is
    # not the host's
    gaps = [g for g in gaps if not any(o0 < g[1] and o1 > g[0] for o0, o1 in spinner_out)]
    return gaps, preempted


def main():
    folder = sys.argv[1]
    threshold = float(sys.argv[2]) if len(sys.argv) > 2 else 200.0
    state = os.path.basename(os.path.normpath(folder))
    run_dir = os.path.dirname(os.path.normpath(folder))
    diag = {}
    for host in "ab":
        for path in (os.path.join(run_dir, f"{host}-diag-{state}.log"), os.path.join(folder, f"{host}-diag-{state}.log")):
            if os.path.exists(path):
                diag[host] = read_diag(path)
                break
    if not diag:
        sys.exit(f"no {state} diagnostics (a-diag-{state}.log) next to {folder}")
    # intervals sorted by start, for a binary search: anything overlapping
    # [t0, t1] starts before t1 and, as none here lasts over a second, after
    # t0 - 1 s
    import bisect
    gap_idx = {h: sorted(g) for h, (g, _) in diag.items()}
    pre_idx = {h: sorted(p) for h, (_, p) in diag.items()}

    def overlapping(intervals, t0, t1):
        lo = bisect.bisect_left(intervals, (t0 - 1.0,))
        hi = bisect.bisect_left(intervals, (t1,))
        return [iv for iv in intervals[lo:hi] if iv[1] > t0]

    BASE = 50e-6  # an ordinary round trip: a cause must account for half of what a spike adds to it

    def read_stalls(path):
        """[(start_s, end_s)] of a client's loop stalls on the wall clock."""
        if not os.path.exists(path):
            return []
        with open(path) as f:
            first = f.readline()
            if not first.startswith("# start_realtime_ns="):
                return []
            base = int(first.split("=", 1)[1]) / 1e9
            out = []
            for line in f:
                parts = line.strip().split(",")
                if len(parts) == 2 and parts[0].isdigit():
                    end = base + int(parts[0]) / 1e9
                    out.append((end - int(parts[1]) / 1e9, end))
        return sorted(out)

    BUCKETS = ("poll-cycle-gaps-20-50us", "poll-cycle-gaps-50-100us", "poll-cycle-gaps-over-100us")

    def agent_gaps(run_tag):
        """The agents' loop gaps during a round trip run, per host: how many of
        20-50, 50-100 and over 100 us (both ports summed), from the jitter
        snapshots taken before and after it."""
        out = {}
        for host in "ab":
            snaps = []
            for when in ("before", "after"):
                path = os.path.join(run_dir, f"{host}-jitter-{when}-rtt-{run_tag}.log")
                if not os.path.exists(path):
                    path = os.path.join(folder, f"{host}-jitter-{when}-rtt-{run_tag}.log")
                if not os.path.exists(path):
                    break
                lines = [l for l in open(path) if l.startswith("counter ")]
                snaps.append([sum(int(l.split()[1]) for l in lines if b in l) for b in BUCKETS])
            if len(snaps) == 2:
                out[host] = "/".join(str(after - before) for before, after in zip(*snaps))
        return out

    clients = {}

    def cause_of(t0, t1, base=BASE):
        need = max(t1 - t0 - base, 0.0) / 2
        for who, stalls in clients.items():
            if any(g1 - g0 >= need for g0, g1 in overlapping(stalls, t0, t1)):
                return f"{who} stall"
        for host in diag:
            if any(g1 - g0 >= need for g0, g1 in overlapping(gap_idx[host], t0, t1)):
                return f"host stall ({host})"
        for host in diag:
            hit = [p for p in overlapping(pre_idx[host], t0, t1) if p[1] - p[0] >= need]
            if hit:
                return f"guest preemption ({host}: {hit[0][3]} by {hit[0][4]})"
        return "unexplained"

    # load points past capacity (achieved below 98% of offered, or back-pressured):
    # their round trips queue, so their spikes are queueing, not stalls
    saturated = set()
    results = os.path.join(folder, "results.csv")
    if os.path.exists(results):
        for r in csv.reader(open(results)):
            if len(r) >= 20 and r[1] in ("load", "mix") and int(r[5]) > 0:
                if float(r[18]) < 0.98 * int(r[5]) or int(r[17]) > 0:
                    mode = r[2].split(":", 1)[-1]
                    point = r[3].removeprefix("udp-")
                    saturated.add((mode, r[4], r[0], point))

    TOP = 50  # the largest spikes of a file attributed one by one; the rest counted
    totals = collections.defaultdict(collections.Counter)
    maxima = collections.defaultdict(list)
    for path in sorted(glob.glob(os.path.join(folder, "spikes-*.csv"))):
        start = None
        with open(path) as f:
            first = f.readline()
            if first.startswith("# start_realtime_ns="):
                start = int(first.split("=", 1)[1]) / 1e9
            rows = []
            for line in f:
                parts = line.rstrip().split(",")
                if len(parts) == 3 and parts[1].isdigit():
                    rows.append((int(parts[2]) / 1e3, int(parts[1])))
        if start is None:
            continue
        stem = os.path.basename(path)[len("spikes-"):-4]
        run = re.sub(r"-\d+(-[^-]+)?$", lambda m: m.group(1) or "", stem)
        m = re.match(rf"^{re.escape(state)}-(.*)-(\d+)-(\d+)-([^-]+)$", stem)
        if m and (m.group(1), m.group(2), m.group(3), m.group(4)) in saturated:
            maxima[run].append((0.0, "past capacity (queueing)"))
            continue
        # a round trip run: its clients' own stalls, and its agents' poll gaps
        rt = re.match(rf"^({re.escape(state)}-.*)-rtt-(\d+)$", stem)
        clients.clear()
        gaps_note = ""
        if rt:
            tag = f"{rt.group(1)}-{rt.group(2)}"
            for who in ("ping", "pong"):
                clients[who] = read_stalls(os.path.join(folder, f"stalls-{who}-{tag}.csv"))
            gaps = agent_gaps(tag)
            if gaps:
                gaps_note = (" (agent loop gaps 20-50/50-100/100+ us: "
                             + ", ".join(f"{h} {n}" for h, n in sorted(gaps.items())) + ")")
        rows.sort(reverse=True)
        # a localhost round trip is ~6 us, not ~50: its causes are judged against that
        base = 6e-6 if f"{state}-local-" in stem else BASE
        over = sum(1 for lat, _ in rows if lat >= threshold)
        for lat, due in rows[:TOP]:
            if lat >= threshold:
                t0 = start + due / 1e9
                totals[run][cause_of(t0, t0 + lat / 1e6, base)] += 1
        if over > TOP:
            totals[run]["not attributed (past the largest 50)"] += over - TOP
        if rows:
            lat, due = rows[0]
            t0 = start + due / 1e9
            maxima[run].append((lat, cause_of(t0, t0 + lat / 1e6, base) + gaps_note))
        else:
            maxima[run].append((0.0, "under 100 us" + gaps_note))
    print(f"## Spikes of {threshold:.0f} us or more by cause, {state}\n")
    print("A load point past its capacity queues for seconds: its spikes are queueing, not stalls,")
    print("so only each file's 50 largest are attributed one by one.\n")
    print("| run | spikes | client stall | host stall | guest preemption | unexplained | not attributed |")
    print("| --- | --- | --- | --- | --- | --- | --- |")
    for run in sorted(set(totals) | set(maxima)):
        c = totals[run]
        client = sum(v for k, v in c.items() if k.endswith(" stall") and k.split()[0] in ("ping", "pong"))
        host = sum(v for k, v in c.items() if k.startswith("host"))
        guest = sum(v for k, v in c.items() if k.startswith("guest"))
        rest = sum(v for k, v in c.items() if k.startswith("not attributed"))
        print(f"| {run} | {sum(c.values())} | {client} | {host} | {guest} | {c['unexplained']} | {rest} |")
    print(f"\n## Each rep's max round trip and its cause, {state}\n")
    print("| run | max per rep (us): cause |")
    print("| --- | --- |")
    for run, reps in sorted(maxima.items()):
        cells = "; ".join(f"{lat:.0f}: {cause}" if lat else cause for lat, cause in reps)
        print(f"| {run} | {cells} |")

if __name__ == "__main__":
    main()
