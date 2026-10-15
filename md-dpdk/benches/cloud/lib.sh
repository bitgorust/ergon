# Shared by md-dpdk/benches/cloud/azure/lab.sh and md-dpdk/benches/cloud/aws/lab.sh, sourced
# after the cloud's VMs exist. The caller sets: A and B (the VMs' public
# addresses), user (their admin user), out (the results directory), known (its
# known_hosts), data_subnet, reps, modes and states (arrays), rtt_messages,
# rtt_warmup, tput_seconds, a_private and b_private (the VMs' management
# addresses, which VM a drives the benchmark over); optionally placement (AWS:
# precision-time turns on the PTP hardware clock and NIC RX timestamps),
# ssh_key, base (LAB_BASE: a git revision of the checkout built next to it,
# into ~/bin-base on each VM, for modes with @BIN=bin-base) and tput_flows
# (LAB_TPUT_FLOWS: flows in the multi-flow sustained maximum, which runs only
# from 2). The functions run
# the benchmark on both VMs in lockstep; teardown stays with the caller's trap.

ssh_opts=(-o BatchMode=yes -o StrictHostKeyChecking=accept-new -o "UserKnownHostsFile=$known"
    -o ConnectTimeout=15 -o ServerAliveInterval=30 -o ServerAliveCountMax=10)
[[ -n ${ssh_key:-} ]] && ssh_opts+=(-i "$ssh_key")
vm=ergon/md-dpdk/benches/cloud/vm.sh

# on <host> <command>: over SSH. A connection failure (exit 255: this
# machine's link dropping or failing over) is retried for about 3 minutes;
# the remote command's own exit status is returned as is.
on() {
    local host=$1 rc try
    shift
    for try in $(seq 18); do
        ssh "${ssh_opts[@]}" "$user@$host" "$@"
        rc=$?
        ((rc != 255)) && return $rc
        sleep 10
    done
    return $rc
}

# both <what> <vm.sh args>: on both VMs in parallel, logging to $out/<node>-<what>.log
# With rtt_align (LAB_RTT_ALIGN), wait until 2 s before that second of the
# minute: the pong starts, and the ping 2 s later starts on it. A host event
# that recurs at a fixed second each minute (Azure: a whole-VM pause near :57)
# then falls outside every mode's run alike instead of in whichever it hits.
align_rtt() {
    [[ -n ${rtt_align:-} ]] || return 0
    sleep "$(python3 -c 'import sys, time; print(round((float(sys.argv[1]) - 2 - time.time()) % 60, 3))' "$rtt_align")"
}

both() {
    local what=$1; shift
    on "$A" "$vm $*" >"$out/a-$what.log" 2>&1 & local p1=$!
    on "$B" "$vm $*" >"$out/b-$what.log" 2>&1 & local p2=$!
    local rc=0
    wait $p1 || { rc=1; log "a: vm.sh $* failed ($out/a-$what.log)"; tail -20 "$out/a-$what.log" >&2; }
    wait $p2 || { rc=1; log "b: vm.sh $* failed ($out/b-$what.log)"; tail -20 "$out/b-$what.log" >&2; }
    return $rc
}

# snap <what> [nodes]: vm.sh counters on VM a and VM b (or the nodes given) at
# once, each to $out/<node>-counters-<what>.log under a "# realtime=" line:
# taken just before and after a flat-out run, what the transport and the NICs
# counted over it (analyse.py's throughput phase frames)
snap() {
    local what=$1 node host pids=()
    for node in ${2:-a b}; do
        [[ $node == a ]] && host=$A || host=$B
        { echo "# realtime=$(date +%s.%N)"; on "$host" "$vm counters" 2>&1; } >"$out/$node-counters-$what.log" & pids+=($!)
    done
    wait "${pids[@]}" 2>/dev/null || true
}

# file_mode <mode>: a mode as file names spell it, a / (BIN's directory) as _
file_mode() { printf '%s' "${1//\//_}"; }

# detached <what> <vm.sh args>: like both, but each VM runs the step under
# nohup and we poll for its exit code, so a dropped SSH session (a NIC
# reconfiguring during apt, say) cannot kill a long step
detached() {
    local what=$1; shift
    local host node rc=0 status
    for host in "$A" "$B"; do
        on "$host" "rm -f bench/$what.rc; nohup bash -c '$vm $*; echo \$? >bench/$what.rc' >bench/$what.log 2>&1 </dev/null &" ||
            { log "cannot start vm.sh $* on $host"; return 1; }
    done
    for host in "$A" "$B"; do
        node=a; [[ $host == "$B" ]] && node=b
        status=
        for _ in $(seq 720); do
            status=$(on "$host" "cat bench/$what.rc 2>/dev/null" 2>/dev/null) && [[ -n $status ]] && break
            sleep 10
        done
        on "$host" "cat bench/$what.log" >"$out/$node-$what.log" 2>&1 || true
        if [[ $status != 0 ]]; then
            rc=1
            log "$node: vm.sh $* failed (exit ${status:-unknown}; $out/$node-$what.log)"
            tail -20 "$out/$node-$what.log" >&2
        fi
    done
    return $rc
}

