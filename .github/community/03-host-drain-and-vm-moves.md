<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# 03: Host drain through banlieue VM moves

Implements ADR-0017: `Host.spec.maintenance: Drain` empties a host by asking banlieue to move
each of its VMs to another host of the same zone. faubourg decides when and which host; banlieue
decides where and performs the move (banlieue ADR-0127, its roadmap 14). ADD cycle per item.

**Status: accepted 2026-11-11, not started.** ADR-0017 is Proposed, revised 2026-11-11 with
decisions 8 to 10 from the open questions below; banlieue ADR-0127 is Proposed (2026-11-10).

## What a drain can move, by banlieue stage

banlieue ADR-0127 is built in stages, and a drain is only as useful as the stage banlieue has
released. faubourg's side is the same from Stage 1 on; what changes is how many VMs banlieue
admits.

| banlieue stage | Moves | What a drain on a Cloud Hypervisor host does |
| --- | --- | --- |
| none (today) | no `VirtualMachineMove` CRD | `maintenance: Drain` is refused with a condition (fail closed, ADR-0017) |
| 1 | VMs without a vTPM; pause grows with RAM | moves those; every `tpmEnabled` VM (every sealed pool member) is reported `MovesBlocked` (`VTPMNotMovable`) |
| 2 | vTPM VMs too, sealed ones included | moves every VM whose policy allows it |
| 3, 4 | shorter pause, no banlieue disk copy | no faubourg change |

So until Stage 2, draining a host that runs mostly sealed pool members empties little of it.
That is acceptable: the cordon already stops new VMs, and the operator sees exactly which VMs
block the drain.

## Open questions

- [ ] **Which machine a moved VM uses** (banlieue ADR-0127, a blocker for its Stage 1).
  banlieue's VM reconciler addresses a VM's infrastructure machine by the VM's own name
  (`virtualmachine.rs`, `delete_existing_infra(.., &vm.name_any())`; the builders and the
  status mirror likewise), while ADR-0127 names the destination machine `<vm>-<move id>`.
  After commit, the next pass would find no machine of the VM's name on the destination and
  create a new one: the recreate a move exists to avoid. ADR-0127 needs to decide it, either
  by the VM recording its current machine, or by the destination keeping the VM's name. Not
  faubourg's to decide; tracked here because the drain depends on it.
- [x] **Pool members** (ADR-0017 decision 8). faubourg asks for a move of every VM,
  members included, and never deletes one; whether an unclaimed member is moved or replaced by
  its pool is banlieue's choice inside the move (raised for ADR-0127).
- [x] **A zone promises matching hosts** (ADR-0017 decision 9). The zone controller reports
  `HostsMatch` from its members' `raw.vmm` and `raw.cpuModel`, advisory; the L2 promise is
  stated in the zone docs. Buildable now: Cloud Hypervisor Providers already publish both.
- [x] **Where the zone key comes from** (ADR-0017 decision 10). From `banlieue-api`, with the
  `VirtualMachineMove` type; decision 1 waits for that release.

## Work

- [ ] ADR-0017 accepted, with the answers above; ADR-0004 decision 3 marked amended
- [ ] CALM: faubourg's controller gains the relationship to banlieue `VirtualMachine`s (read)
  and `VirtualMachineMove`s (create, read, delete); `make calm-validate`
- [ ] `banlieue-api` bumped to the release that exports `LABEL_ZONE` and the
  `VirtualMachineMove` type
- [ ] Decision 1: `banlieue.io/zone: <zone>` on each member host's Provider, by the existing
  field manager and server-side apply, removed when the Host leaves the zone; reserved in
  zones beside `banlieue.io/unschedulable`; `make controller-test` checks set and removal
- [ ] Decision 9: zone condition `HostsMatch` from members' `raw.vmm` and `raw.cpuModel`
  (`HostsDiffer`, `HostsUnreported`), advisory, not part of `Ready`; first, as it needs nothing
  from banlieue
- [ ] Decision 2: `Host.spec.maintenance` (`None` | `Drain`), CRD regenerated (`make crds`);
  `Drain` implies the cordon labels
- [ ] Fail closed: with no `VirtualMachineMove` CRD served, `Drain` sets `Drained=False`,
  reason `MovesUnsupported`, and creates nothing; tested
- [ ] Decision 3: the drain planner as a pure function (VMs on the host, moves in flight,
  limit) returning moves to create and delete; one in flight per host
  (`MAX_MOVES_PER_HOST = 1`); moves labelled with the Host's name; table tests
- [ ] Decision 4: `Drained` condition, `True` when no VM is scheduled on the host, else
  `False` with `MovesBlocked` and the blocked VMs named (bounded); never retried in a loop,
  never deleting a VM
- [ ] Decision 5: clearing `maintenance` deletes moves still `Pending`, `Preparing` or
  `Copying`, leaves the rest
- [ ] Decision 7: RBAC (`get`/`list`/`watch` on `virtualmachines`; `create`/`get`/`list`/
  `watch`/`delete` on `virtualmachinemoves`), nothing more; `make controller-test` proves a
  `patch` on a VM is refused
- [ ] kind: a fake `VirtualMachineMove` CRD and a stub that advances phases, so the planner,
  cancellation and the blocked report are tested without hosts (`make controller-test`)
- [ ] Docs: `docs/concepts.md` (drain, what a zone promises), a maintenance guide, README
  status table
- [ ] Threat model: full pass for ADR-0017 (faubourg may now ask banlieue to move any VM on a
  host it manages; cluster-wide read of VMs; the zone label as a security statement), stamp
  bumped
- [ ] Live: two matching Cloud Hypervisor hosts in one zone, a non-vTPM VM drained from one to
  the other with the same MAC and address; a sealed VM reported blocked (Stage 1), then moved
  (Stage 2)
