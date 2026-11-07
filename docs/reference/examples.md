<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# Examples

Everything below lives in
[`examples/`](https://github.com/firestoned/faubourg/tree/main/examples). All host names and keys
are placeholders.

| File | What it is |
| --- | --- |
| [`00-availabilityzone.yaml`](https://github.com/firestoned/faubourg/blob/main/examples/00-availabilityzone.yaml) | A local-storage zone with one extra provider label. |
| [`01-host.yaml`](https://github.com/firestoned/faubourg/blob/main/examples/01-host.yaml) | A Host with a scoped run Secret, a placement Secret, strict host keys and `revokeAfterBootstrap`. |
| [`02-install-script-configmap.md`](https://github.com/firestoned/faubourg/blob/main/examples/02-install-script-configmap.md) | How to publish the install script as a ConfigMap. |
| [`host/install.sh`](https://github.com/firestoned/faubourg/blob/main/examples/host/install.sh) | The default install script. |
| [`host/sudoers-faubourg`](https://github.com/firestoned/faubourg/blob/main/examples/host/sudoers-faubourg) | The one sudo rule. |

## A zone

```yaml
apiVersion: faubourg.firestoned.io/v1alpha1
kind: AvailabilityZone
metadata:
  name: az1
spec:
  description: Rack A, local NVMe
  storage:
    mode: Local
  providerLabels:
    topology.example.com/rack: a
```

## A host

```yaml
apiVersion: faubourg.firestoned.io/v1alpha1
kind: Host
metadata:
  name: hv01
spec:
  zoneRef: az1
  address: hv01.home.example
  ssh:
    secretRef:
      name: hv01-ssh
    placementSecretRef:
      name: hv01-ssh-placement
    hostKeyPolicy: Strict
    revokeAfterBootstrap: true
  bootstrap:
    method: Script
    scriptRef:
      kind: ConfigMap
      name: faubourg-install
      key: install.sh
```

## Installing the CRDs

```sh
kubectl apply -f deploy/crds/
```
