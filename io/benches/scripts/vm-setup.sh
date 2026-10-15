#!/usr/bin/env bash
# Runs as root on each VM (Azure or AWS).
#   vm-setup.sh install <housekeeping-cpus> <isolated-cpus>   packages + full isolation (reboot afterwards)
#   vm-setup.sh boot <profile> <housekeeping-cpus> <isolated-cpus> [tick-cpus]
#         kernel command line for the next boot (reboot afterwards); profiles:
#           full          isolcpus=managed_irq,domain + nohz_full + rcu_nocbs + irqaffinity, irqbalance off
#           full-sqtick   full, but <tick-cpus> (the SQPOLL CPU) left out of nohz_full: a kernel
#                         thread that never leaves the kernel keeps its tick instead of having it
#                         restarted by an IPI for every RCU grace period
#           full-nomitig  full + mitigations=off
#           isolcpus      isolcpus=managed_irq,domain + irqaffinity only, irqbalance off
#           stock         nothing added, irqbalance on (the VM as it comes)
#   vm-setup.sh tune [stock]        per-boot settings (stock: none), prints the environment
#   vm-setup.sh cond <name> on|off [cpu]   one runtime setting under test:
#           housekeeping  workqueues on housekeeping CPUs, NMI and soft-lockup watchdogs off, vm.stat_interval=120
#           prefer        napi_defer_hard_irqs=100, gro_flush_timeout=200000 on every NIC (for SO_PREFER_BUSY_POLL)
#           noquickack    routes without quickack (rto_min kept)
#           nogro         GRO off on every NIC
#           nocoalesce    interrupt moderation off (adaptive-rx off, rx-usecs 0) where the NIC allows it
#           irqlocal      every NIC queue interrupt on <cpu> (the loop's CPU)
#           rt            kernel.sched_rt_runtime_us=-1 (lets a SCHED_FIFO loop run unthrottled)
#           noagent       the cloud guest agents stopped (Azure walinuxagent and hv-kvp-daemon; AWS
#                         amazon-ssm-agent and the CloudWatch agent), restarted by `off`
#           nodim         adaptive interrupt moderation off (ethtool adaptive-rx off), nothing else:
#                         ENA's DIM work otherwise runs as a kworker on the CPU that polls the
#                         queue, pre-empting the busy-polling loop about 40 times a second
#           onequeue      every NIC down to one combined queue: the busy-polled queue then also
#                         carries the flow's transmit completions, and no other queue interrupts
#           noautocork    net.ipv4.tcp_autocorking=0: a send never waits for the previous one to
#                         leave the qdisc
#           cstate        every CPU idle state deeper than C1 disabled (exit latency above 1 µs,
#                         e.g. C6's 190 µs on c7i), so an idling core wakes for an interrupt at once
#   vm-setup.sh trace start <cpu-list> | trace stop <file>   kernel events on those CPUs, mono clock
#   vm-setup.sh fgraph start <cpu> <us> | fgraph stop <file>  kernel functions slower than <us> on
#         <cpu> (function_graph with tracing_thresh; heavy: diagnosis runs only)
#   vm-setup.sh slowsys start <cpu> <us> <file> | slowsys stop   syscalls slower than <us> on <cpu>
#         (bpftrace, one line each: end mono ns, syscall number, duration ns)
#   vm-setup.sh hwlat start <cpus> <us> | hwlat stop <file>   the kernel's hardware-latency
#         detector on each of <cpus> at once: gaps longer than <us> with interrupts off are the hypervisor or firmware
#   vm-setup.sh netstats <file>     snapshot NIC (ethtool -S) and TCP (nstat) counters
#   vm-setup.sh irqs <file>         snapshot /proc/interrupts (plus each CPU's SMI count on bare metal)
set -euo pipefail
cmd=${1:?install|boot|tune|cond|trace|irqs}
STATE=/var/lib/ergon-lab
mkdir -p "$STATE"

