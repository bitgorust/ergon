#!/usr/bin/env bash
# One VM's side of md-dpdk/benches/cloud/{azure,aws}/lab.sh, on Azure (MANA) or AWS
# (ENA): the cloud is read from the instance metadata service. Every command is
# idempotent and runs as the admin user from ~/ergon (sudo where it must).
#
#   vm.sh bootstrap          packages, the backports kernel, hugepages, the cloud's DPDK prerequisites
#   vm.sh build              the media driver (static, dpdk), the rtt/tput/counters clients, igb_uio (AWS)
#   vm.sh build-base         the revision lib.sh unpacked in ~/ergon-base (LAB_BASE): its media
#                            driver and benchmark clients into ~/bin-base, for modes with @BIN=bin-base
#   vm.sh isolate            CPU isolation on the kernel command line (takes effect at the next boot)
#   vm.sh tune               after each boot: irqbalance off, every IRQ on the housekeeping CPU, hugetlbfs
#                            AERON_DIR, transparent hugepages off (the baseline); the IRQs are pinned
#                            again wherever the kernel may have placed them anew (irqs.sh)
#   vm.sh baseline           the settings every mode runs on top of (the recommended configuration's
#                            host-wide and DPDK settings; per-mode ones such as @RX_RING=4096 are not in it)
#   vm.sh layout             "<housekeeping> <app> <sender> <receiver>" CPUs: the first thread of four cores
#   vm.sh nics               the data NICs from the cloud's metadata: one line per role
#   vm.sh driver <mode>      start the media driver: kernel, kernel-busy or dpdk; a -mono
#                            suffix clocks the transport and clients with CLOCK_MONOTONIC, not TSC
#   vm.sh client <label> <rtt|tput> <args>   a benchmark client against the running driver
#   vm.sh stop               stop the media driver and give the data NICs back to the kernel
#   vm.sh counters           validity counters: VF or allowance counters, data-path switches; under
#                            DPDK also the transport's frames, bytes, refused and queued sends, and
#                            the ENA's per-queue TX statistics
#   vm.sh wc-check           AWS, DPDK: whether the ENA's low-latency queue BAR is mapped write-combining
#   vm.sh pmd-log            the DPDK run's PMD and EAL lines on how the NIC was set up (LLQ, placement)
#   vm.sh errors             the driver's distinct error log and loss report
#   vm.sh loss               one "key=value;..." line of every loss counter: Aeron's (NAKs,
#                            retransmits, short sends, gap fills), the DPDK transport's drops,
#                            the kernel's UDP errors, the data NICs' kernel drop counters and
#                            their missing routes (routes-missing, as found); then a check on
#                            the run that is not loss: the data NICs' interrupts on any CPU but
#                            the housekeeping one (isolated-nic-irqs, irqs.sh)
#   vm.sh clockcheck [secs]  the clocks' read cost and the TSC's drift, on the app CPU
#   vm.sh criterion          the clock Criterion bench (benches/clock.rs), on the app CPU
#   vm.sh ptp                the PTP hardware clock: device, chrony's lock on it, NIC timestamping
#   vm.sh rx-hw-delay        "<mean ns>,<max ns>,<frames>" NIC arrival -> dispatch on the receiver port
#
# AWS with ~/bench/placement = precision-time (lab.sh LAB_PLACEMENT): DPDK 25.11
# from source (ENA RX timestamps), Amazon's ENA driver with the PTP hardware
# clock, chrony on that clock, and the DPDK driver asks for NIC RX timestamps.
#
# Roles: the data NIC with the lower private IP is the driver's sender port,
# the higher its receiver port (both on the data subnet, DATA_SUBNET).
set -euo pipefail
# ethtool, sysctl and modprobe live in /usr/sbin, off a non-login ssh PATH
export PATH=/usr/local/sbin:/usr/sbin:/sbin:$PATH
self=$(readlink -f "$0")
# irqs_pin, irqs_check and irqs_nic_off: the IRQs kept on the housekeeping CPU
# shellcheck source=irqs.sh
. "$(dirname "$self")/irqs.sh"
cd "$HOME/ergon"
run=$HOME/bench
mkdir -p "$run"
# The media driver and the benchmark clients (rtt, tput, load): this
# checkout's build, or another build's (an A/B against older code) for one
# run, <mode>@BIN=<dir> (`driver` records it in $run/bin, `stop` removes it),
# or by hand, BENCH_BIN. The diagnostic tools (counters, clockcheck) are
# always this checkout's, so every mode's counters are read alike.
tools=$HOME/ergon/target/release
bin=$(cat "$run/bin" 2>/dev/null || true)
bin=${bin:-${BENCH_BIN:-$tools}}
# The lab's baseline, the recommended configuration's (md-dpdk/BENCHMARKS.md)
# settings that apply to every mode or every DPDK mode: every mode runs on top
# of it, and an A/B against it is a mode's override (@THP=always,
# @AERON_RECEIVER_IO_VECTOR_CAPACITY=4). Transparent hugepages are host-wide,
# so every mode's; the receiver's IO vectors are DPDK modes'. A setting the
# recommended configuration gives one transport only, such as kernel busy
# polling's 4,096-entry receive ring, stays in the mode (@RX_RING=4096).
BASE_THP=never
BASE_RCV_IO_VECTORS=16
log() { echo "[vm $(hostname) $(date +%T)] $*" >&2; }
# lab.sh writes the data subnet here once (ssh passes no environment)
DATA_SUBNET=${DATA_SUBNET:-$(cat "$run/data_subnet" 2>/dev/null || true)}

# Azure answers its metadata header; anything else here is AWS (IMDSv2).
cloud() {
    if [[ -f $run/cloud ]]; then cat "$run/cloud"; return; fi
    if curl -fsS -m 2 -H Metadata:true "http://169.254.169.254/metadata/instance?api-version=2021-02-01" >/dev/null 2>&1; then
        echo azure | tee "$run/cloud"
    else
        echo aws | tee "$run/cloud"
    fi
}
CLOUD=$(cloud)
PLACEMENT=$(cat "$run/placement" 2>/dev/null || true)
precision_time() { [[ $CLOUD == aws && $PLACEMENT == precision-time ]]; }
# DPDK 25.11, the first with ENA RX timestamps (Debian 13 has 24.11)
DPDK_VERSION=25.11
DPDK_PREFIX=/opt/dpdk-$DPDK_VERSION
if precision_time; then
    export PKG_CONFIG_PATH=$DPDK_PREFIX/lib/x86_64-linux-gnu/pkgconfig
fi

