<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0011: The host's Provider and its identity

**Status:** Accepted · **Date:** 2026-11-07

## Context

A bootstrapped host runs banlieue's host-resident Cloud Hypervisor provider (ADR-0002). In
banlieue, that provider is an `External` Provider (banlieue ADR-0060 decision 3): its operator
creates the Provider's ServiceAccount, Role and Lease but no Deployment, and the process on
the host authenticates as that ServiceAccount with a bound token it renews itself (banlieue
ADR-0060 decision 5). Two things are left to whoever adds the host:

- **The Provider.** ADR-0003 decision 4 says faubourg creates it and owns its zone, host and
  cordon labels. Until now nothing did: an operator labelled one by hand (ADR-0009
  decision 5, accepted risk R-7).
- **The first token.** banlieue issues it with `banlieue bootstrap cloud-hypervisor-host`,
  run by an administrator against the cluster, and the files are copied to the host by hand.
  ADR-0005 left open how faubourg does that.

The obvious design, the controller requesting the token, fails on two counts:

- banlieue's External Providers must live in its operator's namespace, where the operator
  can grant token renewal. `create` on `serviceaccounts/token` there cannot be limited to
  names that do not exist yet, so it would also mint tokens for banlieue's operator.
- The token would then pass through the controller, and from it into a Job or a Secret. The
  controller holds no credential today (ADR-0008), and should not start.

## Decision

1. **One Provider per Host, named after it.** It lives in banlieue's namespace
   (`--provider-namespace`, default `banlieue-system`) and references one ProviderClass
   (`--provider-class`, default `cloud-hypervisor`), which must be `External`. A Host whose
   class is missing or not `External` is `Failed`, reason `ProviderClassNotExternal`, and gets
   no Job.
2. **For a bootstrapped Host (`bootstrap.method: Script`), faubourg creates the Provider.** It
   applies it by server-side apply under `faubourg.firestoned.io/host-controller`, owning:
   - `spec.providerClassRef` and `spec.connection.endpoint` (`ch://<address>`);
   - the labels of ADR-0003: `faubourg.firestoned.io/zone`, `faubourg.firestoned.io/host`,
     the zone's `providerLabels`, and `faubourg.firestoned.io/cordoned: "true"` while the Host
     is cordoned.
   No Job is created until banlieue's operator has given the Provider its identity, which it
   reports as `status.workload.mode: External`. Until then the Host is `Pending`, reason
   `AwaitingProviderIdentity`.
3. **For an adopted Host (`bootstrap.method: None`), faubourg only labels.** It applies the same
   labels, and nothing else, to an existing Provider named after the Host. It never creates
   one and never writes an adopted Provider's spec. With no such Provider the Host waits, as
   before (`ProviderMissing`).
4. **faubourg never deletes a Provider.** Not when its Host is deleted, cordoned or
   `Degraded` (ADR-0007 decision 3). The Provider carries no ownerReference to the Host.
   Removing a host is a separate decision (ADR-0009 decision 13).
5. **The host's Provider is still the one carrying its label** (ADR-0009 decision 5). faubourg
   now owns that label on the Provider named after the Host. A second Provider carrying it is
   still `ProviderAmbiguous`.
