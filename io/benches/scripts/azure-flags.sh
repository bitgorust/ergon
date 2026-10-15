#!/usr/bin/env bash
# The settings matrix on Azure (scripts/lib-matrix.sh): every host, kernel, NIC
# and socket setting under test, with platform noise, round trips, HTTP and a
# market-data burst per condition, each run traced so every stall can be
# attributed. Run it twice at once (two STAMPs) to replicate across VM instances.
#   MODE=matrix (default) | base | confirm | diagnose | sqtick | http | md | many   (base: the baseline set only;
#   confirm: stage 2, CONFIRM="base;<spec>..." at 1,000/s, 15 minutes each;
#   diagnose: hwlat, slow syscalls, slow kernel functions and the guest-agent
#   A/B, with the loop on a CPU the gap map saw stall; sqtick: the SQPOLL CPU with and
#   without nohz_full, boot profiles full and full-sqtick, A/B/A/B; http: ergon-io (every backend),
#   libcurl (distro, source) and reqwest; md: market data, two backends paired per message;
#   many: 100 connections with mixed trickle and bursts, each backend alone)
#
# One resource group per run (ergon-lab-ioflags-<stamp>), deleted on every exit
# path and verified gone; the group is listed every 15 minutes while it exists.
# Results go to results/flags-azure-<stamp>/.
set -euo pipefail
# Run from a private copy: editing this file while a run is going cannot
# reach the run (bash reads a script as it executes it).
if [[ -z ${ERGON_LAB_HOME:-} ]]; then
  ERGON_LAB_HOME=$(cd "$(dirname "$0")/.." && pwd)
  export ERGON_LAB_HOME
  snap=$(mktemp "${TMPDIR:-/tmp}/ergon-lab-$(basename "$0").XXXXXX")
  cp "$0" "$snap"
  exec bash "$snap" "$@"
fi
cd "$ERGON_LAB_HOME"

STAMP=${STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}
LOC=${LOC:-eastasia}
RG=ergon-lab-ioflags-$STAMP
IMAGE=${IMAGE:-Canonical:ubuntu-24_04-lts:server:latest}
RCV_SIZE=${RCV_SIZE:-Standard_D8ds_v6}
SND_SIZE=${SND_SIZE:-Standard_D2ds_v6}
MODE=${MODE:-matrix}
OUT=results/flags-azure-$STAMP
RUSER=azureuser
mkdir -p "$OUT/ssh"
# The results are analysed here at the end (collect_results): build the
# analysis first, so a broken build stops the run before anything is created.
ANALYSE=$PWD/target/release/analyse
cargo build --release -q --bin analyse || { echo "analyse does not build; nothing created"; exit 1; }
cp "$ANALYSE" "$OUT/analyse.bin" && ANALYSE=$PWD/$OUT/analyse.bin
# Snapshot the source now: later edits in the working tree must not reach this run.
( cd ../.. && COPYFILE_DISABLE=1 tar --no-xattrs --exclude target --exclude results -czf "$OLDPWD/$OUT/src.tgz" \
    Cargo.toml rust-toolchain.toml io/Cargo.toml io/src \
    io/benches/Cargo.toml io/benches/Cargo.lock io/benches/src io/benches/tests io/benches/scripts )
# The shared scripts too: this run sources and copies its own snapshot.
mkdir -p "$OUT/scripts"
cp scripts/lib-cloud.sh scripts/lib-matrix.sh scripts/vm-setup.sh "$OUT/scripts/"
SCRIPTS=$OUT/scripts
LOG=$OUT/run.log
exec > >(trap '' INT TERM; exec tee -a "$LOG") 2>&1
MAX_S=${MAX_S:-36000}
( sleep "$MAX_S"; echo "[watchdog] ${MAX_S}s reached, stopping"; kill -TERM $$ ) &
WATCHDOG=$!

log() { echo "[$(date -u +%H:%M:%S)] $*"; }

