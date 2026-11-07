<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# 01: Host controller and bootstrap Job

Implements ADR-0005, ADR-0006, ADR-0007 for one host. ADD cycle per item.

- [x] `banlieue-api` from crates.io, pinned exactly (`=0.5.0`: kube ~5.0, k8s-openapi 0.29; ADR-0002 decision 4)
- [x] Host phase machine (Pending, Bootstrapping, Ready, Degraded, Failed) as a pure function, and `Provider` health read fail closed (`crates/faubourg-controller`)
- [ ] Host reconciler: validate, compute the phase with `phase::decide`, write phase and conditions
- [x] ADR-0005 amendment: the bootstrap Job and the script source live in the credentials namespace
- [x] ADR-0008: a ValidatingAdmissionPolicy constrains every Job and Pod in the credentials namespace (`deploy/admission/`), tested on kind (`make vap-test`)
- [x] Bootstrap Job builder (`job.rs`): mounts only the Host's Secret(s) and the script source; controller RBAC with no Secret or ConfigMap access (`deploy/rbac/controller.yaml`)
- [ ] `make vap-test` in CI (a kind job in `build.yaml`)
- [ ] Refuse Host names longer than 63 in the CRD (a label value, and the builder already refuses them)
- [ ] Re-bootstrap on a changed script source without giving the controller ConfigMap access (ADR-0005 decision 8)
- [ ] Bootstrap image: ssh client, host key verified before authenticating and always `Strict` when the Secret has `knownHosts` (ADR-0008 decision 5), sha256 compare of the on-host script, placement, `sudo -n /opt/faubourg/install.sh`, exit 64 `ScriptDrift` and 65 host key refused
- [ ] Strict host keys, TOFU fingerprint pinned in status, `revokeAfterBootstrap`
- [ ] Create and label the host's banlieue `Provider` (field manager `faubourg.firestoned.io/host-controller`)
- [ ] Ready from the Provider's `Ready` and heartbeat (banlieue ADR-0102): the decision is in `health.rs` and `phase.rs`; the watch on Providers is the reconciler's
- [ ] ADR: how the host-resident provider gets its kube identity
- [ ] Live test against one host; threat-model pass closes R-3
