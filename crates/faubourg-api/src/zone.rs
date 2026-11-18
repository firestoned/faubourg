// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! `AvailabilityZone`: a failure domain and shared-fate boundary for storage and network
//! (ADR-0003). v1 supports local storage only (ADR-0004).

use std::collections::BTreeMap;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{CustomResource, KubeSchema};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::LABEL_PREFIX;
use crate::validation::{Errors, Invalid, is_dns_subdomain};

/// Condition reason when a zone asks for a storage mode v1 does not build (ADR-0004).
pub const REASON_STORAGE_MODE_UNSUPPORTED: &str = "StorageModeUnsupported";
/// Condition reason when `providerLabels` uses a key faubourg owns.
pub const REASON_RESERVED_LABEL: &str = "ReservedLabel";
/// Condition reason when a `providerLabels` key or value is not a valid label.
pub const REASON_INVALID_LABEL: &str = "InvalidLabel";
/// Condition reason when `providerClassRef.name` is not a valid object name.
pub const REASON_INVALID_PROVIDER_CLASS_REF: &str = "InvalidProviderClassRef";

/// The CEL rule keeping a zone's class once it is set (ADR-0018 decision 3). Setting it on a
/// zone that named none is allowed: a member whose Provider has another class is refused
/// (`ProviderClassMismatch`), not rewritten.
pub const ZONE_CLASS_IMMUTABLE: &str = "!has(oldSelf.spec.providerClassRef) || (has(self.spec.providerClassRef) && self.spec.providerClassRef.name == oldSelf.spec.providerClassRef.name)";

/// Desired state of an availability zone.
///
/// Its class cannot change or be removed once set (ADR-0018 decision 3): banlieue owns a
/// Provider's workload and identity by class, so moving a Provider between classes is not a
/// relabel.
#[derive(CustomResource, Clone, Debug, Default, PartialEq, Serialize, Deserialize, KubeSchema)]
#[kube(
    group = "faubourg.firestoned.io",
    version = "v1alpha1",
    kind = "AvailabilityZone",
    plural = "availabilityzones",
    shortname = "az",
    status = "AvailabilityZoneStatus",
    validation = Rule::new(ZONE_CLASS_IMMUTABLE)
        .message("a zone's providerClassRef cannot change or be removed once set"),
    printcolumn = r#"{"name":"Class","type":"string","jsonPath":".spec.providerClassRef.name"}"#,
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
    /// domain. Keys under `faubourg.firestoned.io/`, `banlieue.io/unschedulable` and `banlieue.io/zone` are
    /// reserved.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_labels: BTreeMap<String, String>,
    /// The banlieue `ProviderClass` every member host's Provider references (ADR-0018). Its
    /// `backend` decides how faubourg drives the zone's hosts. Omitted: the controller's
    /// `--provider-class`. It cannot change or be removed once set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_class_ref: Option<ProviderClassRef>,
}

/// A banlieue `ProviderClass`, by name. Cluster-scoped, so no namespace.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderClassRef {
    /// The `ProviderClass`'s name.
    pub name: String,
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
    /// The name of the zone's `ProviderClass`: its own, or `default` (the controller's
    /// `--provider-class`) when it names none (ADR-0018 decision 1).
    #[must_use]
    pub fn provider_class<'a>(&'a self, default: &'a str) -> &'a str {
        self.provider_class_ref
            .as_ref()
            .map_or(default, |r| r.name.as_str())
    }

    /// Validates the spec, failing closed. Every refused field is reported.
    pub fn validate(&self) -> Result<(), Invalid> {
        let mut errs = Errors::default();
        if let Some(r) = &self.provider_class_ref
            && !is_dns_subdomain(&r.name)
        {
            errs.push(
                "spec.providerClassRef.name",
                REASON_INVALID_PROVIDER_CLASS_REF,
                "not a valid object name",
            );
        }
        if self.storage.mode == StorageMode::Shared {
            errs.push(
                "spec.storage.mode",
                REASON_STORAGE_MODE_UNSUPPORTED,
                "Shared storage is not supported in v1; use Local (ADR-0004)",
            );
        }
        for (k, v) in &self.provider_labels {
            let field = format!("spec.providerLabels[{k}]");
            // A zone cannot cordon its hosts behind faubourg's back: banlieue's cordon
            // label is faubourg's, set from a Host's `cordoned` (ADR-0003, amended). Nor can it
            // put its hosts in another banlieue zone (ADR-0017 decision 1).
            if k.starts_with(LABEL_PREFIX)
                || k == crate::LABEL_BANLIEUE_UNSCHEDULABLE
                || k == crate::LABEL_BANLIEUE_ZONE
            {
                errs.push(
                    &field,
                    REASON_RESERVED_LABEL,
                    format!(
                        "keys under {LABEL_PREFIX}, {} and {} are owned by faubourg",
                        crate::LABEL_BANLIEUE_UNSCHEDULABLE,
                        crate::LABEL_BANLIEUE_ZONE
                    ),
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
    fn provider_labels_may_not_cordon_a_zone_through_banlieue() {
        let mut s = spec(StorageMode::Local);
        s.provider_labels.insert(
            crate::LABEL_BANLIEUE_UNSCHEDULABLE.to_string(),
            "true".to_string(),
        );
        assert_eq!(
            s.validate().unwrap_err().reasons(),
            vec![REASON_RESERVED_LABEL]
        );
    }

    #[test]
    fn provider_labels_may_not_put_a_zone_in_another_banlieue_zone() {
        let mut s = spec(StorageMode::Local);
        s.provider_labels.insert(
            crate::LABEL_BANLIEUE_ZONE.to_string(),
            "elsewhere".to_string(),
        );
        assert_eq!(
            s.validate().unwrap_err().reasons(),
            vec![REASON_RESERVED_LABEL]
        );
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
    fn a_zone_that_names_no_class_uses_the_default() {
        let s: AvailabilityZoneSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(s.provider_class_ref, None);
        assert_eq!(s.provider_class("cloud-hypervisor"), "cloud-hypervisor");
        assert_eq!(
            serde_json::to_value(&s).unwrap().get("providerClassRef"),
            None,
            "a zone from before ADR-0018 keeps its exact shape"
        );
    }

    #[test]
    fn a_zone_that_names_a_class_uses_it() {
        let s: AvailabilityZoneSpec =
            serde_json::from_str(r#"{"providerClassRef": {"name": "libvirt"}}"#).unwrap();
        assert_eq!(s.provider_class("cloud-hypervisor"), "libvirt");
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn a_class_ref_that_is_not_an_object_name_is_refused() {
        for name in ["", "Cloud_Hypervisor", "-ch"] {
            let mut s = spec(StorageMode::Local);
            s.provider_class_ref = Some(ProviderClassRef { name: name.into() });
            assert_eq!(
                s.validate().unwrap_err().reasons(),
                vec![REASON_INVALID_PROVIDER_CLASS_REF],
                "{name:?}"
            );
        }
    }

    #[test]
    fn every_refusal_is_reported() {
        let mut s = spec(StorageMode::Shared);
        s.provider_labels
            .insert(crate::LABEL_HOST.to_string(), "x".to_string());
        assert_eq!(s.validate().unwrap_err().0.len(), 2);
    }
}