teardown() {
  local rc=$?
  set +e
  trap '' PIPE INT TERM
  trap - EXIT
  kill "$WATCHDOG" "${BILLING:-}" 2>/dev/null
  if [[ $rc -ne 0 && -n ${RCV:-} ]]; then
    log "teardown: salvaging partial results"
    collect_results 2>/dev/null
  fi
  log "teardown: deleting resource group $RG (exit code $rc)"
  local gone=""
  for _ in 1 2 3 4 5 6; do
    az group delete -n "$RG" --yes 2>/dev/null
    [[ $(az group exists -n "$RG" 2>/dev/null) == false ]] && { gone=1; break; }
    sleep 30
  done
  if [[ -z $gone ]]; then log "!!! RESOURCE GROUP $RG STILL EXISTS: delete it by hand"; rc=1; fi
  log "verify: group exists = $(az group exists -n "$RG")"
  log "verify: resources tagged run=$STAMP: $(az resource list --tag run="$STAMP" --query 'length(@)' -o tsv)"
  exit "$rc"
}
trap teardown EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'echo "[err] line $LINENO: \"$BASH_COMMAND\" exited $?"' ERR
set -E

KEY=$OUT/ssh/id_ed25519
[[ -f $KEY ]] || ssh-keygen -q -t ed25519 -N '' -f "$KEY" -C "ergon-lab-$STAMP"
SSHO=(-i "$KEY" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ServerAliveInterval=30 -o ServerAliveCountMax=20 -o ConnectTimeout=15 -o ConnectionAttempts=5)
MYIP=$(curl -s https://api.ipify.org)

log "existing groups before create:"; az group list --query '[].name' -o tsv
log "creating $RG in $LOC (receiver $RCV_SIZE, sender $SND_SIZE, mode $MODE)"
TAGS=(owner=ergon-lab run="$STAMP")
az group create -n "$RG" -l "$LOC" --tags "${TAGS[@]}" -o none
# Every 15 minutes while the group exists: what is running.
( while sleep 900; do
    log "[billing] $RG: $(az vm list -g "$RG" -d --query '[].{n:name,s:powerState}' -o tsv 2>/dev/null | tr '\n\t' '; ')"
  done ) &
BILLING=$!
az ppg create -g "$RG" -n ppg -t standard --tags "${TAGS[@]}" -o none
az network vnet create -g "$RG" -n vnet --address-prefix 10.23.0.0/16 --subnet-name s --subnet-prefix 10.23.1.0/24 --tags "${TAGS[@]}" -o none
az network nsg create -g "$RG" -n nsg --tags "${TAGS[@]}" -o none
az network nsg rule create -g "$RG" --nsg-name nsg -n ssh --priority 100 --source-address-prefixes "$MYIP/32" --destination-port-ranges 22 --access Allow --protocol Tcp -o none
for vm in rcv snd; do
  size=$RCV_SIZE; [[ $vm == snd ]] && size=$SND_SIZE
  az vm create -g "$RG" -n "$vm" --image "$IMAGE" --size "$size" --ppg ppg --vnet-name vnet --subnet s --nsg nsg \
    --accelerated-networking true --os-disk-size-gb 128 --admin-username "$RUSER" --ssh-key-values "$KEY.pub" --public-ip-sku Standard \
    --os-disk-delete-option Delete --nic-delete-option Delete --data-disk-delete-option Delete --tags "${TAGS[@]}" -o none
done
RCV=$(az vm show -d -g "$RG" -n rcv --query publicIps -o tsv)
SND=$(az vm show -d -g "$RG" -n snd --query publicIps -o tsv)
SND_PRIV=$(az vm show -d -g "$RG" -n snd --query privateIps -o tsv)
log "rcv $RCV  snd $SND (private $SND_PRIV)"

# shellcheck source=lib-cloud.sh
source "$OUT/scripts/lib-cloud.sh"
# shellcheck source=lib-matrix.sh
source "$OUT/scripts/lib-matrix.sh"
prepare_hosts
case $MODE in
  base) run_base_only ;;
  confirm) run_confirm ;;
  diagnose) run_diagnose ;;
  sqtick) run_sqtick ;;
  http) run_http ;;
  md) run_md ;;
  many) run_many ;;
  *) run_matrix ;;
esac
collect_results
log "done; tables in $OUT/tables.md"
