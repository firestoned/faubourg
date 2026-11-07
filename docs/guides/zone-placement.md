<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# Placing VMs in a zone

!!! note "For banlieue users"

    Nothing here is a faubourg API. You keep writing banlieue `VirtualMachine`s and
    `VirtualMachinePool`s; faubourg only puts the labels on the `Provider`s you select by
    ([ADR-0003](../adr/0003-availabilityzone-and-host-crds.md)). The labels appear once the
    host controller lands (roadmap 01).

## Pin a VM to a zone

Select on the zone label with banlieue's ordinary `providerSelector`:

```yaml
apiVersion: banlieue.io/v1alpha1
kind: VirtualMachine
metadata:
  name: build-01
spec:
  # ... class, image, and so on, as in the banlieue examples
  placement:
    providerSelector:
      matchLabels:
        faubourg.firestoned.io/zone: az1
```

banlieue then places the VM on one of the zone's hosts and spreads across their failure domains
as it always does. See banlieue's
[examples](https://github.com/firestoned/banlieue/tree/main/examples) for complete
`VirtualMachine` and `VirtualMachinePool` manifests.

## Select on the zone's own labels

A zone's `providerLabels` land on every member host's `Provider` too, so you can select on
whatever topology you describe there:

```yaml
apiVersion: faubourg.firestoned.io/v1alpha1
kind: AvailabilityZone
metadata:
  name: az1
spec:
  storage:
    mode: Local
  providerLabels:
    topology.example.com/rack: a
```

```yaml
  placement:
    providerSelector:
      matchLabels:
        topology.example.com/rack: a
```

## Pin to one host

`faubourg.firestoned.io/host: <host>` selects a single host's `Provider`. Because v1 storage is
local, a VM never moves off the host it lands on, so pinning is rarely needed.

## Cordoned hosts

A cordoned host's `Provider` carries `faubourg.firestoned.io/cordoned: "true"`. banlieue does not
exclude it yet, so to keep new VMs off cordoned hosts today, add a `matchExpressions` term:

```yaml
  placement:
    providerSelector:
      matchLabels:
        faubourg.firestoned.io/zone: az1
      matchExpressions:
        - key: faubourg.firestoned.io/cordoned
          operator: DoesNotExist
```

This is the convention roadmap 02 will propose to banlieue (accepted risk R-4 in the
[threat model](../security/threat-model.md)).
