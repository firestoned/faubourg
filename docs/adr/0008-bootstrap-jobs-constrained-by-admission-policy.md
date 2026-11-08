<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0008: Bootstrap Jobs are constrained by a ValidatingAdmissionPolicy

**Status:** Accepted · **Date:** 2026-11-07

## Context

ADR-0005 runs each host's SSH session in a Job in the credentials namespace, and ADR-0006
gives the controller no `get`, `list` or `watch` on Secrets there. That alone does not keep
the credentials from the controller. Whoever can create a Job in a namespace can mount any
Secret in it into a container of their choosing, and print it. A compromised controller, or
anyone holding its token, would read every host's root-equivalent key.

A Kubernetes admission policy can close that gap. A `ValidatingAdmissionPolicy` (VAP, GA in
Kubernetes 1.30) evaluates CEL in the API server, needs no webhook, and cannot be bypassed by
the controller. VAP cannot look up other objects, so it cannot check that a Job mounts the
Secret its Host names. The design has to make that check unnecessary.

## Decision

1. **A VAP and its binding guard the credentials namespace.** faubourg ships
   `deploy/admission/bootstrap-policy.yaml`. Its binding matches only the credentials
   namespace, with `validationActions: [Deny]` and `failurePolicy: Fail`. An error in the
   policy refuses the request.
2. **Parameters come from a ConfigMap neither the controller nor a script maintainer can
   write.** `faubourg-bootstrap-policy` lives in the controller's namespace
   (`faubourg-system`), not in the credentials namespace, where script maintainers need
   ConfigMap write access. The controller holds no ConfigMap write anywhere. It holds:
   - `image`: the bootstrap image, pinned by digest;
   - `serviceAccountName`: the bootstrap pods' ServiceAccount, which holds no RBAC;
   - `scriptSources`: a comma-separated allowlist of `ConfigMap/<name>` or `Secret/<name>` that
     may be mounted as the install script.
   With no parameter object, the binding denies everything (`parameterNotFoundAction: Deny`).
3. **Every Job in the namespace must have the bootstrap shape**, on create and update:
   - one container, named `bootstrap`, running the `image` parameter by digest, with no
     `command` and no `args`. No init or ephemeral containers;
   - `env` entries with literal values only: no `valueFrom`, no `envFrom`;
   - exactly the volumes the bootstrap needs, all mounted read-only: `credentials` (a Secret),
     optionally `placement` (a Secret), and `script` (a ConfigMap or Secret on the
     `scriptSources` allowlist). No other volume sources: no `hostPath`, PVC, projected token
     or downward API;
   - `serviceAccountName` equal to the parameter, and `automountServiceAccountToken: false`;
   - no `hostNetwork`, `hostPID`, `hostIPC` or `shareProcessNamespace`;
   - the restricted security profile: `runAsNonRoot`, `allowPrivilegeEscalation: false`,
     `readOnlyRootFilesystem`, capabilities `drop: [ALL]` with nothing added, not privileged;
   - `restartPolicy: Never`.
4. **Only the Job controller creates Pods in the namespace.** A Pod create is admitted only
   from `system:serviceaccount:kube-system:job-controller`, and only for a Pod a Job controls.
   A Pod update may not change its `spec`, which also rules out `kubectl set image`. Ephemeral
   containers are refused. So every pod that can mount a Secret here came from a Job that
   passed rule 3, whatever created that Job.
5. **The image, not the controller, decides how a credential is used.** The image is
   faubourg's own, built from this repository and pinned by digest. It:
   - verifies the host key before sending any authentication. When the credential Secret has
     `knownHosts`, it is always `Strict`, whatever policy the Job's environment names, so a
     key only works against the hosts its own Secret pins;
   - writes only the mounted `script` to `/opt/faubourg/install.sh`, and runs only
     `sudo -n /opt/faubourg/install.sh`;
   - never prints, logs or writes a credential.
6. **The controller's RBAC in the credentials namespace** is `create`, `get`, `list`, `watch`
   and `delete` on Jobs, `get`, `list` and `watch` on Pods (for the termination message that
   carries `ScriptDrift`), and nothing on Secrets, ConfigMaps, Pods' subresources or the
   policy objects.

## Consequences

- A compromised controller can still run the pinned image, for any host, with any credential
  Secret in the namespace, against any address. It cannot read a key. A key Secret with
  `knownHosts` can only be used against the hosts it pins. A password Secret, or a key without
  `knownHosts` (`TrustOnFirstUse`), can be pointed at a server the attacker runs: a password is
  then disclosed, while a key's signature is bound to that SSH session and cannot be replayed.
  This is recorded as an accepted risk, and is one more reason `WeakCredential` exists.
- The script allowlist is a new thing for operators to maintain. Adding a script source is an
  edit to the parameter ConfigMap in `faubourg-system`, by a platform administrator, not by the
  script's maintainer.
- Pod creation is attributed to the Job controller only when kube-controller-manager runs with
  `--use-service-account-credentials` (the kubeadm and kind default; check other
  distributions, k0s included, before relying on it). On a distribution without it, the Pod
  rule refuses every bootstrap pod: it fails closed.
- Kubernetes 1.30 or later is required.
- The policy names the credentials namespace and the bootstrap ServiceAccount. Changing the
  namespace from the default means changing the binding too.
- The policy is tested against a real API server (`make vap-test`, kind), not against a model
  of CEL: a fake that admits more than the API server would hide a hole.
- The controller holds no secret material, so its own memory, logs and crash dumps cannot leak
  any.

*Amended 2026-11-07 by ADR-0011 (decisions 6 to 8):* a bootstrap Job also mounts a `kube-api` volume, a projected token of at most 600 seconds for its own ServiceAccount and the cluster CA, admitted only in that shape; the bootstrap ServiceAccount, which held nothing, may create `serviceaccounts/token` in banlieue's namespace, bounded by a third policy, `faubourg-provider-tokens`.
