#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, faubourg
# SPDX-License-Identifier: Apache-2.0
#
# kind-cluster.sh: one kind cluster shared by vap-test, controller-test and e2e-test, run one
# after another (`make integration`). Each test, given FAUBOURG_KIND_CLUSTER, uses that
# cluster instead of creating its own, and resets it on exit instead of deleting it. They
# cannot share it at the same time: all three write the same cluster-wide objects (the policy
# parameters, the admission policies, Hosts named hv01).
#
#   up <name>      create it, configured for e2e-test's local registry
#   reset <name>   remove everything a test installs: faubourg's and banlieue's CRDs (and so
#                  every Host, zone and Provider), the admission policies, the controller's
#                  cluster role, and the namespaces the tests create
#   down <name>    delete it
#
# Needs: kind, kubectl. With rootless podman, set KIND_EXPERIMENTAL_PROVIDER=podman.
set -euo pipefail

usage() { echo "usage: $0 up|reset|down <cluster>" >&2; exit 2; }
[ "$#" -eq 2 ] || usage
NAME=$2

case "$1" in
  up)
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    # The registry mirror config e2e-test writes into each node needs this.
    cat >"$tmp/kind.yaml" <<'EOF'
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
containerdConfigPatches:
  - |-
    [plugins."io.containerd.grpc.v1.cri".registry]
      config_path = "/etc/containerd/certs.d"
EOF
    # kind's progress goes to stderr; show it only if the cluster does not come up.
    kind create cluster --name "$NAME" --kubeconfig "$tmp/kubeconfig" --config "$tmp/kind.yaml" \
      --wait 180s >"$tmp/kind.log" 2>&1 || { cat "$tmp/kind.log" >&2; exit 1; }
    echo "kind-cluster: $NAME is up"
    ;;
  reset)
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    kind get kubeconfig --name "$NAME" >"$tmp/kubeconfig"
    k() { kubectl --kubeconfig "$tmp/kubeconfig" "$@"; }
    for p in faubourg-bootstrap-jobs faubourg-bootstrap-pods faubourg-provider-tokens; do
      k delete validatingadmissionpolicybinding "$p" --ignore-not-found >/dev/null
      k delete validatingadmissionpolicy "$p" --ignore-not-found >/dev/null
    done
    k delete clusterrolebinding faubourg-controller --ignore-not-found >/dev/null
    k delete clusterrole faubourg-controller --ignore-not-found >/dev/null
    # Deleting a CRD deletes its objects: Hosts (and so their Jobs), zones, Providers.
    crds="$(k get crd -o name | grep -E '\.(faubourg\.firestoned\.io|banlieue\.io)$' || true)"
    # shellcheck disable=SC2086 # one argument per CRD
    [ -z "$crds" ] || k delete $crds --wait=true --timeout=180s >/dev/null
    k delete namespace faubourg-system faubourg-credentials banlieue-system hosts \
      --ignore-not-found --wait=true --timeout=180s >/dev/null
    echo "kind-cluster: $NAME is reset"
    ;;
  down)
    kind delete cluster --name "$NAME" >/dev/null 2>&1 || true
    echo "kind-cluster: $NAME is down"
    ;;
  *) usage ;;
esac