# The first thread of the first five cores, in core order: housekeeping (the
# driver's conductor, its housekeeping and local-I/O threads, the clients'
# conductors), app (ping, pong, publisher or subscriber), the driver's sender,
# its receiver, and a second app core for the localhost runs (both ends on one
# host; empty on a four-core VM).
# Azure numbers SMT twins adjacently (0-1, 2-3, ...), AWS as N and N+cores, so
# the CPUs differ but each always has a core to itself.
# Cores with a CPU the pre-flight saw stall (vm.sh preflight writes
# $run/noisy_cpus: on Azure some vCPUs stall for milliseconds at one second of
# every minute) are skipped, housekeeping's excepted, while enough quiet ones
# remain; the noisy ones fill in last, so a short VM still gets a layout.
layout() {
    local cpu first siblings primaries=() quiet=() noisy=()
    local bad=" $(cat "$run/noisy_cpus" 2>/dev/null) "
    for cpu in $(ls -d /sys/devices/system/cpu/cpu[0-9]* | sed 's/.*cpu//' | sort -n); do
        siblings=$(cat /sys/devices/system/cpu/cpu"$cpu"/topology/thread_siblings_list)
        first=${siblings%%[,-]*}
        [[ $first == "$cpu" ]] || continue
        primaries+=("$cpu")
        local hit=0 s
        local part
        for part in ${siblings//,/ }; do
            for s in $(seq "${part%-*}" "${part#*-}"); do
                [[ $bad == *" $s "* ]] && hit=1
            done
        done
        if ((${#primaries[@]} == 1 || hit == 0)); then quiet+=("$cpu"); else noisy+=("$cpu"); fi
    done
    local ordered=("${quiet[@]}" "${noisy[@]}")
    echo "${ordered[@]:0:5}"
}
read -r HK_CPU APP_CPU SENDER_CPU RECEIVER_CPU APP2_CPU <<<"$(layout)"

# AERON_DIR on 2 MiB hugetlbfs, as rusteron's tuned runs (vm.sh tune mounts it)
export AERON_DIR=/mnt/huge/aeron-bench
# The driver's settings, as `export K='v'` lines the driver's shell sources.
common_env() {
    cat <<ENV
export AERON_DIR='$AERON_DIR'
export AERON_DIR_DELETE_ON_START='true'
export AERON_FILE_PAGE_SIZE='2097152'
export AERON_TERM_BUFFER_SPARSE_FILE='false'
export AERON_THREADING_MODE='DEDICATED'
export AERON_CONDUCTOR_CPU_AFFINITY='$HK_CPU'
export AERON_SENDER_CPU_AFFINITY='$SENDER_CPU'
export AERON_RECEIVER_CPU_AFFINITY='$RECEIVER_CPU'
export AERON_SENDER_IDLE_STRATEGY='noop'
export AERON_RECEIVER_IDLE_STRATEGY='noop'
export AERON_SENDER_WILDCARD_PORT_RANGE='20000 20999'
export AERON_RECEIVER_WILDCARD_PORT_RANGE='21000 21999'
export AERON_MTU_LENGTH='1408'
export AERON_SOCKET_SO_SNDBUF='2097152'
export AERON_SOCKET_SO_RCVBUF='2097152'
export AERON_RCV_INITIAL_WINDOW_LENGTH='2097152'
export RUSTERON_MEDIA_DRIVER_DIR='$AERON_DIR'
ENV
}

# "<mac> <ip> <prefix>" of each data NIC, lowest address first, from the
# metadata service once (with retries: one empty answer from it killed a run),
# then from $run/data_macs: an instance's NICs never change.
data_macs() {
    local subnet=${DATA_SUBNET:?DATA_SUBNET: the data subnet, e.g. 10.20.1.0/24} lines
    if [[ -s $run/data_macs && $(wc -l <"$run/data_macs") -eq 2 ]]; then
        cat "$run/data_macs"
        return
    fi
    # a failed lookup must reach nics()'s message, not exit here under set -e
    lines=$(data_macs_imds "$subnet") || true
    if [[ $(wc -l <<<"$lines") -eq 2 && $lines != *"  "* ]]; then printf '%s\n' "$lines" >"$run/data_macs"; fi
    printf '%s\n' "$lines"
}
data_macs_imds() {
    local subnet=$1
    local get=(curl -fsS -m 5 --retry 5 --retry-all-errors --retry-delay 1)
    case $CLOUD in
    azure)
        "${get[@]}" -H Metadata:true "http://169.254.169.254/metadata/instance/network?api-version=2021-02-01&format=json" |
            python3 -c '
import ipaddress, json, sys
net = ipaddress.ip_network(sys.argv[1])
for i in json.load(sys.stdin)["interface"]:
    mac = ":".join(i["macAddress"][k:k + 2] for k in range(0, 12, 2)).lower()
    for a in i["ipv4"]["ipAddress"]:
        if ipaddress.ip_address(a["privateIpAddress"]) in net:
            print(mac, a["privateIpAddress"], net.prefixlen)
' "$subnet"
        ;;
    aws)
        local token mac
        token=$("${get[@]}" -X PUT -H "X-aws-ec2-metadata-token-ttl-seconds: 300" http://169.254.169.254/latest/api/token)
        local md=http://169.254.169.254/latest/meta-data/network/interfaces/macs
        for mac in $("${get[@]}" -H "X-aws-ec2-metadata-token: $token" "$md/" | tr -d /); do
            local ip
            ip=$("${get[@]}" -H "X-aws-ec2-metadata-token: $token" "$md/$mac/local-ipv4s" | head -1)
            [[ -n $ip ]] || { echo "metadata: no IPv4 for $mac" >&2; continue; }
            python3 -c 'import ipaddress,sys; sys.exit(ipaddress.ip_address(sys.argv[1]) not in ipaddress.ip_network(sys.argv[2]))' \
                "$ip" "$subnet" && echo "$mac $ip ${subnet#*/}"
        done
        ;;
    esac | sort -t. -k4,4n
}

# "<role> <iface> <vf iface> <mac> <ip> <prefix> <bdf>" for the two data NICs.
# Azure: the netvsc interface has the MAC and no master, and its MANA VF is
# enslaved to it. AWS: the ENA is the interface and the PCI device both.
nics() {
    local lines
    lines=$(data_macs)
    [[ $(wc -l <<<"$lines") -eq 2 ]] || { echo "expected two data NICs on $DATA_SUBNET, found: $lines" >&2; return 1; }
    local role=sender
    while read -r mac ip prefix; do
        local iface vf bdf
        iface=$(for d in /sys/class/net/*; do
            if [[ $(cat "$d/address") == "$mac" && ! -e $d/master ]]; then basename "$d"; fi
        done | head -1)
        if [[ $CLOUD == azure ]]; then
            vf=$(ip -br link show master "$iface" | awk '{print $1}' | head -1)
        else
            vf=$iface
        fi
        bdf=$(ethtool -i "$vf" | awk '/bus-info/ {print $2}')
        echo "$role $iface $vf $mac $ip $prefix $bdf"
        role=receiver
    done <<<"$lines"
}

# The data NICs are this script's alone: the OS's network manager must not run
# DHCP on them or tidy away the addresses, rules and routes data_kernel_up
# gives them. On AWS's Debian images cloud-init handles NIC hotplug: each time
# an ENA came back from DPDK it ran DHCP on every NIC and re-applied netplan,
# replacing the data NICs' addresses 2 s after data_kernel_up set them (so the
# kernel dropped their routes: sends failed with ENETUNREACH, or left by the
# other NIC and were dropped) and resetting the management NIC's. So: cloud-init
# keeps to boot-time network configuration and its hotplug socket is masked;
# systemd-networkd sees them unmanaged (an earlier-sorting .network file wins
# over netplan's) and leaves foreign rules and routes alone (from its next
# start: `isolate` reboots); a DHCP client running on one is stopped by PID.
data_unmanaged() {
    local macs file=/etc/systemd/network/00-ergon-lab-data.network conf=/etc/systemd/networkd.conf.d/ergon-lab.conf
    macs=$(data_macs | awk '{printf "%s%s", sep, $1; sep = " "}')
    [[ -n $macs ]] || return 1
    local want
    want=$(printf '[Match]\nMACAddress=%s\n\n[Link]\nUnmanaged=yes\n' "$macs")
    if [[ $(cat "$file" 2>/dev/null) != "$want" ]]; then
        sudo mkdir -p "$(dirname "$file")" "$(dirname "$conf")"
        printf '%s\n' "$want" | sudo tee "$file" >/dev/null
        printf '[Network]\nManageForeignRoutingPolicyRules=no\nManageForeignRoutes=no\n' | sudo tee "$conf" >/dev/null
        if systemctl is-active --quiet systemd-networkd; then
            sudo networkctl reload || echo "networkctl reload failed: the data NICs are unmanaged from the next boot" >&2
        fi
    fi
    local nohp=/etc/cloud/cloud.cfg.d/99-ergon-lab-no-hotplug.cfg
    if [[ -d /etc/cloud/cloud.cfg.d && ! -f $nohp ]]; then
        printf 'updates:\n  network:\n    when: [boot]\n' | sudo tee "$nohp" >/dev/null
    fi
    if systemctl cat cloud-init-hotplugd.socket >/dev/null 2>&1 &&
        [[ $(systemctl is-enabled cloud-init-hotplugd.socket 2>/dev/null) != masked ]]; then
        sudo systemctl mask --now cloud-init-hotplugd.socket cloud-init-hotplugd.service >/dev/null 2>&1 ||
            echo "could not mask cloud-init's hotplug socket" >&2
    fi
    local pid iface
    for iface in $(nics | awk '{print $2; if ($3 != $2) print $3}'); do
        for pid in $(pgrep -x 'dhclient|dhcpcd' || true); do
            if tr '\0' '\n' 2>/dev/null <"/proc/$pid/cmdline" | grep -qx "$iface"; then
                echo "stopping DHCP client $pid on data NIC $iface" >&2
                sudo kill "$pid" 2>/dev/null || true
            fi
        done
    done
}

# Give the data NICs back to the kernel. Both are on one subnet, and both clouds
# drop a packet whose source address is not the sending NIC's, so each NIC gets
# a routing table its own source address selects. Nothing is removed before
# its replacement is in place, so the addresses' traffic always has a route.
# Then every IRQ back on the housekeeping CPU: an ENA brought up (after DPDK
# gave it back, `stop`'s rebind, or a ring change's reset) sets its queue
# IRQs' affinity anew, one queue a CPU.
data_kernel_up() {
    data_unmanaged
    local table=100
    nics | while read -r _ iface vf _ ip prefix _; do
        sudo ip link set "$vf" up
        sudo ip link set "$iface" up
        sudo ip addr replace "$ip/$prefix" dev "$iface"
        local net
        net=$(python3 -c 'import ipaddress,sys; print(ipaddress.ip_network(sys.argv[1], strict=False))' "$ip/$prefix")
        sudo ip route replace "$net" dev "$iface" src "$ip" table "$table"
        if ! ip rule show | awk -v ip="$ip" -v t="$table" '$1 == t":" && $3 == ip && $5 == t {f = 1} END {exit !f}'; then
            sudo ip rule add from "$ip" lookup "$table" priority "$table"
        fi
        # NICs can come back in another order after a DPDK run: any other rule
        # left for this address goes, now that its current one is in place
        ip rule show | awk -v ip="$ip" -v t="$table" '$2 == "from" && $3 == ip && !($1 == t":" && $5 == t) {sub(":", "", $1); print $1, $5}' |
            while read -r prio tbl; do sudo ip rule del priority "$prio" from "$ip" lookup "$tbl"; done
        table=$((table + 1))
    done
    irqs_pin "$HK_CPU" "after the data NICs came up"
    # nothing measured on a broken path: each NIC's rule and route must hold
    local missing
    missing=$(data_routes_missing)
    [[ $missing == 0 ]] || { echo "data NICs: $missing policy route(s) missing after configuring them" >&2; return 1; }
}

# How many data NICs lack their policy rule or their table's route (0 while
# DPDK owns them: no kernel interfaces then). `loss` reports it, so a route
# lost mid-run shows as a loss counter that grew.
data_routes_missing() {
    local table=100 missing=0
    while read -r _ iface _ _ ip _; do
        # a NIC DPDK owns has no kernel interface: nothing to route
        [[ -n $iface && -e /sys/class/net/$iface ]] || { table=$((table + 1)); continue; }
        ip rule show | awk -v ip="$ip" -v t="$table" '$1 == t":" && $3 == ip && $5 == t {f = 1} END {exit !f}' &&
            ip route show table "$table" | grep -q " dev $iface " || missing=$((missing + 1))
        table=$((table + 1))
    done < <(nics 2>/dev/null)
    echo "$missing"
}

# Data NICs a DPDK run took from the kernel go back to it.
data_return_to_kernel() {
    local dev rebound=0
    # Azure: VMBus devices on uio_hv_generic go back to hv_netvsc
    for dev in /sys/bus/vmbus/drivers/uio_hv_generic/*-*-*-*-*; do
        [[ -e $dev ]] || continue
        echo "$(basename "$dev")" | sudo tee /sys/bus/vmbus/drivers/uio_hv_generic/unbind >/dev/null
        echo "$(basename "$dev")" | sudo tee /sys/bus/vmbus/drivers/hv_netvsc/bind >/dev/null
        rebound=1
    done
    # AWS: ENAs on igb_uio go back to ena
    for dev in /sys/bus/pci/drivers/igb_uio/0000:*; do
        [[ -e $dev ]] || continue
        local bdf
        bdf=$(basename "$dev")
        echo "$bdf" | sudo tee /sys/bus/pci/drivers/igb_uio/unbind >/dev/null
        echo | sudo tee /sys/bus/pci/devices/"$bdf"/driver_override >/dev/null
        echo "$bdf" | sudo tee /sys/bus/pci/drivers_probe >/dev/null
        rebound=1
    done
    if ((rebound)); then sleep 3; fi
}

dpdk_active() {
    [[ -n $(ls -d /sys/bus/vmbus/drivers/uio_hv_generic/*-*-*-*-* /sys/bus/pci/drivers/igb_uio/0000:* 2>/dev/null) ]]
}

# "tx_packets: <n> rx_packets: <n>": the frames a kernel interface sent and received
nic_packets() {
    local s=/sys/class/net/$1/statistics
    printf 'tx_packets: %s rx_packets: %s' "$(cat "$s/tx_packets" 2>/dev/null || echo 0)" "$(cat "$s/rx_packets" 2>/dev/null || echo 0)"
}

# A host setting's value before an experiment changed it (vm.sh driver
# <mode>@KEY=value), kept once per setting until `stop` puts it back.
save_host() {
    mkdir -p "$run/restore"
    [[ -f $run/restore/$1 ]] || printf '%s\n' "$2" >"$run/restore/$1"
}

# ethtool -C/-G exit 80 when nothing changed: the value is already in effect.
ethtool_set() {
    local rc=0
    sudo ethtool "$@" 2>"$run/ethtool.err" || rc=$?
    [[ $rc == 0 || $rc == 80 ]] || { echo "ethtool $*: $(cat "$run/ethtool.err")" >&2; return 1; }
}

# The host settings AWS's low-latency guidance names, one experiment at a time
# (<mode>@KEY=value; anything else is an environment variable for the driver).
# One that cannot apply on this host fails the run, never passes as a no-op.
#   THP=<always|madvise|never>  transparent hugepages (never, the baseline,
#        also stops khugepaged, whose every scan drains each CPU's LRU batch,
#        preempting busy-spinning agents; `stop` puts the baseline back)
#   BIN=<dir>         the media driver and benchmark clients from <dir>
#        (absolute, or relative to the home directory) instead of this
#        checkout's build: dpdk@BIN=bin-base A/Bs an older build (lab.sh
#        LAB_BASE builds one there) against the current one. A client <dir>
#        lacks is this checkout's, and the client says so
#   CSTATE=<n>        no idle state deeper than the n-th on any CPU
#        (intel_idle.max_cstate=n at run time; 1 = C1, 0 = poll only)
#   COALESCE=off|<rx>,<tx>  the data NICs' interrupt moderation, adaptive off
#        (kernel modes; off = rx-usecs 0 tx-usecs 0)
#   RX_RING=<n>       the data NICs' receive ring (kernel modes)
#   MIN_FREE_PCT=<n>  vm.min_free_kbytes at n% of memory (no direct reclaim stalls)
#   CLOCKSOURCE=<name> the kernel's clocksource
#   RELIABLE=false    the clients subscribe with reliable=false|group=false|
#        tether=false: no NAKs or retransmits, a gap filled instead (counted as
#        loss gap fills), so a run past capacity sheds rather than storms
#   AGENTS=off        the cloud's guest agents stopped (Azure: walinuxagent and
#        hv-kvp-daemon, suspected of a once-a-minute stall of network
#        syscalls; AWS images run none)
# Returns 1 when KEY is not a host setting.
host_setting() {
    local key=$1 value=$2 mode=$3 st vf names=() cur
    case $key in
    THP)
        [[ $value =~ ^(always|madvise|never)$ ]] || { echo "THP=$value: always, madvise or never" >&2; exit 2; }
        echo "$value" | sudo tee /sys/kernel/mm/transparent_hugepage/enabled >/dev/null || exit 2
        ;;
    BIN)
        local dir=$value
        [[ $dir == /* ]] || dir=$HOME/${dir#\~/}
        [[ -x $dir/media_driver ]] || { echo "BIN=$value: no media_driver in $dir" >&2; exit 2; }
        printf '%s\n' "$dir" >"$run/bin"
        save_host bin set
        bin=$dir
        log "BIN=$value: the media driver and clients from $dir ($(ls "$dir" | tr '\n' ' '))"
        ;;
    CSTATE)
        [[ $value =~ ^[0-9]$ ]] || { echo "CSTATE=$value: an idle state index" >&2; exit 2; }
        if [[ ! -d /sys/devices/system/cpu/cpu0/cpuidle ]]; then
            echo "CSTATE=$value: no cpuidle driver here ($(cat /sys/devices/system/cpu/cpuidle/current_driver 2>/dev/null || echo none)): the hypervisor owns idle" >&2
            exit 2
        fi
        save_host CSTATE all-enabled
        for st in /sys/devices/system/cpu/cpu[0-9]*/cpuidle/state[0-9]*; do
            if ((${st##*state} > value)); then
                echo 1 | sudo tee "$st/disable" >/dev/null
                names+=("$(cat "$st/name")")
            fi
        done
        log "CSTATE=$value: disabled $(printf '%s\n' "${names[@]}" | sort -u | tr '\n' ' ')on every CPU"
        ;;
    COALESCE)
        [[ $mode == kernel || $mode == kernel-busy ]] || { echo "COALESCE: kernel modes only (DPDK takes no interrupts)" >&2; exit 2; }
        local args
        if [[ $value == off ]]; then
            args="adaptive-rx off rx-usecs 0 tx-usecs 0"
        elif [[ $value =~ ^([0-9]+),([0-9]+)$ ]]; then
            args="adaptive-rx off rx-usecs ${BASH_REMATCH[1]} tx-usecs ${BASH_REMATCH[2]}"
        else
            echo "COALESCE=$value: off or <rx usecs>,<tx usecs>" >&2
            exit 2
        fi
        while read -r _ _ vf _; do
            cur=$({ ethtool -c "$vf" 2>/dev/null || true; } | awk '/^Adaptive RX:/ {a = $3} /^rx-usecs:/ {r = $2} /^tx-usecs:/ {t = $2}
                END {if (a != "n/a" && r != "n/a") printf "adaptive-rx %s rx-usecs %s tx-usecs %s", a, r, t}')
            [[ -n $cur ]] || { echo "COALESCE: $vf ($(ethtool -i "$vf" | awk '/^driver/ {print $2}')) has no interrupt moderation to set" >&2; exit 2; }
            save_host "coalesce-$vf" "$cur"
            # shellcheck disable=SC2086
            ethtool_set -C "$vf" $args || exit 2
        done < <(nics)
        # a driver may reset the device for it, placing its IRQs anew
        irqs_pin "$HK_CPU" "after COALESCE=$value"
        log "COALESCE=$value: $args on the data NICs"
        ;;
    RX_RING)
        [[ $mode == kernel || $mode == kernel-busy ]] || { echo "RX_RING: kernel modes only (DPDK sets its own descriptors)" >&2; exit 2; }
        [[ $value =~ ^[0-9]+$ ]] || { echo "RX_RING=$value: a descriptor count" >&2; exit 2; }
        while read -r _ _ vf _; do
            cur=$({ ethtool -g "$vf" 2>/dev/null || true; } | awk '/^Current/ {c = 1} c && /^RX:/ {print $2; exit}')
            [[ -n $cur ]] || { echo "RX_RING: $vf reports no ring size" >&2; exit 2; }
            save_host "ring-$vf" "$cur"
            ethtool_set -G "$vf" rx "$value" || exit 2
        done < <(nics)
        # a ring change resets the device: its addresses and routes again, and
        # its IRQs (data_kernel_up pins them)
        data_kernel_up
        log "RX_RING=$value on the data NICs"
        ;;
    MIN_FREE_PCT)
        [[ $value =~ ^[1-5]$ ]] || { echo "MIN_FREE_PCT=$value: 1 to 5" >&2; exit 2; }
        save_host min_free_kbytes "$(cat /proc/sys/vm/min_free_kbytes)"
        cur=$(awk -v p="$value" '/^MemTotal:/ {printf "%d", $2 * p / 100}' /proc/meminfo)
        sudo sysctl -qw vm.min_free_kbytes="$cur"
        log "MIN_FREE_PCT=$value: vm.min_free_kbytes $cur"
        ;;
    RELIABLE)
        [[ $value == false ]] || { echo "RELIABLE=$value: false" >&2; exit 2; }
        echo 'reliable=false|group=false|tether=false' >"$run/sub_channel_params"
        save_host sub_channel_params set
        log "RELIABLE=false: subscriptions with reliable=false|group=false|tether=false"
        ;;
    AGENTS)
        [[ $value == off ]] || { echo "AGENTS=$value: off" >&2; exit 2; }
        local unit stopped=()
        for unit in walinuxagent hv-kvp-daemon; do
            if systemctl is-active --quiet "$unit"; then
                sudo systemctl stop "$unit"
                stopped+=("$unit")
            fi
        done
        ((${#stopped[@]} > 0)) || { echo "AGENTS=off: no guest agent running here" >&2; exit 2; }
        save_host agents "${stopped[*]}"
        log "AGENTS=off: stopped ${stopped[*]}"
        ;;
    CLOCKSOURCE)
        local cs=/sys/devices/system/clocksource/clocksource0
        grep -qw -- "$value" "$cs/available_clocksource" || { echo "CLOCKSOURCE=$value: not among $(cat "$cs/available_clocksource")" >&2; exit 2; }
        save_host clocksource "$(cat "$cs/current_clocksource")"
        echo "$value" | sudo tee "$cs/current_clocksource" >/dev/null
        ;;
    *) return 1 ;;
    esac
}

# Every host setting an experiment changed goes back (`stop`), the data NICs'
# once the kernel has them again (a ring or interrupt moderation put back
# resets them: `stop`'s data_kernel_up then pins their IRQs again).
restore_host() {
    local f key st
    for f in "$run"/restore/*; do
        [[ -e $f ]] || continue
        key=${f##*/}
        case $key in
        CSTATE)
            for st in /sys/devices/system/cpu/cpu[0-9]*/cpuidle/state[0-9]*/disable; do
                echo 0 | sudo tee "$st" >/dev/null
            done
            ;;
        # shellcheck disable=SC2046
        coalesce-*) [[ -e /sys/class/net/${key#coalesce-} ]] && ethtool_set -C "${key#coalesce-}" $(cat "$f") || true ;;
        ring-*) [[ -e /sys/class/net/${key#ring-} ]] && ethtool_set -G "${key#ring-}" rx "$(cat "$f")" || true ;;
        min_free_kbytes) sudo sysctl -qw vm.min_free_kbytes="$(cat "$f")" ;;
        # shellcheck disable=SC2046
        agents) sudo systemctl start $(cat "$f") ;;
        sub_channel_params) rm -f "$run/sub_channel_params" ;;
        bin) rm -f "$run/bin" ;;
        clocksource) sudo tee /sys/devices/system/clocksource/clocksource0/current_clocksource <"$f" >/dev/null ;;
        esac
        rm -f "$f"
    done
}

case ${1:-} in
bootstrap)
    # x86 only: production runs on x86, so no number may come from anything else
    [[ $(uname -m) == x86_64 ]] || { echo "vm.sh: $(uname -m) host: the benchmarks run on x86_64 only" >&2; exit 2; }
    # Debian 13 with the trixie-backports kernel (MANA DPDK needs Linux 6.14+;
    # AWS gets the same kernel for a like-for-like comparison) and its headers
    # (AWS builds igb_uio against them), DPDK 24.11 and the build tools. The
    # backports kernel brings backports libelf, so libelf-dev comes from there too.
    echo 'deb http://deb.debian.org/debian trixie-backports main' | sudo tee /etc/apt/sources.list.d/backports.list >/dev/null
    sudo apt-get update -q
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -yq -t trixie-backports linux-image-cloud-amd64 linux-headers-cloud-amd64
    # perf, for profiling only (never fatal): the backports kernel's, else
    # trixie's (an older perf reads a newer kernel); a versioned binary
    # (perf_<version>) gets its plain name
    command -v perf >/dev/null ||
        sudo DEBIAN_FRONTEND=noninteractive apt-get install -yq -t trixie-backports linux-perf ||
        sudo DEBIAN_FRONTEND=noninteractive apt-get install -yq -t trixie linux-perf || true
    if ! command -v perf >/dev/null; then
        versioned=$(ls /usr/bin/perf_* 2>/dev/null | sort -V | tail -1) || true
        [[ -z $versioned ]] || sudo ln -sf "$versioned" /usr/local/bin/perf
    fi
    command -v perf >/dev/null || echo "vm.sh: no perf installed (profiling unavailable)" >&2
    if command -v perf >/dev/null; then perf --version; fi
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -yq build-essential pkg-config cmake clang libclang-dev \
        libbsd-dev uuid-dev default-jdk-headless git curl python3 ethtool dpdk dpdk-dev libdpdk-dev \
        libelf-dev/trixie-backports
    if [[ $CLOUD == azure ]]; then
        sudo DEBIAN_FRONTEND=noninteractive apt-get install -yq rdma-core ibverbs-providers libibverbs-dev
        echo mana_ib | sudo tee /etc/modules-load.d/mana_ib.conf >/dev/null
    fi
    if precision_time; then
        sudo DEBIAN_FRONTEND=noninteractive apt-get install -yq meson ninja-build python3-pyelftools libnuma-dev chrony
    fi
    command -v cargo >/dev/null || [[ -x ~/.cargo/bin/cargo ]] ||
        curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
    ~/.cargo/bin/rustup toolchain install "$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)" --profile minimal
    # 2 MiB hugepages for the DPDK EAL and the hugetlbfs AERON_DIR, every boot
    echo 'vm.nr_hugepages = 2048' | sudo tee /etc/sysctl.d/90-dpdk.conf >/dev/null
    # two NICs on one subnet: replies leave by the first, so loose reverse-path filtering
    printf 'net.ipv4.conf.all.rp_filter = 2\nnet.ipv4.conf.default.rp_filter = 2\nnet.core.rmem_max = 16777216\nnet.core.wmem_max = 16777216\n' |
        sudo tee /etc/sysctl.d/90-bench.conf >/dev/null
    # the data NICs are the lab's alone, from the next boot on (`isolate`)
    data_unmanaged
    ;;
