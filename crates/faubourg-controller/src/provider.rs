// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! The host's banlieue Provider, as faubourg applies it (ADR-0011, ADR-0003 decision 4).
//!
//! - A bootstrapped Host gets a Provider named after it: class, endpoint and labels.
//! - An adopted Host's existing Provider gets the labels only.
//! - faubourg never deletes one.
//!
//! Pure functions: the apply bodies are built here and sent by `controller.rs`. The spec and
//! the labels have separate field managers (ADR-0011, amended), so a Host switched from
//! `Script` to `None` stops writing the spec without server-side apply removing it.

use std::collections::BTreeMap;

use banlieue_api::banlieue::{Provider, ProviderClass, ProviderDeployment, WorkloadMode};
use faubourg_api::host::{BootstrapMethod, Host};
use faubourg_api::zone::AvailabilityZone;
use faubourg_api::{LABEL_BANLIEUE_UNSCHEDULABLE, LABEL_CORDONED, LABEL_HOST, LABEL_ZONE};
use serde_json::{Value, json};

/// The field manager for the labels faubourg owns on a Provider, and for everything else the
/// Host controller writes.
pub const LABELS_MANAGER: &str = "faubourg.firestoned.io/host-controller";
/// The field manager for a bootstrapped Host's Provider spec: `providerClassRef` and
/// `connection.endpoint`.
pub const SPEC_MANAGER: &str = "faubourg.firestoned.io/host-provider-spec";

/// Reason: the configured `ProviderClass` is missing or not `External` (ADR-0011 decision 1).
pub const REASON_CLASS_NOT_EXTERNAL: &str = "ProviderClassNotExternal";
/// Reason: waiting for banlieue's operator to give the Provider its identity.
pub const REASON_AWAITING_IDENTITY: &str = "AwaitingProviderIdentity";

/// The labels faubourg owns on a host's Provider: the zone's `providerLabels`, then the zone,
/// the host and, while cordoned, faubourg's cordon label and banlieue's (`banlieue.io/unschedulable`,
/// which banlieue's scheduler enforces). `providerLabels` cannot set a key under
/// `faubourg.firestoned.io/` (the zone's validation refuses it), and faubourg's own are
/// written last regardless.
pub fn labels(host: &Host, zone: Option<&AvailabilityZone>) -> BTreeMap<String, String> {
    let mut l: BTreeMap<String, String> = zone
        .filter(|z| z.spec.validate().is_ok())
        .map(|z| z.spec.provider_labels.clone())
        .unwrap_or_default();
    l.insert(LABEL_ZONE.into(), host.spec.zone_ref.clone());
    if let Some(name) = host.metadata.name.as_deref() {
        l.insert(LABEL_HOST.into(), name.into());
    }
    if host.spec.cordoned {
        l.insert(LABEL_CORDONED.into(), "true".into());
        // banlieue's scheduler keeps every new VM off it (banlieue ADR-0124).
        l.insert(LABEL_BANLIEUE_UNSCHEDULABLE.into(), "true".into());
    }
    l
}

/// The labels' apply body, under [`LABELS_MANAGER`], for every Host (ADR-0011 decisions 2
/// and 3): the labels and nothing in `spec`. `None` for a Host with no name.
pub fn labels_body(host: &Host, zone: Option<&AvailabilityZone>, namespace: &str) -> Option<Value> {
    let name = host.metadata.name.as_deref()?;
    Some(json!({
        "apiVersion": "banlieue.io/v1alpha1",
        "kind": "Provider",
        "metadata": { "name": name, "namespace": namespace, "labels": labels(host, zone) },
    }))
}

/// The spec's apply body, under [`SPEC_MANAGER`], for a bootstrapped Host (ADR-0011 decision
/// 2): class and endpoint (`ch://<address>`). `None` for an adopted Host, whose spec faubourg
/// never sets, and for a Host with no name.
pub fn spec_body(host: &Host, class: &str, namespace: &str) -> Option<Value> {
    if host.spec.bootstrap.method != BootstrapMethod::Script {
        return None;
    }
    let name = host.metadata.name.as_deref()?;
    Some(spec_apply(
        name,
        namespace,
        class,
        &format!("ch://{}", host.spec.address),
    ))
}

/// For a Provider whose spec the labels' manager still owns, as a v0.1 controller left it:
/// its current class and endpoint, unchanged, for [`SPEC_MANAGER`] to take over before the
/// labels' apply would remove them (ADR-0011, amended). `None` otherwise.
pub fn migrated_spec_body(provider: &Provider) -> Option<Value> {
    let owned = provider
        .metadata
        .managed_fields
        .iter()
        .flatten()
        .filter(|m| {
            m.manager.as_deref() == Some(LABELS_MANAGER)
                && m.operation.as_deref() == Some("Apply")
                && m.subresource.as_deref().is_none_or(str::is_empty)
        })
        .filter_map(|m| m.fields_v1.as_ref())
        .any(|f| {
            let spec = &f.0["f:spec"];
            spec.get("f:providerClassRef").is_some()
                || spec["f:connection"].get("f:endpoint").is_some()
        });
    if !owned {
        return None;
    }
    Some(spec_apply(
        provider.metadata.name.as_deref()?,
        provider.metadata.namespace.as_deref()?,
        &provider.spec.provider_class_ref.name,
        &provider.spec.connection.endpoint,
    ))
}

