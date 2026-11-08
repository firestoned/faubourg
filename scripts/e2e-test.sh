#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, faubourg
# SPDX-License-Identifier: Apache-2.0
#
# e2e-test.sh: a Host bootstrapped end to end in a throwaway kind cluster (ADR-0009 … ADR-0011),
# with every faubourg piece real and only banlieue simulated:
#
#   - the bootstrap image, built here and pulled by digest from a local registry;
#   - the admission policies and RBAC from deploy/;
#   - the controller binary, outside the cluster, with its own ServiceAccount's token;
#   - an OpenSSH "host" (test/sshd) as a pod behind a Service.
#
# banlieue's operator is simulated with kubectl: it creates the Provider's ServiceAccount and
# reports the Provider's identity and health. The host's install script is a test script that
# reads the credential bundle and installs it where banlieue's provider would read it.
#
# Needs: kind, kubectl, jq, podman, ssh-keygen, cargo. Set KIND_EXPERIMENTAL_PROVIDER=podman.
#
#   E2E_CLUSTER   kind cluster name (default faubourg-e2e); deleted on exit
#   E2E_KEEP=1    keep the cluster and registry afterwards
#
# Usage: scripts/e2e-test.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLUSTER="${E2E_CLUSTER:-faubourg-e2e}"
REG="${CLUSTER}-registry"
REG_PORT="${E2E_REGISTRY_PORT:-5001}"
CREDS=faubourg-credentials
BANLIEUE=banlieue-system
SA=banlieue-provider-cloud-hypervisor-hv01
HOST_ADDR=hv01.hosts.svc.cluster.local

for tool in kind kubectl jq podman ssh-keygen cargo; do
  command -v "$tool" >/dev/null 2>&1 || { echo "e2e-test: $tool not found" >&2; exit 1; }
done

WORK="$(mktemp -d)"
export KUBECONFIG="$WORK/admin.kubeconfig"
CTL_PID=""
cleanup() {
  if [ -n "$CTL_PID" ]; then kill "$CTL_PID" 2>/dev/null || true; wait "$CTL_PID" 2>/dev/null || true; fi
  if [ "${E2E_KEEP:-}" != 1 ]; then
    kind delete cluster --name "$CLUSTER" >/dev/null 2>&1 || true
    podman rm -f "$REG" >/dev/null 2>&1 || true
    rm -rf "$WORK"
  else
    echo "e2e-test: kept cluster $CLUSTER, registry $REG and $WORK"
  fi
}
trap cleanup EXIT

echo "e2e-test: building the controller and the images"
(cd "$ROOT" && cargo build -q -p faubourg-controller --bin faubourg-controller --example provider-crd)
BIN="$ROOT/target/debug/faubourg-controller"
podman build -q -f "$ROOT/images/bootstrap/Containerfile" -t localhost/faubourg-bootstrap:e2e "$ROOT" >/dev/null
ssh-keygen -q -t ed25519 -N "" -C "" -f "$WORK/run"
ssh-keygen -q -t ed25519 -N "" -C "" -f "$WORK/placement"
mkdir -p "$WORK/sshd-ctx"
cp "$WORK/run.pub" "$WORK/placement.pub" "$ROOT/test/sshd/Containerfile" "$WORK/sshd-ctx/"
podman build -q -t localhost/faubourg-test-sshd:e2e "$WORK/sshd-ctx" >/dev/null

echo "e2e-test: creating kind cluster $CLUSTER with a local registry"
cat >"$WORK/kind.yaml" <<'EOF'
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
containerdConfigPatches:
  - |-
    [plugins."io.containerd.grpc.v1.cri".registry]
      config_path = "/etc/containerd/certs.d"
EOF
kind create cluster --name "$CLUSTER" --kubeconfig "$KUBECONFIG" --config "$WORK/kind.yaml" --wait 180s >/dev/null
podman rm -f "$REG" >/dev/null 2>&1 || true
podman run -d --name "$REG" --network kind -p "127.0.0.1:${REG_PORT}:5000" docker.io/library/registry:2 >/dev/null
for node in $(kind get nodes --name "$CLUSTER"); do
  podman exec "$node" sh -c "mkdir -p /etc/containerd/certs.d/localhost:${REG_PORT} && \
    printf '[host.\"http://${REG}:5000\"]\n' > /etc/containerd/certs.d/localhost:${REG_PORT}/hosts.toml"
done
push() { # local image, name -> prints name@digest as the cluster pulls it
  local ref="localhost:${REG_PORT}/$2:e2e"
  podman push -q --tls-verify=false --digestfile "$WORK/$2.digest" "$1" "$ref" >/dev/null
  echo "localhost:${REG_PORT}/$2@$(cat "$WORK/$2.digest")"
}
IMAGE="$(push localhost/faubourg-bootstrap:e2e faubourg-bootstrap)"
SSHD_IMAGE="$(push localhost/faubourg-test-sshd:e2e faubourg-test-sshd)"
echo "e2e-test: bootstrap image $IMAGE"

