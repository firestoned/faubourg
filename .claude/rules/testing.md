<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# Testing Standards

## TDD is mandatory

Red, Green, Refactor for all code changes: new features start with a failing test, bug
fixes start with a test that reproduces the bug. Exceptions: marked prototype code
(removed before merging) and mechanical refactors covered by existing tests.

## Quality gate

After ANY `.rs` change, and before every commit:

```sh
make check   # fmt + clippy -D warnings (pedantic) + test + deny
```

The task is not complete until all four pass. Clippy warnings are fixed, not allowed,
unless the allow carries a one-line justification.

## Test placement

- **Unit tests:** embedded `#[cfg(test)] mod tests` at the bottom of the source file.
- **CRD drift:** `faubourg-api` has a test that fails when `deploy/crds/` is stale
  against the Rust types. Fix it with `make crds`, never by editing the YAML.
- **Live tests** (a real host, a real cluster) are `#[ignore]` and named `live_*`.

## Rules the suite enforces

1. **Fail closed.** An unsupported or unimplemented path returns an error, and there is
   a test proving the refusal. A permissive default is a bug. Example: an
   `AvailabilityZone` with `storage.mode: Shared` is rejected in v1 (ADR-0004).
2. **A fake more permissive than the real thing hides bugs.** When a live test finds
   behaviour a fake got wrong, the fake is fixed in the same change.
3. **A test that skips must never report success.** Live tests fail loudly when their
   fixture is missing.
4. **Never log or assert on secret material.** SSH keys, passwords and passphrases are
   never printed, formatted with `Debug`, or compared in assertions.
5. **No real infrastructure** in fixtures (`rules/no-real-infrastructure.md`).
