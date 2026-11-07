<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0006: SSH credentials: one Secret per host in a dedicated namespace, strict host keys

**Status:** Accepted · **Date:** 2026-11-07

## Context

The bootstrap credential is root-equivalent on a hypervisor. A static private key in a
Kubernetes Secret is the weakest form of it, and trust on first use leaves the first
connection open to interception.

## Decision

1. A Host references its scoped run credential by name (`ssh.secretRef.name`) and,
   optionally, a privileged placement credential (`ssh.placementSecretRef.name`,
   ADR-0005 decision 4). Both Secrets live in one **credentials namespace** set in the
   controller's configuration (default `faubourg-credentials`), never in the Host. Only
   the bootstrap Jobs mount them.
2. Secret keys: `username` (required); one of `privateKey` (preferred) or `password`
   (supported, discouraged, flagged by a `WeakCredential` condition); `passphrase`
   (optional); `certificate` (an SSH certificate from an SSH CA, preferred over a bare key);
   `knownHosts` (recommended).
3. Host keys are verified. `ssh.hostKeyPolicy: Strict` (default) refuses a host whose key
   is not in `knownHosts`. `TrustOnFirstUse` must be set explicitly; the accepted key's
   fingerprint is recorded in `status.acceptedHostKey` and pinned for every later run.
4. `ssh.revokeAfterBootstrap: true` makes the Job remove the placement key from the host
   once placement succeeds, leaving only the scoped run credential, whose one sudo rule
   runs the root-owned script (ADR-0005).
5. Nothing logs a Secret value or a command line. Types holding credentials implement
   `Debug` by redaction.

## Consequences

- The controller's own RBAC never includes reading Secrets in the credentials namespace.
- Open: whether to support only SSH certificates once an SSH CA is available.
