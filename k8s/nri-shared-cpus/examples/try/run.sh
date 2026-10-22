#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Throwaway kind cluster. Installs the public ergon-shared-cpus chart 0.1.0,
# applies the latency-pod admission policy from this repo, runs one hot pod,
# and shows that an exec probe is refused.
#
# Needs: docker (at least 6 CPUs), helm, kubectl, kind.
# Uses KUBECONFIG under /tmp. Does not read or write ~/.kube/config.
# Deletes the cluster it created, on success, failure and interrupt. No cloud.
#
#   examples/try/run.sh
#
# KIND=/path/to/kind examples/try/run.sh
set -uo pipefail

TRY=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$TRY/../../../.." && pwd)
ADMISSION="$ROOT/k8s/nri-latency-pods/admission"
CLUSTER=ergon-shared-cpus-try
KUBECONFIG=${KUBECONFIG:-/tmp/ergon-shared-cpus-try.kubeconfig}
export KUBECONFIG
KIND=${KIND:-kind}
LOG=/tmp/ergon-shared-cpus-try.log
: >"$LOG"

say() { printf '%s\n' "$*" | tee -a "$LOG"; }

if [[ "${KUBECONFIG}" != /tmp/ergon-shared-cpus-try.kubeconfig ]]; then
  say "refusing: KUBECONFIG must stay /tmp/ergon-shared-cpus-try.kubeconfig"
  exit 1
fi

cleanup() {
  say "deleting kind cluster ${CLUSTER}"
  "$KIND" delete cluster --name "$CLUSTER" >>"$LOG" 2>&1 || true
  rm -f "$KUBECONFIG"
}
trap cleanup EXIT

fail() {
  say "FAIL: $*"
  {
    echo "=== nodes ==="
    kubectl get nodes -o wide 2>&1 || true
    echo "=== pods ==="
    kubectl get pods -A -o wide 2>&1 || true
    echo "=== plugin logs ==="
    kubectl -n ergon-system logs -l app=ergon-shared-cpus --tail=200 2>&1 || true
    echo "=== trader ==="
    kubectl -n trading describe pod trader 2>&1 || true
    kubectl -n trading logs trader 2>&1 || true
  } >>"$LOG"
  say "log: $LOG"
  exit 1
}

command -v docker >/dev/null || fail "docker is not on PATH"
command -v helm >/dev/null || fail "helm is not on PATH"
command -v kubectl >/dev/null || fail "kubectl is not on PATH"
command -v "$KIND" >/dev/null || fail "kind is not on PATH (set KIND= or install kind v0.33.0)"
[[ -f "$ADMISSION/priority-class.yaml" ]] || fail "admission YAML missing at $ADMISSION"

ncpu=$(docker info --format '{{.NCPU}}' 2>/dev/null || echo 0)
say "docker CPUs: $ncpu"
[[ "$ncpu" -ge 6 ]] || fail "Docker has ${ncpu} CPUs; this sample needs at least 6"

if "$KIND" get clusters 2>/dev/null | grep -qx "$CLUSTER"; then
  fail "cluster ${CLUSTER} already exists; delete it before running this sample"
fi

say "creating ${CLUSTER} (kindest/node:v1.33.0)"
"$KIND" create cluster \
  --name "$CLUSTER" \
  --config "$TRY/kind.yaml" \
  --kubeconfig "$KUBECONFIG" \
  --wait 300s >>"$LOG" 2>&1 || fail "kind create failed; see $LOG"

kubectl get nodes -o wide >>"$LOG" || fail "kubectl cannot reach the cluster"
node=$(kubectl get nodes -o jsonpath='{.items[0].metadata.name}')
kubectl label node "$node" ergon.io/shared-cpus=true --overwrite >>"$LOG" || fail "node label failed"
say "node: $node"

say "helm install oci://ghcr.io/mimran1980/charts/ergon-shared-cpus --version 0.1.0"
helm install ergon-shared-cpus oci://ghcr.io/mimran1980/charts/ergon-shared-cpus \
  --version 0.1.0 \
  --namespace ergon-system --create-namespace \
  --set config.sharedCpus=0 \
  --set 'config.namespaces={trading}' \
  --wait --timeout 300s >>"$LOG" 2>&1 || fail "helm install failed; see $LOG"
say "chart 0.1.0 notes still say it is not released. That sentence is stale. This install is the public chart."

say "waiting for ergon.io/shared-millicpus"
ok=0
i=0
while [[ "$i" -lt 60 ]]; do
  alloc=$(kubectl get node "$node" -o go-template='{{index .status.allocatable "ergon.io/shared-millicpus"}}' 2>/dev/null || true)
  if [[ -n "$alloc" && "$alloc" != "<no value>" ]]; then
    say "allocatable ergon.io/shared-millicpus=${alloc}"
    ok=1
    break
  fi
  i=$((i + 1))
  sleep 2
done
[[ "$ok" -eq 1 ]] || fail "the device plugin did not publish ergon.io/shared-millicpus"

kubectl create namespace trading >>"$LOG" 2>&1 || fail "namespace trading already exists or could not be created"
kubectl apply -f "$ADMISSION/priority-class.yaml" >>"$LOG" || fail "priority classes failed"
kubectl apply -f "$ADMISSION/validating-admission-policy.yaml" >>"$LOG" || fail "admission policy failed"

say "applying the hot pod"
kubectl apply -f "$TRY/hot-pod.yaml" >>"$LOG" || fail "hot pod was refused"
kubectl wait -n trading --for=condition=Ready pod/trader --timeout=180s >>"$LOG" 2>&1 || fail "trader did not become Ready"

# The container prints once, then sleeps. Give the log a moment.
sleep 2
grant=$(kubectl -n trading logs trader 2>>"$LOG" || true)
printf '%s\n' "$grant" | tee -a "$LOG"
printf '%s\n' "$grant" | grep -q '^ERGON_SHARED_CPUS=0$' || fail "ERGON_SHARED_CPUS is not 0"
printf '%s\n' "$grant" | grep -q '^ERGON_ISOLATED_CPUS=..*' || fail "ERGON_ISOLATED_CPUS is empty"
printf '%s\n' "$grant" | grep -q '^Cpus_allowed_list:[[:space:]]*0$' || fail "PID 1 Cpus_allowed_list is not the shared CPU 0"
printf '%s\n' "$grant" | grep -q '^nproc=1$' || fail "nproc is not 1; PID 1 can see more than the shared CPU"

say "applying the exec-probe pod (expect deny)"
deny=$(kubectl apply -f "$TRY/bad-probe.yaml" 2>&1 || true)
printf '%s\n' "$deny" | tee -a "$LOG"
printf '%s\n' "$deny" | grep -q 'livenessProbe must not be exec' || fail "the exec probe was not denied"
if kubectl -n trading get pod trader-bad-probe >/dev/null 2>&1; then
  fail "trader-bad-probe exists; the policy did not deny it"
fi

say "PASS"
say "log: $LOG"
