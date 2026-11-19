<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# API reference

Group `faubourg.firestoned.io`, version `v1alpha1`. Both kinds are cluster-scoped. The Rust types in
[`crates/faubourg-api`](https://github.com/firestoned/faubourg/tree/main/crates/faubourg-api)
are the source of truth. `make crds` writes the CRDs into
[`deploy/crds/`](https://github.com/firestoned/faubourg/tree/main/deploy/crds), and a test
fails when that output is stale.

Validation fails closed: every refused field is reported, with the condition reason in the
tables below.

## AvailabilityZone

`kubectl get az` shows `Class`, `Storage`, `Hosts`, `Ready` and `Cordoned`.

### spec

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `description` | string | | Free-form, for operators. |
| `storage.mode` | `Local` \| `Shared` | `Local` | `Shared` is refused in v1 (`StorageModeUnsupported`, [ADR-0004](../adr/0004-v1-local-storage-only.md)). |
| `providerLabels` | map | | Copied onto every member host's `Provider`. Keys under `faubourg.firestoned.io/` are refused (`ReservedLabel`); keys and values must be valid labels (`InvalidLabel`). |
| `providerClassRef.name` | string | the controller's `--provider-class` | The banlieue `ProviderClass` every member host's `Provider` references; its `backend` decides how faubourg drives the zone's hosts ([ADR-0018](../adr/0018-a-zone-names-its-provider-class.md)). Must be an object name (`InvalidProviderClassRef`). The API server refuses a change or a removal once set; setting it on a zone that named none is allowed. Driven today: `cloud-hypervisor` (`External`) and `libvirt` (`Managed`). Any other backend (`firecracker` until banlieue ships it), a mode that does not match, or a missing class is `ProviderBackendUnsupported`. |
| `libvirt.credentialsRef.name` | string | | Required for a `libvirt` zone, refused on any other (`ProviderConnectionInvalid`). A Secret in banlieue's namespace holding the client certificate (`tls.crt`, `tls.key`) libvirtd accepts, named `faubourg-...` (`InvalidLibvirtRef`). faubourg copies the name into each member's `Provider` and never reads it. |
| `libvirt.caBundleRef.name`, `.key` | string | key `ca.crt` | The ConfigMap key, in banlieue's namespace, holding the CA that signed each host's libvirtd certificate. The name starts with `faubourg-` (`InvalidLibvirtRef`). |

### status

| Field | Notes |
| --- | --- |
| `hosts` | Hosts whose `zoneRef` names this zone. |
| `readyHosts` | Of those, hosts in phase `Ready`. |
| `cordonedHosts` | Of the ready ones, those cordoned. |
| `conditions` | `Ready`, and why: `StorageModeUnsupported`, `InvalidSpec`, `ProviderBackendUnsupported`, `ProviderConnectionInvalid`, `NoHosts`, `NoReadyHost`, `AllReadyHostsCordoned` or `HostsReady` ([ADR-0015](../adr/0015-zone-controller.md)). |
| `observedGeneration` | The generation the conditions describe. |

## Host

`kubectl get hosts` shows `Zone`, `Address`, `Phase` and `Cordoned`.

A Host's name is at most 63 characters: it becomes a label value on its `Provider` and its
bootstrap Job. The CRD refuses a longer one on create.

### spec

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `zoneRef` | string | required | An `AvailabilityZone` name (`InvalidName`). |
| `address` | string | required | An IP address or a lowercase DNS name (`InvalidAddress`). Neither can start with `-`, so an address is never read as an SSH option. |
| `ssh` | object | | Required unless `bootstrap.method` is `None` (`SshRequired`). |
| `ssh.port` | integer | `22` | Port 0 is refused (`InvalidPort`). |
| `ssh.secretRef.name` | string | required | The scoped run credential, in the credentials namespace ([SSH credentials](../guides/ssh-credentials.md)). |
| `ssh.placementSecretRef.name` | string | | The privileged placement credential. Must differ from `secretRef` (`SharedCredential`). |
| `ssh.hostKeyPolicy` | `Strict` \| `TrustOnFirstUse` | `Strict` | |
| `ssh.revokeAfterBootstrap` | bool | `false` | Remove the placement key once placement succeeds. |
| `bootstrap.method` | `Script` \| `None` | `Script` | `None` adopts a host that already runs banlieue's provider. |
| `bootstrap.scriptRef.kind` | `ConfigMap` \| `Secret` | | Where the install script lives, in the credentials namespace. It must be on the admission policy's `scriptSources` allowlist ([ADR-0008](../adr/0008-bootstrap-jobs-constrained-by-admission-policy.md)). |
| `bootstrap.scriptRef.name` | string | | Object name (`InvalidName`). |
| `bootstrap.scriptRef.key` | string | `install.sh` | `[A-Za-z0-9._-]`, not `.` or `..` (`InvalidKey`). |
| `cordoned` | bool | `false` | Label the `Provider` cordoned (advisory today). |

The Host API carries no free-form arguments and no shell: the script, not a CR field, is the
escape hatch ([ADR-0005](../adr/0005-bootstrap-runs-one-scoped-install-script.md)).

### status

| Field | Notes |
| --- | --- |
| `phase` | `Pending`, `Bootstrapping`, `Ready`, `Degraded` or `Failed` ([Concepts](../concepts.md#lifecycle-of-a-host)). |
| `providerRef` | The banlieue `Provider` labelled `faubourg.firestoned.io/host: <host>`, when exactly one is. For a bootstrapped host, faubourg creates it, named after the Host. |
| `acceptedHostKey` | The fingerprint accepted under `TrustOnFirstUse`, pinned afterwards. |
| `conditions` | `Ready`: `True` only in phase `Ready`. Its reason says why: `AwaitingBootstrapJob`, `BootstrapJobRunning`, `BootstrapJobFailed`, `ScriptDrift`, `HostKeyRefused`, `ZoneNotFound`, `ZoneUnsupported`, `NoScriptSource`, `InvalidSpec`, `DuplicateAddress`, `ProviderBackendUnsupported`, `ProviderConnectionInvalid`, `ProviderClassMismatch`, `AwaitingProviderIdentity`, `AwaitingProvider`, `ProviderAmbiguous`, `ProviderMissing`, `ProviderNotReady`, `HeartbeatNotAlive`, `ProviderUnreachable` (a libvirt host whose provider does not report reaching it) or `ProviderHealthy`. |
| `observedGeneration` | The generation this status describes. |
| `message` | Human-readable. Never contains secret material or a command line. |

## Labels faubourg owns on a Provider

| Label | Value |
| --- | --- |
| `faubourg.firestoned.io/zone` | the zone name |
| `faubourg.firestoned.io/host` | the Host name |
| `faubourg.firestoned.io/cordoned` | `"true"` while cordoned |

The group is also the label prefix, so it is part of the contract with banlieue users'
selectors. Changing it after the first release is a breaking change.
