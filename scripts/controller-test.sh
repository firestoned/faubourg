#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, faubourg
# SPDX-License-Identifier: Apache-2.0
#
# controller-test.sh: the Host controller against a real API server (ADR-0009). Creates a
# throwaway kind cluster, installs the CRDs (faubourg's, and banlieue's Provider from the
# pinned banlieue-api), the RBAC and the admission policy, then runs the controller binary
# outside the cluster with its own ServiceAccount's token, so a missing permission fails here.
#
# What it checks: a Host gets its Job and the admission policy admits it; cordoning runs
# nothing; changed inputs replace the Job; a missing zone holds a Host; an adopted Host is
# Ready from a labelled, healthy Provider and refuses to pick between two; a long Host name
# is refused by the CRD; deleting a Host deletes its Job; the logs never carry an address.
#
# Needs: kind, kubectl, jq, cargo. With rootless podman, set KIND_EXPERIMENTAL_PROVIDER=podman.
#
#   CT_CLUSTER   kind cluster name (default faubourg-ctl); deleted on exit
#   CT_KEEP=1    keep the cluster afterwards
#   CT_IMAGE     a real digest-pinned image for the bootstrap pods
#
# Usage: scripts/controller-test.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLUSTER="${CT_CLUSTER:-faubourg-ctl}"
# pause 3.10: runs forever, so a Job stays Running. Any digest-pinned image works.
IMAGE="${CT_IMAGE:-registry.k8s.io/pause@sha256:7c38f24774e3cbd906d2d33c38354ccf787635581c122965132c9bd309754d4a}"
CREDS=faubourg-credentials
BANLIEUE=banlieue-system
# Fixture addresses (RFC 2606). The controller must never log them.
ADDR1=hv01.home.example
ADDR1B=hv01b.home.example

for tool in kind kubectl jq cargo; do
  command -v "$tool" >/dev/null 2>&1 || { echo "controller-test: $tool not found" >&2; exit 1; }
done