build)
    export PATH=$HOME/.cargo/bin:$PATH
    precision_time && "$self" build-ptp
    cargo build --release -p ergon-md-dpdk --features static,dpdk --bin media_driver
    cargo build --release -p ergon-md-dpdk-harness --features static --bin rtt --bin load --bin tput --bin counters --bin clockcheck
    if [[ $CLOUD == aws ]]; then
        # No IOMMU on virtualised EC2 and no write-combining in mainline
        # vfio-pci, which ENA's low-latency queues need: Amazon's documented
        # route is igb_uio with wc_activate=1, built for the running kernel.
        [[ -d $HOME/dpdk-kmods ]] || git clone -q https://dpdk.org/git/dpdk-kmods "$HOME/dpdk-kmods"
        make -s -C "$HOME/dpdk-kmods/linux/igb_uio" >/dev/null
    fi
    ;;
build-base)
    # lab.sh LAB_BASE: the revision lib.sh unpacked in ~/ergon-base (its
    # tracked files, from git archive), built as `build` builds this checkout,
    # and its media driver and benchmark clients installed in ~/bin-base, for
    # modes with @BIN=bin-base. A client the revision lacks is left out.
    export PATH=$HOME/.cargo/bin:$PATH
    cd "$HOME/ergon-base" 2>/dev/null || { echo "build-base: no ~/ergon-base (lab.sh LAB_BASE)" >&2; exit 2; }
    if [[ -f rust-toolchain.toml ]]; then
        rustup toolchain install "$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)" --profile minimal
    fi
    cargo build --release -p ergon-md-dpdk --features static,dpdk --bin media_driver
    clients=()
    for c in rtt load tput; do
        if [[ -f md-dpdk/harness/src/bin/$c.rs ]]; then clients+=(--bin "$c"); fi
    done
    if ((${#clients[@]})); then
        cargo build --release -p ergon-md-dpdk-harness --features static "${clients[@]}"
    fi
    rm -rf "$HOME/bin-base" && mkdir -p "$HOME/bin-base"
    for b in media_driver rtt load tput; do
        if [[ -x target/release/$b ]]; then cp "target/release/$b" "$HOME/bin-base/"; fi
    done
    echo "bin-base $(ls "$HOME/bin-base" | tr '\n' ' ')"
    ;;
build-ptp)
    # precision-time only, run by `build` before the media driver.
    # DPDK 25.11: only what the ENA path needs, installed under $DPDK_PREFIX.
    if [[ ! -f $DPDK_PREFIX/lib/x86_64-linux-gnu/pkgconfig/libdpdk.pc ]]; then
        curl -fsSL "https://fast.dpdk.org/rel/dpdk-$DPDK_VERSION.tar.xz" | tar -xJ -C "$HOME"
        (cd "$HOME/dpdk-$DPDK_VERSION" &&
            meson setup -Dprefix="$DPDK_PREFIX" -Dtests=false -Ddisable_apps='*' \
                -Denable_drivers=bus/pci,bus/vdev,mempool/ring,net/ena build >/dev/null &&
            ninja -C build >/dev/null && sudo ninja -C build install >/dev/null)
        echo "$DPDK_PREFIX/lib/x86_64-linux-gnu" | sudo tee /etc/ld.so.conf.d/dpdk-$DPDK_VERSION.conf >/dev/null
        sudo ldconfig
    fi
    # Amazon's ENA driver with the PTP hardware clock, loaded with it on from
    # the next boot (lab.sh reboots); the in-tree driver it replaces has no PHC.
    [[ -d $HOME/amzn-drivers ]] || git clone -q https://github.com/amzn/amzn-drivers "$HOME/amzn-drivers"
    make -s -C "$HOME/amzn-drivers/kernel/linux/ena" ENA_PHC_INCLUDE=1 >/dev/null
    sudo install -D -m 644 "$HOME/amzn-drivers/kernel/linux/ena/ena.ko" "/lib/modules/$(uname -r)/updates/ena.ko"
    sudo depmod -a
    echo 'options ena phc_enable=1' | sudo tee /etc/modprobe.d/ena-phc.conf >/dev/null
    sudo update-initramfs -u >/dev/null 2>&1
    # /dev/ptp_ena for chrony, which keeps CLOCK_REALTIME on the PHC: the clock
    # the transport compares NIC RX timestamps against
    echo 'SUBSYSTEM=="ptp", ATTR{clock_name}=="ena-ptp-*", SYMLINK += "ptp_ena"' |
        sudo tee /etc/udev/rules.d/53-ec2-network-interfaces-ptp.rules >/dev/null
    grep -q '^refclock PHC /dev/ptp_ena' /etc/chrony/chrony.conf ||
        echo 'refclock PHC /dev/ptp_ena poll 0 delay 0.000010 prefer' | sudo tee -a /etc/chrony/chrony.conf >/dev/null
    sudo systemctl enable chrony >/dev/null 2>&1
    ;;
ptp)
    if ! precision_time; then echo "ptp: not a precision-time run"; exit 0; fi
    echo "ptp $(readlink -f /dev/ptp_ena 2>/dev/null || echo missing) $(cat /sys/class/ptp/ptp*/clock_name 2>/dev/null | tr '\n' ' ')"
    echo "ena $(modinfo -F version ena 2>/dev/null) phc_enable=$(cat /sys/module/ena/parameters/phc_enable 2>/dev/null)"
    chronyc -n sources 2>/dev/null | sed 's/^/chrony /'
    chronyc tracking 2>/dev/null | sed -n 's/^\(Reference ID\|System time\|RMS offset\) *: */chrony \1: /p'
    nics | while read -r role iface _; do echo "$role $(ethtool -T "$iface" 2>/dev/null | grep -c hardware-receive) hardware-receive"; done
    ;;
isolate)
    # rusteron's "isolated" state: every CPU but the housekeeping one off the
    # scheduler's balancing, the tick, RCU callbacks and managed IRQs
    last=$(($(nproc) - 1))
    # AWS metal has a real IOMMU, which igb_uio does not program: passthrough
    # (identity mapping for host-owned devices) lets its DMA through. A
    # virtualised instance has none, so it changes nothing there.
    iommu=
    [[ $CLOUD == aws ]] && iommu=" iommu=pt"
    sudo mkdir -p /etc/default/grub.d
    echo "GRUB_CMDLINE_LINUX_DEFAULT=\"\$GRUB_CMDLINE_LINUX_DEFAULT isolcpus=nohz,domain,managed_irq,1-$last nohz_full=1-$last rcu_nocbs=1-$last irqaffinity=0$iommu\"" |
        sudo tee /etc/default/grub.d/99-ergon-isolation.cfg >/dev/null
    sudo update-grub >/dev/null 2>&1
    ;;
