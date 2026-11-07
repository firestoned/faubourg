<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0004: v1 zones use local storage only: no live migration, no fencing

**Status:** Accepted · **Date:** 2026-11-07

## Context

A zone's storage model decides what the platform can promise. Shared storage allows restart
elsewhere and live migration, but requires fencing before a VM restarts on another host, a
storage backend to operate and a migration network. The first consumers are ephemeral
sandboxes and per-identity VMs, where the VM is disposable and state lives elsewhere.
banlieue ADR-0036 phases live migration separately.

## Decision

1. Every zone is `storage.mode: Local` in v1. A VM is pinned to its host.
2. `Shared` stays in the CRD schema so the API does not break later, but faubourg
   **rejects** it: the zone gets `Ready=False`, reason `StorageModeUnsupported`, and its
   hosts are not bootstrapped. Fail closed, with a test proving the refusal.
3. No live migration. Recovery is recreating the VM from its pool or image on a healthy
   host, which banlieue already does for pool members.
4. No fencing. Without shared disks, a host wrongly presumed dead cannot corrupt a disk
   another host is using. A host that misses heartbeats is marked `Degraded` (ADR-0007).

## Consequences

- A host failure loses the VMs on it. A workload that is not disposable (the algo trader VM
  is the likely case) needs its data outside the VM, or waits for a `Shared` zone. Decide
  per workload before putting it on this platform.
- No storage backend, migration network or fencing hardware to operate in v1.
- `Shared` returns as its own ADR, with fencing as a requirement.
