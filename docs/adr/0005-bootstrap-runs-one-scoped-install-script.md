<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0005: Bootstrap runs one install script, from a ConfigMap or Secret, under one scoped sudo rule

**Status:** Accepted, amended 2026-11-07 (decisions 1 and 5) · **Date:** 2026-11-07

## Context

A new host needs Cloud Hypervisor, firmware, the guest users, the template units, the
polkit rule and banlieue's host-resident provider. banlieue puts all of that behind
`banlieue host cloud-hypervisor install` (banlieue ADR-0067, ADR-0084).

The maintainer's direction (2026-11-07): keep faubourg's side of the install to **one shell
script**, delivered from a ConfigMap or a Secret, living at `/opt/faubourg/install.sh`, so
the SSH account needs exactly **one** sudo-allowed command instead of a list of them.

The design question that follows is who may write that file. A sudo rule that allows
`/opt/faubourg/install.sh` grants root to anyone who can change the script. If the scoped
SSH account could overwrite it, the scope would be decorative: that account would be root.

Running SSH from the controller process is rejected: the controller would hold every host's
credential for its whole life, run the SSH client that parses traffic from every host inside
the one long-lived process that manages them all, and lose an install in flight whenever it
restarts. The SSH session therefore runs in a per-host Job.

A Job alone does not keep the credentials from the controller, though. Permission to create a
Job in a namespace is, in effect, permission to read every Secret in it: the Job's pod can mount
any of them and print it. What the Job buys on its own is isolation of the SSH code, short-lived
credential exposure, per-host logs and retries, and a network path that only bootstrap pods
need. Keeping a compromised controller from the keys takes an admission policy on top
(ADR-0008).

## Decision

1. **One script.** `bootstrap.method: Script` (default). The script's source is
   `bootstrap.scriptRef`: a ConfigMap or a Secret and a key (default key `install.sh`) in
   the **credentials namespace** (ADR-0006), where the Job runs. With no `scriptRef`, the
   controller's configured default applies; it names an object in the same namespace. The default script shipped in `examples/` downloads a pinned `banlieue` binary,
   verifies its sha256, and runs `banlieue host cloud-hypervisor install`; operators may
   replace it entirely. The script takes **no arguments**; everything it needs is in it.
2. **One sudo rule.** On the host the SSH account (default `faubourg`) holds exactly
   `faubourg ALL=(root) NOPASSWD: /opt/faubourg/install.sh ""` in
   `/etc/sudoers.d/faubourg`. The trailing `""` forbids arguments.
3. **The script is root-owned and the scoped account cannot change it.**
   `/opt/faubourg` is `root:root 0755` and `install.sh` is `root:root 0755`. The scoped
   account can run it, never write it.
4. **Two credentials, two phases.**
   - **Placement** writes `/opt/faubourg/install.sh` and the sudoers drop-in. It needs a
     privileged credential, `ssh.placementSecretRef`, used only when the host's script is
     absent or differs from the source (sha256 compared). With `ssh.revokeAfterBootstrap`
     the placement key is removed from the host once placement succeeds.
   - **Run** connects with the scoped credential, `ssh.secretRef`, compares the on-host
     script's sha256 with the source, and runs `sudo -n /opt/faubourg/install.sh`.
   If the on-host script differs from the source and no placement credential is set, the
   Job does not run anything and the Host fails with reason `ScriptDrift`. An operator who
   prepares hosts out of band (image build, configuration management) places the script
   and sudoers rule there and never gives faubourg a privileged credential.
5. **A per-host Job, in the credentials namespace.** A pod mounts Secrets and ConfigMaps
   only from its own namespace, so the Job runs where the SSH Secrets and the script source
   live. The Host controller creates it, named after the Host and its generation and owned by
   it (a cluster-scoped owner may own a namespaced dependent). Its pod
   mounts only that host's Secret(s) and the script source as volumes. The controller holds no
   `get`, `list` or `watch` on Secrets there, and what it can put in a Job is constrained by
   ADR-0008.
6. **Success is the provider coming up, not an exit code.** The Host is `Ready` only when
   the Job succeeded **and** the host's banlieue `Provider` is `Ready=True` with a live
   Lease (ADR-0007).
7. `bootstrap.method: None` adopts a host that already runs banlieue's provider: no Job,
   no SSH, `ssh` may be omitted.
8. Re-bootstrap is a new generation of the Host spec, or a changed script source. The
   script must be idempotent; the default one is, because the installer's stages are.

## Consequences

- The steady-state credential on every host can do exactly one thing: run a script that
  root owns. Compromising it gains a re-run of the install, nothing more.
- Changing the install means changing one ConfigMap, then a placement run per host.
- The script, not a list of flags on the Host, is the escape hatch, so the Host API carries
  no free-form arguments and no shell reaches the host from a CR field.
- Open: how the host-resident provider receives its own scoped kube identity (banlieue
  ADR-0060 decision 5). The script is the natural carrier; that needs its own ADR.
- Open: the network path from the cluster's nodes to the hosts' SSH port.
- The credentials namespace now holds every root-equivalent input (A-1, A-2 and A-3 in the
  threat model), so one set of RBAC rules protects all of them. Whoever maintains the install
  script needs write access to that namespace, for ConfigMaps only.

*Amended 2026-11-07:* decision 1 moved the script source from the controller's namespace to
the credentials namespace, and decision 5 says where the Job runs and no longer claims that a
Job by itself keeps the credentials from the controller. Both followed from the first Job
design: as accepted, a pod could not have mounted both its Secret and its script.

*Amended 2026-11-07 by ADR-0009 (decision 6):* re-bootstrap follows a change to the Host's
bootstrap inputs (`address`, `ssh`, the resolved script source), not every new generation, so
cordoning a host or moving it to another zone runs nothing on it.
