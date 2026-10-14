#!/usr/bin/env bash
# Kernel UDP against DPDK (ENA) for the Aeron media driver, on two EC2 instances
# that exist only while this script runs. Runs on macOS or Linux with the aws
# CLI signed in (`aws login`) and an SSH public key (~/.ssh/id_ed25519.pub or
# id_rsa.pub).
#
#   md-dpdk/benches/cloud/aws/lab.sh                                   c7i.2xlarge
#   LAB_TYPE=c7i.metal-24xl md-dpdk/benches/cloud/aws/lab.sh
#   LAB_PLACEMENT=precision-time md-dpdk/benches/cloud/aws/lab.sh        NIC RX timestamps
#
# Creates, in $LAB_REGION, a VPC of its own (10.30.0.0/16) with a management
# subnet (10.30.0.0/24, an internet gateway, SSH from this machine's address
# only) and a data subnet (10.30.1.0/24), a cluster placement group (with
# LAB_PLACEMENT=precision-time, nested in a precision-time parent group, so the
# instances also get the Nitro PTP hardware clock and NIC RX timestamps), an
# imported key pair, and two $LAB_TYPE instances (Debian 13), each with its
# primary ENI on the management subnet and two ENIs attached on the data
# subnet: the media driver's sender port (a .4, b .6) and receiver port (a .5,
# b .7). Everything carries the tags owner=ergon-lab and run=<stamp>, and is
# deleted on any exit, then the deletion is verified by tag. The benchmark
# itself is ../lib.sh and ../vm.sh, as on Azure.
#
#   LAB_REGION (us-east-1) LAB_TYPE (c7i.2xlarge; LAB_TYPE_A, LAB_TYPE_B per instance:
#   a hosts the localhost runs, which need five cores, so c7i.4xlarge for those)
#   LAB_REPS (5) LAB_PLACEMENT (cluster)
#   LAB_MODES ("kernel kernel-busy dpdk") LAB_STATES ("pinned isolated")
#   LAB_LOCAL_MODES, LAB_LOAD_SIZES, LAB_LOAD_FRACTIONS, LAB_LOAD_SECONDS,
#   LAB_LOAD_LADDER, LAB_MIX, LAB_MIX_SECONDS: as azure/lab.sh
#   LAB_KEEP=1 keeps everything (delete it with LAB_DELETE=<stamp> lab.sh);
#   LAB_REPS=0 with LAB_KEEP=1 stops after the build, for work by hand
#   RTT_MESSAGES (300000) RTT_WARMUP (50000) TPUT_SECONDS (5)
set -uo pipefail
export AWS_PAGER=""

here=$(cd "$(dirname "$0")" && pwd)
repo=${LAB_REPO:-$(cd "$here/../../../.." && pwd)}
# Run from a snapshot of these scripts: bash reads a script as it runs, so an
# edit to the checkout mid-run would otherwise break this run (and its teardown).
if [[ -z ${LAB_SNAPSHOT:-} ]]; then
    snapshot=$(mktemp -d)
    cp -R "$here/.." "$snapshot/cloud"
    LAB_SNAPSHOT=$snapshot LAB_REPO=$repo exec bash "$snapshot/cloud/${here##*/}/${0##*/}" "$@"
fi
region=${LAB_REGION:-us-east-1}
type=${LAB_TYPE:-c7i.2xlarge}
type_a=${LAB_TYPE_A:-$type}
type_b=${LAB_TYPE_B:-$type}
# x86 only: production runs on x86. Graviton families carry a `g` after the
# generation (c7g, c7gn, m8g, ...).
for t in "$type_a" "$type_b"; do
    if [[ ${t%%.*} =~ ^[a-z]+[0-9]+g ]]; then
        echo "LAB_TYPE $t is a Graviton (Arm) type: the benchmarks run on x86 only" >&2
        exit 2
    fi
done
placement=${LAB_PLACEMENT:-cluster}
[[ $placement == cluster || $placement == precision-time ]] || { echo "LAB_PLACEMENT is cluster or precision-time" >&2; exit 2; }
reps=${LAB_REPS:-5}
read -ra modes <<<"${LAB_MODES:-kernel kernel-busy dpdk}"
read -ra states <<<"${LAB_STATES:-pinned isolated}"
local_modes=${LAB_LOCAL_MODES:-}
load_sizes=${LAB_LOAD_SIZES:-}
load_fractions=${LAB_LOAD_FRACTIONS:-10 50 80 95 100}
load_seconds=${LAB_LOAD_SECONDS:-5}
load_ladder=${LAB_LOAD_LADDER:-}
mix=${LAB_MIX:-}
mix_seconds=${LAB_MIX_SECONDS:-10}
rtt_messages=${RTT_MESSAGES:-300000}
rtt_warmup=${RTT_WARMUP:-50000}
tput_seconds=${TPUT_SECONDS:-5}
user=admin
stamp=${LAB_DELETE:-$(date +%Y%m%d-%H%M%S)}
out=$repo/target/md-dpdk-bench/aws-$stamp
known=$out/known_hosts
data_subnet=10.30.1.0/24
created=0

