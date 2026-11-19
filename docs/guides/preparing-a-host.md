<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# Preparing a host

!!! warning "The bootstrap Job is not built yet"

    This guide describes the contract the bootstrap Job implements
    ([ADR-0005](../adr/0005-bootstrap-runs-one-scoped-install-script.md)). The files it
    places are in `examples/host/` today. The Job itself is roadmap 01.

faubourg prepares a host by running **one shell script**, `/opt/faubourg/install.sh`, as root,
under **one sudo rule**. The script is yours: the default one installs banlieue's
host-resident provider, and you can replace it entirely.

## What ends up on the host

| Path | Owner and mode | Purpose |
| --- | --- | --- |
| `/opt/faubourg/` | `root:root 0755` | Holds the script. The scoped account cannot write here. |
| `/opt/faubourg/install.sh` | `root:root 0755` | The one script root runs. |
| `/etc/sudoers.d/faubourg` | `root:root 0440` | The one sudo rule. |

The sudo rule, from
[`examples/host/sudoers-faubourg`](https://github.com/firestoned/faubourg/blob/main/examples/host/sudoers-faubourg):

```text
Defaults:faubourg !requiretty, env_reset, secure_path="/usr/sbin:/usr/bin:/sbin:/bin"
faubourg ALL=(root) NOPASSWD: /opt/faubourg/install.sh ""
```

The trailing `""` forbids arguments. Because root owns both the directory and the script, the
`faubourg` account can run the script and never change it. If it could change the script, the
sudo rule would make it root.

## The default install script

[`examples/host/install.sh`](https://github.com/firestoned/faubourg/blob/main/examples/host/install.sh):

1. refuses any argument and any non-root caller, and sets a fixed `PATH`;
2. downloads a pinned `banlieue` binary, or banlieue's release tarball, with
   `curl --proto '=https' --tlsv1.2`, from any repository, including an internal mirror in an
   air-gapped site;
3. checks its sha256 against the value pinned in the script before unpacking anything, refuses
   on a mismatch, and installs the binary owned by root;
4. runs `banlieue host cloud-hypervisor install` with that binary as `--provider-binary`:
   banlieue's staged and idempotent host installer, which writes only what differs (see the
   [banlieue docs](https://firestoned.github.io/banlieue/)).

Edit the settings block at the top: `BANLIEUE_VERSION`, `BANLIEUE_URL`, `BANLIEUE_SHA256`,
optionally `CA_BUNDLE` for an internal CA, and `INSTALL_FLAGS` for the installer.

## Publish the script

The script source is a `ConfigMap` (or a `Secret`, if the script embeds something secret) in
the **credentials namespace**, where the bootstrap Job runs and its SSH Secrets live
([ADR-0005](../adr/0005-bootstrap-runs-one-scoped-install-script.md)). A Host points at it with
`bootstrap.scriptRef`; with no `scriptRef`, the controller's configured default applies.

```sh
kubectl -n faubourg-credentials create configmap faubourg-install \
  --from-file=install.sh=examples/host/install.sh --dry-run=client -o yaml | kubectl apply -f -
```

A Job runs only a script whose content is pinned
([ADR-0013](../adr/0013-script-content-pinned-and-rolled-out.md)). The platform administrator
pins it in the `scriptSources` key of the `faubourg-bootstrap-policy` ConfigMap in
`faubourg-system`: the source, the key and the sha256 of exactly what it holds.

```sh
sha256sum examples/host/install.sh
# scriptSources: ConfigMap/faubourg-install/install.sh@sha256:<that sha256>
```

The admission policy refuses a Job for any other source, key or digest, and the bootstrap image
hashes the mounted script before it connects to anything: a script edited without a re-pin
fails its Job (`ScriptUnpinned`, exit 66) and reaches no host. A Host whose script is not
pinned at all is `Failed`, reason `ScriptNotPinned`, and gets no Job.

!!! danger "The script maintainer and the platform administrator are root on every host"

    The script runs as root on every host it is placed on. A change takes both roles: one
    writes the ConfigMap, the other pins it. Restrict write access to both ConfigMaps as
    tightly as you restrict cluster-admin. This is accepted risk R-1 in the
    [threat model](../security/threat-model.md).

## Two ways to place the script

=== "faubourg places it"

    Give the Host a **placement credential** (`ssh.placementSecretRef`): `root`, or an account
    with passwordless `sudo`. The Job uses it only when the host's copy of the script is
    missing, differs from the source (sha256 compared), or is not `root:root 0755`, or when
    the scoped account cannot log in yet. It creates the scoped account if it is missing (no
    password) and authorizes the run key for it, so a fresh host needs nothing but the
    placement account. The run credential must then be a key, not a password. It writes
    the script and the sudo rule through temporary files, checks the rule with `visudo -cf`,
    then checks the result again with the scoped account. With
    `ssh.revokeAfterBootstrap: true`, it then removes the placement key from that account's
    `~/.ssh/authorized_keys`.

=== "You place it out of band"

    Your image build or configuration management installs the two files with the owners and
    modes above, and creates the `faubourg` account with only the scoped key. faubourg never
    receives a privileged credential. If the host's script later drifts from the source, the
    Job runs nothing and the Host fails with reason `ScriptDrift`.

## Then the run

With the script in place, the Job connects as the scoped account, compares the on-host script's
owner, mode and sha256 with the source, and runs:

```sh
sudo -n /opt/faubourg/install.sh
```

The Host turns `Ready` when the Job succeeded *and* the host's banlieue `Provider` is
`Ready=True` with an `Alive` heartbeat ([Concepts](../concepts.md#lifecycle-of-a-host)).

A host is re-bootstrapped when its bootstrap inputs change: `address`, `ssh`, the script
source it names, or that script's pinned digest
([ADR-0009](../adr/0009-host-reconciler.md) decision 6,
[ADR-0013](../adr/0013-script-content-pinned-and-rolled-out.md)). Cordoning a host or moving it
to another zone runs nothing on it. The script must still be idempotent; the default one is,
because banlieue's installer stages are.

## Changing the script

Edit the ConfigMap, then pin its new sha256 in `scriptSources`. Every Host using that script is
then due for a re-bootstrap, and they roll out **one at a time** (`FAUBOURG_MAX_REBOOTSTRAPS`
on the controller, default 1):

- a Host waiting for its turn keeps its phase, with condition `BootstrapCurrent=False`, reason
  `RolloutWaiting`;
- if one re-bootstrap fails, no other starts (`RolloutHalted`) until that Host's inputs change
  again or you delete its failed Job;
- `BootstrapCurrent=True` once a Host's Job for the current inputs has succeeded.

A host that has never bootstrapped is not held: it has nothing to lose. To roll back, put the
previous content back and pin its digest again: the same rollout, in reverse. Upgrading to a
release with ADR-0013 changes every Host's inputs, so each bootstrapped host re-runs its
install once, through the same rollout.

A failed bootstrap is not retried by the controller: the Job stays as the record and the Host
stays `Failed`. Fix the cause, then change the inputs or delete the Job
(`kubectl -n faubourg-credentials delete job <name>`); the controller creates a new one.

Editing the script's content without a re-pin runs nothing: the next Job for that script
fails before reaching a host, and says why.

## Adopting a host that already runs banlieue

Set `bootstrap.method: None` and name the Host after the host's existing `Provider`.
faubourg creates no Job and needs no SSH settings. It labels that Provider with the host's
zone, name and cordon ([ADR-0011](../adr/0011-the-hosts-provider-and-its-identity.md)
decision 3), never touches its spec, and reports its health. With no Provider of that name,
the Host waits (`ProviderMissing`).

Exactly one Provider may carry the host's label. With two, the Host reports
`ProviderAmbiguous` and is never `Ready`.

## Bootstrapping a host that already runs banlieue

Switching an adopted Host to `bootstrap.method: Script` re-runs banlieue's installer on a host
that has its own settings. The script is shared by every host, so its `INSTALL_FLAGS` must
reproduce that host's: a VMM build other than the release's, or a guest budget
(`--guest-memory-max` and the like). With flags that do not fit, the installer repoints the VMM
and rewrites the guest slice (accepted risk R-13 in the
[threat model](../security/threat-model.md)).

See what it would change first, on the host, with the binary it already runs and your flags:

```sh
sudo env BANLIEUE_HOST_PROVIDER_NAME=hv01 BANLIEUE_HOST_PROVIDER_NAMESPACE=banlieue-system \
  /usr/local/bin/banlieue host cloud-hypervisor install --dry-run \
  --provider-binary /usr/local/bin/banlieue <INSTALL_FLAGS>
```

Every `would write` and `would link` line is a change. Adjust `INSTALL_FLAGS` until the only
ones left are those you want. Point the Host's `address` at the host's own sshd and pin its key
in `knownHosts`: an SSH front end on another address, such as a mesh VPN's, presents a
different key and is refused.

## The host's Provider and its credential

For a bootstrapped Host, faubourg creates the `Provider` itself, named after the Host, in
banlieue's namespace, of its zone's class: the zone's `providerClassRef`, or the controller's
`--provider-class` (default `cloud-hypervisor`) when the zone names none
([ADR-0018](../adr/0018-a-zone-names-its-provider-class.md)). faubourg drives a class whose
`backend` is `cloud-hypervisor` and whose `deployment` is `External`, or `libvirt` and `Managed`
(below); any other class fails the zone's Hosts with `ProviderBackendUnsupported`. A Host whose existing `Provider` references
another class than its zone's is `ProviderClassMismatch`, and faubourg leaves that `Provider`
alone. Two Hosts with the same `address` (compared as written, ignoring the case of an IPv6 address's hex digits, never resolved)
are both `DuplicateAddress`: one machine is one Host. banlieue's operator then creates its
ServiceAccount, and only then does faubourg create the bootstrap Job (`Pending`, reason
`AwaitingProviderIdentity`, until it has).

The Job mints the provider's first token, valid for a day, and hands it to the install script
on its standard input, with a kubeconfig that reads it from `/etc/banlieue/credentials/token`.
The default script installs both there and names the Provider to banlieue's installer. From
then on banlieue's provider renews its own token. The controller never sees the token, and no
Secret holds it.

A replacement install script must read the bundle from stdin (format in ADR-0011 decision 9),
or install the credential some other way. A host that stays down longer than its token's
lifetime is recovered by re-bootstrapping it: delete its Job.

## A libvirt host

A zone whose class has backend `libvirt` holds hosts that run libvirtd and no banlieue process:
banlieue's libvirt provider runs in the cluster and dials each host over mutual TLS on 16514
([ADR-0018](../adr/0018-a-zone-names-its-provider-class.md)). The same Job, script path and sudo
rule prepare the host; what differs:

- **The zone names a client credential.** `spec.libvirt.credentialsRef` is a Secret in
  banlieue's namespace with the client certificate (`tls.crt`, `tls.key`), and
  `spec.libvirt.caBundleRef` a ConfigMap key with the CA that signed each host's libvirtd
  certificate. Both are yours to make, both are named `faubourg-...`, and faubourg copies their
  names into each host's `Provider` without reading them. An admission policy,
  `faubourg-provider-connection`, refuses any other name from faubourg's controller. See
  [`examples/03-libvirt-zone.yaml`](https://github.com/firestoned/faubourg/blob/main/examples/03-libvirt-zone.yaml).
- **The Provider** is `qemu+tls://<address>/system`, with the zone's credential and CA. Its
  class must be `Managed`.
- **No token.** The Job mints nothing, mounts no API access and sends the script nothing on its
  standard input, and is created without waiting for banlieue to give the Provider an identity.
- **The script** is
  [`examples/host/install-libvirt.sh`](https://github.com/firestoned/faubourg/blob/main/examples/host/install-libvirt.sh):
  it installs libvirtd and QEMU from the distribution's packages, checks that the host's server
  certificate, key and CA are in libvirt's default paths, sets TLS with the client certificate
  as the only credential, admits only the client DN you set in it, and enables libvirtd's TLS
  socket (never plain TCP). It does not make the server certificate: that is per host, from your
  PKI, or from a per-host copy of the script kept in a Secret. Set `CLIENT_DN` before pinning it.
- **Health** is the provider's `ProviderReachable`, not its heartbeat, which is a pod's.
- **Drain** cordons the host and reports `MovesUnsupported`: banlieue moves no libvirt VM.

One machine is one Host, so a machine is either a Cloud Hypervisor host or a libvirt host in
faubourg, never both.