kubectl apply -f "$ROOT/deploy/crds/" >/dev/null
"$ROOT/target/debug/examples/provider-crd" | kubectl apply -f - >/dev/null
kubectl wait --for=condition=Established crd --all --timeout=60s >/dev/null
kubectl create namespace "$BANLIEUE" >/dev/null
kubectl apply -f "$ROOT/deploy/rbac/controller.yaml" >/dev/null
kubectl create --dry-run=client -o json -f "$ROOT/deploy/admission/bootstrap-policy-params.yaml" \
  | jq -c --arg img "$IMAGE" '.data.image = $img' | kubectl apply -f - >/dev/null
kubectl apply -f "$ROOT/deploy/admission/bootstrap-policy.yaml" >/dev/null
kubectl apply -f - >/dev/null <<'EOF'
{"apiVersion": "banlieue.io/v1alpha1", "kind": "ProviderClass", "metadata": {"name": "cloud-hypervisor"},
 "spec": {"backend": "cloud-hypervisor", "deployment": "External"}}
EOF
kubectl apply -f - >/dev/null <<'EOF'
{"apiVersion": "faubourg.firestoned.io/v1alpha1", "kind": "AvailabilityZone",
 "metadata": {"name": "az1"},
 "spec": {"storage": {"mode": "Local"}, "providerLabels": {"topology.home.example/rack": "r1"}}}
EOF

echo "e2e-test: starting the host"
kubectl create namespace hosts >/dev/null
kubectl -n hosts apply -f - >/dev/null <<EOF
{"apiVersion": "v1", "kind": "List", "items": [
 {"apiVersion": "v1", "kind": "Pod", "metadata": {"name": "hv01", "labels": {"app": "hv01"}},
  "spec": {"containers": [{"name": "sshd", "image": "$SSHD_IMAGE", "ports": [{"containerPort": 22}]}]}},
 {"apiVersion": "v1", "kind": "Service", "metadata": {"name": "hv01"},
  "spec": {"selector": {"app": "hv01"}, "ports": [{"port": 22, "targetPort": 22}]}}]}
EOF
kubectl -n hosts wait --for=condition=Ready pod/hv01 --timeout=120s >/dev/null
on_host() { kubectl -n hosts exec hv01 -- sh -c "$1"; }
HOST_KEY="$(on_host 'cat /etc/ssh/ssh_host_ed25519_key.pub' | awk '{print $1, $2}')"
printf '%s %s\n' "$HOST_ADDR" "$HOST_KEY" >"$WORK/known_hosts"
kubectl -n "$CREDS" create secret generic hv01-ssh --from-literal=username=faubourg \
  --from-file=privateKey="$WORK/run" --from-file=knownHosts="$WORK/known_hosts" >/dev/null
kubectl -n "$CREDS" create secret generic hv01-ssh-placement --from-literal=username=admin \
  --from-file=privateKey="$WORK/placement" >/dev/null
# The host's install script: reads the bundle the way examples/host/install.sh does, and
# installs the credential where banlieue's provider reads it. No banlieue here.
cat >"$WORK/install.sh" <<'EOF'
#!/bin/sh
set -eu
[ "$(id -u)" -eq 0 ] || exit 1
IFS= read -r header
[ "$header" = "faubourg-provider-credential v1" ] || exit 9
IFS=' ' read -r k provider; [ "$k" = provider ] || exit 9
IFS=' ' read -r k namespace; [ "$k" = namespace ] || exit 9
IFS=' ' read -r k token; [ "$k" = token ] || exit 9
IFS= read -r k; [ "$k" = kubeconfig ] || exit 9
kubeconfig="$(cat)"
install -d -m 0700 /etc/banlieue/credentials
umask 077
printf '%s' "$token" > /etc/banlieue/credentials/token
printf '%s\n' "$kubeconfig" > /etc/banlieue/credentials/kubeconfig
printf '%s %s\n' "$provider" "$namespace" > /etc/banlieue/credentials/provider
echo "install ran for provider $provider"
EOF
kubectl -n "$CREDS" create configmap faubourg-install --from-file=install.sh="$WORK/install.sh" >/dev/null

# The controller's own identity: a kubeconfig holding only its ServiceAccount token.
SERVER="$(kubectl config view --minify --raw -o jsonpath='{.clusters[0].cluster.server}')"
kubectl config view --minify --raw -o jsonpath='{.clusters[0].cluster.certificate-authority-data}' \
  | base64 -d >"$WORK/ca.crt"
