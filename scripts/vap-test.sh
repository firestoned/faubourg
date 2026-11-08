#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, faubourg
# SPDX-License-Identifier: Apache-2.0
#
# vap-test.sh: the bootstrap admission policy and the controller's RBAC against a real API
# server (ADR-0008). A model of CEL that admits more than the API server would hide a hole,
# so this creates a throwaway kind cluster, applies deploy/rbac and deploy/admission, and
# submits the Job the controller's builder really makes (the bootstrap-job example), then
# one mutation of it per rule. Every mutation must be refused with that rule's message.
#
# Needs: kind, kubectl, jq, cargo. With rootless podman, set KIND_EXPERIMENTAL_PROVIDER=podman.
#
#   VAP_CLUSTER   kind cluster name (default faubourg-vap); deleted on exit
#   VAP_KEEP=1    keep the cluster afterwards
#   VAP_IMAGE     a real digest-pinned image, used for the one pod that is really created
#   FAUBOURG_KIND_CLUSTER  use this existing kind cluster instead (`make integration`): it is
#                          reset first and left in place
#
# Usage: scripts/vap-test.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SHARED="${FAUBOURG_KIND_CLUSTER:-}"
CLUSTER="${SHARED:-${VAP_CLUSTER:-faubourg-vap}}"
# pause 3.10: tiny, runs as non-root, needs no command. Any digest-pinned image works.
IMAGE="${VAP_IMAGE:-registry.k8s.io/pause@sha256:7c38f24774e3cbd906d2d33c38354ccf787635581c122965132c9bd309754d4a}"
CREDS=faubourg-credentials
CTL=system:serviceaccount:faubourg-system:faubourg-controller

for tool in kind kubectl jq cargo; do
  command -v "$tool" >/dev/null 2>&1 || { echo "vap-test: $tool not found" >&2; exit 1; }
done