# Hex mask for a CPU list such as "2,3,6-7" (CPUs below 64).
mask() {
  local m=0 part a b c
  IFS=, read -ra parts <<< "$1"
  for part in "${parts[@]}"; do
    a=${part%-*}; b=${part#*-}
    for ((c = a; c <= b; c++)); do m=$((m | (1 << c))); done
  done
  printf '%x' "$m"
}
# NICs: interfaces backed by a device (netvsc, the MANA VF, ENA), not lo,
# tunnels or plain files such as bonding_masters.
nics() {
  local n
  for n in /sys/class/net/*; do
    [[ -e $n/device ]] && basename "$n"
  done
  return 0
}
nic_irqs() { awk -F: '/mana|ena|ens|eth|enp|mlx|virtio/ && $1 ~ /^ *[0-9]+$/ {gsub(/ /, "", $1); print $1}' /proc/interrupts; }
online() { cat /sys/devices/system/cpu/online; }
hk_cpus() { sed -n 's/.*irqaffinity=\([^ ]*\).*/\1/p' /proc/cmdline; }

# The CPUs of list $1 not in list $2, comma-separated (lists may hold ranges).
cpus_minus() {
  local c out=""
  for c in $(expand_cpus "$1"); do
    [[ ,$(expand_cpus "$2" | tr ' ' ,), == *,$c,* ]] || out+=${out:+,}$c
  done
  echo "$out"
}
expand_cpus() {
  local part
  for part in ${1//,/ }; do
    if [[ $part == *-* ]]; then seq "${part%-*}" "${part#*-}"; else echo "$part"; fi
  done | tr '\n' ' '
}

boot() {
  local profile=$1 hk=$2 iso=$3 line=""
  case $profile in
    full) line="isolcpus=managed_irq,domain,$iso nohz_full=$iso rcu_nocbs=$iso irqaffinity=$hk" ;;
    full-sqtick)
      local nohz
      nohz=$(cpus_minus "$iso" "${4:?tick cpus}")
      line="isolcpus=managed_irq,domain,$iso nohz_full=$nohz rcu_nocbs=$iso irqaffinity=$hk" ;;
    full-nomitig) line="isolcpus=managed_irq,domain,$iso nohz_full=$iso rcu_nocbs=$iso irqaffinity=$hk mitigations=off" ;;
    isolcpus) line="isolcpus=managed_irq,domain,$iso irqaffinity=$hk" ;;
    stock) line="" ;;
    *) echo "unknown boot profile $profile" >&2; exit 2 ;;
  esac
  local f=/etc/default/grub.d/99-iobench.cfg
  if [[ -n $line ]]; then
    printf 'GRUB_CMDLINE_LINUX_DEFAULT="$GRUB_CMDLINE_LINUX_DEFAULT %s"\n' "$line" > "$f"
    systemctl disable --now irqbalance 2>/dev/null || true
  else
    rm -f "$f"
    systemctl enable irqbalance 2>/dev/null || true
  fi
  update-grub >/dev/null 2>&1 || grub2-mkconfig -o /boot/grub2/grub.cfg >/dev/null 2>&1
  echo "$profile" > "$STATE/boot-profile"
  echo "boot profile $profile set; reboot to apply: ${line:-<none>}"
}

