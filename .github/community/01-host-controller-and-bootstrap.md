<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# 01: Host controller and bootstrap Job

Implements ADR-0005, ADR-0006, ADR-0007 for one host. ADD cycle per item.

- [x] `banlieue-api` pinned by git rev (kube ~4.2, k8s-openapi 0.28)
- [x] Host phase machine (Pending, Bootstrapping, Ready, Degraded, Failed) as a pure function, and `Provider` health read fail closed (`crates/faubourg-controller`)
- [ ] Host reconciler: validate, compute the phase with `phase::decide`, write phase and conditions
- [ ] ADR-0005 amendment: the namespace the bootstrap Job runs in. A pod mounts Secrets and ConfigMaps only from its own namespace, but ADR-0005 puts the script source in the controller's namespace and ADR-0006 puts the SSH Secrets in the credentials namespace
- [ ] Bootstrap Job builder: mounts only the Host's Secret(s) and the script source; RBAC for the controller has no Secret read in the credentials namespace
- [ ] Job image: ssh client, sha256 compare of on-host script, placement, `sudo -n /opt/faubourg/install.sh`, `ScriptDrift`
- [ ] Strict host keys, TOFU fingerprint pinned in status, `revokeAfterBootstrap`
- [ ] Create and label the host's banlieue `Provider` (field manager `faubourg.firestoned.io/host-controller`)
- [ ] Ready from the Provider's `Ready` and heartbeat (banlieue ADR-0102): the decision is in `health.rs` and `phase.rs`; the watch on Providers is the reconciler's
- [ ] ADR: how the host-resident provider gets its kube identity
- [ ] Live test against one host; threat-model pass closes R-3
