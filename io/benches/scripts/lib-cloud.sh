# shellcheck shell=bash
# Host access and preparation shared by azure-flags.sh and aws-flags.sh
# (sourced once RCV, SND, SND_PRIV, RUSER, SSHO, OUT and log are set).

rssh() { ssh "${SSHO[@]}" "$RUSER@$RCV" "$@"; }
sssh() { ssh "${SSHO[@]}" "$RUSER@$SND" "$@"; }
# Copies host1:path1 to host2:path2 through this machine.
relay() {
  local tmp="$OUT/relay.$$"
  scp "${SSHO[@]}" "$RUSER@$1:$2" "$tmp" > /dev/null && scp "${SSHO[@]}" "$tmp" "$RUSER@$3:$4" > /dev/null && rm -f "$tmp"
}
# Detached job on the receiver polled with short ssh calls (a dropped session
# cannot kill it); prints its output and returns its exit code.
rrun() {
  local id rc=""
  id="job$(date +%s)$RANDOM"
  rssh "mkdir -p ~/jobs; nohup bash -c $(printf '%q' "($1); echo \$? > ~/jobs/$id.rc") > ~/jobs/$id.log 2>&1 < /dev/null &"
  until [[ -n $rc ]]; do
    sleep 3
    rc=$(rssh "cat ~/jobs/$id.rc 2>/dev/null" 2>/dev/null || true)
  done
  until rssh "cat ~/jobs/$id.log"; do sleep 5; done
  return "$rc"
}
wait_ssh() { for _ in $(seq 90); do ssh "${SSHO[@]}" "$RUSER@$1" true 2>/dev/null && return 0; sleep 5; done; return 1; }

# CPU layout, packages, build on the receiver, full isolation, reboot, tune.
prepare_hosts() {
  wait_ssh "$RCV"; wait_ssh "$SND"
  TOPO=$(rssh "lscpu -p=CPU,CORE | grep -v '^#'")
  cpu_of_core() { echo "$TOPO" | awk -F, -v c="$1" '$2==c {print $1; exit}'; }
  siblings_of_core() { echo "$TOPO" | awk -F, -v c="$1" '$2==c {print $1}' | paste -sd, -; }
  CPU_A=$(cpu_of_core 1)
  CPU_SIB=$(echo "$TOPO" | awk -F, -v a="$CPU_A" '$2==1 && $1!=a {print $1; exit}')
  CPU_SQ=$(cpu_of_core 3)
  # An idle isolated core for the spare-core noise probe (cores 1 and 3 are
  # the loop's and the SQPOLL thread's).
  CPU_SPARE=$(cpu_of_core 2)
  HK=$(siblings_of_core 0)
  NCORES=$(echo "$TOPO" | awk -F, '{print $2}' | sort -un | wc -l | tr -d ' ')
  ISO=$(for c in $(seq 1 $((NCORES - 1))); do siblings_of_core "$c"; done | paste -sd, -)
  STOPO=$(sssh "lscpu -p=CPU,CORE | grep -v '^#'")
  if echo "$STOPO" | awk -F, '$2==1 {f=1} END {exit !f}'; then
    # Two or more cores: the server loop gets core 1 to itself (its
    # hyperthread twin, if any, is isolated and idle).
    S_CPU=$(echo "$STOPO" | awk -F, '$2==1 {print $1; exit}')
    S_ISO=$(echo "$STOPO" | awk -F, '$2==1 {print $1}' | paste -sd, -)
    S_HK=$(echo "$STOPO" | awk -F, '$2!=1 {print $1}' | paste -sd, -)
  else
    # One core: the loop on its second hyperthread.
    S_CPU=$(echo "$STOPO" | awk -F, 'NR==2 {print $1}'); S_HK=$(echo "$STOPO" | awk -F, 'NR==1 {print $1}')
    [[ -n $S_CPU ]] || S_CPU=$S_HK
    S_ISO=$S_CPU
  fi
  log "receiver: housekeeping $HK, isolated $ISO, loop CPU $CPU_A (sibling ${CPU_SIB:-none}), SQPOLL $CPU_SQ, spare ${CPU_SPARE:-none}; sender CPU $S_CPU"

  for h in "$RCV" "$SND"; do scp "${SSHO[@]}" "${SCRIPTS:-scripts}/vm-setup.sh" "$RUSER@$h:" > /dev/null; done
  rssh "sudo bash vm-setup.sh install $HK $ISO" | tail -1
  sssh "sudo bash vm-setup.sh install $S_HK $S_ISO" | tail -1

  log "building on the receiver"
  scp "${SSHO[@]}" "$OUT/src.tgz" "$RUSER@$RCV:" > /dev/null
  rrun "set -e; exec > build.log 2>&1
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq build-essential pkg-config >/dev/null
    command -v cargo >/dev/null || (curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal >/dev/null); . ~/.cargo/env
    mkdir -p ergon bin; tar -xzf src.tgz -C ergon; cd ergon/io/benches
    cargo build --release --bins -q
    cargo test --release -q
    cp target/release/{io_bench,http_bench,replay_server,noise_probe,analyse,mio_bench} ~/bin/" \
    || { rssh "tail -40 build.log"; exit 1; }
  rssh "tail -3 build.log"
  for b in replay_server noise_probe; do
    scp "${SSHO[@]}" "$RUSER@$RCV:bin/$b" "$OUT/$b" > /dev/null
  done
  sssh "mkdir -p bin"
  scp "${SSHO[@]}" "$OUT/replay_server" "$OUT/noise_probe" "$RUSER@$SND:bin/" > /dev/null
  rm -f "$OUT/replay_server" "$OUT/noise_probe"

  log "rebooting for CPU isolation"
  rssh "sudo reboot" || true; sssh "sudo reboot" || true
  sleep 45; wait_ssh "$RCV"; wait_ssh "$SND"
  rssh "sudo bash vm-setup.sh tune" > "$OUT/env-rcv.txt"
  sssh "sudo bash vm-setup.sh tune" > "$OUT/env-snd.txt"
  # The echo and HTTP servers spin on non-blocking reads without polling the
  # NIC, so each request waits for the interrupt on a housekeeping CPU: woken
  # from C6 (190 µs exit on c7i) that added 150–250 µs to 0–16% of round trips
  # in AWS stage 1, varying run to run. The sender is the rig, not what is
  # measured: no idle state deeper than C1 there, ever.
  sssh "sudo bash vm-setup.sh cond cstate on" >> "$OUT/env-snd.txt" 2>&1 || log "sender: capping idle states failed"
  log "sender: idle states deeper than C1 disabled ($(grep -c WARN "$OUT/env-snd.txt" || true) warnings)"
  log "receiver isolated CPUs: $(rssh cat /sys/devices/system/cpu/isolated)"
  cpu_map
}

