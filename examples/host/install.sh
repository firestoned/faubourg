#!/bin/sh
# Copyright (c) 2026 Erick Bourgeois, faubourg
# SPDX-License-Identifier: Apache-2.0
#
# faubourg's default host install script (ADR-0005). Placed root-owned at
# /opt/faubourg/install.sh and run as `sudo -n /opt/faubourg/install.sh` with no arguments,
# under the scoped account's one sudo rule (sudoers-faubourg).
#
# It fetches a pinned banlieue binary with curl from BANLIEUE_URL, which can be an internal
# artifact repository in an air-gapped site, verifies its sha256, and runs banlieue's own
# installer. Edit the settings below, publish the script in a ConfigMap, and faubourg
# places it on every host whose copy differs.
#
# The script takes no arguments and reads no environment: everything it does is in this
# file, which root owns. Its only input is stdin: faubourg's bootstrap writes the host's
# provider credential there (ADR-0011 decision 9). It installs that credential where
# banlieue's provider reads it, and keeps an existing one when stdin is empty.

set -eu
umask 022
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

# ----- settings --------------------------------------------------------------------------
BANLIEUE_VERSION="v0.0.0"
BANLIEUE_URL="https://registry.internal:5000/artifactory/banlieue/${BANLIEUE_VERSION}/banlieue-x86_64-unknown-linux-musl"
BANLIEUE_SHA256="0000000000000000000000000000000000000000000000000000000000000000"
# Extra CA for the internal repository; empty to use the system store.
CA_BUNDLE=""
# Flags for `banlieue host cloud-hypervisor install` (banlieue ADR-0084), e.g. the VMM and
# firmware from the same internal repository:
#   --vmm-url=https://registry.internal:5000/... --vmm-sha256=...
INSTALL_FLAGS=""
# -----------------------------------------------------------------------------------------

DEST_DIR=/opt/faubourg/bin
DEST="${DEST_DIR}/banlieue-${BANLIEUE_VERSION}"

[ "$(id -u)" -eq 0 ] || { echo "install.sh: must run as root (sudo -n /opt/faubourg/install.sh)" >&2; exit 1; }
[ "$#" -eq 0 ] || { echo "install.sh: takes no arguments" >&2; exit 2; }

# ----- the provider credential bundle on stdin (ADR-0011) ---------------------------------
# faubourg-provider-credential v1
# provider <name>
# namespace <namespace>
# token <token>
# kubeconfig
# <kubeconfig, to the end>
CRED_DIR=/etc/banlieue/credentials
PROVIDER_NAME=""
PROVIDER_NAMESPACE=""
TOKEN=""
KUBECONFIG_TEXT=""
bad_bundle() { echo "install.sh: malformed credential bundle on stdin" >&2; exit 1; }
if IFS= read -r header; then
    [ "$header" = "faubourg-provider-credential v1" ] || bad_bundle
    IFS=' ' read -r key PROVIDER_NAME && [ "$key" = provider ] || bad_bundle
    IFS=' ' read -r key PROVIDER_NAMESPACE && [ "$key" = namespace ] || bad_bundle
    IFS=' ' read -r key TOKEN && [ "$key" = token ] || bad_bundle
    IFS= read -r key && [ "$key" = kubeconfig ] || bad_bundle
    KUBECONFIG_TEXT="$(cat)"
    case "$PROVIDER_NAME" in ''|*[!a-z0-9.-]*) bad_bundle ;; esac
    case "$PROVIDER_NAMESPACE" in ''|*[!a-z0-9-]*) bad_bundle ;; esac
    case "$TOKEN" in ''|*[!A-Za-z0-9._-]*) bad_bundle ;; esac
    [ -n "$KUBECONFIG_TEXT" ] || bad_bundle
fi

# Writes $2 to $1 as the banlieue user, mode 0600, through a temporary file in the same
# directory. The content is never echoed.
write_credential() {
    tmp_cred="$(umask 077; mktemp "${CRED_DIR}/.credential.XXXXXX")"
    printf '%s' "$2" >"$tmp_cred"
    chown banlieue: "$tmp_cred"
    chmod 0600 "$tmp_cred"
    mv -f "$tmp_cred" "$1"
}
command -v curl >/dev/null 2>&1 || { echo "install.sh: curl not found" >&2; exit 1; }
command -v sha256sum >/dev/null 2>&1 || { echo "install.sh: sha256sum not found" >&2; exit 1; }

install -d -o root -g root -m 0755 "$DEST_DIR"

if [ ! -x "$DEST" ] || ! echo "${BANLIEUE_SHA256}  ${DEST}" | sha256sum -c - >/dev/null 2>&1; then
    tmp="$(mktemp "${DEST_DIR}/.banlieue.XXXXXX")"
    trap 'rm -f "$tmp"' EXIT
    set -- --fail --silent --show-error --location --proto '=https' --tlsv1.2
    [ -n "$CA_BUNDLE" ] && set -- "$@" --cacert "$CA_BUNDLE"
    curl "$@" --output "$tmp" "$BANLIEUE_URL"
    echo "${BANLIEUE_SHA256}  ${tmp}" | sha256sum -c - >/dev/null || {
        echo "install.sh: sha256 mismatch for banlieue ${BANLIEUE_VERSION}" >&2
        exit 1
    }
    chmod 0755 "$tmp"
    mv -f "$tmp" "$DEST"
    trap - EXIT
fi
ln -sfn "$DEST" "${DEST_DIR}/banlieue"

# banlieue's installer is idempotent by stage; preflight and selftest change nothing. With a
# bundle, the Provider is named, and the installer runs again once the credential is in
# place, so it enables the provider it could not enable without one.
if [ -n "$PROVIDER_NAME" ]; then
    BANLIEUE_HOST_PROVIDER_NAME="$PROVIDER_NAME"
    BANLIEUE_HOST_PROVIDER_NAMESPACE="$PROVIDER_NAMESPACE"
    export BANLIEUE_HOST_PROVIDER_NAME BANLIEUE_HOST_PROVIDER_NAMESPACE
fi
"${DEST_DIR}/banlieue" host cloud-hypervisor preflight
# shellcheck disable=SC2086 # INSTALL_FLAGS is word-split on purpose; root owns its value
"${DEST_DIR}/banlieue" host cloud-hypervisor install $INSTALL_FLAGS
if [ -n "$TOKEN" ]; then
    write_credential "${CRED_DIR}/token" "$TOKEN"
    write_credential "${CRED_DIR}/kubeconfig" "$KUBECONFIG_TEXT
"
    # shellcheck disable=SC2086
    "${DEST_DIR}/banlieue" host cloud-hypervisor install $INSTALL_FLAGS
fi
"${DEST_DIR}/banlieue" host cloud-hypervisor selftest
