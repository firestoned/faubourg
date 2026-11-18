<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0007: Host and zone health come from the host's banlieue Provider

**Status:** Accepted · **Date:** 2026-11-07

## Context

The design sketch has a faubourg agent heartbeat. With no faubourg agent (ADR-0002),
the signal already exists: every banlieue provider renews a Lease every 5 seconds, and
banlieue's operator records `status.workload.heartbeat` as `Alive`, `Stale` or `Absent`
(banlieue ADR-0102). The `Provider`'s own `Ready` condition says whether the host is usable.

## Decision

1. A bootstrapped Host is `Ready` while its `Provider` is `Ready=True` and its heartbeat is
   `Alive`; `Degraded` when either stops holding; `Bootstrapping` while its Job runs;
   `Failed` when the Job fails; `Pending` before that. faubourg adds no heartbeat of its
   own.
2. An `AvailabilityZone` publishes `hosts`, `readyHosts`, and `Ready=True` when at least one
   member host is `Ready` and its storage mode is supported.
3. faubourg never deletes a `Provider` because its host is `Degraded`; banlieue already
   stops placing on a stale provider.

## Consequences

- One health signal for banlieue and faubourg, so they cannot disagree.
- faubourg needs `get` and `watch` on `Provider` and `get` on Leases in banlieue's
  workload namespace.

*Amended 2026-11-07 by ADR-0009 (decision 12):* banlieue's operator writes the heartbeat into
the Provider's status, so faubourg reads Providers only (`get`, `list`, `watch`) and needs
nothing on Leases.

*Amended 2026-11-08 by ADR-0015:* decision 2's zone `Ready` counts only hosts that are `Ready`
and not cordoned (a zone whose every ready host is cordoned is `AllReadyHostsCordoned`), and
the status gains `cordonedHosts`. The zone controller writes it.

*Amended 2026-11-18 by ADR-0018 (decision 8):* a live Lease means a live host only for an
`External` Provider. A `Managed` (libvirt) Provider's Lease is renewed in the cluster, so its
Host is `Ready` on `Ready=True` and `ProviderReachable=True` for the Provider's current
generation, and `Degraded` when either is not.
