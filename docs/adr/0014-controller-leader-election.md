<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0014: The controller elects a leader through a Lease

**Status:** Accepted · **Date:** 2026-11-08

## Context

The Host controller runs as one replica with the `Recreate` strategy, and has no leader
election (ADR-0009 decision 1). That was safe while every write was idempotent: a Job's name
comes from its Host, and status is written by server-side apply, so two instances overlapping
during a restart could only repeat each other.

ADR-0013 ended that. The rollout counts re-bootstrap Jobs across Hosts and allows at most
`--max-rebootstraps` at once, and the controller reconciles one Host at a time so the count is
exact. Two controllers acting at once could each see the last free slot, and two re-bootstraps
would run root's install at once. One replica also means the controller is down for every
restart, node drain and upgrade: nothing on the hosts depends on it, but no Host is
reconciled, cordoned or bootstrapped meanwhile.

banlieue's controllers already elect a leader through a `coordination.k8s.io/v1` Lease
(`banlieue-provider-sdk::leader`). That crate is not published, and its take-over is an
unconditional patch: two standbys that see the Lease expire at once can both believe they
won, until the next renewal. For banlieue that is a short overlap of idempotent work. For
faubourg it is the overlap the rollout must not have.

## Decision

1. **A Lease, `faubourg-controller` in `faubourg-system`.** Its `holderIdentity` is the pod's
   name, from the downward API as `POD_NAME`; outside a pod, `POD_NAME` must be set by hand.
   The controller refuses to start without one: a guessed identity could match another's. Lease duration 15
   seconds, renewed every 5, retried every 2: banlieue's timings, and client-go's.
2. **Acquiring and taking over are conditional writes.** A candidate creates the Lease when
   there is none, or replaces it with the `resourceVersion` it read when the holder has not
   renewed within the lease duration. A second candidate's write then fails (`AlreadyExists`,
   or a conflict), and it goes back to waiting. Exactly one candidate wins.
3. **Only the leader runs the controller.** A standby watches nothing and writes nothing but
   its attempts on the Lease.
4. **The leader stops before its lease can lapse.** It must renew within a deadline of 10
   seconds, shorter than the lease. If it has not, it exits, and its pod restarts as a
   candidate. So a standby can take over only after the old leader has stopped acting, given
   clock skew between nodes under 5 seconds (the lease duration less the deadline). This,
   with decision 2, keeps the rollout's count exact across a failover.
5. **A leader shutting down releases the Lease.** On SIGTERM it stops the controller, then
   clears `holderIdentity`, so a standby takes over at its next retry instead of waiting out
   the lease.
6. **Two replicas.** The Deployment runs 2 replicas with a rolling update and a preferred
   anti-affinity across nodes. The controller still reconciles one Host at a time
   (ADR-0013); a second replica is for availability, not throughput.
7. **The RBAC is one Lease.** A Role in `faubourg-system` grants `get` and `update` on the
   Lease `faubourg-controller` by name, and `create` on Leases there (Kubernetes cannot
   restrict `create` by name). Nothing else on Leases, and none outside that namespace.
8. **There is no switch to turn it off.** Every controller elects, tests included. A flag
   that disables election would be one way back to two actors.
9. **The logic is a pure function.** What a candidate does next (create, renew, wait, take
   over) is decided from the time, the Lease it read and its identity, and unit-tested; the
   loop around it only reads, writes and sleeps (ADR-0009 decision 2).

## Alternatives considered

- **banlieue's `leader` module.** Not published, and its unconditional take-over is the
  overlap decision 2 removes. faubourg follows its design and timings otherwise; if banlieue
  publishes a crate with conditional writes, faubourg can move to it.
- **A third-party crate** (`kube-leader-election`, `kube-lease-manager`). Each would be a
  dependency to vet against `kube` 5 and `cargo deny` for about 200 lines of logic that this
  ADR wants tested in its own terms.
- **Stay at one replica.** Keeps the rollout exact, but leaves the outage on every restart,
  and nothing would stop an operator who scales the Deployment to two.

## Consequences

- ADR-0009 decision 1 is superseded in part: two replicas, one leader, `RollingUpdate`.
- Failover takes about 15 seconds when a leader dies (the lease runs out), and about 2 when it
  shuts down cleanly (decision 5).
- The controller gains RBAC on one Lease. `vap-test` checks it can manage that Lease and
  nothing else of the kind; `controller-test` runs two controllers and checks only one acts,
  then that the other takes over.
- A leader partitioned from the API server stops within 10 seconds, so a partition costs
  availability, never a second actor.
- There is no health server yet, so readiness does not depend on leadership. When one comes,
  banlieue ADR-0093's rule applies: a standby is ready, since it can take over.
