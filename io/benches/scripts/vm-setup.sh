#!/usr/bin/env bash
# Runs as root on each VM.
#   vm-setup.sh install <housekeeping-cpus> <isolated-cpus>   packages + kernel cmdline (reboot afterwards)
#   vm-setup.sh tune                                          per-boot sysctls and route options, prints the environment
set -euo pipefail
cmd=${1:?install|tune}

if [[ $cmd == install ]]; then
  hk=${2:?housekeeping cpus}; iso=${3:?isolated cpus}
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y -qq build-essential pkg-config ethtool chrony jq linux-tools-common "linux-tools-$(uname -r)" >/dev/null 2>&1 \
    || apt-get install -y -qq build-essential pkg-config ethtool chrony jq >/dev/null
  cat > /etc/default/grub.d/99-iobench.cfg <<CFG
GRUB_CMDLINE_LINUX_DEFAULT="\$GRUB_CMDLINE_LINUX_DEFAULT isolcpus=managed_irq,domain,$iso nohz_full=$iso rcu_nocbs=$iso irqaffinity=$hk"
CFG
  update-grub >/dev/null 2>&1
  systemctl disable --now irqbalance 2>/dev/null || true
  echo "install done; reboot to apply isolcpus=$iso"
  exit 0
fi

if [[ $cmd == tune ]]; then
  # Busy polling is opted into per variant (SO_BUSY_POLL, EPIOCSPARAMS, NAPI
  # registration), never through these global defaults.
  sysctl -qw net.core.busy_read=0 net.core.busy_poll=0
  # Re-add the default and subnet routes with quickack and a 5 ms minimum RTO,
  # keeping every existing attribute (proto dhcp, src, metric).
  dev=$(ip route show default | awk '{print $5; exit}')
  def=$(ip -4 route show default | head -1)
  # shellcheck disable=SC2086
  ip route replace $def quickack 1 rto_min 5ms || echo "WARN: default route tune failed: $def"
  for net in $(ip -4 route show dev "$dev" scope link | awk '{print $1}'); do
    line=$(ip -4 route show "$net" dev "$dev" scope link | head -1)
    # shellcheck disable=SC2086
    ip route replace $net dev "$dev" ${line#"$net"} quickack 1 rto_min 5ms || echo "WARN: route tune failed: $line"
  done
  # Steer every movable IRQ to the housekeeping CPUs.
  hk=$(sed -n 's/.*irqaffinity=\([^ ]*\).*/\1/p' /proc/cmdline)
  for f in /proc/irq/*/smp_affinity_list; do echo "$hk" > "$f" 2>/dev/null || true; done
  echo "== uname";        uname -a
  echo "== cmdline";      cat /proc/cmdline
  echo "== isolated";     cat /sys/devices/system/cpu/isolated
  echo "== clocksource";  cat /sys/devices/system/clocksource/clocksource0/current_clocksource
  echo "== cpu";          lscpu | grep -E 'Model name|^CPU\(s\)|Thread|Core|Socket|NUMA node0'
  echo "== topology";     lscpu -p=CPU,CORE | grep -v '^#'
  echo "== routes";       ip route show
  echo "== links";        ip -br link
  for i in $(ls /sys/class/net | grep -v lo); do
    echo "== ethtool -i $i"; ethtool -i "$i" 2>/dev/null | head -3
    echo "== ethtool -T $i"; ethtool -T "$i" 2>/dev/null | head -20
  done
  echo "== chrony";       chronyc tracking 2>/dev/null || true
  echo "== chrony sources"; chronyc sources 2>/dev/null || true
  echo "== io_uring";     sysctl kernel.io_uring_disabled 2>/dev/null || echo "kernel.io_uring_disabled absent"
  exit 0
fi
echo "unknown command $cmd" >&2; exit 2
