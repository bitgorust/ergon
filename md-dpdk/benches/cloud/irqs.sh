# Sourced by vm.sh: every IRQ the kernel lets move kept on the housekeeping
# CPU, and the data NICs' interrupts counted wherever else they land. Reads
# and writes $LAB_PROC_ROOT (/proc; test/test-lab.sh gives it a fake tree).
#
# `tune` pinning them once a boot is not enough: the kernel's ENA driver sets
# each queue IRQ's affinity again (its hint, one queue a CPU) whenever it
# brings an interface up (or resets it: a ring change, its own recovery), so
# once the data NICs came back from a DPDK mode, or a ring change reset them,
# the kernel modes took their interrupts on the isolated CPUs (AWS round 5's
# trace, which covered the agents', the clients' and its spinner's CPUs but
# not housekeeping's: 1.1 M on each VM). vm.sh pins again after each of those
# and just before each driver starts; a reset mid-run is not undone until
# then, and `loss` counts what landed elsewhere (isolated-nic-irqs).
LAB_PROC_ROOT=${LAB_PROC_ROOT:-/proc}

# irqs_pin <housekeeping CPU> <when>: irqbalance stopped, every IRQ's
# affinity and new IRQs' default the housekeeping CPU (the first core's, so
# its mask is one word), then irqs_check's line, "irqs <when>: ...", on
# stderr. An IRQ that refuses (per-CPU, or managed) is the kernel's to keep:
# irqs_check lists it.
irqs_pin() {
    local hk=$1 when=$2
    sudo systemctl stop irqbalance 2>/dev/null || true
    # one sudo for them all
    sudo bash -c 'for f in "$2"/irq/[0-9]*/smp_affinity_list; do echo "$1" >"$f"; done
        printf "%x\n" $((1 << $1)) >"$2/irq/default_smp_affinity"' _ "$hk" "$LAB_PROC_ROOT" 2>/dev/null || true
    echo "irqs $when: $(irqs_check "$hk")" >&2
}

# irqs_check <housekeeping CPU>: how many of the IRQs with a handler (those
# in /proc/interrupts) have the housekeeping CPU alone, how many not, each of
# those as <irq>:<name>@<CPUs>, and the default mask for new IRQs. An IRQ's
# CPUs are the kernel's effective ones (effective_affinity_list), or its
# affinity where a kernel keeps no effective list or an IRQ not started has
# it empty. Managed IRQs (NVMe's queues, say) stay where the kernel spread
# them: listed, not fatal.
irqs_check() {
    awk -v hk="$1" -v root="$LAB_PROC_ROOT" '
        function first(path, line) {
            line = ""
            if ((getline line <path) <= 0) line = ""
            close(path)
            return line
        }
        NR == 1 {n = NF; next}
        $1 ~ /^[0-9]+:$/ {
            irq = substr($1, 1, length($1) - 1)
            name = ""
            for (i = n + 4; i <= NF; i++) name = name $i
            if (name == "") name = $NF
            cpus = first(root "/irq/" irq "/effective_affinity_list")
            if (cpus == "") cpus = first(root "/irq/" irq "/smp_affinity_list")
            if (cpus == hk) on++
            else {off++; list = list (list == "" ? "" : " ") irq ":" name "@" (cpus == "" ? "?" : cpus)}
        }
        END {
            mask = first(root "/irq/default_smp_affinity")
            printf "%d on CPU %s alone, %d elsewhere%s, default for new ones %s\n", on, hk, off,
                (off ? " (" list ")" : ""), (mask == "" ? "?" : mask)
        }' "$LAB_PROC_ROOT/interrupts"
}

# irqs_nic_off <housekeeping CPU> [<NIC>...]: the interrupts the data NICs'
# IRQs have taken on every CPU but the housekeeping one, a running count
# (/proc/interrupts; `loss` reads it before and after each run). A NIC is
# its names and PCI address, comma-separated (<interface>,<VF>,<address>;
# under DPDK the address alone; empty ones skipped), and its IRQs those named
# after one of its names (ENA's queues, <interface>-Tx-Rx-<n>) or naming its
# address (its MSI-X chip, PCI-MSIX-<address>; ENA's management IRQ,
# ena-mgmnt@pci:<address>; MANA's, mana_q<n>@pci:<address>; under DPDK
# igb_uio's, through which the ENA PMD takes its admin queue's interrupts).
# An IRQ counts once, however many NICs it names (MANA's ports share one
# address). 0 with no NIC given; nothing, never 0, when a NIC given matches
# no IRQ: whichever driver holds a NIC keeps an IRQ on it, so that NIC was
# not found, and a count without it is not the data NICs'.
irqs_nic_off() {
    local hk=$1 nic id alt res='' ids
    shift
    for nic in "$@"; do
        alt=
        IFS=, read -ra ids <<<"$nic"
        for id in "${ids[@]}"; do
            if [[ -n $id ]]; then alt+=${alt:+|}${id//./[.]}; fi
        done
        if [[ -n $alt ]]; then res+=${res:+ }$alt; fi
    done
    [[ -n $res ]] || { echo 0; return; }
    # one pattern a NIC, each name or address whole: after a space, -, : or
    # @, before a space or -
    awk -v hk="CPU$hk" -v res="$res" '
        BEGIN {k = split(res, re, " "); for (j = 1; j <= k; j++) re[j] = "[ :@-](" re[j] ")[ -]"}
        NR == 1 {n = NF; for (i = 1; i <= NF; i++) if ($i != hk) col[i + 1] = 1; next}
        $1 ~ /^[0-9]+:$/ {
            tail = " "
            for (i = n + 2; i <= NF; i++) tail = tail $i " "
            hit = 0
            for (j = 1; j <= k; j++) if (tail ~ re[j]) {seen[j] = 1; hit = 1}
            if (hit) for (c in col) s += $c
        }
        END {
            for (j = 1; j <= k; j++) if (!(j in seen)) exit
            printf "%.0f\n", s
        }' "$LAB_PROC_ROOT/interrupts"
}