tune)
    # the data NICs up with their addresses and routing (unmanaged by the OS,
    # so nothing else does it at boot), before their IRQs are placed; a VF
    # not there yet is left to the first kernel run, which does it again
    data_kernel_up || echo "tune: data NICs not configured yet; the first kernel run will" >&2
    networkctl list --no-pager 2>/dev/null | sed 's/^/networkd /' || true
    irqs_pin "$HK_CPU" tune
    echo 0 | sudo tee /sys/kernel/mm/ksm/run >/dev/null 2>&1 || true
    sudo sysctl -qw kernel.numa_balancing=0 2>/dev/null || true
    # the baseline's transparent hugepages, from every boot on
    echo "$BASE_THP" | sudo tee /sys/kernel/mm/transparent_hugepage/enabled >/dev/null
    sudo mkdir -p /mnt/huge
    mountpoint -q /mnt/huge || sudo mount -t hugetlbfs -o pagesize=2M,size=1G none /mnt/huge
    echo "cloud $CLOUD layout $(layout) cpus $(nproc)"
    echo "cmdline $(cat /proc/cmdline)"
    echo "isolated $(cat /sys/devices/system/cpu/isolated) nohz_full $(cat /sys/devices/system/cpu/nohz_full 2>/dev/null)"
    echo "clocksource $(cat /sys/devices/system/clocksource/clocksource0/current_clocksource)"
    echo "thp $(cat /sys/kernel/mm/transparent_hugepage/enabled)"
    ;;
