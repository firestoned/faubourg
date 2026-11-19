// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! Draining a host through banlieue VM moves (ADR-0017 decisions 2 to 5).
//!
//! faubourg decides *when* and *from which host*; banlieue decides *where* and performs each
//! move (banlieue ADR-0127). A pure function: given the Host, its Provider, every VM and the
//! moves faubourg made for this Host, it says which moves to create, which to delete, and the
//! `Drained` condition. `controller.rs` reads and writes.
//!
//! - One move per VM, with no target, in the VM's namespace, labelled with the Host's name.
//! - At most [`MAX_MOVES_PER_HOST`] in flight; the next VM waits for a slot.
//! - A move banlieue rejected, or that failed, is reported and never retried or replaced by
//!   deleting the VM (decision 4). Deleting that move lets the next pass ask again.
//! - Clearing `maintenance` deletes the moves that have not reached `HandingOver`; banlieue
//!   cancels a move deleted before then (decision 5).
//! - Fails closed: no `VirtualMachineMove` CRD served, no known Provider, or a host that is not
//!   `Ready` creates nothing and says why.

use banlieue_api::banlieue::{
    MovePhase, VirtualMachine, VirtualMachineMove, VirtualMachineMoveSpec,
    move_condition_types::ADMITTED,
};
use banlieue_api::common::LocalObjectReference;
use faubourg_api::LABEL_HOST;

use crate::backend::Backend;
use faubourg_api::host::{Host, Maintenance};
use kube::ResourceExt;

/// Moves in flight at once per draining host (ADR-0017 decision 3). Raised only by measurement.
pub const MAX_MOVES_PER_HOST: usize = 1;
/// At most this many VMs are named in a condition message.
pub const MAX_NAMED_VMS: usize = 10;
/// Kubernetes' limit on an object name.
const MAX_NAME_LEN: usize = 253;

/// Host condition: whether a draining host has no VM left on it.
pub const CONDITION_DRAINED: &str = "Drained";
/// Reason: no VM is scheduled on the host.
pub const REASON_DRAINED: &str = "Drained";
/// Reason: moves are in flight or waiting for a slot.
pub const REASON_DRAINING: &str = "Draining";
/// Reason: banlieue rejected or failed a move; the VMs are named.
pub const REASON_MOVES_BLOCKED: &str = "MovesBlocked";
/// Reason: the cluster serves no `VirtualMachineMove` (no banlieue, or one older than v0.5.7).
pub const REASON_MOVES_UNSUPPORTED: &str = "MovesUnsupported";
/// Reason: the host is not `Ready`; a move needs both hosts alive (decision 6).
pub const REASON_HOST_NOT_READY: &str = "HostNotReady";
/// Reason: the host has no single Provider, so its VMs cannot be known.
pub const REASON_PROVIDER_UNKNOWN: &str = "ProviderUnknown";

/// What one drain pass reads.
#[derive(Clone, Copy, Debug)]
pub struct DrainInput<'a> {
    /// The Host.
    pub host: &'a Host,
    /// Whether the Host is `Ready` (its provider runs).
    pub host_ready: bool,
    /// The name of the host's one Provider, if there is exactly one.
    pub provider: Option<&'a str>,
    /// Whether the cluster serves `VirtualMachineMove` (and `VirtualMachine`).
    pub moves_served: bool,
    /// The backend driving the host, when faubourg drives its zone's class (ADR-0018). banlieue
    /// moves VMs only between hosts of some backends (decision 9).
    pub backend: Option<Backend>,
    /// Every banlieue VM, in every namespace.
    pub vms: &'a [VirtualMachine],
    /// The moves labelled with this Host's name, in every namespace.
    pub moves: &'a [VirtualMachineMove],
}

/// The `Drained` condition: status, reason, message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drained {
    /// `True` once no VM is scheduled on the host.
    pub status: bool,
    /// One of the `REASON_*` constants.
    pub reason: &'static str,
    /// For a person; names VMs, never anything secret.
    pub message: String,
}

/// What one drain pass does.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrainPlan {
    /// Moves to create.
    pub create: Vec<VirtualMachineMove>,
    /// Moves to delete, as `(namespace, name)`.
    pub delete: Vec<(String, String)>,
    /// The `Drained` condition, or `None` when the host is not draining.
    pub drained: Option<Drained>,
}

