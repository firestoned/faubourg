#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, faubourg
# SPDX-License-Identifier: Apache-2.0
#
# bootstrap-test.sh: the bootstrap image against a real OpenSSH server (ADR-0010 decision 10).
# Builds the image (images/bootstrap) and a throwaway sshd host (test/sshd), then runs the
# image the way a Job does: read-only root, UID 65532, no capabilities, the credentials and the
# script mounted read-only, the termination message in /dev/termination-log.
#
# Needs: podman, ssh-keygen, jq. Every key is generated here and deleted on exit.
#
#   BT_KEEP=1   keep the images, network and work directory afterwards
#
# Usage: scripts/bootstrap-test.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NET=faubourg-bt
IMG=localhost/faubourg-bootstrap:test
SSHD_IMG=localhost/faubourg-test-sshd:test
SSHD=faubourg-bt-sshd

for tool in podman ssh-keygen jq; do
  command -v "$tool" >/dev/null 2>&1 || { echo "bootstrap-test: $tool not found" >&2; exit 1; }
done

WORK="$(mktemp -d)"
cleanup() {
  podman rm -f "$SSHD" >/dev/null 2>&1 || true
  if [ "${BT_KEEP:-}" != 1 ]; then
    podman network rm -f "$NET" >/dev/null 2>&1 || true
    podman rmi -f "$SSHD_IMG" >/dev/null 2>&1 || true
    rm -rf "$WORK"
  else
    echo "bootstrap-test: kept $WORK"
  fi
}
trap cleanup EXIT

echo "bootstrap-test: building the bootstrap image"
podman build -q -f "$ROOT/images/bootstrap/Containerfile" -t "$IMG" "$ROOT" >/dev/null

# Throwaway keys. The credential volumes are made readable by the image's UID, which rootless
# podman maps to a host UID other than ours.
keygen() { ssh-keygen -q -t "$1" -N "" -C "" -f "$WORK/$2"; }
keygen ed25519 run
keygen ed25519 placement
keygen ed25519 stranger
keygen rsa rsa
mkdir -p "$WORK/sshd-ctx"
cp "$WORK/run.pub" "$WORK/placement.pub" "$WORK/sshd-ctx/"
cp "$ROOT/test/sshd/Containerfile" "$WORK/sshd-ctx/"
echo "bootstrap-test: building the test sshd host"
podman build -q -t "$SSHD_IMG" "$WORK/sshd-ctx" >/dev/null
podman network create "$NET" >/dev/null 2>&1 || true

fresh_host() { # a host with nothing placed
  podman rm -f "$SSHD" >/dev/null 2>&1 || true
  podman run -d --name "$SSHD" --network "$NET" --network-alias sshd "$SSHD_IMG" >/dev/null
  for _ in $(seq 1 30); do
    podman logs "$SSHD" 2>&1 | grep -q 'Server listening' && return 0
    sleep 0.5
  done
  echo "bootstrap-test: sshd did not start" >&2
  podman logs "$SSHD" >&2
  exit 1
}

fresh_host
HOST_KEY_LINE="$(podman exec "$SSHD" cat /etc/ssh/ssh_host_ed25519_key.pub | awk '{print $1, $2}')"
HOST_FP="$(ssh-keygen -lf <(echo "$HOST_KEY_LINE") | awk '{print $2}')"
STRANGER_LINE="$(awk '{print $1, $2}' "$WORK/stranger.pub")"

