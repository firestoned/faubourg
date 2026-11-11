<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0017: Draining a host asks banlieue to move its VMs within the zone

**Status:** Accepted · **Date:** 2026-11-10 · **Revised:** 2026-11-11 (decisions 8 to 10,
from roadmap 03's open questions) · **Accepted:** 2026-11-11

Amends ADR-0004 Decision 3 and ADR-0003 Decision 4. The moving side is banlieue ADR-0127.

## Context

A cordon (ADR-0003 Decision 5, honoured by banlieue since its ADR-0124) stops new VMs landing
on a host and moves nothing. Taking a host down for maintenance therefore still loses every VM
on it, or waits until each one is deleted. ADR-0004 Decision 3 said "no live migration" because
the only migration it considered needs shared storage, a migration network and fencing.

banlieue ADR-0127 proposes moving a running Cloud Hypervisor VM between two hosts of one zone
without shared storage: the source host's banlieue provider copies the disk (mostly while the
guest runs, using reflink as dirty tracking) and the VMM state to the destination's, and the
guest runs on exactly one host throughout. VMs with a vTPM (every sealed guest) move only from
its Stage 2, once Cloud Hypervisor carries vTPM state in its own migration stream; until then
banlieue rejects them, and a drain reports them as blocked (Decision 4). It splits the work the way
the cordon is split: **faubourg decides when and where, banlieue does it.** banlieue offers a
request object, `VirtualMachineMove`, and admits a move only between Providers that carry the
same `banlieue.io/zone` label (among other checks).

faubourg already knows which hosts form a zone and which host an operator wants empty. It does
not know a VM's guest uid, swtpm state or machine directory, and must not (ADR-0002: banlieue
owns the host agent and VM placement).

## Decision

1. **faubourg labels each host's `Provider` with banlieue's zone key.** Alongside
   `faubourg.firestoned.io/zone`, faubourg owns `banlieue.io/zone: <zone name>` on every
   member host's `Provider`, by the same field manager and the same server-side apply, exactly
   as it owns `banlieue.io/unschedulable` for the cordon. Removing a Host from a zone removes
   the label. This is faubourg's statement that the zone's hosts share the L2 segments behind
   their network classes, which is what makes a moved guest's addresses still valid; a zone
   that does not meet that must not be one zone.

2. **`Host.spec.maintenance: Drain`** (new; `None` by default) asks for the host to be emptied.
   It implies the cordon: faubourg labels the `Provider` cordoned first, so nothing new lands
   while the host drains.

3. **Draining creates one `VirtualMachineMove` per VM on the host, a few at a time.** faubourg
   lists the `VirtualMachine`s whose `status.scheduled.providerName` is the host's `Provider`
   and creates a `VirtualMachineMove` for each in the VM's namespace, with **no target**:
   banlieue picks the destination inside the zone with its scheduler, which knows capacity and
   spread and faubourg does not. Among the hosts that admit the move, the one with the most
   free memory after it wins, free vCPUs breaking a tie: a drain spreads its VMs across the
   zone instead of piling them onto one neighbour, and every host keeps headroom. This ranking
   is for moves only; new placements keep banlieue's spread (its ADR-0098). It is banlieue's to
   implement (raised for its ADR-0127 decision 2). At most a fixed number of moves per draining host are in
   flight at once (a named constant, starting at one, raised only by measurement). faubourg
   labels each move with the Host's name, so a drain's moves can be listed and cancelled
   together.

