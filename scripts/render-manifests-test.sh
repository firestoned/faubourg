#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, faubourg
# SPDX-License-Identifier: Apache-2.0
#
# render-manifests-test.sh: scripts/render-manifests.sh renders both digests where they belong
# and refuses everything else (ADR-0012 decision 5).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
A="$(printf 'a%.0s' $(seq 64))"
B="$(printf 'b%.0s' $(seq 64))"
CI="ghcr.io/firestoned/faubourg-controller@sha256:$A"
BI="ghcr.io/firestoned/faubourg-bootstrap@sha256:$B"

failures=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1: $2"; failures=$((failures + 1)); }
render() { "$ROOT/scripts/render-manifests.sh" >/dev/null 2>"$WORK/err"; }
refused() { # name, env assignments...
  local name=$1; shift
  if env "$@" OUT="$WORK/out.yaml" "$ROOT/scripts/render-manifests.sh" >/dev/null 2>&1; then
    fail "$name" "rendered"
  else pass "$name"; fi
}

if CONTROLLER_IMAGE="$CI" BOOTSTRAP_IMAGE="$BI" PROVIDER_API_SERVER=https://kube.home.example:6443 \
   OUT="$WORK/r.yaml" render; then pass "renders"; else fail "renders" "$(cat "$WORK/err")"; fi
body="$(grep -v '^#' "$WORK/r.yaml")"
[ "$(grep -cF "$BI" <<<"$body")" -eq 2 ] && pass "the bootstrap digest is in the policy and the controller" \
  || fail "the bootstrap digest" "not exactly twice"
grep -qF "image: $CI" <<<"$body" && pass "the controller runs its digest" || fail "the controller image" "missing"
grep -qF 'image: "'"$BI"'"' <<<"$body" && pass "the policy parameter is the bootstrap digest" \
  || fail "the policy parameter" "missing"
grep -qF 'value: "https://kube.home.example:6443"' <<<"$body" && pass "the API server is rendered" \
  || fail "the API server" "missing"
if grep -qF '@sha256:<digest>' "$WORK/r.yaml"; then fail "placeholders" "left"; else pass "no placeholder is left"; fi
for f in "$ROOT"/deploy/crds/*.yaml "$ROOT"/deploy/rbac/*.yaml "$ROOT"/deploy/admission/*.yaml \
         "$ROOT"/deploy/controller/*.yaml; do
  name="$(awk '/^  name:/ {print $2; exit}' "$f")"
  grep -qF "name: $name" "$WORK/r.yaml" || fail "$(basename "$f")" "not in the install file"
done
pass "every manifest in deploy/ is in the install file"

CONTROLLER_IMAGE="$CI" BOOTSTRAP_IMAGE="$BI" OUT="$WORK/n.yaml" render
grep -qF 'value: "" # rendered from PROVIDER_API_SERVER' "$WORK/n.yaml" \
  && pass "without PROVIDER_API_SERVER the controller is left to refuse to start" \
  || fail "the empty API server" "changed"

refused "a controller image by tag" CONTROLLER_IMAGE=ghcr.io/firestoned/faubourg-controller:v0.1.0 BOOTSTRAP_IMAGE="$BI"
refused "a bootstrap image by tag" CONTROLLER_IMAGE="$CI" BOOTSTRAP_IMAGE=ghcr.io/firestoned/faubourg-bootstrap:latest
refused "a short digest" CONTROLLER_IMAGE="$CI" BOOTSTRAP_IMAGE="ghcr.io/x@sha256:abc"
refused "an uppercase digest" CONTROLLER_IMAGE="$CI" BOOTSTRAP_IMAGE="ghcr.io/x@sha256:$(printf 'A%.0s' $(seq 64))"
refused "a missing image" CONTROLLER_IMAGE="$CI"
refused "an image with a sed delimiter" CONTROLLER_IMAGE="$CI" BOOTSTRAP_IMAGE="ghcr.io/x|y@sha256:$B"
refused "an http API server" CONTROLLER_IMAGE="$CI" BOOTSTRAP_IMAGE="$BI" PROVIDER_API_SERVER=http://kube.home.example
refused "an API server with a delimiter" CONTROLLER_IMAGE="$CI" BOOTSTRAP_IMAGE="$BI" PROVIDER_API_SERVER='https://a|b'

if [ "$failures" -ne 0 ]; then echo "render-manifests-test: $failures failure(s)"; exit 1; fi
echo "render-manifests-test: all passed"
