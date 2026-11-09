// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! A zone's health, from its hosts (ADR-0015). Pure: `controller.rs` reads the zone and the
//! Hosts and writes what this returns.
//!
//! The status is a report, not a gate: nothing faubourg decides reads it (decision 6).

use faubourg_api::host::{Host, HostPhase};
use faubourg_api::zone::{
    AvailabilityZone, AvailabilityZoneStatus, REASON_STORAGE_MODE_UNSUPPORTED,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};

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

/// The zone's status from its Hosts, at `now`. The transition time moves only when `Ready`
/// flips.
pub fn status(zone: &AvailabilityZone, hosts: &[Host], now: &Time) -> AvailabilityZoneStatus {
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

    let generation = zone.metadata.generation;
    let status = if is_ready { "True" } else { "False" };
    let last_transition_time = zone
        .status
        .iter()
        .flat_map(|s| s.conditions.iter())
        .find(|c| c.type_ == CONDITION_READY && c.status == status)
        .map_or_else(|| now.clone(), |c| c.last_transition_time.clone());
    AvailabilityZoneStatus {
        hosts: count(members.len()),
        ready_hosts: count(ready.len()),
        cordoned_hosts: count(cordoned),
        conditions: vec![Condition {
            type_: CONDITION_READY.into(),
            status: status.into(),
            reason: reason.into(),
            message,
            observed_generation: generation,
            last_transition_time,
        }],
        observed_generation: generation,
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
        let s = status(&z, &[], &at(1));
        assert_eq!(ready(&s), ("False", REASON_INVALID_SPEC));
    }

    #[test]
    fn a_zone_with_no_hosts_is_not_ready() {
        let s = status(
            &zone(StorageMode::Local),
            &[host("hv01", "az2", Some(HostPhase::Ready), false)],
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
        let s = status(&zone(StorageMode::Local), &hosts, &at(1));
        assert_eq!(ready(&s), ("False", REASON_NO_READY_HOST));
        assert_eq!((s.hosts, s.ready_hosts, s.cordoned_hosts), (3, 0, 0));
    }

    #[test]
    fn a_zone_whose_ready_hosts_are_all_cordoned_is_not_ready() {
        let hosts = [
            host("hv01", "az1", Some(HostPhase::Ready), true),
            host("hv02", "az1", Some(HostPhase::Pending), false),
        ];
        let s = status(&zone(StorageMode::Local), &hosts, &at(1));
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
        let s = status(&zone(StorageMode::Local), &hosts, &at(1));
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
        let s = status(&zone(StorageMode::Local), &hosts, &at(1));
        assert_eq!(s.cordoned_hosts, 0, "cordonedHosts is of the ready ones");
        assert_eq!(ready(&s), ("True", REASON_HOSTS_READY));
    }

    #[test]
    fn the_transition_time_moves_only_when_ready_flips() {
        let up = [host("hv01", "az1", Some(HostPhase::Ready), false)];
        let mut z = zone(StorageMode::Local);
        z.status = Some(status(&z, &up, &at(100)));
        let again = status(&z, &up, &at(200));
        assert_eq!(again.conditions[0].last_transition_time, at(100));
        assert_eq!(
            again,
            z.status.clone().unwrap(),
            "an unchanged zone writes nothing new"
        );
        let down = [host("hv01", "az1", Some(HostPhase::Degraded), false)];
        let flipped = status(&z, &down, &at(300));
        assert_eq!(flipped.conditions[0].last_transition_time, at(300));
    }
}