4. **faubourg never forces.** A move banlieue rejects (`migrationPolicy: Never`, no host in
   the zone that fits, no `move` capability) or that fails is reported, never retried in a
   loop and never replaced by deleting the VM. The Host's condition `Drained` is `True` when
   no VM is scheduled on it, and otherwise `False` with reason `MovesBlocked` and the blocked
   VMs named (bounded, as banlieue's scheduling messages are). The operator decides what
   happens to those: change the VM's policy, delete it, or power the host off anyway.

5. **Clearing `maintenance` cancels what has not started.** faubourg deletes its moves that
   are still `Pending`, `Preparing` or `Copying` (banlieue cancels a move deleted before
   `HandingOver`), and leaves the rest to finish. The cordon stays until `cordoned` is cleared,
   as it is a separate field.

6. **ADR-0004 Decision 3 is amended**: "no migration through shared storage; a VM moves only
   by copy, between hosts of one `Local` zone, performed by banlieue (banlieue ADR-0127)".
   Decision 4 is unchanged: **no fencing**, and so no restarting a dead host's VMs elsewhere.
   A move needs both hosts alive; a host that is down is not drained, its VMs are lost as
   before.

7. **RBAC grows by exactly what draining needs**: `get`, `list`, `watch` on banlieue
   `virtualmachines` (to find a host's VMs; cluster-wide, since VMs are namespaced and a host
   serves every namespace), and `create`, `get`, `list`, `watch`, `delete` on
   `virtualmachinemoves`. No `update` or `patch` on `virtualmachines`, no access to
   `cloudhypervisormachines`, no Secrets. The `Provider` label set grows by one key.

8. **Pool members are moved like any VM; faubourg never special-cases them.** An unclaimed
   member (a `banlieue.io/pool` label and no `banlieue.io/claim`) may well be cheaper to
   replace than to move, but replacing it means deleting a VM, which faubourg never does
   (decision 4), and which only the pool may do to its own members. So faubourg asks for a move
   of every VM on the host, members included, and banlieue decides how to satisfy a move of an
   unclaimed member (move it, or let its pool replace it elsewhere and report the move done).
   That choice belongs in banlieue's ADR-0127, not here.

9. **A zone reports whether its hosts can take each other's VMs.** banlieue admits a move
   only between hosts with the same VMM version and CPU model (banlieue ADR-0127 decision 3).
   The zone controller (ADR-0015) reads both from its members' Providers (the failure domain's
   `attributes.raw.vmm` and `attributes.raw.cpuModel`, which the Cloud Hypervisor provider
   already publishes) and sets a zone condition **`HostsMatch`**: `True` when every member
   with a Provider reports the same pair, `False` with reason `HostsDiffer` naming the hosts
   that differ from the most common pair (bounded), and `Unknown` with reason
   `HostsUnreported` while a member's Provider lacks either field. It is advisory: it does not
   change `Ready`, and a drain still runs, so banlieue's own check stays the one that admits a
   move. The L2 promise of decision 1 cannot be observed and is stated in the zone docs only.

10. **banlieue owns the key.** faubourg spells neither `banlieue.io/zone` nor
    `VirtualMachineMove`'s group or fields: both come from `banlieue-api` (as
    `LABEL_UNSCHEDULABLE` does), so decision 1 waits for the `banlieue-api` release that
    exports them. faubourg reserves `banlieue.io/zone` in a zone's labels, as it reserves
    `banlieue.io/unschedulable`.

## Consequences

- An operator drains a host with one field, and the zone keeps its VMs running, with a pause
  per VM that banlieue reports (`VirtualMachineMove.status.downtimeMilliseconds`).
- faubourg's reach grows from "labels on Providers" to "may ask for any VM on a host it manages
  to move inside that host's zone". It still cannot choose a VM's destination outside the zone,
  change a VM, or delete one: banlieue admits or rejects every move.
- A zone now carries a promise about its network (Decision 1). The zone docs and
  `AvailabilityZone` validation must say so; a mixed-network zone breaks moved guests'
  addresses rather than failing a check.
- Depends on banlieue ADR-0127 being implemented (Stage 1 at least) and on banlieue honouring
  `banlieue.io/zone`. ADR-0127 must also settle which `CloudHypervisorMachine` a moved VM uses
  afterwards: banlieue's VM reconciler finds a VM's machine by the VM's own name, and the
  destination machine is named `<vm>-<move id>`, so without that a move is followed by a
  recreate (roadmap 03).
- Until banlieue's Stage 2, every VM with a vTPM (every sealed pool member) is reported
  blocked: a drain empties a host of non-vTPM VMs only. Until then `maintenance: Drain` must fail closed: faubourg refuses it
  with a condition when the cluster has no `VirtualMachineMove` CRD.
- Threat model: a new row for a compromised or misconfigured faubourg asking banlieue to move
  VMs (D: moves churn a zone; mitigated by banlieue's admission checks and the in-flight limit)
  and the new RBAC.