WORK="$(mktemp -d)"
export KUBECONFIG="$WORK/kubeconfig"
cleanup() {
  if [ -z "$SHARED" ] && [ "${VAP_KEEP:-}" != 1 ]; then
    kind delete cluster --name "$CLUSTER" >/dev/null 2>&1 || true
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

if [ -n "$SHARED" ]; then
  echo "vap-test: using kind cluster $CLUSTER"
  "$ROOT/scripts/kind-cluster.sh" reset "$CLUSTER" >/dev/null
  kind get kubeconfig --name "$CLUSTER" >"$KUBECONFIG"
else
  echo "vap-test: creating kind cluster $CLUSTER"
  kind create cluster --name "$CLUSTER" --kubeconfig "$KUBECONFIG" --wait 180s >/dev/null
fi
kubectl version -o json | jq -r '"vap-test: server " + .serverVersion.gitVersion'

# The controller reads Providers in banlieue's namespace, which banlieue would create, with
# its Provider CRD (printed from the pinned banlieue-api).
kubectl create namespace banlieue-system >/dev/null
(cd "$ROOT" && cargo run -q -p faubourg-controller --example provider-crd) | kubectl apply -f - >/dev/null 2>&1
kubectl wait --for=condition=Established crd/providers.banlieue.io --timeout=60s >/dev/null
kubectl apply -f "$ROOT/deploy/rbac/controller.yaml" >/dev/null
# The script the fixture ConfigMap holds, pinned as an administrator would (ADR-0013).
SCRIPT_SHA="$(printf '#!/bin/sh' | sha256sum | cut -d' ' -f1)"
SOURCES="ConfigMap/faubourg-install/install.sh@sha256:$SCRIPT_SHA"
jq_image() { jq -c --arg img "$IMAGE" --arg src "$SOURCES" '.data.image = $img | .data.scriptSources = $src'; }
kubectl create --dry-run=client -o json -f "$ROOT/deploy/admission/bootstrap-policy-params.yaml" \
  | jq_image | kubectl apply -f - >/dev/null
kubectl apply -f "$ROOT/deploy/admission/bootstrap-policy.yaml" >/dev/null
kubectl -n "$CREDS" create secret generic hv01-ssh --from-literal=username=faubourg >/dev/null
kubectl -n "$CREDS" create secret generic hv01-ssh-placement --from-literal=username=root >/dev/null
kubectl -n "$CREDS" create secret generic hv02-ssh --from-literal=username=faubourg >/dev/null
kubectl -n "$CREDS" create configmap faubourg-install --from-literal=install.sh='#!/bin/sh' >/dev/null
kubectl -n "$CREDS" create configmap other-script --from-literal=install.sh='#!/bin/sh' >/dev/null

GOOD="$(cd "$ROOT" && FAUBOURG_BOOTSTRAP_IMAGE="$IMAGE" FAUBOURG_SCRIPT_SOURCES="$SOURCES" cargo run -q -p faubourg-controller --example bootstrap-job | jq -c .)"
# The fixture Host does not exist here; without its owner the garbage collector would delete
# the Job that is really created.
GOOD="$(jq -c 'del(.metadata.ownerReferences)' <<<"$GOOD")"

failures=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1: $2"; failures=$((failures + 1)); }

submit() { # json, extra kubectl args...
  local json=$1; shift
  kubectl create --dry-run=server -o name "$@" -f - <<<"$json" 2>&1
}
admit() { # name, json, extra args...
  local name=$1 json=$2 out; shift 2
  if out="$(submit "$json" "$@")"; then pass "$name"; else fail "$name" "refused: $out"; fi
}
deny() { # name, expected message fragment, json, extra args...
  local name=$1 want=$2 json=$3 out; shift 3
  if out="$(submit "$json" "$@")"; then
    fail "$name" "admitted"
  elif grep -qF -- "$want" <<<"$out"; then
    pass "$name"
  else
    fail "$name" "refused for another reason: $out"
  fi
}
mut() { jq -c "$1" <<<"$GOOD"; }

# The binding takes a moment to take effect; wait until a known-bad Job is refused.
for _ in $(seq 1 60); do
  if ! submit "$(mut '.spec.template.spec.containers[0].command = ["sh"]')" >/dev/null 2>&1; then break; fi
  sleep 1
done

C='.spec.template.spec.containers[0]'
P='.spec.template.spec'
AS=(--as="$CTL")

echo "--- the builder's Job"
admit "the controller's own Job is admitted" "$GOOD" "${AS[@]}"

echo "--- image and command"
deny "an image by tag" "digest-pinned bootstrap image" "$(mut "$C.image = \"registry.k8s.io/pause:3.10\"")" "${AS[@]}"
deny "another digest-pinned image" "digest-pinned bootstrap image" \
  "$(mut "$C.image = \"registry.k8s.io/pause@sha256:$(printf '1%.0s' $(seq 64))\"")" "${AS[@]}"
deny "a command that prints the key" "no command and no arguments" \
  "$(mut "$C.command = [\"cat\", \"/var/run/faubourg/credentials/privateKey\"]")" "${AS[@]}"
deny "arguments" "no command and no arguments" "$(mut "$C.args = [\"--debug\"]")" "${AS[@]}"
deny "a second container" "exactly one container" \
  "$(mut "$P.containers += [$C | .name = \"sidecar\"]")" "${AS[@]}"
deny "an init container" "exactly one container" \
  "$(mut "$P.initContainers = [$C | .name = \"init\"]")" "${AS[@]}"
deny "a renamed container" "exactly one container" \
  "$(mut "$C.name = \"other\" | .spec.podFailurePolicy.rules[0].onExitCodes.containerName = \"other\"")" "${AS[@]}"