# Gap map: a counter-only spinner (noise_probe) on every isolated CPU of both
# hosts at once for CPUMAP_S seconds (at least two minute marks), so the CPUs
# the once-a-minute stall reaches are known before anything is measured.
# Writes res/cpumap-receiver and (merged later) srvres/cpumap-sender, logs
# each CPU's longest gap and its gaps of 100 µs or more by second of the
# minute. PICK_CORE=quiet (the default) moves the loop to the isolated core
# whose worst gap is smallest, the SQPOLL thread to the next and the spare
# probe to the third; PICK_CORE=noisy (the default with MODE=diagnose) does
# the reverse, so the diagnosis watches a CPU the stall reaches; fixed keeps
# cores 1, 3 and 2.
cpu_map() {
  local secs=${CPUMAP_S:-130} summary
  summary='for d in DIR/cpu*; do awk -v cpu="${d##*/cpu}" '"'"'
      /^#/ { for (i = 2; i <= NF; i++) if ($i ~ /^max_ns=/) max = substr($i, 8) + 0; next }
      { if ($1 > max) max = $1; if ($1 >= 100000) { n++; s[int(($3 / 1e9) % 60)]++ } }
      END { at = ""; for (k in s) at = at sprintf(" :%02d x%d", k, s[k]); printf "%s %d %d%s\n", cpu, max / 1000, n, at }'"'"' "$d/stalls-0.txt"; done'
  log "gap map: a spinner on every isolated CPU of both hosts for $secs s"
  rssh "mkdir -p res; for c in ${ISO//,/ }; do sudo bin/noise_probe --cpu \$c --duration $secs --stall-us 20 --out res/cpumap-receiver/cpu\$c > /dev/null 2>&1 & done; wait" &
  local r=$!
  sssh "mkdir -p srvres; for c in ${S_ISO//,/ }; do sudo bin/noise_probe --cpu \$c --duration $secs --stall-us 20 --out srvres/cpumap-sender/cpu\$c > /dev/null 2>&1 & done; wait" &
  local s=$!
  wait "$r" "$s" || log "gap map: a spinner failed"
  rssh "sudo chown -R \$(id -un) res; ${summary//DIR/res/cpumap-receiver}" > "$OUT/cpumap-receiver.txt" || true
  sssh "sudo chown -R \$(id -un) srvres; ${summary//DIR/srvres/cpumap-sender}" > "$OUT/cpumap-sender.txt" || true
  local f
  for f in receiver sender; do
    while read -r cpu worst n at; do
      log "gap map $f CPU $cpu: longest ${worst} µs, $n gaps of 100 µs or more${at:+ at$at}"
    done < "$OUT/cpumap-$f.txt"
  done
  local pick=${PICK_CORE:-quiet}
  [[ -n ${PICK_CORE:-} || ${MODE:-} != diagnose ]] || pick=noisy
  [[ $pick == quiet || $pick == noisy ]] || return 0
  # Cores 1.. ranked by the worst gap of their CPUs.
  local ranked
  ranked=$(echo "$TOPO" | awk -F, -v map="$OUT/cpumap-receiver.txt" '
    BEGIN { while ((getline l < map) > 0) { split(l, f, " "); w[f[1]] = f[2] } }
    $2 != 0 && ($1 in w) { if (!($2 in c) || w[$1] > c[$2]) c[$2] = w[$1] }
    END { for (k in c) print c[k], k }' | if [[ $pick == noisy ]]; then sort -rn; else sort -n; fi | awk '{print $2}')
  local cores=($ranked)
  if (( ${#cores[@]} < 3 )); then
    log "gap map: fewer than three isolated cores mapped; CPU roles unchanged"
    return 0
  fi
  CPU_A=$(cpu_of_core "${cores[0]}")
  CPU_SIB=$(echo "$TOPO" | awk -F, -v a="$CPU_A" -v c="${cores[0]}" '$2==c && $1!=a {print $1; exit}')
  CPU_SQ=$(cpu_of_core "${cores[1]}")
  CPU_SPARE=$(cpu_of_core "${cores[2]}")
  log "gap map ($pick core first): loop CPU $CPU_A (core ${cores[0]}, sibling ${CPU_SIB:-none}), SQPOLL $CPU_SQ, spare $CPU_SPARE"
}