routes() {
  # Re-add the default and subnet routes with the given options, keeping every
  # existing attribute (proto dhcp, src, metric); quickack and rto_min (shown
  # as "rto_min lock 5ms") are stripped first so the options can be switched
  # both ways.
  local opts=$1 dev def net line
  dev=$(ip route show default | awk '{print $5; exit}')
  def=$(ip -4 route show default | sed -n '1{s/ quickack [0-9]*//; s/ rto_min \(lock \)\{0,1\}[^ ]*//; p}')
  # shellcheck disable=SC2086
  ip route replace $def $opts || echo "WARN: default route: $def"
  for net in $(ip -4 route show dev "$dev" scope link | awk '{print $1}'); do
    line=$(ip -4 route show "$net" dev "$dev" scope link | sed -n '1{s/ quickack [0-9]*//; s/ rto_min \(lock \)\{0,1\}[^ ]*//; p}')
    # shellcheck disable=SC2086
    ip route replace $net dev "$dev" ${line#"$net"} $opts || echo "WARN: route: $line"
  done
}

if [[ $cmd == install ]]; then
  hk=${2:?housekeeping cpus}; iso=${3:?isolated cpus}
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  # Parallel copies (several SSH connections at once) must not be refused.
  echo "MaxStartups 100:30:200" > /etc/ssh/sshd_config.d/99-lab.conf
  systemctl reload ssh 2> /dev/null || true
  apt-get install -y -qq build-essential pkg-config ethtool chrony jq msr-tools linux-tools-common "linux-tools-$(uname -r)" >/dev/null 2>&1 \
    || apt-get install -y -qq build-essential pkg-config ethtool chrony jq msr-tools >/dev/null
  boot full "$hk" "$iso"
  exit 0
fi

if [[ $cmd == boot ]]; then
  boot "${2:?profile}" "${3:?housekeeping cpus}" "${4:?isolated cpus}" "${5:-}"
  exit 0
fi

if [[ $cmd == tune ]]; then
  if [[ ${2:-} != stock ]]; then
    # Busy polling is opted into per variant (SO_BUSY_POLL, EPIOCSPARAMS, NAPI
    # registration), never through these global defaults.
    sysctl -qw net.core.busy_read=0 net.core.busy_poll=0
    routes "quickack 1 rto_min 5ms"
    # Steer every movable IRQ to the housekeeping CPUs.
    hk=$(hk_cpus)
    if [[ -n $hk ]]; then
      for f in /proc/irq/*/smp_affinity_list; do echo "$hk" > "$f" 2>/dev/null || true; done
    fi
  fi
  # A report: an unsupported query (ethtool on a synthetic NIC) must not stop it.
  set +e +o pipefail
  echo "== uname";        uname -a
  echo "== cmdline";      cat /proc/cmdline
  echo "== boot profile"; cat "$STATE/boot-profile" 2>/dev/null || echo unknown
  echo "== isolated";     cat /sys/devices/system/cpu/isolated
  echo "== clocksource";  cat /sys/devices/system/clocksource/clocksource0/current_clocksource
  echo "== mitigations";  grep -r . /sys/devices/system/cpu/vulnerabilities/ 2>/dev/null | sed 's|.*/||' | head -20
  echo "== cpuidle";      echo "driver $(cat /sys/devices/system/cpu/cpuidle/current_driver 2>/dev/null)"
  for st in /sys/devices/system/cpu/cpu0/cpuidle/state[0-9]*; do
    [[ -e $st/name ]] && echo "$(cat "$st/name") latency $(cat "$st/latency") µs disabled $(cat "$st/disable")"
  done
  echo "== cpu";          lscpu | grep -E 'Model name|^CPU\(s\)|Thread|Core|Socket|NUMA node0|Hypervisor'
  echo "== topology";     lscpu -p=CPU,CORE | grep -v '^#'
  echo "== routes";       ip route show
  echo "== links";        ip -br link
  for i in $(nics); do
    echo "== ethtool -i $i"; ethtool -i "$i" 2>/dev/null | head -3
    echo "== ethtool -l $i"; ethtool -l "$i" 2>/dev/null | tail -5
    echo "== ethtool -c $i"; ethtool -c "$i" 2>/dev/null | grep -E 'Adaptive|rx-usecs|tx-usecs' | head -4
    echo "== ethtool -T $i"; ethtool -T "$i" 2>/dev/null | head -20
  done
  echo "== nic irqs";     for q in $(nic_irqs); do echo "$q $(cat /proc/irq/$q/smp_affinity_list 2>/dev/null) $(grep -E "^ *$q:" /proc/interrupts | awk '{print $NF}')"; done
  echo "== chrony";       chronyc tracking 2>/dev/null || true
  echo "== chrony sources"; chronyc sources 2>/dev/null || true
  echo "== io_uring";     sysctl kernel.io_uring_disabled 2>/dev/null || echo "kernel.io_uring_disabled absent"
  exit 0
fi

if [[ $cmd == cond ]]; then
  name=${2:?condition}; state=${3:?on|off}; cpu=${4:-}
  saved=$STATE/cond-$name
  case $name:$state in
    housekeeping:on)
      {
        echo "wq=$(cat /sys/devices/virtual/workqueue/cpumask 2>/dev/null)"
        echo "nmi=$(sysctl -n kernel.nmi_watchdog 2>/dev/null || echo 0)"
        echo "wd=$(sysctl -n kernel.watchdog 2>/dev/null || echo 1)"
        echo "stat=$(sysctl -n vm.stat_interval)"
      } > "$saved"
      hk=$(hk_cpus); [[ -n $hk ]] || hk=0
      echo "$(mask "$hk")" > /sys/devices/virtual/workqueue/cpumask || echo "WARN: workqueue cpumask"
      sysctl -qw kernel.nmi_watchdog=0 kernel.watchdog=0 vm.stat_interval=120 || echo "WARN: sysctl"
      ;;
    housekeeping:off)
      # shellcheck disable=SC1090
      source "$saved"
      [[ -n ${wq:-} ]] && echo "$wq" > /sys/devices/virtual/workqueue/cpumask
      sysctl -qw kernel.nmi_watchdog="$nmi" kernel.watchdog="$wd" vm.stat_interval="$stat" || true
      ;;
    prefer:on)
      : > "$saved"
      for d in $(nics); do
        [[ -e /sys/class/net/$d/napi_defer_hard_irqs ]] || continue
        echo "$d $(cat /sys/class/net/$d/napi_defer_hard_irqs) $(cat /sys/class/net/$d/gro_flush_timeout)" >> "$saved"
        echo 100 > "/sys/class/net/$d/napi_defer_hard_irqs"
        echo 200000 > "/sys/class/net/$d/gro_flush_timeout"
      done
      ;;
    prefer:off)
      while read -r d defer flush; do
        echo "$defer" > "/sys/class/net/$d/napi_defer_hard_irqs"
        echo "$flush" > "/sys/class/net/$d/gro_flush_timeout"
      done < "$saved"
      ;;
    noquickack:on) routes "rto_min 5ms" ;;
    noquickack:off) routes "quickack 1 rto_min 5ms" ;;
    nogro:on) for d in $(nics); do ethtool -K "$d" gro off 2>/dev/null || echo "WARN: $d gro off unsupported"; done ;;
    nogro:off) for d in $(nics); do ethtool -K "$d" gro on 2>/dev/null || true; done ;;
    nocoalesce:on)
      : > "$saved"
      for d in $(nics); do
        a=$(ethtool -c "$d" 2>/dev/null | awk '/Adaptive RX/ {print $3}' || true)
        u=$(ethtool -c "$d" 2>/dev/null | awk '/^rx-usecs:/ {print $2}' || true)
        [[ -n $u ]] || { echo "WARN: $d reports no interrupt moderation"; continue; }
        echo "$d ${a:-off} $u" >> "$saved"
        ethtool -C "$d" adaptive-rx off rx-usecs 0 2>/dev/null || echo "WARN: $d rx-usecs 0 unsupported"
      done
      ;;
    nocoalesce:off)
      while read -r d a u; do ethtool -C "$d" adaptive-rx "$a" rx-usecs "$u" 2>/dev/null || true; done < "$saved"
      ;;
    irqlocal:on)
      : > "$saved"
      for q in $(nic_irqs); do
        echo "$q $(cat /proc/irq/$q/smp_affinity_list)" >> "$saved"
        echo "${cpu:?cpu}" > "/proc/irq/$q/smp_affinity_list" 2>/dev/null || echo "WARN: irq $q cannot move (managed)"
      done
      ;;
    irqlocal:off)
      while read -r q aff; do echo "$aff" > "/proc/irq/$q/smp_affinity_list" 2>/dev/null || true; done < "$saved"
      ;;
    cstate:on)
      : > "$saved"
      for st in /sys/devices/system/cpu/cpu[0-9]*/cpuidle/state[0-9]*; do
        [[ -e $st/latency ]] || continue
        if (( $(cat "$st/latency") > 1 )); then
          echo "$st $(cat "$st/disable")" >> "$saved"
          echo 1 > "$st/disable"
        fi
      done
      [[ -s $saved ]] || echo "WARN: no idle state deeper than C1 to disable ($(cat /sys/devices/system/cpu/cpuidle/current_driver 2>/dev/null || echo no cpuidle driver))"
      ;;
    cstate:off)
      while read -r st dis; do echo "$dis" > "$st/disable"; done < "$saved"
      ;;
    noagent:on)
      : > "$saved"
      for u in walinuxagent hv-kvp-daemon snap.amazon-ssm-agent.amazon-ssm-agent amazon-ssm-agent amazon-cloudwatch-agent; do
        if systemctl is-active --quiet "$u"; then
          echo "$u" >> "$saved"
          systemctl stop "$u"
        fi
      done
      [[ -s $saved ]] || echo "WARN: no cloud guest agent was running"
      ;;
    noagent:off)
      while read -r u; do systemctl start "$u" || echo "WARN: $u did not restart"; done < "$saved"
      ;;
    nodim:on)
      : > "$saved"
      for d in $(nics); do
        a=$(ethtool -c "$d" 2>/dev/null | awk '/Adaptive RX/ {print $3}' || true)
        [[ -n $a ]] || { echo "WARN: $d reports no adaptive moderation"; continue; }
        echo "$d $a" >> "$saved"
        ethtool -C "$d" adaptive-rx off 2>/dev/null || echo "WARN: $d adaptive-rx off unsupported"
      done
      ;;
    nodim:off)
      while read -r d a; do ethtool -C "$d" adaptive-rx "$a" 2>/dev/null || true; done < "$saved"
      ;;
    onequeue:on)
      : > "$saved"
      for d in $(nics); do
        n=$(ethtool -l "$d" 2>/dev/null | awk '/^Current hardware settings/ {c = 1} c && /^Combined:/ {print $2; exit}')
        [[ -n $n ]] || { echo "WARN: $d reports no channels"; continue; }
        echo "$d $n" >> "$saved"
        ethtool -L "$d" combined 1 2>/dev/null || echo "WARN: $d combined 1 unsupported"
      done
      sleep 3
      ;;
    onequeue:off)
      while read -r d n; do ethtool -L "$d" combined "$n" 2>/dev/null || echo "WARN: $d combined $n not restored"; done < "$saved"
      sleep 3
      ;;
    noautocork:on)
      sysctl -n net.ipv4.tcp_autocorking > "$saved"
      sysctl -qw net.ipv4.tcp_autocorking=0
      ;;
    noautocork:off) sysctl -qw net.ipv4.tcp_autocorking="$(cat "$saved")" ;;
    rt:on) sysctl -qw kernel.sched_rt_runtime_us=-1 ;;
    rt:off) sysctl -qw kernel.sched_rt_runtime_us=950000 ;;
    *) echo "unknown condition $name:$state" >&2; exit 2 ;;
  esac
  echo "cond $name $state"
  exit 0
