# ergon-shared-cpus

A containerd NRI plugin and a kubelet device plugin in one daemon, plus a
small tool. Together they give a Kubernetes pod its exclusive CPUs **and**
the node's shared housekeeping CPUs, and start every thread on the shared
ones. The busy-spin loop keeps its exclusive core; only sleepy helpers
(dial, TLS, warnings, Aeron conductors) run on the shared CPUs.

Experimental and unpublished. Nothing is pushed to a registry. Tested on
kind, not yet on EKS.

## What the app sees

A Guaranteed pod asks for integer CPUs and for
`ergon.io/shared-millicpus` (millicpus, because extended resources are
whole numbers and requests must equal limits):

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: trader
  namespace: trading                       # must be in the plugin's allowlist
  annotations:
    required-plugins.noderesource.dev: '["ergon-shared-cpus"]'   # fail closed
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
| `cpuset.cpus.effective` | the 2 exclusive CPUs plus 1 shared CPU |
| `ERGON_ISOLATED_CPUS` | the exclusive CPUs, sorted, comma list: `1,5` |
| `ERGON_SHARED_CPUS` | this container's shared CPUs: `4` |
| `ERGON_ISOLATED_CORES` | exclusive CPUs grouped by physical core: `1,5` |
| `ERGON_SHARED_MILLICPUS` | `250` |
| `ERGON_HELPER_CPUS` | same as `ERGON_SHARED_CPUS` |
| `OPENSHIFT_ISOLATED_CPUS` / `OPENSHIFT_SHARED_CPUS` | `1,5` / `4` (CRI-O style) |
| affinity of PID 1 and of every new thread | the shared CPUs only |

`250` is a quarter core of **scheduler accounting only**. The plugin adds
`ceil(M/1000)` shared CPUs (here 1). There is no hard cap: helpers may use
the whole shared CPU, shared with the kubelet, system daemons and other
pods.

## Pinning

Pin hot threads by index into `ERGON_ISOLATED_CPUS`. The app never needs
the real CPU numbers. Leave helpers alone: they start on the shared CPUs.

```c
/* Pin the calling thread to isolated CPU index i. Returns the CPU, or -1. */
int pin_isolated(int i);   /* full code in examples/pin.c */
```

See [examples/pin.c](examples/pin.c) (C) and
[examples/pin.rs](examples/pin.rs) (Rust, std only). A planned typed Rust
API in `ergon-tune` is not built yet.

Rules:

