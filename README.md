<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# faubourg

[![Build](https://github.com/firestoned/faubourg/actions/workflows/build.yaml/badge.svg)](https://github.com/firestoned/faubourg/actions/workflows/build.yaml)
[![Docs](https://github.com/firestoned/faubourg/actions/workflows/docs.yaml/badge.svg)](https://firestoned.github.io/faubourg/)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/firestoned/faubourg/badge)](https://scorecard.dev/viewer/?uri=github.com/firestoned/faubourg)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![FINOS CALM](https://img.shields.io/badge/FINOS_CALM-1.37-blue)](https://calm.finos.org/)

A control plane that groups bare-metal [Cloud Hypervisor](https://www.cloudhypervisor.org/)
hosts into availability zones for [banlieue](https://github.com/firestoned/banlieue), the way
vCenter groups ESXi hosts into clusters.

**Documentation: <https://firestoned.github.io/faubourg/>**

## What it does

banlieue already runs a host-resident provider on each hypervisor and places VMs across
failure domains. faubourg adds what banlieue has no object for
([ADR-0002](docs/adr/0002-banlieue-provider-is-the-host-agent.md)):

- an inventory of **Hosts**, grouped into **AvailabilityZones**
  ([ADR-0003](docs/adr/0003-availabilityzone-and-host-crds.md));
- **bootstrap** of a new host: a per-host Job places one root-owned script,
  `/opt/faubourg/install.sh`, and runs it under a single scoped sudo rule. The default script
  pulls banlieue with curl from any repository, including an internal mirror in an air-gapped
  site, checks its sha256, and runs `banlieue host cloud-hypervisor install`
  ([ADR-0005](docs/adr/0005-bootstrap-runs-one-scoped-install-script.md));
- **SSH credentials** held in one Secret per host in a dedicated namespace, with strict host
  keys by default ([ADR-0006](docs/adr/0006-ssh-credentials.md)). The controller cannot read
  them, and an admission policy keeps any Job but the pinned bootstrap image from mounting them
  ([ADR-0008](docs/adr/0008-bootstrap-jobs-constrained-by-admission-policy.md));
- **zone membership** as a label on each host's banlieue `Provider`, so banlieue users target a
  zone with an ordinary `providerSelector`
  ([placing VMs in a zone](docs/guides/zone-placement.md));
- **zone health** from the providers' own heartbeats, with no faubourg agent
  ([ADR-0007](docs/adr/0007-health-from-the-banlieue-provider.md)).

v1 zones use **local storage only**: no live migration, no fencing
([ADR-0004](docs/adr/0004-v1-local-storage-only.md)).

```yaml
apiVersion: faubourg.firestoned.io/v1alpha1
kind: Host
metadata:
  name: hv01
spec:
  zoneRef: az1
  address: hv01.home.example
  ssh:
    secretRef: { name: hv01-ssh }
    hostKeyPolicy: Strict
  bootstrap:
    method: Script
```

More in [`examples/`](examples/) and the [concepts page](docs/concepts.md).

## Status

Pre-1.0. Released (v0.1.1) and run against one live Cloud Hypervisor host, adopted and then
bootstrapped from nothing. Every control in the [threat model](docs/security/threat-model.md)
exists; read its accepted risks before deploying, and expect the API to change before 1.0.

| Area | State |
| --- | --- |
| Architecture: [ADR-0001 … ADR-0012](docs/adr/README.md), [CALM](docs/architecture/diagrams/system.md), [threat model](docs/security/threat-model.md) | accepted |
| [`faubourg-api`](crates/faubourg-api/): CRDs, fail-closed validation, SSH Secret contract | implemented, tested |
| [`faubourg-controller`](crates/faubourg-controller/): the Host reconciler ([ADR-0009](docs/adr/0009-host-reconciler.md)), its phase machine, `Provider` health and bootstrap Job builder | implemented, tested against a real API server with its own RBAC (`make controller-test`); released |
| [Bootstrap admission policy](deploy/admission/) and [controller RBAC](deploy/rbac/) ([ADR-0008](docs/adr/0008-bootstrap-jobs-constrained-by-admission-policy.md)) | implemented, tested against a real API server (`make vap-test`) |
| [Bootstrap image](images/bootstrap/) ([ADR-0010](docs/adr/0010-bootstrap-image.md)) | implemented, tested against a real OpenSSH host (`make bootstrap-test`); released |
| [Provider creation and the provider's credential](docs/adr/0011-the-hosts-provider-and-its-identity.md) | implemented, tested end to end on kind with the real image (`make e2e-test`) |
| [Signed images and the install file](docs/guides/installing.md) ([ADR-0012](docs/adr/0012-images-signed-and-published-by-digest.md)) | implemented; v0.1.0 and v0.1.1 released and verified |
| A run against a real hypervisor | done: one host adopted, then bootstrapped from nothing ([roadmap 01](.github/community/01-host-controller-and-bootstrap.md)) |
| Zone controller ([ADR-0015](docs/adr/0015-zone-controller.md)) and cordon | zone health implemented, tested on kind (`make controller-test`); cordon advisory until banlieue skips cordoned Providers ([roadmap 02](.github/community/02-zone-health-and-cordon.md)) |
| CI and docs site | [build](.github/workflows/build.yaml), [CALM](.github/workflows/calm.yaml), [docs](.github/workflows/docs.yaml), [CodeQL](.github/workflows/codeql.yaml), [SAST](.github/workflows/sast.yaml), [Scorecard](.github/workflows/scorecard.yaml) |

The status board is [`ROADMAPS.md`](ROADMAPS.md).

## Documentation

| Read | For |
| --- | --- |
| [Concepts](docs/concepts.md) | Zones, hosts, the phase machine, how a zone reaches banlieue's scheduler |
| [Installing](docs/guides/installing.md) | Verifying a release's signatures, and applying its install file |
| [Preparing a host](docs/guides/preparing-a-host.md) | The install script, the sudo rule, placement |
| [SSH credentials](docs/guides/ssh-credentials.md) | Secret keys, host key policy, revoking the placement key |
| [Placing VMs in a zone](docs/guides/zone-placement.md) | `providerSelector` for banlieue users |
| [API reference](docs/reference/api.md) | Every field and every validation reason |
| [ADRs](docs/adr/README.md) | The decisions, and why |
| [Threat model](docs/security/threat-model.md) | STRIDE per trust boundary, accepted risks |
| [Development](docs/development.md) | The ADD cycle, Make targets, CI |

The same pages are published at <https://firestoned.github.io/faubourg/>.

## Layout

| Path | What |
| --- | --- |
| [`crates/faubourg-api/`](crates/faubourg-api/) | CRD types and validation; `make crds` writes [`deploy/crds/`](deploy/crds/) |
| [`crates/faubourg-bootstrap/`](crates/faubourg-bootstrap/) | The bootstrap image's one binary: host key, placement, run, on russh |
| [`crates/faubourg-controller/`](crates/faubourg-controller/) | The Host controller: pure functions first, then the `faubourg-controller` binary |
| [`deploy/admission/`](deploy/admission/) | The bootstrap admission policy and its parameters |
| [`deploy/rbac/`](deploy/rbac/) | Namespaces, identities and RBAC for the controller and its Jobs |
| [`deploy/controller/`](deploy/controller/) | The controller Deployment |
| [`docs/adr/`](docs/adr/) | Decisions, read first |
| [`docs/architecture/calm/architecture.json`](docs/architecture/calm/architecture.json) | The architecture as code; diagrams in [`docs/architecture/diagrams/`](docs/architecture/diagrams/) |
| [`docs/security/threat-model.md`](docs/security/threat-model.md) | STRIDE per trust boundary |
| [`examples/`](examples/) | A zone, a host, the default install script and sudo rule |
| [`docs/design-sketch.md`](docs/design-sketch.md) | The original sketch, superseded where the ADRs differ |
| [`.github/community/`](.github/community/) | Roadmap detail |

## Development

This repository follows Architecture Driven Development, the same cycle as banlieue and
mediatore: ADR, then CALM, then TDD, then implementation, then docs, then the threat-model pass.
See [CONTRIBUTING.md](CONTRIBUTING.md), [`AGENTS.md`](AGENTS.md) and
[`.claude/rules/`](.claude/rules/).

```sh
make check          # fmt, clippy pedantic -D warnings, test, cargo deny
make crds           # regenerate deploy/crds from the types
make calm-validate  # validate the CALM model
make calm-diagrams  # render the Mermaid diagrams
make docs-serve     # preview the docs site at http://127.0.0.1:8000
make vap-test       # the admission policy and RBAC against a kind cluster
```

Every commit is signed and signed off ([DCO](https://developercertificate.org/)).

## Related projects

- [banlieue](https://github.com/firestoned/banlieue): Kubernetes-native VM management; owns the
  host agent and VM placement ([docs](https://firestoned.github.io/banlieue/)).
- [mediatore](https://github.com/firestoned/mediatore): identity broker for banlieue sandbox VMs
  ([docs](https://firestoned.github.io/mediatore/)).
- [firestoned/github-actions](https://github.com/firestoned/github-actions): the composite
  actions CI uses.

## Security

Report vulnerabilities privately, as described in [SECURITY.md](SECURITY.md).

## License

[Apache-2.0](LICENSE)
