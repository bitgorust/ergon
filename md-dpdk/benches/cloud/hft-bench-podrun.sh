#!/usr/bin/env bash
# One HFT extensions lab run inside a pod (hft-bench.d/*.yaml's command).
# Two nodes, two pods, cross-host only.
#
#   hft-bench-podrun.sh ping <ping ep> <pong ep> [messages] [warmup]   (node a)
#   hft-bench-podrun.sh pong <ping ep> <pong ep>                       (node b)
#
# Runs `rtt` (ergon-md-dpdk-harness) cross-host with HARNESS_DRIVER=invoker:
# the measuring thread is also the client conductor and the media driver, one
# spinner per pod, which fits cpu: 2 on c8g.xlarge (exclusive 2,3 + shared 0).
# The measuring loop pins to the first ERGON_ISOLATED_CPUS CPU (the shared-cpus
# plugin gives the pod both isolated CPUs; this script keeps the busy-spin off
# the shared one). The second isolated CPU runs gapspin, so every max can be
# told from a host stall. Busy helpers (none here) would stay on
# ERGON_SHARED_CPUS.
#
# When the plugin set ERGON_ISOLATED_CGROUP (cpu-partition.ergon.io), the
# measuring thread is first moved into that child cgroup and only then pinned
# to the exclusive CPU -- the kernel rejects a partition write if a thread
# outside the child holds an exclusive CPU, so move-then-pin is the order.
#
# When the plugin set ERGON_HUGEPAGES_DIR (hugepages.ergon.io), AERON_DIR is
# $ERGON_HUGEPAGES_DIR/aeron and AERON_FILE_PAGE_SIZE is $ERGON_HUGEPAGE_SIZE,
# unless the manifest already set them.
#
# Writes to $HFT_OUT (an emptyDir the lab script collects):
#   rtt-line.txt        xping's one line: rtt,<label>,udp,p50,...,samples,lost
#   rtt-histogram.csv   RTT_HISTOGRAM
#   spikes.csv          LOAD_SPIKES (LOAD_SPIKE_NS=100000)
#   stalls-ping.csv / stalls-pong.csv   LOOP_STALLS_DIR (LOOP_STALL_NS=20000)
#   gapspin-a.txt / gapspin-b.txt       gapspin on the spare isolated CPU
#   vmstat.txt          pgfault/pgmajfault around the run (/proc/vmstat)
#   meta.txt            env the run saw (AERON_DIR mount type, ulimits, cgroup)
#
# No prelock, no SCHED_FIFO: measured not worth it on AWS (io/benches/BENCHMARK.md).
# Nothing here adds capabilities; rlimits ride along from the NRI plugin.
set -uo pipefail

role=${1:?hft-bench-podrun.sh: ping|pong <ping ep> <pong ep> [messages] [warmup]}
case $role in
ping | pong) ;;
*)
    echo "hft-bench-podrun.sh: role ping|pong, got $role" >&2
    exit 2
    ;;
esac
ping_ep=${2:?ping endpoint host:port}
pong_ep=${3:?pong endpoint host:port}
messages=${4:-${RTT_MESSAGES:-300000}}
warmup=${5:-${RTT_WARMUP:-50000}}
out=${HFT_OUT:-/out}
mkdir -p "$out"

log() { echo "[hft-bench-podrun $role $(date +%T)] $*" >&2; }
die() { log "ERROR: $*"; exit 1; }

