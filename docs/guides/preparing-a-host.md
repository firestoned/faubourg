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
2. downloads a pinned `banlieue` binary with `curl --proto '=https' --tlsv1.2`, from any
   repository, including an internal mirror in an air-gapped site;
3. checks its sha256 against the value pinned in the script, and refuses on a mismatch;
4. runs `banlieue host cloud-hypervisor install`, banlieue's staged and idempotent host
   installer (see the [banlieue docs](https://firestoned.github.io/banlieue/)).

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

The admission policy only lets a Job mount a script source that is on its allowlist
([ADR-0008](../adr/0008-bootstrap-jobs-constrained-by-admission-policy.md)), so add it to the
`scriptSources` key of the `faubourg-bootstrap-policy` ConfigMap in `faubourg-system`, for
example `ConfigMap/faubourg-install`. That ConfigMap is a platform administrator's to edit,
not the script maintainer's.

!!! danger "Whoever can write the script source is root on every host"

    The script runs as root on every host whose copy is updated. Restrict write access to this
    ConfigMap or Secret as tightly as you restrict cluster-admin. This is accepted risk R-1 in
    the [threat model](../security/threat-model.md).

## Two ways to place the script

=== "faubourg places it"

    Give the Host a **placement credential** (`ssh.placementSecretRef`): an account that can
    write `/opt/faubourg` and `/etc/sudoers.d`. The Job uses it only when the host's copy of the
    script is missing or differs from the source (sha256 compared). With
    `ssh.revokeAfterBootstrap: true`, the Job removes the placement key from the host once
    placement succeeds.

=== "You place it out of band"

    Your image build or configuration management installs the two files with the owners and
    modes above, and creates the `faubourg` account with only the scoped key. faubourg never
    receives a privileged credential. If the host's script later drifts from the source, the
    Job runs nothing and the Host fails with reason `ScriptDrift`.

## Then the run

With the script in place, the Job connects as the scoped account, compares the on-host script's
sha256 with the source, and runs:

```sh
sudo -n /opt/faubourg/install.sh
```

The Host turns `Ready` when the Job succeeded *and* the host's banlieue `Provider` is
`Ready=True` with an `Alive` heartbeat ([Concepts](../concepts.md#lifecycle-of-a-host)).

A host is re-bootstrapped when its bootstrap inputs change: `address`, `ssh`, or the script
source it names ([ADR-0009](../adr/0009-host-reconciler.md) decision 6). Cordoning a host,
moving it to another zone, or upgrading the controller runs nothing on it. The script must
still be idempotent; the default one is, because banlieue's installer stages are.

A failed bootstrap is not retried by the controller: the Job stays as the record and the Host
stays `Failed`. Fix the cause, then change the inputs or delete the Job
(`kubectl -n faubourg-credentials delete job <name>`); the controller creates a new one.

Editing the script's content under the same ConfigMap name does not re-run anything yet.

## Adopting a host that already runs banlieue

Set `bootstrap.method: None`. faubourg creates no Job, needs no SSH settings, and reports the
health of the host's `Provider`. Until faubourg creates and labels Providers itself
(roadmap 01), label the existing one with the host's name:

```sh
kubectl -n banlieue-system label provider <provider> faubourg.firestoned.io/host=<host>
```

Exactly one Provider may carry that label. With two, the Host reports `ProviderAmbiguous`
and is never `Ready`.