fi

if [[ $cmd == trace ]]; then
  T=/sys/kernel/tracing
  [[ -d $T/events ]] || mount -t tracefs nodev "$T" 2>/dev/null || true
  [[ -d $T/events ]] || T=/sys/kernel/debug/tracing
  case ${2:?start|stop} in
    start)
      echo 0 > "$T/tracing_on"
      echo > "$T/trace"
      echo mono > "$T/trace_clock"
      echo "$(mask "${3:?cpu list}")" > "$T/tracing_cpumask"
      echo 32768 > "$T/buffer_size_kb"
      for e in irq/irq_handler_entry irq/irq_handler_exit irq/softirq_entry irq/softirq_exit \
               irq_vectors ipi sched/sched_switch timer/hrtimer_expire_entry timer/hrtimer_expire_exit \
               workqueue/workqueue_execute_start workqueue/workqueue_execute_end \
               exceptions/page_fault_user exceptions/page_fault_kernel; do
        [[ -e $T/events/$e/enable ]] && echo 1 > "$T/events/$e/enable"
      done
      echo 1 > "$T/tracing_on"
      ;;
    stop)
      echo 0 > "$T/tracing_on"
      cat "$T/trace" > "${3:?file}"
      echo 0 > "$T/events/enable"
      echo "$(mask "$(online)")" > "$T/tracing_cpumask"
      echo > "$T/trace"
      ;;
  esac
  exit 0
