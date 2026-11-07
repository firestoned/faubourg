<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# Working on faubourg

- Read `docs/adr` first. This project follows Architecture Driven Development (ADD), the
  same cycle as banlieue and mediatore: ADR, then CALM, then TDD, then implement, then
  docs, then the threat-model pass. A change to a trust boundary, credential handling, an
  API surface or the contract with banlieue starts with an ADR, not with code.
- Quality gate before any commit: `make check` (fmt, clippy pedantic `-D warnings`, test,
  `cargo deny check`).
- No em-dashes in prose or comments.
- Never log SSH private keys, passwords, passphrases, or bootstrap command lines. Log
  Host names, zone names, host key fingerprints and Job names.
- Fail closed: an unsupported or unimplemented path returns an error, never a permissive
  default.
- banlieue owns the per-host agent (its host-resident Cloud Hypervisor provider) and VM
  placement. faubourg does not reimplement either (ADR-0002).
- Licenses: permissive or foundation-governed only; `cargo deny` enforces the allowlist.

Full rules, adapted from banlieue and mediatore, live in `.claude/rules/`:

- `rules/architecture-driven-development.md`: the ADD cycle and when it applies.
- `rules/testing.md`: TDD, the quality gate (`make check`), test placement.
- `rules/threat-modeling.md`: full pass over `docs/security/threat-model.md` after every
  implemented ADR; the header stamp is the deliverable.
- `rules/github-workflows.md`: Makefile-driven workflows, firestoned/github-actions
  composites, SHA pinning, `workflow_call`.
- `rules/no-real-infrastructure.md`: public repo; placeholders only, sweep the diff before
  finishing.
- Roadmaps: status board in `ROADMAPS.md`, detail docs in `.github/community/NN-name.md`
  (lowercase-hyphen, contiguous from 00). Update the board row in the same commit that
  changes an item's state.
