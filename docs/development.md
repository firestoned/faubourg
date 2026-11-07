<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# Development

## Architecture Driven Development

faubourg follows the same cycle as banlieue and mediatore, in a fixed order:

```text
ADR  ->  CALM  ->  TDD  ->  implement  ->  docs  ->  threat model
```

1. **ADR**: `docs/adr/NNNN-title.md`, one decision per record ([index](adr/README.md)). A
   change to a trust boundary, credential handling, an API surface or the contract with banlieue
   starts here, not with code.
2. **CALM**: update
   [`architecture.json`](https://github.com/firestoned/faubourg/blob/main/docs/architecture/calm/architecture.json)
   ([FINOS CALM](https://calm.finos.org/)); `make calm-validate` must pass and
   `make calm-diagrams` must render the [system](architecture/diagrams/system.md) and
   [flow](architecture/diagrams/flows.md) diagrams.
3. **TDD**: a failing test first, then the minimum implementation.
4. **Docs**: README, this site, ADR cross-links, examples.
5. **Threat model**: a full pass over the [threat model](security/threat-model.md), with its
   header stamp bumped. An ADR is not implemented until this pass is done.

The full rules are in
[`.claude/rules/`](https://github.com/firestoned/faubourg/tree/main/.claude/rules).

## Make targets

Every CI job calls a Make target, so everything runs the same locally.

| Target | What it does |
| --- | --- |
| `make check` | The pre-commit gate: `fmt`, `lint` (clippy pedantic, `-D warnings`), `test`, `deny`. |
| `make crds` | Regenerate `deploy/crds/` from the Rust types. |
| `make crds-check` | Fail when `deploy/crds/` is stale. |
| `make calm-validate` | Validate the CALM model. Needs Node.js. |
| `make calm-diagrams` | Render the Mermaid diagrams from the model. |
| `make vap-test` | The bootstrap admission policy and controller RBAC against a throwaway kind cluster ([ADR-0008](adr/0008-bootstrap-jobs-constrained-by-admission-policy.md)). Needs kind, kubectl, jq; set `KIND_EXPERIMENTAL_PROVIDER=podman` for rootless podman. Runs in CI. |
| `make controller-test` | The Host controller binary, with its own ServiceAccount's token, against a throwaway kind cluster ([ADR-0009](adr/0009-host-reconciler.md)). Same tools as `vap-test`. Runs in CI. |
| `make docs` | Build this site with `mkdocs build --strict`. Needs Poetry. |
| `make docs-serve` | Serve this site with live reload on port 8000. |
| `make dco-check` | Every commit since `origin/main` is signed off by its author. |
| `make check-infra-identifiers` | No real host names or addresses in tracked files. |
| `make help` | Everything else. |

## Continuous integration

| Workflow | Runs | Checks |
| --- | --- | --- |
| [`build.yaml`](https://github.com/firestoned/faubourg/blob/main/.github/workflows/build.yaml) | PR, push, release | SPDX headers, signed commits, DCO, no real infrastructure, fmt, clippy, tests, CRD drift, cargo-audit, cargo-deny; one aggregate `Required Checks` job |
| [`calm.yaml`](https://github.com/firestoned/faubourg/blob/main/.github/workflows/calm.yaml) | changes under `docs/architecture/` | CALM validates and the committed diagrams match the model |
| [`docs.yaml`](https://github.com/firestoned/faubourg/blob/main/.github/workflows/docs.yaml) | changes to the docs | Strict MkDocs build; deploys to GitHub Pages from `main` |
| [`codeql.yaml`](https://github.com/firestoned/faubourg/blob/main/.github/workflows/codeql.yaml) | PR, push, weekly | CodeQL for Rust and Actions |
| [`sast.yaml`](https://github.com/firestoned/faubourg/blob/main/.github/workflows/sast.yaml) | PR, push | Semgrep |
| [`scorecard.yaml`](https://github.com/firestoned/faubourg/blob/main/.github/workflows/scorecard.yaml) | push, weekly | [OpenSSF Scorecard](https://scorecard.dev/) |
| [`dependabot-auto-merge.yaml`](https://github.com/firestoned/faubourg/blob/main/.github/workflows/dependabot-auto-merge.yaml) | Dependabot PRs | Patch and minor merge themselves after the build gate; majors wait for a review |

Composite actions come from
[`firestoned/github-actions`](https://github.com/firestoned/github-actions), and every
third-party action is pinned by SHA.

## Commits

Every commit is signed (`-S`) and signed off (`-s`) for the
[Developer Certificate of Origin](https://developercertificate.org/). See
[`CONTRIBUTING.md`](https://github.com/firestoned/faubourg/blob/main/CONTRIBUTING.md).

## Roadmaps

The status board is
[`ROADMAPS.md`](https://github.com/firestoned/faubourg/blob/main/ROADMAPS.md); each roadmap's
detail is under
[`.github/community/`](https://github.com/firestoned/faubourg/tree/main/.github/community).
