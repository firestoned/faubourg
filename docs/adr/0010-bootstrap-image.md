<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0010: The bootstrap image

**Status:** Accepted · **Date:** 2026-11-07

## Context

ADR-0005 and ADR-0008 say what the bootstrap Job may mount and run, and ADR-0009 creates it.
What runs inside it is still open: the program that turns a mounted credential, an address and
a script into an installed host. ADR-0008 decision 5 puts the security of the credentials on
that program, not on the controller: it alone decides how a key is used. So it has to:

- verify the host's key before it sends any authentication, and keep a key with `knownHosts`
  to the hosts it pins;
- run nothing on the host but fixed commands, with the Host's data never spliced into one;
- place the script and the sudo rule only when they differ, and say `ScriptDrift` when it
  cannot (ADR-0005 decision 4);
- report the host key it saw, so the controller can pin it under `TrustOnFirstUse`
  (ADR-0006 decision 3);
- never print a credential.

Two ways to speak SSH were considered: the OpenSSH client driven by a wrapper, and an SSH
library in Rust. OpenSSH is the most reviewed client there is, but the wrapper would build an
`ssh` command line and a `known_hosts` file, parse the fingerprint out of its output, and need
a shell and OpenSSH's libraries in the image.

## Decision

1. **A Rust binary on russh, in an image with nothing else.** `faubourg-bootstrap`, in
   `crates/faubourg-bootstrap`, uses [russh](https://github.com/warp-tech/russh) (Apache-2.0),
   pinned exactly, with the `ring` backend. The image holds that one static binary and runs
   as UID 65532: no shell, no package manager, no `ssh`. Even a Job that got past the
   admission policy with a command would find nothing to run.
2. **Key types.** Ed25519 and ECDSA, for the client's own key and the host's. RSA is refused:
   it would pull in the `rsa` crate, which has an open timing advisory, and both kinds of key
   are available on every supported OpenSSH. An SSH host *certificate* is refused for now;
   accepting one needs a CA to trust, which is the SSH CA question in ADR-0006.
3. **Inputs are the Job's environment and its mounted volumes, nothing else.** The address,
   port, host key policy, pinned fingerprint and revoke flag come from the literal environment
   the builder writes (ADR-0008 decision 3). The credentials come from the mounted Secret
   files, parsed by `faubourg-api`'s `SshCredentials` (ADR-0006). The script comes from the
   mounted `script` volume. Anything missing or malformed exits 1 before any connection.
4. **The host key is checked before authentication, in russh's `check_server_key`.**
   - When the run credential's Secret has `knownHosts`, the policy is `Strict` whatever the
     environment says (ADR-0008 decision 5). The key must equal one on a `knownHosts` line
     whose host patterns name the address, as `<address>` or `[<address>]:<port>`. Hashed and
     wildcard patterns are not read, so they never match: fail closed.
   - `Strict` with no `knownHosts` refuses every key.
   - `TrustOnFirstUse` with a pinned fingerprint (`FAUBOURG_ACCEPTED_HOST_KEY`) accepts only
     that key. With none, it accepts the first key and records its fingerprint.
   - Every connection in one run, placement and run alike, must see the same key as the first.
   - A refused key exits 65 (`HostKeyRefused`) without authenticating.
5. **Fixed commands, data on stdin.** Every command the binary sends is a constant in its
   source. Nothing from the Host, the Secret or the script is ever part of a command string:
   the script, the sudoers line and a public key to revoke reach the host on the channel's
   standard input.
6. **The steps.**
   1. *Check*, with the run credential: read the owner, group and mode of `/opt/faubourg` and
      `/opt/faubourg/install.sh`, and the script's sha256. It is in place only when both are
      `root:root`, mode `0755`, and the hash equals the mounted script's.
   2. *Place*, only when it is not in place. With no placement credential, exit 64
      (`ScriptDrift`): nothing ran. With one, log in as its user (through `sudo -n` when that
      user is not root), write the script to a temporary file, set `root:root 0755` and move
      it into place; write `/etc/sudoers.d/faubourg` with the one rule of ADR-0005 decision 2
      for the run credential's user, check it with `visudo -cf`, and move it into place. Then
      check again, with the run credential, and exit 64 if it is still not in place.
   3. *Revoke*, when `revokeAfterBootstrap` is set and placement ran: remove the placement
      key's public half from that user's `~/.ssh/authorized_keys` (ADR-0006 decision 4).
   4. *Run*, with the run credential: `sudo -n /opt/faubourg/install.sh`. Its exit status
      decides the Job's: 0 succeeds, anything else exits 1.
7. **What it reports.** On success and on `ScriptDrift`, it writes the host key's SHA256
   fingerprint to the pod's termination message as `{"hostKey":"SHA256:..."}`. Its logs carry
   the Host name, the fingerprint, each step and its exit status, and the install script's own
   output. Never a credential, never the script's content, never a command line.
8. **The controller pins a first-use key from a succeeded Job.** When the Host's policy is
   `TrustOnFirstUse`, it has no `acceptedHostKey`, and its current Job succeeded, the
   reconciler copies the fingerprint from that Job's pod into `status.acceptedHostKey`. It is
   passed to every later Job (ADR-0006 decision 3). It is not a bootstrap input, so recording
   it re-runs nothing (ADR-0009 decision 6). A fingerprint is pinned only from a Job the Host
   owns, read from its own pod.
9. **Timeouts.** 30 seconds to connect and authenticate, per connection. The Job's
   `activeDeadlineSeconds` bounds the rest (ADR-0008).
10. **Tested against a real sshd.** `make bootstrap-test` builds the binary and an OpenSSH
    server image with sudo (`test/sshd/`), and runs the binary against it outside
    Kubernetes: first placement, drift without placement, a refused host key, `knownHosts`
    overriding the policy, a TOFU pin, revoke, and a failing script.

## Consequences

- faubourg depends on russh's SSH implementation, which has had far less review than
  OpenSSH. In exchange, the host-key check is code faubourg tests, and the image has no shell
  and no client to misconfigure. A russh advisory is caught by `cargo deny` and Dependabot.
- RSA-only hosts and RSA keys cannot be bootstrapped. The operator generates an Ed25519 key;
  every supported OpenSSH has an Ed25519 host key.
- `knownHosts` lines must name the host plainly (`ssh-keyscan` without `-H`).
- A placement user other than root needs passwordless `sudo` for the placement commands. The
  scoped run user never needs more than ADR-0005's one rule.
- The image is built here (`images/bootstrap/Containerfile`); publishing and signing it is the
  next roadmap 01 item, and until then its digest is set by whoever builds it.
