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
- [ ] Re-bootstrap on changed script *content* under the same source name, without giving the controller ConfigMap access (ADR-0009 decision 6)
- [x] ADR-0010 and the bootstrap image (`crates/faubourg-bootstrap`, `images/bootstrap/`): russh, no shell in the image; host key verified before authenticating and always `Strict` when the Secret has `knownHosts`; owner, mode and sha256 check of the on-host script; placement and `visudo`-checked sudo rule; `sudo -n /opt/faubourg/install.sh`; exit 64 `ScriptDrift` and 65 `HostKeyRefused`; tested against a real sshd (`make bootstrap-test`)
- [x] TOFU fingerprint reported by the Job and pinned in status by the controller; `revokeAfterBootstrap`
- [ ] Create and label the host's banlieue `Provider` (field manager `faubourg.firestoned.io/host-controller`)
- [ ] Controller image: build, sign and publish `faubourg-controller`, and pin it in `deploy/controller/deployment.yaml`
- [ ] Leader election, before more than one replica
- [x] Ready from the Provider's `Ready` and heartbeat (banlieue ADR-0102), read from the one Provider labelled with the host (ADR-0009 decision 5)
- [ ] ADR: how the host-resident provider gets its kube identity
- [ ] Live test against one host; threat-model pass closes R-3