- never run a spinner or SQPOLL on a shared CPU;
- create helpers from a thread that is still on the shared set (a new
  thread copies its creator's mask);
- each sleepy helper checks `sched_getcpu()` when it wakes and alarms if it
  is not on `ERGON_SHARED_CPUS`.

## ergon-cpus

One tool, three jobs.

| command | what |
|---|---|
| `ergon-cpus hook --shared S` | OCI `createRuntime` hook. The plugin injects it; it sets every thread of the new container to `S` |
| `ergon-cpus [--env NAME=SPEC]... [--affinity shared\|keep] -- cmd` | exec wrapper. Turns `isolated:<i>` into a real CPU number, then execs |
| `ergon-cpus show` | prints the lists, cores, affinity and `cpuset.cpus.effective` as JSON |

SPEC forms for `--env`:

| SPEC | becomes |
|---|---|
| `isolated:<i>` | the i-th CPU of `ERGON_ISOLATED_CPUS` |
| `shared:<i>` | the i-th CPU of `ERGON_SHARED_CPUS` |
| `core:<j>` / `core:<j>:<t>` | CPU t (default 0) of core group j |
| `isolated` / `shared` | the whole list |

Aeron C media driver example:

```yaml
command: ["ergon-cpus",
          "--env", "AERON_SENDER_CPU_AFFINITY=isolated:0",
          "--env", "AERON_RECEIVER_CPU_AFFINITY=isolated:1",
          "--", "aeronmd"]
env: [{ name: AERON_THREADING_MODE, value: DEDICATED }]
```

Leave `AERON_CONDUCTOR_CPU_AFFINITY` and
`AERON_DRIVER_NATIVE_RESOURCE_AGENT_CPU_AFFINITY` unset: those agents start
on the shared CPUs. Keep `AERON_DRIVER_CPUSET_AFFINITY` false; the wrapper
refuses to translate Aeron variables when it is true.

## Build and test

Nothing is installed on the host. Go and builds run in containers.

```sh
scripts/check.sh          # gofmt, go vet, go test -race, static builds, then the cgroup kernel lane
scripts/check.sh --local  # the unit lane on the host (CI)
scripts/check-cgroup.sh   # the cgroup kernel lane on its own
docker build -t ergon-shared-cpus:dev .
test/e2e.sh               # the 18-step kind e2e (Docker only, needs >= 6 CPUs)
```

The cgroup kernel lane runs `test/cgroup` against a real cgroup v2
hierarchy, in a privileged container with the host's cgroup namespace. The
unit fixtures in `internal/hook` are ordinary files, so they accept writes a
kernel would reject — a bare name in `cgroup.subtree_control` is EINVAL, for
example. This lane is what proves the partition is one a real kernel will
take a task in, and that one thread of a process can sit on the exclusive
CPUs while the others stay on the shared ones.

The e2e builds both images, creates a kind cluster, runs every check and
deletes every container, cluster, image and volume it created, on success,
failure and interrupt.

## Deploy

1. Node: Kubernetes >= 1.33, `cpuManagerPolicy: static`,
   `reservedSystemCPUs` = the IRQ CPU(s) + the shared CPUs, kernel >= 6.2,
   cgroup v2, containerd >= 2.2 with the NRI default validator on. Label
   the node `ergon.io/shared-cpus=true`. A `cpu-partition` opt-in also needs
   `cpuset.cpus.exclusive`, which is Linux 6.7 or a vendor kernel that
   backported it: 6.2 is only enough for the shared CPUs. The plugin reads
   the file rather than the version, so a backport is accepted.
2. Image: `docker build -t ergon-shared-cpus:dev .` and load it on the node.
3. `kubectl apply -f deploy/namespace.yaml -f deploy/configmap.yaml -f deploy/daemonset.yaml`
4. Edit `deploy/configmap.yaml` first: `sharedCpus` must be inside
   `reservedSystemCPUs`, and `namespaces` is the allowlist.

containerd:

```toml
[plugins."io.containerd.nri.v1.nri".default_validator]
  enable = true
  # leave required_plugins empty: a node-wide list would also block
  # kube-system pods and this plugin's own pod while it is down
```

The DaemonSet needs three host paths (`/var/run/nri`,
`/var/lib/kubelet/device-plugins`, `/opt/ergon/bin`) and the namespace
needs Pod Security `privileged`. It is root on the node: anything on the
NRI socket can change any container.

## Config

`/etc/ergon-shared-cpus/config.yaml`, from a ConfigMap. Strict YAML, no hot
reload.

```yaml
sharedCpus: "4"            # required. Inside reservedSystemCPUs
oversubscription: 1        # capacity = len(sharedCpus) x 1000 x this, max 100000
namespaces: [trading]      # allowlist; empty refuses every opt-in
defaultAffinity: shared    # shared (hook) | all (no hook; use the wrapper)
hookPath: /opt/ergon/bin/ergon-cpus   # must end with /ergon-cpus (written as root)
annotationOptIn: false     # allow cpu-shared.ergon.io/<container> without the device plugin
metricsAddress: ":9466"
```

## Verify

```sh
kubectl -n ergon-system get pods -l app=ergon-shared-cpus   # Ready
kubectl get node <node> -o jsonpath='{.status.allocatable.ergon\.io/shared-millicpus}'
curl -s <pod-ip>:9466/readyz
curl -s <pod-ip>:9466/metrics
```

Then run a pod with the example above and check
`cpuset.cpus.effective` and the `ERGON_*` variables.

## Failure modes

| event | effect |
|---|---|
| plugin down at create | `required-plugins` makes the create fail (containerd >= 2.2); the wrapper refuses without `ERGON_SHARED_CPUS`; the device plugin is down too, so the resource does not allocate |
| kubelet restart, plugin up | the re-write goes through `UpdateContainer` and keeps the union |
| kubelet restart, plugin down | cpuset shrinks to the exclusive CPUs; helpers fall onto them; `Synchronize` re-widens on return |
| kernel < 6.2 or cgroup v1 | the plugin exits with one clear line |
| kernel has no `cpuset.cpus.exclusive` | shared-only pods work; every `cpu-partition` opt-in is refused and the pod stays out of the partition (6.7 or a vendor backport of that file) |

## Limits

- **AKS**: no supported way to enable containerd NRI plugins and no
  `reservedSystemCPUs`. Not supported.
- **CRI-O**: use its own `shared_cpuset` / `cpu-shared.crio.io` instead.
- **Bottlerocket**: whether runc may exec a hook binary that a pod wrote to
  a host path is not verified (read-only root, SELinux). Use
  `defaultAffinity: all` and the wrapper until checked.
- `kubectl exec` and exec probes start on any CPU of the cpuset, exclusive
  ones included. No exec probes on hot containers; run debug commands as
  `ergon-cpus -- <cmd>`.
- Apps that read their CPUs from their own affinity (`nproc`,
  `sched_getaffinity`) now see only the shared CPUs. Read
  `ERGON_ISOLATED_CPUS` or `cpuset.cpus.effective` instead.
- In-place CPU resize leaves the CPU env stale. Use
  `resizePolicy: RestartContainer` for `cpu`.
- Not verified on a real EKS node: the NRI socket, nodeadm TOML merge,
  `/opt` writable, `system-node-critical` outside kube-system.
- Not measured: latency of helpers on shared CPUs. A wake-up there can wait
  milliseconds under node load.

## Trust

Only a marked container is touched: the device plugin's container
annotation `ergon.io/shared-millicpus` (pods cannot set container
annotations), or the pod annotation `cpu-shared.ergon.io/<container>` when
`annotationOptIn` is on. The marker also names the millicpu request.

The `ERGON_*` env is set by the plugin and is not a marker: a pod can set
env by hand. A hand-set `ERGON_SHARED_CPUS` is ignored; only the shared
CPUs the plugin itself recorded are ever re-widened, and only inside
`sharedCpus`. A container that is no longer exclusive integer-CPU (CPU
quota or wrong shares) loses its shared CPUs.

## Contract

The resource name, the annotation keys and the env names are the contract.
Changes go in the repo `CHANGELOG.md`. `--version` prints the version, the
git describe string and the NRI module version.

Apache-2.0. The repo `LICENSE` applies.