fn spec_apply(name: &str, namespace: &str, class: &str, endpoint: &str) -> Value {
    json!({
        "apiVersion": "banlieue.io/v1alpha1",
        "kind": "Provider",
        "metadata": { "name": name, "namespace": namespace },
        "spec": {
            "providerClassRef": { "name": class },
            "connection": { "endpoint": endpoint },
        },
    })
}

/// True when the class exists and is `External` (banlieue ADR-0060 decision 3).
pub fn class_is_external(class: Option<&ProviderClass>) -> bool {
    class.is_some_and(|c| c.spec.deployment_mode() == ProviderDeployment::External)
}

/// True once banlieue's operator has given the Provider its identity, which it reports as
/// `status.workload.mode: External`.
pub fn identity_ready(provider: Option<&Provider>) -> bool {
    provider
        .and_then(|p| p.status.as_ref())
        .and_then(|s| s.workload.as_ref())
        .is_some_and(|w| w.mode == WorkloadMode::External)
}

#[cfg(test)]
mod tests {
    use super::*;
    use faubourg_api::host::{BootstrapSpec, HostSpec, SecretName, SshSpec};

    fn host(method: BootstrapMethod) -> Host {
        Host::new(
            "hv01",
            HostSpec {
                zone_ref: "az1".into(),
                address: "hv01.home.example".into(),
                ssh: (method == BootstrapMethod::Script).then(|| SshSpec {
                    port: 22,
                    secret_ref: SecretName {
                        name: "hv01-ssh".into(),
                    },
                    ..SshSpec::default()
                }),
                bootstrap: BootstrapSpec {
                    method,
                    script_ref: None,
                },
                cordoned: false,
            },
        )
    }

    fn zone(labels: &Value, mode: &str) -> AvailabilityZone {
        serde_json::from_value(json!({
            "apiVersion": "faubourg.firestoned.io/v1alpha1", "kind": "AvailabilityZone",
            "metadata": { "name": "az1" },
            "spec": { "storage": { "mode": mode }, "providerLabels": labels }
        }))
        .unwrap()
    }