fi

if [[ $cmd == fgraph ]]; then
  T=/sys/kernel/tracing
  [[ -d $T/events ]] || mount -t tracefs nodev "$T" 2>/dev/null || true
  [[ -d $T/events ]] || T=/sys/kernel/debug/tracing
  case ${2:?start|stop} in
    start)
      echo 0 > "$T/tracing_on"
      echo > "$T/trace"
      echo mono > "$T/trace_clock"
      echo "$(mask "${3:?cpu}")" > "$T/tracing_cpumask"
      echo 65536 > "$T/buffer_size_kb"
      echo "${4:?microseconds}" > "$T/tracing_thresh"
      echo function_graph > "$T/current_tracer"
      echo 1 > "$T/options/funcgraph-abstime" 2>/dev/null || true
      echo 1 > "$T/options/funcgraph-proc" 2>/dev/null || true
      echo 1 > "$T/tracing_on"
      ;;
    stop)
      echo 0 > "$T/tracing_on"
      cat "$T/trace" > "${3:?file}"
      echo nop > "$T/current_tracer"
      echo 0 > "$T/tracing_thresh"
      echo "$(mask "$(online)")" > "$T/tracing_cpumask"
      echo > "$T/trace"
      ;;
  esac
  exit 0
fi

if [[ $cmd == hwlat ]]; then
  T=/sys/kernel/tracing
  [[ -d $T/events ]] || mount -t tracefs nodev "$T" 2>/dev/null || true
  [[ -d $T/events ]] || T=/sys/kernel/debug/tracing
  case ${2:?start|stop} in
    start)
      echo 0 > "$T/tracing_on"
      echo > "$T/trace"
      echo mono > "$T/trace_clock"
      echo "$(mask "${3:?cpu list}")" > "$T/tracing_cpumask"
      echo "${4:?microseconds}" > "$T/tracing_thresh"
      # Sample 950 ms of every second, interrupts off, on every listed CPU at
      # once (per-cpu, kernel 5.14+), else one at a time.
      echo 1000000 > "$T/hwlat_detector/window"
      echo 950000 > "$T/hwlat_detector/width"
      echo per-cpu > "$T/hwlat_detector/mode" 2>/dev/null \
        || echo round-robin > "$T/hwlat_detector/mode" 2>/dev/null || true
      echo hwlat > "$T/current_tracer"
      echo 1 > "$T/tracing_on"
      ;;
    stop)
      echo 0 > "$T/tracing_on"
      cat "$T/trace" > "${3:?file}"
      echo nop > "$T/current_tracer"
      echo 0 > "$T/tracing_thresh"
      echo "$(mask "$(online)")" > "$T/tracing_cpumask"
      echo > "$T/trace"
      ;;
  esac
  exit 0
