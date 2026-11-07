<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# SSH credentials

The contract is [ADR-0006](../adr/0006-ssh-credentials.md), and its parser is
[`credentials.rs`](https://github.com/firestoned/faubourg/blob/main/crates/faubourg-api/src/credentials.rs).

## Where the Secrets live

Every Host credential is a Secret in one **credentials namespace**, set in the controller's
configuration (default `faubourg-credentials`). A Host names its Secrets but never their
namespace, so a Host cannot point at a Secret anywhere else.

| Host field | Credential | Can do |
| --- | --- | --- |
| `ssh.secretRef.name` | the scoped **run** credential | run `/opt/faubourg/install.sh`, nothing else |
| `ssh.placementSecretRef.name` | the optional privileged **placement** credential | write the script and its sudo rule |

The two must be different Secrets: validation refuses a Host where they are the same
(`SharedCredential`), because then the scoped credential would be the privileged one.

Only the bootstrap Job mounts these Secrets, and only its own Host's. The controller holds no
access to Secrets, and an admission policy limits every Job in the namespace to the pinned
bootstrap image, so a compromised controller cannot have a Job print a key
([ADR-0008](../adr/0008-bootstrap-jobs-constrained-by-admission-policy.md)).

When a Secret has `knownHosts`, the bootstrap image always verifies the host key strictly,
whatever the Host's `hostKeyPolicy` says. A key can then only be used against the hosts its own
Secret pins.

## Secret keys

| Key | Required | Notes |
| --- | --- | --- |
| `username` | yes | A plain login name: lowercase, digits, `_` and `-`, at most 32, not starting with `-`. |
| `privateKey` | one of these two | An OpenSSH private key. Preferred. |
| `password` | one of these two | Supported, discouraged. The Host gets a `WeakCredential` condition. |
| `passphrase` | no | For an encrypted `privateKey`. Refused with `password`. |
| `certificate` | no | An SSH certificate for `privateKey`, from an SSH CA. Preferred over a bare key. Refused with `password`. |
| `knownHosts` | recommended | `known_hosts` lines pinning the host key. Required under `Strict`. |

Setting both `privateKey` and `password` is refused: the choice must be explicit.

```sh
kubectl -n faubourg-credentials create secret generic hv01-ssh \
  --from-literal=username=faubourg \
  --from-file=privateKey=./hv01_ed25519 \
  --from-file=knownHosts=./hv01_known_hosts
```

## Host key policy

`ssh.hostKeyPolicy` decides what happens when the host's key is not in `knownHosts`:

`Strict` (default)
:   Refuse the connection. Pin the key in `knownHosts` before the first run.

`TrustOnFirstUse`
:   Must be set explicitly. The first key seen is accepted, its fingerprint is recorded in the
    Host's `status.acceptedHostKey`, and every later run is pinned to it. The first connection
    is open to interception, so prefer `Strict` on any network you do not control.

## Revoking the placement key

`ssh.revokeAfterBootstrap: true` makes the Job remove the placement key from the host once
placement succeeds. What remains is the scoped key, whose only sudo rule runs a script that root
owns. Compromising that key gains a re-run of the install, nothing more.

## Logging

Nothing in faubourg logs a Secret value or a command line. Types that hold credential bytes
redact themselves in `Debug` output. Logs carry Host names, zone names, host key fingerprints
and Job names.
