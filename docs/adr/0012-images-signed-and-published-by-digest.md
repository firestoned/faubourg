<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0012: Images are signed, published and deployed by digest

**Status:** Proposed · **Date:** 2026-11-07

## Context

faubourg ships two images: the controller (ADR-0009) and the bootstrap image (ADR-0010).
Neither is published yet, so `deploy/` carries placeholder digests and nothing can be
installed. The bootstrap image is the fleet's root of trust: whatever runs as it holds every
host's credentials (ADR-0008 decision 5). How it is built, where it is published, how an
operator knows a published image is faubourg's, and how its digest reaches the admission
policy are security decisions, not packaging details.

banlieue already publishes its images this way (banlieue ADR-0006): GitHub Actions builds them,
Cosign signs them keyless, and a release carries manifests rendered with the digests. faubourg
follows it, with two differences that come from the bootstrap image's weight.

## Decision

1. **Two images, one shape.** `ghcr.io/firestoned/faubourg-controller` and
   `ghcr.io/firestoned/faubourg-bootstrap`, from `images/controller/Containerfile` and
   `images/bootstrap/Containerfile`. Each is one static binary on `scratch`, run as UID 65532,
   built with `cargo build --locked` on a base image pinned by digest. `linux/amd64` only for
   now.
2. **Built on every event, pushed only from `main` and releases.** Pull requests build both
   images, which validates the Containerfiles. A push to `main` publishes them with the tags
   `sha-<short>` and `main-<date>`; a release with `v<version>` and `v<major>.<minor>`. There
   is no `latest`, and no pull request image: unlike banlieue, nothing but `main` and a release
   ever carries this repository's signature.
3. **Every pushed image is signed and attested, by digest.**
   - Cosign keyless signing with the workflow's GitHub OIDC identity, through
     `firestoned/github-actions/security/cosign-sign`.
   - A build provenance attestation (`actions/attest-build-provenance`) and BuildKit's SBOM
     and provenance.
   The signing certificate names the workflow and the ref, so a release image is told apart
   from a `main` image by its certificate, not by its tag.
4. **Scanned before release.** Trivy, through `firestoned/github-actions/security/trivy-scan`,
   scans every pushed digest and uploads SARIF. On a release, a `CRITICAL` or `HIGH` finding
   fails the release.
5. **Manifests are rendered with the digests, never edited by hand.** `deploy/` keeps
   `@sha256:<digest>` placeholders, which no runtime accepts as a pinned image.
   `make release-manifests` renders one file, `faubourg-<version>.yaml`, from `deploy/` (CRDs,
   RBAC, admission policies, their parameters and the controller Deployment). The bootstrap
   digest is written in two places, the admission policy's `image` parameter and the
   controller's `FAUBOURG_BOOTSTRAP_IMAGE`, by the same render, so they cannot disagree. The
   render refuses an image that is not `<repository>@sha256:<64 hex>`.
6. **The release carries the rendered file, signed.** The release job uploads it with a Cosign
   signature bundle (`cosign sign-blob`). A release without both digests fails: fail closed.
7. **What the operator still sets.** The rendered Deployment leaves `FAUBOURG_PROVIDER_API_SERVER`
   empty, and the controller refuses to start until it is set (ADR-0011 decision 11). Script
   sources, the API audiences and the banlieue namespace stay operator configuration.
8. **How an operator verifies.** The documentation gives the `cosign verify` command for a
   release image (certificate identity
   `https://github.com/firestoned/faubourg/.github/workflows/build.yaml@refs/tags/v*`, issuer
   `https://token.actions.githubusercontent.com`) and `gh attestation verify`. Enforcing
   signatures at admission (a policy controller) is the operator's choice; the admission policy
   already pins the bootstrap image by digest.
9. **The end-to-end test deploys what a release deploys.** `make e2e-test` builds both images,
   renders the manifests with `make release-manifests` against a local registry, and runs the
   controller as the rendered Deployment, in the cluster, instead of the binary beside it.
10. **Permissions.** Only the image job holds `packages: write`, `id-token: write` and
    `attestations: write`, and only the release job holds `contents: write`, each declared at
    the job with a comment.

## Consequences

- An operator can install faubourg from one signed file, and check every image in it.
- A compromise of this repository's `main` branch, or of its release workflow, can publish a
  signed malicious bootstrap image. Pinning the release ref in verification narrows that to
  the release workflow, and the admission policy accepts only the digest the operator applied.
- No pull request images: a change is tried on a cluster by building locally
  (`make bootstrap-image`) or from `main`.
- `linux/amd64` only. arm64 needs a cross toolchain in the build stage; revisit when a host or
  cluster needs it.
- Base image updates come through Dependabot's docker ecosystem; a new base digest changes the
  image digest, which a release then pins.
