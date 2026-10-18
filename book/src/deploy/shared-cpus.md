# Shared CPUs for Helper Threads (`k8s/nri-shared-cpus`)

How to install and use `k8s/nri-shared-cpus` on a Kubernetes node.

A busy-spinning pod gets whole cores of its own from the kubelet. Its
helpers (dial, TLS, warnings, Aeron conductors) do not need a whole core,
and running them on the loop's core costs latency. This is a containerd
NRI plugin and a kubelet device plugin that give the pod the node's shared
housekeeping CPUs as well, and start every thread there.

The busy-spin loop keeps its exclusive core. Only sleepy helpers go to the
shared CPUs. Never a spinner or SQPOLL there.

Source: `k8s/nri-shared-cpus/`. The install steps below are the ones the
`k8s/nri-shared-cpus/test/e2e.sh` kind run executes.

Status: experimental and unpublished. Tested on kind. Not yet on EKS: two
attempts at an EKS spot lab (eu-north-1 `c8g.xlarge`, us-east-1 `c7g.xlarge`)
found no spot capacity and tore down cleanly, so these steps come from the
manifests and the kind e2e, not from a live EKS node. **AKS cannot run it**
(see [Limits](#limits-and-what-is-not-verified)).

## What it gives

| item | value |
|---|---|
| exclusive CPUs | the kubelet's static CPU manager, `cpu: N` |
| shared CPUs | `ergon.io/shared-millicpus: M` adds `ceil(M/1000)` shared CPUs |
| start affinity | PID 1 and every new thread start on the shared CPUs |
| timer slack | 1 ns on every thread, inherited by every thread and child the program makes |
| pinning | the app pins hot threads by index into `ERGON_ISOLATED_CPUS` |

`M` is millicpus because extended resources are whole numbers and requests
must equal limits. `250` is a quarter core of **scheduler accounting only**:
the plugin still adds one whole shared CPU, and there is no hard cap.

### Timer slack

The kernel's default timer slack is 50,000 ns. It is added to every sleep and
to every `epoll`/`io_uring` timeout, so a 100 µs wait can take 150 µs. The
hook sets 1 ns, the kernel's floor, on every thread the container has when it
starts. The kernel copies the value to every thread and child the program
makes afterwards, so the whole tree inherits it. A container that cannot have
it does not start.

The hook writes `/proc/<tid>/timerslack_ns` for each thread and reads the
result back. Access to that file is free only for a task reading or writing
itself; any other task needs `CAP_SYS_NICE` over it and is refused with
`EPERM`. There is no thread-group exception, so a thread reading
`/proc/self/timerslack_ns` is reading its leader's file and hits the rule. The
OCI hook has the capability (containerd runs hooks as root with full
capabilities), and the plugin DaemonSet's `capabilities: drop: ["ALL"]`
applies to the plugin process only. A program can always set its own slack
with `prctl(PR_SET_TIMERSLACK)`, which needs no privilege.

What the 1 ns slack is worth in latency is **not yet measured**. This
environment cannot measure it: sleeps below 1 ms are quantised to about 1 ms
here at both 50,000 ns and 1 ns slack. The mechanism is tested; the number is
not, and is not claimed.

## Node setup

Do this before installing the plugin. Everything here is per node.

| setting | value |
|---|---|
| Kubernetes | >= 1.33 (1.36 recommended): no CPU quota on exclusive containers |
| kubelet | `cpuManagerPolicy: static`; x86 `full-pcpus-only`; `reservedSystemCPUs` = the IRQ CPU(s) + the shared CPUs |
| kernel | >= 6.2, cgroup v2. Boot line: `nohz_full`, `rcu_nocbs`, `isolcpus=managed_irq` on the exclusive CPUs, never on the shared ones |
| containerd | >= 2.2, NRI on, default validator on |
| node label | `ergon.io/shared-cpus=true` |

Check the node first. Each command must print what the comment says:

```sh
# kernel >= 6.2, cgroup v2
uname -r
stat -fc %T /sys/fs/cgroup                       # cgroup2fs

# containerd >= 2.2 with NRI
containerd --version
nerdctl info 2>/dev/null | grep -i nri           # or: grep -r nri /etc/containerd/

# the boot line has the isolation on the exclusive CPUs only
cat /proc/cmdline
```

**CPU split.** Pick three CPU sets on the node and write them down; every
later step names one of them.

| set | who | example (8 CPUs) |
|---|---|---|
| exclusive | the pod's busy-spin loops | `2,3` (and `4,5` for a second pod) |
| shared | helpers, kubelet, containerd, system | `0` |
| IRQ | NIC interrupts | `1` |

**Kernel boot line.** Isolate the exclusive CPUs only. Never the shared one.

```
nohz_full=2-5 rcu_nocbs=2-5 isolcpus=managed_irq,domain,2-5
```

**kubelet** — `/var/lib/kubelet/config.yaml` (or the drop-in your
distribution uses):

```yaml
cpuManagerPolicy: static
cpuManagerPolicyOptions:
  full-pcpus-only: "true"          # x86 only
reservedSystemCPUs: "0,1"          # shared 0 + IRQ 1
# cpuManagerReconcilePeriod, topologyManagerPolicy as your node needs
```

Restart kubelet after changing it, and check it took:

```sh
systemctl restart kubelet
# must print static and your reserved list
journalctl -u kubelet --no-pager | grep -i 'cpu manager\|reserved' | tail
```

**containerd** — the NRI plugin and the default validator must be on. The
validator is what makes an unmet `required-plugins` annotation fail the
create instead of starting the pod half-granted.

```toml
version = 3

[plugins."io.containerd.nri.v1.nri"]
  disable = false
  disable_connections = false
  socket_path = "/var/run/nri/nri.sock"

[plugins."io.containerd.nri.v1.nri".default_validator]
  enable = true
  # leave required_plugins empty: a node-wide list would also block
  # kube-system pods and the plugin's own pod while it is down
```

```sh
systemctl restart containerd
ls -l /var/run/nri/nri.sock
```

## Install on Kubernetes

Six steps, from the `k8s/nri-shared-cpus` directory. Nothing is pushed to a
registry: build the image and load it onto each node.

### 1. Label the nodes that take the plugin

```sh
kubectl label node <node> ergon.io/shared-cpus=true
```

Only labelled nodes run the DaemonSet. A pod that asks for the device on an
unlabelled node stays pending.

### 2. Edit `deploy/configmap.yaml`

This is the step people get wrong. The file is
`k8s/nri-shared-cpus/deploy/configmap.yaml`, and it becomes
`/etc/ergon-shared-cpus/config.yaml` in the plugin's container.

```yaml
sharedCpus: "0"          # the node's shared CPUs, kernel cpulist
oversubscription: 1
namespaces: ["trading"]  # who may opt in. Empty refuses every pod
defaultAffinity: shared
annotationOptIn: false

# These three are "off" by default. A latency node turns the first two on.
# Each one is a node-level permission: the pod still opts in by annotation,
# and a pod that asks for a grant this field does not allow is refused.
seccomp.ioUring: "allow"  # default "off". Without it, a pod asking for
                          # seccomp.ergon.io/<ctr>: "io-uring" is refused, and
                          # Aeron's conductor cannot use io_uring (RuntimeDefault
                          # blocks it: containerd issue 9320)
ptp: "auto"               # default "off". Without it, a pod asking for
                          # ptp.ergon.io/<ctr>: "auto" is refused, and the app
                          # has no NIC hardware clock. Use "auto" for the NIC's
                          # PHC, or a /dev/ptp* path the node has
schedFifo: "off"          # keep off: measured worse (see Latency Pods)
```

Rules:

- `sharedCpus` must sit inside the kubelet's `reservedSystemCPUs`, minus the
  IRQ CPU(s). In the table above that is `0`.
- `namespaces` is an allowlist. Create those namespaces before applying.
- Quote `off`, `on`, `yes`, `no`. YAML 1.1 turns unquoted ones into booleans.
- An unknown field or an out-of-range value stops the plugin at start, and
  the message names the field.
- **`off` is not harmless.** It is a refusal. A pod whose annotation asks for
  a grant the ConfigMap does not allow does not start: the kubelet shows
  `CreateContainerError` and the text names the field to change.

### 3. Build and load the image

**Do not publish this to Docker Hub.** The plugin is root on the node and
installs an executable at `/opt/ergon/bin`. A public image is a root shell on
every node that pulls it, for whoever can push. Use one of these three.

**a. kind, or a single node** — build and load it into the node's own store:

```sh
docker build -t ergon-shared-cpus:dev .
kind load docker-image ergon-shared-cpus:dev        # kind
# anything else:
docker save ergon-shared-cpus:dev | ctr -n k8s.io images import -
```

**b. A private registry, for a real multi-node cluster** — the right default
outside a lab. Push to a registry only your nodes can read, and pin the
digest so a re-push cannot change what runs:

```sh
docker build -t <registry>/ergon-shared-cpus:<tag> .
docker push <registry>/ergon-shared-cpus:<tag>
docker inspect --format='{{index .RepoDigests 0}}' <registry>/ergon-shared-cpus:<tag>
```

Then in `deploy/daemonset.yaml` set the image to that digest
(`<registry>/ergon-shared-cpus@sha256:…`), `imagePullPolicy: IfNotPresent`,
and give the `ergon-system` namespace an `imagePullSecret`:

```sh
kubectl -n ergon-system create secret docker-registry regcred \
  --docker-server=<registry> --docker-username=<user> --docker-password=<token>
kubectl -n ergon-system patch serviceaccount default \
  -p '{"imagePullSecrets":[{"name":"regcred"}]}'
```

**c. No registry at all** — `imagePullPolicy: Never` plus a DaemonSet init
that expects the image to already be on the node (a config-management step, or
the AMI). Only worth it where nodes cannot reach any registry.

The DaemonSet ships `image: ergon-shared-cpus:dev` with
`imagePullPolicy: IfNotPresent`, which matches (a). Change the image field to
match whichever you chose.

### 4. Apply the three manifests

```sh
kubectl apply -f deploy/namespace.yaml \
              -f deploy/configmap.yaml \
              -f deploy/daemonset.yaml
```

The DaemonSet needs three host paths (`/var/run/nri`,
`/var/lib/kubelet/device-plugins`, `/opt/ergon/bin`) and the namespace is
labelled Pod Security `privileged` for that reason. The plugin is root on
the node: anything on the NRI socket can change any container.

### 5. Verify the plugin before any workload

```sh
# 1. the pod is Ready on every labelled node
kubectl -n ergon-system get pods -l app=ergon-shared-cpus -o wide

# 2. it registered the extended resource (a number > 0)
kubectl get node <node> -o \
  jsonpath='{.status.allocatable.ergon\.io/shared-millicpus}{"\n"}'

# 3. it is healthy
kubectl -n ergon-system exec ds/ergon-shared-cpus -- \
  curl -s localhost:9466/readyz
kubectl -n ergon-system exec ds/ergon-shared-cpus -- \
  curl -s localhost:9466/metrics | head

# 4. it logged no config error (strict YAML: a bad field stops it)
kubectl -n ergon-system logs -l app=ergon-shared-cpus --tail=50
```

If (2) prints nothing, the device plugin did not register: check the logs in
(4) and that `/var/lib/kubelet/device-plugins` is mounted.

### 6. Run a pod and check the grants landed

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: trader
  namespace: trading                       # must be in the ConfigMap's allowlist
  annotations:
    required-plugins.noderesource.dev: '["ergon-shared-cpus"]'
spec:
  nodeSelector: { ergon.io/shared-cpus: "true" }
  containers:
  - name: app
    image: my-app
    command: ["sleep", "3600"]
    resources:
      requests: { cpu: "2", memory: 1Gi, ergon.io/shared-millicpus: "250" }
      limits:   { cpu: "2", memory: 1Gi, ergon.io/shared-millicpus: "250" }
```

```sh
kubectl exec trader -- sh -c 'echo isolated=$ERGON_ISOLATED_CPUS shared=$ERGON_SHARED_CPUS'
kubectl exec trader -- cat /proc/1/status | grep Cpus_allowed_list
```

The second must print the shared CPUs only. That is the point of the plugin.

## Config

`/etc/ergon-shared-cpus/config.yaml`, from the ConfigMap. Strict YAML: an
unknown field is an error at start. No hot reload — edit the ConfigMap and
restart the DaemonSet.

| field | default | what it is |
|---|---|---|
| `sharedCpus` | **required** | the housekeeping CPUs, kernel cpulist (`"4"` or `"0,2"`). Inside `reservedSystemCPUs`, never the IRQ CPU(s) |
| `oversubscription` | `1` | 1–100. Capacity is `len(sharedCpus) x 1000 x` this; the max is 100,000 device IDs (the kubelet's 4 MiB gRPC limit) |
| `namespaces` | empty | the allowlist. Empty refuses every opt-in. Each must be a valid namespace name |
| `defaultAffinity` | `shared` | `shared` injects the hook that starts every thread on the shared CPUs. `all` injects nothing; use the `ergon-cpus` wrapper (see Bottlerocket below) |
| `hookPath` | `/opt/ergon/bin/ergon-cpus` | where the daemon writes the hook on the node. Must be a clean absolute path ending in `/ergon-cpus` — the daemon writes it as root, so the name is fixed |
| `annotationOptIn` | `false` | allow the pod annotation `cpu-shared.ergon.io/<container>` as a fallback when the device plugin is not used |
| `metricsAddress` | `:9466` | serves `/metrics`, `/healthz` and `/readyz` |
| `rlimits.memlock`, `.nofile`, `.rtPrio`, `.nice` | unset | the ceiling a pod may ask for by kind. `"inf"` or a positive count. The pod asks for kinds, never values |
| `seccomp.ioUring` | `off` | `allow` widens the container's seccomp profile for `io_uring_*`. See [Latency Pods](latency-pods.md) |
| `seccompProfilePath` | `/etc/ergon-shared-cpus/ergon-io-uring.json` | the shipped io_uring profile on the node, the fallback for a pod that cannot take the NRI grant |
| `devRoot` | `/dev` | where this process sees the host's `/dev`. The DaemonSet mounts it read-only at `/host/dev` so the plugin can stat the PHC and PM QoS nodes; granted paths stay `/dev/...` |
| `schedFifo` | `off` | `off`, or `thread` (the hot thread only, never the container). Quote the value: unquoted `off` is a YAML boolean |
| `ptp` | `off` | `off`, `auto`, or a `/dev/ptp*` path the node has. `auto` grants the NIC's PHC |
| `hugepagesAeronDir` | unset | absolute `AERON_DIR` to export when the pod mounts a sized hugetlbfs |

Quote `off`, `on`, `yes` and `no` in this file. YAML 1.1 turns unquoted ones
into booleans.

Unknown fields and out-of-range values are refused at start, each naming its
field: `oversubscription` outside 1–100, a `namespaces` entry that is not a
namespace name, `hookPath` not ending in `/ergon-cpus`, an `rlimits` value
that is neither `inf` nor a positive count, `schedFifo` / `ptp` /
`seccomp.ioUring` outside their sets.

## Pod

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: trader
  namespace: trading                       # must be in the plugin's allowlist
  annotations:
    required-plugins.noderesource.dev: '["ergon-shared-cpus"]'
spec:
  nodeSelector: { ergon.io/shared-cpus: "true" }
  containers:
  - name: app
    image: my-app
    resources:
      requests: { cpu: "2", memory: 1Gi, ergon.io/shared-millicpus: "250" }
      limits:   { cpu: "2", memory: 1Gi, ergon.io/shared-millicpus: "250" }
```

Inside the container:

| item | value |
|---|---|
| `cpuset.cpus.effective` | the exclusive CPUs plus the shared ones |
| affinity of PID 1 and of every new thread | the shared CPUs only |
| `nproc`, `GOMAXPROCS`, `availableProcessors()` | usually the shared CPUs: they count the affinity mask |

## Env vars

| name | value | format |
|---|---|---|
| `ERGON_ISOLATED_CPUS` | the exclusive CPUs | sorted comma list, no ranges: `1,5` |
| `ERGON_SHARED_CPUS` | this container's shared CPUs | same |
| `ERGON_ISOLATED_CORES` | the exclusive CPUs by physical core | groups by `;`, CPUs by `,`: `1,5;2,6` |
| `ERGON_SHARED_MILLICPUS` | `M` | decimal |
| `ERGON_HELPER_CPUS` | same as `ERGON_SHARED_CPUS` | comma list |
| `OPENSHIFT_ISOLATED_CPUS` | same as `ERGON_ISOLATED_CPUS` | kernel cpulist with ranges |
| `OPENSHIFT_SHARED_CPUS` | same as `ERGON_SHARED_CPUS` | same |

Read the CPUs from these variables or from `cpuset.cpus.effective`, never
from the process's own affinity: the start mask is the shared CPUs.

## Pinning by index

Pin each hot thread to isolated index `i`. The app never needs the real CPU
numbers.

```c
{{#include ../../../k8s/nri-shared-cpus/examples/pin.c}}
```

Rules:

- leave helpers alone: they start on the shared CPUs;
- create helpers from a thread that is still on the shared set (a new
  thread copies its creator's mask);
- each sleepy helper checks `sched_getcpu()` when it wakes and alarms if it
  is not on `ERGON_SHARED_CPUS`;
- never a spinner or SQPOLL on a shared CPU. ergon-io keeps this: with no
  `sqpoll_cpu`, `uring-sqpoll` takes a CPU of the pod's own outside
  `ERGON_HELPER_CPUS` and refuses (`Error::Cpu`) when there is none
  ([io: Kubernetes](../io/kubernetes.md#shared-cpus-from-the-node)).

## Worked example: an Aeron media driver

Two pinned cores (sender and receiver) plus the conductor on the shared
CPU, with huge pages. This is the layout a latency node wants.

**Node boot line** (from the table above), for exclusive CPUs 2 and 3,
shared 0, IRQ 1:

```
nohz_full=2-3 rcu_nocbs=2-3 isolcpus=managed_irq,domain,2-3 hugepagesz=2M hugepages=512
```

**ConfigMap** (`sharedCpus: "0"`), **kubelet**
(`reservedSystemCPUs: "0,1"`), and a namespace in the allowlist.

**Pod.** Three grants on top of the shared CPU: a sized hugetlbfs for
`AERON_DIR`, io_uring seccomp, and the NIC PHC. The isolation of CPUs 2-3
comes from the **boot line**, not from a grant: a media driver is one
process with three threads, and a cpuset partition takes whole processes,
so `cpu-partition` cannot take the sender and receiver while the
conductor stays on the shared CPU. See
[Latency Pods](latency-pods.md#2-isolated-partition).

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: aeron-driver
  namespace: trading
  labels: { ergon.io/latency: "hot" }
  annotations:
    required-plugins.noderesource.dev: '["ergon-shared-cpus"]'
    hugepages.ergon.io/app: "2M"          # sized hugetlbfs for AERON_DIR
    seccomp.ergon.io/app: "io-uring"      # the conductor uses io_uring
    ptp.ergon.io/app: "auto"              # the NIC's PHC
spec:
  nodeSelector: { ergon.io/shared-cpus: "true" }
  priorityClassName: system-node-critical
  containers:
  - name: app
    image: ergon-media-driver:dev
    command: ["ergon-cpus",
              "--env", "AERON_SENDER_CPU_AFFINITY=isolated:0",
              "--env", "AERON_RECEIVER_CPU_AFFINITY=isolated:1",
              "--", "aeronmd"]
    env:
    - { name: AERON_THREADING_MODE, value: DEDICATED }
    - { name: AERON_DIR, value: /dev/hugepages/aeron }   # set by the app, see below
    resources:
      requests: { cpu: "2", memory: 2Gi, ergon.io/shared-millicpus: "250", hugepages-2Mi: 256Mi }
      limits:   { cpu: "2", memory: 2Gi, ergon.io/shared-millicpus: "250", hugepages-2Mi: 256Mi }
```

**What each piece does:**

| piece | effect |
|---|---|
| `cpu: "2"` | two exclusive CPUs from the static CPU manager: the sender's and the receiver's |
| `ergon.io/shared-millicpus: "250"` | the plugin adds the shared CPU and starts every thread there |
| `AERON_SENDER_CPU_AFFINITY=isolated:0` | sender on exclusive index 0 |
| `AERON_RECEIVER_CPU_AFFINITY=isolated:1` | receiver on exclusive index 1 |
| `AERON_CONDUCTOR_CPU_AFFINITY` unset | the conductor stays on the shared CPU |
| `AERON_THREADING_MODE=DEDICATED` | sender, receiver and conductor on their own threads |
| `hugepages.ergon.io/app: "2M"` + `hugepages-2Mi` | a **sized** hugetlbfs mounted at `/dev/hugepages` |
| `AERON_DIR` on that hugetlbfs | the CnC file and the terms are 2 MiB pages, not 4 KiB |
| `seccomp.ergon.io/app: "io-uring"` | the profile gains the `io_uring_*` calls the conductor needs; never `Unconfined` |
| `ptp.ergon.io/app: "auto"` | the NIC's PHC, for clock work. x86 also gets `cpuqos` (`/dev/cpu_dma_latency`) to cap C-states |
| `ergon.io/latency: "hot"` (pod label) | the admission policy refuses exec probes and `restartPolicy: Always` sidecars |
| `isolcpus=managed_irq,domain,2-3` on the boot line | CPUs 2-3 leave the scheduler's load-balancing domains. This is what keeps the sender's and receiver's cores quiet; a cpuset partition cannot do it per-thread |

**The app sets `AERON_DIR` itself.** The plugin exports
`ERGON_HUGEPAGES_DIR` and `ERGON_HUGEPAGE_SIZE` and does not set `AERON_DIR`,
because the app must create the subdirectory:

```sh
mkdir -p "$ERGON_HUGEPAGES_DIR/aeron"
export AERON_DIR="$ERGON_HUGEPAGES_DIR/aeron"
export AERON_FILE_PAGE_SIZE="$ERGON_HUGEPAGE_SIZE"
```

An unsized hugetlbfs — what `emptyDir: {medium: HugePages}` gives you — is
rejected by Aeron. That is why the size comes from the annotation.

**Affinity variables:**

| variable | agent | here |
|---|---|---|
| `AERON_SENDER_CPU_AFFINITY` | sender | `isolated:0` |
| `AERON_RECEIVER_CPU_AFFINITY` | receiver | `isolated:1` |
| `AERON_CONDUCTOR_CPU_AFFINITY` | conductor | leave unset: it starts on the shared CPUs |
| `AERON_DRIVER_NATIVE_RESOURCE_AGENT_CPU_AFFINITY` | native resource agent | leave unset |
| `AERON_DRIVER_CPUSET_AFFINITY` | makes the four above indexes into the cgroup cpuset | keep `false`; the wrapper refuses to translate when it is true |

`ergon-cpus` turns `isolated:<i>` into a real CPU number before exec. It
also sets its own affinity to the shared CPUs, so the driver starts there.

**Check it worked:**

```sh
kubectl exec aeron-driver -- sh -c 'echo isolated=$ERGON_ISOLATED_CPUS shared=$ERGON_SHARED_CPUS'
kubectl exec aeron-driver -- sh -c 'echo hugepages=$ERGON_HUGEPAGES_DIR size=$ERGON_HUGEPAGE_SIZE'
kubectl exec aeron-driver -- sh -c 'ps -L -o pid,tid,psr,comm | head'
```

`psr` must show the sender and receiver on 2 and 3 and every other thread on
0. `mount | grep hugepages` must show a size, not just `hugetlbfs`.

## Fractional shared CPUs

- Extended resources are whole numbers and requests must equal limits, so
  the unit is millicpus: `250` = a quarter.
- The plugin adds `ceil(M/1000)` shared CPUs, at least one.
- The fraction is scheduler accounting only. With 2 shared CPUs and
  oversubscription 2, a node offers 4000; sixteen pods at 250 fit.
- There is no hard cap. Helpers may use the whole shared CPU, shared with
  the kubelet, containerd, system daemons and other pods.
- A helper wake-up on a shared CPU can wait milliseconds under node load.
  That slows connects and the conductor, not the loop.

## Fail closed

| event | effect | guard |
|---|---|---|
| plugin down at create | NRI makes no change | `required-plugins.noderesource.dev` makes the create fail; the wrapper refuses without `ERGON_SHARED_CPUS`; the resource does not allocate |
| kubelet restart, plugin up | the re-write keeps the union | the plugin re-widens in `UpdateContainer` |
| kubelet restart, plugin down | cpuset shrinks to the exclusive CPUs; helpers fall onto them | the plugin re-widens on return; helpers self-check meanwhile |
| hook binary missing | the create fails | the DaemonSet installs the binary before it connects |

## Security

- The plugin is root on the node: anything on the NRI socket can change any
  container.
- It mounts `/var/run/nri`, `/var/lib/kubelet/device-plugins` and
  `/opt/ergon/bin` (it installs the hook binary there).
- Nothing else: no `hostPID`, no `hostNetwork`, not privileged,
  `capabilities.drop: [ALL]`, `readOnlyRootFilesystem: true`, seccomp
  `RuntimeDefault`, no API access.
- Only namespaces in the allowlist may opt in. A pod author cannot fake the
  device-plugin annotation.

## Limits and what is not verified

- **AKS** has no supported way to enable containerd NRI plugins and no
  `reservedSystemCPUs`. Not supported.
- **CRI-O** users use its `shared_cpuset` instead.
- **Bottlerocket**: whether runc may exec a hook binary that a pod wrote to
  a host path is not verified. Use `defaultAffinity: all` and the wrapper
  until checked.
- `kubectl exec` and exec probes start on any CPU of the cpuset, exclusive
  ones included. No exec probes on hot containers; run debug commands as
  `ergon-cpus -- <cmd>`.
- In-place CPU resize leaves the CPU env stale. Use
  `resizePolicy: RestartContainer` for `cpu`.
- Not verified on a real EKS node: the NRI socket, nodeadm TOML merge,
  `/opt` writable, `system-node-critical` outside kube-system. Two EKS spot
  lab attempts reached neither nodes (no spot capacity).
- Not measured: latency of helpers on shared CPUs.