log() { echo "[$(date +%T)] $*" >&2; }
die() { log "ERROR: $*"; exit 1; }
ec2() { aws ec2 --region "$region" "$@"; }
tags() { echo "ResourceType=$1,Tags=[{Key=owner,Value=ergon-lab},{Key=run,Value=$stamp},{Key=Name,Value=ergon-md-dpdk-$2}]"; }
tagged() { echo "Name=tag:run,Values=$stamp"; }

command -v aws >/dev/null || die "aws is not on PATH"
aws sts get-caller-identity >/dev/null 2>&1 || die "aws is not signed in (aws login)"
mkdir -p "$out"

# Delete everything tagged with this run, in dependency order, then list again
# by tag and report what (if anything) is left.
teardown_run() {
    log "deleting run $stamp in $region"
    local ids id
    ids=$(ec2 describe-instances --filters "$(tagged)" "Name=instance-state-name,Values=pending,running,stopping,stopped" \
        --query 'Reservations[].Instances[].InstanceId' --output text)
    if [[ -n $ids ]]; then
        ec2 terminate-instances --instance-ids $ids >/dev/null
        ec2 wait instance-terminated --instance-ids $ids
    fi
    for id in $(ec2 describe-network-interfaces --filters "$(tagged)" --query 'NetworkInterfaces[].NetworkInterfaceId' --output text); do
        ec2 delete-network-interface --network-interface-id "$id" 2>/dev/null
    done
    local vpc
    vpc=$(ec2 describe-vpcs --filters "$(tagged)" --query 'Vpcs[0].VpcId' --output text)
    if [[ -n $vpc && $vpc != None ]]; then
        for id in $(ec2 describe-internet-gateways --filters "Name=attachment.vpc-id,Values=$vpc" --query 'InternetGateways[].InternetGatewayId' --output text); do
            ec2 detach-internet-gateway --internet-gateway-id "$id" --vpc-id "$vpc"
            ec2 delete-internet-gateway --internet-gateway-id "$id"
        done
        for id in $(ec2 describe-subnets --filters "Name=vpc-id,Values=$vpc" --query 'Subnets[].SubnetId' --output text); do
            ec2 delete-subnet --subnet-id "$id"
        done
        for id in $(ec2 describe-route-tables --filters "Name=vpc-id,Values=$vpc" \
            --query 'RouteTables[?!Associations[?Main]].RouteTableId' --output text); do
            ec2 delete-route-table --route-table-id "$id"
        done
        for id in $(ec2 describe-security-groups --filters "Name=vpc-id,Values=$vpc" \
            --query 'SecurityGroups[?GroupName!=`default`].GroupId' --output text); do
            ec2 delete-security-group --group-id "$id"
        done
        ec2 delete-vpc --vpc-id "$vpc"
    fi
    ec2 delete-placement-group --group-name "ergon-md-dpdk-$stamp" 2>/dev/null
    ec2 delete-placement-group --group-name "ergon-md-dpdk-pt-$stamp" 2>/dev/null
    ec2 delete-key-pair --key-name "ergon-md-dpdk-$stamp" 2>/dev/null
    # verify: nothing billable (or otherwise) left with this run's tag
    local left
    left=$( {
        ec2 describe-instances --filters "$(tagged)" "Name=instance-state-name,Values=pending,running,stopping,stopped" \
            --query 'Reservations[].Instances[].InstanceId' --output text
        ec2 describe-volumes --filters "$(tagged)" --query 'Volumes[].VolumeId' --output text
        ec2 describe-network-interfaces --filters "$(tagged)" --query 'NetworkInterfaces[].NetworkInterfaceId' --output text
        ec2 describe-addresses --filters "$(tagged)" --query 'Addresses[].AllocationId' --output text
        ec2 describe-vpcs --filters "$(tagged)" --query 'Vpcs[].VpcId' --output text
        ec2 describe-placement-groups --filters "$(tagged)" --query 'PlacementGroups[].GroupName' --output text
        ec2 describe-key-pairs --filters "$(tagged)" --query 'KeyPairs[].KeyName' --output text
    } 2>/dev/null | tr -s ' \t\n' ' ')
    if [[ -z ${left// /} ]]; then
        log "verified: nothing tagged run=$stamp remains in $region"
    else
        log "WARNING: still present for run $stamp: $left (rerun with LAB_DELETE=$stamp $0)"
    fi
}

if [[ -n ${LAB_DELETE:-} ]]; then
    teardown_run
    exit 0
fi

teardown() {
    local rc=$?
    [[ -n ${follow:-} ]] && kill "$follow" 2>/dev/null
    if ((created)) && [[ -z ${LAB_KEEP:-} ]]; then
        teardown_run
    elif ((created)); then
        log "LAB_KEEP: run $stamp kept; delete it with: LAB_DELETE=$stamp $0"
    fi
    log "results: $out"
    exit "$rc"
}
trap teardown EXIT
trap 'exit 130' INT TERM

key=
for k in ~/.ssh/id_ed25519.pub ~/.ssh/id_rsa.pub; do [[ -f $k ]] && { key=$k; break; }; done
[[ -n $key ]] || die "no ~/.ssh/id_ed25519.pub or ~/.ssh/id_rsa.pub"
# LAB_SSH_FROM: this machine's public addresses (space-separated), for a
# connection that fails over between links; default the current one
me=${LAB_SSH_FROM:-$(curl -fsS https://api.ipify.org)} || die "cannot find this machine's public address"
me=$(printf '%s\n' $me | sort -u | tr '\n' ' ') # a repeated address is an error to Azure

# the availability zones that offer both types
offered() { ec2 describe-instance-type-offerings --location-type availability-zone \
    --filters "Name=instance-type,Values=$1" --query 'InstanceTypeOfferings[].Location' --output text | tr '\t' '\n' | sort; }
zones=$(comm -12 <(offered "$type_a") <(offered "$type_b"))
[[ -n $zones ]] || die "$type_a and $type_b are not both offered in one zone of $region"
ami=$(ec2 describe-images --owners 136693071363 --filters "Name=name,Values=debian-13-amd64-*" "Name=architecture,Values=x86_64" \
    --query 'sort_by(Images,&CreationDate)[-1].ImageId' --output text)
[[ -n $ami && $ami != None ]] || die "no Debian 13 AMI in $region"
log "run $stamp: a $type_a, b $type_b in $region (AMI $ami), SSH from $me"

created=1
vpc=$(ec2 create-vpc --cidr-block 10.30.0.0/16 --tag-specifications "$(tags vpc vpc)" --query Vpc.VpcId --output text) || die "vpc"
igw=$(ec2 create-internet-gateway --tag-specifications "$(tags internet-gateway igw)" \
    --query InternetGateway.InternetGatewayId --output text) || die "igw"
ec2 attach-internet-gateway --internet-gateway-id "$igw" --vpc-id "$vpc" || die "igw attach"
rt=$(ec2 create-route-table --vpc-id "$vpc" --tag-specifications "$(tags route-table rt)" --query RouteTable.RouteTableId --output text) || die "rt"
ec2 create-route --route-table-id "$rt" --destination-cidr-block 0.0.0.0/0 --gateway-id "$igw" >/dev/null || die "route"
sg=$(ec2 create-security-group --vpc-id "$vpc" --group-name "ergon-md-dpdk-$stamp" --description "ergon md-dpdk bench $stamp" \
    --tag-specifications "$(tags security-group sg)" --query GroupId --output text) || die "sg"
for ip in $me; do
    ec2 authorize-security-group-ingress --group-id "$sg" --protocol tcp --port 22 --cidr "$ip/32" >/dev/null || die "sg ssh"
done
# Follow this machine's public address: a connection that fails over between
# links gets a new one every few minutes, and SSH must keep working
follow_address() {
    local now known_ips=" $me "
    while sleep 20; do
        now=$(curl -fsS -m 5 https://api.ipify.org 2>/dev/null) || continue
        [[ $known_ips == *" $now "* ]] && continue
        known_ips="$known_ips$now "
        ec2 authorize-security-group-ingress --group-id "$sg" --protocol tcp --port 22 --cidr "$now/32" >/dev/null &&
            log "SSH now also allowed from $now"
    done
}
follow_address & follow=$!
ec2 authorize-security-group-ingress --group-id "$sg" --protocol -1 --cidr 10.30.0.0/16 >/dev/null || die "sg vpc"
ec2 import-key-pair --key-name "ergon-md-dpdk-$stamp" --public-key-material "fileb://$key" \
    --tag-specifications "$(tags key-pair key)" >/dev/null || die "key pair"
parent=()
if [[ $placement == precision-time ]]; then
    # PHC-equipped hosts (precision-time) as the parent, the cluster group
    # inside it: NIC timestamps without giving up cluster placement
    pt=$(ec2 create-placement-group --group-name "ergon-md-dpdk-pt-$stamp" --strategy precision-time \
        --tag-specifications "$(tags placement-group pg-pt)" --query PlacementGroup.GroupId --output text) ||
        die "precision-time placement group"
    parent=(--parent-group-id "$pt")
fi
ec2 create-placement-group --group-name "ergon-md-dpdk-$stamp" --strategy cluster "${parent[@]}" \
    --tag-specifications "$(tags placement-group pg)" >/dev/null || die "placement group"

# Launch both instances in one zone, trying each zone that offers the type
# until one has the capacity.
launched=
for zone in $zones; do
    mgmt=$(ec2 create-subnet --vpc-id "$vpc" --cidr-block 10.30.0.0/24 --availability-zone "$zone" \
        --tag-specifications "$(tags subnet mgmt)" --query Subnet.SubnetId --output text) || die "mgmt subnet"
    ec2 modify-subnet-attribute --subnet-id "$mgmt" --map-public-ip-on-launch
    ec2 associate-route-table --route-table-id "$rt" --subnet-id "$mgmt" >/dev/null
    data=$(ec2 create-subnet --vpc-id "$vpc" --cidr-block "$data_subnet" --availability-zone "$zone" \
        --tag-specifications "$(tags subnet data)" --query Subnet.SubnetId --output text) || die "data subnet"
    launch() { # type: one instance, its id
        ec2 run-instances --image-id "$ami" --instance-type "$1" --count 1 --key-name "ergon-md-dpdk-$stamp" \
            --subnet-id "$mgmt" --security-group-ids "$sg" --placement "GroupName=ergon-md-dpdk-$stamp" \
            --block-device-mappings 'DeviceName=/dev/xvda,Ebs={VolumeSize=40,VolumeType=gp3,DeleteOnTermination=true}' \
            --metadata-options HttpTokens=required \
            --tag-specifications "$(tags instance node)" "$(tags volume disk)" "$(tags network-interface mgmt-eni)" \
            --query 'Instances[0].InstanceId' --output text 2>>"$out/run-instances-$zone.log"
    }
    if ida=$(launch "$type_a"); then
        if idb=$(launch "$type_b"); then
            ids="$ida $idb"
            launched=$zone
            break
        fi
        # a alone in this zone: give it back before trying the next
        ec2 terminate-instances --instance-ids "$ida" >/dev/null
        ec2 wait instance-terminated --instance-ids "$ida"
    fi
    log "no capacity for $type_a and $type_b in $zone: $(tail -1 "$out/run-instances-$zone.log")"
    ec2 delete-subnet --subnet-id "$mgmt"
    ec2 delete-subnet --subnet-id "$data"
done
[[ -n $launched ]] || die "no zone had capacity for $type_a and $type_b"
read -r ida idb <<<"$ids"
log "instances $ida $idb in $launched; waiting for them to run"
ec2 wait instance-running --instance-ids "$ida" "$idb" || die "instances did not start"

# two data ENIs each, attached at device indexes 1 and 2, deleted with the instance
attach() { # instance device-index ip
    local eni att
    eni=$(ec2 create-network-interface --subnet-id "$data" --groups "$sg" --private-ip-address "$3" \
        --tag-specifications "$(tags network-interface data-eni)" --query NetworkInterface.NetworkInterfaceId --output text) || return 1
    att=$(ec2 attach-network-interface --network-interface-id "$eni" --instance-id "$1" --device-index "$2" \
        --query AttachmentId --output text) || return 1
    ec2 modify-network-interface-attribute --network-interface-id "$eni" \
        --attachment "AttachmentId=$att,DeleteOnTermination=true" || return 1
}
attach "$ida" 1 10.30.1.4 && attach "$ida" 2 10.30.1.5 && attach "$idb" 1 10.30.1.6 && attach "$idb" 2 10.30.1.7 ||
    die "data ENIs"
A=$(ec2 describe-instances --instance-ids "$ida" --query 'Reservations[0].Instances[0].PublicIpAddress' --output text)
B=$(ec2 describe-instances --instance-ids "$idb" --query 'Reservations[0].Instances[0].PublicIpAddress' --output text)
a_private=$(ec2 describe-instances --instance-ids "$ida" --query 'Reservations[0].Instances[0].PrivateIpAddress' --output text)
b_private=$(ec2 describe-instances --instance-ids "$idb" --query 'Reservations[0].Instances[0].PrivateIpAddress' --output text)
log "a=$A b=$B"
{ echo "type a $type_a b $type_b"; echo "zone $launched"; echo "ami $ami"; echo "placement $placement"; } >"$out/instances.txt"

. "$here/../lib.sh"
prepare_vms
run_bench
