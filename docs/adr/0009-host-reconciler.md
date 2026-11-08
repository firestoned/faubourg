<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0009: The Host reconciler

**Status:** Accepted · **Date:** 2026-11-07

## Context

ADR-0005, ADR-0007 and ADR-0008 decide what a Host's bootstrap Job looks like, what phase a
Host is in, and what the admission policy admits. `faubourg-controller` implements each of
those as a pure function (`job.rs`, `phase.rs`, `health.rs`), but nothing runs them against a
cluster yet. A running controller has to settle what those ADRs leave open:

- which objects it watches, and which of them it trusts as its own;
- when a Host gets a new Job. ADR-0005 decision 8 says "a new generation of the Host spec",
  but a Host's generation also moves when it is cordoned or moved to another zone, and neither
  should re-run an install on a hypervisor;
- what happens to a failed Job, and to Jobs from earlier inputs;
- which banlieue `Provider` is the host's, before faubourg creates Providers itself;
- how it is deployed, configured, and what it may log.

## Decision

1. **One binary, one replica.** `faubourg-controller` runs as a Deployment in
   `faubourg-system` under the `faubourg-controller` ServiceAccount, with `replicas: 1` and
   the `Recreate` strategy. There is no leader election yet. Two instances overlapping
   briefly are safe because every write is idempotent: a Job is created under a name derived
   from its Host, and status is written by server-side apply.
2. **Each reconcile is a plan, computed by a pure function.** The reconciler reads the Host,
   its zone, its Jobs and its Provider, and a pure `plan` function returns the Job to create,
   the Jobs to delete and the status to write. The I/O around it does nothing else, so every
   decision is covered by a unit test that needs no API server.
3. **What it watches, and what it counts as its own.**
   - `Host`, the primary object.
   - Jobs in the credentials namespace. Only a Job whose controller `ownerReference` carries
     the Host's UID is that Host's Job. A Job carrying the right labels but another owner, or
     no owner, is ignored, whoever created it.
   - `AvailabilityZone`, mapped to the Hosts whose `zoneRef` names it.
   - `Provider` in banlieue's namespace (configured, default `banlieue-system`), mapped to a
     Host by the `faubourg.firestoned.io/host` label.
   - Pods in the credentials namespace are read only for a failed Job, for its exit code.
4. **The zone gates the bootstrap.** No Job is created for a Host whose `AvailabilityZone` is
   missing or refused by its own validation (a `Shared` zone, ADR-0004). The Host is
   `Pending`, or `Degraded` if it was `Ready`, with reason `ZoneNotFound` or
   `ZoneUnsupported`. A Job already running is not stopped by a zone change.
5. **The host's Provider is the one Provider labelled with its name.** In banlieue's
   namespace, the Provider with `faubourg.firestoned.io/host: <host>` is the host's. None is
   `ProviderMissing`. More than one is unhealthy, reason `ProviderAmbiguous`: faubourg does not
   pick one. This reconciler does not create or label Providers; that is the next roadmap 01
   item, under the field manager ADR-0003 decision 4 names. Until then, an operator labels the
   Provider of an adopted host (`bootstrap.method: None`), and a bootstrapped host stays
   `Bootstrapping` until a labelled Provider is healthy.
6. **A new Job when the bootstrap inputs change, not on every generation.** This amends
   ADR-0005 decision 8. The bootstrap inputs are the Host's `address`, `ssh` and `bootstrap`
   fields, with the script source resolved against the controller's default. Their sha256,
   over a canonical JSON encoding, is the Job's `faubourg.firestoned.io/bootstrap-inputs`
   annotation.
   - The Host's current Job is its newest owned Job whose annotation matches. If there is
     none, the reconciler creates one, named for the current generation (`job::job_name`).
   - Every other owned Job is deleted, a running one included: its inputs are stale.
   - `zoneRef` and `cordoned` are not inputs, so cordoning a host or moving it to another zone
     runs nothing on it. Neither is the bootstrap image: upgrading the controller or its image
     does not re-run every host's install. A host picks up a new image at its next
     re-bootstrap.
   - A changed script *content* under the same source name is still not seen, because the
     controller cannot read ConfigMaps there (ADR-0008 decision 6). That stays open in
     roadmap 01.