/// Plans one drain pass.
#[must_use]
pub fn plan(i: &DrainInput<'_>) -> DrainPlan {
    if i.host.spec.maintenance != Maintenance::Drain {
        return cancel(i.moves);
    }
    let blocked_by = |reason, message: String| DrainPlan {
        drained: Some(Drained {
            status: false,
            reason,
            message,
        }),
        ..DrainPlan::default()
    };
    if !i.moves_served {
        return blocked_by(
            REASON_MOVES_UNSUPPORTED,
            "the cluster serves no VirtualMachineMove (banlieue v0.5.7 or later); nothing is moved"
                .into(),
        );
    }
    if let Some(b) = i.backend.filter(|b| !b.moves_vms()) {
        return blocked_by(
            REASON_MOVES_UNSUPPORTED,
            format!(
                "banlieue does not move VMs between {} hosts; the host is cordoned, nothing is moved",
                b.name()
            ),
        );
    }
    let Some(provider) = i.provider else {
        return blocked_by(
            REASON_PROVIDER_UNKNOWN,
            "the host has no single Provider, so its VMs cannot be found".into(),
        );
    };

    let mut here: Vec<&VirtualMachine> = i
        .vms
        .iter()
        .filter(|vm| {
            vm.status
                .as_ref()
                .and_then(|s| s.scheduled.as_ref())
                .is_some_and(|s| s.provider_name == provider)
        })
        .collect();
    if here.is_empty() {
        return DrainPlan {
            drained: Some(Drained {
                status: true,
                reason: REASON_DRAINED,
                message: "no VM is scheduled on the host".into(),
            }),
            ..DrainPlan::default()
        };
    }
    here.sort_by_key(|vm| (vm.namespace(), vm.name_any()));

    let Sorted {
        in_flight,
        blocked,
        waiting,
    } = sort(&here, i.moves);

    if !i.host_ready {
        return blocked_by(
            REASON_HOST_NOT_READY,
            format!(
                "{} VMs remain; a move needs the host's provider running",
                here.len()
            ),
        );
    }
    let free = MAX_MOVES_PER_HOST.saturating_sub(in_flight);
    let create: Vec<VirtualMachineMove> = waiting
        .iter()
        .take(free)
        .map(|vm| move_for(&i.host.name_any(), vm))
        .collect();
    let drained = if blocked.is_empty() {
        Drained {
            status: false,
            reason: REASON_DRAINING,
            message: format!("{} VMs remain on the host", here.len()),
        }
    } else {
        Drained {
            status: false,
            reason: REASON_MOVES_BLOCKED,
            message: format!("banlieue will not move {}", named(&blocked)),
        }
    };
    DrainPlan {
        create,
        delete: Vec::new(),
        drained: Some(drained),
    }
}

/// Decision 5: the moves that have not started handing over are cancelled; the rest finish.
fn cancel(moves: &[VirtualMachineMove]) -> DrainPlan {
    let delete = moves
        .iter()
        .filter(|m| phase(m).is_cancellable())
        .filter_map(|m| Some((m.namespace()?, m.metadata.name.clone()?)))
        .collect();
    DrainPlan {
        delete,
        ..DrainPlan::default()
    }
}

/// A draining host's VMs, by what their moves say.
struct Sorted<'a> {
    /// VMs with a move not yet finished, or done while the VM still names this host.
    in_flight: usize,
    /// `ns/name (reason)` of each VM whose move banlieue rejected or that failed.
    blocked: Vec<String>,
    /// VMs with no move yet, in order.
    waiting: Vec<&'a VirtualMachine>,
}

fn sort<'a>(here: &[&'a VirtualMachine], moves: &[VirtualMachineMove]) -> Sorted<'a> {
    let mut s = Sorted {
        in_flight: 0,
        blocked: Vec::new(),
        waiting: Vec::new(),
    };
    for vm in here {
        let mine: Vec<&VirtualMachineMove> = moves
            .iter()
            .filter(|m| m.namespace() == vm.namespace() && m.spec.vm_ref.name == vm.name_any())
            .collect();
        if mine
            .iter()
            .any(|m| !matches!(phase(m), MovePhase::Failed | MovePhase::Rejected))
        {
            s.in_flight += 1;
        } else if let Some(m) = mine.first() {
            s.blocked.push(format!(
                "{}/{} ({})",
                vm.namespace().unwrap_or_default(),
                vm.name_any(),
                why(m)
            ));
        } else {
            s.waiting.push(vm);
        }
    }
    s
}

