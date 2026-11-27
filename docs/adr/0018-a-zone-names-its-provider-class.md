<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0018: A zone names its provider class; faubourg drives Cloud Hypervisor, Firecracker and libvirt hosts

**Status:** Accepted · **Date:** 2026-11-18

## Context

faubourg was built for one banlieue provider class. The class is a single controller flag
(`--provider-class`, default `cloud-hypervisor`), which must be `External` (ADR-0011 decision
1). A Provider's endpoint is always `ch://<address>`. The first credential always goes to the
host as a ServiceAccount token on the install script's stdin (ADR-0011 decisions 6 to 9). The
default install script always runs `banlieue host cloud-hypervisor install`.

The maintainer's direction (2026-11-18): group bare-metal hosts of more than one kind into
zones, with each host still one banlieue failure domain. The kinds are:

- **Cloud Hypervisor**, as today.
- **Firecracker**, which banlieue is building (its roadmap 20). It is host-resident and
  `External` like Cloud Hypervisor, outbound-only, with a bound token. It is a separate class
  with its own InfraMachine, and in banlieue v1 a host is either a Cloud Hypervisor host or a
  Firecracker host, never both.
- **libvirt**. banlieue's libvirt provider is `Managed`: a Deployment in the cluster dials
  `qemu+tls://<host>/system` with an x509 client certificate held in a Secret, and trusts
  libvirtd's server certificate through a CA bundle (banlieue ADR-0011). One libvirt
  `Provider` is one host and publishes one failure domain, named after the Provider and
  carrying its labels.

All three keep the shape faubourg is built on: **one Host, one Provider, one failure domain**,
so zone membership stays a Provider label (ADR-0003 decision 4) and banlieue's scheduler
needs nothing new. Proxmox and vSphere do not keep it: one Provider publishes a failure domain
per node or per cluster, while labels and `banlieue.io/unschedulable` are per Provider. They
are out of scope until banlieue has per-failure-domain labels and cordon.

banlieue already says what kind of backend a class is: `ProviderClass.spec.backend` (for
example `cloud-hypervisor`, `libvirt`) and `spec.deployment` (`Managed` or `External`).

## Decision

1. **A zone names one ProviderClass.** `AvailabilityZone.spec.providerClassRef.name` names a
   ProviderClass. Every member Host's Provider references that class. A zone is homogeneous:
   a drain moves VMs between interchangeable failure domains (ADR-0017), and `HostsMatch`
   compares like with like. The field is optional. Omitted, it is the controller's
   `--provider-class`, so a zone written before this ADR keeps its meaning.
2. **The class's backend picks a driver, from a closed set.** faubourg reads the ProviderClass
   and selects a driver by `spec.backend`:

   | `backend` | `deployment` required | Provider endpoint | First credential |
   | --- | --- | --- | --- |
   | `cloud-hypervisor` | `External` | `ch://<address>` | token minted by the bootstrap pod, on the script's stdin (ADR-0011) |
   | `firecracker` | `External` | the scheme banlieue's Firecracker provider settles | as `cloud-hypervisor` |
   | `libvirt` | `Managed` | `qemu+tls://<address>/system` | a client certificate the operator provides (decision 6) |

   Any other backend, or a `deployment` that does not match the table, fails closed. The zone
   gets `Ready=False`, reason `ProviderBackendUnsupported`. Its Hosts get `Failed` with the
   same reason, and no Provider and no Job. This replaces ADR-0011's
   `ProviderClassNotExternal`, which becomes the `External` case of the same check.
3. **A zone's class is immutable once set.** The CRD refuses a change (`self == oldSelf`).
   banlieue owns a Provider's workload and its identity by class, so moving a Provider between
   classes is not a relabel. A Host moved into a zone whose class differs from its existing
   Provider's is `Failed`, reason `ProviderClassMismatch`. faubourg does not rewrite the
   Provider and never deletes one (ADR-0011 decision 4).
4. **What stays the same for every backend:** the Host API (`address`, `ssh`, `bootstrap`,
   `cordoned`, `maintenance`), the Job and its admission policy (ADR-0008), the one install
   script under one sudo rule (ADR-0005), the labels faubourg owns on the Provider
   (ADR-0003, ADR-0017 decision 10), the two field managers (ADR-0011 amendment of
   2026-11-08), and adoption with `bootstrap.method: None`.
5. **External backends (`cloud-hypervisor`, `firecracker`) keep ADR-0011 as written.** The
   token policy's single `providerServiceAccountPrefix` becomes a list,
   `providerServiceAccountPrefixes`, one `banlieue-provider-<class>-` entry per External class
   the site uses. The bootstrap identity still cannot mint a token for banlieue's operator,
   or for a class the site did not list.
