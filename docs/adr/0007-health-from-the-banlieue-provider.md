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