baseline)
    # what every mode runs on top of, and what is in effect now
    echo "baseline thp=$BASE_THP (now: $(cat /sys/kernel/mm/transparent_hugepage/enabled)); dpdk modes: AERON_RECEIVER_IO_VECTOR_CAPACITY=$BASE_RCV_IO_VECTORS"
    ;;
layout) layout ;;
nics) nics ;;
driver)
    mode=${2:?kernel, kernel-busy, dpdk or dpdk-spin, optionally -mono, then @KEY=value overrides}
    "$self" stop
    # `stop` removed any earlier run's BIN: this run's own, if any, follows
    bin=${BENCH_BIN:-$tools}
    env_file=$run/driver.env
    common_env >"$env_file"
    # <mode>@KEY=value@...: environment overrides for one experiment, applied
    # last so they win (e.g. dpdk@RUSTERON_DPDK_BURST_SIZE=32)
    overrides=
    if [[ $mode == *@* ]]; then
        overrides=${mode#*@}
        mode=${mode%%@*}
    fi
    clock=tsc
    if [[ $mode == *-mono ]]; then clock=monotonic; mode=${mode%-mono}; fi
    echo "export RUSTERON_DPDK_CLOCK='$clock'" >>"$env_file"
    echo "$clock" >"$run/bench_clock"
    # dpdk-spin: the DPDK driver with its agents reading host-local kernel
    # sockets every duty cycle, not when the local-I/O thread flags them
    if [[ $mode == dpdk-spin || $mode == dpdk-adaptive || $mode == dpdk-epoll ]]; then
        echo "export RUSTERON_DPDK_LOCAL_IO='${mode#dpdk-}'" >>"$env_file"
        mode=dpdk
    fi
    case $mode in
    kernel | kernel-busy)
        data_kernel_up
        busy=0
        [[ $mode == kernel-busy ]] && busy=50
        sudo sysctl -qw net.core.busy_read=$busy net.core.busy_poll=$busy
        nics | while read -r _ _ vf _; do
            if [[ $mode == kernel-busy ]]; then defer=2 gro=200000; else defer=0 gro=0; fi
            echo $defer | sudo tee /sys/class/net/"$vf"/napi_defer_hard_irqs >/dev/null
            echo $gro | sudo tee /sys/class/net/"$vf"/gro_flush_timeout >/dev/null
        done
        ;;
    dpdk)
        if [[ $CLOUD == azure ]]; then
            sudo modprobe mana_ib
            sudo modprobe uio_hv_generic
            # the network-device class GUID, so uio_hv_generic may bind NICs
            echo f8615163-df3e-46c5-913f-f2d2f965ed0e | sudo tee /sys/bus/vmbus/drivers/uio_hv_generic/new_id >/dev/null 2>&1 || true
        else
            sudo modprobe uio
            lsmod | grep -q '^igb_uio' || sudo insmod "$HOME/dpdk-kmods/linux/igb_uio/igb_uio.ko" wc_activate=1
        fi
        {
            echo "export RUSTERON_MEDIA_DRIVER_TRANSPORT='dpdk-ena'"
            echo "export RUSTERON_DPDK_FILE_PREFIX='ergon-bench'"
            # the baseline: the receiver drains up to this many frames a duty
            # cycle (an @AERON_RECEIVER_IO_VECTOR_CAPACITY=n override, later, wins)
            echo "export AERON_RECEIVER_IO_VECTOR_CAPACITY='$BASE_RCV_IO_VECTORS'"
            # the EAL's only lcore is the housekeeping CPU: no worker threads
            # elsewhere. On AWS the ENA PMD's and the PCI bus's probe messages
            # at info: whether the low-latency queues and write-combining took
            # (vm.sh pmd-log)
            eal="-l $HK_CPU"
            if [[ $CLOUD == aws ]]; then eal="$eal --log-level=pmd.net.ena.*:info --log-level=bus.pci:info"; fi
            echo "export RUSTERON_DPDK_EAL_ARGS='$eal'"
            if [[ $CLOUD == azure ]]; then
                # MANA queues hold at most 256 descriptors
                # (LAB_RX_DESC: a smaller ring, for loss experiments)
                echo "export RUSTERON_DPDK_RX_DESCRIPTORS='${LAB_RX_DESC:-256}'"
                echo "export RUSTERON_DPDK_TX_DESCRIPTORS='256'"
            fi
            echo "export RUSTERON_DPDK_MBUFS_PER_PORT='16384'"
            precision_time && echo "export RUSTERON_DPDK_RX_HW_TIMESTAMP='1'"
            data_nics=$(nics)
            while read -r role iface vf mac ip prefix bdf; do
                R=${role^^}
                gw=$(python3 -c 'import ipaddress,sys; print(next(ipaddress.ip_network(sys.argv[1], strict=False).hosts()))' "$ip/$prefix")
                echo "export RUSTERON_DPDK_${R}_PCI='$bdf'"
                echo "export RUSTERON_DPDK_${R}_IPV4_CIDR='$ip/$prefix'"
                echo "export RUSTERON_DPDK_${R}_GATEWAY='$gw'"
                sudo ip link set "$iface" down
                if [[ $CLOUD == azure ]]; then
                    # The netvsc PMD takes the NIC's synthetic channel (moved to
                    # uio_hv_generic) and drives the MANA VF beneath it, keeping
                    # the VF data path switched in; the port is selected by MAC.
                    echo "export RUSTERON_DPDK_${R}_MAC='$mac'"
                    dev=$(basename "$(readlink /sys/class/net/"$iface"/device)")
                    sudo ip link set "$vf" down
                    echo "$dev" | sudo tee /sys/bus/vmbus/drivers/hv_netvsc/unbind >/dev/null
                    echo "$dev" | sudo tee /sys/bus/vmbus/drivers/uio_hv_generic/bind >/dev/null
                else
                    # The ENA moves from the kernel's ena driver to igb_uio.
                    echo igb_uio | sudo tee /sys/bus/pci/devices/"$bdf"/driver_override >/dev/null
                    echo "$bdf" | sudo tee /sys/bus/pci/devices/"$bdf"/driver/unbind >/dev/null
                    echo "$bdf" | sudo tee /sys/bus/pci/drivers_probe >/dev/null
                fi
            done <<<"$data_nics"
        } >>"$env_file"
        ;;
    *) echo "unknown mode $mode" >&2; exit 2 ;;
    esac
    if [[ -n $overrides ]]; then
        IFS=@ read -ra pairs <<<"$overrides"
        for pair in "${pairs[@]}"; do
            [[ $pair =~ ^[A-Z][A-Z0-9_]*=[^\'[:space:]]*$ ]] || { echo "bad override $pair" >&2; exit 2; }
            # a host setting (host_setting) for this run, which `stop` undoes;
            # anything else is the driver's environment
            host_setting "${pair%%=*}" "${pair#*=}" "$mode" && continue
            echo "export ${pair%%=*}='${pair#*=}'" >>"$env_file"
        done
    fi
    # what this run ran on, for its notes: the baseline as in effect now
    # (after any override) and whose binaries
    log "driver: $mode${overrides:+ with $overrides} (thp $(sed 's/.*\[\(.*\)\].*/\1/' /sys/kernel/mm/transparent_hugepage/enabled), receiver IO vectors $(sed -n "s/^export AERON_RECEIVER_IO_VECTOR_CAPACITY='\(.*\)'$/\1/p" "$env_file" | tail -1 | grep . || echo "Aeron's default"), binaries $bin)"
    # Every IRQ on the housekeeping CPU once more, after everything above
    # that may have placed them anew (the NICs back from DPDK, a ring or
    # moderation change), and where each is now: the run's own record
    irqs_pin "$HK_CPU" "before the driver starts"
    # The EAL needs root (hugepages, the device), so the driver runs as root in
    # every mode, and the clients (`vm.sh client`) do too, to map its files.
    sudo rm -rf "$AERON_DIR"
    # confined to the housekeeping CPU: every thread that is not an agent
    # (signal handler, Aeron's, the EAL's) stays there; the agents pin themselves
    sudo bash -c ". '$env_file' && exec taskset -c $HK_CPU '$bin/media_driver'" >"$run/driver.log" 2>&1 &
    pid=$!
    for _ in $(seq 120); do
        grep -q "media driver started" "$run/driver.log" 2>/dev/null && break
        if ! ps -p "$pid" >/dev/null; then cat "$run/driver.log" >&2; exit 1; fi
        sleep 0.5
    done
    grep -q "media driver started" "$run/driver.log" || { cat "$run/driver.log" >&2; exit 1; }
    grep "transport backend" "$run/driver.log" >&2
    ;;
client)
    # vm.sh client <label> <rtt|tput|load> <args...>: a benchmark client against the running driver
    label=${2:?label}
    prog=${3:?rtt or tput}
    shift 3
    [[ $prog == rtt && ${1:-} == xping || $prog == load && ${1:-} != xecho ]] && rm -f "$run/load-spikes.csv"
    [[ $prog == rtt && ${1:-} == xping ]] && rm -f "$run/stalls-ping.csv"
    [[ $prog == rtt && ${1:-} == xpong ]] && rm -f "$run/stalls-pong.csv"
    # a build under BIN without this client: this checkout's, said in its log
    exe=$bin/$prog
    if [[ ! -x $exe ]]; then
        log "client: no $prog in $bin: this checkout's"
        exe=$tools/$prog
    fi
    # started on the housekeeping CPU, so the client's conductor stays there;
    # the measured thread then pins itself to its own CPU (lab.sh passes the app CPU)
    sudo env AERON_DIR="$AERON_DIR" LABEL="$label" RTT_SECONDS="${RTT_SECONDS:-20}" \
        SUB_CHANNEL_PARAMS="$(cat "$run/sub_channel_params" 2>/dev/null || true)" \
        RTT_LOST_TIMEOUT_US="$([[ -s $run/sub_channel_params ]] && echo 50000 || echo 0)" \
        RTT_HISTOGRAM="$run/rtt-histogram.csv" LOOP_STALLS_DIR="$run" LOAD_SPIKES="$run/load-spikes.csv" LOAD_SPIKE_NS="${LOAD_SPIKE_NS:-100000}" BENCH_CLOCK="$(cat "$run/bench_clock" 2>/dev/null || echo tsc)" AERON_TERM_BUFFER_SPARSE_FILE=false AERON_CLIENT_PRE_TOUCH_MAPPED_MEMORY=true \
        taskset -c "$HK_CPU" "$exe" "$@"
    ;;
stop)
    sudo pkill -INT -x media_driver 2>/dev/null || true
    for _ in $(seq 40); do pgrep -x media_driver >/dev/null || break; sleep 0.25; done
    sudo pkill -KILL -x media_driver 2>/dev/null || true
    data_return_to_kernel
    restore_host
    data_kernel_up
    sudo sysctl -qw net.core.busy_read=0 net.core.busy_poll=0
    # the baseline again, after an experiment's @THP= (THP_DEFAULT: by hand)
    echo "${THP_DEFAULT:-$BASE_THP}" | sudo tee /sys/kernel/mm/transparent_hugepage/enabled >/dev/null || true
    ;;
loss)
    line=
    if pgrep -x media_driver >/dev/null; then
        line=$(sudo env AERON_DIR="$AERON_DIR" timeout 20 "$tools/counters" loss 2>/dev/null) || line=aeron-read-failed=1
    fi
    snmp=$(awk '$1 == "Udp:" { if (!seen) { for (i = 2; i <= NF; i++) name[i] = $i; seen = 1; next }
        for (i = 2; i <= NF; i++) if (name[i] ~ /^(InErrors|RcvbufErrors|SndbufErrors)$/) printf "kernel-udp-%s=%s;", name[i], $i }' /proc/net/snmp)
    drops=0 nic_ids=()
    while read -r _ iface vf _ _ _ bdf; do
        nic_ids+=("$iface,$vf,$bdf")
        for dev in "$iface" "$vf"; do
            for stat in rx_dropped rx_missed_errors rx_fifo_errors; do
                f=/sys/class/net/$dev/statistics/$stat
                [[ -n $dev && -r $f ]] && drops=$((drops + $(cat "$f")))
            done
        done
    done < <(nics 2>/dev/null)
    # then a check on the run, not loss: the data NICs' interrupts on any CPU
    # but housekeeping's (their IRQs placed anew and not pinned again:
    # irqs.sh). Under DPDK the kernel still holds an IRQ on each (on AWS
    # igb_uio's, the ENA PMD's admin queue's; on Azure MANA's own), found by
    # the PCI addresses the driver was given. Left out, never 0, unless both
    # NICs are found and each matches an IRQ (analyse.py then reports the run
    # unread); a failed read never costs the loss counters their line
    if dpdk_active; then
        mapfile -t nic_ids < <(sed -nE "s/^export RUSTERON_DPDK_(SENDER|RECEIVER)_PCI='(.+)'$/\2/p" "$run/driver.env" 2>/dev/null)
    fi
    nic_irqs=
    if ((${#nic_ids[@]} == 2)); then
        nic_irqs=$(irqs_nic_off "$HK_CPU" "${nic_ids[@]}") || nic_irqs=
    fi
    [[ $nic_irqs =~ ^[0-9]+$ ]] || { echo "loss: the data NICs' interrupts not read" >&2; nic_irqs=; }
    echo "${line:+$line;}${snmp}nic-kernel-drops=$drops;routes-missing=$(data_routes_missing)${nic_irqs:+;isolated-nic-irqs=$nic_irqs}"
    ;;
errors)
    # the driver's distinct error log and loss report (from its CnC file, also
    # once it has stopped): what Aeron's errors counter and NAKs were about
    sudo env AERON_DIR="$AERON_DIR" timeout 20 "$tools/counters" errors 2>&1 || true
    ;;
pmd-log)
    # After a DPDK run: the PMD's and EAL's lines on how the NIC was set up
    # (on AWS at info, `driver`'s EAL arguments): ENA's placement policy (its
    # low-latency queues or host memory), LLQ, the queue sizes, and whether
    # its BAR could be mapped write-combining
    grep -E 'Placement policy|LLQ|tx queue size|cannot be mapped|write combin|WC' "$run/driver.log" 2>/dev/null ||
        echo "pmd-log: no such line in the driver's log"
    ;;
wc-check)
    # AWS, during a DPDK run: ENA's low-latency queues need its memory BAR
    # (BAR 2) mapped write-combining. The kernel offers a prefetchable BAR as
    # resource2_wc, which DPDK maps for ENA when it is there; igb_uio with
    # wc_activate=1 (`driver` loads it so) keeps no uncached mapping of the
    # BAR, to whose type PAT would demote DPDK's. Per data NIC: the file,
    # PAT's memory type for the BAR, and the media driver's mapping of it;
    # then igb_uio's kernel messages.
    if [[ $CLOUD != aws ]]; then
        echo "wc-check: AWS (ENA) only"
        exit 0
    fi
    pid=$(pgrep -x media_driver | head -1 || true)
    mountpoint -q /sys/kernel/debug || sudo mount -t debugfs none /sys/kernel/debug 2>/dev/null || true
    pat=/sys/kernel/debug/x86/pat_memtype_list
    sudo test -r "$pat" || pat=
    echo "wc igb_uio $(lsmod | awk '$1 == "igb_uio" {f = 1} END {print f ? "loaded" : "not loaded"}') wc_activate=$(cat /sys/module/igb_uio/parameters/wc_activate 2>/dev/null || echo n/a)"
    while read -r role bdf; do
        dev=/sys/bus/pci/devices/$bdf
        start= end=
        read -r start end _ < <(sed -n 3p "$dev/resource" 2>/dev/null) || true
        echo "wc $role $bdf driver=$(basename "$(readlink "$dev/driver" 2>/dev/null || echo none)") resource2_wc=$([[ -e $dev/resource2_wc ]] && echo yes || echo no) bar2=${start:-?}-${end:-?}"
        if [[ -n $pat && -n $start ]]; then
            echo "wc $role pat $({ sudo grep -i -- "$start" "$pat" || echo "no entry at $start"; } | head -3 | tr '\n' ' ')"
        else
            echo "wc $role pat unavailable (no pat_memtype_list)"
        fi
        maps="none (no media driver running)"
        if [[ -n $pid ]]; then
            maps=$(sudo grep -F "$bdf/resource" "/proc/$pid/maps" | awk '{printf "%s %s %s; ", $1, $2, $6}' || true)
        fi
        echo "wc $role maps ${maps:-none}"
    done < <(sed -nE "s/^export RUSTERON_DPDK_(SENDER|RECEIVER)_PCI='(.*)'$/\1 \2/p" "$run/driver.env" 2>/dev/null | tr 'A-Z' 'a-z')
    sudo dmesg | { grep -iE 'igb_uio|wc_activate' || true; } | tail -20 | sed 's/^/wc dmesg /'
    ;;