# The measuring CPU: the first isolated CPU the plugin named. The gap spinner
# takes the second. Never a shared CPU: the loop is the hot path.
IFS=, read -r -a isolated <<<"${ERGON_ISOLATED_CPUS:-}"
if [[ ${#isolated[@]} -lt 1 || -z ${isolated[0]} ]]; then
    die "ERGON_ISOLATED_CPUS is unset: run with the shared-cpus plugin (ergon.io/shared-millicpus)"
fi
meas_cpu=${isolated[0]}
spin_cpu=${isolated[1]:-}

# Hugepages: the plugin's sized hugetlbfs is the AERON_DIR when it is there.
if [[ -n ${ERGON_HUGEPAGES_DIR:-} ]]; then
    export AERON_DIR=${AERON_DIR:-$ERGON_HUGEPAGES_DIR/aeron}
    export AERON_FILE_PAGE_SIZE=${AERON_FILE_PAGE_SIZE:-${ERGON_HUGEPAGE_SIZE:-2097152}}
    mkdir -p "${AERON_DIR}" 2>/dev/null || die "cannot create AERON_DIR=$AERON_DIR"
fi
: "${AERON_DIR:=/aeron}"

export HARNESS_DRIVER=${HARNESS_DRIVER:-invoker}
export LOAD_SPIKE_NS=${LOAD_SPIKE_NS:-100000}
export LOOP_STALL_NS=${LOOP_STALL_NS:-20000}
export RTT_HISTOGRAM=$out/rtt-histogram.csv
export LOAD_SPIKES=$out/spikes.csv
export LOOP_STALLS_DIR=$out
export RTT_SECONDS=${RTT_SECONDS:-60}

# What the run saw, so a max can be read against the mode.
{
    echo "role=$role"
    echo "meas_cpu=$meas_cpu spin_cpu=${spin_cpu:-none}"
    echo "isolated=${ERGON_ISOLATED_CPUS:-} shared=${ERGON_SHARED_CPUS:-}"
    echo "isolated_cgroup=${ERGON_ISOLATED_CGROUP:-unset}"
    echo "AERON_DIR=$AERON_DIR file_page_size=${AERON_FILE_PAGE_SIZE:-unset}"
    echo "hugepages_dir=${ERGON_HUGEPAGES_DIR:-unset} hugepage_size=${ERGON_HUGEPAGE_SIZE:-unset}"
    findmnt -T "$AERON_DIR" -o FSTYPE,SOURCE,OPTIONS -n 2>/dev/null || true
    echo "sched=$(cut -d' ' -f2 /proc/self/stat 2>/dev/null) policy=$(chrt -p $$ 2>&1 | tr '\n' ' ')"
    echo "memlock=$(ulimit -l) nofile=$(ulimit -n) rtprio=$(ulimit -r)"
    echo "seccomp=${ERGON_SECCOMP:-unset} ptp=${ERGON_PTP_DEVICE:-unset} cpu_dma_latency=${ERGON_CPU_DMA_LATENCY:-unset}"
} >"$out/meta.txt"

vmstat_line() { awk '/^pgfault |^pgmajfault /{printf "%s %s\n", $1, $2}' /proc/vmstat; }

# Move-then-pin: into the isolated child cgroup first, then onto the CPU. The
# partition write (done by the hook) already named this cgroup; a thread that
# is not in it cannot hold an exclusive CPU of a valid isolated partition.
if [[ -n ${ERGON_ISOLATED_CGROUP:-} ]]; then
    if [[ -w $ERGON_ISOLATED_CGROUP/cgroup.procs ]]; then
        echo $$ >"$ERGON_ISOLATED_CGROUP/cgroup.procs" 2>/dev/null ||
            log "WARN: could not move pid $$ into $ERGON_ISOLATED_CGROUP"
        part=$(cat "$ERGON_ISOLATED_CGROUP/cgroup.cpuset.cpus.partition" 2>/dev/null ||
            cat "$ERGON_ISOLATED_CGROUP/cpuset.cpus.partition" 2>/dev/null || echo unknown)
        echo "partition=$part" >>"$out/meta.txt"
        [[ $part == isolated ]] || log "WARN: partition reads '$part', expected 'isolated'"
    else
        log "WARN: $ERGON_ISOLATED_CGROUP/cgroup.procs not writable"
    fi
fi

gapspin_bg=
cleanup() {
    [[ -n ${gapspin_bg:-} ]] && kill "$gapspin_bg" 2>/dev/null
    wait 2>/dev/null
}
trap cleanup EXIT INT TERM

if [[ -n $spin_cpu ]]; then
    # 200 us threshold: anything shorter is not a round trip's max at this scale
    gapspin "$spin_cpu" 200 ${RTT_SECONDS:-60} >"$out/gapspin-$role.txt" 2>>"$out/gapspin-$role.log" &
    gapspin_bg=$!
    log "gapspin on CPU $spin_cpu (pid $gapspin_bg)"
else
    log "no second isolated CPU: no gapspin, host stalls cannot be ruled out"
fi

before=$(vmstat_line)
log "rtt on CPU $meas_cpu, $messages messages after $warmup warmup, AERON_DIR=$AERON_DIR"
rc=0
case $role in
ping)
    rtt xping "$ping_ep" "$pong_ep" "$messages" "$warmup" "$meas_cpu" >"$out/rtt-line.txt" 2>>"$out/rtt-stderr.log" || rc=$?
    ;;
pong)
    # xpong stops on SIGTERM (the lab script sends it after xping prints)
    rtt xpong "$ping_ep" "$pong_ep" "$meas_cpu" >"$out/rtt-pong.log" 2>>"$out/rtt-stderr.log" &
    pong_bg=$!
    echo "$pong_bg" >"$out/pong.pid"
    wait "$pong_bg" || rc=$?
    ;;
esac
after=$(vmstat_line)
paste <(echo "$before") <(echo "$after" | awk '{print $2}') 2>/dev/null |
    awk '{print $1, $2, $3}' >"$out/vmstat.txt" || { echo "$before" >"$out/vmstat.txt"; }

if [[ $role == ping ]]; then
    cat "$out/rtt-line.txt"
    log "done rc=$rc: $(cat "$out/rtt-line.txt" 2>/dev/null)"
else
    log "pong done rc=$rc"
fi
exit $rc
