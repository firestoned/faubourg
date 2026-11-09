// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! `AvailabilityZone`: a failure domain and shared-fate boundary for storage and network
//! (ADR-0003). v1 supports local storage only (ADR-0004).

use std::collections::BTreeMap;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::LABEL_PREFIX;
use crate::validation::{Errors, Invalid};

/// Condition reason when a zone asks for a storage mode v1 does not build (ADR-0004).
pub const REASON_STORAGE_MODE_UNSUPPORTED: &str = "StorageModeUnsupported";
/// Condition reason when `providerLabels` uses a key faubourg owns.
pub const REASON_RESERVED_LABEL: &str = "ReservedLabel";
/// Condition reason when a `providerLabels` key or value is not a valid label.
pub const REASON_INVALID_LABEL: &str = "InvalidLabel";

/// Desired state of an availability zone.
#[derive(CustomResource, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "faubourg.firestoned.io",
    version = "v1alpha1",
    kind = "AvailabilityZone",
    plural = "availabilityzones",
    shortname = "az",
    status = "AvailabilityZoneStatus",
    printcolumn = r#"{"name":"Storage","type":"string","jsonPath":".spec.storage.mode"}"#,
    printcolumn = r#"{"name":"Hosts","type":"integer","jsonPath":".status.hosts"}"#,
    printcolumn = r#"{"name":"Ready","type":"integer","jsonPath":".status.readyHosts"}"#,
    printcolumn = r#"{"name":"Cordoned","type":"integer","jsonPath":".status.cordonedHosts"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct AvailabilityZoneSpec {
    /// Free-form description for operators.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The zone's storage model. Decides what the platform can promise (ADR-0004).
    #[serde(default)]
    pub storage: ZoneStorage,
    /// Labels copied onto every member host's banlieue `Provider`, and so onto its failure
    /// domain. Keys under `faubourg.firestoned.io/` are reserved.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_labels: BTreeMap<String, String>,
}

/// A zone's storage model.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ZoneStorage {
    /// `Local` (v1) or `Shared` (in the schema, refused until built).
    #[serde(default)]
    pub mode: StorageMode,
}

/// Where a zone's VM disks live.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum StorageMode {
    /// Disks on the host. A VM is pinned to its host; host loss is VM loss.
    #[default]
    Local,
    /// Disks shared across the zone. Needs fencing; not supported in v1.
    Shared,
}

/// Observed state of an availability zone (ADR-0007, ADR-0015), written by the zone controller.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AvailabilityZoneStatus {
    /// Hosts whose `zoneRef` names this zone.
    #[serde(default)]
    pub hosts: u32,
    /// Of those, hosts in phase `Ready`.
    #[serde(default)]
    pub ready_hosts: u32,
    /// Of the ready ones, those cordoned: healthy, but no place for new VMs (ADR-0015).
    #[serde(default)]
    pub cordoned_hosts: u32,
    /// `Ready` and the reasons behind it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// The generation these conditions describe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}

impl AvailabilityZoneSpec {
    /// Validates the spec, failing closed. Every refused field is reported.
    pub fn validate(&self) -> Result<(), Invalid> {
        let mut errs = Errors::default();
        if self.storage.mode == StorageMode::Shared {
            errs.push(
                "spec.storage.mode",
                REASON_STORAGE_MODE_UNSUPPORTED,
                "Shared storage is not supported in v1; use Local (ADR-0004)",
            );
        }
        for (k, v) in &self.provider_labels {
            let field = format!("spec.providerLabels[{k}]");
            if k.starts_with(LABEL_PREFIX) {
                errs.push(
                    &field,
                    REASON_RESERVED_LABEL,
                    format!("keys under {LABEL_PREFIX} are owned by faubourg"),
                );
            } else if !is_label_key(k) || !is_label_value(v) {
                errs.push(&field, REASON_INVALID_LABEL, "not a valid Kubernetes label");
            }
        }
        errs.finish()
    }
}

/// A Kubernetes label key: optional DNS-subdomain prefix and `/`, then a name segment.
fn is_label_key(k: &str) -> bool {
    let (prefix, name) = match k.split_once('/') {
        Some((p, n)) => (Some(p), n),
        None => (None, k),
    };
    prefix.is_none_or(crate::validation::is_dns_subdomain) && is_label_name(name)
}

/// A label value: empty, or a name segment.
fn is_label_value(v: &str) -> bool {
    v.is_empty() || is_label_name(v)
}

/// Up to 63 of `[A-Za-z0-9_.-]`, alphanumeric at both ends.
fn is_label_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 63
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        && b[0].is_ascii_alphanumeric()
        && b[b.len() - 1].is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(mode: StorageMode) -> AvailabilityZoneSpec {
        AvailabilityZoneSpec {
            storage: ZoneStorage { mode },
            ..Default::default()
        }
    }

    #[test]
    fn local_zone_is_valid() {
        assert_eq!(spec(StorageMode::Local).validate(), Ok(()));
    }

    #[test]
    fn storage_defaults_to_local() {
        let s: AvailabilityZoneSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(s.storage.mode, StorageMode::Local);
    }

    #[test]
    fn shared_storage_is_refused_in_v1() {
        let err = spec(StorageMode::Shared).validate().unwrap_err();
        assert_eq!(err.reasons(), vec![REASON_STORAGE_MODE_UNSUPPORTED]);
    }

    #[test]
    fn provider_labels_may_not_use_the_reserved_prefix() {
        let mut s = spec(StorageMode::Local);
        s.provider_labels
            .insert(crate::LABEL_ZONE.to_string(), "elsewhere".to_string());
        assert_eq!(
            s.validate().unwrap_err().reasons(),
            vec![REASON_RESERVED_LABEL]
        );
    }

    #[test]
    fn provider_labels_must_be_valid_labels() {
        for (k, v) in [("rack", "r 1"), ("-rack", "r1"), ("Bad_Prefix!/rack", "r1")] {
            let mut s = spec(StorageMode::Local);
            s.provider_labels.insert(k.to_string(), v.to_string());
            assert_eq!(
                s.validate().unwrap_err().reasons(),
                vec![REASON_INVALID_LABEL],
                "{k}={v}"
            );
        }
        let mut s = spec(StorageMode::Local);
        s.provider_labels
            .insert("topology.example.com/rack".to_string(), "r1".to_string());
        s.provider_labels.insert("tier".to_string(), String::new());
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn every_refusal_is_reported() {
        let mut s = spec(StorageMode::Shared);
        s.provider_labels
            .insert(crate::LABEL_HOST.to_string(), "x".to_string());
        assert_eq!(s.validate().unwrap_err().0.len(), 2);
    }
}