wait_ssh() {
    local host
    for host in "$A" "$B"; do
        for _ in $(seq 120); do on "$host" true 2>/dev/null && break; sleep 5; done
        on "$host" true || die "no SSH to $host"
    done
}

# reboot both VMs and wait for them to come back
reboot_both() {
    local host
    for host in "$A" "$B"; do on "$host" 'sudo systemctl reboot' 2>/dev/null || true; done
    sleep 30
    wait_ssh
}

# copy the checkout (tracked and new files; not the upstream submodules), and
# with base (LAB_BASE) that revision's tracked files (git archive: the VMs get
# no git history), then bootstrap, reboot into the backports kernel and build
prepare_vms() {
    wait_ssh
    log "copying the checkout"
    (cd "$repo" && git ls-files -co --exclude-standard | grep -v -e '^aeron$' -e '^simple-binary-encoding$' |
        COPYFILE_DISABLE=1 tar --no-xattrs -cf - -T -) >"$out/ergon.tar" || die "cannot pack the checkout"
    local tars=(ergon.tar)
    if [[ -n ${base:-} ]]; then
        git -C "$repo" rev-parse --verify "$base^{commit}" >"$out/base-commit" || die "LAB_BASE $base: not a commit"
        git -C "$repo" archive --format=tar "$base" >"$out/ergon-base.tar" || die "cannot pack LAB_BASE $base"
        tars+=(ergon-base.tar)
    fi
    local host tar
    for host in "$A" "$B"; do
        for tar in "${tars[@]}"; do
            local try copied=
            for try in 1 2 3 4 5; do
                scp -q "${ssh_opts[@]}" "$out/$tar" "$user@$host:$tar" && { copied=1; break; }
                sleep 15
            done
            [[ -n $copied ]] || die "copy of $tar to $host failed"
        done
        on "$host" "rm -rf ergon bench && mkdir -p ergon bench && echo $data_subnet >bench/data_subnet && echo ${placement:-} >bench/placement && tar -xf ergon.tar -C ergon && rm ergon.tar" ||
            die "unpack on $host failed"
        if [[ -n ${base:-} ]]; then
            on "$host" "rm -rf ergon-base bin-base && mkdir -p ergon-base && tar -xf ergon-base.tar -C ergon-base && rm ergon-base.tar" ||
                die "unpack of LAB_BASE on $host failed"
        fi
    done
    git -C "$repo" rev-parse HEAD >"$out/commit"
    git -C "$repo" status --porcelain >"$out/dirty"
    log "bootstrap"
    detached bootstrap bootstrap || die "bootstrap failed"
    reboot_both
    log "build"
    detached build build || die "build failed"
    if [[ -n ${base:-} ]]; then
        log "build LAB_BASE $base ($(cat "$out/base-commit")) into ~/bin-base"
        detached build-base build-base || die "LAB_BASE build failed"
    fi
    # precision-time: boot into the PHC-enabled ENA driver the build installed
    [[ ${placement:-} == precision-time ]] && reboot_both
    both tune-boot tune || die "tune failed"
    # each host's environment, with the baseline every mode runs on top of
    # (once tune has set it) and the base build, if any
    for host in "$A" "$B"; do
        on "$host" "echo \"kernel \$(uname -r)\"; echo \"dpdk \$(pkg-config --modversion libdpdk)\";
            echo \"cpu \$(lscpu | sed -n 's/^Model name: *//p')\"; echo \"cpus \$(nproc)\";
            echo \"rustc \$(~/.cargo/bin/rustc -V)\"; $vm layout; $vm nics; $vm ptp; $vm baseline" >"$out/env-$host.txt" 2>&1
        if [[ -n ${base:-} ]]; then
            echo "base $base $(cat "$out/base-commit") in ~/bin-base (@BIN=bin-base)" >>"$out/env-$host.txt"
        fi
    done
    both clockcheck-boot clockcheck 120 || log "WARNING: clockcheck failed"
    both criterion-boot criterion || log "WARNING: criterion failed"
}

# the benchmark: every state in $states, each state's reps run on VM a by
# remote.sh (bench_state) over the private network, so this machine's
# connection only has to start a state and collect it; the isolated state's
# reboot is driven from here. Results to $out/results.csv.
run_bench() {
    results=$out/results.csv
    if ((reps == 0)); then
        log "LAB_REPS=0: VMs built, no benchmark run (a=$A b=$B, known_hosts $known)"
        return 0
    fi
    remote_access
    local state
    for state in "${states[@]}"; do
        if [[ $state == isolated ]]; then
            log "state isolated: kernel command line, reboot"
            both isolate isolate || die "isolate failed"
            reboot_both
            both tune-isolated tune || die "tune failed"
            both clockcheck-isolated clockcheck 120 || log "WARNING: clockcheck failed"
            both criterion-isolated criterion || log "WARNING: criterion failed"
        fi
        # a host that stalls the VM shows up here, before hours are spent on it
        both "preflight-$state" preflight 120 || log "WARNING: preflight failed"
        grep -h "on_several_cpus" "$out/a-preflight-$state.log" "$out/b-preflight-$state.log" >&2 || true
        both "diag-start-$state" diag-start || log "WARNING: diagnostics did not start"
        remote_state "$state" || die "$state benchmark failed"
        both "diag-$state" diag-fetch || log "WARNING: diagnostics not fetched"
        both "diag-stop-$state" diag-stop || true
    done
    python3 "$repo/md-dpdk/benches/cloud/analyse.py" "$results" >"$out/summary.md"
    # every spike and every rep's max, attributed: host stall, guest preemption or unexplained
    for state in "${states[@]}"; do
        python3 "$repo/md-dpdk/benches/cloud/spikes.py" "$out/$state" >>"$out/summary.md" 2>&1 || true
    done
    cat "$out/summary.md"
}

