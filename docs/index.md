<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# faubourg

faubourg groups bare-metal [Cloud Hypervisor](https://www.cloudhypervisor.org/) hosts into
**availability zones** for [banlieue](https://github.com/firestoned/banlieue), roughly the way
vCenter groups ESXi hosts into clusters. It also bootstraps new hosts: one SSH session runs one
root-owned install script.

banlieue already runs an agent on each hypervisor (its host-resident Cloud Hypervisor
provider) and already places VMs across failure domains. faubourg adds only what banlieue has
no object for ([ADR-0002](adr/0002-banlieue-provider-is-the-host-agent.md)):

| faubourg owns | banlieue owns |
| --- | --- |
| The inventory of `Host`s and their `AvailabilityZone`s | The per-host agent (`banlieue provider cloud-hypervisor`) |
| Bootstrap of a new host over SSH ([ADR-0005](adr/0005-bootstrap-runs-one-scoped-install-script.md)) | The host installer (`banlieue host cloud-hypervisor install`) |
| The zone label on each host's `Provider` ([ADR-0003](adr/0003-availabilityzone-and-host-crds.md)) | VM placement, spread across failure domains |
| Zone health, rolled up from providers ([ADR-0007](adr/0007-health-from-the-banlieue-provider.md)) | Provider heartbeats and `Ready` |

## Where to start

- **New to the project?** Read [Concepts](concepts.md), then the
  [system diagram](architecture/diagrams/system.md).
- **Adding a host?** [Preparing a host](guides/preparing-a-host.md) and
  [SSH credentials](guides/ssh-credentials.md).
- **Running VMs in a zone?** [Placing VMs in a zone](guides/zone-placement.md).
- **Reviewing the design?** The [ADRs](adr/README.md) are the source of truth, and the
  [threat model](security/threat-model.md) says what is controlled and what is not yet.
- **Contributing?** [Development](development.md) covers the ADD cycle and the quality gate.

## Status

faubourg is pre-1.0. The architecture (ADR-0001 to ADR-0007) is accepted, and the API crate
(the CRD types, their fail-closed validation and the SSH Secret contract) is implemented and
tested. The controllers come next.

!!! warning "Do not deploy yet"

    The host controller and the bootstrap Job are being built
    ([roadmap 01](https://github.com/firestoned/faubourg/blob/main/.github/community/01-host-controller-and-bootstrap.md)).
    Until they land and their threat-model pass closes accepted risk R-3, several controls
    in the [threat model](security/threat-model.md) are still planned, not built.

The status board is
[`ROADMAPS.md`](https://github.com/firestoned/faubourg/blob/main/ROADMAPS.md).

## Related projects

- [banlieue](https://github.com/firestoned/banlieue) ([docs](https://firestoned.github.io/banlieue/)):
  the Kubernetes-native VM API faubourg serves.
- [mediatore](https://github.com/firestoned/mediatore) ([docs](https://firestoned.github.io/mediatore/)):
  the identity broker for banlieue sandbox VMs. faubourg follows its project layout.
- [FINOS CALM](https://calm.finos.org/): the architecture-as-code format behind the diagrams.
