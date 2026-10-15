#!/usr/bin/env bash
# Compile-verify and test the `dpdk` feature of ergon-md-dpdk on
# Linux x86_64. Runs in Docker amd64 emulation (works on an arm64 host);
# builds the environment image once, then runs the full feature build + ABI
# test against the workspace mounted read-only.
#
# Usage: ./verify-dpdk.sh [--build] [test_filter]
#   --build   (re)build the verification image (ergon-md-dpdk-verify)
#   test_filter  optional cargo test filter, default `--test dpdk_abi`
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../../.." && pwd)"
IMAGE="ergon-md-dpdk-verify"
FILTER="${1:---test dpdk_abi}"

if [[ "${1:-}" == "--build" ]]; then
  docker build --platform linux/amd64 -t "$IMAGE" "$HERE"
  shift
  FILTER="${1:---test dpdk_abi}"
fi

# dpdk_config mutates process-global env vars, so its tests must run
# single-threaded; a single-binary run is unaffected.
# --shm-size=512m is required: the Aeron driver's IPC publication allocates a
# ~200MB log in /dev/shm, and docker's default 64MB shm fails it with
# "(-12) insufficient storage space" (aeron_driver_context_run_storage_checks).
# The target dir and cargo registry live in named volumes, so the host tree is
# untouched and later runs rebuild incrementally.
docker run --rm --platform linux/amd64 \
  --shm-size=512m \
  -v "$REPO:/src:ro" \
  -v ergon-md-dpdk-target:/target \
  -v ergon-md-dpdk-cargo-registry:/root/.cargo/registry \
  -e CARGO_TARGET_DIR=/target \
  -w /src \
  "$IMAGE" \
  bash -c "set -o pipefail; cargo test -p ergon-md-dpdk --features dpdk $FILTER -- --test-threads=1 2>&1 | tail -40"