/// A move's phase; one with no status has not been admitted yet.
fn phase(m: &VirtualMachineMove) -> MovePhase {
    m.status.as_ref().map(|s| s.phase).unwrap_or_default()
}

/// Why a move ended unmoved: banlieue's `Admitted` reason, or else its phase.
fn why(m: &VirtualMachineMove) -> String {
    m.status
        .as_ref()
        .and_then(|s| {
            s.conditions
                .iter()
                .find(|c| c.type_ == ADMITTED && c.status == "False")
        })
        .map_or_else(|| format!("{:?}", phase(m)), |c| c.reason.clone())
}

/// `a, b and N more`, naming at most [`MAX_NAMED_VMS`].
fn named(items: &[String]) -> String {
    let list = items
        .iter()
        .take(MAX_NAMED_VMS)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    match items.len().saturating_sub(MAX_NAMED_VMS) {
        0 => list,
        more => format!("{list} and {more} more"),
    }
}

/// The move faubourg asks for: the VM, no target, in its namespace, labelled with the Host.
#[must_use]
pub fn move_for(host: &str, vm: &VirtualMachine) -> VirtualMachineMove {
    let suffix = format!("-drain-{host}");
    let mut base = vm.name_any();
    base.truncate(MAX_NAME_LEN - suffix.len());
    let mut m = VirtualMachineMove::new(
        &format!("{base}{suffix}"),
        VirtualMachineMoveSpec {
            vm_ref: LocalObjectReference {
                name: vm.name_any(),
            },
            target: None,
        },
    );
    m.metadata.namespace = vm.namespace();
    m.metadata.labels = Some([(LABEL_HOST.to_string(), host.to_string())].into());
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn host(maintenance: &str) -> Host {
        serde_json::from_value(json!({
            "apiVersion": "faubourg.firestoned.io/v1alpha1", "kind": "Host",
            "metadata": { "name": "hv01" },
            "spec": { "zoneRef": "az1", "address": "hv01.home.example",
                      "bootstrap": { "method": "None" }, "maintenance": maintenance }
        }))
        .unwrap()
    }

    fn vm(ns: &str, name: &str, provider: &str) -> VirtualMachine {
        serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1", "kind": "VirtualMachine",
            "metadata": { "name": name, "namespace": ns },
            "spec": { "classRef": { "name": "small" }, "imageRef": { "name": "img" } },
            "status": { "scheduled": { "providerName": provider, "providerClass": "cloud-hypervisor",
                                       "failureDomain": provider } }
        }))
        .unwrap()
    }

    /// A move for `vm`, in `phase`; `None` for a move banlieue has not touched (no status).
    fn mv(
        ns: &str,
        vm: &str,
        phase: Option<&str>,
        admitted: Option<(&str, &str)>,
    ) -> VirtualMachineMove {
        let mut m = json!({
            "apiVersion": "banlieue.io/v1alpha1", "kind": "VirtualMachineMove",
            "metadata": { "name": format!("{vm}-move"), "namespace": ns,
                          "labels": { LABEL_HOST: "hv01" } },
            "spec": { "vmRef": { "name": vm } }
        });
        if let Some(p) = phase {
            m["status"] = json!({ "phase": p });
            if let Some((status, reason)) = admitted {
                m["status"]["conditions"] = json!([{ "type": "Admitted", "status": status,
                    "reason": reason, "message": "", "lastTransitionTime": "2026-11-14T00:00:00Z" }]);
            }
        }
        serde_json::from_value(m).unwrap()
    }

    fn input<'a>(
        h: &'a Host,
        vms: &'a [VirtualMachine],
        moves: &'a [VirtualMachineMove],
    ) -> DrainInput<'a> {
        DrainInput {
            host: h,
            host_ready: true,
            provider: Some("hv01"),
            moves_served: true,
            backend: Some(Backend::CloudHypervisor),
            vms,
            moves,
        }
    }

    fn created(p: &DrainPlan) -> Vec<String> {
        p.create
            .iter()
            .map(|m| format!("{}/{}", m.namespace().unwrap(), m.spec.vm_ref.name))
            .collect()
    }

    fn drained(p: &DrainPlan) -> (bool, &'static str) {
        let d = p.drained.as_ref().expect("a draining host reports Drained");
        (d.status, d.reason)
    }

    #[test]
    fn a_host_not_draining_reports_nothing_and_creates_nothing() {
        let h = host("None");
        let vms = [vm("apps", "web-0", "hv01")];
        let p = plan(&input(&h, &vms, &[]));
        assert_eq!(p, DrainPlan::default());
    }

    /// Decision 5: clearing `maintenance` cancels what has not started handing over, and leaves
    /// the rest to finish (or as the record of how they ended).
    #[test]
    fn clearing_maintenance_cancels_only_moves_before_handover() {
        let h = host("None");
        let moves = [
            mv("apps", "a", None, None),
            mv("apps", "b", Some("Pending"), None),
            mv("apps", "c", Some("Preparing"), None),
            mv("apps", "d", Some("Copying"), None),
            mv("apps", "e", Some("HandingOver"), None),
            mv("apps", "f", Some("Succeeded"), None),
            mv("apps", "g", Some("Failed"), None),
            mv("apps", "h", Some("Rejected"), None),
        ];
        let p = plan(&input(&h, &[], &moves));
        let deleted: Vec<_> = p.delete.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(deleted, ["a-move", "b-move", "c-move", "d-move"]);
        assert_eq!(p.create.len(), 0);
        assert_eq!(p.drained, None);
    }

    #[test]
    fn a_host_with_no_vm_is_drained() {
        let h = host("Drain");
        let elsewhere = [vm("apps", "web-0", "hv02")];
        let p = plan(&input(&h, &elsewhere, &[]));
        assert_eq!(drained(&p), (true, REASON_DRAINED));
        assert_eq!(p.create.len(), 0);
    }

    /// Decision 3: one move in flight per host; VMs are taken in namespace and name order.
    #[test]
    fn a_drain_asks_for_one_move_at_a_time() {
        let h = host("Drain");
        let vms = [
            vm("db", "pg-0", "hv01"),
            vm("apps", "web-1", "hv01"),
            vm("apps", "web-0", "hv01"),
            vm("apps", "other", "hv02"),
        ];
        let p = plan(&input(&h, &vms, &[]));
        assert_eq!(created(&p), ["apps/web-0"]);
        assert_eq!(drained(&p), (false, REASON_DRAINING));
        assert!(p.drained.unwrap().message.contains("3 VMs"));
    }

    #[test]
    fn a_move_in_flight_holds_the_next_one() {
        let h = host("Drain");
        let vms = [vm("apps", "web-0", "hv01"), vm("apps", "web-1", "hv01")];
        for phase in [None, Some("Pending"), Some("Copying"), Some("HandingOver")] {
            let moves = [mv("apps", "web-0", phase, None)];
            let p = plan(&input(&h, &vms, &moves));
            assert_eq!(p.create.len(), 0, "{phase:?}");
            assert_eq!(drained(&p), (false, REASON_DRAINING), "{phase:?}");
        }
    }

    /// A move banlieue reports done while the VM still shows this host holds the slot: the
    /// VM's status has not caught up, and asking again would be a second move of the same VM.
    #[test]
    fn a_succeeded_move_whose_vm_is_still_here_holds_the_slot() {
        let h = host("Drain");
        let vms = [vm("apps", "web-0", "hv01"), vm("apps", "web-1", "hv01")];
        let moves = [mv("apps", "web-0", Some("Succeeded"), None)];
        let p = plan(&input(&h, &vms, &moves));
        assert_eq!(p.create.len(), 0);
    }

    /// Decision 4: a rejected or failed move is reported, with banlieue's reason, and never
    /// retried; it does not hold the slot, so the other VMs still move.
    #[test]
    fn a_rejected_or_failed_move_blocks_its_vm_and_never_retries() {
        let h = host("Drain");
        let vms = [
            vm("apps", "sealed-0", "hv01"),
            vm("apps", "stuck-0", "hv01"),
            vm("apps", "web-0", "hv01"),
        ];
        let moves = [
            mv(
                "apps",
                "sealed-0",
                Some("Rejected"),
                Some(("False", "VTPMNotMovable")),
            ),
            mv("apps", "stuck-0", Some("Failed"), None),
        ];
        let p = plan(&input(&h, &vms, &moves));
        assert_eq!(
            created(&p),
            ["apps/web-0"],
            "the blocked ones are not asked again"
        );
        let d = p.drained.unwrap();
        assert_eq!((d.status, d.reason), (false, REASON_MOVES_BLOCKED));
        assert!(
            d.message.contains("apps/sealed-0 (VTPMNotMovable)"),
            "{}",
            d.message
        );
        assert!(d.message.contains("apps/stuck-0 (Failed)"), "{}", d.message);
        assert_eq!(p.delete.len(), 0, "never deletes a VM or its record");
    }

    #[test]
    fn a_blocked_message_names_a_bounded_number_of_vms() {
        let h = host("Drain");
        let vms: Vec<_> = (0..15)
            .map(|i| vm("apps", &format!("v{i:02}"), "hv01"))
            .collect();
        let moves: Vec<_> = (0..15)
            .map(|i| {
                mv(
                    "apps",
                    &format!("v{i:02}"),
                    Some("Rejected"),
                    Some(("False", "NoDestination")),
                )
            })
            .collect();
        let d = plan(&input(&h, &vms, &moves)).drained.unwrap();
        assert_eq!(d.message.matches("NoDestination").count(), MAX_NAMED_VMS);
        assert!(d.message.contains("and 5 more"), "{}", d.message);
    }

    /// Fail closed: with no `VirtualMachineMove` served, nothing is created and the condition
    /// says so, rather than the drain silently doing nothing.
    #[test]
    fn no_move_api_creates_nothing_and_says_so() {
        let h = host("Drain");
        let vms = [vm("apps", "web-0", "hv01")];
        let mut i = input(&h, &vms, &[]);
        i.moves_served = false;
        let p = plan(&i);
        assert_eq!(p.create.len(), 0);
        assert_eq!(drained(&p), (false, REASON_MOVES_UNSUPPORTED));
    }

    /// ADR-0018 decision 9: banlieue moves only Cloud Hypervisor VMs, so a libvirt host's drain
    /// cordons it and says nothing will move.
    #[test]
    fn a_backend_banlieue_cannot_move_creates_nothing_and_says_so() {
        let h = host("Drain");
        let vms = [vm("apps", "web-0", "hv01")];
        let mut i = input(&h, &vms, &[]);
        i.backend = Some(Backend::Libvirt);
        let p = plan(&i);
        assert_eq!(p.create.len(), 0);
        let d = p.drained.unwrap();
        assert_eq!((d.status, d.reason), (false, REASON_MOVES_UNSUPPORTED));
        assert!(d.message.contains("libvirt"), "{}", d.message);
    }

    /// Decision 6: a move needs both hosts alive; a host that is not Ready is not drained.
    #[test]
    fn a_host_that_is_not_ready_is_not_drained() {
        let h = host("Drain");
        let vms = [vm("apps", "web-0", "hv01")];
        let mut i = input(&h, &vms, &[]);
        i.host_ready = false;
        let p = plan(&i);
        assert_eq!(p.create.len(), 0);
        assert_eq!(drained(&p), (false, REASON_HOST_NOT_READY));
    }

    #[test]
    fn a_host_without_one_provider_cannot_know_its_vms() {
        let h = host("Drain");
        let vms = [vm("apps", "web-0", "hv01")];
        let mut i = input(&h, &vms, &[]);
        i.provider = None;
        let p = plan(&i);
        assert_eq!(p.create.len(), 0);
        assert_eq!(drained(&p), (false, REASON_PROVIDER_UNKNOWN));
    }

    /// Decision 3: no target (banlieue picks, by most free memory after the move), the VM's
    /// namespace, and the Host's label so a drain's moves are found and cancelled together.
    #[test]
    fn the_move_names_the_vm_and_no_target_and_carries_the_host() {
        let m = move_for("hv01", &vm("apps", "web-0", "hv01"));
        assert_eq!(m.namespace().as_deref(), Some("apps"));
        assert_eq!(m.spec.vm_ref.name, "web-0");
        assert!(m.spec.target.is_none());
        assert_eq!(m.labels().get(LABEL_HOST).map(String::as_str), Some("hv01"));
        let name = m.name_any();
        assert!(name.contains("web-0") && name.contains("hv01"), "{name}");
        let long = move_for("hv01", &vm("apps", &"v".repeat(253), "hv01"));
        assert!(long.name_any().len() <= MAX_NAME_LEN);
        let v: Value = serde_json::to_value(&m).unwrap();
        assert!(v["spec"].get("target").is_none(), "{v}");
    }
}