# VM a's own key, authorised on both VMs, for remote.sh
remote_access() {
    local pub host
    on "$A" '[[ -f ~/.ssh/lab ]] || ssh-keygen -q -t ed25519 -N "" -f ~/.ssh/lab' || die "VM a key"
    pub=$(on "$A" 'cat ~/.ssh/lab.pub') || die "VM a key"
    for host in "$A" "$B"; do
        on "$host" "grep -qF '$pub' ~/.ssh/authorized_keys || echo '$pub' >>~/.ssh/authorized_keys" || die "authorise VM a on $host"
    done
}

# remote_state <state>: run remote.sh for the state on VM a, detached; poll
# until it finishes, then fetch its directory into $out and its results
remote_state() {
    local state=$1 dir=bench/remote-$1 status= last= line
    on "$A" "rm -rf $dir && mkdir -p $dir && cat >$dir/env" <<ENV || return 1
A=$a_private
B=$b_private
user=$user
reps=$reps
modes='${modes[*]}'
local_modes='${local_modes:-}'
load_sizes='${load_sizes:-}'
load_fractions='${load_fractions:-10 50 80 95 100}'
load_ladder='${load_ladder:-}'
load_seconds=${load_seconds:-5}
mix='${mix:-}'
mix_seconds=${mix_seconds:-10}
rtt_messages=$rtt_messages
rtt_warmup=$rtt_warmup
rtt_align='${rtt_align:-}'
tput_seconds=$tput_seconds
tput_flows='${tput_flows:-}'
ENV
    on "$A" "nohup bash ergon/md-dpdk/benches/cloud/remote.sh $1 >$dir/run.log 2>&1 </dev/null &" || return 1
    log "state $state: running on VM a ($dir)"
    mkdir -p "$out/$state"
    for i in $(seq 2880); do # up to 8 hours (load sweeps are long)
        sleep 10
        status=$(on "$A" "cat $dir/rc 2>/dev/null" 2>/dev/null) && [[ -n $status ]] && break
        # every 5 minutes, the results so far: an interrupted run keeps them
        ((i % 30 == 0)) && on "$A" "cat $dir/results.csv 2>/dev/null" >"$out/$state/results.partial.csv" 2>/dev/null
        line=$(on "$A" "grep '^\[' $dir/run.log | tail -1" 2>/dev/null) || continue
        [[ -n $line && $line != "$last" ]] && { echo "  a: $line" >&2; last=$line; }
    done
    mkdir -p "$out/$state"
    on "$A" "tar -C $dir -cf - ." | tar -xf - -C "$out/$state" || log "WARNING: could not fetch $dir"
    cat "$out/$state/results.csv" >>"$results" 2>/dev/null
    [[ $status == 0 ]] || { log "state $state on VM a: exit ${status:-unknown} ($out/$state/run.log)"; tail -20 "$out/$state/run.log" >&2; return 1; }
}

# loss_row <rep> <label> <host-name> <before> <after>: one results row of the
# loss counters' growth over a run (vm.sh loss before and after); a state, not
# a counter (routes-missing: data NICs without their route), as found after it
loss_row() {
    python3 - "$@" <<'PY' >>"$results"
import sys
rep, label, host, before, after = sys.argv[1:6]
parse = lambda s: dict(kv.split("=", 1) for kv in s.split(";") if "=" in kv)
b, a = parse(before), parse(after)
if not a or not b:  # a failed read: never an absolute count passed off as growth
    print(f"{rep},loss,{label},{host},invalid=1")
else:
    STATES = ("routes-missing",)
    print(f"{rep},loss,{label},{host}," + ";".join(
        f"{k}={int(a[k]) if k in STATES else int(a[k]) - int(b[k])}" for k in a if k in b))
PY
}

