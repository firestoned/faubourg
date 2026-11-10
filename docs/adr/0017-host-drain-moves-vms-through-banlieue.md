<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0017: Draining a host asks banlieue to move its VMs within the zone

**Status:** Proposed · **Date:** 2026-11-10

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
   spread and faubourg does not. At most a fixed number of moves per draining host are in
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
  `banlieue.io/zone`. Until then `maintenance: Drain` must fail closed: faubourg refuses it
  with a condition when the cluster has no `VirtualMachineMove` CRD.
- Threat model: a new row for a compromised or misconfigured faubourg asking banlieue to move
  VMs (D: moves churn a zone; mitigated by banlieue's admission checks and the in-flight limit)
  and the new RBAC.
