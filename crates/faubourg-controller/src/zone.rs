// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! A zone's health, from its hosts (ADR-0015). Pure: `controller.rs` reads the zone and the
//! Hosts and writes what this returns.
//!
//! The status is a report, not a gate: nothing faubourg decides reads it (decision 6).
//!
//! Besides `Ready`, a zone reports `HostsMatch` (ADR-0017 decision 9): whether every member's
//! Provider runs the same VMM on the same CPU model, which banlieue requires before it moves a
//! VM between two hosts (banlieue ADR-0127). Advisory: it never changes `Ready`.

use std::collections::BTreeMap;

use banlieue_api::banlieue::Provider;
use faubourg_api::LABEL_HOST;
use faubourg_api::host::{Host, HostPhase};
use faubourg_api::zone::{
    AvailabilityZone, AvailabilityZoneStatus, REASON_STORAGE_MODE_UNSUPPORTED,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use kube::ResourceExt;

/// The field manager for zone status, the zone controller's only write.
pub const FIELD_MANAGER: &str = "faubourg.firestoned.io/zone-controller";
/// The zone condition the controller writes.
pub const CONDITION_READY: &str = "Ready";
/// Reason: the zone's spec is refused for something other than its storage mode.
pub const REASON_INVALID_SPEC: &str = "InvalidSpec";
/// Reason: no Host names the zone.
pub const REASON_NO_HOSTS: &str = "NoHosts";
/// Reason: no member Host is `Ready`.
pub const REASON_NO_READY_HOST: &str = "NoReadyHost";
/// Reason: every `Ready` member is cordoned.
pub const REASON_ALL_READY_HOSTS_CORDONED: &str = "AllReadyHostsCordoned";
/// Reason: at least one member is `Ready` and not cordoned.
pub const REASON_HOSTS_READY: &str = "HostsReady";
/// The advisory zone condition: the members can take each other's VMs (ADR-0017 decision 9).
pub const CONDITION_HOSTS_MATCH: &str = "HostsMatch";
/// Reason: every member that reports runs the same VMM on the same CPU model.
pub const REASON_HOSTS_MATCH: &str = "HostsMatch";
/// Reason: members report different VMMs or CPU models; the odd ones are named.
pub const REASON_HOSTS_DIFFER: &str = "HostsDiffer";
/// Reason: a member's Provider does not report both, or none reports at all.
pub const REASON_HOSTS_UNREPORTED: &str = "HostsUnreported";
/// The failure domain's raw attribute carrying the VMM version, as banlieue's Cloud Hypervisor
/// provider publishes it.
pub const RAW_VMM: &str = "vmm";
/// The failure domain's raw attribute carrying the host CPU model.
pub const RAW_CPU_MODEL: &str = "cpuModel";
/// At most this many hosts are named in a condition message; the rest are counted.
const MAX_NAMED_HOSTS: usize = 10;

/// The zone's status from its Hosts, at `now`. The transition time moves only when `Ready`
/// flips.
pub fn status(
    zone: &AvailabilityZone,
    hosts: &[Host],
    providers: &[Provider],
    now: &Time,
) -> AvailabilityZoneStatus {
    let name = zone.metadata.name.as_deref().unwrap_or_default();
    let members: Vec<&Host> = hosts.iter().filter(|h| h.spec.zone_ref == name).collect();
    let ready: Vec<&&Host> = members
        .iter()
        .filter(|h| {
            h.status
                .as_ref()
                .is_some_and(|s| s.phase == Some(HostPhase::Ready))
        })
        .collect();
    let cordoned = ready.iter().filter(|h| h.spec.cordoned).count();
    let (is_ready, reason, message) = decide(zone, members.len(), ready.len(), cordoned);
    let ready_condition = condition(
        zone,
        CONDITION_READY,
        if is_ready { "True" } else { "False" },
        reason,
        message,
        now,
    );
    let (status, reason, message) = hosts_match(&members, providers);
    let match_condition = condition(zone, CONDITION_HOSTS_MATCH, status, reason, message, now);
    AvailabilityZoneStatus {
        hosts: count(members.len()),
        ready_hosts: count(ready.len()),
        cordoned_hosts: count(cordoned),
        conditions: vec![ready_condition, match_condition],
        observed_generation: zone.metadata.generation,
    }
}

/// A condition of `type_`, whose transition time moves only when its status flips.
fn condition(
    zone: &AvailabilityZone,
    type_: &str,
    status: &str,
    reason: &str,
    message: String,
    now: &Time,
) -> Condition {
    let last_transition_time = zone
        .status
        .iter()
        .flat_map(|s| s.conditions.iter())
        .find(|c| c.type_ == type_ && c.status == status)
        .map_or_else(|| now.clone(), |c| c.last_transition_time.clone());
    Condition {
        type_: type_.into(),
        status: status.into(),
        reason: reason.into(),
        message,
        observed_generation: zone.metadata.generation,
        last_transition_time,
    }
}

/// What a member's Provider says about the host a moved VM would run on.
enum Report {
    /// No Provider is labelled with the host yet: it is not counted.
    NoProvider,
    /// A Provider, but not both values (or more than one Provider claims the host).
    Unreported,
    /// `(vmm, cpuModel)`.
    Pair(String, String),
}

/// The report of the one Provider labelled with `host` (ADR-0009 decision 5), from its first
/// failure domain: a Cloud Hypervisor host is one Provider and one failure domain.
fn report(host: &str, providers: &[Provider]) -> Report {
    let mut own = providers
        .iter()
        .filter(|p| p.labels().get(LABEL_HOST).map(String::as_str) == Some(host));
    let (Some(provider), None) = (own.next(), own.next()) else {
        return if providers
            .iter()
            .any(|p| p.labels().get(LABEL_HOST).map(String::as_str) == Some(host))
        {
            Report::Unreported
        } else {
            Report::NoProvider
        };
    };
    let raw = provider
        .status
        .as_ref()
        .and_then(|s| s.failure_domains.first())
        .map(|fd| &fd.attributes.raw);
    let value = |key: &str| {
        raw.and_then(|r| r.get(key))
            .filter(|v| !v.trim().is_empty())
            .cloned()
    };
    match (value(RAW_VMM), value(RAW_CPU_MODEL)) {
        (Some(vmm), Some(cpu)) => Report::Pair(vmm, cpu),
        _ => Report::Unreported,
    }
}

/// `HostsMatch`, its reason and message (ADR-0017 decision 9). A difference among the members
/// that report is said first: it is the fact a drain would fail on.
fn hosts_match(members: &[&Host], providers: &[Provider]) -> (&'static str, &'static str, String) {
    let mut pairs: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut unreported: Vec<String> = Vec::new();
    for h in members {
        let name = h.name_any();
        match report(&name, providers) {
            Report::NoProvider => {}
            Report::Unreported => unreported.push(name),
            Report::Pair(vmm, cpu) => pairs.entry((vmm, cpu)).or_default().push(name),
        }
    }
    let reporting: usize = pairs.values().map(Vec::len).sum();
    // The most common pair; on a tie, the first in order, so the answer is stable.
    let common = pairs
        .iter()
        .max_by(|a, b| a.1.len().cmp(&b.1.len()).then_with(|| b.0.cmp(a.0)))
        .map(|(k, _)| k.clone());
    let Some((vmm, cpu)) = common else {
        return (
            "Unknown",
            REASON_HOSTS_UNREPORTED,
            if unreported.is_empty() {
                "no member's Provider reports its VMM and CPU model".into()
            } else {
                format!("no VMM and CPU model reported for {}", named(unreported))
            },
        );
    };
    if pairs.len() > 1 {
        let odd: Vec<String> = pairs
            .iter()
            .filter(|(k, _)| k.0 != vmm || k.1 != cpu)
            .flat_map(|(_, hosts)| hosts.iter().cloned())
            .collect();
        return (
            "False",
            REASON_HOSTS_DIFFER,
            format!(
                "{} of {reporting} hosts differ from {vmm} on {cpu}: {}",
                odd.len(),
                named(odd)
            ),
        );
    }
    if !unreported.is_empty() {
        return (
            "Unknown",
            REASON_HOSTS_UNREPORTED,
            format!("no VMM and CPU model reported for {}", named(unreported)),
        );
    }
    (
        "True",
        REASON_HOSTS_MATCH,
        format!("{reporting} hosts run {vmm} on {cpu}"),
    )
}

/// Host names, sorted, the first [`MAX_NAMED_HOSTS`] named and the rest counted.
fn named(mut hosts: Vec<String>) -> String {
    hosts.sort();
    let more = hosts.len().saturating_sub(MAX_NAMED_HOSTS);
    hosts.truncate(MAX_NAMED_HOSTS);
    let list = hosts.join(", ");
    if more > 0 {
        format!("{list} and {more} more")
    } else {
        list
    }
}

/// `Ready`, its reason and message: the first row of ADR-0015's table that applies.
fn decide(
    zone: &AvailabilityZone,
    members: usize,
    ready: usize,
    cordoned: usize,
) -> (bool, &'static str, String) {
    if let Err(invalid) = zone.spec.validate() {
        let reason = if invalid.reasons().contains(&REASON_STORAGE_MODE_UNSUPPORTED) {
            REASON_STORAGE_MODE_UNSUPPORTED
        } else {
            REASON_INVALID_SPEC
        };
        return (false, reason, invalid.to_string());
    }
    if members == 0 {
        return (false, REASON_NO_HOSTS, "no Host names this zone".into());
    }
    if ready == 0 {
        return (
            false,
            REASON_NO_READY_HOST,
            format!("none of its {members} hosts is Ready"),
        );
    }
    if cordoned == ready {
        return (
            false,
            REASON_ALL_READY_HOSTS_CORDONED,
            format!("all {ready} Ready hosts are cordoned"),
        );
    }
    (
        true,
        REASON_HOSTS_READY,
        format!(
            "{} of {members} hosts are Ready and not cordoned",
            ready - cordoned
        ),
    )
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use faubourg_api::host::{BootstrapMethod, BootstrapSpec, HostSpec, HostStatus};
    use faubourg_api::zone::{AvailabilityZoneSpec, StorageMode, ZoneStorage};
    use k8s_openapi::jiff::Timestamp;

    fn at(second: i64) -> Time {
        Time(Timestamp::from_second(second).unwrap())
    }

    fn zone(mode: StorageMode) -> AvailabilityZone {
        let mut z = AvailabilityZone::new(
            "az1",
            AvailabilityZoneSpec {
                storage: ZoneStorage { mode },
                ..AvailabilityZoneSpec::default()
            },
        );
        z.metadata.generation = Some(3);
        z
    }

    fn host(name: &str, zone: &str, phase: Option<HostPhase>, cordoned: bool) -> Host {
        let mut h = Host::new(
            name,
            HostSpec {
                zone_ref: zone.into(),
                address: format!("{name}.home.example"),
                ssh: None,
                bootstrap: BootstrapSpec {
                    method: BootstrapMethod::None,
                    script_ref: None,
                },
                cordoned,
            },
        );
        h.status = Some(HostStatus {
            phase,
            ..HostStatus::default()
        });
        h
    }

    fn ready(s: &AvailabilityZoneStatus) -> (&str, &str) {
        let c = &s.conditions[0];
        assert_eq!(c.type_, CONDITION_READY);
        (c.status.as_str(), c.reason.as_str())
    }

    #[test]
    fn a_shared_zone_is_refused_whatever_its_hosts() {
        let s = status(
            &zone(StorageMode::Shared),
            &[host("hv01", "az1", Some(HostPhase::Ready), false)],
            &[],
            &at(1),
        );
        assert_eq!(ready(&s), ("False", REASON_STORAGE_MODE_UNSUPPORTED));
        assert_eq!(s.ready_hosts, 1, "the counts are still reported");
    }

    #[test]
    fn any_other_refused_spec_is_invalid() {
        let mut z = zone(StorageMode::Local);
        z.spec
            .provider_labels
            .insert("faubourg.firestoned.io/zone".into(), "az2".into());
        let s = status(&z, &[], &[], &at(1));
        assert_eq!(ready(&s), ("False", REASON_INVALID_SPEC));
    }

    #[test]
    fn a_zone_with_no_hosts_is_not_ready() {
        let s = status(
            &zone(StorageMode::Local),
            &[host("hv01", "az2", Some(HostPhase::Ready), false)],
            &[],
            &at(1),
        );
        assert_eq!(ready(&s), ("False", REASON_NO_HOSTS));
        assert_eq!(
            (s.hosts, s.ready_hosts),
            (0, 0),
            "another zone's host is not a member"
        );
    }

    #[test]
    fn hosts_that_are_not_ready_make_no_ready_zone() {
        let hosts = [
            host("hv01", "az1", Some(HostPhase::Bootstrapping), false),
            host("hv02", "az1", Some(HostPhase::Degraded), false),
            host("hv03", "az1", None, false),
        ];
        let s = status(&zone(StorageMode::Local), &hosts, &[], &at(1));
        assert_eq!(ready(&s), ("False", REASON_NO_READY_HOST));
        assert_eq!((s.hosts, s.ready_hosts, s.cordoned_hosts), (3, 0, 0));
    }

    #[test]
    fn a_zone_whose_ready_hosts_are_all_cordoned_is_not_ready() {
        let hosts = [
            host("hv01", "az1", Some(HostPhase::Ready), true),
            host("hv02", "az1", Some(HostPhase::Pending), false),
        ];
        let s = status(&zone(StorageMode::Local), &hosts, &[], &at(1));
        assert_eq!(ready(&s), ("False", REASON_ALL_READY_HOSTS_CORDONED));
        assert_eq!((s.hosts, s.ready_hosts, s.cordoned_hosts), (2, 1, 1));
    }

    #[test]
    fn one_ready_uncordoned_host_makes_a_ready_zone() {
        let hosts = [
            host("hv01", "az1", Some(HostPhase::Ready), true),
            host("hv02", "az1", Some(HostPhase::Ready), false),
            host("hv03", "az1", Some(HostPhase::Failed), false),
        ];
        let s = status(&zone(StorageMode::Local), &hosts, &[], &at(1));
        assert_eq!(ready(&s), ("True", REASON_HOSTS_READY));
        assert_eq!((s.hosts, s.ready_hosts, s.cordoned_hosts), (3, 2, 1));
        assert_eq!(s.observed_generation, Some(3));
        assert_eq!(s.conditions[0].observed_generation, Some(3));
    }

    #[test]
    fn a_cordoned_host_that_is_not_ready_is_not_counted_as_cordoned() {
        let hosts = [
            host("hv01", "az1", Some(HostPhase::Pending), true),
            host("hv02", "az1", Some(HostPhase::Ready), false),
        ];
        let s = status(&zone(StorageMode::Local), &hosts, &[], &at(1));
        assert_eq!(s.cordoned_hosts, 0, "cordonedHosts is of the ready ones");
        assert_eq!(ready(&s), ("True", REASON_HOSTS_READY));
    }

    #[test]
    fn the_transition_time_moves_only_when_ready_flips() {
        let up = [host("hv01", "az1", Some(HostPhase::Ready), false)];
        let mut z = zone(StorageMode::Local);
        z.status = Some(status(&z, &up, &[], &at(100)));
        let again = status(&z, &up, &[], &at(200));
        assert_eq!(again.conditions[0].last_transition_time, at(100));
        assert_eq!(
            again,
            z.status.clone().unwrap(),
            "an unchanged zone writes nothing new"
        );
        let down = [host("hv01", "az1", Some(HostPhase::Degraded), false)];
        let flipped = status(&z, &down, &[], &at(300));
        assert_eq!(flipped.conditions[0].last_transition_time, at(300));
    }

    // --- HostsMatch (ADR-0017 decision 9) ------------------------------------

    const VMM: &str = "cloud-hypervisor v53.0";
    const CPU: &str = "Intel(R) Xeon(R) CPU E5-2630 v3 @ 2.40GHz";

    /// The Provider faubourg labels with `host`, reporting `raw` on its one failure domain.
    fn provider(host: &str, raw: &[(&str, &str)]) -> Provider {
        let raw: BTreeMap<&str, &str> = raw.iter().copied().collect();
        let labels = BTreeMap::from([(LABEL_HOST, host)]);
        serde_json::from_value(serde_json::json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "Provider",
            "metadata": { "name": host, "namespace": "banlieue-system",
                          "labels": labels },
            "spec": { "providerClassRef": { "name": "cloud-hypervisor" },
                      "connection": { "endpoint": "unix:///run/banlieue/provider.sock" } },
            "status": { "failureDomains": [ { "name": host, "attributes": { "raw": raw } } ] },
        }))
        .expect("a Provider")
    }

    fn reporting(host: &str, vmm: &str, cpu: &str) -> Provider {
        provider(host, &[(RAW_VMM, vmm), (RAW_CPU_MODEL, cpu)])
    }

    fn up(name: &str) -> Host {
        host(name, "az1", Some(HostPhase::Ready), false)
    }

    fn matching(hosts: &[Host], providers: &[Provider]) -> (String, String, String) {
        let s = status(&zone(StorageMode::Local), hosts, providers, &at(1));
        let c = s
            .conditions
            .iter()
            .find(|c| c.type_ == CONDITION_HOSTS_MATCH)
            .expect("HostsMatch is always written");
        (c.status.clone(), c.reason.clone(), c.message.clone())
    }

    #[test]
    fn hosts_reporting_the_same_vmm_and_cpu_match() {
        let (st, reason, msg) = matching(
            &[up("hv01"), up("hv02")],
            &[reporting("hv01", VMM, CPU), reporting("hv02", VMM, CPU)],
        );
        assert_eq!((st.as_str(), reason.as_str()), ("True", REASON_HOSTS_MATCH));
        assert!(msg.starts_with("2 hosts run"), "{msg}");
    }

    #[test]
    fn a_host_on_another_cpu_differs_and_is_named() {
        let (st, reason, msg) = matching(
            &[up("hv01"), up("hv02"), up("hv03")],
            &[
                reporting("hv01", VMM, CPU),
                reporting("hv02", VMM, "AMD EPYC 7302"),
                reporting("hv03", VMM, CPU),
            ],
        );
        assert_eq!(
            (st.as_str(), reason.as_str()),
            ("False", REASON_HOSTS_DIFFER)
        );
        assert!(msg.contains("1 of 3") && msg.ends_with(": hv02"), "{msg}");
    }

    #[test]
    fn another_vmm_version_differs_too() {
        let (st, reason, msg) = matching(
            &[up("hv01"), up("hv02")],
            &[
                reporting("hv01", VMM, CPU),
                reporting("hv02", "cloud-hypervisor v54.0", CPU),
            ],
        );
        assert_eq!(
            (st.as_str(), reason.as_str()),
            ("False", REASON_HOSTS_DIFFER)
        );
        // A tie: the first pair in order is the reference, every time.
        assert!(msg.ends_with(": hv02"), "{msg}");
    }

    #[test]
    fn a_provider_missing_a_value_is_unreported() {
        let (st, reason, msg) = matching(
            &[up("hv01"), up("hv02")],
            &[
                reporting("hv01", VMM, CPU),
                provider("hv02", &[(RAW_VMM, VMM), (RAW_CPU_MODEL, " ")]),
            ],
        );
        assert_eq!(
            (st.as_str(), reason.as_str()),
            ("Unknown", REASON_HOSTS_UNREPORTED)
        );
        assert!(msg.ends_with("hv02"), "{msg}");
    }

    #[test]
    fn a_difference_is_reported_before_a_missing_value() {
        let (_, reason, _) = matching(
            &[up("hv01"), up("hv02"), up("hv03")],
            &[
                reporting("hv01", VMM, CPU),
                reporting("hv02", VMM, "AMD EPYC 7302"),
                provider("hv03", &[]),
            ],
        );
        assert_eq!(reason, REASON_HOSTS_DIFFER);
    }

    #[test]
    fn a_host_without_a_provider_yet_is_not_counted() {
        let (st, reason, msg) = matching(
            &[
                up("hv01"),
                up("hv02"),
                host("hv03", "az1", Some(HostPhase::Pending), false),
            ],
            &[reporting("hv01", VMM, CPU), reporting("hv02", VMM, CPU)],
        );
        assert_eq!((st.as_str(), reason.as_str()), ("True", REASON_HOSTS_MATCH));
        assert!(msg.starts_with("2 hosts"), "{msg}");
    }

    #[test]
    fn no_reporting_member_is_unreported() {
        let (st, reason, _) = matching(&[up("hv01")], &[]);
        assert_eq!(
            (st.as_str(), reason.as_str()),
            ("Unknown", REASON_HOSTS_UNREPORTED)
        );
    }

    #[test]
    fn providers_of_other_zones_hosts_are_ignored() {
        let (st, _, _) = matching(
            &[
                up("hv01"),
                host("hv09", "az2", Some(HostPhase::Ready), false),
            ],
            &[
                reporting("hv01", VMM, CPU),
                reporting("hv09", VMM, "AMD EPYC 7302"),
            ],
        );
        assert_eq!(st, "True", "hv09 is in az2");
    }

    #[test]
    fn two_providers_claiming_one_host_are_unreported() {
        let mut twin = reporting("hv01", VMM, CPU);
        twin.metadata.name = Some("hv01-old".into());
        let (st, reason, _) = matching(&[up("hv01")], &[reporting("hv01", VMM, CPU), twin]);
        assert_eq!(
            (st.as_str(), reason.as_str()),
            ("Unknown", REASON_HOSTS_UNREPORTED)
        );
    }

    #[test]
    fn the_named_hosts_are_bounded() {
        let mut hosts = vec![up("hv00"), up("hv01")];
        let mut providers = vec![reporting("hv00", VMM, CPU), reporting("hv01", VMM, CPU)];
        for i in 0..12 {
            let n = format!("odd{i:02}");
            hosts.push(up(&n));
            providers.push(reporting(&n, VMM, &format!("cpu {i}")));
        }
        let (_, reason, msg) = matching(&hosts, &providers);
        assert_eq!(reason, REASON_HOSTS_DIFFER);
        assert!(msg.ends_with(" and 2 more"), "{msg}");
    }

    #[test]
    fn hosts_match_never_changes_ready() {
        let s = status(
            &zone(StorageMode::Local),
            &[up("hv01"), up("hv02")],
            &[
                reporting("hv01", VMM, CPU),
                reporting("hv02", VMM, "AMD EPYC 7302"),
            ],
            &at(1),
        );
        assert_eq!(ready(&s), ("True", REASON_HOSTS_READY));
    }

    #[test]
    fn each_condition_keeps_its_own_transition_time() {
        let hosts = [up("hv01"), up("hv02")];
        let same = [reporting("hv01", VMM, CPU), reporting("hv02", VMM, CPU)];
        let mut z = zone(StorageMode::Local);
        z.status = Some(status(&z, &hosts, &same, &at(100)));
        let differ = [
            reporting("hv01", VMM, CPU),
            reporting("hv02", VMM, "AMD EPYC 7302"),
        ];
        let s = status(&z, &hosts, &differ, &at(200));
        assert_eq!(s.conditions[0].last_transition_time, at(100), "Ready held");
        assert_eq!(s.conditions[1].type_, CONDITION_HOSTS_MATCH);
        assert_eq!(
            s.conditions[1].last_transition_time,
            at(200),
            "HostsMatch flipped"
        );
    }
}