counters)
    # Validity. Under DPDK, the driver's mirrored PMD stats: on Azure the VF's
    # packets (vf_*) must equal the totals; on AWS the ENA allowance counters
    # must stay at zero. In a kernel mode, the same from ethtool. On Azure, the
    # kernel's count of VF data-path switches.
    # Frames per message (lib.sh snapshots these around each flat-out run):
    # under DPDK the transport's own frames (pkts) and bytes, its sends a full
    # ring refused back to Aeron (tx-eagain) or queued to go first
    # (tx-queued), and the ENA's per-queue TX statistics (type 92, named by
    # the PMD: packets, doorbells, completion polls, free descriptors); in a
    # kernel mode the data NICs' packet counts. On receive: frames whose UDP
    # checksum the transport verified in software (the NIC did not), the
    # deepest backlog one poll drained, and the polls that nearly emptied a
    # full ring.
    pattern=' (vf_)?(rx|tx)_good_packets$|allowance_exceeded$|dir=- clock$| dir=(rx|tx) pkts$| dir=tx (bytes|tx-eagain|tx-queued)$| dir=rx (sw-checksum-verified|rx-backlog-max|rx-ring-full-suspected)$| tx_q[0-9]+_(cnt|doorbells|tx_poll|available_desc)$'
    if pgrep -x media_driver >/dev/null && dpdk_active; then
        # Every counter (the PMD's are labelled with their bare names, so
        # each takes the role of the port listed before it: a port registers
        # its own counters, then its PMD's), filtered to the pattern
        { sudo env AERON_DIR="$AERON_DIR" "$tools/counters" '' | awk -v pat="$pattern" '
            / rusteron-dpdk role=/ {match($0, /role=[a-z]+/); role = substr($0, RSTART, RLENGTH)}
            $2 == 92 && role != "" {$3 = role " " $3}
            $0 ~ pat' | tr -s ' ' | tr '\n' ';'; } || true
        echo
    elif [[ $CLOUD == azure ]]; then
        # netvsc's own stats split the VF path (vf_*) from the synthetic one (per queue)
        nics | while read -r role iface _; do
            echo "$role $(ethtool -S "$iface" 2>/dev/null | awk '
                $1 == "vf_rx_packets:" || $1 == "vf_tx_packets:" {printf "%s %s ", $1, $2}
                $1 ~ /^(rx|tx)_queue_[0-9]+_packets:$/ {synthetic += $2}
                END {printf "synthetic_packets: %d", synthetic}') $(nic_packets "$iface")"
        done
    else
        nics | while read -r role iface _; do
            echo "$role $(ethtool -S "$iface" 2>/dev/null | awk '$1 ~ /allowance_exceeded:$/ {printf "%s %s ", $1, $2}')$(nic_packets "$iface")"
        done
    fi
    [[ $CLOUD == azure ]] && echo "path-switches $(sudo dmesg | grep -ci 'data path switched' || true)"
    true
    ;;
jitter)
    # Where a spike can come from: each media-driver thread's allowed CPUs and
    # context switches (nvcsw: preempted), and on each measured CPU the
    # interrupts taken and the hypervisor's steal (ticks). Before/after a run.
    pid=$(pgrep -x media_driver | head -1)
    if [[ -n $pid ]]; then
        # RUSTERON_MEDIA_DRIVER_MLOCKALL=1 shows here as locked memory
        echo "driver $(sudo grep -hE '^(VmLck|VmRSS)' /proc/"$pid"/status 2>/dev/null | tr -s ' \t\n' ' ')"
        for t in /proc/"$pid"/task/*; do
            sudo awk -v comm="$(sudo cat "$t/comm" 2>/dev/null)" -v tid="${t##*/}" '
                /^Cpus_allowed_list:/ {c = $2} /^voluntary_ctxt_switches:/ {v = $2} /^nonvoluntary_ctxt_switches:/ {n = $2}
                END {printf "thread %s/%s cpus=%s vcsw=%s nvcsw=%s\n", comm, tid, c, v, n}' "$t/status" 2>/dev/null
        done
    fi
    # the agents' own stalls, per port (DPDK only): gaps between polls that
    # found frames (rx-poll-gap*) and between any polls (poll-cycle-gap*)
    if [[ -n $pid ]] && dpdk_active; then
        sudo env AERON_DIR="$AERON_DIR" timeout 20 "$tools/counters" -gap 2>/dev/null | sed 's/^/counter /'
    fi
    for cpu in $HK_CPU $APP_CPU $SENDER_CPU $RECEIVER_CPU $APP2_CPU; do
        irqs=$(awk -v col=$((cpu + 2)) 'NR > 1 && NF >= col {s += $col} END {print s + 0}' /proc/interrupts)
        loc=$(awk -v col=$((cpu + 2)) '$1 == "LOC:" {print $col}' /proc/interrupts)
        # the IPIs that can stop a spinning CPU: irq_work (a nohz_full tick
        # restarted, e.g. for RCU), rescheduling, function calls, TLB shootdowns
        ipis=$(awk -v col=$((cpu + 2)) '$1 ~ /^(IWI|RES|CAL|TLB):$/ {printf " %s=%s", tolower(substr($1, 1, 3)), $col}' /proc/interrupts)
        steal=$(awk -v c="cpu$cpu" '$1 == c {print $9}' /proc/stat)
        echo "cpu $cpu irqs=$irqs timer=$loc$ipis steal=$steal"
    done
    ;;
