#!/usr/bin/env bash
# Kernel UDP against DPDK (Azure MANA) for the Aeron media driver, on two Azure
# VMs that exist only while this script runs. Runs on macOS or Linux with the az
# CLI logged in and an SSH public key (~/.ssh/id_ed25519.pub or id_rsa.pub).
#
#   md-dpdk/benches/cloud/azure/lab.sh
#
# Creates resource group $LAB_GROUP (refusing one that exists) in $LAB_REGION:
# a proximity placement group, a VNet with a management subnet (10.20.0.0/24,
# SSH from this machine's address only) and a data subnet (10.20.1.0/24), and
# two $LAB_SIZE VMs (Debian 13), each with an accelerated management NIC and two
# accelerated data NICs: the media driver's sender port (A .4, B .6) and
# receiver port (A .5, B .7). Then, on both VMs at once (../vm.sh, ../lib.sh): bootstrap, a
# reboot into the backports kernel, vm.sh build. Then $LAB_REPS reps of every
# mode in $LAB_MODES (a fresh random order each rep): both drivers started in
# the mode, a round-trip run (A pings, B pongs) and a throughput run (A
# publishes, B subscribes). Results go to target/md-dpdk-bench/<stamp>/ and the
# tables to its summary.md (../analyse.py). The group is deleted on any exit.
#
#   LAB_REGION (northcentralus) LAB_SIZE (Standard_D8s_v6; LAB_SIZE_A, LAB_SIZE_B per VM) LAB_REPS (5)
#   LAB_MODES ("kernel kernel-busy dpdk"; also dpdk-spin, and a +idle suffix
#   holding an idle localhost subscription on each driver) LAB_LOCAL_MODES (none;
#   e.g. "kernel dpdk dpdk-spin": localhost runs on VM a, needs 5 cores) LAB_STATES ("pinned isolated": the stock
#   kernel, then CPUs 1-7 isolated on the kernel command line) LAB_GROUP (ergon-lab-md-dpdk)
#   LAB_IMAGE (Debian:debian-13:13-gen2:latest) LAB_KEEP=1 keeps the group;
#   LAB_REPS=0 with LAB_KEEP=1 stops after the build, for work on the VMs by hand
#   LAB_MIX (none; e.g. "trading=orders:3001:10000:32 md:3002:200000:288:50 bulk:3003:20000:1344:10")
#   LAB_MIX_SECONDS (10); round trips of 100 µs or more are logged with their times (spikes-*.csv)
#   RTT_MESSAGES (300000) RTT_WARMUP (50000) TPUT_SECONDS (5)
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=${LAB_REPO:-$(cd "$here/../../../.." && pwd)}
# Run from a snapshot of these scripts: bash reads a script as it runs, so an
# edit to the checkout mid-run would otherwise break this run (and its teardown).
if [[ -z ${LAB_SNAPSHOT:-} ]]; then
    snapshot=$(mktemp -d)
    cp -R "$here/.." "$snapshot/cloud"
    LAB_SNAPSHOT=$snapshot LAB_REPO=$repo exec bash "$snapshot/cloud/${here##*/}/${0##*/}" "$@"
fi
group=${LAB_GROUP:-ergon-lab-md-dpdk}
region=${LAB_REGION:-northcentralus}
size=${LAB_SIZE:-Standard_D8s_v6}
# per-VM overrides: VM a hosts the localhost runs (LAB_LOCAL_MODES), which need five cores
size_a=${LAB_SIZE_A:-$size}
size_b=${LAB_SIZE_B:-$size}
# x86 only: production runs on x86. Azure's Arm sizes carry a `p` after the
# family letters (Standard_D2ps_v6, Standard_E4pds_v6, ...).
for s in "$size_a" "$size_b"; do
    if [[ $s =~ ^Standard_[A-Z]+[0-9]+[a-z]*p[a-z]*_ ]]; then
        echo "LAB_SIZE $s is an Arm size: the benchmarks run on x86 only" >&2
        exit 2
    fi
done
image=${LAB_IMAGE:-Debian:debian-13:13-gen2:latest}
reps=${LAB_REPS:-5}
read -ra modes <<<"${LAB_MODES:-kernel kernel-busy dpdk}"
local_modes=${LAB_LOCAL_MODES:-}
# latency under load: message sizes (none: skipped), fractions of each size's
# sustained maximum, seconds per point
load_sizes=${LAB_LOAD_SIZES:-}
load_fractions=${LAB_LOAD_FRACTIONS:-10 50 80 95 100}
load_seconds=${LAB_LOAD_SECONDS:-5}
# absolute rates per size instead of fractions: "1344:200000,400000 288:..."
load_ladder=${LAB_LOAD_LADDER:-}
# several streams at once, each with its own rate, size and bursts (lib.sh
# mix_run): profiles "<profile>=<name:stream:msgs/s:size[:burst]> ...;..."
mix=${LAB_MIX:-}
mix_seconds=${LAB_MIX_SECONDS:-10}
read -ra states <<<"${LAB_STATES:-pinned isolated}"
rtt_messages=${RTT_MESSAGES:-300000}
rtt_warmup=${RTT_WARMUP:-50000}
tput_seconds=${TPUT_SECONDS:-5}
user=azureuser
out=$repo/target/md-dpdk-bench/$(date +%Y%m%d-%H%M%S)
known=$out/known_hosts
created=0

log() { echo "[$(date +%T)] $*" >&2; }
die() { log "ERROR: $*"; exit 1; }

