<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# 04: Provider-agnostic zones: Cloud Hypervisor, Firecracker and libvirt

Implements ADR-0018: a zone names one banlieue ProviderClass, and faubourg drives its hosts
through a driver chosen by the class's `backend`. Each Host is still one Provider and one
banlieue failure domain; the zone is still a label on those failure domains. ADD cycle per item.

**Status: in progress.** ADR-0018 Accepted 2026-11-18; CALM updated. Phases B (the driver seam)
and C (libvirt) done, but for C's live run (unblocked by banlieue ADR-0157 on 2026-11-27); Firecracker is planned. Firecracker (phase D) is gated on
banlieue's Firecracker provider (banlieue roadmap 20); phases B and C do not wait for it.

## Backends in scope

| Backend | banlieue deployment | Host to failure domain | Bootstrap credential | Drain |
| --- | --- | --- | --- | --- |
| `cloud-hypervisor` | External, on the host | one Provider, one FD | token on the script's stdin | banlieue ADR-0127 moves |
| `firecracker` | External, on the host | one Provider, one FD | as Cloud Hypervisor | `MovesUnsupported` (Recreate-only in banlieue) |
| `libvirt` | Managed, in the cluster | one Provider, one FD | client cert Secret, operator-made, referenced | `MovesUnsupported` |

Out of scope: Proxmox and vSphere, whose one Provider publishes many failure domains. They
need per-failure-domain labels and cordon in banlieue first.

## Open questions

- [x] Per-zone or per-Host libvirt client credential (ADR-0018 decision 6): **per zone**.
- [x] `DuplicateAddress` on the literal `spec.address` or a resolved one (decision 10):
      **literal**, case-folded, no resolution.
- [ ] Firecracker's endpoint scheme, install subcommand and ServiceAccount prefix: banlieue
      roadmap 20 decides; copied into ADR-0018 decision 2 when it does.

## Work

### A. Architecture

- [x] ADR-0018 accepted, open questions closed; amendment notes on ADR-0002, 0003, 0005,
      0007, 0011 and 0017
- [x] CALM: the zone controller and the Host reconciler read ProviderClasses; the controller
      writes libvirt Providers' `connection`; banlieue's libvirt provider reaches hosts on
      16514; the new `faubourg-provider-connection` policy; `make calm-validate`,
      `make calm-diagrams`

### B. The driver seam (no behaviour change for Cloud Hypervisor)

- [x] Mechanical refactor: endpoint and required deployment mode behind a `Backend` driver
      chosen from `ProviderClass.spec.backend` (`crates/faubourg-controller/src/backend.rs`);
      existing tests stay green
- [x] `AvailabilityZone.spec.providerClassRef`, optional, immutable once set (CEL on the
      root); `make crds`
- [x] Fallback to `--provider-class` when omitted; a pre-ADR zone serializes unchanged
- [x] Fail closed: missing class, unknown backend (libvirt and firecracker included, until
      their phases) or mismatched deployment gives `ProviderBackendUnsupported` on the zone and
      its Hosts, bootstrapped and adopted alike, no Provider, no Job
- [x] `ProviderClassMismatch` for a Host whose existing Provider has another class
- [x] `DuplicateAddress` for two Hosts with one literal address (IPv6 case-folded)
- [x] Token policy: `providerServiceAccountPrefixes` list; `make vap-test` covers a second
      listed class, an unlisted class, banlieue's operator, a bare prefix and an empty entry
- [x] `controller-test`: Managed class, a libvirt zone refused, the class immutable, two Hosts
      at one address and freeing one

### C. libvirt

- [x] Zone `spec.libvirt.credentialsRef` and `caBundleRef`; validation (names, `faubourg-`
      prefix, a ConfigMap key) in `faubourg-api`; `ProviderConnectionInvalid` when the
      connection does not fit the backend
- [x] Provider apply: `qemu+tls://<address>/system`, `credentialsRef`, `caBundle` (configMapRef)
      under the spec field manager
- [x] `faubourg-provider-connection` ValidatingAdmissionPolicy and its `vap-test` cases (it judges
      only a write that changes the connection, so an adopted Provider can still be labelled)
- [x] Bootstrap Job without provider variables, `kube-api` or provider CA for a Managed
      backend, created without an identity wait; the Job policy already admits that shape
      (`vap-test` submits it); a provider CA re-pin does not re-bootstrap such a host
- [x] Health for Managed: `Ready` and `ProviderReachable` for the current generation
      (`ProviderUnreachable`)
- [x] Drain refused with `MovesUnsupported` for non-moving backends, with the cordon kept
- [x] `examples/host/install-libvirt.sh` (same sudo rule); `examples/03-libvirt-zone.yaml`
- [x] `controller-test` cases for a libvirt zone (missing connection, the Provider past the
      policy, a Job that mints nothing, health, drain). No separate `e2e-test` case: the image
      runs with no provider inputs in `bootstrap-test`, against OpenSSH, which is the libvirt
      Job's whole difference
- [x] A Provider write the API server refuses fails the Host (`ProviderApplyRefused`);
      `controller-test` runs with banlieue's credentialsRef policy installed
- [x] banlieue ADR-0157 (Accepted 2026-11-22, shipped in banlieue v0.6.0) taken up:
      `banlieue-api` `=0.6.1`, its policy re-vendored, the controller granted `reference` (never
      `get`) on Secrets in banlieue's namespace, ADR-0018's amendment closed (2026-11-27);
      `controller-test` covers the refusal without the grant, the label apply that needs none,
      and the create with it; `vap-test` checks the grant is reference-only and namespaced
- [ ] Live: one libvirt host adopted, then bootstrapped from nothing (R-17), on banlieue v0.6.0
      or later. First attempt 2026-11-19, adopting a libvirt Provider on the core cluster:
      refused by banlieue's credentialsRef policy, which the kind tests did not install

### D. Firecracker (gated on banlieue roadmap 20)

- [ ] `banlieue-api` bumped to the release with the `firecracker` class
- [ ] Driver row filled in: endpoint, ServiceAccount prefix, `examples/install-firecracker.sh`
- [ ] `e2e-test` case; live: one Firecracker host bootstrapped from nothing

### E. Docs and threat model

- [x] README status table and opening paragraph; concepts, preparing-a-host, API reference
      (zone-placement is unchanged: a zone still reaches banlieue as a label)
- [x] Threat-model full pass (eighteenth), stamp bumped to ADR-0018: the controller's write to
      `credentialsRef` (confused deputy, bounded by the policy), A-16 and A-17, TB-8 (the
      cluster-to-host path on 16514), R-16 to R-18. Another pass when Firecracker lands