clockcheck)
    taskset -c "$APP_CPU" "$tools/clockcheck" "${2:-60}"
    ;;
preflight)
    # vm.sh preflight [seconds]: before any benchmark, spin on every CPU but
    # housekeeping's and report its stalls of 50 us or more (count, how many
    # of 100 us or more, longest), then how many stalls of 100 us or more hit
    # two or more CPUs within a millisecond (the whole VM paused: the host).
    # A host that stalls is worth redeploying before hours are spent on it.
    # (`nproc --all`: once isolated, a login's own CPU mask is housekeeping's.)
    gcc -O2 -o "$run/gapspin" "$(dirname "$self")/gapspin.c" || exit 1
    secs=${2:-120}
    pids=()
    for cpu in $(seq 0 $(($(nproc --all) - 1))); do
        [[ $cpu == "$HK_CPU" ]] && continue
        "$run/gapspin" "$cpu" 50 "$secs" >"$run/preflight-$cpu.log" & pids+=($!)
    done
    wait "${pids[@]}"
    python3 - "$run" <<'PY'
import glob, os, sys
gaps = []
for path in sorted(glob.glob(os.path.join(sys.argv[1], "preflight-*.log")), key=lambda p: int(p.rsplit("-", 1)[1][:-4])):
    cpu = int(path.rsplit("-", 1)[1][:-4])
    rows = [l.split() for l in open(path) if not l.startswith("#")]
    us = [float(r[1]) for r in rows]
    gaps += [(float(r[0]), float(r[1]), cpu) for r in rows if float(r[1]) >= 100]
    print(f"preflight cpu {cpu} gaps50={len(us)} gaps100={sum(u >= 100 for u in us)} max_us={max(us, default=0):.0f}")
