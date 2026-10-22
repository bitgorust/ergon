# ergon

[![Crates.io](https://img.shields.io/crates/v/ergo-sbe)](https://crates.io/crates/ergo-sbe)
[![CI](https://github.com/mimran1980/ergon/actions/workflows/ci.yml/badge.svg)](https://github.com/mimran1980/ergon/actions/workflows/ci.yml)
[![API Docs](https://docs.rs/ergo-sbe/badge.svg)](https://docs.rs/ergo-sbe/)
[![Book](https://img.shields.io/badge/book-mimran1980.github.io%2Fergon-blue)](https://mimran1980.github.io/ergon/)

Rust crates for low-latency trading: an SBE codec generator, an Aeron Cluster
client, busy-spin network I/O, host checks and a DPDK transport for the Aeron
media driver. 0.x and not production-ready: APIs may change. Exit criteria:
[Road to 1.0](https://mimran1980.github.io/ergon/project/road-to-1.0.html).

> **AI assistance.** This repository was developed with heavy AI assistance.
> Process notes for `ergo-sbe`:
> [AI-ASSISTANCE.md](https://github.com/mimran1980/ergon/blob/main/AI-ASSISTANCE.md).

**Tuning Kubernetes for HFT?
[Kubernetes HFT tuning](https://mimran1980.github.io/ergon/deploy/hft.html)
— start there.** It lists each setting, marks which ones were measured, and
has the copy-paste blocks.

The `ergon-shared-cpus` image is built from
[k8s/nri-shared-cpus](https://github.com/mimran1980/ergon/tree/main/k8s/nri-shared-cpus).
Image `ghcr.io/mimran1980/ergon-shared-cpus:0.1.0`
(digest `sha256:d28fea2f95ebce3e53d33f8770f3ba69be75c76ec7437ea2b92c1329759e07e9`).
Chart `oci://ghcr.io/mimran1980/charts/ergon-shared-cpus` version `0.1.0`
(digest `sha256:7347131d7d87caee5cf6de11b2a11e2b62454da59c41092b3805bc2834795dbd`).
Anyone can pull both. No image pull secret.
`docker buildx imagetools inspect ghcr.io/mimran1980/ergon-shared-cpus:0.1.0`
prints the image digest. The chart records a tag. It cannot take `@sha256`.

Kubernetes HFT tuning is the list of node and pod settings in the
[HFT chapter](https://mimran1980.github.io/ergon/deploy/hft.html).
There is no second image. Helper threads land on the shared CPUs through
this chart. An exec probe on a hot pod is refused by the admission YAML in
[k8s/nri-latency-pods](https://github.com/mimran1980/ergon/tree/main/k8s/nri-latency-pods),
which is not inside the chart. Hugepages, an isolated cpuset partition,
io_uring seccomp and the NIC PHC are opt-in grants in the same image.
Chart 0.1.0 does not write `seccomp`, `ptp` or `schedFifo`, so those stay
off. EKS is not verified. Latency figures are not measured.

On your own cluster:

1. Prepare the node. The chart does not. Kubernetes >= 1.33, cgroup v2,
   kernel >= 6.2, containerd >= 2.2 with NRI and the default validator on,
   `cpuManagerPolicy: static`, `reservedSystemCPUs` equal to the IRQ CPU
   plus the shared CPUs, and the label `ergon.io/shared-cpus=true`.
   Isolate only the exclusive CPUs on the boot line. AKS cannot do this.
2. Install. `config.sharedCpus` must sit inside `reservedSystemCPUs` and
   must not be the IRQ CPU. For the 8-CPU split (shared `0`, IRQ `1`,
   exclusive `2-5`) the value is `0`.

```sh
helm install ergon-shared-cpus oci://ghcr.io/mimran1980/charts/ergon-shared-cpus \
  --version 0.1.0 \
  --namespace ergon-system --create-namespace \
  --set config.sharedCpus=0 \
  --set 'config.namespaces={trading}'
```

3. Apply `admission/priority-class.yaml` and
   `admission/validating-admission-policy.yaml` from
   `k8s/nri-latency-pods`. Do not apply `admission/examples/` or the
   mutating policy. A pod labelled `ergon.io/latency: hot` then needs a
   priority class, `ergon.io/shared-millicpus`, and the
   `required-plugins.noderesource.dev` annotation. An exec probe is denied.
4. Run a Guaranteed pod in an allowlisted namespace: whole-number `cpu`,
   requests equal limits, `ergon.io/shared-millicpus: "250"`.
   PID 1 `Cpus_allowed_list` is the shared CPUs.
   `cpuset.cpus.effective` is those CPUs plus the exclusive ones.

`helm install` of chart 0.1.0 prints "This chart is NOT released".
That note was packed before the push. The image and the chart at that
tag are the public ones.

A kind sample pulls this chart and runs the hot pod and the admission
denial:
[k8s/nri-shared-cpus/examples/try](https://github.com/mimran1980/ergon/tree/main/k8s/nri-shared-cpus/examples/try).
On 2026-10-22 that script, chart `0.1.0`, `kindest/node:v1.33.0`, printed
`ERGON_ISOLATED_CPUS=2,3`, `ERGON_SHARED_CPUS=0`,
`cpuset.cpus.effective=0,2-3`, PID 1 `Cpus_allowed_list` `0` and `nproc=1`,
and the API server denied the exec-probe pod. It does not measure latency,
and it does not grant hugepages, PTP, or an isolated cpuset partition.
The longer write-up is the
[shared CPUs chapter](https://mimran1980.github.io/ergon/deploy/shared-cpus.html).

## Crates

| Path | Crate | What it is |
|---|---|---|
| [`sbe/`](sbe/) | `ergo-sbe` | SBE schema XML → Rust codecs, generated in `build.rs` |
| [`cluster/`](cluster/) | `ergo-aeron-cluster` | Aeron Cluster client. Experimental, not a production substitute |
| [`io/`](io/) | `ergon-io` | WebSocket and HTTP/1.1 over TLS 1.3 on one busy-spin thread; epoll or `io_uring` |
| [`tune/`](tune/) | `ergon-tune` | Reads the host's low-latency settings and reports findings; changes nothing |
| [`md-dpdk/`](md-dpdk/) | `ergon-md-dpdk` | DPDK transport for the Aeron media driver, Linux x86_64 and aarch64. Proof of concept |
| [`k8s/nri-shared-cpus`](https://github.com/mimran1980/ergon/tree/main/k8s/nri-shared-cpus) | Helm chart | Gives a pod its exclusive CPUs plus the node's shared CPUs. Public chart: `oci://ghcr.io/mimran1980/charts/ergon-shared-cpus` version `0.1.0`. Image: `ghcr.io/mimran1980/ergon-shared-cpus:0.1.0` |
| [`k8s/nri-latency-pods`](k8s/nri-latency-pods/) | node and admission setup | Node tuning, an admission policy and pod annotations for latency pods (EKS AL2023) |

Unpublished support: `sbe/benchmarks`, `cluster-test-harness`,
`io/benches`, `md-dpdk/harness`, `md-dpdk/device-plugin` and seven sample
crates in [`samples/`](samples/README.md).

Start with `ergo-sbe`. Add another crate only for a need you have. Which one
does what, and how to call it: the
[book introduction](https://mimran1980.github.io/ergon/).

## Set up

Rust 1.99 or newer. The Aeron and SBE upstreams are submodules:

```sh
git submodule update --init --recursive
just policy
just check-products
just test        # full suite; builds the Aeron Java jars
```

Every check and what CI runs: [CONTRIBUTING.md](CONTRIBUTING.md).

## Project rules

- Official SBE wire compatibility takes priority over API convenience.
- Maintained hot paths are compared with sbe-tool output on equal work
  ([method](sbe/BENCHMARKS.md)). Treat a surprising ratio as a benchmark
  defect first.
- Checked entry points report malformed input rather than return default,
  empty or lossy values.
- The cluster crate is a client only: no consensus module, service container,
  archive or admin tool.

## Documentation

- [Book](https://mimran1980.github.io/ergon/): guides for every crate and
  deployment
- API: [ergo-sbe](https://docs.rs/ergo-sbe/),
  [ergo-aeron-cluster](https://docs.rs/ergo-aeron-cluster/)
- [CHANGELOG.md](CHANGELOG.md), [SECURITY.md](SECURITY.md)

Apache-2.0.