WORK="$(mktemp -d)"
export KUBECONFIG="$WORK/admin.kubeconfig"
CTL_PID=""
cleanup() {
  if [ -n "$CTL_PID" ]; then kill "$CTL_PID" 2>/dev/null || true; wait "$CTL_PID" 2>/dev/null || true; fi
  if [ "${CT_KEEP:-}" != 1 ]; then kind delete cluster --name "$CLUSTER" >/dev/null 2>&1 || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT

echo "controller-test: building"
(cd "$ROOT" && cargo build -q -p faubourg-controller --bin faubourg-controller --example provider-crd)
BIN="$ROOT/target/debug/faubourg-controller"

echo "controller-test: creating kind cluster $CLUSTER"
kind create cluster --name "$CLUSTER" --kubeconfig "$KUBECONFIG" --wait 180s >/dev/null
kubectl version -o json | jq -r '"controller-test: server " + .serverVersion.gitVersion'

kubectl apply -f "$ROOT/deploy/crds/" >/dev/null
"$ROOT/target/debug/examples/provider-crd" | kubectl apply -f - >/dev/null
kubectl wait --for=condition=Established crd --all --timeout=60s >/dev/null
kubectl create namespace "$BANLIEUE" >/dev/null
kubectl apply -f "$ROOT/deploy/rbac/controller.yaml" >/dev/null
kubectl create --dry-run=client -o json -f "$ROOT/deploy/admission/bootstrap-policy-params.yaml" \
  | jq -c --arg img "$IMAGE" '.data.image = $img' | kubectl apply -f - >/dev/null
kubectl apply -f "$ROOT/deploy/admission/bootstrap-policy.yaml" >/dev/null
kubectl -n "$CREDS" create secret generic hv01-ssh --from-literal=username=faubourg >/dev/null
kubectl -n "$CREDS" create configmap faubourg-install --from-literal=install.sh='#!/bin/sh' >/dev/null

# The controller's own identity: a kubeconfig holding only its ServiceAccount token.
SERVER="$(kubectl config view --minify --raw -o jsonpath='{.clusters[0].cluster.server}')"
kubectl config view --minify --raw -o jsonpath='{.clusters[0].cluster.certificate-authority-data}' \
  | base64 -d >"$WORK/ca.crt"
TOKEN="$(kubectl -n faubourg-system create token faubourg-controller --duration=1h)"
CTL_KUBECONFIG="$WORK/controller.kubeconfig"
KUBECONFIG="$CTL_KUBECONFIG" kubectl config set-cluster kind --server="$SERVER" \
  --certificate-authority="$WORK/ca.crt" --embed-certs >/dev/null
KUBECONFIG="$CTL_KUBECONFIG" kubectl config set-credentials controller --token="$TOKEN" >/dev/null
KUBECONFIG="$CTL_KUBECONFIG" kubectl config set-context ctl --cluster=kind --user=controller >/dev/null
KUBECONFIG="$CTL_KUBECONFIG" kubectl config use-context ctl >/dev/null
unset TOKEN

failures=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1: $2"; failures=$((failures + 1)); }

echo "--- configuration fails closed"
if KUBECONFIG="$CTL_KUBECONFIG" FAUBOURG_BOOTSTRAP_IMAGE=registry.k8s.io/pause:3.10 \
  "$BIN" >"$WORK/refused.log" 2>&1; then
  fail "an image by tag" "the controller started"
elif grep -qF "pinned by digest" "$WORK/refused.log"; then
  pass "the controller refuses an image by tag"
else
  fail "an image by tag" "refused for another reason: $(cat "$WORK/refused.log")"
fi

echo "controller-test: starting the controller"
KUBECONFIG="$CTL_KUBECONFIG" FAUBOURG_BOOTSTRAP_IMAGE="$IMAGE" \
  FAUBOURG_DEFAULT_SCRIPT=ConfigMap/faubourg-install RUST_LOG=info,kube=warn \
  "$BIN" >"$WORK/controller.log" 2>&1 &
CTL_PID=$!

# The admission binding takes a moment to take effect; the controller retries a refused Job.

eventually() { # description, seconds, command...
  local what=$1 secs=$2; shift 2
  for _ in $(seq 1 "$secs"); do
    if "$@" >/dev/null 2>&1; then pass "$what"; return 0; fi
    sleep 1
  done
  fail "$what" "not within ${secs}s"
  return 0
}
never() { # description, seconds, command...: the command must stay false for that long
  local what=$1 secs=$2; shift 2
  for _ in $(seq 1 "$secs"); do
    if "$@" >/dev/null 2>&1; then fail "$what" "it happened"; return 0; fi
    sleep 1
  done
  pass "$what"
}
phase_is() { # host, phase[, reason]
  local got
  got="$(kubectl get host "$1" -o json 2>/dev/null \
    | jq -r '[.status.phase, (.status.conditions // [] | map(select(.type == "Ready"))[0].reason)] | join(" ")')"
  [ "${got%% *}" = "$2" ] && { [ -z "${3:-}" ] || [ "${got#* }" = "$3" ]; }
}
job_exists() { kubectl -n "$CREDS" get job "$1" >/dev/null 2>&1; }
jobs_of() { kubectl -n "$CREDS" get jobs -l "faubourg.firestoned.io/host=$1" -o name 2>/dev/null | sort | tr '\n' ' '; }
jobs_are() { [ "$(jobs_of "$1")" = "$2" ]; }

host() { # name, zone, address, method
  local ssh='"ssh": {"port": 22, "secretRef": {"name": "hv01-ssh"}},'
  [ "$4" = None ] && ssh=''
  kubectl apply -f - >/dev/null <<EOF
{"apiVersion": "faubourg.firestoned.io/v1alpha1", "kind": "Host", "metadata": {"name": "$1"},
 "spec": {"zoneRef": "$2", "address": "$3", $ssh "bootstrap": {"method": "$4"}}}
EOF
}

kubectl apply -f - >/dev/null <<'EOF'
{"apiVersion": "faubourg.firestoned.io/v1alpha1", "kind": "AvailabilityZone",
 "metadata": {"name": "az1"}, "spec": {"storage": {"mode": "Local"}}}
EOF

echo "--- a Host gets its Job"
host hv01 az1 "$ADDR1" Script
eventually "the Job is created and admitted" 90 job_exists bootstrap-hv01-g1
eventually "the Host is Bootstrapping" 30 phase_is hv01 Bootstrapping BootstrapJobRunning
eventually "the Job's pod runs the pinned image" 60 bash -c \
  "kubectl -n $CREDS get pods -l batch.kubernetes.io/job-name=bootstrap-hv01-g1 -o json \
   | jq -e '.items[0].spec.containers[0].image == \"$IMAGE\"'"

echo "--- cordoning runs nothing"
kubectl patch host hv01 --type=merge -p '{"spec": {"cordoned": true}}' >/dev/null
never "no new Job after a cordon" 10 job_exists bootstrap-hv01-g2
eventually "the Host still has only its first Job" 5 jobs_are hv01 "job.batch/bootstrap-hv01-g1 "

echo "--- changed inputs replace the Job"
kubectl patch host hv01 --type=merge -p "{\"spec\": {\"address\": \"$ADDR1B\"}}" >/dev/null
eventually "the stale Job goes and a new one comes" 60 jobs_are hv01 "job.batch/bootstrap-hv01-g3 "

echo "--- the zone gates the bootstrap"
host hv02 az9 hv02.home.example Script
eventually "a Host in a missing zone is Pending" 30 phase_is hv02 Pending ZoneNotFound
never "and gets no Job" 5 job_exists bootstrap-hv02-g1

echo "--- an adopted Host follows its labelled Provider"
host hv03 az1 hv03.home.example None
eventually "an adopted Host waits for its Provider" 30 phase_is hv03 Pending AwaitingProvider
provider() { # name
  kubectl apply -f - >/dev/null <<EOF
{"apiVersion": "banlieue.io/v1alpha1", "kind": "Provider",
 "metadata": {"name": "$1", "namespace": "$BANLIEUE", "labels": {"faubourg.firestoned.io/host": "hv03"}},
 "spec": {"providerClassRef": {"name": "cloud-hypervisor"}, "connection": {"endpoint": "hv03.home.example"}}}
EOF
  local gen
  gen="$(kubectl -n "$BANLIEUE" get provider "$1" -o jsonpath='{.metadata.generation}')"
  kubectl -n "$BANLIEUE" patch provider "$1" --subresource=status --type=merge -p "{\"status\": {
    \"conditions\": [{\"type\": \"Ready\", \"status\": \"True\", \"reason\": \"Test\", \"message\": \"\",
      \"observedGeneration\": $gen, \"lastTransitionTime\": \"2026-11-07T00:00:00Z\"}],
    \"workload\": {\"namespace\": \"$BANLIEUE\", \"heartbeat\": \"Alive\"}}}" >/dev/null
}
provider hv03
eventually "the Host is Ready from a healthy Provider" 30 phase_is hv03 Ready ProviderHealthy
eventually "status names the Provider" 5 bash -c \
  "[ \"\$(kubectl get host hv03 -o jsonpath='{.status.providerRef}')\" = hv03 ]"
provider hv03-copy
eventually "two labelled Providers: Degraded, none picked" 30 phase_is hv03 Degraded ProviderAmbiguous

echo "--- the CRD refuses a long Host name"
LONG="h$(printf 'a%.0s' $(seq 1 63))"
if out="$(host "$LONG" az1 hv04.home.example None 2>&1)"; then
  fail "a 64-character Host name" "admitted"
elif grep -qF "at most 63 characters" <<<"$out"; then
  pass "a 64-character Host name is refused"
else
  fail "a 64-character Host name" "refused for another reason: $out"
fi

echo "--- deleting a Host deletes its Job"
kubectl delete host hv01 --wait=true >/dev/null
eventually "the Job is garbage collected" 60 bash -c "! kubectl -n $CREDS get job bootstrap-hv01-g3"

echo "--- logs"
if kill -0 "$CTL_PID" 2>/dev/null; then pass "the controller is still running"
else fail "the controller" "exited: $(tail -5 "$WORK/controller.log")"; fi
if grep -qE "$ADDR1|$ADDR1B|hv02\.home|hv03\.home" "$WORK/controller.log"; then
  fail "the logs" "carry a Host address"
else
  pass "the logs carry no Host address"
fi
# An admission denial also reads "forbidden"; only an RBAC refusal counts here.
if grep -i 'forbidden' "$WORK/controller.log" | grep -qv 'ValidatingAdmissionPolicy'; then
  fail "RBAC" "$(grep -i 'forbidden' "$WORK/controller.log" | grep -v ValidatingAdmissionPolicy | head -3)"
else
  pass "the controller's RBAC was enough"
fi

if [ "$failures" -ne 0 ]; then
  echo "--- controller log"
  tail -40 "$WORK/controller.log"
  echo "controller-test: $failures failure(s)"
  exit 1
fi
echo "controller-test: all passed"