gaps.sort()
# CPUs that stalled 300 us or more: the layout keeps the run's threads off
# their cores (vm.sh layout)
noisy = sorted({c for _, us, c in gaps if us >= 300})
open(os.path.join(sys.argv[1], "noisy_cpus"), "w").write(" ".join(map(str, noisy)) + "\n")
print(f"preflight noisy_cpus {' '.join(map(str, noisy)) or 'none'} (stalls of 300 us or more; seconds of the minute: "
      + ", ".join(sorted({f':{int(t % 60):02d}' for t, us, _ in gaps if us >= 300})) + ")")
shared = 0
i = 0
while i < len(gaps):
    j = i
    while j + 1 < len(gaps) and gaps[j + 1][0] - gaps[i][0] < 0.001:
        j += 1
    if len({g[2] for g in gaps[i:j + 1]}) > 1:
        shared += 1
    i = j + 1
print(f"preflight stalls100={len(gaps)} on_several_cpus={shared}")
PY
    ;;
diag-start)
    # For the whole state: the gap spinner on a spare CPU (the first core
    # outside the layout, else housekeeping's SMT twin) and a trace of the app
    # and agent CPUs (context switches, work items, IRQs), plus who calls
    # lru_add_drain_all (every CPU), so each spike can be told apart:
    # a guest preemption (traced), a host stall (the spinner too), or neither.
    gcc -O2 -o "$run/gapspin" "$(dirname "$self")/gapspin.c" || exit 1
    spare=
    used=" $HK_CPU $APP_CPU $SENDER_CPU $RECEIVER_CPU $APP2_CPU "
    for cpu in $(seq 1 $(($(nproc --all) - 1))); do
        first=$(sed 's/[,-].*//' /sys/devices/system/cpu/cpu"$cpu"/topology/thread_siblings_list)
        [[ $first == "$cpu" && $used != *" $cpu "* ]] && { spare=$cpu; break; }
    done
    [[ -n $spare ]] || spare=$(sed 's/.*[,-]//' /sys/devices/system/cpu/cpu"$HK_CPU"/topology/thread_siblings_list)
    pkill -x gapspin 2>/dev/null || true
    nohup "$run/gapspin" "$spare" 50 >"$run/diag-gapspin.log" 2>&1 </dev/null &
    # every change to addresses, routes, rules and links, timestamped: who
    # took a data NIC's route away, if anything does
    if [[ -s $run/diag-ipmon.pid ]]; then kill "$(cat "$run/diag-ipmon.pid")" 2>/dev/null || true; fi
    nohup ip -ts monitor address route rule link >"$run/diag-ipmon.log" 2>&1 </dev/null &
    echo $! >"$run/diag-ipmon.pid"
    mask=0
    for cpu in $APP_CPU $SENDER_CPU $RECEIVER_CPU $APP2_CPU $spare; do mask=$((mask | (1 << cpu))); done
    sudo bash -c "cd /sys/kernel/tracing && echo 0 > tracing_on && echo > trace && printf '%x' $mask > tracing_cpumask &&
        echo 8192 > buffer_size_kb && echo 1 > events/sched/sched_switch/enable &&
        echo 1 > events/workqueue/workqueue_execute_start/enable && echo 1 > events/irq/irq_handler_entry/enable &&
        for e in call_function_entry call_function_single_entry reschedule_entry; do
            [ -d events/irq_vectors/\$e ] && echo 1 > events/irq_vectors/\$e/enable; done;
        { grep -q drainall kprobe_events || echo 'p:drainall lru_add_drain_all' >> kprobe_events; } &&
        for k in text_poke_bp_batch synchronize_rcu_expedited; do
            grep -qw \$k available_filter_functions && { grep -q \"p:\$k \" kprobe_events || echo \"p:\$k \$k\" >> kprobe_events; }; done;
        mkdir -p instances/drain && echo 1 > instances/drain/events/kprobes/enable && echo 1 > instances/drain/options/stacktrace &&
        echo 1 > tracing_on"
    echo "diag spinner cpu $spare, traced cpus $(printf '%x' $mask)"
    ;;
diag-fetch)
    # the spinner's gaps, then the trace with the uptime and wall clock it
    # counts against (to put its events on the wall clock), then the drains
    echo "== gapspin"
    cat "$run/diag-gapspin.log" 2>/dev/null
    echo "== trace"
    sudo grep -E 'sched_switch|workqueue_execute_start|irq_handler_entry|call_function|reschedule_entry' /sys/kernel/tracing/trace
    echo "UPTIME $(cut -d' ' -f1 /proc/uptime) REALTIME $(date +%s.%N)"
    echo "== drains"
    # who drains LRU batches, patches kernel text (a static key flipped on
    # every CPU) or waits for an expedited RCU grace period, with their stacks
    sudo grep -v '^#' /sys/kernel/tracing/instances/drain/trace 2>/dev/null
    echo "== ipmon"
    cat "$run/diag-ipmon.log" 2>/dev/null
    ;;
diag-stop)
    pkill -x gapspin 2>/dev/null || true
    if [[ -s $run/diag-ipmon.pid ]]; then kill "$(cat "$run/diag-ipmon.pid")" 2>/dev/null || true; rm -f "$run/diag-ipmon.pid"; fi
    echo 0 | sudo tee /sys/kernel/tracing/tracing_on >/dev/null
    ;;
criterion)
    # the same features as `build`: cargo also builds the package's binaries
    # for a bench, and a different feature set would relink media_driver
    export PATH=$HOME/.cargo/bin:$PATH
    cargo bench -q -p ergon-md-dpdk --features static,dpdk --bench clock --no-run 2>/dev/null
    taskset -c "$APP_CPU" cargo bench -q -p ergon-md-dpdk --features static,dpdk --bench clock 2>&1 |
        grep -E '^(clock_read|send_32b|poll_32b)/|time:|change:|no invariant'
    ;;
rx-hw-delay)
    # the receiver port's NIC-arrival-to-dispatch counters (type 81), as
    # "<mean ns>,<max ns>,<frames>"; nothing without NIC RX timestamps
    pgrep -x media_driver >/dev/null || exit 0
    sudo env AERON_DIR="$AERON_DIR" "$tools/counters" 'role=receiver' | awk '
        / rx-hw-delay-max-ns$/ {max = $1} / rx-hw-delay-sum-ns$/ {sum = $1} / rx-hw-timestamped$/ {n = $1}
        END {if (n > 0) printf "%.0f,%d,%d\n", sum / n, max, n}'
    ;;
*)
    awk 'NR > 1 && !/^#/ {exit} NR > 1 {print}' "$self" >&2
    exit 2
    ;;
esac
