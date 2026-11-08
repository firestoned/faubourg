#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, faubourg
# SPDX-License-Identifier: Apache-2.0
#
# render-manifests.sh: one install file from deploy/, with the image digests written in
# (ADR-0012 decision 5). The bootstrap digest goes into the admission policy's parameters and
# into the controller's FAUBOURG_BOOTSTRAP_IMAGE in the same pass, so they cannot disagree.
#
#   CONTROLLER_IMAGE      <repository>@sha256:<64 hex>   (required)
#   BOOTSTRAP_IMAGE       <repository>@sha256:<64 hex>   (required)
#   OUT                   output file                    (required)
#   PROVIDER_API_SERVER   https://... for the controller (optional; left empty otherwise, and
#                         the controller refuses to start until the operator sets it)
#
# Usage: CONTROLLER_IMAGE=... BOOTSTRAP_IMAGE=... OUT=... scripts/render-manifests.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
: "${CONTROLLER_IMAGE:?set CONTROLLER_IMAGE to <repository>@sha256:<64 hex>}"
: "${BOOTSTRAP_IMAGE:?set BOOTSTRAP_IMAGE to <repository>@sha256:<64 hex>}"
: "${OUT:?set OUT to the output file}"
API="${PROVIDER_API_SERVER:-}"

pinned() { [[ "$1" =~ ^[a-z0-9][a-z0-9./:_-]*@sha256:[0-9a-f]{64}$ ]]; }
for v in CONTROLLER_IMAGE BOOTSTRAP_IMAGE; do
  pinned "${!v}" || { echo "render-manifests: $v is not <repository>@sha256:<64 hex>" >&2; exit 1; }
done
url_re='^https://[][A-Za-z0-9.:/_-]+$'
if [ -n "$API" ] && ! [[ "$API" =~ $url_re ]]; then
  echo "render-manifests: PROVIDER_API_SERVER must be an https:// URL" >&2; exit 1
fi

# Every input is validated above, so none holds sed's `|` or `&`.
render() { # file: substitutes the placeholders, exactly
  local api_line='value: "" # rendered from PROVIDER_API_SERVER, or set by the operator'
  local args=(-e "s|ghcr.io/firestoned/faubourg-controller@sha256:<digest>|${CONTROLLER_IMAGE}|g"
              -e "s|ghcr.io/firestoned/faubourg-bootstrap@sha256:<digest>|${BOOTSTRAP_IMAGE}|g")
  if [ -n "$API" ]; then args+=(-e "s|${api_line}|value: \"${API}\"|"); fi
  sed "${args[@]}" "$1"
}

mkdir -p "$(dirname "$OUT")"
{
  echo "# faubourg, rendered by scripts/render-manifests.sh (ADR-0012)."
  echo "# controller: $CONTROLLER_IMAGE"
  echo "# bootstrap:  $BOOTSTRAP_IMAGE"
  for f in "$ROOT"/deploy/crds/*.yaml "$ROOT/deploy/rbac/controller.yaml" \
           "$ROOT/deploy/admission/bootstrap-policy-params.yaml" \
           "$ROOT/deploy/admission/bootstrap-policy.yaml" \
           "$ROOT/deploy/controller/deployment.yaml"; do
    echo "---"
    render "$f"
  done
} >"$OUT"

if grep -qF '@sha256:<digest>' "$OUT"; then
  echo "render-manifests: a placeholder digest is left in $OUT" >&2; exit 1
fi
[ "$(grep -v "^#" "$OUT" | grep -cF "$BOOTSTRAP_IMAGE")" -eq 2 ] || {
  echo "render-manifests: the bootstrap image must appear exactly twice (policy, controller)" >&2; exit 1
}
echo "render-manifests: wrote $OUT"
