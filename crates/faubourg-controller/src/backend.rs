// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! The provider backends faubourg drives, chosen by a zone's `ProviderClass` (ADR-0018).
//!
//! A zone names one class (or the controller's `--provider-class`); the class's `spec.backend`
//! picks a [`Backend`] from a closed set, and its `spec.deployment` must be the one that backend
//! runs as. Anything else fails closed: the zone and every member Host report
//! `ProviderBackendUnsupported`, and no Provider and no Job is made (decision 2).
//!
//! `cloud-hypervisor` (External, on the host) and `libvirt` (Managed, in the cluster, ADR-0018
//! decisions 6 to 9) are driven. `firecracker` is refused until banlieue ships the class and its
//! driver lands (roadmap 04 phase D), so a zone of it never half-works.

use banlieue_api::banlieue::{ProviderClass, ProviderDeployment};
use faubourg_api::zone::{AvailabilityZone, LibvirtConnection};
use serde_json::{Value, json};

/// Reason: the zone's class is missing, has a backend faubourg does not drive, or runs in a mode
/// that backend does not (ADR-0018 decision 2).
pub const REASON_BACKEND_UNSUPPORTED: &str = "ProviderBackendUnsupported";
/// Reason: the Host's existing Provider references another class than its zone's (decision 3).
pub const REASON_CLASS_MISMATCH: &str = "ProviderClassMismatch";
/// Reason: a libvirt zone has no `spec.libvirt`, or another zone has one (decision 6).
pub const REASON_CONNECTION_INVALID: &str = "ProviderConnectionInvalid";

/// A provider backend faubourg drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// banlieue's host-resident Cloud Hypervisor provider (banlieue ADR-0060).
    CloudHypervisor,
    /// banlieue's in-cluster libvirt provider, dialing libvirtd over mutual TLS (banlieue
    /// ADR-0011).
    Libvirt,
}

impl Backend {
    /// The value of `ProviderClass.spec.backend` this driver serves.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::CloudHypervisor => "cloud-hypervisor",
            Self::Libvirt => "libvirt",
        }
    }

    /// The deployment mode its class must declare.
    #[must_use]
    pub const fn deployment(self) -> ProviderDeployment {
        match self {
            Self::CloudHypervisor => ProviderDeployment::External,
            Self::Libvirt => ProviderDeployment::Managed,
        }
    }

    /// Whether the bootstrap mints the host's first provider token and hands it to the install
    /// script (ADR-0011): only for a provider that runs on the host. A Managed provider runs in
    /// the cluster under banlieue's operator, so its host gets no credential (ADR-0018
    /// decision 7).
    #[must_use]
    pub fn mints_token(self) -> bool {
        self.deployment() == ProviderDeployment::External
    }

    /// Whether banlieue can move a VM between two hosts of this backend (ADR-0018 decision 9;
    /// banlieue ADR-0127 moves Cloud Hypervisor VMs only).
    #[must_use]
    pub const fn moves_vms(self) -> bool {
        matches!(self, Self::CloudHypervisor)
    }

    /// The Provider's `connection.endpoint` for a host at `address`.
    #[must_use]
    pub fn endpoint(self, address: &str) -> String {
        match self {
            Self::CloudHypervisor => format!("ch://{address}"),
            Self::Libvirt => format!("qemu+tls://{address}/system"),
        }
    }

    /// The Provider's `spec.connection` for a host at `address` in `zone`: the endpoint, and for
    /// libvirt the zone's client credential and CA (ADR-0018 decision 6). Never
    /// `insecureSkipTLSVerify`, never an inline CA.
    #[must_use]
    pub fn connection(self, address: &str, zone: Option<&AvailabilityZone>) -> Value {
        let mut c = json!({ "endpoint": self.endpoint(address) });
        if let (Self::Libvirt, Some(l)) = (self, libvirt_of(zone)) {
            c["credentialsRef"] = json!({ "name": l.credentials_ref.name });
            c["caBundle"] = json!({
                "configMapRef": { "name": l.ca_bundle_ref.name, "key": l.ca_bundle_ref.key }
            });
        }
        c
    }

    /// Whether `zone` carries what this backend needs, and nothing it does not: a libvirt zone
    /// has `spec.libvirt`, any other has none (ADR-0018 decision 6). A missing zone is the
    /// phase machine's to report.
    pub fn check_zone(self, zone: Option<&AvailabilityZone>) -> Result<(), String> {
        let Some(z) = zone else { return Ok(()) };
        match (self, z.spec.libvirt.is_some()) {
            (Self::Libvirt, false) => Err(
                "a zone whose class has backend libvirt needs spec.libvirt: the client credential \
                 and CA banlieue's provider uses"
                    .into(),
            ),
            (Self::CloudHypervisor, true) => Err(format!(
                "spec.libvirt is only for a libvirt zone; this zone's backend is {}",
                self.name()
            )),
            _ => Ok(()),
        }
    }

    /// Every driver, for lookups and tests.
    pub const ALL: [Self; 2] = [Self::CloudHypervisor, Self::Libvirt];

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