cred() { # dir, user, private key file[, knownHosts line]
  mkdir -p "$1"
  printf '%s' "$2" >"$1/username"
  cp "$3" "$1/privateKey"
  if [ -n "${4:-}" ]; then printf 'sshd %s\n' "$4" >"$1/knownHosts"; fi
  chmod 0755 "$1"; chmod 0644 "$1"/*
}
script() { # dir, body
  mkdir -p "$1"
  printf '#!/bin/sh\n%s\n' "$2" >"$1/install.sh"
  chmod 0755 "$1"; chmod 0644 "$1/install.sh"
}
GOOD_SCRIPT='echo "install ran as uid $(id -u)"; touch /var/tmp/faubourg-installed'
script "$WORK/script-a" "$GOOD_SCRIPT"
script "$WORK/script-b" "$GOOD_SCRIPT
# version b"
script "$WORK/script-fail" 'echo failing on purpose; exit 3'

cred "$WORK/run-kh" faubourg "$WORK/run" "$HOST_KEY_LINE"
cred "$WORK/run-wrong-kh" faubourg "$WORK/run" "$STRANGER_LINE"
cred "$WORK/run-nokh" faubourg "$WORK/run"
cred "$WORK/run-rsa" faubourg "$WORK/rsa" "$HOST_KEY_LINE"
cred "$WORK/place-admin" admin "$WORK/placement"
cred "$WORK/place-root" root "$WORK/placement"

failures=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1: $2"; failures=$((failures + 1)); }

# bootstrap name, want-exit, run-cred, script, [placement-cred], [policy], [accepted], [revoke]
LAST_LOG=""; LAST_REPORT=""
bootstrap() {
  local name=$1 want=$2 run=$3 scr=$4 place=${5:-} policy=${6:-Strict} accepted=${7:-} revoke=${8:-false}
  local term="$WORK/term-$RANDOM" log="$WORK/log-$RANDOM" got=0
  : >"$term"; chmod 0666 "$term"
  local args=(--rm --network "$NET" --read-only --user 65532:65532 --cap-drop ALL
    --security-opt no-new-privileges
    -v "$WORK/$run:/var/run/faubourg/credentials:ro"
    -v "$WORK/$scr:/var/run/faubourg/script:ro"
    -v "$term:/dev/termination-log"
    -e FAUBOURG_HOST=hv01 -e FAUBOURG_ADDRESS=sshd -e FAUBOURG_PORT=22
    -e FAUBOURG_HOST_KEY_POLICY="$policy" -e FAUBOURG_REVOKE_PLACEMENT="$revoke")
  [ -n "$place" ] && args+=(-v "$WORK/$place:/var/run/faubourg/placement:ro")
  [ -n "$accepted" ] && args+=(-e FAUBOURG_ACCEPTED_HOST_KEY="$accepted")
  podman run "${args[@]}" "$IMG" >"$log" 2>&1 || got=$?
  LAST_LOG="$log"; LAST_REPORT="$(cat "$term")"
  if [ "$got" = "$want" ]; then pass "$name (exit $got)"
  else fail "$name" "exit $got, want $want: $(tail -5 "$log")"; fi
}
on_host() { podman exec "$SSHD" sh -c "$1"; }
reported() { [ "$(jq -r '.hostKey // empty' <<<"$LAST_REPORT")" = "$HOST_FP" ]; }

echo "--- the image"
if podman image inspect "$IMG" --format '{{.Config.User}}' | grep -qx '65532:65532'; then
  pass "the image runs as 65532"
else fail "the image user" "$(podman image inspect "$IMG" --format '{{.Config.User}}')"; fi
if podman run --rm --entrypoint /bin/sh "$IMG" -c true >/dev/null 2>&1; then
  fail "the image" "has a shell"
else pass "the image has no shell"; fi

echo "--- host keys"
bootstrap "Strict without knownHosts refuses every key" 65 run-nokh script-a
if on_host 'test -e /opt/faubourg/install.sh'; then fail "a refused key" "placed something"; else pass "nothing was placed"; fi
bootstrap "knownHosts pinning another key is refused" 65 run-wrong-kh script-a place-admin
bootstrap "knownHosts overrides TrustOnFirstUse" 65 run-wrong-kh script-a "" TrustOnFirstUse
bootstrap "an RSA credential is refused before connecting" 1 run-rsa script-a
if grep -qF "RSA keys are refused" "$LAST_LOG"; then pass "and says so"; else fail "the RSA refusal" "$(tail -3 "$LAST_LOG")"; fi

echo "--- drift and placement"
bootstrap "no script and no placement credential is ScriptDrift" 64 run-kh script-a
if reported; then pass "drift reports the host key"; else fail "the drift report" "$LAST_REPORT"; fi
bootstrap "placement through sudo, then the run" 0 run-kh script-a place-admin
if reported; then pass "success reports the host key"; else fail "the report" "$LAST_REPORT"; fi
if [ "$(on_host 'stat -c "%u %g %a" /opt/faubourg /opt/faubourg/install.sh' | tr '\n' ' ')" = "0 0 755 0 0 755 " ]; then
  pass "the script is root:root 0755"
else fail "the script's owner and mode" "$(on_host 'ls -ld /opt/faubourg /opt/faubourg/install.sh')"; fi
if [ "$(on_host 'stat -c "%u %g %a" /etc/sudoers.d/faubourg')" = "0 0 440" ] \
   && on_host 'grep -qxF "faubourg ALL=(root) NOPASSWD: /opt/faubourg/install.sh \"\"" /etc/sudoers.d/faubourg'; then
  pass "the sudo rule is root:root 0440 and allows only the script"
else fail "the sudo rule" "$(on_host 'ls -l /etc/sudoers.d; cat /etc/sudoers.d/faubourg')"; fi
if on_host 'test -e /var/tmp/faubourg-installed' && grep -qF "install ran as uid 0" "$LAST_LOG"; then
  pass "the script ran as root and its output is in the log"
else fail "the run" "$(tail -5 "$LAST_LOG")"; fi
bootstrap "in place: the run needs no placement credential" 0 run-kh script-a
bootstrap "a changed script without placement is ScriptDrift" 64 run-kh script-b
on_host 'chmod 0777 /opt/faubourg/install.sh'
bootstrap "a script made writable is drift, not run" 64 run-kh script-a
on_host 'chmod 0755 /opt/faubourg/install.sh'
bootstrap "a failing script fails the Job" 1 run-kh script-fail place-admin
if grep -qF "failing on purpose" "$LAST_LOG"; then pass "with its output in the log"; else fail "the failing script's log" "$(tail -3 "$LAST_LOG")"; fi

echo "--- placement as root, and revoke"
fresh_host
bootstrap "placement as root, then revoke" 0 run-kh script-a place-root "" "" true
PLACEMENT_BLOB="$(awk '{print $2}' "$WORK/placement.pub")"
if on_host "grep -qF '$PLACEMENT_BLOB' /root/.ssh/authorized_keys"; then
  fail "revoke" "the placement key is still in root's authorized_keys"
else pass "the placement key is gone from root's authorized_keys"; fi
if on_host "grep -qF '$PLACEMENT_BLOB' /home/admin/.ssh/authorized_keys"; then pass "and only from the account that placed"
else fail "revoke" "it removed another account's key"; fi
bootstrap "the revoked key no longer works" 1 run-kh script-b place-root

echo "--- trust on first use"
fresh_host
HOST_KEY_LINE2="$(podman exec "$SSHD" cat /etc/ssh/ssh_host_ed25519_key.pub | awk '{print $1, $2}')"
HOST_FP="$(ssh-keygen -lf <(echo "$HOST_KEY_LINE2") | awk '{print $2}')"
bootstrap "first use accepts and reports the key" 0 run-nokh script-a place-admin TrustOnFirstUse
if reported; then pass "the reported key is the host's"; else fail "the TOFU report" "$LAST_REPORT"; fi
bootstrap "the pinned key is accepted" 0 run-nokh script-a "" TrustOnFirstUse "$HOST_FP"
bootstrap "another pinned key is refused" 65 run-nokh script-a "" TrustOnFirstUse \
  "$(ssh-keygen -lf "$WORK/stranger.pub" | awk '{print $2}')"

echo "--- logs"
if cat "$WORK"/log-* | grep -qE 'PRIVATE KEY|faubourg-installed|version b'; then
  fail "the logs" "carry a key or the script's content"
else pass "the logs carry no key and no script content"; fi

if [ "$failures" -ne 0 ]; then echo "bootstrap-test: $failures failure(s)"; exit 1; fi
echo "bootstrap-test: all passed"
