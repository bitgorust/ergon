#!/usr/bin/env bash
# Fixture test for the lab's analysis and a syntax check of its scripts, with
# no cloud and no VM. fixture/ is a run directory as lab.sh leaves one: a
# results.csv of three reps of DPDK against a base build (dpdk@BIN=bin-base)
# and one rep of kernel busy polling and of a localhost sustained maximum,
# with two-flow sustained maxima (one failed, one rep without its one-flow
# run; one overlapping 0.899 of the shortest window, one exactly 0.900, one a
# row from before the overlap was recorded), and a state directory (isolated/) with
# the jitter snapshots around two reps' round trips and the counters snapshots
# around six flat-out runs (one the two-flow run left out at 0.899) and a
# localhost one. Checks that:
#   * Fisher's exact test gives the textbook two-sided p values;
#   * analyse.py --ab prints expected-ab.md: medians and ranges per metric, a
#     verdict only where the ranges do not overlap, NAK runs pooled with
#     Fisher's p (a host's failed loss read, invalid=1, leaves its run out),
#     and the agents' poll-cycle gaps per rep (n/a where a snapshot is
#     missing or the driver changed between them);
#   * a mode given with its state compares the same as one given bare, and an
#     unknown mode fails, naming the modes there are;
#   * analyse.py prints expected-points.md: per load point, the reps that
#     lost anything of the reps run, and the NAKs sent;
#   * analyse.py prints expected-max.md as its sustained maximum table: the
#     flat-out ceiling with its loss, and the highest ladder rate below the
#     first lossy point (a NAK, a short send, an unechoed message, a failed
#     loss read; the transport's send-side counters alone are not loss), and
#     apart from it the highest rate it keeps up with (98% of offered);
#   * analyse.py prints expected-flows.md as its multi-flow sustained
#     maximum table: the aggregate, each flow, the ratio to the one-flow run
#     of the same rep (the 32 B flat-out run at 32 B), counting only runs
#     whose flows overlapped at least 0.9 (0.900 counts, 0.899 does not, nor
#     an unknown overlap: both reported as left out, with the A/B's rows),
#     a failed flow's run counted apart, and the loss counters with refused
#     sends apart; without multi-flow rows it leaves that table out and
#     changes nothing else;
#   * analyse.py prints expected-frames.md as its throughput phase frames
#     table (the localhost run left out: its frames cross no counted NIC; the
#     two-flow run left out for its overlap named, with no msgs/s, not
#     another rep's), and without the counters snapshots leaves only that
#     table out;
#   * analyse.py and spikes.py read gzipped spike files (spikes-*.csv.gz)
#     exactly as plain ones;
#   * analyse.py treats isolated-nic-irqs (vm.sh loss: the data NICs'
#     interrupts on CPUs other than the housekeeping one) as a check on the
#     run, never loss: a load point where only it grew stays loss-free, the
#     multi-flow table leaves it out of its loss counters, and a note under
#     the loss tables lists each run where it grew (a DPDK one too: vm.sh
#     counts the IRQs the kernel keeps on the NICs DPDK holds), says so where
#     none did, names a run a host did not read it in (unknown there, never
#     counted as none), and is absent from results without it;
#   * irqs.sh against a fake /proc: irqs_check lists every IRQ with a
#     handler whose effective CPUs are not the housekeeping one alone, by
#     name (smp_affinity_list where a kernel has no effective list, or an IRQ
#     not started has it empty); irqs_pin stops irqbalance, moves every IRQ
#     that lets it and new IRQs' default, and lists what stayed (a managed
#     IRQ, whose affinity refuses the write); irqs_nic_off counts the data
#     NICs' interrupts (IRQs named after their interfaces or naming their PCI
#     addresses, not a near miss of either; under DPDK igb_uio's, by address)
#     in every CPU's column but housekeeping's, each IRQ once however many
#     NICs it names, 0 with no NIC given, and nothing, never 0, where a NIC
#     given matches no IRQ;
#   * lib.sh, dry (every VM command faked), with LAB_TPUT_FLOWS=2 runs the
#     multi-flow sustained maximum in cross-host modes only, at 32 B and each
#     load size, after the mode's other runs and before its drivers stop: one
#     channel port and one app CPU per flow at each end, the flows at once
#     (no publisher ends before the last starts, no subscriber before then
#     either), its row (a failed flow's left empty) with the flows' overlap
#     from their subscribers' windows (truncated to the thousandth, empty
#     when a flow has no window, each shortfall logged), loss rows and
#     counters snapshots; that the tput line's readers read the fields they
#     did with its window appended; that unset, 0 or 1 runs none and the
#     app CPUs cap it; that it reaches VM a's environment; and that with it
#     unset nothing multi-flow runs and every other row is the same;
#   * vm.sh, irqs.sh, lib.sh, remote.sh and both lab.sh parse (bash -n),
#     and analyse.py and spikes.py compile.
#
# Run from anywhere:  bash md-dpdk/benches/cloud/test/test-lab.sh
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cloud=$(cd "$here/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
failed=0
fail() { echo "FAIL: $*" >&2; failed=1; }
pass() { echo "ok: $*"; }

# A table's padding and its separator rows' width are presentation: compare
# what is left.
normalise() {
    python3 -c '
import re, sys
for line in sys.stdin.read().rstrip("\n").split("\n"):
    line = re.sub(r"\s+", " ", line).strip()
    print(re.sub(r"-{3,}", "---", line))
'
}
# section <heading>: from that "## " heading to the next one
section() {
    awk -v h="$1" '$0 == h {on = 1; print; next} on && /^## / {exit} on {print}'
}
# drop <heading>: all but that "## " section
drop() {
    awk -v h="$1" '$0 == h {skip = 1; next} skip && /^## / {skip = 0} !skip'
}
same() { # <what> <expected file> <actual file>
    if diff -u <(normalise <"$2") <(normalise <"$3") >"$tmp/diff"; then
        pass "$1"
    else
        fail "$1"
        cat "$tmp/diff" >&2
    fi
}

python3 - "$cloud" <<'PY' && pass "Fisher's exact test" || fail "Fisher's exact test"
import sys
sys.path.insert(0, sys.argv[1])
import analyse
cases = [((3, 1, 1, 3), 34 / 70),  # the tea-tasting table
         ((3, 0, 0, 3), 0.1), ((5, 0, 0, 5), 2 / 252), ((1, 2, 2, 1), 1.0), ((0, 5, 0, 5), 1.0),
         ((1, 14, 5, 10), 100100 / 593775), ((1, 14, 5, 9), 38038 / 475020)]
for table, want in cases:
    got = analyse.fisher_exact(*table)
    assert abs(got - want) < 1e-12, (table, got, want)
PY

# the loss-free rule on its own: a short send is loss, the send-side
# counters behind it are not on their own (a full ring's queued vector)
python3 - "$cloud" "$tmp/lf.csv" <<'PY' && pass "loss-free counts short sends" || fail "loss-free counts short sends"
import sys
sys.path.insert(0, sys.argv[1])
import analyse
load = "{rep},load,isolated:{m},udp-r{rate},288,{rate},1,28000,30000,33000,38000,42000,60000,28500,1250000,1500000,1500000,0,{rate},9000\n"
zero = "naks-sent=0;naks-received=0;retransmits-sent=0;short-sends=0"
loss = "{rep},loss,isolated:{m}:288:r{rate},{h},{kv}\n"
rows = []
for rep in (1, 2):
    for m, at250, at500 in (("x", zero + ";dpdk-tx-eagain=9", zero.replace("short-sends=0", "short-sends=3")),
                            ("y", zero.replace("short-sends=0", "short-sends=1"), zero)):
        for rate, kv in ((250000, at250), (500000, at500)):
            rows.append(load.format(rep=rep, m=m, rate=rate))
            for h in "ab":
                rows.append(loss.format(rep=rep, m=m, rate=rate, h=h, kv=kv if (h, rep) == ("a", 2) else zero))
open(sys.argv[2], "w").writelines(rows)
# z keeps up at 250 k/s, falls behind at 500 k/s (achieved 400 k) with nothing
# lost, and keeps up again at 750 k/s: saturation is not loss
for rep in (1, 2):
    for rate, achieved in ((250000, 250000), (500000, 400000), (750000, 750000)):
        rows.append(load.format(rep=rep, m="z", rate=rate).replace(f",0,{rate},9000", f",0,{achieved},9000"))
        rows += [loss.format(rep=rep, m="z", rate=rate, h=h, kv=zero) for h in "ab"]
open(sys.argv[2], "w").writelines(rows)
r = analyse.read_results(sys.argv[2])
got = (analyse.loss_free(r, "isolated:x", 288), analyse.loss_free(r, "isolated:y", 288))
assert got == ("250,000/s", "none (250,000/s lost)"), got
got = (analyse.loss_free(r, "isolated:z", 288), analyse.keeps_up(r, "isolated:z", 288), analyse.keeps_up(r, "isolated:x", 288))
assert got == ("750,000/s", "250,000/s", "500,000/s"), got
PY

# isolated-nic-irqs, the data NICs' interrupts on CPUs other than the
# housekeeping one: a check on the run, never loss, in every mode. A kernel
# mode and a DPDK one, one rep each of the round trip, throughput, two load
# points and two flows at once, every loss row with it (or, for results from
# before it, without; or a host's row without it, a run where vm.sh could not
# read it); only it grows, in the runs given
python3 - "$cloud" "$tmp/nic-irqs" <<'PY' && pass "isolated-nic-irqs: a check, not loss" || fail "isolated-nic-irqs: a check, not loss"
import contextlib, io, os, subprocess, sys
sys.path.insert(0, sys.argv[1])
import analyse

def write(name, irqs):
    rows = []
    for m in ("kernel", "dpdk"):
        rows += [f"1,rtt,isolated:{m},udp,30000,31000,32000,33000,34000,90000,30500,100000,0\n",
                 f"1,tput,isolated:{m},udp,9000000,8900000,9100000,45000000,0\n",
                 f"1,tputflows,isolated:{m},32,2,18000000,9000000,9000000,0.995\n"]
        rows += [f"1,load,isolated:{m},udp-r{rate},288,{rate},1,28000,30000,33000,38000,42000,60000,28500,"
                 f"1250000,1500000,1500000,0,{rate},9000\n" for rate in (250000, 500000)]
        for run in ("rtt", "", "288:r250000", "288:r500000", "32:flows2"):
            label = f"isolated:{m}" + (f":{run}" if run else "")
            for h in "ab":
                kv = "naks-sent=0;short-sends=0"
                if irqs is not None and irqs.get((label, h), 0) is not None:
                    kv += f";isolated-nic-irqs={irqs.get((label, h), 0)}"
                rows.append(f"1,loss,{label},{h},{kv}\n")
    path = os.path.join(sys.argv[2], name, "results.csv")
    os.makedirs(os.path.dirname(path))
    open(path, "w").writelines(rows)
    return path

def note(path):
    report = subprocess.run([sys.executable, os.path.join(sys.argv[1], "analyse.py"), path],
                            capture_output=True, text=True, check=True).stdout
    return [l for l in report.splitlines() if l.startswith("Data NIC interrupts on CPUs other than")]

grew = write("grew", {("isolated:kernel", "a"): 5, ("isolated:kernel:288:r500000", "b"): 7,
                      ("isolated:kernel:32:flows2", "a"): 3, ("isolated:dpdk", "a"): 4,
                      ("isolated:dpdk:288:r500000", "a"): 2})
r = analyse.read_results(grew)
got = (analyse.loss_free(r, "isolated:kernel", 288), analyse.loss_free(r, "isolated:dpdk", 288))
assert got == ("500,000/s", "500,000/s"), f"loss-free: {got}"
out = io.StringIO()
with contextlib.redirect_stdout(out):
    analyse.flows(r)
row = [l.split("|") for l in out.getvalue().splitlines() if l.startswith("| isolated, kernel UDP ")]
assert row and row[0][-3].strip() == "none", f"the multi-flow table's loss counters: {row}"
lead = ("Data NIC interrupts on CPUs other than the housekeeping one (`isolated-nic-irqs`: on each host, the "
        "growth over the run of the data NICs' IRQ counts, under DPDK those of the IRQs the kernel keeps on their PCI "
        "addresses, in those CPUs' columns of /proc/interrupts; a check on the run, not loss. vm.sh pins every IRQ to "
        "the housekeeping CPU before each driver starts, and the kernel places a NIC's IRQs anew whenever it brings "
        "the NIC up or resets it): ")
want = [lead + "5 of 10 runs grew: `isolated:kernel` rep 1 (a 5); `isolated:kernel:288:r500000` rep 1 (b 7); "
        "`isolated:kernel:32:flows2` rep 1 (a 3); `isolated:dpdk` rep 1 (a 4); `isolated:dpdk:288:r500000` rep 1 (a 2)."]
assert note(grew) == want, f"the note: {note(grew)}"
none = write("none", {})
assert note(none) == [lead + "none of 10 runs grew."], f"the note where none grew: {note(none)}"
unread = write("unread", {("isolated:kernel:288:r250000", "b"): None, ("isolated:kernel", "a"): 5,
                          ("isolated:dpdk:rtt", "a"): None})
want = [lead + "1 of 10 runs grew: `isolated:kernel` rep 1 (a 5). Not read on a host, so unknown there: "
        "`isolated:kernel:288:r250000` rep 1 (b); `isolated:dpdk:rtt` rep 1 (a)."]
assert note(unread) == want, f"the note where a host did not read it: {note(unread)}"
before = write("before", None)
assert note(before) == [], f"the note without the counter: {note(before)}"
PY

python3 "$cloud/analyse.py" --ab dpdk dpdk@BIN=bin-base "$here/fixture/results.csv" >"$tmp/ab.md" ||
    fail "analyse.py --ab exited $?"
same "analyse.py --ab" "$here/expected-ab.md" "$tmp/ab.md"

python3 "$cloud/analyse.py" --ab isolated:dpdk isolated:dpdk@BIN=bin-base "$here/fixture/results.csv" >"$tmp/ab-labels.md" ||
    fail "analyse.py --ab with states exited $?"
same "analyse.py --ab with states" "$here/expected-ab.md" "$tmp/ab-labels.md"

if python3 "$cloud/analyse.py" --ab dpdk dpdk@nothing "$here/fixture/results.csv" >"$tmp/ab-unknown.md" 2>"$tmp/ab-unknown.err"; then
    fail "analyse.py --ab with an unknown mode succeeded"
elif grep -q 'isolated:dpdk@BIN=bin-base' "$tmp/ab-unknown.err"; then
    pass "analyse.py --ab with an unknown mode fails, naming the modes"
else
    fail "analyse.py --ab with an unknown mode: $(cat "$tmp/ab-unknown.err")"
fi

python3 "$cloud/analyse.py" "$here/fixture/results.csv" >"$tmp/full.md" || fail "analyse.py exited $?"
section "## Throughput phase frames" <"$tmp/full.md" >"$tmp/frames.md"
same "analyse.py throughput phase frames" "$here/expected-frames.md" "$tmp/frames.md"
section "## Loss by load point (reps that lost anything, of the reps run; NAKs sent)" <"$tmp/full.md" >"$tmp/points.md"
same "analyse.py loss by load point" "$here/expected-points.md" "$tmp/points.md"
section "## Sustained one-way maximum by message size (M msgs/s)" <"$tmp/full.md" >"$tmp/max.md"
same "analyse.py sustained maximum, flat-out and loss-free" "$here/expected-max.md" "$tmp/max.md"
flows="## Sustained maximum over several flows at once (M msgs/s)"
section "$flows" <"$tmp/full.md" >"$tmp/flows.md"
same "analyse.py sustained maximum over several flows" "$here/expected-flows.md" "$tmp/flows.md"

# without multi-flow rows (LAB_TPUT_FLOWS unset, as by default, or no fifth core): no such
# table, and the rest as before (the frames table aside: it needs the state
# directory's snapshots)
mkdir -p "$tmp/one-flow"
grep -v ',tputflows,' "$here/fixture/results.csv" >"$tmp/one-flow/results.csv"
python3 "$cloud/analyse.py" "$tmp/one-flow/results.csv" >"$tmp/one-flow.md" || fail "analyse.py without multi-flow rows exited $?"
if grep -q 'several flows' "$tmp/one-flow.md"; then
    fail "analyse.py printed the multi-flow table without multi-flow rows"
elif diff -u <(drop "$flows" <"$tmp/full.md" | drop "## Throughput phase frames") "$tmp/one-flow.md" >"$tmp/diff"; then
    pass "analyse.py without multi-flow rows"
else
    fail "analyse.py without multi-flow rows changed more than the multi-flow table"
    cat "$tmp/diff" >&2
fi

# without the snapshots: the same report less that one table
cp -R "$here/fixture" "$tmp/bare"
rm "$tmp/bare"/isolated/*-counters-*
python3 "$cloud/analyse.py" "$tmp/bare/results.csv" >"$tmp/bare.md" || fail "analyse.py without snapshots exited $?"
drop "## Throughput phase frames" <"$tmp/full.md" >"$tmp/full-less-frames.md"
if grep -q 'Throughput phase frames' "$tmp/bare.md"; then
    fail "analyse.py printed the frames table without snapshots"
elif diff -u "$tmp/full-less-frames.md" "$tmp/bare.md" >"$tmp/diff"; then
    pass "analyse.py without snapshots"
else
    fail "analyse.py without snapshots changed more than the frames table"
    cat "$tmp/diff" >&2
fi

# spike files gzipped (the large ones are, to save disk): both reports the same.
# Two of them, an idle round trip with 30 spikes and a load point's, written
# into the state directory and, for analyse.py's per-run summary, next to
# results.csv
cp -R "$here/fixture" "$tmp/gz"
python3 - "$tmp/gz" <<'PY'
import os, sys
for d in (sys.argv[1], os.path.join(sys.argv[1], "isolated")):
    for name, n in (("spikes-isolated-dpdk-rtt-1.csv", 30), ("spikes-isolated-dpdk-288-1-r250000.csv", 3)):
        with open(os.path.join(d, name), "w") as f:
            f.write("# start_realtime_ns=1792032877578595733\nstream,due_ns,latency_ns\n")
            f.writelines(f"0,{i * 33_000_007},{150_000 + 20_000 * i}\n" for i in range(n))
            f.write(f"# spikes_total={n}\n")
for host in "ab":  # spikes.py needs each host's diagnostics; empty ones explain nothing
    with open(os.path.join(sys.argv[1], f"{host}-diag-isolated.log"), "w") as f:
        f.write("== gapspin\n== trace\n")
PY
python3 "$cloud/analyse.py" "$tmp/gz/results.csv" >"$tmp/plain.md" || fail "analyse.py with spike files exited $?"
python3 "$cloud/spikes.py" "$tmp/gz/isolated" >"$tmp/plain-spikes.md" || fail "spikes.py exited $?"
gzip "$tmp/gz"/spikes-*.csv "$tmp/gz"/isolated/spikes-*.csv
python3 "$cloud/analyse.py" "$tmp/gz/results.csv" >"$tmp/gzipped.md" || fail "analyse.py with gzipped spike files exited $?"
python3 "$cloud/spikes.py" "$tmp/gz/isolated" >"$tmp/gzipped-spikes.md" || fail "spikes.py with gzipped spike files exited $?"
if ! grep -Eq 'isolated-dpdk-rtt +\| 1 +\| 30 ' "$tmp/plain.md" || ! grep -q 'isolated-dpdk-rtt | 27 ' "$tmp/plain-spikes.md"; then
    fail "the spike files were not read"
elif diff -u "$tmp/plain.md" "$tmp/gzipped.md" >"$tmp/diff" && diff -u "$tmp/plain-spikes.md" "$tmp/gzipped-spikes.md" >>"$tmp/diff"; then
    pass "gzipped spike files read as plain ones"
else
    fail "gzipped spike files read differently"
    cat "$tmp/diff" >&2
fi

# lib.sh dry, as remote.sh runs it on VM a: every VM command faked (on), VM
# a's layout with a fifth core (app CPUs 2 and 4), VM b's too (6 and 7); one
# rep of a cross-host mode, the same with an idle subscription, and a
# localhost mode, with three load sizes and one mix profile, run with
# LAB_TPUT_FLOWS=2 and again with it unset. Every fake subscriber prints its
# measured window on the wall clock, as tput does. The fake multi-flow
# clients take a while, as real ones do, and log when they start and end. Per
# size, by port: at 32 B (22700-22701) the flows overlap 0.996 of the
# shortest window; at 288 B (22702-22703) the second starts 0.8972 s late and
# its window is the shortest, so they overlap 0.85475, 0.854 truncated; at
# 1344 B the subscriber on port 22705 prints nothing (a failed flow); at 64 B
# the one on 22707 prints no window (a client from before windows).
dry=$tmp/dry
mkdir -p "$dry"
cat >"$tmp/dry.sh" <<'SH'
set -uo pipefail
cloud=$1 out=$2
known=$out/known_hosts user=admin A=192.0.2.1 B=192.0.2.2
log() { echo "$*" >>"$out/log"; }
die() { log "ERROR: $*"; exit 1; }
. "$cloud/lib.sh"
sleep() { command sleep 0.05; }
S=1792032877000000000
# a client's label (vm.sh client <label> ...), which it prints as tput does
label() { local l=${1#*client }; echo "${l%% *}"; }
on() {
    local host=$1 cmd
    shift
    cmd=$*
    printf '%s %s\n' "$host" "$cmd" >>"$out/calls"
    case $cmd in
    *"vm.sh nics") echo "receiver ens6 ens6v 00:00:00:00:00:00 $([[ $host == "$A" ]] && echo 10.30.1.5 || echo 10.30.1.7) 24 0000:00:07.0" ;;
    *"vm.sh layout") [[ $host == "$A" ]] && echo "0 2 1 3 4" || echo "0 6 1 3 7" ;;
    *"vm.sh loss") echo "naks-sent=0;short-sends=0" ;;
    *"vm.sh counters") echo " 1 82 rusteron-dpdk role=sender bdf=0000:00:06.0 port=0 q=0 dir=tx pkts;" ;;
    *" rtt idle "*) echo "idle subscription" ;;
    *" tput x"[ps]"ub "*:227[0-9][0-9]" "*)
        # a subscriber lasts until its publisher has sent a while, a publisher its seconds
        local role=${cmd#* tput x} ep=${cmd#* tput x?ub } port window
        role=${role%% *} ep=${ep%% *} port=${ep##*:}
        echo "$role-start $port" >>"$out/flow-order"
        command sleep "$([[ $role == sub ]] && echo 0.8 || echo 0.3)"
        echo "$role-end $port" >>"$out/flow-order"
        case $port in
        22701) window=,$((S + 20000000)),$((S + 5030000000)) ;;
        22703) window=,$((S + 897200000)),$((S + 5697200000)) ;;
        22707) window= ;;
        *) window=,$S,$((S + 5000000000)) ;;
        esac
        [[ $role == sub && $port != 22705 ]] && echo "tput,$(label "$cmd"),udp,${port}000,1,2,3,0$window"
        ;;
    *" tput xsub "*)
        local ep=${cmd#* tput xsub }
        ep=${ep%% *}
        echo "tput,$(label "$cmd"),udp,${ep##*:}000,1,2,3,0,$S,$((S + 5000000000))"
        ;;
    *"/env") cat >"$out/env-written" ;;
    *"/rc 2>/dev/null") echo 0 ;;
    esac
    return 0
}
reps=1 local_modes=kernel load_sizes='288 1344 64' load_fractions=50 load_seconds=1 load_ladder= mix='p=s:3001:1000:32' mix_seconds=1
tput_seconds=5 rtt_messages=10 rtt_warmup=1 rtt_align=
if [[ $3 == unset ]]; then unset tput_flows; else tput_flows=$3; fi
modes=(dpdk dpdk+idle)
bench_state isolated
[[ $3 == unset ]] && exit 0
# how many flows: none unless LAB_TPUT_FLOWS is 2 or more, capped at the app
# CPUs both VMs have
for tput_flows in "" 0 1 2 3; do
    echo "${tput_flows:-unset} $(flow_count "2 4" "6 7"), one app CPU $(flow_count "2" "6 7")"
done >"$out/counts"
# VM a's environment
tput_flows=2 a_private=10.30.0.10 b_private=10.30.0.11
remote_state isolated
SH
bash "$tmp/dry.sh" "$cloud" "$dry" 2 </dev/null 2>"$dry/stderr" || fail "lib.sh dry run exited $?"
S=1792032877000000000
expect_rows='1,tputflows,isolated:dpdk,32,2,45401000,22700000,22701000,0.996
1,tputflows,isolated:dpdk,288,2,45405000,22702000,22703000,0.854
1,tputflows,isolated:dpdk,1344,2,,22704000,,
1,tputflows,isolated:dpdk,64,2,45413000,22706000,22707000,'
expect_loss=$(for size in 32 288 1344 64; do for node in a b; do
    echo "1,loss,isolated:dpdk:$size:flows2,$node,naks-sent=0;short-sends=0"
done; done)
# each flow's subscriber on VM b and publisher on VM a: its own port and app CPU
expect_calls=$(
    i=0
    for size in 32 288 1344 64; do
        for cpu in 2 4; do
            echo "192.0.2.1 ergon/md-dpdk/benches/cloud/vm.sh client isolated:dpdk tput xpub 10.30.1.7:$((22700 + i)) 11 $cpu $size"
            i=$((i + 1))
        done
    done
    i=0
    for size in 32 288 1344 64; do
        for cpu in 6 7; do
            echo "192.0.2.2 ergon/md-dpdk/benches/cloud/vm.sh client isolated:dpdk tput xsub 10.30.1.7:$((22700 + i)) 5 $cpu"
            i=$((i + 1))
        done
    done | sort
)
snaps=0
for node in a b; do
    for when in before after; do
        for size in 32 288 1344 64; do
            grep -q '^# realtime=' "$dry/$node-counters-flows-$when-isolated-dpdk-$size-1.log" 2>/dev/null && snaps=$((snaps + 1))
        done
    done
done
# Each size's flows at once: every publisher started before any ends, and
# every subscriber still up when the last starts (one after another, each
# flow would have the path to itself, and their sum would pass for a per-flow
# ceiling). And after the mode's every other run, its drivers still up.
order=$(python3 - "$dry/flow-order" "$dry/calls" 2>&1 <<'PY'
import re, sys
events = [line.split() for line in open(sys.argv[1])]
for first in (22700, 22702, 22704, 22706):  # 32, 288, 1344 and 64 B
    at = lambda what: [i for i, (e, p) in enumerate(events) if e == what and first <= int(p) < first + 2]
    if len(at("pub-start")) != 2 or len(at("pub-end")) != 2 or len(at("sub-end")) != 2:
        print(f"ports {first}-{first + 1}: not two flows: {events}")
    elif max(at("pub-start")) > min(at("pub-end")):
        print(f"ports {first}-{first + 1}: publishers one after another: {events}")
    elif max(at("pub-start")) > min(at("sub-end")):
        print(f"ports {first}-{first + 1}: a subscriber ended before the last publisher started: {events}")
calls = open(sys.argv[2]).read().splitlines()
flows = [i for i, c in enumerate(calls) if re.search(r" tput x(sub|pub) \S+:227\d\d ", c)]
others = [i for i, c in enumerate(calls) if "client isolated:dpdk " in c and i not in flows]
stops = [i for i, c in enumerate(calls) if c.endswith("vm.sh stop")]
if not flows or not others or max(others) > flows[0] or any(min(others) < i < flows[-1] for i in stops):
    print("not after the mode's other runs with its drivers up")
PY
) || order="the check failed: $order"
# The tput line's two new fields at its end: every reader still reads the
# fields it did (the throughput row's msgs/s, received and lost; the load
# sweep's maximum, and the rate it derives from it), and analyse.py counts the
# 32 B run, leaves out the 288 B and 64 B ones, and counts the 1344 B one failed
readers=$(python3 - "$cloud" "$dry/results.csv" "$dry/calls" 2>&1 <<'PY'
import sys
sys.path.insert(0, sys.argv[1])
import analyse
r = analyse.read_results(sys.argv[2])
calls = open(sys.argv[3]).read()
checks = [
    (r.tput_rep.get(("isolated:dpdk", 1)), 20125000.0),
    (r.tput_lost.get("isolated:dpdk"), [(3, 0)]),
    (r.tputmax_rep.get(("isolated:dpdk", 288, 1)), 20125000.0),
    ("load xping 10.30.1.7:22200 10.30.1.5:22201 10062500 1 288 2" in calls, True),
    ([(rep, total, overlap) for rep, total, _, overlap in r.tputflows.get(("isolated:dpdk", 32, 2), [])],
     [(1, 45401000.0, 0.996)]),
    (dict(r.tputflows_apart), {("isolated:dpdk", 288, 2): [(1, 0.854)], ("isolated:dpdk", 64, 2): [(1, None)]}),
    (dict(r.tputflows_failed), {("isolated:dpdk", 1344, 2): 1}),
]
for got, want in checks:
    if got != want:
        print(f"got {got!r}, want {want!r}")
PY
) || readers="the check failed: $readers"
if grep -E 'unbound variable|command not found|syntax error' "$dry/stderr"; then
    fail "lib.sh dry run: shell errors"
elif [[ $(grep ',tputflows,' "$dry/results.csv" 2>/dev/null) != "$expect_rows" ]]; then
    fail "lib.sh dry run: multi-flow rows: $(grep ',tputflows,' "$dry/results.csv" 2>/dev/null; tail -5 "$dry/stderr" "$dry/log" 2>/dev/null)"
elif [[ $(grep ':flows' "$dry/results.csv") != "$expect_loss" ]]; then
    fail "lib.sh dry run: multi-flow loss rows: $(grep ':flows' "$dry/results.csv")"
elif [[ $(grep -E 'tput x(sub|pub) [^ ]*:227' "$dry/calls" | sort) != "$expect_calls" ]]; then
    fail "lib.sh dry run: multi-flow clients: $(grep -E 'tput x(sub|pub) [^ ]*:227' "$dry/calls" | sort)"
elif ((snaps != 16)); then
    fail "lib.sh dry run: $snaps of 16 multi-flow counters snapshots"
elif [[ -n $order ]]; then
    fail "lib.sh dry run: multi-flow order: $order"
elif [[ $(grep -c "^1,tput,isolated:dpdk,udp,20125000,1,2,3,0,$S,$((S + 5000000000))\$" "$dry/results.csv") != 1 ]] ||
    [[ $(grep '^1,tputmax,isolated:dpdk,' "$dry/results.csv") != $'1,tputmax,isolated:dpdk,288,20125000,1,2\n1,tputmax,isolated:dpdk,1344,20125000,1,2\n1,tputmax,isolated:dpdk,64,20125000,1,2' ]] ||
    [[ -n $readers ]]; then
    fail "lib.sh dry run: the tput line's readers: $readers $(grep -E ',tput(max)?,isolated:dpdk,' "$dry/results.csv")"
elif ! grep -q 'WARNING: isolated:dpdk 1344 B, 2 flows at once: no result from flow 2' "$dry/log"; then
    fail "lib.sh dry run: the failed flow was not reported: $(cat "$dry/log")"
elif ! grep -q 'WARNING: isolated:dpdk 64 B, 2 flows at once: no measured window from flow 2' "$dry/log"; then
    fail "lib.sh dry run: the flow without a window was not reported: $(cat "$dry/log")"
elif ! grep -q 'WARNING: isolated:dpdk 288 B, 2 flows at once: their measured windows overlapped 0.854 of the shortest' "$dry/log" ||
    grep -q 'isolated:dpdk 32 B, 2 flows at once: their measured windows' "$dry/log"; then
    fail "lib.sh dry run: the overlap was not reported as it should be: $(cat "$dry/log")"
elif [[ $(cat "$dry/counts") != $'unset 0, one app CPU 0\n0 0, one app CPU 0\n1 0, one app CPU 0\n2 2, one app CPU 0\n3 2, one app CPU 0' ]]; then
    fail "lib.sh dry run: flow counts: $(cat "$dry/counts")"
elif ! grep -q 'LAB_TPUT_FLOWS=3, but' "$dry/log" || ! grep -q 'LAB_TPUT_FLOWS=2, but' "$dry/log"; then
    fail "lib.sh dry run: LAB_TPUT_FLOWS over the app CPUs was not reported: $(cat "$dry/log")"
elif ! grep -qx "tput_flows='2'" "$dry/env-written" 2>/dev/null; then
    fail "lib.sh dry run: LAB_TPUT_FLOWS did not reach VM a's environment"
else
    pass "lib.sh dry: multi-flow sustained maximum with LAB_TPUT_FLOWS=2"
fi

# LAB_TPUT_FLOWS unset: nothing multi-flow runs (no client on its ports, no
# snapshot, no row, nothing logged about it), and every other row as with it
off=$tmp/dry-off
mkdir -p "$off"
bash "$tmp/dry.sh" "$cloud" "$off" unset </dev/null 2>"$off/stderr" || fail "lib.sh dry run without LAB_TPUT_FLOWS exited $?"
if grep -E 'unbound variable|command not found|syntax error' "$off/stderr"; then
    fail "lib.sh dry run without LAB_TPUT_FLOWS: shell errors"
elif grep -E ':227[0-9][0-9] ' "$off/calls" || compgen -G "$off/*-counters-flows-*" >/dev/null ||
    grep -E ',tputflows,|:flows' "$off/results.csv" || grep -iE 'flows' "$off/log"; then
    fail "lib.sh dry run without LAB_TPUT_FLOWS ran the multi-flow sustained maximum"
elif ! grep -q ',tput,isolated:dpdk,' "$off/results.csv"; then
    fail "lib.sh dry run without LAB_TPUT_FLOWS: no results: $(tail -5 "$off/stderr" "$off/log" 2>/dev/null)"
elif ! diff -u <(grep -vE ',tputflows,|:flows' "$dry/results.csv" | sort) <(sort "$off/results.csv") >"$tmp/diff"; then
    fail "lib.sh dry run without LAB_TPUT_FLOWS: other rows changed"
    cat "$tmp/diff" >&2
else
    pass "lib.sh dry: no multi-flow sustained maximum with LAB_TPUT_FLOWS unset, the other rows unchanged"
fi

# irqs.sh against a fake /proc: four CPUs, housekeeping 0. The data NICs'
# ENA queues one a CPU (as bringing an interface up leaves them) and one
# management IRQ, a queue named as an older kernel names it (no address in its
# chip's name), the management NIC's queue, a near miss of the data NIC's
# address and name both, a managed NVMe queue (its affinity refuses a write:
# a link into nothing stands in), one IRQ with no effective_affinity_list (an
# older kernel) and one with it empty (not started), and an IRQ with no
# handler (absent from /proc/interrupts). A movable IRQ's effective CPUs
# follow its affinity (a link to it).
proc=$tmp/proc
mkdir -p "$proc/irq"
cat >"$proc/interrupts" <<'EOF'
            CPU0       CPU1       CPU2       CPU3
   0:         40          0          0          0  IR-IO-APIC    2-edge      timer
  24:         10          0          2          0  PCI-MSIX-0000:28:00.0    0-edge      ena-mgmnt@pci:0000:28:00.0
  25:        100          3          0          0  PCI-MSIX-0000:28:00.0    1-edge      enp40s0-Tx-Rx-0
  26:          0          0         50          0  PCI-MSIX-0000:28:00.0    2-edge      enp40s0-Tx-Rx-1
  27:          0          7          0          0  PCI-MSIX-0000:29:00.0    1-edge      enp41s0-Tx-Rx-0
  28:          0          4          0          0  PCI-MSI 1572864-edge      enp41s0-Tx-Rx-1
  29:          0          0          0          9  PCI-MSIX-0000:27:00.0    1-edge      enp39s0-Tx-Rx-0
  30:          0          0          5          0  PCI-MSIX-0000:28:00.1    1-edge      enp40s0np1-Tx-Rx-0
  40:          0          0          0         11  PCI-MSIX-0000:00:04.0    3-edge      nvme0q3
 NMI:          1          2          3          4   Non-maskable interrupts
 LOC:        100        200        300        400   Local timer interrupts
EOF
# <irq> <smp_affinity_list> <effective_affinity_list: link, none, empty or the CPUs>
while read -r irq smp eff; do
    mkdir -p "$proc/irq/$irq"
    echo "$smp" >"$proc/irq/$irq/smp_affinity_list"
    case $eff in
    link) ln -s smp_affinity_list "$proc/irq/$irq/effective_affinity_list" ;;
    none) ;;
    empty) : >"$proc/irq/$irq/effective_affinity_list" ;;
    *) echo "$eff" >"$proc/irq/$irq/effective_affinity_list" ;;
    esac
done <<'EOF'
0 0-3 link
5 0-3 empty
24 0 link
25 1 link
26 2 link
27 1 link
28 1 none
29 3 empty
30 2 link
40 3 3
EOF
rm "$proc/irq/40/smp_affinity_list"
ln -s "$tmp/nothing/here" "$proc/irq/40/smp_affinity_list"
echo f >"$proc/irq/default_smp_affinity"
# A DPDK host's: the data NICs on igb_uio, whose one IRQ each (the ENA
# PMD's admin queue) names only the address, beside the management NIC's
# queue; and a MANA device, whose ports (one a data NIC) share its address.
proc_dpdk=$tmp/proc-dpdk
mkdir -p "$proc_dpdk"
cat >"$proc_dpdk/interrupts" <<'EOF'
            CPU0       CPU1       CPU2       CPU3
  29:          0          0          0          9  PCI-MSIX-0000:27:00.0    1-edge      enp39s0-Tx-Rx-0
  60:         12          4          0          0  PCI-MSIX-0000:28:00.0    0-edge      igb_uio
  61:          5          0          0          0  PCI-MSIX-0000:29:00.0    0-edge      igb_uio
  70:          0          0          8          0  PCI-MSIX-7870:00:00.0    1-edge      mana_q0@pci:7870:00:00.0
EOF
# one argument a NIC: <interface>,<VF>,<PCI address> (DPDK: the address alone)
data_nics="enp40s0,enp40s0,0000:28:00.0 enp41s0,enp41s0,0000:29:00.0"
irqs=$(
    set +e
    exec 2>&1
    LAB_PROC_ROOT=$proc
    # shellcheck source=../irqs.sh
    . "$cloud/irqs.sh" || exit 1
    sudo() { "$@"; }
    systemctl() { echo "systemctl $*" >>"$tmp/irqs-calls"; }
    echo "check: $(irqs_check 0)"
    # shellcheck disable=SC2086
    echo "nic: $(irqs_nic_off 0 $data_nics) $(irqs_nic_off 2 $data_nics) $(irqs_nic_off 0 '' ',,')" \
        "[$(irqs_nic_off 0 $data_nics enp42s0,enp42s0,0000:2a:00.0)]"
    LAB_PROC_ROOT=$proc_dpdk
    echo "dpdk: $(irqs_nic_off 0 0000:28:00.0 0000:29:00.0) [$(irqs_nic_off 0 0000:28:00.0 0000:2a:00.0)]" \
        "$(irqs_nic_off 0 eth1,enP30832s1,7870:00:00.0 eth2,enP30832s1d1,7870:00:00.0)"
    LAB_PROC_ROOT=$proc
    irqs_pin 0 "after the test" 2>&1 >/dev/null | sed 's/^/pin: /'
    for irq in 0 5 24 25 26 27 28 29 30; do printf '%s' "$(cat "$proc/irq/$irq/smp_affinity_list")"; done
    echo " default $(cat "$proc/irq/default_smp_affinity")"
) || true
# Before pinning, every IRQ off CPU 0 listed (28's CPUs read from its
# affinity, it having no effective list, and 29's, its list empty); the data
# NICs' interrupts off CPU 0 (IRQs 24 to 28: 2 + 3 + 50 + 7 + 4), off CPU 2 as
# if it were housekeeping (10 + 103 + 0 + 7 + 4), none with no NIC given, and
# unknown with a third NIC that has no IRQ; on the DPDK host, igb_uio's off
# CPU 0 (4), unknown with a NIC whose igb_uio IRQ is not there, and the MANA
# queue once for both ports (8); after pinning, the managed queue alone
# elsewhere, every other affinity CPU 0 (5's too, with no handler) and new
# IRQs' default mask 1
want="check: 1 on CPU 0 alone, 8 elsewhere (0:timer@0-3 25:enp40s0-Tx-Rx-0@1 26:enp40s0-Tx-Rx-1@2 27:enp41s0-Tx-Rx-0@1 28:enp41s0-Tx-Rx-1@1 29:enp39s0-Tx-Rx-0@3 30:enp40s0np1-Tx-Rx-0@2 40:nvme0q3@3), default for new ones f
nic: 66 124 0 []
dpdk: 4 [] 8
pin: irqs after the test: 8 on CPU 0 alone, 1 elsewhere (40:nvme0q3@3), default for new ones 1
000000000 default 1"
if [[ $irqs != "$want" ]]; then
    fail "irqs.sh against a fake /proc"
    diff -u <(echo "$want") <(echo "$irqs") >&2 || true
elif [[ $(cat "$tmp/irqs-calls" 2>/dev/null) != "systemctl stop irqbalance" ]]; then
    fail "irqs.sh: irqbalance was not stopped: $(cat "$tmp/irqs-calls" 2>/dev/null)"
else
    pass "irqs.sh against a fake /proc: pinned, checked, data NIC interrupts off housekeeping counted"
fi

for script in vm.sh irqs.sh lib.sh remote.sh azure/lab.sh aws/lab.sh; do
    bash -n "$cloud/$script" && pass "bash -n $script" || fail "bash -n $script"
done
for script in analyse.py spikes.py; do
    python3 -c 'import ast, sys; ast.parse(open(sys.argv[1]).read(), sys.argv[1])' "$cloud/$script" &&
        pass "$script compiles" || fail "$script does not compile"
done

if ((failed)); then
    echo "test-lab: FAILED" >&2
    exit 1
fi
echo "test-lab: all passed"
