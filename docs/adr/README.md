<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# Architecture Decision Records

| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | Accepted |
| [0002](0002-banlieue-provider-is-the-host-agent.md) | banlieue's host-resident provider is the host agent | Accepted |
| [0003](0003-availabilityzone-and-host-crds.md) | AvailabilityZone and Host CRDs; zone label on the Provider | Accepted |
| [0004](0004-v1-local-storage-only.md) | v1 zones use local storage only | Accepted |
| [0005](0005-bootstrap-runs-one-scoped-install-script.md) | Bootstrap runs one install script under one scoped sudo rule | Accepted |
| [0006](0006-ssh-credentials.md) | SSH credentials | Accepted |
| [0007](0007-health-from-the-banlieue-provider.md) | Health from the banlieue Provider | Accepted |
| [0008](0008-bootstrap-jobs-constrained-by-admission-policy.md) | Bootstrap Jobs are constrained by a ValidatingAdmissionPolicy | Accepted |
| [0009](0009-host-reconciler.md) | The Host reconciler | Accepted |
| [0010](0010-bootstrap-image.md) | The bootstrap image | Accepted |
| [0011](0011-the-hosts-provider-and-its-identity.md) | The host's Provider and its identity | Accepted |
| [0012](0012-images-signed-and-published-by-digest.md) | Images are signed, published and deployed by digest | Accepted |
| [0013](0013-script-content-pinned-and-rolled-out.md) | The install script's content is pinned by digest, and a change rolls out one host at a time | Accepted |
| [0014](0014-controller-leader-election.md) | The controller elects a leader through a Lease | Accepted |
| [0015](0015-zone-controller.md) | The zone controller reports each zone's health from its hosts | Accepted |
