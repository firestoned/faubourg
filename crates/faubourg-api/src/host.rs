// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! `Host`: one bare-metal machine running Cloud Hypervisor under banlieue's host-resident
//! provider (ADR-0002, ADR-0003), bootstrapped by a per-host Job running one root-owned install script (ADR-0005) with an SSH
//! credential it references but never embeds (ADR-0006).

use std::net::IpAddr;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{CustomResource, KubeSchema};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::validation::{Errors, Invalid, is_dns_subdomain};

/// Condition reason for a field that is not a valid object name.
pub const REASON_INVALID_NAME: &str = "InvalidName";
/// Condition reason for an address that is neither an IP nor a lowercase DNS name.
pub const REASON_INVALID_ADDRESS: &str = "InvalidAddress";
/// Condition reason for a bootstrap that needs SSH settings and has none.
pub const REASON_SSH_REQUIRED: &str = "SshRequired";
/// Condition reason for port 0.
pub const REASON_INVALID_PORT: &str = "InvalidPort";
/// Condition reason for a script key that is not a plain file name.
pub const REASON_INVALID_KEY: &str = "InvalidKey";
/// Condition reason when the placement and run credentials are the same Secret.
pub const REASON_SHARED_CREDENTIAL: &str = "SharedCredential";

/// Default key of the install script in its `ConfigMap` or `Secret`.
pub const DEFAULT_SCRIPT_KEY: &str = "install.sh";
/// Where the script lives on the host, root-owned (ADR-0005 decision 3).
pub const HOST_SCRIPT_PATH: &str = "/opt/faubourg/install.sh";

/// Desired state of a host.
///
/// A Host name is a label value on its bootstrap Job, its pods and its `Provider`, so the CRD
/// refuses one longer than 63 characters (ADR-0009 decision 10).
#[derive(CustomResource, Clone, Debug, Default, PartialEq, Serialize, Deserialize, KubeSchema)]
#[kube(
    group = "faubourg.firestoned.io",
    version = "v1alpha1",
    kind = "Host",
    plural = "hosts",
    status = "HostStatus",
    validation = Rule::new("self.metadata.name.size() <= 63")
        .message("a Host name is at most 63 characters: it is a label value"),
    printcolumn = r#"{"name":"Zone","type":"string","jsonPath":".spec.zoneRef"}"#,
    printcolumn = r#"{"name":"Address","type":"string","jsonPath":".spec.address"}"#,
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Cordoned","type":"boolean","jsonPath":".spec.cordoned"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct HostSpec {
    /// Name of the `AvailabilityZone` this host belongs to.
    pub zone_ref: String,
    /// IP address or DNS name the bootstrap Job reaches over SSH.
    pub address: String,
    /// SSH settings. Required unless `bootstrap.method` is `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<SshSpec>,
    /// How the host is prepared (ADR-0005).
    #[serde(default)]
    pub bootstrap: BootstrapSpec,
    /// Mark the host's `Provider` cordoned (ADR-0003 decision 5).
    #[serde(default)]
    pub cordoned: bool,
}

/// How the bootstrap Job reaches the host (ADR-0006).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SshSpec {
    /// SSH port.
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    /// The scoped run credential, in the controller's credentials namespace. Its account's
    /// only sudo rule runs the install script (ADR-0005 decision 2).
    pub secret_ref: SecretName,
    /// Optional privileged credential, used only to place or update the script and its
    /// sudoers rule (ADR-0005 decision 4). Omit when hosts are prepared out of band.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement_secret_ref: Option<SecretName>,
    /// How an unknown host key is treated.
    #[serde(default)]
    pub host_key_policy: HostKeyPolicy,
    /// Remove the placement key from the host once placement succeeds.
    #[serde(default)]
    pub revoke_after_bootstrap: bool,
}

fn default_ssh_port() -> u16 {
    22
}

/// A Secret name. The namespace is the controller's, never the Host's.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SecretName {
    /// Secret name.
    pub name: String,
}

/// Host key verification policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum HostKeyPolicy {
    /// Refuse a host whose key is not in the Secret's `knownHosts`.
    #[default]
    Strict,
    /// Accept the first key seen, record its fingerprint in status, and pin it.
    TrustOnFirstUse,
}

/// How the host is prepared.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapSpec {
    /// The bootstrap method.
    #[serde(default)]
    pub method: BootstrapMethod,
    /// Where the install script comes from. Omitted: the controller's configured default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_ref: Option<ScriptRef>,
}