CTL_KUBECONFIG="$WORK/controller.kubeconfig"
TOKEN="$(kubectl -n faubourg-system create token faubourg-controller --duration=1h)"
KUBECONFIG="$CTL_KUBECONFIG" kubectl config set-cluster kind --server="$SERVER" \
  --certificate-authority="$WORK/ca.crt" --embed-certs >/dev/null
KUBECONFIG="$CTL_KUBECONFIG" kubectl config set-credentials controller --token="$TOKEN" >/dev/null
KUBECONFIG="$CTL_KUBECONFIG" kubectl config set-context ctl --cluster=kind --user=controller >/dev/null
KUBECONFIG="$CTL_KUBECONFIG" kubectl config use-context ctl >/dev/null
unset TOKEN

failures=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1: $2"; failures=$((failures + 1)); }
eventually() { # description, seconds, command...
  local what=$1 secs=$2; shift 2
  for _ in $(seq 1 "$secs"); do
    if "$@" >/dev/null 2>&1; then pass "$what"; return 0; fi
    sleep 1
  done
  fail "$what" "not within ${secs}s"
}
phase_is() { # phase reason
  local got
  got="$(kubectl get host hv01 -o json 2>/dev/null \
    | jq -r '[.status.phase, (.status.conditions // [] | map(select(.type == "Ready"))[0].reason)] | join(" ")')"
  [ "$got" = "$1 $2" ]
}
label_is() { # key value
  [ "$(kubectl -n "$BANLIEUE" get provider hv01 -o json | jq -r --arg k "$1" '.metadata.labels[$k] // ""')" = "$2" ]
}

echo "e2e-test: starting the controller"
KUBECONFIG="$CTL_KUBECONFIG" FAUBOURG_BOOTSTRAP_IMAGE="$IMAGE" \
  FAUBOURG_DEFAULT_SCRIPT=ConfigMap/faubourg-install \
  FAUBOURG_PROVIDER_API_SERVER=https://kubernetes.default.svc RUST_LOG=info,kube=warn \
  "$BIN" >"$WORK/controller.log" 2>&1 &
CTL_PID=$!

kubectl apply -f - >/dev/null <<EOF
{"apiVersion": "faubourg.firestoned.io/v1alpha1", "kind": "Host", "metadata": {"name": "hv01"},
 "spec": {"zoneRef": "az1", "address": "$HOST_ADDR",
          "ssh": {"port": 22, "secretRef": {"name": "hv01-ssh"},
                  "placementSecretRef": {"name": "hv01-ssh-placement"}},
          "bootstrap": {"method": "Script"}}}
EOF

echo "--- the Provider (ADR-0011)"
eventually "faubourg creates the host's Provider" 60 kubectl -n "$BANLIEUE" get provider hv01
eventually "labelled with its zone" 10 label_is faubourg.firestoned.io/zone az1
eventually "and its host" 10 label_is faubourg.firestoned.io/host hv01
eventually "and the zone's providerLabels" 10 label_is topology.home.example/rack r1
eventually "class and endpoint set" 10 bash -c \
  "[ \"\$(kubectl -n $BANLIEUE get provider hv01 -o jsonpath='{.spec.providerClassRef.name} {.spec.connection.endpoint}')\" = 'cloud-hypervisor ch://$HOST_ADDR' ]"
eventually "no Job until banlieue gives it an identity" 30 phase_is Pending AwaitingProviderIdentity
if kubectl -n "$CREDS" get jobs -o name | grep -q .; then fail "the identity gate" "a Job exists"; else pass "and indeed no Job"; fi

echo "--- banlieue (simulated) gives the Provider its identity"
kubectl -n "$BANLIEUE" create serviceaccount "$SA" >/dev/null
kubectl -n "$BANLIEUE" patch provider hv01 --subresource=status --type=merge \
  -p "{\"status\": {\"workload\": {\"mode\": \"External\", \"namespace\": \"$BANLIEUE\"}}}" >/dev/null

echo "--- the bootstrap Job, with the real image"
eventually "the Job is created and admitted" 60 kubectl -n "$CREDS" get job bootstrap-hv01-g1
eventually "the Job completes" 240 bash -c \
  "kubectl -n $CREDS get job bootstrap-hv01-g1 -o json | jq -e '.status.conditions // [] | any(.type == \"Complete\" and .status == \"True\")'"
JOB_LOG="$(kubectl -n "$CREDS" logs job/bootstrap-hv01-g1 2>&1 || true)"
if grep -qF "install ran for provider hv01" <<<"$JOB_LOG"; then pass "the script ran on the host, for its Provider"
else fail "the run" "$(tail -8 <<<"$JOB_LOG")"; fi
eventually "the Host waits for its provider" 30 phase_is Bootstrapping AwaitingProvider

