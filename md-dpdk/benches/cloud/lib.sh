# Shared by md-dpdk/benches/cloud/azure/lab.sh and md-dpdk/benches/cloud/aws/lab.sh, sourced
# after the cloud's VMs exist. The caller sets: A and B (the VMs' public
# addresses), user (their admin user), out (the results directory), known (its
# known_hosts), data_subnet, reps, modes and states (arrays), rtt_messages,
# rtt_warmup, tput_seconds, a_private and b_private (the VMs' management
# addresses, which VM a drives the benchmark over); optionally placement (AWS:
# precision-time turns on the PTP hardware clock and NIC RX timestamps) and
# ssh_key. The functions run the benchmark on both VMs in lockstep; teardown
# stays with the caller's trap.

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
both() {
    local what=$1; shift
    on "$A" "$vm $*" >"$out/a-$what.log" 2>&1 & local p1=$!
    on "$B" "$vm $*" >"$out/b-$what.log" 2>&1 & local p2=$!
    local rc=0
    wait $p1 || { rc=1; log "a: vm.sh $* failed ($out/a-$what.log)"; tail -20 "$out/a-$what.log" >&2; }
    wait $p2 || { rc=1; log "b: vm.sh $* failed ($out/b-$what.log)"; tail -20 "$out/b-$what.log" >&2; }
    return $rc
}

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

