<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0002: banlieue's host-resident provider is the host agent; faubourg owns inventory, bootstrap and zones

**Status:** Accepted · **Date:** 2026-11-07

## Context

The design sketch (`docs/design-sketch.md`) proposes a faubourg host agent that drives the
Cloud Hypervisor API, plus a faubourg scheduler, exposed to banlieue as one provider.
banlieue has since settled most of that ground:

- **banlieue ADR-0060:** Cloud Hypervisor is a first-class banlieue provider class, and the
  provider is **host-resident**: `banlieue provider cloud-hypervisor` runs on each KVM host
  as a systemd service, outbound-only to the Kubernetes API, opening no listening socket.
  **One `Provider` is one host.** It rejects both a first-party host daemon with an mTLS API
  and SSH exec from an in-cluster provider.
- **banlieue ADR-0063:** that provider supervises one `cloud-hypervisor` process per guest
  through root-owned systemd template units and a polkit rule scoped to a guest uid range.
- **banlieue ADR-0067 and ADR-0084:** `banlieue host cloud-hypervisor install` (alias `ch`)
  prepares a host from the one binary, in fixed stages, with `preflight`, `status`,
  `selftest` and `--dry-run`, every download verified against a sha256.
- **banlieue ADR-0098, ADR-0102, ADR-0107:** the banlieue scheduler already spreads across
  failure domains, refuses providers whose Lease has gone stale, and filters on published
  capacity. Each host's `Provider` publishes one failure domain, named after the `Provider`
  and carrying its labels.

A faubourg agent and scheduler would duplicate all of this and reopen security decisions
banlieue has already verified on hosts.

## Decision

1. faubourg **ships no host agent**. The per-host agent is banlieue's host-resident Cloud
   Hypervisor provider, at the version the bootstrap installs.
2. faubourg **ships no VM scheduler**. Placement stays in banlieue. faubourg shapes it
   only through what banlieue already reads: the labels on each host's `Provider`, which
   become its failure domain's labels (ADR-0003), and whether that `Provider` exists.
3. faubourg **owns** what banlieue has no object for: an inventory of hosts, the grouping
   of hosts into availability zones, the remote bootstrap of a new host (ADR-0005, ADR-0006),
   and zone-level health (ADR-0007). In vCenter terms banlieue is the per-host ESXi agent and
   the DRS placement, and faubourg is the inventory, the cluster objects and "add host".
4. faubourg is a separate repository and consumes `banlieue-api` as a pinned git
   dependency for the `Provider` type, as mediatore does (mediatore ADR-0002). banlieue never
   imports faubourg.

## Consequences

- faubourg's attack surface is a controller and a bootstrap Job, not a privileged daemon
  on every hypervisor.
- The contract with banlieue is CRDs only: faubourg writes `Provider` objects and reads
  their status and Leases. Schema drift is caught at compile time by the pin.
- faubourg's kube and k8s-openapi versions follow banlieue-api's (kube ~4.2, k8s-openapi
  0.28 today).
- Anything faubourg wants from a host that the banlieue provider does not publish is a
  banlieue change first.
- Supersedes the design sketch's "host agent", "scheduler" and milestone 2 and 3.
