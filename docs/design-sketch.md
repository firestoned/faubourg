<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

> **Historical.** The original design sketch, kept for context. Where it disagrees with
> `docs/adr/`, the ADRs win: ADR-0002 replaces the host agent and scheduler with
> banlieue's host-resident provider and scheduler, and ADR-0005 moves SSH out of the
> controller into a per-host Job.

# faubourg design sketch

## Terms

- **Host**: one physical machine running many `cloud-hypervisor` processes
  (one process per VM).
- **AvailabilityZone (AZ)**: a failure domain and a shared-fate boundary for
  storage and network. A zone holds N hosts.
- **Host agent**: a small daemon on each host that drives the Cloud Hypervisor
  REST API over its unix socket, manages taps and disks, and reports health.
- **Controller**: runs in kube. Reconciles Host and AvailabilityZone, runs the
  scheduler, and implements the banlieue provider.

Kube is used for infrastructure orchestration only. Guest workloads never run in
it.

## Architecture

    banlieue (VirtualMachineClaim)
            |
            v
    faubourg provider  -->  scheduler  -->  Host (picked)
            |                                     |
            v                                     v
    Host / AvailabilityZone CRs           host agent -> cloud-hypervisor x N

## Bootstrap flow (default)

1. User creates a Host CR with `zoneRef`, `address`, and `ssh.secretRef`.
2. Controller sets phase `Bootstrapping`, loads the Secret, and opens SSH with
   host key verification.
3. Controller runs the banlieue CLI against the host (method `BanlieueCLI`).
   The exact command, flags and idempotency contract are TBD, see open
   questions. The Host spec carries `bootstrap.banlieue.args` as an escape
   hatch until that is pinned down.
4. The bootstrap installs Cloud Hypervisor (pinned version), the host agent, and
   host networking for the zone.
5. The agent starts, attests to the controller (SPIFFE/SPIRE), and reports
   capacity. Host moves to `Ready`.
6. SSH is no longer needed for steady state. Day-2 operations go through the
   agent. The controller keeps SSH only for re-bootstrap and upgrades.

`bootstrap.method: None` adopts a host that already runs the agent.

## Credentials

The Host CR references a Secret, it never embeds credentials.

Secret keys:

| key          | required              | notes                                  |
|--------------|-----------------------|----------------------------------------|
| `username`   | yes                   | account used for bootstrap             |
| `privateKey` | one of key or password | preferred                              |
| `passphrase` | no                    | for an encrypted `privateKey`          |
| `password`   | one of key or password | supported, discouraged                 |
| `knownHosts` | recommended           | pins the host key, otherwise TOFU      |

Concerns to design around, since this is a root-equivalent credential on every
hypervisor:

- Prefer keys, and prefer short-lived SSH certificates from an SSH CA if one is
  available. A static key in a kube Secret is the weakest option here.
- Verify host keys. Default to refusing unknown hosts unless the user opts into
  trust on first use, and record the accepted key in status.
- Scope the Secret to a namespace the controller can read and few others can.
- After a successful bootstrap, support `ssh.revokeAfterBootstrap` so the
  controller can remove its key or disable the account, leaving the agent as the
  only management path.
- Never log command lines or Secret values.

## Zones

An AZ declares its storage and network model, because that decides what the
platform can promise:

- `storage.mode: Shared`: VMs can restart on another host in the zone, and live
  migration is possible. Requires fencing before restart elsewhere. Not
  supported in v1.
- `storage.mode: Local`: VMs are pinned to their host. Host loss means VM loss.
  Simplest, and fine for ephemeral sandboxes. The only mode in v1.

Networking is routed, not bridged. Each host gets a CIDR slice from the zone
CIDR (`hostPrefixLength`), so IPAM is local to the host and the zone does not
hit a single-subnet address limit.

## Scheduling (v1)

Filter on zone, cordon state, capacity and labels. Spread across hosts, with
zone spread as a soft preference when a claim says `zone: any`.

## Proposed decisions (v1, overturn freely)

These close the storage question so the rest of the design can move. They are
recommendations, not settled choices.

1. **Local disks only.** Every zone is `storage.mode: Local` in v1. A VM is
   pinned to its host. `Shared` stays in the CRD schema so the API does not
   break later, but the controller rejects it as unsupported until it is built.
2. **No live migration.** Recovery means recreating the VM from its pool or
   image on a healthy host in the zone, not moving the running VM. This fits
   ephemeral sandbox and per-identity VMs, where the VM is disposable and state
   lives elsewhere.
3. **No fencing in v1.** Without shared disks, a host that is wrongly presumed
   dead cannot corrupt a disk another host is using. The controller still marks
   the host `Degraded` on missed agent heartbeats and stops scheduling to it.
   Fencing returns as a requirement only if `Shared` is built.

What this costs: a host failure loses the VMs on it, and any workload that is
not disposable (the algo trader VM is the likely case) needs its data on
storage outside the VM, or a later `Shared` zone. Decide that per workload
before putting it on this platform.

What it saves: no storage backend to operate, no migration network, no fencing
hardware, and the scheduler stays a simple filter and spread.

## Open questions

1. What is the banlieue CLI interface for bootstrapping a host: command,
   inputs, exit codes, is it idempotent, does it need to run from inside the
   cluster or from a workstation?
2. What does the banlieue provider interface look like, and does it model zones
   natively or as one provider instance per zone?
3. How does the controller reach hosts for SSH (network path from the cluster)?
4. Does the algo trader VM belong on this platform in v1, given decision 1?
5. Language and repo location (assumed Rust, under firestoned).
6. Project name, see README.

## Milestones

1. CRDs and Host controller that bootstraps one host via the banlieue CLI.
2. Host agent: create, boot, stop, delete one VM through the CH API.
3. Zone-aware scheduler and the banlieue provider.
4. Networking slices and egress integration.
5. Shared-storage zones, live migration and fencing, only if a workload needs
   them.