/// A `ConfigMap` or `Secret` key, in the credentials namespace where the bootstrap Job runs
/// (ADR-0005 decision 1), holding the install script.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScriptRef {
    /// `ConfigMap` or `Secret`.
    pub kind: ScriptSourceKind,
    /// Object name.
    pub name: String,
    /// Key holding the script.
    #[serde(default = "default_script_key")]
    pub key: String,
}

fn default_script_key() -> String {
    DEFAULT_SCRIPT_KEY.to_string()
}

/// Kind of object holding the install script.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ScriptSourceKind {
    /// A `ConfigMap`. The script is not secret.
    ConfigMap,
    /// A Secret, when the script embeds something that is.
    Secret,
}

/// Bootstrap method.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum BootstrapMethod {
    /// A per-host Job places `/opt/faubourg/install.sh` if needed and runs it under the
    /// scoped account's one sudo rule (ADR-0005).
    #[default]
    Script,
    /// Adopt a host that already runs banlieue's provider. No Job, no SSH.
    None,
}

/// Lifecycle phase (ADR-0007).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum HostPhase {
    /// Accepted, nothing started.
    Pending,
    /// The bootstrap Job is running.
    Bootstrapping,
    /// The host's `Provider` is `Ready=True` with a live heartbeat.
    Ready,
    /// Was ready; the `Provider` is no longer ready or its heartbeat is stale.
    Degraded,
    /// The bootstrap Job failed, or the spec was refused.
    Failed,
}

/// Observed state of a host.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostStatus {
    /// Lifecycle phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<HostPhase>,
    /// Name of the banlieue `Provider` faubourg created for this host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_ref: Option<String>,
    /// Fingerprint of the host key accepted under `TrustOnFirstUse`, pinned afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_host_key: Option<String>,
    /// Conditions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// The generation this status describes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    /// Last human-readable message. Never contains secret material or command lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl HostSpec {
    /// Validates the spec, failing closed. Every refused field is reported.
    pub fn validate(&self) -> Result<(), Invalid> {
        let mut errs = Errors::default();
        if !is_dns_subdomain(&self.zone_ref) {
            errs.push(
                "spec.zoneRef",
                REASON_INVALID_NAME,
                "not a valid object name",
            );
        }
        if !is_address(&self.address) {
            errs.push(
                "spec.address",
                REASON_INVALID_ADDRESS,
                "must be an IP address or a lowercase DNS name",
            );
        }
        match (&self.bootstrap.method, &self.ssh) {
            (BootstrapMethod::Script, None) => errs.push(
                "spec.ssh",
                REASON_SSH_REQUIRED,
                "bootstrap method Script needs ssh",
            ),
            (_, Some(ssh)) => {
                if ssh.port == 0 {
                    errs.push("spec.ssh.port", REASON_INVALID_PORT, "port 0");
                }
                if !is_dns_subdomain(&ssh.secret_ref.name) {
                    errs.push(
                        "spec.ssh.secretRef.name",
                        REASON_INVALID_NAME,
                        "not a valid object name",
                    );
                }
                if let Some(p) = &ssh.placement_secret_ref {
                    if !is_dns_subdomain(&p.name) {
                        errs.push(
                            "spec.ssh.placementSecretRef.name",
                            REASON_INVALID_NAME,
                            "not a valid object name",
                        );
                    } else if p.name == ssh.secret_ref.name {
                        errs.push(
                            "spec.ssh.placementSecretRef.name",
                            REASON_SHARED_CREDENTIAL,
                            "the placement credential must differ from the scoped run credential",
                        );
                    }
                }
            }
            (BootstrapMethod::None, None) => {}
        }
        if let Some(r) = &self.bootstrap.script_ref {
            if !is_dns_subdomain(&r.name) {
                errs.push(
                    "spec.bootstrap.scriptRef.name",
                    REASON_INVALID_NAME,
                    "not a valid object name",
                );
            }
            if !is_config_key(&r.key) {
                errs.push(
                    "spec.bootstrap.scriptRef.key",
                    REASON_INVALID_KEY,
                    "must be a ConfigMap or Secret key of [A-Za-z0-9._-]",
                );
            }
        }
        errs.finish()
    }
}

/// An IP address, or a lowercase DNS name. A DNS label cannot start with `-`, so an address
/// can never be read as an SSH option.
fn is_address(s: &str) -> bool {
    s.parse::<IpAddr>().is_ok() || is_dns_subdomain(s)
}