7. **A failed bootstrap is not retried by the controller.** The Job's own `backoffLimit`
   retries transient failures (ADR-0008). Once the Job has failed, it stays as the record and
   the Host stays `Failed`, until its bootstrap inputs change or an operator deletes the Job.
   A wrong key or a refused host key is never retried in a loop.
8. **How a Job's state is read.** `Complete=True` is `Succeeded`. `Failed=True` or
   `FailureTarget=True` is `Failed`. The cause comes from the exit code of the `bootstrap`
   container in the Job's pods: 64 is `ScriptDrift`, 65 is `HostKeyRefused`, anything else,
   or no pod to read, is `BootstrapJobFailed`. Anything else is `Running`.
9. **Status is written by server-side apply, and only when it changes.** The field manager is
   `faubourg.firestoned.io/host-controller`. It writes `phase`, `observedGeneration`,
   `message` and a `Ready` condition, `True` only in phase `Ready`, whose
   `lastTransitionTime` moves only when its status does. A Host the Job builder refuses (no
   script source, for instance) is `Failed` with the builder's reason, and gets no Job.
10. **Host names are at most 63 characters, refused by the CRD.** A Host name is a label value
    on its Job, its pods and its Provider. A CEL rule on the Host CRD refuses a longer name
    on create, so the builder's own refusal is no longer the only check.
11. **Configuration fails closed.** The controller takes the credentials namespace, the
    bootstrap image, the bootstrap ServiceAccount, the default script source and banlieue's
    namespace as flags, each with an environment variable. It refuses to start with an image
    that is not pinned by digest, or a namespace or name that is not a valid Kubernetes name.
    The image must also be the one in the admission policy's parameters, or the API server
    refuses every Job: also fail closed.
12. **RBAC.** On top of ADR-0008 decision 6: `get`, `list` and `watch` on Providers in
    banlieue's namespace, and `list` and `watch` on AvailabilityZones, which the ClusterRole
    already grants. Nothing on Leases: banlieue's operator writes the heartbeat into the
    Provider's status, which supersedes the Lease access in ADR-0007's consequences. Nothing
    new in the credentials namespace.
13. **No finalizer.** Deleting a Host deletes its Jobs through their `ownerReference`.
    Nothing on the host itself is undone; removing a host is a separate decision.
14. **Logs** carry Host, zone and Job names, phases and reasons. Never a spec value beyond a
    name, never an address, never a Secret's content.

## Consequences

- Who can label Providers in banlieue's namespace can make a Host `Ready`. They can already
  create a Provider banlieue places VMs on, so this grants nothing new; when faubourg creates
  Providers itself, it owns the label by field manager.
- Re-running a host's install needs a change to its bootstrap inputs, or deleting its Job.
  Neither cordon nor a controller upgrade does it.
- A one-replica controller is unavailable while it restarts. Nothing on the hosts depends on
  it being up: banlieue keeps placing on healthy Providers, and Hosts' phases catch up on the
  next reconcile.
- Changing the canonical encoding of the bootstrap inputs re-runs every host's install once.
  The encoding is pinned by a test, so such a change is deliberate and visible in review.
- The reconciler is tested end to end against a real API server (`make controller-test`,
  kind): a Host gets its Job, the admission policy admits it, and an adopted Host reaches
  `Ready` from a labelled, healthy Provider.

*Amended 2026-11-07 by ADR-0011:* decision 5's interim, an operator labelling the Provider, is replaced: faubourg creates the Provider of a bootstrapped host and labels an adopted one. No Job is created before the Provider has its identity.

*Superseded in part 2026-11-08 by ADR-0014:* decision 1's single replica. The controller now
runs two replicas, of which only the holder of the Lease `faubourg-controller` acts, with a
rolling update instead of `Recreate`. Every other decision stands.