6. **The first token is minted by the bootstrap pod, never by the controller.** The bootstrap
   pod gets one more volume, `kube-api`: a projected token for its own `faubourg-bootstrap`
   ServiceAccount (at most 10 minutes, the API server's default audience) and the cluster CA
   (`kube-root-ca.crt`). With it, the image requests a token for the Provider's
   ServiceAccount through the TokenRequest API:
   - its name is banlieue's `workload_name(<class>, <Host>)`, which the controller computes
     with banlieue's algorithm and passes in the Job's literal environment, with the Provider's
     name and namespace and the API server address the host uses
     (`--provider-api-server`);
   - its lifetime is 24 hours (banlieue's default), and it is bound to no object, so it
     outlives the pod.
   The token reaches the host and nowhere else: not the controller, not the Job's spec, not a
   Secret, not etcd. `automountServiceAccountToken` stays `false`.
7. **RBAC.** `faubourg-bootstrap`, which held nothing, gets `create` on
   `serviceaccounts/token` in banlieue's namespace. The controller gets `get`, `list`, `watch`,
   `create` and `patch` on Providers there, and `get` on ProviderClasses. The controller still
   has nothing on Secrets or on `serviceaccounts/token`, anywhere.
8. **An admission policy bounds those TokenRequests.** A new ValidatingAdmissionPolicy,
   `faubourg-provider-tokens`, matches TokenRequests from the `faubourg-bootstrap`
   ServiceAccount and admits one only when:
   - the target ServiceAccount's name starts with `banlieue-provider-<class>-` (the parameter
     `providerServiceAccountPrefix`), so the bootstrap identity cannot mint a token for
     banlieue's operator or for a provider of another class;
   - `expirationSeconds` is set and at most 86400;
   - it names no audience and binds to no object.
   ADR-0008's Job policy admits the `kube-api` volume only in exactly that shape: one
   `serviceAccountToken` source with no audience and `expirationSeconds` at most 600, and the
   `kube-root-ca.crt` ConfigMap.
9. **The credential reaches the host on the install script's standard input.** The run step
   (ADR-0010 decision 6) writes a bundle to `sudo -n /opt/faubourg/install.sh`'s stdin. Stdin
   is not an argument, so the sudo rule (ADR-0005 decision 2) is unchanged. Format, version 1,
   lines of text:
   ```
   faubourg-provider-credential v1
   provider <Provider name>
   namespace <Provider namespace>
   token <token>
   kubeconfig
   <kubeconfig, to the end of the input>
   ```
   The kubeconfig names the API server, the cluster CA, and a user that reads its token from
   `/etc/banlieue/credentials/token` (banlieue's path), so it holds no secret. The default
   install script writes both files there, owned by the `banlieue` user, mode `0600`, through
   temporary files, and passes the Provider's name and namespace to banlieue's installer. A
   script that ignores its stdin still runs; its provider never authenticates, and the Host
   stays `Bootstrapping`.
10. **From then on the host renews its own token** (banlieue ADR-0060 decision 5). A
    re-bootstrap mints a fresh one, which is banlieue's recovery for a host that stayed down
    longer than one token lifetime.
11. **Configuration.** `--provider-api-server` is required: without it there is no address to
    give a host, and the controller refuses to start. It must be an `https://` URL.
12. **Logs** carry the Provider and ServiceAccount names and the token's expiry. Never the
    token, never the bundle.

## Consequences

- R-7 closes for bootstrapped hosts: faubourg owns the label on their Provider. Whoever can
  label Providers can still attach a second one and make the Host `ProviderAmbiguous`, which
  fails closed.
- A compromised controller can still make the image deliver a provider token to an address
  of its choosing, by pairing a password or first-use credential with it (R-5). The token
  is a provider's for one day, not a key, and a `knownHosts` key confines it to the pinned
  host.
- The bootstrap pod now has API access, bounded by RBAC and by the token policy to minting
  provider tokens of one class. The bootstrap image is already the fleet's root of trust
  (ADR-0008 decision 5).
- faubourg re-derives banlieue's ServiceAccount name. The algorithm is pinned by a test
  against banlieue's own outputs; publishing the name in the Provider's status, or the naming
  function in `banlieue-api`, would remove the copy, and is worth asking banlieue for.
- The host must trust the cluster CA for `--provider-api-server`. An API server reached
  through a different certificate authority (a load balancer) needs a CA option, not built yet:
  until then such a provider fails TLS and its Host stays `Bootstrapping`.
- The default install script changes: it reads the bundle from stdin. A replaced script must
  do the same, or install the credential some other way.

*Amended 2026-11-08, after the second live test (roadmap 01):* decisions 2 and 3 put the
Provider's spec and its labels under one field manager. Server-side apply removes a field when
the only manager that owned it stops sending it, so a Host switched from `Script` to `None`
sent labels only, the API server refused a Provider with no `spec`, and the Host never
reconciled again. faubourg now applies the Provider as two managers:

- `faubourg.firestoned.io/host-provider-spec` owns `spec.providerClassRef` and
  `spec.connection.endpoint`. It is applied first, and only for a `Script` Host.
- `faubourg.firestoned.io/host-controller` owns the labels, for every Host, and nothing in
  `spec`.

A Host switched to `None` therefore keeps its Provider's spec as faubourg last set it: faubourg
stops writing it, and does not remove it. A Provider whose spec the labels' manager still
owns, as a v0.1 controller left it, is moved first: its current class and endpoint are applied,
unchanged, under the spec manager, so the labels' apply that follows removes nothing. That is
the only write faubourg makes to an adopted Provider's spec, and it changes no value.
`controller-test` covers both.
