// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! The provider backends faubourg drives, chosen by a zone's `ProviderClass` (ADR-0018).
//!
//! A zone names one class (or the controller's `--provider-class`); the class's `spec.backend`
//! picks a [`Backend`] from a closed set, and its `spec.deployment` must be the one that backend
//! runs as. Anything else fails closed: the zone and every member Host report
//! `ProviderBackendUnsupported`, and no Provider and no Job is made (decision 2).
//!
//! Only `cloud-hypervisor` is driven so far. `libvirt` and `firecracker` are refused until their
//! drivers land (roadmap 04 phases C and D), so a zone of either class never half-works.

use banlieue_api::banlieue::{ProviderClass, ProviderDeployment};
use faubourg_api::zone::AvailabilityZone;

/// Reason: the zone's class is missing, has a backend faubourg does not drive, or runs in a mode
/// that backend does not (ADR-0018 decision 2).
pub const REASON_BACKEND_UNSUPPORTED: &str = "ProviderBackendUnsupported";
/// Reason: the Host's existing Provider references another class than its zone's (decision 3).
pub const REASON_CLASS_MISMATCH: &str = "ProviderClassMismatch";

/// A provider backend faubourg drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// banlieue's host-resident Cloud Hypervisor provider (banlieue ADR-0060).
    CloudHypervisor,
}

impl Backend {
    /// The value of `ProviderClass.spec.backend` this driver serves.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::CloudHypervisor => "cloud-hypervisor",
        }
    }

    /// The deployment mode its class must declare.
    #[must_use]
    pub const fn deployment(self) -> ProviderDeployment {
        match self {
            Self::CloudHypervisor => ProviderDeployment::External,
        }
    }

    /// The Provider's `connection.endpoint` for a host at `address`.
    #[must_use]
    pub fn endpoint(self, address: &str) -> String {
        match self {
            Self::CloudHypervisor => format!("ch://{address}"),
        }
    }

    /// Every driver, for lookups and tests.
    pub const ALL: [Self; 1] = [Self::CloudHypervisor];

    /// The driver for the class named `name`, or why there is none. The message names the class
    /// and its backend, nothing else.
    pub fn of(name: &str, class: Option<&ProviderClass>) -> Result<Self, String> {
        let Some(class) = class else {
            return Err(format!("ProviderClass {name} does not exist"));
        };
        let backend = class.spec.backend.as_str();
        let Some(b) = Self::ALL.into_iter().find(|b| b.name() == backend) else {
            return Err(format!(
                "ProviderClass {name} has backend {backend}, which faubourg does not drive"
            ));
        };
        let mode = class.spec.deployment_mode();
        if mode != b.deployment() {
            return Err(format!(
                "ProviderClass {name} is {mode:?}; backend {backend} runs {:?}",
                b.deployment()
            ));
        }
        Ok(b)
    }
}

/// The name of the class a Host in `zone` uses: the zone's, or `default` when the zone names
/// none or is not there (ADR-0018 decision 1).
#[must_use]
pub fn class_name<'a>(zone: Option<&'a AvailabilityZone>, default: &'a str) -> &'a str {
    zone.map_or(default, |z| z.spec.provider_class(default))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn class(backend: &str, deployment: Option<&str>) -> ProviderClass {
        let mut spec = json!({ "backend": backend });
        if let Some(d) = deployment {
            spec["deployment"] = json!(d);
        }
        serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1", "kind": "ProviderClass",
            "metadata": { "name": "c" }, "spec": spec
        }))
        .unwrap()
    }

    #[test]
    fn an_external_cloud_hypervisor_class_is_driven() {
        let c = class("cloud-hypervisor", Some("External"));
        assert_eq!(Backend::of("c", Some(&c)), Ok(Backend::CloudHypervisor));
        assert_eq!(
            Backend::CloudHypervisor.endpoint("hv01.home.example"),
            "ch://hv01.home.example"
        );
    }

    #[test]
    fn a_missing_class_is_refused() {
        let e = Backend::of("cloud-hypervisor", None).unwrap_err();
        assert!(e.contains("does not exist"), "{e}");
    }

    #[test]
    fn a_backend_without_a_driver_is_refused() {
        // libvirt and firecracker until their drivers land (roadmap 04), and anything else.
        for backend in ["libvirt", "firecracker", "vsphere", "proxmox", ""] {
            let c = class(backend, Some("External"));
            let e = Backend::of("c", Some(&c)).unwrap_err();
            assert!(e.contains("does not drive"), "{backend}: {e}");
        }
    }

    #[test]
    fn a_class_in_the_wrong_mode_is_refused() {
        // banlieue's default is Managed, so a class that says nothing is Managed.
        for deployment in [None, Some("Managed")] {
            let c = class("cloud-hypervisor", deployment);
            let e = Backend::of("c", Some(&c)).unwrap_err();
            assert!(e.contains("Managed"), "{deployment:?}: {e}");
        }
    }

    #[test]
    fn the_zone_names_the_class_or_the_default_does() {
        let named: AvailabilityZone = serde_json::from_value(json!({
            "apiVersion": "faubourg.firestoned.io/v1alpha1", "kind": "AvailabilityZone",
            "metadata": { "name": "az1" },
            "spec": { "providerClassRef": { "name": "ch-canary" } }
        }))
        .unwrap();
        let unnamed: AvailabilityZone = serde_json::from_value(json!({
            "apiVersion": "faubourg.firestoned.io/v1alpha1", "kind": "AvailabilityZone",
            "metadata": { "name": "az1" }, "spec": {}
        }))
        .unwrap();
        assert_eq!(class_name(Some(&named), "cloud-hypervisor"), "ch-canary");
        assert_eq!(
            class_name(Some(&unnamed), "cloud-hypervisor"),
            "cloud-hypervisor"
        );
        assert_eq!(class_name(None, "cloud-hypervisor"), "cloud-hypervisor");
    }
}