# load_sweep <state> <rep> <mode> <label> <far host> <far ip> <near ip> <near cpu> <far cpu>:
# with the drivers up in <mode> (as file names spell it: file_mode), for each
# size in $load_sizes: the one-way
# sustained maximum (tput, VM a publishing to the far host), then the round
# trip open loop (VM a pings, the far host pongs) at each fraction of it in
# $load_fractions and one burst run, each with its loss counters on both
# hosts. For localhost the far host is VM a and both addresses 127.0.0.1.
load_sweep() {
    local state=$1 rep=$2 mode=$3 label=$4 far=$5 far_ip=$6 near_ip=$7 near_cpu=$8 far_cpu=$9
    local size max frac rate before_a before_b pong sub tag port=22200 nodes=a
    [[ $far == "$B" ]] && nodes="a b"
    for size in ${load_sizes:-}; do
        tag=$state-$mode-$size-$rep
        # the sustained maximum's own loss (a flat-out run is where a ring
        # overflows), and its counters just before and after (frames per message)
        before_a=$(on "$A" "$vm loss" 2>/dev/null)
        [[ $far == "$B" ]] && before_b=$(on "$B" "$vm loss" 2>/dev/null)
        on "$far" "$vm client $label tput xsub $far_ip:20125 $tput_seconds $far_cpu" \
            2>"$out/maxsub-$tag.log" >"$out/max-$tag.csv" & sub=$!
        sleep 2
        snap "max-before-$tag" "$nodes"
        on "$A" "$vm client $label tput xpub $far_ip:20125 $((tput_seconds + 6)) $near_cpu $size" \
            >"$out/maxpub-$tag.log" 2>&1
        wait $sub || true
        snap "max-after-$tag" "$nodes"
        loss_row "$rep" "$label:$size:max" a "$before_a" "$(on "$A" "$vm loss" 2>/dev/null)"
        [[ $far == "$B" ]] && loss_row "$rep" "$label:$size:max" b "$before_b" "$(on "$B" "$vm loss" 2>/dev/null)"
        max=$(awk -F, '{printf "%d", $4}' "$out/max-$tag.csv")
        awk -F, -v rep="$rep" -v label="$label" -v size="$size" '{print rep ",tputmax," label "," size "," $4 "," $5 "," $6}' \
            "$out/max-$tag.csv" >>"$results"
        [[ -n $max && $max -gt 0 ]] || { log "WARNING: $label $size B: no sustained maximum"; continue; }
        # $load_ladder ("<size>:<rate>,<rate> ..."): absolute rates for a size,
        # where a fraction of a flat-out maximum means nothing (a maximum the
        # network cannot carry without loss, so it collapses); points r<rate>
        local ladder= points
        for entry in ${load_ladder:-}; do [[ ${entry%%:*} == "$size" ]] && ladder=${entry#*:}; done
        if [[ -n $ladder ]]; then
            points="$(tr , ' ' <<<"$ladder" | sed 's/[0-9][0-9]*/r&/g') burst"
        else
            points="${load_fractions:-} burst"
        fi
        for frac in $points; do
            if [[ $frac == r* ]]; then
                rate=${frac#r}
            elif [[ $frac == burst ]]; then
                rate=$((max / 2)) # half the maximum, in bursts of 1000
                [[ -n $ladder ]] && rate=$(tr , '\n' <<<"$ladder" | sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}')
            else
                rate=$((max * frac / 100))
            fi
            ((rate > 0)) || continue
            # fresh endpoints per point: no lingering session of the last one
            # (its clients were killed, so the driver holds them until liveness
            # times out) can be the image the new ones attach to
            local ping_ep=$far_ip:$port pong_ep=$near_ip:$((port + 1))
            port=$((port + 2))
            before_a=$(on "$A" "$vm loss" 2>/dev/null)
            [[ $far == "$B" ]] && before_b=$(on "$B" "$vm loss" 2>/dev/null)
            on "$far" "$vm client $label rtt xpong $ping_ep $pong_ep $far_cpu" \
                >"$out/lpong-$tag-$frac.log" 2>&1 & pong=$!
            sleep 2
            on "$A" "timeout $((2 * (load_seconds + 1) + 60)) $vm client $label load xping $ping_ep $pong_ep $rate $load_seconds $size $near_cpu $([[ $frac == burst ]] && echo 1000)" \
                2>"$out/load-$tag-$frac.log" | sed "s/^/$rep,/;s/,udp,/,udp-$frac,/" | tee -a "$results" >&2
            on "$A" 'cat bench/load-spikes.csv' >"$out/spikes-$tag-$frac.csv" 2>/dev/null || true
            # the pong ends by itself once the ping's image goes; then make sure
            for _ in $(seq 20); do kill -0 $pong 2>/dev/null || break; sleep 0.5; done
            on "$far" "sudo pkill -f '[r]tt xpong'" 2>/dev/null || true
            wait $pong 2>/dev/null || true
            loss_row "$rep" "$label:$size:$frac" a "$before_a" "$(on "$A" "$vm loss" 2>/dev/null)"
            [[ $far == "$B" ]] && loss_row "$rep" "$label:$size:$frac" b "$before_b" "$(on "$B" "$vm loss" 2>/dev/null)"
        done
    done
}

# flow_count <VM a's app CPUs> <VM b's app CPUs>: how many flows the
# multi-flow sustained maximum (multi_flow_max) runs: $tput_flows
# (LAB_TPUT_FLOWS), off unless asked for (unset, 0 or 1: 0, none), and at most
# one per app CPU on each VM (2 where both have a fifth core)
flow_count() {
    local near=($1) far=($2) most n=${tput_flows:-0}
    ((n >= 2)) || { echo 0; return; }
    most=$((${#near[@]} < ${#far[@]} ? ${#near[@]} : ${#far[@]}))
    if ((n > most)); then
        log "WARNING: LAB_TPUT_FLOWS=$n, but the layouts give $most app CPUs on each VM (a: $1, b: $2): $((most >= 2 ? most : 0)) flows"
        n=$most
    fi
    echo $((n >= 2 ? n : 0))
}

# multi_flow_max <state> <rep> <mode> <label> <far ip> <near cpus> <far cpus>:
# with the drivers up in <mode> (as file names spell it: file_mode), for 32 B
# and each size in $load_sizes, the sustained maximum over several flows at
# once: per CPU in <near cpus> and <far cpus>, a publication on VM a and its
# subscription on VM b, pinned there, on a channel of their own, so on a UDP
# port of their own: a flow of their own (one 5-tuple) to a cloud network.
# The subscribers start first, then every publisher at once, so their measured
# seconds should overlap; nothing makes them (on() retries a dropped SSH
# connection 10 s on, so a flow can start late and have the path to itself for
# a while, raising the sum), so each subscriber reports its measured window on
# the wall clock (tput's last two fields): all on VM b, one clock. With
# the counters on both hosts just before and after (frames per message), and
# the loss counters on both. Result rows: each flow's median msgs/s and their
# sum (a flow without a result left empty, and the sum too), then the flows'
# overlap, the span every flow's window shares over the shortest window,
# truncated to the thousandth so a run never reads as better overlapped than it
# was (empty when a flow reported no window; analyse.py counts a run from 0.9):
# <rep>,tputflows,<label>,<size>,<flows>,<sum>,<flow 1>,...,<flow n>,<overlap>
multi_flow_max() {
    local state=$1 rep=$2 mode=$3 label=$4 far_ip=$5 near=($6) far=($7)
    local n=${#near[@]} size tag i before_a before_b subs pubs sum flow each port=22700
    local start end latest earliest shortest windows span overlap
    for size in $(printf '%s\n' 32 ${load_sizes:-} | awk '!seen[$0]++'); do
        tag=$state-$mode-$size-$rep
        before_a=$(on "$A" "$vm loss" 2>/dev/null)
        before_b=$(on "$B" "$vm loss" 2>/dev/null)
        # fresh ports per size: no lingering session of the last size's
        subs=() pubs=()
        for i in $(seq "$n"); do
            on "$B" "$vm client $label tput xsub $far_ip:$((port + i - 1)) $tput_seconds ${far[i - 1]}" \
                2>"$out/flowsub-$tag-$i.log" >"$out/flows-$tag-$i.csv" & subs+=($!)
        done
        sleep 2
        snap "flows-before-$tag"
        for i in $(seq "$n"); do
            on "$A" "$vm client $label tput xpub $far_ip:$((port + i - 1)) $((tput_seconds + 6)) ${near[i - 1]} $size" \
                >"$out/flowpub-$tag-$i.log" 2>&1 & pubs+=($!)
        done
        port=$((port + n))
        wait "${pubs[@]}"
        wait "${subs[@]}"
        snap "flows-after-$tag"
        loss_row "$rep" "$label:$size:flows$n" a "$before_a" "$(on "$A" "$vm loss" 2>/dev/null)"
        loss_row "$rep" "$label:$size:flows$n" b "$before_b" "$(on "$B" "$vm loss" 2>/dev/null)"
        sum=0 each= latest= earliest= shortest= windows=1
        for i in $(seq "$n"); do
            flow= start= end=
            read -r flow start end < <(awk -F, '$1 == "tput" {printf "%d %s %s\n", $4, $9, $10; exit}' "$out/flows-$tag-$i.csv")
            if [[ -z $flow ]]; then
                log "WARNING: $label $size B, $n flows at once: no result from flow $i ($out/flowsub-$tag-$i.log)"
                sum= windows=
            elif [[ ! $start =~ ^[0-9]+$ || ! $end =~ ^[0-9]+$ ]] || ((end <= start)); then
                log "WARNING: $label $size B, $n flows at once: no measured window from flow $i (a tput client older than the window): overlap unknown"
                windows=
            else
                if [[ -z $latest ]] || ((start > latest)); then latest=$start; fi
                if [[ -z $earliest ]] || ((end < earliest)); then earliest=$end; fi
                if [[ -z $shortest ]] || ((end - start < shortest)); then shortest=$((end - start)); fi
            fi
            [[ -n $sum ]] && sum=$((sum + flow))
            each+=,$flow
        done
        overlap=
        if [[ -n $windows ]]; then
            # in thousandths, truncated
            span=$((earliest > latest ? earliest - latest : 0))
            span=$((span * 1000 / shortest))
            overlap=$((span / 1000)).$(printf '%03d' $((span % 1000)))
            ((span >= 900)) ||
                log "WARNING: $label $size B, $n flows at once: their measured windows overlapped $overlap of the shortest: analyse.py leaves the run out"
        fi
        echo "$rep,tputflows,$label,$size,$n,$sum$each,$overlap" >>"$results"
    done
}

# mix_run <state> <rep> <mode> <label> <far host> <far ip> <near ip> <near cpu> <far cpu>:
# for each profile in $mix ("<profile>=<name:stream:msgs/s:size[:burst]> ...;..."),
# every stream of the profile at once, open loop on its own schedule, on one
# pair of channels (VM a pings, the far host echoes each stream on stream + 1000),
# with loss counters on both hosts and the spikes (LOAD_SPIKE_NS) kept per run
# (<mode> as file names spell it: file_mode).
# Result rows: <rep>,mix,<label>,udp-<profile>/<stream name>,...
mix_run() {
    local state=$1 rep=$2 mode=$3 label=$4 far=$5 far_ip=$6 near_ip=$7 near_cpu=$8 far_cpu=$9
    local profile pname specs streams ping_ep pong_ep before_a before_b echo tag port=22800
    local IFS_SAVE=$IFS
    IFS=';'
    local profiles=(${mix:-})
    IFS=$IFS_SAVE
    for profile in "${profiles[@]}"; do
        [[ $profile == *=* ]] || { log "WARNING: mix profile '$profile' is not <name>=<specs>"; continue; }
        pname=${profile%%=*} specs=${profile#*=}
        streams=$(tr ' ' '\n' <<<"$specs" | awk -F: 'NF >= 4 {print $2}' | tr '\n' ' ')
        tag=$state-$mode-mix-$pname-$rep
        ping_ep=$far_ip:$port pong_ep=$near_ip:$((port + 1))
        port=$((port + 2))
        before_a=$(on "$A" "$vm loss" 2>/dev/null)
        [[ $far == "$B" ]] && before_b=$(on "$B" "$vm loss" 2>/dev/null)
        on "$far" "$vm client $label load xecho $ping_ep $pong_ep $far_cpu $streams" \
            >"$out/mecho-$tag.log" 2>&1 & echo=$!
        sleep 2
        on "$A" "timeout $((2 * (mix_seconds + 1) + 60)) $vm client $label load xmix $ping_ep $pong_ep $mix_seconds $near_cpu $specs" \
            2>"$out/mix-$tag.log" | sed "s/^/$rep,/;s/,udp-/,udp-$pname\//" | tee -a "$results" >&2
        on "$A" 'cat bench/load-spikes.csv' >"$out/spikes-$tag.csv" 2>/dev/null || true
        for _ in $(seq 20); do kill -0 $echo 2>/dev/null || break; sleep 0.5; done
        on "$far" "sudo pkill -f '[l]oad xecho'" 2>/dev/null || true
        wait $echo 2>/dev/null || true
        loss_row "$rep" "$label:mix:$pname" a "$before_a" "$(on "$A" "$vm loss" 2>/dev/null)"
        [[ $far == "$B" ]] && loss_row "$rep" "$label:mix:$pname" b "$before_b" "$(on "$B" "$vm loss" 2>/dev/null)"
    done
}

# bench_state <state>, on VM a (remote.sh): $reps reps of every mode in a fresh
# random order each rep, against VM a itself and VM b
bench_state() {
    local state=$1 rep mode fm label pong sub
    results=$out/results.csv
    a_rcv=$(on "$A" "$vm nics" 2>>"$out/nics.log" | awk '$1 == "receiver" {print $5}')
    b_rcv=$(on "$B" "$vm nics" 2>>"$out/nics.log" | awk '$1 == "receiver" {print $5}')
    [[ -n $a_rcv && -n $b_rcv ]] || { cat "$out/nics.log" >&2; die "cannot read the data NICs"; }
    app_a=$(on "$A" "$vm layout" | awk '{print $2}')
    app2_a=$(on "$A" "$vm layout" | awk '{print $5}')
    app_b=$(on "$B" "$vm layout" | awk '{print $2}')
    app2_b=$(on "$B" "$vm layout" | awk '{print $5}')
    log "receiver ports: a=$a_rcv b=$b_rcv; app CPUs: a=$app_a b=$app_b"
    # the multi-flow sustained maximum's flows (LAB_TPUT_FLOWS; none unless
    # asked for), each end of each on an app CPU of its own
    local cpus_a=($app_a $app2_a) cpus_b=($app_b $app2_b) flows
    flows=$(flow_count "${cpus_a[*]}" "${cpus_b[*]}")
    for rep in $(seq "$reps"); do
        for mode in $(printf '%s\n' "${modes[@]}" | python3 -c 'import random,sys; l=sys.stdin.read().split(); random.shuffle(l); print(" ".join(l))'); do
            label=$state:$mode
            fm=$(file_mode "$mode")
            log "$state rep $rep: $mode"
            # <mode>+idle: the same, with an idle localhost subscription held on
            # each driver throughout (what an unused host-local channel costs)
            both "driver-$state-$fm-$rep" driver "${mode%+idle}" || die "$mode drivers did not start"
            idle_pids=()
            if [[ $mode == *+idle ]]; then
                # held until killed below; one log per VM, both must be up first
                for n in a b; do
                    [[ $n == a ]] && host=$A || host=$B
                    on "$host" "$vm client $label rtt idle 127.0.0.1:22100 3600" \
                        >"$out/idle-$state-$fm-$rep-$n.log" 2>&1 & idle_pids+=($!)
                done
                for _ in $(seq 40); do
                    [[ $(cat "$out/idle-$state-$fm-$rep-"{a,b}.log 2>/dev/null | grep -c 'idle subscription') -ge 2 ]] && break
                    sleep 0.5
                done
                [[ $(cat "$out/idle-$state-$fm-$rep-"{a,b}.log 2>/dev/null | grep -c 'idle subscription') -ge 2 ]] ||
                    die "$mode rep $rep: the idle subscriptions did not start"
            fi
            both "counters-before-$state-$fm-$rep" counters || true
            # DPDK on AWS: whether the ENA's low-latency queue BAR is write-combining
            if [[ ${mode%%@*} == dpdk* ]]; then both "wc-$state-$fm-$rep" wc-check || true; fi
            both "jitter-before-rtt-$state-$fm-$rep" jitter || true
            loss_a=$(on "$A" "$vm loss" 2>/dev/null); loss_b=$(on "$B" "$vm loss" 2>/dev/null)
            # round trip: B pongs, A pings (ping's endpoint on B, pong's on A),
            # the ping started at second $rtt_align of the minute when set
            align_rtt
            on "$B" "$vm client $label rtt xpong $b_rcv:20123 $a_rcv:20124 $app_b" \
                >"$out/pong-$state-$fm-$rep.log" 2>&1 & pong=$!
            sleep 2
            on "$A" "RTT_SECONDS=20 $vm client $label rtt xping $b_rcv:20123 $a_rcv:20124 $rtt_messages $rtt_warmup $app_a" \
                2>"$out/ping-$state-$fm-$rep.log" | sed "s/^/$rep,/" | tee -a "$results" >&2
            on "$B" "sudo pkill -f '[r]tt xpong'" 2>/dev/null || true
            wait $pong 2>/dev/null || true
            on "$A" 'cat bench/rtt-histogram.csv' >"$out/histogram-$state-$fm-$rep.csv" 2>/dev/null || true
            on "$A" 'cat bench/load-spikes.csv' >"$out/spikes-$state-$fm-rtt-$rep.csv" 2>/dev/null || true
            on "$A" 'cat bench/stalls-ping.csv' >"$out/stalls-ping-$state-$fm-$rep.csv" 2>/dev/null || true
            on "$B" 'cat bench/stalls-pong.csv' >"$out/stalls-pong-$state-$fm-$rep.csv" 2>/dev/null || true
            both "jitter-after-rtt-$state-$fm-$rep" jitter || true
            # NIC arrival -> dispatch on each receiver port (DPDK with NIC RX timestamps only)
            for host in "$A" "$B"; do
                # (awk, not sed: a label may hold a /, BIN's directory)
                on "$host" "$vm rx-hw-delay" 2>/dev/null | awk -v p="$rep,rxhw,$label," '{print p $0}' >>"$results" || true
            done
            # the round trip's own loss (the row after the throughput run covers both)
            mid_a=$(on "$A" "$vm loss" 2>/dev/null); mid_b=$(on "$B" "$vm loss" 2>/dev/null)
            loss_row "$rep" "$label:rtt" a "$loss_a" "$mid_a"
            loss_row "$rep" "$label:rtt" b "$loss_b" "$mid_b"
            # throughput: A publishes, B subscribes, with the counters on both
            # hosts just before and after (its wire frames per message)
            on "$B" "$vm client $label tput xsub $b_rcv:20125 $tput_seconds $app_b" \
                2>"$out/sub-$state-$fm-$rep.log" | sed "s/^/$rep,/" >>"$results" & sub=$!
            sleep 2
            snap "tput-before-$state-$fm-$rep"
            on "$A" "$vm client $label tput xpub $b_rcv:20125 $((tput_seconds + 6)) $app_a" \
                >"$out/pub-$state-$fm-$rep.log" 2>&1
            wait $sub || log "WARNING: $mode rep $rep throughput subscriber failed"
            snap "tput-after-$state-$fm-$rep"
            tail -1 "$results" >&2
            both "counters-after-$state-$fm-$rep" counters || true
            loss_row "$rep" "$label" a "$loss_a" "$(on "$A" "$vm loss" 2>/dev/null)"
            loss_row "$rep" "$label" b "$loss_b" "$(on "$B" "$vm loss" 2>/dev/null)"
            # latency under load (LAB_LOAD_SIZES), the same drivers
            [[ -n ${load_sizes:-} && $mode != *+idle ]] &&
                load_sweep "$state" "$rep" "$fm" "$label" "$B" "$b_rcv" "$a_rcv" "$app_a" "$app_b"
            [[ -n ${mix:-} && $mode != *+idle ]] &&
                mix_run "$state" "$rep" "$fm" "$label" "$B" "$b_rcv" "$a_rcv" "$app_a" "$app_b"
            # several flows at once (LAB_TPUT_FLOWS), after everything above so
            # that runs as it did without it; the same drivers
            ((flows)) && [[ $mode != *+idle ]] &&
                multi_flow_max "$state" "$rep" "$fm" "$label" "$b_rcv" "${cpus_a[*]:0:flows}" "${cpus_b[*]:0:flows}"
            if ((${#idle_pids[@]})); then
                kill "${idle_pids[@]}" 2>/dev/null
                for host in "$A" "$B"; do on "$host" "sudo pkill -f '[r]tt idle'" 2>/dev/null || true; done
                wait "${idle_pids[@]}" 2>/dev/null || true
            fi
            both "errors-$state-$fm-$rep" errors || true
            if [[ ${mode%%@*} == dpdk* ]]; then both "pmd-$state-$fm-$rep" pmd-log || true; fi
            both "stop-$state-$fm-$rep" stop || die "drivers did not stop"
        done
        # localhost: both ends on VM a over 127.0.0.1, through one driver
        [[ -n $app2_a ]] || { [[ -n ${local_modes:-} ]] && log "WARNING: no fifth core on VM a, localhost runs skipped"; continue; }
        for mode in $(printf '%s\n' ${local_modes:-} | python3 -c 'import random,sys; l=sys.stdin.read().split(); random.shuffle(l); print(" ".join(l))'); do
            label=$state:local-$mode
            fm=$(file_mode "$mode")
            log "$state rep $rep: localhost $mode"
            on "$A" "$vm driver $mode" >"$out/driver-$state-local-$fm-$rep.log" 2>&1 || die "$mode driver did not start"
            if [[ ${mode%%@*} == dpdk* ]]; then on "$A" "$vm wc-check" >"$out/a-wc-$state-local-$fm-$rep.log" 2>&1 || true; fi
            on "$A" "$vm jitter" >"$out/a-jitter-before-rtt-$state-local-$fm-$rep.log" 2>&1 || true
            align_rtt
            on "$A" "$vm client $label rtt xpong 127.0.0.1:20123 127.0.0.1:20124 $app2_a" \
                >"$out/pong-$state-local-$fm-$rep.log" 2>&1 & pong=$!
            sleep 2
            # a localhost round trip is ~6 us: log its spikes from 25 us, not 100
            on "$A" "RTT_SECONDS=20 LOAD_SPIKE_NS=25000 $vm client $label rtt xping 127.0.0.1:20123 127.0.0.1:20124 $rtt_messages $rtt_warmup $app_a" \
                2>"$out/ping-$state-local-$fm-$rep.log" | sed "s/^/$rep,/" | tee -a "$results" >&2
            on "$A" "sudo pkill -f '[r]tt xpong'" 2>/dev/null || true
            wait $pong 2>/dev/null || true
            on "$A" 'cat bench/rtt-histogram.csv' >"$out/histogram-$state-local-$fm-$rep.csv" 2>/dev/null || true
            on "$A" 'cat bench/load-spikes.csv' >"$out/spikes-$state-local-$fm-rtt-$rep.csv" 2>/dev/null || true
            on "$A" 'cat bench/stalls-ping.csv' >"$out/stalls-ping-$state-local-$fm-$rep.csv" 2>/dev/null || true
            on "$A" 'cat bench/stalls-pong.csv' >"$out/stalls-pong-$state-local-$fm-$rep.csv" 2>/dev/null || true
            on "$A" "$vm jitter" >"$out/a-jitter-after-rtt-$state-local-$fm-$rep.log" 2>&1 || true
            on "$A" "$vm client $label tput xsub 127.0.0.1:20125 $tput_seconds $app2_a" \
                2>"$out/sub-$state-local-$fm-$rep.log" | sed "s/^/$rep,/" >>"$results" & sub=$!
            sleep 2
            on "$A" "$vm client $label tput xpub 127.0.0.1:20125 $((tput_seconds + 6)) $app_a" \
                >"$out/pub-$state-local-$fm-$rep.log" 2>&1
            wait $sub || log "WARNING: localhost $mode rep $rep throughput subscriber failed"
            [[ -n ${load_sizes:-} ]] &&
                load_sweep "$state" "$rep" "local-$fm" "$label" "$A" 127.0.0.1 127.0.0.1 "$app_a" "$app2_a"
            [[ -n ${mix:-} ]] &&
                mix_run "$state" "$rep" "local-$fm" "$label" "$A" 127.0.0.1 127.0.0.1 "$app_a" "$app2_a"
            if [[ ${mode%%@*} == dpdk* ]]; then on "$A" "$vm pmd-log" >"$out/a-pmd-$state-local-$fm-$rep.log" 2>&1 || true; fi
            on "$A" "$vm stop" >/dev/null 2>&1 || die "driver did not stop"
        done
    done
}