fi

if [[ $cmd == slowsys ]]; then
  case ${2:?start|stop} in
    start)
      cpu=${3:?cpu}; ns=$(( ${4:?microseconds} * 1000 )); out=${5:?file}
      command -v bpftrace > /dev/null || DEBIAN_FRONTEND=noninteractive apt-get install -y -qq bpftrace > /dev/null 2>&1
      # Kernel-side filter: only the loop CPU's syscalls are timed, and only
      # the slow ones are printed.
      nohup bpftrace -e "
        tracepoint:raw_syscalls:sys_enter /cpu == $cpu/ { @s[tid] = nsecs; @n[tid] = args->id; }
        tracepoint:raw_syscalls:sys_exit /cpu == $cpu && @s[tid]/ {
          \$d = nsecs - @s[tid];
          if (\$d > $ns) { printf(\"%llu %d %llu\\n\", nsecs, @n[tid], \$d); }
          delete(@s[tid]);
        }" > "$out" 2> "$out.err" < /dev/null &
      echo $! > "$STATE/slowsys.pid"
      sleep 3
      ;;
    stop)
      kill "$(cat "$STATE/slowsys.pid")" 2>/dev/null || true
      sleep 1
      ;;
  esac
  exit 0
fi

if [[ $cmd == netstats ]]; then
  # NIC and TCP counters: drops, the cloud's allowance counters (ENA:
  # pps/bw/conntrack_allowance_exceeded), retransmissions and timeouts.
  {
    for d in $(nics); do echo "== ethtool -S $d"; ethtool -S "$d" 2>/dev/null; done
    echo "== nstat"; nstat -az 2>/dev/null
  } > "${2:?file}"
  exit 0
fi

if [[ $cmd == irqs ]]; then
  cat /proc/interrupts > "${2:?file}"
  # Bare-metal Intel: each CPU's SMI count (MSR 0x34) as one more row, so
  # firmware pauses show next to the interrupts. VMs do not expose it.
  if [[ -e /dev/cpu/0/msr ]] || modprobe msr 2> /dev/null; then
    if command -v rdmsr > /dev/null && smi=$(rdmsr -a -d 0x34 2> /dev/null); then
      echo " SMI: $(echo $smi)   System management interrupts" >> "$2"
    fi
  fi
  exit 0
fi
echo "unknown command $cmd" >&2; exit 2
