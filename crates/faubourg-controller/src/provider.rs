// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! The host's banlieue Provider, as faubourg applies it (ADR-0011, ADR-0003 decision 4).
//!
//! - A bootstrapped Host gets a Provider named after it: class, endpoint and labels.
//! - An adopted Host's existing Provider gets the labels only.
//! - faubourg never deletes one.
//!
//! Pure functions: the apply bodies are built here and sent by `controller.rs` under the
//! Host controller's field manager.

use std::collections::BTreeMap;

use banlieue_api::banlieue::{Provider, ProviderClass, ProviderDeployment, WorkloadMode};
use faubourg_api::host::{BootstrapMethod, Host};
use faubourg_api::zone::AvailabilityZone;
use faubourg_api::{LABEL_CORDONED, LABEL_HOST, LABEL_ZONE};
use serde_json::{Value, json};

/// Reason: the configured `ProviderClass` is missing or not `External` (ADR-0011 decision 1).
pub const REASON_CLASS_NOT_EXTERNAL: &str = "ProviderClassNotExternal";
/// Reason: waiting for banlieue's operator to give the Provider its identity.
pub const REASON_AWAITING_IDENTITY: &str = "AwaitingProviderIdentity";

/// The labels faubourg owns on a host's Provider: the zone's `providerLabels`, then the zone,
/// the host and, while cordoned, the cordon label. `providerLabels` cannot set a key under
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
    }
    l
}

/// The server-side apply body for the host's Provider (ADR-0011 decisions 2 and 3): for a
/// bootstrapped Host, class, endpoint and labels; for an adopted one, labels only. `None` for a
/// Host with no name.
pub fn apply_body(
    host: &Host,
    zone: Option<&AvailabilityZone>,
    class: &str,
    namespace: &str,
) -> Option<Value> {
    let name = host.metadata.name.as_deref()?;
    let metadata = json!({ "name": name, "namespace": namespace, "labels": labels(host, zone) });
    let body = match host.spec.bootstrap.method {
        BootstrapMethod::Script => json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "Provider",
            "metadata": metadata,
            "spec": {
                "providerClassRef": { "name": class },
                "connection": { "endpoint": format!("ch://{}", host.spec.address) },
            },
        }),
        BootstrapMethod::None => json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "Provider",
            "metadata": metadata,
        }),
    };
    Some(body)
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
    fn a_bootstrapped_host_gets_class_endpoint_and_labels() {
        let z = zone(&json!({ "topology.home.example/rack": "r1" }), "Local");
        let b = apply_body(
            &host(BootstrapMethod::Script),
            Some(&z),
            "cloud-hypervisor",
            "banlieue-system",
        )
        .unwrap();
        assert_eq!(b["metadata"]["name"], "hv01");
        assert_eq!(b["metadata"]["namespace"], "banlieue-system");
        assert_eq!(b["spec"]["providerClassRef"]["name"], "cloud-hypervisor");
        assert_eq!(
            b["spec"]["connection"]["endpoint"],
            "ch://hv01.home.example"
        );
        let l = &b["metadata"]["labels"];
        assert_eq!(l[LABEL_ZONE], "az1");
        assert_eq!(l[LABEL_HOST], "hv01");
        assert_eq!(l["topology.home.example/rack"], "r1");
        assert!(l.get(LABEL_CORDONED).is_none());
        // The body parses as a Provider: what the API server will validate.
        let _: Provider = serde_json::from_value(b).unwrap();
    }

    #[test]
    fn an_adopted_host_gets_labels_and_nothing_else() {
        let b = apply_body(&host(BootstrapMethod::None), None, "cloud-hypervisor", "ns").unwrap();
        assert!(b.get("spec").is_none(), "never an adopted Provider's spec");
        assert_eq!(b["metadata"]["labels"][LABEL_HOST], "hv01");
    }

    #[test]
    fn a_cordoned_host_is_labelled_and_the_label_goes_with_the_cordon() {
        let mut h = host(BootstrapMethod::Script);
        h.spec.cordoned = true;
        assert_eq!(
            labels(&h, None).get(LABEL_CORDONED).map(String::as_str),
            Some("true")
        );
        h.spec.cordoned = false;
        assert!(!labels(&h, None).contains_key(LABEL_CORDONED));
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
