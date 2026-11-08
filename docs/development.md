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
| `make bootstrap-image` | Build the bootstrap image with podman: one static binary, no shell ([ADR-0010](adr/0010-bootstrap-image.md)). |
| `make controller-image` | Build the controller image with podman ([ADR-0012](adr/0012-images-signed-and-published-by-digest.md)). |
| `make release-manifests` | Render `deploy/` into one install file with `CONTROLLER_IMAGE` and `BOOTSTRAP_IMAGE` by digest (and optionally `PROVIDER_API_SERVER`), as a release does. Refuses an image not pinned by digest. |
| `make render-manifests-test` | Test the render: both digests where they belong, everything else refused. Runs in CI. |
| `make e2e-test` | A Host bootstrapped end to end in kind, installed as a release installs: both images pulled by digest from a local registry, the rendered install file applied, the controller running as its Deployment; an OpenSSH host as a pod; banlieue simulated with kubectl ([ADR-0011](adr/0011-the-hosts-provider-and-its-identity.md)). Needs kind, kubectl, jq, podman. Runs in CI. |
| `make bootstrap-test` | The bootstrap image against a throwaway OpenSSH host, run the way a Job runs it (read-only, UID 65532, no capabilities). Needs podman, ssh-keygen, jq. Runs in CI. |
| `make controller-test` | The Host controller binary, with its own ServiceAccount's token, against a throwaway kind cluster ([ADR-0009](adr/0009-host-reconciler.md)). Same tools as `vap-test`. Runs in CI. |
| `make integration` | All four integration tests at once on one machine: `bootstrap-test` alongside, and `vap-test`, `controller-test` and `e2e-test` one after another on one shared kind cluster (they write the same cluster-wide objects, so they cannot share it at the same time). Each test's log is kept. CI runs them as separate parallel jobs instead. |
| `make docs` | Build this site with `mkdocs build --strict`. Needs Poetry. |
| `make docs-serve` | Serve this site with live reload on port 8000. |
| `make dco-check` | Every commit since `origin/main` is signed off by its author. |
| `make check-infra-identifiers` | No real host names or addresses in tracked files. |
| `make help` | Everything else. |

Image builds cache the cargo registry and `target/` (cache mounts), so a rebuild compiles only
what changed. The integration tests build their images with `--build-arg PROFILE=image-test`
(`Cargo.toml`): the release code without link-time optimization, which is most of a release
build's time. Shipped images always build with `release`; the Containerfiles refuse any other
profile.

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
