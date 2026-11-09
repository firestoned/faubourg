<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0015: The zone controller reports each zone's health from its hosts

**Status:** Accepted · **Date:** 2026-11-08

## Context

ADR-0003 gave an `AvailabilityZone` a status (`hosts`, `readyHosts`, `conditions`,
`observedGeneration`) and ADR-0004 one of its reasons: a `Shared` zone is `Ready=False`,
`StorageModeUnsupported`. Nothing writes that status yet. The Host controller reads a zone (a
Host waits while its zone is missing or refused, ADR-0009), but an operator asking "can VMs land
in az1 now?" has to count Hosts by hand, and cannot see that every ready host in a zone is
cordoned.

A zone's health is not something faubourg decides on its own. Each host's health comes from its
banlieue `Provider` (ADR-0007), and banlieue places VMs by Provider labels (ADR-0002, ADR-0003).
A zone is the sum of its hosts, and its status should say exactly that, nothing more.

## Decision

1. **A zone controller, in the same binary, under the same leader.** It runs beside the Host
   controller in `faubourg-controller` and only while that replica holds the Lease (ADR-0014).
   It reconciles every `AvailabilityZone`, and again whenever a Host naming it changes.
2. **It writes status only.** By server-side apply under its own field manager,
   `faubourg.firestoned.io/zone-controller`. It writes no spec, no label, no Provider and no Job,
   and its RBAC is what the controller already holds: read Hosts and zones, patch
   `availabilityzones/status`.
3. **The counts.** `hosts`: Hosts whose `zoneRef` names the zone. `readyHosts`: of those, Hosts
   in phase `Ready` (ADR-0003). `cordonedHosts`, new: of the ready ones, those cordoned. A
   cordoned host is still healthy, but banlieue should not place new VMs on it (ADR-0003
   decision 5).
4. **`Ready` and its reasons**, the first that applies:

   | Reason | `Ready` | When |
   | --- | --- | --- |
   | `StorageModeUnsupported` | False | `storage.mode: Shared` (ADR-0004) |
   | `InvalidSpec` | False | any other validation failure |
   | `NoHosts` | False | no Host names the zone |
   | `NoReadyHost` | False | no member Host is `Ready` |
   | `AllReadyHostsCordoned` | False | every `Ready` member is cordoned |
   | `HostsReady` | True | at least one member is `Ready` and not cordoned |

   There is no threshold in v1. A zone with one usable host is `Ready`; a minimum, if one is
   needed, is a later field.
5. **The decision is a pure function** of the zone and its Hosts, unit-tested for every row of
   the table; the reconciler around it only reads and writes (ADR-0009 decision 2).
6. **Nothing reads the zone's `Ready` to decide anything.** The Host controller keeps gating on
   the zone's existence and validity (ADR-0009), not on its health: a zone with no ready host
   must still be able to bootstrap its first one.

## Alternatives considered

- **A threshold (`minReadyHosts`) now.** It would turn "can anything land here" into a policy
  question with no user asking it yet. Deferred to a field when one does.
- **Count cordoned hosts as not ready.** It would hide a healthy host from `readyHosts`, which
  ADR-0003 defines by phase. A separate `cordonedHosts` keeps both facts.
- **Health from the Providers directly.** The Host's phase already folds in its Provider's
  health and faubourg's own state (a bootstrap in progress, a refused spec); reading Providers
  again would be a second, divergent opinion.

## Consequences

- `AvailabilityZone.status` gains `cordonedHosts`; the CRD changes, additively.
- `kubectl get availabilityzones` already shows hosts and ready hosts; it gains a `Cordoned`
  column, and the counts stop being empty.
- A zone's status can lag a Host's change by one reconcile. It is a report, not a gate
  (decision 6), so a lag misleads no decision faubourg makes.
- Cordon stays advisory for banlieue (R-4). The zone status makes it visible; a VM excludes
  cordoned hosts with `faubourg.firestoned.io/cordoned DoesNotExist` in its `providerSelector`,
  which banlieue's scheduler honours today.