key=
for k in ~/.ssh/id_ed25519.pub ~/.ssh/id_rsa.pub; do [[ -f $k ]] && { key=$k; break; }; done
[[ -n $key ]] || die "no ~/.ssh/id_ed25519.pub or ~/.ssh/id_rsa.pub"
command -v az >/dev/null || die "az is not on PATH"
az account show -o none 2>/dev/null || die "az is not logged in (az login)"
mkdir -p "$out"

teardown() {
    local rc=$?
    # an interrupt must not abort the delete, nor a create still running race it
    trap '' INT TERM
    [[ -n ${follow:-} ]] && kill "$follow" 2>/dev/null
    [[ -n ${pa:-} ]] && kill "$pa" "${pb:-}" 2>/dev/null
    if ((created)) && [[ -z ${LAB_KEEP:-} ]]; then
        log "deleting resource group $group"
        { az group delete -n "$group" --yes --no-wait -o none &&
            az group wait -n "$group" --deleted --timeout 3600 -o none; } ||
            log "WARNING: delete $group by hand: az group delete -n $group --yes"
        if [[ $(az group exists -n "$group") == false ]]; then
            log "verified: resource group $group no longer exists"
        else
            log "WARNING: resource group $group still exists: az group delete -n $group --yes"
        fi
    elif ((created)); then
        log "LAB_KEEP: resource group $group kept; delete it with: az group delete -n $group --yes"
    fi
    log "results: $out"
    exit "$rc"
}
trap teardown EXIT
trap 'exit 130' INT TERM

if [[ $(az group exists -n "$group") == true ]]; then
    die "resource group $group exists: delete it or set LAB_GROUP"
fi

# LAB_SSH_FROM: this machine's public addresses (space-separated), for a
# connection that fails over between links; default the current one
me=${LAB_SSH_FROM:-$(curl -fsS https://api.ipify.org)} || die "cannot find this machine's public address"
me=$(printf '%s\n' $me | sort -u | tr '\n' ' ') # a repeated address is an error to Azure
read -ra me_cidrs <<<"$(printf '%s/32 ' $me)"
log "creating $group in $region: a $size_a, b $size_b, SSH from $me"
az group create -n "$group" -l "$region" -o none || die "group create failed"
created=1
az ppg create -g "$group" -n ppg -l "$region" --intent-vm-sizes $(printf "%s\n" "$size_a" "$size_b" | sort -u) -o none || die "ppg create failed"
az network vnet create -g "$group" -n vnet --address-prefixes 10.20.0.0/16 \
    --subnet-name mgmt --subnet-prefixes 10.20.0.0/24 -o none || die "vnet create failed"
az network vnet subnet create -g "$group" --vnet-name vnet -n data --address-prefixes 10.20.1.0/24 -o none ||
    die "data subnet create failed"
az network nsg create -g "$group" -n nsg -o none || die "nsg create failed"
az network nsg rule create -g "$group" --nsg-name nsg -n ssh --priority 100 --source-address-prefixes "${me_cidrs[@]}" \
    --destination-port-ranges 22 --protocol Tcp --access Allow -o none || die "nsg rule create failed"

# Follow this machine's public address: a connection that fails over between
# links gets a new one every few minutes, and SSH must keep working
follow_address() {
    local now known_ips=" $me "
    while sleep 20; do
        now=$(curl -fsS -m 5 https://api.ipify.org 2>/dev/null) || continue
        [[ $known_ips == *" $now "* ]] && continue
        known_ips="$known_ips$now "
        # shellcheck disable=SC2086
        az network nsg rule update -g "$group" --nsg-name nsg -n ssh \
            --source-address-prefixes $(printf '%s/32 ' $known_ips) -o none &&
            log "SSH now also allowed from $now"
    done
}
follow_address & follow=$!

declare_node() { # name sender-ip receiver-ip size
    local n=$1
    az network public-ip create -g "$group" -n "$n-ip" --sku Standard -o none &&
        az network nic create -g "$group" -n "$n-mgmt" --vnet-name vnet --subnet mgmt --network-security-group nsg \
            --public-ip-address "$n-ip" --accelerated-networking true -o none &&
        az network nic create -g "$group" -n "$n-snd" --vnet-name vnet --subnet data --private-ip-address "$2" \
            --accelerated-networking true -o none &&
        az network nic create -g "$group" -n "$n-rcv" --vnet-name vnet --subnet data --private-ip-address "$3" \
            --accelerated-networking true -o none &&
        az vm create -g "$group" -n "$n" --size "$4" --image "$image" --ppg ppg \
            --nics "$n-mgmt" "$n-snd" "$n-rcv" --admin-username "$user" --ssh-key-values "$key" \
            --os-disk-size-gb 64 -o none
}
declare_node a 10.20.1.4 10.20.1.5 "$size_a" & pa=$!
declare_node b 10.20.1.6 10.20.1.7 "$size_b" & pb=$!
wait $pa || die "node a create failed"
wait $pb || die "node b create failed"
A=$(az network public-ip show -g "$group" -n a-ip --query ipAddress -o tsv)
B=$(az network public-ip show -g "$group" -n b-ip --query ipAddress -o tsv)
a_private=$(az network nic show -g "$group" -n a-mgmt --query 'ipConfigurations[0].privateIPAddress' -o tsv)
b_private=$(az network nic show -g "$group" -n b-mgmt --query 'ipConfigurations[0].privateIPAddress' -o tsv)
log "a=$A b=$B (private $a_private $b_private)"

data_subnet=10.20.1.0/24
. "$here/../lib.sh"
prepare_vms
run_bench
