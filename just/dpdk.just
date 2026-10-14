# ergon-md-dpdk: the DPDK ENA kernel-bypass transport for the rusteron media
# driver. The `dpdk` feature builds only on Linux x86_64 with libdpdk >= 23.11
# (Ubuntu 24.04: `libdpdk-dev`), so these recipes are not part of `just test`
# on other hosts; CI runs them in the `dpdk` job. The portable half of the
# crate (config, environment, binary selection) is in the regular recipes.

# Clippy and every test of the DPDK transport against the DPDK-free fakes (Linux x86_64 + libdpdk)
check-dpdk:
    cargo clippy -p ergon-md-dpdk -p ergon-md-dpdk-harness --all-targets --features ergon-md-dpdk/dpdk,ergon-md-dpdk-harness/dpdk -- -D warnings
    cargo test -p ergon-md-dpdk --features dpdk -- --test-threads=1
    cargo test -p ergon-md-dpdk --features static,dpdk -- --test-threads=1

# Interop over DPDK tap devices on a Linux bridge (root, /dev/net/tun, hugepages)
test-dpdk-vdev *args:
    sudo env "PATH=$PATH" scripts/test-dpdk-vdev.sh {{args}}

# Node bootstrap and preflight against fake sysfs and IMDS trees
test-dpdk-bootstrap:
    bash md-dpdk/deploy/aws-eks/node/test/test-bootstrap.sh

# `check-dpdk`'s tests in Docker amd64 emulation, for macOS and arm64 hosts
verify-dpdk-docker *args:
    bash md-dpdk/native/dpdk/test/verify-dpdk.sh {{args}}
