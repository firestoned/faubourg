#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, faubourg
# SPDX-License-Identifier: Apache-2.0
#
# integration.sh: every integration test, as fast as they can share a machine. bootstrap-test
# (podman only, no kind) runs alongside; vap-test, controller-test and e2e-test run one after
# another on one kind cluster (scripts/kind-cluster.sh), which saves two cluster creations.
# They cannot run at the same time on it: they write the same cluster-wide objects.
#
# Each test's log is kept; a failing one's tail is printed. CI runs the tests as separate,
# parallel jobs instead, each on its own cluster.
#
#   FAUBOURG_KIND_CLUSTER  use this existing cluster and leave it up (default: create
#                          faubourg-it, delete it afterwards)
#
# Needs what the four tests need. With rootless podman, set KIND_EXPERIMENTAL_PROVIDER=podman.
#
# Usage: scripts/integration.sh
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOGS="$(mktemp -d)"
OWN=""
NAME="${FAUBOURG_KIND_CLUSTER:-}"
if [ -z "$NAME" ]; then NAME=faubourg-it; OWN=1; fi

BT_PID=""
cleanup() {
  if [ -n "$BT_PID" ]; then kill "$BT_PID" 2>/dev/null || true; fi
  if [ -n "$OWN" ]; then "$ROOT/scripts/kind-cluster.sh" down "$NAME" >/dev/null; fi
}
trap cleanup EXIT

failed=()
run() { # name, command...
  local name=$1 start=$SECONDS; shift
  if "$@" >"$LOGS/$name.log" 2>&1; then
    echo "ok   $name ($((SECONDS - start))s)"
  else
    echo "FAIL $name ($((SECONDS - start))s), log: $LOGS/$name.log"
    failed+=("$name")
  fi
}

echo "integration: logs in $LOGS"
bt_start=$SECONDS
"$ROOT/scripts/bootstrap-test.sh" >"$LOGS/bootstrap-test.log" 2>&1 &
BT_PID=$!

if [ -n "$OWN" ]; then "$ROOT/scripts/kind-cluster.sh" up "$NAME" >/dev/null || exit 1; fi
for t in vap-test controller-test e2e-test; do
  run "$t" env FAUBOURG_KIND_CLUSTER="$NAME" "$ROOT/scripts/$t.sh"
done

if wait "$BT_PID"; then
  echo "ok   bootstrap-test (alongside, $((SECONDS - bt_start))s)"
else
  echo "FAIL bootstrap-test, log: $LOGS/bootstrap-test.log"
  failed+=(bootstrap-test)
fi
BT_PID=""

for name in "${failed[@]}"; do
  echo "--- $name"
  grep -E '^FAIL' "$LOGS/$name.log" || tail -15 "$LOGS/$name.log"
done
if [ "${#failed[@]}" -ne 0 ]; then
  echo "integration: ${#failed[@]} failed (${failed[*]}), in ${SECONDS}s"
  exit 1
fi
echo "integration: all passed, in ${SECONDS}s"