/// A `ConfigMap` or `Secret` data key: `[A-Za-z0-9._-]`, at most 253, not `.` or `..`.
fn is_config_key(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> HostSpec {
        HostSpec {
            zone_ref: "az1".to_string(),
            address: "192.0.2.10".to_string(),
            ssh: Some(SshSpec {
                port: 22,
                secret_ref: SecretName {
                    name: "hv01-ssh".to_string(),
                },
                placement_secret_ref: None,
                host_key_policy: HostKeyPolicy::Strict,
                revoke_after_bootstrap: false,
            }),
            bootstrap: BootstrapSpec::default(),
            cordoned: false,
        }
    }

    #[test]
    fn a_complete_host_is_valid() {
        assert_eq!(valid().validate(), Ok(()));
    }

    #[test]
    fn defaults_are_the_safe_ones() {
        let s: HostSpec = serde_json::from_str(
            r#"{"zoneRef":"az1","address":"hv01.home.example","ssh":{"secretRef":{"name":"k"}}}"#,
        )
        .unwrap();
        let ssh = s.ssh.unwrap();
        assert_eq!(ssh.port, 22);
        assert_eq!(ssh.host_key_policy, HostKeyPolicy::Strict);
        assert!(!ssh.revoke_after_bootstrap);
        assert_eq!(s.bootstrap.method, BootstrapMethod::Script);
        assert_eq!(s.bootstrap.script_ref, None);
        assert_eq!(ssh.placement_secret_ref, None);
        assert!(!s.cordoned);
    }

    #[test]
    fn install_without_ssh_is_refused() {
        let mut s = valid();
        s.ssh = None;
        assert_eq!(
            s.validate().unwrap_err().reasons(),
            vec![REASON_SSH_REQUIRED]
        );
    }

    #[test]
    fn adopting_needs_no_ssh() {
        let mut s = valid();
        s.ssh = None;
        s.bootstrap.method = BootstrapMethod::None;
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn addresses_that_could_be_ssh_options_or_shell_are_refused() {
        for a in [
            "",
            "-oProxyCommand=sh",
            "hv01;reboot",
            "hv 01",
            "HV01",
            "hv01$(id)",
        ] {
            let mut s = valid();
            s.address = a.to_string();
            assert_eq!(
                s.validate().unwrap_err().reasons(),
                vec![REASON_INVALID_ADDRESS],
                "{a:?}"
            );
        }
        for a in ["2001:db8::10", "hv01.home.example", "198.51.100.7"] {
            let mut s = valid();
            s.address = a.to_string();
            assert_eq!(s.validate(), Ok(()), "{a:?}");
        }
    }

    #[test]
    fn names_and_port_are_checked() {
        let mut s = valid();
        s.zone_ref = "AZ_1".to_string();
        let ssh = s.ssh.as_mut().unwrap();
        ssh.port = 0;
        ssh.secret_ref.name = "../etc".to_string();
        assert_eq!(
            s.validate().unwrap_err().reasons(),
            vec![
                REASON_INVALID_NAME,
                REASON_INVALID_PORT,
                REASON_INVALID_NAME
            ]
        );
    }

    #[test]
    fn script_ref_defaults_its_key() {
        let r: ScriptRef =
            serde_json::from_str(r#"{"kind":"ConfigMap","name":"faubourg-install"}"#).unwrap();
        assert_eq!(r.key, DEFAULT_SCRIPT_KEY);
    }

    #[test]
    fn script_ref_is_checked() {
        for (name, key) in [
            ("Bad Name", "install.sh"),
            ("ok", "../install.sh"),
            ("ok", ".."),
        ] {
            let mut s = valid();
            s.bootstrap.script_ref = Some(ScriptRef {
                kind: ScriptSourceKind::Secret,
                name: name.to_string(),
                key: key.to_string(),
            });
            assert!(s.validate().is_err(), "{name}/{key}");
        }
        let mut s = valid();
        s.bootstrap.script_ref = Some(ScriptRef {
            kind: ScriptSourceKind::ConfigMap,
            name: "faubourg-install".to_string(),
            key: "install-v2.sh".to_string(),
        });
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn placement_credential_must_differ_from_the_scoped_one() {
        let mut s = valid();
        let ssh = s.ssh.as_mut().unwrap();
        ssh.placement_secret_ref = Some(ssh.secret_ref.clone());
        assert_eq!(
            s.validate().unwrap_err().reasons(),
            vec![REASON_SHARED_CREDENTIAL]
        );
        let ssh = s.ssh.as_mut().unwrap();
        ssh.placement_secret_ref = Some(SecretName {
            name: "hv01-ssh-placement".to_string(),
        });
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn the_host_script_path_is_under_opt_faubourg() {
        assert!(HOST_SCRIPT_PATH.starts_with("/opt/faubourg/"));
    }
}
