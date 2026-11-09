<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# 01: Host controller and bootstrap Job

Implements ADR-0005, ADR-0006, ADR-0007 for one host. ADD cycle per item.

- [x] `banlieue-api` from crates.io, pinned exactly (`=0.5.0`: kube ~5.0, k8s-openapi 0.29; ADR-0002 decision 4)
- [x] Host phase machine (Pending, Bootstrapping, Ready, Degraded, Failed) as a pure function, and `Provider` health read fail closed (`crates/faubourg-controller`)
- [x] ADR-0009 and the Host reconciler: `plan.rs` decides each reconcile as a pure function, `controller.rs` carries it out; a new Job only when the bootstrap inputs change; zone gate; status by server-side apply; `deploy/controller/deployment.yaml`; tested on kind with its own RBAC (`make controller-test`)
- [x] ADR-0005 amendment: the bootstrap Job and the script source live in the credentials namespace
- [x] ADR-0008: a ValidatingAdmissionPolicy constrains every Job and Pod in the credentials namespace (`deploy/admission/`), tested on kind (`make vap-test`)
- [x] Bootstrap Job builder (`job.rs`): mounts only the Host's Secret(s) and the script source; controller RBAC with no Secret or ConfigMap access (`deploy/rbac/controller.yaml`)
- [x] `make vap-test` and `make controller-test` in CI (the `kind` job in `build.yaml`)
- [x] Refuse Host names longer than 63 in the CRD (a CEL rule, ADR-0009 decision 10)
- [x] Re-bootstrap on changed script *content*: the content is pinned by digest in the bootstrap policy, the image refuses any other, and a change rolls out one host at a time, halting at a failure (ADR-0013; the controller reads that one ConfigMap and no other)
- [x] ADR-0010 and the bootstrap image (`crates/faubourg-bootstrap`, `images/bootstrap/`): russh, no shell in the image; host key verified before authenticating and always `Strict` when the Secret has `knownHosts`; owner, mode and sha256 check of the on-host script; placement and `visudo`-checked sudo rule; `sudo -n /opt/faubourg/install.sh`; exit 64 `ScriptDrift` and 65 `HostKeyRefused`; tested against a real sshd (`make bootstrap-test`)
- [x] TOFU fingerprint reported by the Job and pinned in status by the controller; `revokeAfterBootstrap`
- [x] ADR-0011: faubourg creates and labels the host's banlieue `Provider` (field manager `faubourg.firestoned.io/host-controller`), labels an adopted one, never deletes one; tested on kind (`make controller-test`)
- [x] ADR-0012: both images built on every event, pushed, Cosign-signed, attested and Trivy-scanned from `main` and releases; a release ships one install file rendered with both digests and signed (`make release-manifests`); `make e2e-test` installs that file and runs the controller in the cluster
- [x] Cut the first release (v0.1.0) and verify it as `docs/guides/installing.md` says
- [x] Live test, stage 1: install v0.1.0 on a real k0s cluster and adopt a live host (labels, health, cordon); found the missing run-account provisioning (ADR-0010 amended)
- [x] Live test, stage 2: bootstrap that host from faubourg (placement, account, token), on v0.1.1; found that the example install script never installed the provider binary (`--provider-binary`, fixed)
- [x] A Host switched from `Script` back to `None` keeps a valid Provider: the spec has its own field manager (ADR-0011 amended)
- [x] Leader election: two replicas, one leader through the Lease `faubourg-controller`, taken by conditional writes, the leader stopping before its lease can lapse (ADR-0014); `controller-test` runs two controllers and checks the handover
- [x] Ready from the Provider's `Ready` and heartbeat (banlieue ADR-0102), read from the one Provider labelled with the host (ADR-0009 decision 5)
- [x] ADR-0011: the host's first provider token is minted by the bootstrap pod (TokenRequest, bounded by `faubourg-provider-tokens`) and delivered on the install script's stdin; end to end on kind with the real image (`make e2e-test`)
- [ ] Ask banlieue to publish the provider ServiceAccount name (status or `banlieue-api`), and drop faubourg's copy of its naming
- [x] A CA option for hosts that reach the API server through another certificate authority: `providerCASource`, pinned by digest in the bootstrap policy, checked by the policy and the image (exit 67), written into the host's kubeconfig (ADR-0016)
- [x] Threat-model pass after stage 2 closes R-3 (eighth pass; R-13 added)