fn libvirt_of(zone: Option<&AvailabilityZone>) -> Option<&LibvirtConnection> {
    zone.and_then(|z| z.spec.libvirt.as_ref())
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
    fn a_managed_libvirt_class_is_driven() {
        for deployment in [None, Some("Managed")] {
            let c = class("libvirt", deployment);
            assert_eq!(Backend::of("c", Some(&c)), Ok(Backend::Libvirt));
        }
        let e = Backend::of("c", Some(&class("libvirt", Some("External")))).unwrap_err();
        assert!(e.contains("External"), "{e}");
        assert_eq!(
            Backend::Libvirt.endpoint("kvm01.home.example"),
            "qemu+tls://kvm01.home.example/system"
        );
    }

    #[test]
    fn only_a_provider_on_the_host_gets_a_token_and_only_cloud_hypervisor_moves() {
        assert!(Backend::CloudHypervisor.mints_token());
        assert!(!Backend::Libvirt.mints_token());
        assert!(Backend::CloudHypervisor.moves_vms());
        assert!(!Backend::Libvirt.moves_vms());
    }

    fn zone(spec: &serde_json::Value) -> AvailabilityZone {
        serde_json::from_value(json!({
            "apiVersion": "faubourg.firestoned.io/v1alpha1", "kind": "AvailabilityZone",
            "metadata": { "name": "az1" }, "spec": spec
        }))
        .unwrap()
    }

    fn libvirt_zone() -> AvailabilityZone {
        zone(&json!({ "libvirt": {
            "credentialsRef": { "name": "faubourg-kvm-client" },
            "caBundleRef": { "name": "faubourg-kvm-ca", "key": "ca.pem" } } }))
    }

    #[test]
    fn a_libvirt_provider_gets_the_zones_credential_and_ca() {
        let z = libvirt_zone();
        assert_eq!(
            Backend::Libvirt.connection("kvm01.home.example", Some(&z)),
            json!({
                "endpoint": "qemu+tls://kvm01.home.example/system",
                "credentialsRef": { "name": "faubourg-kvm-client" },
                "caBundle": { "configMapRef": { "name": "faubourg-kvm-ca", "key": "ca.pem" } },
            })
        );
        assert_eq!(
            Backend::CloudHypervisor.connection("hv01.home.example", Some(&zone(&json!({})))),
            json!({ "endpoint": "ch://hv01.home.example" }),
            "a host-resident provider reads no Secret (banlieue ADR-0060 decision 4)"
        );
    }

    #[test]
    fn a_zone_carries_a_libvirt_connection_exactly_when_its_backend_is_libvirt() {
        let plain = zone(&json!({}));
        let kvm = libvirt_zone();
        assert_eq!(Backend::Libvirt.check_zone(Some(&kvm)), Ok(()));
        assert_eq!(Backend::CloudHypervisor.check_zone(Some(&plain)), Ok(()));
        assert!(Backend::Libvirt.check_zone(Some(&plain)).is_err());
        assert!(Backend::CloudHypervisor.check_zone(Some(&kvm)).is_err());
        assert_eq!(
            Backend::Libvirt.check_zone(None),
            Ok(()),
            "a missing zone is reported as one"
        );
    }

    #[test]
    fn a_backend_without_a_driver_is_refused() {
        // firecracker until its driver lands (roadmap 04 phase D), and anything else.
        for backend in ["firecracker", "vsphere", "proxmox", ""] {
            let c = class(backend, Some("External"));
            let e = Backend::of("c", Some(&c)).unwrap_err();
            assert!(e.contains("does not drive"), "{backend}: {e}");
        }
    }

    #[test]
    fn a_cloud_hypervisor_class_in_the_wrong_mode_is_refused() {
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