    fn class(deployment: Option<&str>) -> ProviderClass {
        let mut spec = json!({ "backend": "cloud-hypervisor" });
        if let Some(d) = deployment {
            spec["deployment"] = json!(d);
        }
        serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1", "kind": "ProviderClass",
            "metadata": { "name": "cloud-hypervisor" }, "spec": spec
        }))
        .unwrap()
    }

    #[test]
    fn a_bootstrapped_host_gets_class_and_endpoint_under_the_spec_manager() {
        let b = spec_body(
            &host(BootstrapMethod::Script),
            "cloud-hypervisor",
            "banlieue-system",
        )
        .unwrap();
        assert_eq!(b["metadata"]["name"], "hv01");
        assert_eq!(b["metadata"]["namespace"], "banlieue-system");
        assert!(
            b["metadata"].get("labels").is_none(),
            "labels are the other manager's"
        );
        assert_eq!(b["spec"]["providerClassRef"]["name"], "cloud-hypervisor");
        assert_eq!(
            b["spec"]["connection"]["endpoint"],
            "ch://hv01.home.example"
        );
        // The body parses as a Provider: what the API server will validate.
        let _: Provider = serde_json::from_value(b).unwrap();
        assert_eq!(
            spec_body(&host(BootstrapMethod::None), "cloud-hypervisor", "ns"),
            None,
            "never an adopted Provider's spec"
        );
    }

    #[test]
    fn every_host_gets_labels_and_nothing_else_under_the_labels_manager() {
        let z = zone(&json!({ "topology.home.example/rack": "r1" }), "Local");
        for method in [BootstrapMethod::Script, BootstrapMethod::None] {
            let b = labels_body(&host(method), Some(&z), "banlieue-system").unwrap();
            assert!(b.get("spec").is_none(), "{method:?}: no spec");
            assert_eq!(b["metadata"]["namespace"], "banlieue-system");
            let l = &b["metadata"]["labels"];
            assert_eq!(l[LABEL_ZONE], "az1");
            assert_eq!(l[LABEL_HOST], "hv01");
            assert_eq!(l["topology.home.example/rack"], "r1");
            assert!(l.get(LABEL_CORDONED).is_none());
        }
        assert_ne!(SPEC_MANAGER, LABELS_MANAGER);
    }

    fn managed(entries: &Value) -> Provider {
        let mut p = provider(&json!({}));
        p.metadata.managed_fields = Some(serde_json::from_value(entries.clone()).unwrap());
        p
    }

    fn spec_fields() -> Value {
        json!({ "f:spec": { "f:providerClassRef": { "f:name": {} },
                            "f:connection": { "f:endpoint": {} } } })
    }

    #[test]
    fn a_spec_the_labels_manager_still_owns_is_moved_unchanged() {
        let p = managed(&json!([
            { "manager": LABELS_MANAGER, "operation": "Apply", "fieldsV1": spec_fields() }
        ]));
        let b = migrated_spec_body(&p).expect("moved to the spec manager");
        assert_eq!(b["metadata"]["name"], "hv01");
        assert_eq!(b["metadata"]["namespace"], "banlieue-system");
        assert_eq!(b["spec"]["providerClassRef"]["name"], "cloud-hypervisor");
        assert_eq!(
            b["spec"]["connection"]["endpoint"],
            "ch://hv01.home.example"
        );
        assert!(b["metadata"].get("labels").is_none());
    }

    #[test]
    fn nothing_moves_unless_the_labels_manager_owns_the_spec() {
        for entries in [
            json!([]),
            json!([{ "manager": LABELS_MANAGER, "operation": "Apply",
                     "fieldsV1": { "f:metadata": { "f:labels": {} } } }]),
            json!([{ "manager": SPEC_MANAGER, "operation": "Apply", "fieldsV1": spec_fields() }]),
            json!([{ "manager": "kubectl", "operation": "Apply", "fieldsV1": spec_fields() }]),
            json!([{ "manager": LABELS_MANAGER, "operation": "Update", "fieldsV1": spec_fields() }]),
            json!([{ "manager": LABELS_MANAGER, "operation": "Apply", "subresource": "status",
                     "fieldsV1": spec_fields() }]),
        ] {
            assert_eq!(migrated_spec_body(&managed(&entries)), None, "{entries}");
        }
        assert_eq!(
            migrated_spec_body(&provider(&json!({}))),
            None,
            "no managedFields"
        );
    }

    #[test]
    fn a_cordoned_host_is_labelled_and_the_label_goes_with_the_cordon() {
        for method in [BootstrapMethod::Script, BootstrapMethod::None] {
            let mut h = host(method);
            h.spec.cordoned = true;
            for key in [LABEL_CORDONED, LABEL_BANLIEUE_UNSCHEDULABLE] {
                assert_eq!(
                    labels(&h, None).get(key).map(String::as_str),
                    Some("true"),
                    "{method:?} {key}"
                );
            }
            h.spec.cordoned = false;
            for key in [LABEL_CORDONED, LABEL_BANLIEUE_UNSCHEDULABLE] {
                assert!(!labels(&h, None).contains_key(key), "{method:?} {key}");
            }
        }
    }

    #[test]
    fn faubourgs_copy_of_banlieues_cordon_label_matches_banlieue() {
        assert_eq!(
            LABEL_BANLIEUE_UNSCHEDULABLE,
            banlieue_api::banlieue::LABEL_UNSCHEDULABLE
        );
    }

    #[test]
    fn faubourgs_labels_win_and_an_unsupported_zone_adds_none() {
        let mut z = zone(&json!({ "extra": "x" }), "Local");
        z.spec
            .provider_labels
            .insert(LABEL_HOST.into(), "someone-else".into());
        assert_eq!(
            labels(&host(BootstrapMethod::Script), Some(&z))[LABEL_HOST],
            "hv01"
        );
        let shared = zone(&json!({ "extra": "x" }), "Shared");
        assert!(!labels(&host(BootstrapMethod::Script), Some(&shared)).contains_key("extra"));
    }

    #[test]
    fn only_an_external_class_will_do() {
        assert!(class_is_external(Some(&class(Some("External")))));
        assert!(!class_is_external(Some(&class(Some("Managed")))));
        assert!(
            !class_is_external(Some(&class(None))),
            "the default is Managed"
        );
        assert!(!class_is_external(None));
    }

    fn provider(status: &Value) -> Provider {
        serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1", "kind": "Provider",
            "metadata": { "name": "hv01", "namespace": "banlieue-system" },
            "spec": { "providerClassRef": { "name": "cloud-hypervisor" },
                      "connection": { "endpoint": "ch://hv01.home.example" } },
            "status": status
        }))
        .unwrap()
    }

    #[test]
    fn the_identity_is_ready_once_banlieue_reports_external() {
        assert!(!identity_ready(None));
        assert!(!identity_ready(Some(&provider(&json!({})))));
        assert!(!identity_ready(Some(&provider(
            &json!({ "workload": { "mode": "Managed", "namespace": "banlieue-system" } })
        ))));
        assert!(identity_ready(Some(&provider(
            &json!({ "workload": { "mode": "External", "namespace": "banlieue-system" } })
        ))));
    }
}