# copy the checkout (tracked and new files; not the upstream submodules), then
# bootstrap, reboot into the backports kernel and build
prepare_vms() {
    wait_ssh
    log "copying the checkout"
    (cd "$repo" && git ls-files -co --exclude-standard | grep -v -e '^aeron$' -e '^simple-binary-encoding$' |
        COPYFILE_DISABLE=1 tar --no-xattrs -cf - -T -) >"$out/ergon.tar" || die "cannot pack the checkout"
    local host
    for host in "$A" "$B"; do
        local try copied=
        for try in 1 2 3 4 5; do
            scp -q "${ssh_opts[@]}" "$out/ergon.tar" "$user@$host:ergon.tar" && { copied=1; break; }
            sleep 15
        done
        [[ -n $copied ]] || die "copy to $host failed"
        on "$host" "rm -rf ergon bench && mkdir -p ergon bench && echo $data_subnet >bench/data_subnet && echo ${placement:-} >bench/placement && tar -xf ergon.tar -C ergon && rm ergon.tar" ||
            die "unpack on $host failed"
    done
    git -C "$repo" rev-parse HEAD >"$out/commit"
    git -C "$repo" status --porcelain >"$out/dirty"
    log "bootstrap"
    detached bootstrap bootstrap || die "bootstrap failed"
    reboot_both
    log "build"
    detached build build || die "build failed"
    # precision-time: boot into the PHC-enabled ENA driver the build installed
    [[ ${placement:-} == precision-time ]] && reboot_both
    for host in "$A" "$B"; do
        on "$host" "echo \"kernel \$(uname -r)\"; echo \"dpdk \$(pkg-config --modversion libdpdk)\";
            echo \"cpu \$(lscpu | sed -n 's/^Model name: *//p')\"; echo \"cpus \$(nproc)\";
            echo \"rustc \$(~/.cargo/bin/rustc -V)\"; $vm layout; $vm nics; $vm ptp" >"$out/env-$host.txt" 2>&1
    done
    both tune-boot tune || die "tune failed"
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
tput_seconds=$tput_seconds
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
# loss counters' growth over a run (vm.sh loss before and after)
loss_row() {
    python3 - "$@" <<'PY' >>"$results"
import sys
rep, label, host, before, after = sys.argv[1:6]
parse = lambda s: dict(kv.split("=", 1) for kv in s.split(";") if "=" in kv)
b, a = parse(before), parse(after)
if not a or not b:  # a failed read: never an absolute count passed off as growth
    print(f"{rep},loss,{label},{host},invalid=1")
else:
    print(f"{rep},loss,{label},{host}," + ";".join(f"{k}={int(a[k]) - int(b[k])}" for k in a if k in b))
PY
}

# load_sweep <state> <rep> <mode> <label> <far host> <far ip> <near ip> <near cpu> <far cpu>:
# with the drivers up in <mode>, for each size in $load_sizes: the one-way
# sustained maximum (tput, VM a publishing to the far host), then the round
# trip open loop (VM a pings, the far host pongs) at each fraction of it in
# $load_fractions and one burst run, each with its loss counters on both
# hosts. For localhost the far host is VM a and both addresses 127.0.0.1.
load_sweep() {
    local state=$1 rep=$2 mode=$3 label=$4 far=$5 far_ip=$6 near_ip=$7 near_cpu=$8 far_cpu=$9
    local size max frac rate before_a before_b pong sub tag port=22200
    for size in ${load_sizes:-}; do
        tag=$state-$mode-$size-$rep
        on "$far" "$vm client $label tput xsub $far_ip:20125 $tput_seconds $far_cpu" \
            2>"$out/maxsub-$tag.log" >"$out/max-$tag.csv" & sub=$!
        sleep 2
        on "$A" "$vm client $label tput xpub $far_ip:20125 $((tput_seconds + 6)) $near_cpu $size" \
            >"$out/maxpub-$tag.log" 2>&1
        wait $sub || true
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

# mix_run <state> <rep> <mode> <label> <far host> <far ip> <near ip> <near cpu> <far cpu>:
# for each profile in $mix ("<profile>=<name:stream:msgs/s:size[:burst]> ...;..."),
# every stream of the profile at once, open loop on its own schedule, on one
# pair of channels (VM a pings, the far host echoes each stream on stream + 1000),
# with loss counters on both hosts and the spikes (LOAD_SPIKE_NS) kept per run.
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
    local state=$1 rep mode label pong sub
    results=$out/results.csv
    a_rcv=$(on "$A" "$vm nics" 2>>"$out/nics.log" | awk '$1 == "receiver" {print $5}')
    b_rcv=$(on "$B" "$vm nics" 2>>"$out/nics.log" | awk '$1 == "receiver" {print $5}')
    [[ -n $a_rcv && -n $b_rcv ]] || { cat "$out/nics.log" >&2; die "cannot read the data NICs"; }
    app_a=$(on "$A" "$vm layout" | awk '{print $2}')
    app2_a=$(on "$A" "$vm layout" | awk '{print $5}')
    app_b=$(on "$B" "$vm layout" | awk '{print $2}')
    log "receiver ports: a=$a_rcv b=$b_rcv; app CPUs: a=$app_a b=$app_b"
    for rep in $(seq "$reps"); do
        for mode in $(printf '%s\n' "${modes[@]}" | python3 -c 'import random,sys; l=sys.stdin.read().split(); random.shuffle(l); print(" ".join(l))'); do
            label=$state:$mode
            log "$state rep $rep: $mode"
            # <mode>+idle: the same, with an idle localhost subscription held on
            # each driver throughout (what an unused host-local channel costs)
            both "driver-$state-$mode-$rep" driver "${mode%+idle}" || die "$mode drivers did not start"
            idle_pids=()
            if [[ $mode == *+idle ]]; then
                # held until killed below; one log per VM, both must be up first
                for n in a b; do
                    [[ $n == a ]] && host=$A || host=$B
                    on "$host" "$vm client $label rtt idle 127.0.0.1:22100 3600" \
                        >"$out/idle-$state-$mode-$rep-$n.log" 2>&1 & idle_pids+=($!)
                done
                for _ in $(seq 40); do
                    [[ $(cat "$out/idle-$state-$mode-$rep-"{a,b}.log 2>/dev/null | grep -c 'idle subscription') -ge 2 ]] && break
                    sleep 0.5
                done
                [[ $(cat "$out/idle-$state-$mode-$rep-"{a,b}.log 2>/dev/null | grep -c 'idle subscription') -ge 2 ]] ||
                    die "$mode rep $rep: the idle subscriptions did not start"
            fi
            both "counters-before-$state-$mode-$rep" counters || true
            both "jitter-before-rtt-$state-$mode-$rep" jitter || true
            loss_a=$(on "$A" "$vm loss" 2>/dev/null); loss_b=$(on "$B" "$vm loss" 2>/dev/null)
            # round trip: B pongs, A pings (ping's endpoint on B, pong's on A)
            on "$B" "$vm client $label rtt xpong $b_rcv:20123 $a_rcv:20124 $app_b" \
                >"$out/pong-$state-$mode-$rep.log" 2>&1 & pong=$!
            sleep 2
            on "$A" "RTT_SECONDS=20 $vm client $label rtt xping $b_rcv:20123 $a_rcv:20124 $rtt_messages $rtt_warmup $app_a" \
                2>"$out/ping-$state-$mode-$rep.log" | sed "s/^/$rep,/" | tee -a "$results" >&2
            on "$B" "sudo pkill -f '[r]tt xpong'" 2>/dev/null || true
            wait $pong 2>/dev/null || true
            on "$A" 'cat bench/rtt-histogram.csv' >"$out/histogram-$state-$mode-$rep.csv" 2>/dev/null || true
            on "$A" 'cat bench/load-spikes.csv' >"$out/spikes-$state-$mode-rtt-$rep.csv" 2>/dev/null || true
            on "$A" 'cat bench/stalls-ping.csv' >"$out/stalls-ping-$state-$mode-$rep.csv" 2>/dev/null || true
            on "$B" 'cat bench/stalls-pong.csv' >"$out/stalls-pong-$state-$mode-$rep.csv" 2>/dev/null || true
            both "jitter-after-rtt-$state-$mode-$rep" jitter || true
            # NIC arrival -> dispatch on each receiver port (DPDK with NIC RX timestamps only)
            for host in "$A" "$B"; do
                on "$host" "$vm rx-hw-delay" 2>/dev/null | sed "s/^/$rep,rxhw,$label,/" >>"$results" || true
            done
            # throughput: A publishes, B subscribes
            on "$B" "$vm client $label tput xsub $b_rcv:20125 $tput_seconds $app_b" \
                2>"$out/sub-$state-$mode-$rep.log" | sed "s/^/$rep,/" >>"$results" & sub=$!
            sleep 2
            on "$A" "$vm client $label tput xpub $b_rcv:20125 $((tput_seconds + 6)) $app_a" \
                >"$out/pub-$state-$mode-$rep.log" 2>&1
            wait $sub || log "WARNING: $mode rep $rep throughput subscriber failed"
            tail -1 "$results" >&2
            both "counters-after-$state-$mode-$rep" counters || true
            loss_row "$rep" "$label" a "$loss_a" "$(on "$A" "$vm loss" 2>/dev/null)"
            loss_row "$rep" "$label" b "$loss_b" "$(on "$B" "$vm loss" 2>/dev/null)"
            # latency under load (LAB_LOAD_SIZES), the same drivers
            [[ -n ${load_sizes:-} && $mode != *+idle ]] &&
                load_sweep "$state" "$rep" "$mode" "$label" "$B" "$b_rcv" "$a_rcv" "$app_a" "$app_b"
            [[ -n ${mix:-} && $mode != *+idle ]] &&
                mix_run "$state" "$rep" "$mode" "$label" "$B" "$b_rcv" "$a_rcv" "$app_a" "$app_b"
            if ((${#idle_pids[@]})); then
                kill "${idle_pids[@]}" 2>/dev/null
                for host in "$A" "$B"; do on "$host" "sudo pkill -f '[r]tt idle'" 2>/dev/null || true; done
                wait "${idle_pids[@]}" 2>/dev/null || true
            fi
            both "stop-$state-$mode-$rep" stop || die "drivers did not stop"
        done
        # localhost: both ends on VM a over 127.0.0.1, through one driver
        [[ -n $app2_a ]] || { [[ -n ${local_modes:-} ]] && log "WARNING: no fifth core on VM a, localhost runs skipped"; continue; }
        for mode in $(printf '%s\n' ${local_modes:-} | python3 -c 'import random,sys; l=sys.stdin.read().split(); random.shuffle(l); print(" ".join(l))'); do
            label=$state:local-$mode
            log "$state rep $rep: localhost $mode"
            on "$A" "$vm driver $mode" >"$out/driver-$state-local-$mode-$rep.log" 2>&1 || die "$mode driver did not start"
            on "$A" "$vm jitter" >"$out/a-jitter-before-rtt-$state-local-$mode-$rep.log" 2>&1 || true
            on "$A" "$vm client $label rtt xpong 127.0.0.1:20123 127.0.0.1:20124 $app2_a" \
                >"$out/pong-$state-local-$mode-$rep.log" 2>&1 & pong=$!
            sleep 2
            # a localhost round trip is ~6 us: log its spikes from 25 us, not 100
            on "$A" "RTT_SECONDS=20 LOAD_SPIKE_NS=25000 $vm client $label rtt xping 127.0.0.1:20123 127.0.0.1:20124 $rtt_messages $rtt_warmup $app_a" \
                2>"$out/ping-$state-local-$mode-$rep.log" | sed "s/^/$rep,/" | tee -a "$results" >&2
            on "$A" "sudo pkill -f '[r]tt xpong'" 2>/dev/null || true
            wait $pong 2>/dev/null || true
            on "$A" 'cat bench/rtt-histogram.csv' >"$out/histogram-$state-local-$mode-$rep.csv" 2>/dev/null || true
            on "$A" 'cat bench/load-spikes.csv' >"$out/spikes-$state-local-$mode-rtt-$rep.csv" 2>/dev/null || true
            on "$A" 'cat bench/stalls-ping.csv' >"$out/stalls-ping-$state-local-$mode-$rep.csv" 2>/dev/null || true
            on "$A" 'cat bench/stalls-pong.csv' >"$out/stalls-pong-$state-local-$mode-$rep.csv" 2>/dev/null || true
            on "$A" "$vm jitter" >"$out/a-jitter-after-rtt-$state-local-$mode-$rep.log" 2>&1 || true
            on "$A" "$vm client $label tput xsub 127.0.0.1:20125 $tput_seconds $app2_a" \
                2>"$out/sub-$state-local-$mode-$rep.log" | sed "s/^/$rep,/" >>"$results" & sub=$!
            sleep 2
            on "$A" "$vm client $label tput xpub 127.0.0.1:20125 $((tput_seconds + 6)) $app_a" \
                >"$out/pub-$state-local-$mode-$rep.log" 2>&1
            wait $sub || log "WARNING: localhost $mode rep $rep throughput subscriber failed"
            [[ -n ${load_sizes:-} ]] &&
                load_sweep "$state" "$rep" "local-$mode" "$label" "$A" 127.0.0.1 127.0.0.1 "$app_a" "$app2_a"
            [[ -n ${mix:-} ]] &&
                mix_run "$state" "$rep" "local-$mode" "$label" "$A" 127.0.0.1 127.0.0.1 "$app_a" "$app2_a"
            on "$A" "$vm stop" >/dev/null 2>&1 || die "driver did not stop"
        done
    done
}