echo "--- the provider credential on the host"
HOST_TOKEN="$(on_host 'cat /etc/banlieue/credentials/token' 2>/dev/null || true)"
if [ -n "$HOST_TOKEN" ]; then pass "a token was installed"; else fail "the token" "none on the host"; fi
# A kubeconfig with no client certificate, which would otherwise win over the token.
WHO="$(kubectl --kubeconfig=/dev/null --server="$SERVER" --certificate-authority="$WORK/ca.crt" \
  --token="$HOST_TOKEN" auth whoami -o json 2>/dev/null | jq -r '.status.userInfo.username' || true)"
if [ "$WHO" = "system:serviceaccount:$BANLIEUE:$SA" ]; then pass "it authenticates as the Provider's ServiceAccount"
else fail "the token's identity" "got '$WHO'"; fi
KCFG="$(on_host 'cat /etc/banlieue/credentials/kubeconfig')"
if grep -qF "tokenFile: /etc/banlieue/credentials/token" <<<"$KCFG" && ! grep -qF "$HOST_TOKEN" <<<"$KCFG" \
   && grep -qF "server: https://kubernetes.default.svc" <<<"$KCFG"; then
  pass "the kubeconfig reads the token from a file and holds none"
else fail "the kubeconfig" "$KCFG"; fi
if grep -qF "$HOST_TOKEN" <<<"$JOB_LOG" || grep -qF "$HOST_TOKEN" "$WORK/controller.log"; then
  fail "the logs" "carry the provider token"
else pass "neither the Job's nor the controller's log carries the token"; fi
if kubectl -n "$CREDS" get job bootstrap-hv01-g1 -o json | grep -qF "$HOST_TOKEN"; then
  fail "the Job" "carries the token"
else pass "the Job's spec and status carry no token"; fi

echo "--- banlieue (simulated) reports the provider healthy"
GEN="$(kubectl -n "$BANLIEUE" get provider hv01 -o jsonpath='{.metadata.generation}')"
kubectl -n "$BANLIEUE" patch provider hv01 --subresource=status --type=merge -p "{\"status\": {
  \"conditions\": [{\"type\": \"Ready\", \"status\": \"True\", \"reason\": \"Test\", \"message\": \"\",
    \"observedGeneration\": $GEN, \"lastTransitionTime\": \"2026-11-07T00:00:00Z\"}],
  \"workload\": {\"mode\": \"External\", \"namespace\": \"$BANLIEUE\", \"heartbeat\": \"Alive\"}}}" >/dev/null
eventually "the Host is Ready" 30 phase_is Ready ProviderHealthy

echo "--- cordon"
kubectl patch host hv01 --type=merge -p '{"spec": {"cordoned": true}}' >/dev/null
eventually "the Provider is labelled cordoned" 30 label_is faubourg.firestoned.io/cordoned true
kubectl patch host hv01 --type=merge -p '{"spec": {"cordoned": false}}' >/dev/null
eventually "and the label goes with the cordon" 30 label_is faubourg.firestoned.io/cordoned ""
if kubectl -n "$CREDS" get job bootstrap-hv01-g2 >/dev/null 2>&1 || kubectl -n "$CREDS" get job bootstrap-hv01-g3 >/dev/null 2>&1; then
  fail "cordon" "started a new Job"
else pass "cordoning ran nothing on the host"; fi

echo "--- deleting the Host"
kubectl delete host hv01 --wait=true >/dev/null
eventually "its Job is garbage collected" 60 bash -c "! kubectl -n $CREDS get job bootstrap-hv01-g1"
if kubectl -n "$BANLIEUE" get provider hv01 >/dev/null 2>&1; then pass "its Provider is kept (ADR-0011 decision 4)"
else fail "the Provider" "was deleted"; fi

if grep -qF 'not found in local store' "$WORK/controller.log"; then
  fail "the controller's watches" "$(grep -F 'not found in local store' "$WORK/controller.log" | head -2)"
else pass "every watch event maps to a Host"; fi
if grep -i 'forbidden' "$WORK/controller.log" | grep -qv 'ValidatingAdmissionPolicy'; then
  fail "the controller's RBAC" "$(grep -i forbidden "$WORK/controller.log" | head -3)"
else pass "the controller's RBAC was enough"; fi

if [ "$failures" -ne 0 ]; then
  echo "--- controller log"; tail -30 "$WORK/controller.log"
  echo "--- job log"; tail -30 <<<"$JOB_LOG"
  echo "e2e-test: $failures failure(s)"
  exit 1
fi
echo "e2e-test: all passed"
