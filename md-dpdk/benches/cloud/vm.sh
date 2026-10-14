#!/usr/bin/env bash
# One VM's side of md-dpdk/benches/cloud/{azure,aws}/lab.sh, on Azure (MANA) or AWS
# (ENA): the cloud is read from the instance metadata service. Every command is
# idempotent and runs as the admin user from ~/ergon (sudo where it must).
#
#   vm.sh bootstrap          packages, the backports kernel, hugepages, the cloud's DPDK prerequisites
#   vm.sh build              the media driver (static, dpdk), the rtt/tput/counters clients, igb_uio (AWS)
#   vm.sh isolate            CPU isolation on the kernel command line (takes effect at the next boot)
#   vm.sh tune               after each boot: irqbalance off, every IRQ on the housekeeping CPU, hugetlbfs AERON_DIR
#   vm.sh layout             "<housekeeping> <app> <sender> <receiver>" CPUs: the first thread of four cores
#   vm.sh nics               the data NICs from the cloud's metadata: one line per role
#   vm.sh driver <mode>      start the media driver: kernel, kernel-busy or dpdk; a -mono
#                            suffix clocks the transport and clients with CLOCK_MONOTONIC, not TSC
#   vm.sh client <label> <rtt|tput> <args>   a benchmark client against the running driver
#   vm.sh stop               stop the media driver and give the data NICs back to the kernel
#   vm.sh counters           validity counters: VF or allowance counters, data-path switches
#   vm.sh loss               one "key=value;..." line of every loss counter: Aeron's (NAKs,
#                            retransmits, short sends, gap fills), the DPDK transport's drops,
#                            the kernel's UDP errors and the data NICs' kernel drop counters
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
cd "$HOME/ergon"
# BENCH_BIN: another build of the driver and clients (an A/B against older code)
bin=${BENCH_BIN:-$HOME/ergon/target/release}
run=$HOME/bench
mkdir -p "$run"
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
layout() {
    local cpu first primaries=()
    for cpu in $(ls -d /sys/devices/system/cpu/cpu[0-9]* | sed 's/.*cpu//' | sort -n); do
        first=$(sed 's/[,-].*//' /sys/devices/system/cpu/cpu"$cpu"/topology/thread_siblings_list)
        [[ $first == "$cpu" ]] && primaries+=("$cpu")
    done
    echo "${primaries[@]:0:5}"
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

# "<mac> <ip> <prefix>" of each data NIC, lowest address first, from the metadata service.
data_macs() {
    local subnet=${DATA_SUBNET:?DATA_SUBNET: the data subnet, e.g. 10.20.1.0/24}
    case $CLOUD in
    azure)
        curl -fsS -H Metadata:true "http://169.254.169.254/metadata/instance/network?api-version=2021-02-01&format=json" |
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
        token=$(curl -fsS -X PUT -H "X-aws-ec2-metadata-token-ttl-seconds: 300" http://169.254.169.254/latest/api/token)
        local md=http://169.254.169.254/latest/meta-data/network/interfaces/macs
        for mac in $(curl -fsS -H "X-aws-ec2-metadata-token: $token" "$md/" | tr -d /); do
            local ip
            ip=$(curl -fsS -H "X-aws-ec2-metadata-token: $token" "$md/$mac/local-ipv4s" | head -1)
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

# Give the data NICs back to the kernel. Both are on one subnet, and both clouds
# drop a packet whose source address is not the sending NIC's, so each NIC gets
# a routing table its own source address selects.
data_kernel_up() {
    local table=100
    nics | while read -r _ iface vf _ ip prefix _; do
        sudo ip link set "$vf" up
        sudo ip link set "$iface" up
        sudo ip addr replace "$ip/$prefix" dev "$iface"
        local net
        net=$(python3 -c 'import ipaddress,sys; print(ipaddress.ip_network(sys.argv[1], strict=False))' "$ip/$prefix")
        sudo ip route replace "$net" dev "$iface" src "$ip" table "$table"
        # NICs can come back in another order after a DPDK run: drop any rule
        # left for this address before adding its current one
        while sudo ip rule del from "$ip" 2>/dev/null; do :; done
        sudo ip rule add from "$ip" lookup "$table" priority "$table"
        table=$((table + 1))
    done
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
    sudo mkdir -p /etc/default/grub.d
    echo "GRUB_CMDLINE_LINUX_DEFAULT=\"\$GRUB_CMDLINE_LINUX_DEFAULT isolcpus=nohz,domain,managed_irq,1-$last nohz_full=1-$last rcu_nocbs=1-$last irqaffinity=0\"" |
        sudo tee /etc/default/grub.d/99-ergon-isolation.cfg >/dev/null
    sudo update-grub >/dev/null 2>&1
    ;;
tune)
    sudo systemctl stop irqbalance 2>/dev/null || true
    for f in /proc/irq/*/smp_affinity_list; do echo "$HK_CPU" | sudo tee "$f" >/dev/null 2>&1 || true; done
    echo 1 | sudo tee /proc/irq/default_smp_affinity >/dev/null 2>&1 || true
    echo 0 | sudo tee /sys/kernel/mm/ksm/run >/dev/null 2>&1 || true
    sudo sysctl -qw kernel.numa_balancing=0 2>/dev/null || true
    sudo mkdir -p /mnt/huge
    mountpoint -q /mnt/huge || sudo mount -t hugetlbfs -o pagesize=2M,size=1G none /mnt/huge
    echo "cloud $CLOUD layout $(layout) cpus $(nproc)"
    echo "cmdline $(cat /proc/cmdline)"
    echo "isolated $(cat /sys/devices/system/cpu/isolated) nohz_full $(cat /sys/devices/system/cpu/nohz_full 2>/dev/null)"
    echo "clocksource $(cat /sys/devices/system/clocksource/clocksource0/current_clocksource)"
    ;;
layout) layout ;;
nics) nics ;;
driver)
    mode=${2:?kernel, kernel-busy, dpdk or dpdk-spin, optionally -mono, then @KEY=value overrides}
    "$self" stop
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
            # the EAL's only lcore is the housekeeping CPU: no worker threads elsewhere
            echo "export RUSTERON_DPDK_EAL_ARGS='-l $HK_CPU'"
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
            # THP=<always|madvise|never>: the host's transparent hugepages for
            # this run (never also stops khugepaged, whose every scan drains
            # each CPU's LRU batch, preempting busy-spinning agents); `stop`
            # restores Debian's default, always
            if [[ ${pair%%=*} == THP ]]; then
                echo "${pair#*=}" | sudo tee /sys/kernel/mm/transparent_hugepage/enabled >/dev/null || exit 2
                continue
            fi
            echo "export ${pair%%=*}='${pair#*=}'" >>"$env_file"
        done
    fi
    log "driver: $mode${overrides:+ with $overrides}"
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
    # started on the housekeeping CPU, so the client's conductor stays there;
    # the measured thread then pins itself to its own CPU (lab.sh passes the app CPU)
    sudo env AERON_DIR="$AERON_DIR" LABEL="$label" RTT_SECONDS="${RTT_SECONDS:-20}" \
        RTT_HISTOGRAM="$run/rtt-histogram.csv" LOOP_STALLS_DIR="$run" LOAD_SPIKES="$run/load-spikes.csv" LOAD_SPIKE_NS="${LOAD_SPIKE_NS:-100000}" BENCH_CLOCK="$(cat "$run/bench_clock" 2>/dev/null || echo tsc)" AERON_TERM_BUFFER_SPARSE_FILE=false AERON_CLIENT_PRE_TOUCH_MAPPED_MEMORY=true \
        taskset -c "$HK_CPU" "$bin/$prog" "$@"
    ;;
stop)
    sudo pkill -INT -x media_driver 2>/dev/null || true
    for _ in $(seq 40); do pgrep -x media_driver >/dev/null || break; sleep 0.25; done
    sudo pkill -KILL -x media_driver 2>/dev/null || true
    data_return_to_kernel
    data_kernel_up
    sudo sysctl -qw net.core.busy_read=0 net.core.busy_poll=0
    echo "${THP_DEFAULT:-always}" | sudo tee /sys/kernel/mm/transparent_hugepage/enabled >/dev/null || true
    ;;
loss)
    line=
    if pgrep -x media_driver >/dev/null; then
        line=$(sudo env AERON_DIR="$AERON_DIR" timeout 20 "$bin/counters" loss 2>/dev/null) || line=aeron-read-failed=1
    fi
    snmp=$(awk '$1 == "Udp:" { if (!seen) { for (i = 2; i <= NF; i++) name[i] = $i; seen = 1; next }
        for (i = 2; i <= NF; i++) if (name[i] ~ /^(InErrors|RcvbufErrors|SndbufErrors)$/) printf "kernel-udp-%s=%s;", name[i], $i }' /proc/net/snmp)
    drops=0
    while read -r _ iface vf _; do
        for dev in "$iface" "$vf"; do
            for stat in rx_dropped rx_missed_errors rx_fifo_errors; do
                f=/sys/class/net/$dev/statistics/$stat
                [[ -n $dev && -r $f ]] && drops=$((drops + $(cat "$f")))
            done
        done
    done < <(nics 2>/dev/null)
    echo "${line:+$line;}${snmp}nic-kernel-drops=$drops"
    ;;
counters)
    # Validity. Under DPDK, the driver's mirrored PMD stats: on Azure the VF's
    # packets (vf_*) must equal the totals; on AWS the ENA allowance counters
    # must stay at zero. In a kernel mode, the same from ethtool. On Azure, the
    # kernel's count of VF data-path switches.
    pattern=' (vf_)?(rx|tx)_good_packets$|allowance_exceeded$|dir=- clock$'
    if pgrep -x media_driver >/dev/null && dpdk_active; then
        { sudo env AERON_DIR="$AERON_DIR" "$bin/counters" _ | grep -E "$pattern" | tr -s ' ' | tr '\n' ';'; } || true
        echo
    elif [[ $CLOUD == azure ]]; then
        # netvsc's own stats split the VF path (vf_*) from the synthetic one (per queue)
        nics | while read -r role iface _; do
            echo "$role $(ethtool -S "$iface" 2>/dev/null | awk '
                $1 == "vf_rx_packets:" || $1 == "vf_tx_packets:" {printf "%s %s ", $1, $2}
                $1 ~ /^(rx|tx)_queue_[0-9]+_packets:$/ {synthetic += $2}
                END {printf "synthetic_packets: %d", synthetic}')"
        done
    else
        nics | while read -r role iface _; do
            echo "$role $(ethtool -S "$iface" 2>/dev/null | awk '$1 ~ /allowance_exceeded:$/ {printf "%s %s ", $1, $2}')"
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
        for t in /proc/"$pid"/task/*; do
            sudo awk -v comm="$(sudo cat "$t/comm" 2>/dev/null)" -v tid="${t##*/}" '
                /^Cpus_allowed_list:/ {c = $2} /^voluntary_ctxt_switches:/ {v = $2} /^nonvoluntary_ctxt_switches:/ {n = $2}
                END {printf "thread %s/%s cpus=%s vcsw=%s nvcsw=%s\n", comm, tid, c, v, n}' "$t/status" 2>/dev/null
        done
    fi
    # the agents' own stalls, per port (DPDK only): gaps between polls that
    # found frames (rx-poll-gap*) and between any polls (poll-cycle-gap*)
    if [[ -n $pid ]] && dpdk_active; then
        sudo env AERON_DIR="$AERON_DIR" timeout 20 "$bin/counters" -gap 2>/dev/null | sed 's/^/counter /'
    fi
    for cpu in $HK_CPU $APP_CPU $SENDER_CPU $RECEIVER_CPU $APP2_CPU; do
        irqs=$(awk -v col=$((cpu + 2)) 'NR > 1 && NF >= col {s += $col} END {print s + 0}' /proc/interrupts)
        loc=$(awk -v col=$((cpu + 2)) '$1 == "LOC:" {print $col}' /proc/interrupts)
        steal=$(awk -v c="cpu$cpu" '$1 == c {print $9}' /proc/stat)
        echo "cpu $cpu irqs=$irqs timer=$loc steal=$steal"
    done
    ;;
clockcheck)
    taskset -c "$APP_CPU" "$bin/clockcheck" "${2:-60}"
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
    mask=0
    for cpu in $APP_CPU $SENDER_CPU $RECEIVER_CPU $APP2_CPU $spare; do mask=$((mask | (1 << cpu))); done
    sudo bash -c "cd /sys/kernel/tracing && echo 0 > tracing_on && echo > trace && printf '%x' $mask > tracing_cpumask &&
        echo 8192 > buffer_size_kb && echo 1 > events/sched/sched_switch/enable &&
        echo 1 > events/workqueue/workqueue_execute_start/enable && echo 1 > events/irq/irq_handler_entry/enable &&
        { grep -q drainall kprobe_events || echo 'p:drainall lru_add_drain_all' >> kprobe_events; } &&
        mkdir -p instances/drain && echo 1 > instances/drain/events/kprobes/drainall/enable && echo 1 > tracing_on"
    echo "diag spinner cpu $spare, traced cpus $(printf '%x' $mask)"
    ;;
diag-fetch)
    # the spinner's gaps, then the trace with the uptime and wall clock it
    # counts against (to put its events on the wall clock), then the drains
    echo "== gapspin"
    cat "$run/diag-gapspin.log" 2>/dev/null
    echo "== trace"
    sudo grep -E 'sched_switch|workqueue_execute_start|irq_handler_entry' /sys/kernel/tracing/trace
    echo "UPTIME $(cut -d' ' -f1 /proc/uptime) REALTIME $(date +%s.%N)"
    echo "== drains"
    sudo grep -v '^#' /sys/kernel/tracing/instances/drain/trace 2>/dev/null | grep drainall
    ;;
diag-stop)
    pkill -x gapspin 2>/dev/null || true
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
    sudo env AERON_DIR="$AERON_DIR" "$bin/counters" 'role=receiver' | awk '
        / rx-hw-delay-max-ns$/ {max = $1} / rx-hw-delay-sum-ns$/ {sum = $1} / rx-hw-timestamped$/ {n = $1}
        END {if (n > 0) printf "%.0f,%d,%d\n", sum / n, max, n}'
    ;;
*)
    awk 'NR > 1 && !/^#/ {exit} NR > 1 {print}' "$self" >&2
    exit 2
    ;;
esac
