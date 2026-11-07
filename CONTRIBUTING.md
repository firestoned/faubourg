<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# Contributing to faubourg

Thank you for helping. This page covers what every contribution needs, how a change moves from
idea to merge, and how to build and test locally.

Security issues do **not** go in public issues or pull requests: report them privately as
described in [SECURITY.md](SECURITY.md).

## Every commit: sign-off (DCO) and signature

faubourg uses the [Developer Certificate of Origin](https://developercertificate.org/) (DCO)
instead of a CLA. A sign-off certifies that you wrote the change, or otherwise have the right
to submit it under the project's [Apache-2.0 licence](LICENSE).

**Every commit needs both:**

1. **A `Signed-off-by:` trailer** matching the commit author (`git commit -s`).
2. **A cryptographic signature**, GPG or SSH (`git commit -S`). CI verifies signatures on every
   pull request, and a pull request with an unsigned commit cannot merge.

```sh
git commit -s -S -m "fix(api): describe the change"
```

To fix commits that are missing either, rewrite your branch before pushing again:

```sh
git rebase --signoff -S origin/main
git push --force-with-lease
```

`make dco-check` runs the same check CI does.

## How a change gets in

faubourg is built ADR-first, under
[Architecture Driven Development](.claude/rules/architecture-driven-development.md):

```text
ADR  ->  CALM  ->  TDD  ->  implement  ->  docs  ->  threat model
```

- **Architecturally significant changes** (a trust boundary, credential handling, an API
  surface, a new binary, or the contract with banlieue) start with an ADR in
  [`docs/adr/`](docs/adr/) and an update to the CALM model in
  [`docs/architecture/calm/`](docs/architecture/calm/). Open the ADR as its own pull request,
  or as the first commit of the change, so the decision can be discussed before code is
  reviewed.
- **Everything else** (bug fixes, docs, refactors that keep behaviour) needs a test first and a
  green quality gate.

Rules worth knowing before you write code:

- **Fail closed.** An unsupported or unimplemented path returns an error, with a test proving
  the refusal ([`testing.md`](.claude/rules/testing.md)).
- **No secrets in logs.** SSH keys, passwords, passphrases and bootstrap command lines are
  never logged, formatted with `Debug`, or compared in assertions.
- **No real infrastructure.** This repository is public: placeholders only
  ([`no-real-infrastructure.md`](.claude/rules/no-real-infrastructure.md)).
- **banlieue owns the host agent and VM placement** ([ADR-0002](docs/adr/0002-banlieue-provider-is-the-host-agent.md)).
- **No em-dashes** in prose or comments.

## Building and testing

You need the toolchain in [`rust-toolchain.toml`](rust-toolchain.toml) and
[`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny). Node.js is needed for the CALM
targets and [Poetry](https://python-poetry.org/) for the docs site.

```sh
make check          # fmt, clippy (pedantic, -D warnings), tests, cargo-deny
make crds           # regenerate deploy/crds/ after changing the API types
make calm-validate  # after changing docs/architecture/calm/architecture.json
make docs-serve     # preview the documentation site at http://127.0.0.1:8000
```

`make help` lists every target.