6. **libvirt: faubourg references a client credential and never reads it.** The zone gains
   `spec.libvirt.credentialsRef` (a Secret holding `tls.crt` and `tls.key`) and
   `spec.libvirt.caBundleRef` (a ConfigMap holding libvirtd's server CA). Both are in
   banlieue's namespace, created by the operator, and copied into each member's Provider
   `spec.connection`. faubourg's controller gains no access to Secrets anywhere. A
   ValidatingAdmissionPolicy, `faubourg-provider-connection`, matches Provider writes by the
   controller's ServiceAccount. It admits a `credentialsRef` only when its name starts with
   `faubourg-`. A compromised controller can point a libvirt Provider at a credential an
   operator named for faubourg, never at another Provider's.
7. **libvirt bootstrap delivers no credential.** The Job for a libvirt Host has no `kube-api`
   volume and mints no token. The default libvirt install script installs libvirtd and
   switches it to TLS on 16514, with the server certificate the script carries (a script
   sourced from a Secret, ADR-0005 decision 1) or one the host already has. It reads nothing
   on stdin. Success is still the Provider coming up (ADR-0005 decision 6).
8. **Health by deployment mode** (amends ADR-0007). An `External` Provider's Lease is renewed
   on the host, so a live Lease and `Ready=True` mean the host is up, as today. A `Managed`
   Provider's Lease is renewed by a pod in the cluster, so it says nothing about the host:
   a libvirt Host is `Ready` on `Ready=True` and `ProviderReachable=True` for the Provider's
   current generation, and `Degraded` when either is not.
9. **Drain is by backend** (amends ADR-0017). `maintenance: Drain` asks banlieue for moves only
   for a backend banlieue can move (today `cloud-hypervisor`, banlieue ADR-0127). On any other
   it cordons the host and reports `MovesUnsupported`, as ADR-0017 does when the move API is
   absent.
10. **One machine, one Host.** Two Hosts with the same `address` are both `Failed`, reason
    `DuplicateAddress`. The comparison is on the literal `spec.address`, case-folded, with no
    name resolution: a name and an IP for one machine are not caught, and the docs say so. Two Providers on one machine report the same CPU and memory to
    banlieue's capacity-aware scheduler, which then double-counts it (banlieue roadmap 20,
    "One VMM per host"). A machine in two classes is a later ADR, after banlieue's.
11. **Firecracker waits for banlieue.** Its row in decision 2 is filled in from banlieue's
    roadmap 20 (endpoint scheme, `banlieue host firecracker install`, the ServiceAccount
    prefix). Until a banlieue release ships the class, a zone whose class has backend
    `firecracker` is `ProviderBackendUnsupported`, so nothing half-works.

## Consequences

- Amends ADR-0002 decision 1: the per-host agent is banlieue's host-resident provider for
  External backends. For libvirt it is libvirtd, driven by banlieue's in-cluster provider.
  faubourg still ships no agent and no scheduler.
- Amends ADR-0011 decisions 1 and 8 (class per zone; a list of prefixes), ADR-0005 decision 1
  (a default script per backend), ADR-0007 (decision 8) and ADR-0017 (decision 9).
- `--provider-class` becomes the default for zones that name none. It can be removed once no
  zone relies on it, as a breaking change before 1.0.
- New trust: faubourg's controller writes `credentialsRef` on libvirt Providers, and banlieue's
  operator grants that Provider's workload `get` on the named Secret. That is a confused-deputy
  path, bounded by decision 6's admission policy. The threat-model pass covers it, with the
  libvirt provider's network path from the cluster to the hosts' 16514.
- The libvirt client certificate is one per zone, shared by its hosts' Providers. Revoking it
  means rotating it for the whole zone. A per-Host credential is possible later, as an
  optional Host field under the same prefix rule.
- `HostsMatch` reads `raw.vmm` and `raw.cpuModel`. A backend that does not publish them is
  reported `HostsUnreported`, which is advisory.
- A new backend means a new driver and a new ADR, not a configuration value.

## Resolved questions

Settled by the maintainer on 2026-11-18, when the ADR was accepted:

- **The libvirt client credential is per zone** (decision 6), shared by the zone's hosts'
  Providers. A per-Host credential stays possible later, as an optional Host field under the
  same prefix rule.
- **`DuplicateAddress` compares the literal address** (decision 10), not a resolved one: a
  reconcile that resolves names would depend on DNS at the time it runs.
- Whether banlieue should publish a Provider's backend kind in its status, so faubourg need not
  read the ProviderClass, is left to banlieue. faubourg reads the class until it does.

*Amended 2026-11-19, after the first live test of a libvirt zone:* banlieue's own admission
policy, `banlieue-provider-credentialsref-authorization` (its security review CHAIN-001), refuses
any create or update of a Provider that names a credential unless the writer may `get` that
Secret. faubourg's controller reads no Secret (ADR-0008, decision 6 above), so on a real banlieue
it can neither create nor label a libvirt Provider; the kind tests had not installed banlieue's
policy. Two consequences:

- A Provider write the API server refuses (at admission, or by RBAC) fails the Host with reason
  `ProviderApplyRefused` and the API server's message, instead of retrying with no status; it is
  tried again after the retry interval. `controller-test` installs banlieue's policy (vendored in
  `test/banlieue/`).
- Decision 6 depends on banlieue ADR-0157: a write that leaves `spec.connection` unchanged is
  not checked, and `reference` on a Secret is enough to name it. When banlieue ships it,
  faubourg's controller is granted `reference` on Secrets in banlieue's namespace, still
  bounded to `faubourg-` names by `faubourg-provider-connection`, and this note says so. Until
  then libvirt zones are refused on a cluster that runs banlieue's policy.

*Amended 2026-11-27:* banlieue ADR-0157 is Accepted and shipped in banlieue v0.6.0. faubourg
pins `banlieue-api` `=0.6.1` and vendors that release's policy in `test/banlieue/`. The
controller holds `reference`, and no other verb, on Secrets in banlieue's namespace
(`deploy/rbac/controller.yaml`, Role `faubourg-controller-provider-credentials`). RBAC cannot
narrow it to the `faubourg-` prefix, so `faubourg-provider-connection` is its bound, as banlieue
ADR-0157 decision 3 requires. Labelling an adopted libvirt Provider needs no grant: it does not
change the connection. `controller-test` proves both outcomes against banlieue's policy: without
the binding the Host is `ProviderApplyRefused`, and with it the Provider is created. On a banlieue
older than v0.6.0 libvirt zones are still refused, and fail closed the same way.