echo "--- environment"
deny "envFrom a Secret" "literal values only" \
  "$(mut "$C.envFrom = [{\"secretRef\": {\"name\": \"hv02-ssh\"}}]")" "${AS[@]}"
deny "env valueFrom a Secret" "literal values only" \
  "$(mut "$C.env += [{\"name\": \"K\", \"valueFrom\": {\"secretKeyRef\": {\"name\": \"hv02-ssh\", \"key\": \"username\"}}}]")" "${AS[@]}"

echo "--- volumes"
deny "an extra Secret volume" "exactly the credentials" \
  "$(mut "$P.volumes += [{\"name\": \"loot\", \"secret\": {\"secretName\": \"hv02-ssh\"}}]")" "${AS[@]}"
deny "a hostPath volume" "exactly the credentials" \
  "$(mut "$P.volumes += [{\"name\": \"host\", \"hostPath\": {\"path\": \"/\"}}]")" "${AS[@]}"
deny "credentials from a ConfigMap" "are Secrets" \
  "$(mut "($P.volumes[] | select(.name == \"credentials\")) |= {name, configMap: {name: \"other-script\"}}")" "${AS[@]}"
deny "a script not on the allowlist" "pinned in scriptSources" \
  "$(mut "($P.volumes[] | select(.name == \"script\") | .configMap.name) = \"other-script\"")" "${AS[@]}"
deny "another host's key as the script" "pinned in scriptSources" \
  "$(mut "($P.volumes[] | select(.name == \"script\")) |= {name, secret: {secretName: \"hv02-ssh\"}}")" "${AS[@]}"
echo "--- the pinned script (ADR-0013)"
SH='(.spec.template.spec.containers[0].env[] | select(.name == "FAUBOURG_SCRIPT_SHA256"))'
SV='(.spec.template.spec.volumes[] | select(.name == "script"))'
deny "a digest the policy does not pin" "pinned in scriptSources" \
  "$(mut "$SH.value = \"$(printf '0%.0s' $(seq 64))\"")" "${AS[@]}"
deny "no pinned digest in the environment" "pinned in scriptSources" \
  "$(mut "$C.env |= map(select(.name != \"FAUBOURG_SCRIPT_SHA256\"))")" "${AS[@]}"
deny "a second digest after the pinned one (the container sees the last)" "pinned in scriptSources" \
  "$(mut "$C.env += [{\"name\": \"FAUBOURG_SCRIPT_SHA256\", \"value\": \"$(printf '0%.0s' $(seq 64))\"}]")" "${AS[@]}"
deny "another key of the pinned source" "pinned in scriptSources" \
  "$(mut "$SV.configMap.items[0].key = \"other.sh\"")" "${AS[@]}"
deny "two keys of the pinned source" "pinned in scriptSources" \
  "$(mut "$SV.configMap.items += [{\"key\": \"other.sh\", \"path\": \"other.sh\"}]")" "${AS[@]}"
deny "the whole source, no key named" "pinned in scriptSources" \
  "$(mut "$SV.configMap |= del(.items)")" "${AS[@]}"
params_sources() { # scriptSources value: apply it, then wait until the API server uses it
  kubectl -n faubourg-system patch configmap faubourg-bootstrap-policy --type=merge \
    -p "{\"data\": {\"scriptSources\": \"$1\"}}" >/dev/null
}
params_sources "ConfigMap/faubourg-install"
for _ in $(seq 1 30); do
  if ! submit "$GOOD" "${AS[@]}" >/dev/null 2>&1; then break; fi
  sleep 1
done
deny "a policy with an unpinned entry refuses every Job" "every scriptSources entry is pinned" "$GOOD" "${AS[@]}"
params_sources "$SOURCES"
for _ in $(seq 1 30); do
  if submit "$GOOD" "${AS[@]}" >/dev/null 2>&1; then break; fi
  sleep 1
done
admit "and the pinned entry admits it again" "$GOOD" "${AS[@]}"
deny "a writable mount" "mounted read-only" "$(mut "$C.volumeMounts[0].readOnly = false")" "${AS[@]}"

echo "--- the kube-api volume (ADR-0011)"
K='(.spec.template.spec.volumes[] | select(.name == "kube-api"))'
deny "a long-lived kube-api token" "kube-api volume is a token" \
  "$(mut "$K.projected.sources[0].serviceAccountToken.expirationSeconds = 86400")" "${AS[@]}"
deny "a kube-api token for another audience" "kube-api volume is a token" \
  "$(mut "$K.projected.sources[0].serviceAccountToken.audience = \"vault\"")" "${AS[@]}"
deny "a Secret smuggled into the kube-api projection" "kube-api volume is a token" \
  "$(mut "$K.projected.sources += [{\"secret\": {\"name\": \"hv02-ssh\"}}]")" "${AS[@]}"
deny "another ConfigMap as the cluster CA" "kube-api volume is a token" \
  "$(mut "$K.projected.sources[1].configMap.name = \"other-script\"")" "${AS[@]}"
deny "a Secret named kube-api" "kube-api volume is a token" \
  "$(mut "$K |= {name, secret: {secretName: \"hv02-ssh\"}}")" "${AS[@]}"

echo "--- identity and isolation"
deny "the default ServiceAccount" "bootstrap ServiceAccount" \
  "$(mut "$P.serviceAccountName = \"default\"")" "${AS[@]}"
deny "a mounted token" "bootstrap ServiceAccount" \
  "$(mut "$P.automountServiceAccountToken = true")" "${AS[@]}"
deny "hostNetwork" "no host namespace" "$(mut "$P.hostNetwork = true")" "${AS[@]}"
deny "hostPID" "no host namespace" "$(mut "$P.hostPID = true")" "${AS[@]}"
deny "shareProcessNamespace" "no host namespace" "$(mut "$P.shareProcessNamespace = true")" "${AS[@]}"
deny "privileged" "runs restricted" \
  "$(mut "$C.securityContext.privileged = true | del($C.securityContext.allowPrivilegeEscalation)")" "${AS[@]}"
deny "privilege escalation" "runs restricted" \
  "$(mut "$C.securityContext.allowPrivilegeEscalation = true")" "${AS[@]}"
deny "a writable root" "runs restricted" \
  "$(mut "$C.securityContext.readOnlyRootFilesystem = false")" "${AS[@]}"
deny "an added capability" "runs restricted" \
  "$(mut "$C.securityContext.capabilities.add = [\"NET_ADMIN\"]")" "${AS[@]}"
deny "capabilities kept" "runs restricted" "$(mut "del($C.securityContext.capabilities)")" "${AS[@]}"
deny "restartPolicy OnFailure" "never restarted in place" \
  "$(mut "$P.restartPolicy = \"OnFailure\" | del(.spec.podFailurePolicy)")" "${AS[@]}"

echo "--- who asks does not matter"
deny "cluster-admin's Job with a command" "no command and no arguments" \
  "$(mut "$C.command = [\"cat\", \"/var/run/faubourg/credentials/privateKey\"]")"
POD="$(jq -c --arg img "$IMAGE" '{apiVersion: "v1", kind: "Pod",
  metadata: {name: "direct", namespace: "faubourg-credentials"},
  spec: {containers: [{name: "c", image: $img}],
         volumes: [{name: "k", secret: {secretName: "hv02-ssh"}}]}}' <<<'{}')"
deny "cluster-admin's direct Pod" "only the Job controller" "$POD"
deny "a direct Pod claiming a Job owner" "only the Job controller" \
  "$(jq -c '.metadata.ownerReferences = [{apiVersion: "batch/v1", kind: "Job", name: "x",
     uid: "8f6b8a52-0000-4000-8000-000000000002", controller: true}]' <<<"$POD")"
admit "the policy is scoped to the credentials namespace" \
  "$(jq -c '.metadata.namespace = "default" | .spec.volumes = []' <<<"$POD")"

echo "--- the controller's RBAC"
can() { kubectl auth can-i "$@" --as="$CTL" 2>/dev/null || true; }
expect_can() { # want, description, can-i args...
  local want=$1 what=$2 got; shift 2
  got="$(can "$@")"
  if [ "$got" = "$want" ]; then pass "$what: $want"; else fail "$what" "can-i says $got, want $want"; fi
}
expect_can yes "create Jobs in the credentials namespace" create jobs.batch -n "$CREDS"
expect_can no "read a Secret" get secrets -n "$CREDS"
expect_can no "list Secrets" list secrets -n "$CREDS"
expect_can no "watch Secrets" watch secrets -n "$CREDS"
expect_can no "create a Pod" create pods -n "$CREDS"
expect_can no "exec into a Pod" create pods/exec -n "$CREDS"
expect_can no "add an ephemeral container" patch pods/ephemeralcontainers -n "$CREDS"
expect_can no "read the script ConfigMap" get configmaps -n "$CREDS"
expect_can yes "read the policy parameters (ADR-0013)" get configmaps/faubourg-bootstrap-policy -n faubourg-system
expect_can yes "watch the policy parameters" watch configmaps/faubourg-bootstrap-policy -n faubourg-system
expect_can no "read another ConfigMap there" get configmaps/other -n faubourg-system
expect_can no "list every ConfigMap there" list configmaps -n faubourg-system
expect_can yes "renew the leader Lease (ADR-0014)" update leases.coordination.k8s.io/faubourg-controller -n faubourg-system
expect_can yes "read the leader Lease" get leases.coordination.k8s.io/faubourg-controller -n faubourg-system
expect_can yes "create the leader Lease" create leases.coordination.k8s.io -n faubourg-system
expect_can no "write another Lease" update leases.coordination.k8s.io/kube-scheduler -n faubourg-system
expect_can no "delete the leader Lease" delete leases.coordination.k8s.io/faubourg-controller -n faubourg-system
expect_can no "create a Lease elsewhere" create leases.coordination.k8s.io -n kube-system
expect_can no "take another controller's Lease" update leases.coordination.k8s.io/kube-controller-manager -n kube-system
expect_can no "change the policy parameters" update configmaps/faubourg-bootstrap-policy -n faubourg-system
expect_can no "change the policy" patch validatingadmissionpolicies.admissionregistration.k8s.io
expect_can no "change the binding" delete validatingadmissionpolicybindings.admissionregistration.k8s.io
expect_can yes "watch Providers in banlieue's namespace" watch providers.banlieue.io -n banlieue-system
expect_can yes "label a Provider (ADR-0011)" patch providers.banlieue.io -n banlieue-system
expect_can no "delete a Provider" delete providers.banlieue.io -n banlieue-system
expect_can no "read a Secret in banlieue's namespace" get secrets -n banlieue-system

echo "--- provider tokens (ADR-0011)"
BOOT=system:serviceaccount:faubourg-credentials:faubourg-bootstrap
kubectl -n banlieue-system create serviceaccount banlieue-provider-cloud-hypervisor-hv01 >/dev/null
kubectl -n banlieue-system create serviceaccount banlieue-operator >/dev/null
kubectl -n banlieue-system create serviceaccount banlieue-provider-libvirt-kvm01 >/dev/null
token() { kubectl -n banlieue-system create token "$@" --as="$BOOT" 2>&1; }
token_ok() { # name, args...
  local name=$1 out; shift
  if out="$(token "$@")" && [ -n "$out" ]; then pass "$name"; else fail "$name" "refused: $out"; fi
}
token_deny() { # name, expected message fragment, args...
  local name=$1 want=$2 out; shift 2
  if out="$(token "$@")"; then fail "$name" "admitted"
  elif grep -qF -- "$want" <<<"$out"; then pass "$name"
  else fail "$name" "refused for another reason: $out"; fi
}
for _ in $(seq 1 60); do
  if ! token banlieue-operator --duration=1h >/dev/null 2>&1; then break; fi
  sleep 1
done
token_ok "a day's token for a provider ServiceAccount" \
  banlieue-provider-cloud-hypervisor-hv01 --duration=24h
token_deny "a token for banlieue's operator" "only for a provider ServiceAccount" \
  banlieue-operator --duration=1h
token_deny "a token for a provider of another class" "only for a provider ServiceAccount" \
  banlieue-provider-libvirt-kvm01 --duration=1h
token_deny "a token for longer than a day" "at most a day" \
  banlieue-provider-cloud-hypervisor-hv01 --duration=48h
token_deny "a token for another audience" "own audience" \
  banlieue-provider-cloud-hypervisor-hv01 --duration=1h --audience=vault
token_deny "a token bound to an object" "bound to no object" \
  banlieue-provider-cloud-hypervisor-hv01 --duration=1h --bound-object-kind=Secret --bound-object-name=x
if out="$(kubectl -n banlieue-system create token banlieue-operator --duration=1h 2>&1)"; then
  pass "the policy binds only the bootstrap pods (cluster-admin mints as before)"
else fail "the token policy's scope" "cluster-admin refused: $out"; fi
expect_boot() { # want, description, can-i args...
  local want=$1 what=$2 got; shift 2
  got="$(kubectl auth can-i "$@" --as="$BOOT" 2>/dev/null || true)"
  if [ "$got" = "$want" ]; then pass "$what: $want"; else fail "$what" "can-i says $got, want $want"; fi
}
expect_boot yes "the bootstrap pods mint tokens in banlieue's namespace" create serviceaccounts --subresource=token -n banlieue-system
expect_boot no "the bootstrap pods read a Secret there" get secrets -n banlieue-system
expect_boot no "the bootstrap pods read their own credentials" get secrets -n "$CREDS"
expect_boot no "the bootstrap pods mint tokens elsewhere" create serviceaccounts --subresource=token -n "$CREDS"
expect_can no "the controller mints a token" create serviceaccounts --subresource=token -n banlieue-system
expect_can yes "the controller creates Providers" create providers.banlieue.io -n banlieue-system

echo "--- a real Job and its pod"
if out="$(kubectl create -f - "${AS[@]}" <<<"$GOOD" 2>&1)"; then
  pass "the controller creates its Job"
  pod=""
  for _ in $(seq 1 60); do
    pod="$(kubectl -n "$CREDS" get pods -l job-name=bootstrap-hv01-g1 -o name 2>/dev/null | head -1)"
    [ -n "$pod" ] && break
    sleep 1
  done
  if [ -n "$pod" ]; then
    pass "the Job controller's pod is admitted ($pod)"
    if out="$(kubectl -n "$CREDS" debug "$pod" --image=busybox --target=bootstrap -- true 2>&1)"; then
      fail "an ephemeral container" "admitted"
    elif grep -qF "no ephemeral containers" <<<"$out"; then pass "an ephemeral container is refused"
    else fail "an ephemeral container" "refused for another reason: $out"; fi
    if out="$(kubectl -n "$CREDS" set image "$pod" bootstrap=busybox 2>&1)"; then
      fail "kubectl set image on the pod" "admitted"
    elif grep -qF "spec does not change" <<<"$out"; then pass "kubectl set image on the pod is refused"
    else fail "kubectl set image on the pod" "refused for another reason: $out"; fi
  else
    fail "the Job controller's pod" "no pod after 60s: $(kubectl -n "$CREDS" describe job bootstrap-hv01-g1 | tail -5)"
  fi
else
  fail "the controller creates its Job" "$out"
fi

echo "--- no parameters, no Jobs"
kubectl -n faubourg-system delete configmap faubourg-bootstrap-policy >/dev/null
deny_any() { # name, json, args...: refused for any reason that names a parameter
  local name=$1 json=$2 out; shift 2
  if out="$(submit "$json" "$@")"; then fail "$name" "admitted"
  elif grep -qiE "param|faubourg-bootstrap-policy" <<<"$out"; then pass "$name"
  else fail "$name" "refused for another reason: $out"; fi
}
for _ in $(seq 1 30); do
  submit "$GOOD" "${AS[@]}" >/dev/null 2>&1 || break
  sleep 1
done
deny_any "the controller's own Job without parameters" "$GOOD" "${AS[@]}"

if [ "$failures" -gt 0 ]; then
  echo "vap-test: $failures failure(s)"
  exit 1
fi
echo "vap-test: all passed"
