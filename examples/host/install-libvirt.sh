#!/bin/sh
# Copyright (c) 2026 Erick Bourgeois, faubourg
# SPDX-License-Identifier: Apache-2.0
#
# faubourg's example install script for a libvirt host (ADR-0018 decision 7). Placed
# root-owned at /opt/faubourg/install.sh and run as `sudo -n /opt/faubourg/install.sh` with no
# arguments, under the same one sudo rule as any host (sudoers-faubourg).
#
# A libvirt host runs no banlieue process: banlieue's libvirt provider runs in the cluster and
# dials libvirtd over mutual TLS on 16514 (banlieue ADR-0011), with the client certificate the
# zone names (spec.libvirt.credentialsRef). This script prepares the host for that:
#
#   - installs libvirtd and QEMU/KVM from the distribution's packages;
#   - checks that the host's server certificate, its key and the CA are in libvirt's default
#     paths. It does not make them: they are per host, and come from your PKI (banlieue's
#     scripts/bootstrap-libvirt-tls.sh is one way) or from a per-host copy of this script kept
#     in a Secret (ADR-0005 decision 1);
#   - turns on TLS with the client certificate as the only credential (auth_tls "none"), and
#     admits only the client DN below;
#   - enables libvirtd's TLS socket.
#
# faubourg mints no token for a libvirt host, so nothing arrives on stdin; anything that does
# is refused. The script is idempotent: a rerun changes nothing that is already in place.

set -eu
umask 022
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

# ----- settings --------------------------------------------------------------------------
# The subject DN of the client certificate in the zone's credentialsRef Secret, exactly as
# `openssl x509 -noout -subject -nameopt RFC2253` prints it, minus "subject=". Only it may
# connect. Empty refuses to run: any certificate the CA signed would otherwise be admitted.
CLIENT_DN=""
# Packages, by package manager.
APT_PACKAGES="libvirt-daemon-system qemu-system-x86 qemu-utils"
DNF_PACKAGES="libvirt-daemon-kvm qemu-kvm"
# -----------------------------------------------------------------------------------------

# libvirt's default TLS paths (libvirtd.conf: key_file, cert_file, ca_file).
CA_FILE=/etc/pki/CA/cacert.pem
CERT_FILE=/etc/pki/libvirt/servercert.pem
KEY_FILE=/etc/pki/libvirt/private/serverkey.pem
CONF=/etc/libvirt/libvirtd.conf

[ "$(id -u)" -eq 0 ] || { echo "install-libvirt.sh: must run as root (sudo -n /opt/faubourg/install.sh)" >&2; exit 1; }
[ "$#" -eq 0 ] || { echo "install-libvirt.sh: takes no arguments" >&2; exit 2; }
[ -n "$CLIENT_DN" ] || { echo "install-libvirt.sh: set CLIENT_DN to the zone's client certificate subject" >&2; exit 1; }
# It goes into libvirtd.conf through sed: no quote, backslash, `|` or `&`.
# shellcheck disable=SC1003 # '\' is a literal backslash pattern, not an escaped quote
case "$CLIENT_DN" in *'"'*|*'\'*|*'|'*|*'&'*) echo "install-libvirt.sh: CLIENT_DN holds a quote, backslash, | or &" >&2; exit 1 ;; esac
# A libvirt host gets no credential on stdin (ADR-0018 decision 7).
if [ ! -t 0 ] && IFS= read -r _; then
    echo "install-libvirt.sh: unexpected input on stdin; a libvirt host is sent no credential" >&2
    exit 1
fi

# ----- packages --------------------------------------------------------------------------
if command -v apt-get >/dev/null 2>&1; then
    # shellcheck disable=SC2086 # word-split on purpose; root owns the value
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends $APT_PACKAGES >/dev/null
elif command -v dnf >/dev/null 2>&1; then
    # shellcheck disable=SC2086
    dnf install -y -q $DNF_PACKAGES >/dev/null
else
    echo "install-libvirt.sh: no apt-get or dnf; install libvirtd and QEMU yourself" >&2
    exit 1
fi
[ -e /dev/kvm ] || { echo "install-libvirt.sh: /dev/kvm is missing: no hardware virtualisation" >&2; exit 1; }

# ----- the host's TLS material -------------------------------------------------------------
for f in "$CA_FILE" "$CERT_FILE" "$KEY_FILE"; do
    if [ ! -s "$f" ] || [ -L "$f" ]; then
        echo "install-libvirt.sh: $f is missing; place the host's libvirt TLS material first" >&2
        exit 1
    fi
done
chown root:root "$KEY_FILE"
chmod 0600 "$KEY_FILE"

# ----- libvirtd.conf ---------------------------------------------------------------------
# Sets `key = value` in $CONF, replacing a commented or active line, or appending one.
set_conf() {
    if grep -Eq "^[#[:space:]]*$1[[:space:]]*=" "$CONF"; then
        tmp_conf="$(mktemp "${CONF}.XXXXXX")"
        sed -E "s|^[#[:space:]]*$1[[:space:]]*=.*|$1 = $2|" "$CONF" >"$tmp_conf"
        chmod 0644 "$tmp_conf"
        mv -f "$tmp_conf" "$CONF"
    else
        printf '%s = %s\n' "$1" "$2" >>"$CONF"
    fi
}
before="$(sha256sum "$CONF")"
set_conf auth_tls '"none"'
set_conf tls_no_verify_certificate 0
set_conf tls_allowed_dn_list "[\"$CLIENT_DN\"]"
set_conf ca_file "\"$CA_FILE\""
set_conf cert_file "\"$CERT_FILE\""
set_conf key_file "\"$KEY_FILE\""
after="$(sha256sum "$CONF")"

# ----- the TLS socket ----------------------------------------------------------------------
# Socket activation: libvirtd listens on 16514 through libvirtd-tls.socket, and listen_tls in
# libvirtd.conf is ignored. Plain TCP (16509) stays off.
systemctl disable --now libvirtd-tcp.socket >/dev/null 2>&1 || true
systemctl enable --now libvirtd.socket libvirtd-tls.socket >/dev/null
if [ "$before" != "$after" ]; then
    systemctl try-restart libvirtd.service
fi
systemctl is-active --quiet libvirtd-tls.socket || {
    echo "install-libvirt.sh: libvirtd-tls.socket is not active" >&2
    exit 1
}
echo "install-libvirt.sh: libvirtd accepts TLS on 16514 for one client DN"
