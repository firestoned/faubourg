<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0016: The CA hosts trust for the API server can be set, pinned by digest

**Status:** Accepted · **Date:** 2026-11-09

## Context

The bootstrap pod writes the host's kubeconfig (ADR-0011 decision 9): the address
`--provider-api-server`, and as `certificate-authority-data` the cluster CA, read from the pod's
own `kube-root-ca.crt`. That is right when the hosts reach the API server directly. When they
reach it through something that presents another certificate (a load balancer terminating TLS,
an ingress with a public certificate), the provider on the host refuses the connection, and the
Host stays `Bootstrapping` (ADR-0011 consequences: "needs a CA option, not built yet").

That CA is a credential's trust anchor. A host trusts whatever it names to be the API server,
and sends it the provider's token. So whoever can choose it can impersonate the API server to
every host, and collect their tokens. It needs the same protection the install script got in
ADR-0013: the platform administrator decides it, and a script maintainer cannot.

## Decision

1. **An optional CA source, pinned in A-8.** The bootstrap policy's parameters gain
   `providerCASource`: `ConfigMap/<name>/<key>@sha256:<64 hex>`, a PEM bundle in the credentials
   namespace and the sha256 of exactly that key's content. Empty or absent means the cluster CA,
   as today. A value that is not of that form refuses every Job, as a malformed `scriptSources`
   does.
2. **The controller reads it from A-8** (it already reads that one ConfigMap, ADR-0013) and,
   when it is set, mounts that key as a read-only `provider-ca` volume in each Job and passes the
   pinned digest as a literal `FAUBOURG_PROVIDER_CA_SHA256`. The digest is a bootstrap input:
   changing the CA re-bootstraps every Host, through the rollout (ADR-0013 decision 5), so each
   host gets a kubeconfig that trusts the new one.
3. **The admission policy admits the volume only as pinned.** A `provider-ca` volume must be
   that one ConfigMap and key, and `FAUBOURG_PROVIDER_CA_SHA256` must be its pin, exactly once;
   without a pin, a Job may not mount one.
4. **The image checks it before any connection.** It hashes the mounted bundle and exits 67
   (`ProviderCAUnpinned`, failing the Job at once like 66 for the script) if the digest differs;
   it refuses a bundle with no certificate. It then writes that bundle, not the cluster CA, into
   the host's kubeconfig. Its own calls to the API server (minting the token) still use
   `kube-root-ca.crt`: it runs in the cluster.
5. **The controller's flag stays an address.** `--provider-api-server` is unchanged; this ADR
   only adds what the hosts should trust at that address.

## Alternatives considered

- **A controller flag naming the CA.** The controller's configuration is the platform
  administrator's too, but it is a second place for a trust decision A-8 already holds, and the
  admission policy could not check it.
- **The CA inline in A-8.** A-8 would carry certificates; a ConfigMap and a digest keep it a
  list of pins, like `scriptSources`.
- **Trust the system store on the host.** Every host's store becomes the trust anchor for its
  token, outside anything faubourg can check.

## Consequences

- A-8, the admission policy, the Job, the image and the inputs hash each gain the CA, alongside
  the script; `vap-test`, `bootstrap-test` and the unit tests cover a pinned, an unpinned and a
  missing bundle.
- Without `providerCASource`, nothing changes: Jobs, inputs and kubeconfigs are as before, and
  no host re-runs on upgrade.
- ADR-0011's open consequence closes, and roadmap 01's CA item with it.
