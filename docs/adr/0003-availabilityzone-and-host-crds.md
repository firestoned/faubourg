<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0003: AvailabilityZone and Host CRDs; zone membership is a label on the host's Provider

**Status:** Accepted · **Date:** 2026-11-07

## Context

ADR-0002 leaves faubourg two objects to own: hosts and zones. banlieue's scheduler selects
providers by label and spreads across failure domains, and each Cloud Hypervisor `Provider`
copies its own labels onto the one failure domain it publishes. So a zone needs no new
scheduler input if every host's `Provider` carries the zone as a label.

## Decision

1. Group `faubourg.firestoned.io`, version `v1alpha1`. Both kinds are **cluster-scoped**,
   like `Node`: a hypervisor belongs to the platform, not to a tenant namespace.
2. **`AvailabilityZone`**: a failure domain and shared-fate boundary for storage and
   network. Spec: `storage.mode` (`Local` or `Shared`, ADR-0004), optional `description`,
   optional `providerLabels` copied onto every member host's `Provider`. Status: `hosts`,
   `readyHosts`, `conditions`, `observedGeneration`.
3. **`Host`**: one physical machine. Spec: `zoneRef`, `address`, `ssh` (ADR-0006),
   `bootstrap` (ADR-0005), `cordoned`. Status: `phase`, `providerRef`, `acceptedHostKey`,
   `conditions`, `observedGeneration`, `message`.
4. **The zone label.** faubourg creates the host's banlieue `Provider` (class
   `cloud-hypervisor`) and owns, by its own field manager, exactly these labels on it:
   `faubourg.firestoned.io/zone: <zone>`, `faubourg.firestoned.io/host: <host>`, and
   the zone's `providerLabels`. A VM, pool or claim targets a zone through banlieue's
   existing `providerSelector` on that label.
5. **Cordon.** `spec.cordoned: true` (on a Host, or every Host of a zone) makes faubourg
   label the `Provider` `faubourg.firestoned.io/cordoned: "true"`. Excluding cordoned
   hosts from placement needs a banlieue selector convention or a banlieue change; until
   then cordon is advisory, and the condition says so.
6. The Rust types in `faubourg-api` are the source of truth. `make crds` writes
   `deploy/crds/`, and a test fails when that output is stale.

## Consequences

- No new banlieue API is needed for zone placement in v1.
- The group is also the label prefix, so it is part of the contract with banlieue users'
  selectors; changing it after the first release is a breaking change.
- Open: whether banlieue should grow a first-class zone attribute on failure domains instead
  of a label convention.
