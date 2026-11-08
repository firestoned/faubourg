// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! The controller's configuration (ADR-0009 decision 11): flags, each with an environment
//! variable, validated before the controller starts. A value it cannot use stops it: fail
//! closed, never a permissive default.

use clap::Parser;
use faubourg_api::host::{DEFAULT_SCRIPT_KEY, ScriptRef, ScriptSourceKind};
use faubourg_api::validation::{is_dns_label, is_dns_subdomain};

use crate::job::{BootstrapConfig, ProviderConfig, is_digest_pinned};

/// Flags, as the command line and the environment give them.
#[derive(Clone, Debug, Parser)]
#[command(
    name = "faubourg-controller",
    version,
    about = "faubourg's Host controller"
)]
pub struct Args {
    /// The credentials namespace: the SSH Secrets, the script sources and the bootstrap Jobs.
    #[arg(
        long,
        env = "FAUBOURG_CREDENTIALS_NAMESPACE",
        default_value = "faubourg-credentials"
    )]
    pub credentials_namespace: String,
    /// The bootstrap image, pinned by digest: the one the admission policy's parameters name.
    #[arg(long, env = "FAUBOURG_BOOTSTRAP_IMAGE")]
    pub bootstrap_image: String,
    /// The bootstrap pods' `ServiceAccount`, which holds no RBAC.
    #[arg(
        long,
        env = "FAUBOURG_BOOTSTRAP_SERVICE_ACCOUNT",
        default_value = "faubourg-bootstrap"
    )]
    pub bootstrap_service_account: String,
    /// The script source for a Host with no `bootstrap.scriptRef`: `ConfigMap/<name>` or
    /// `Secret/<name>`, optionally `:<key>` (default `install.sh`).
    #[arg(long, env = "FAUBOURG_DEFAULT_SCRIPT")]
    pub default_script: Option<String>,
    /// banlieue's namespace, where the hosts' Providers live.
    #[arg(
        long,
        env = "FAUBOURG_PROVIDER_NAMESPACE",
        default_value = "banlieue-system"
    )]
    pub provider_namespace: String,
    /// The `ProviderClass` every host's Provider references. It must be `External`.
    #[arg(
        long,
        env = "FAUBOURG_PROVIDER_CLASS",
        default_value = "cloud-hypervisor"
    )]
    pub provider_class: String,
    /// The API server address the hosts' providers use, `https://...` (ADR-0011 decision 11).
    #[arg(long, env = "FAUBOURG_PROVIDER_API_SERVER")]
    pub provider_api_server: String,
    /// The namespace of the bootstrap policy's parameters, which pin each script (ADR-0013).
    #[arg(
        long,
        env = "FAUBOURG_POLICY_NAMESPACE",
        default_value = "faubourg-system"
    )]
    pub policy_namespace: String,
    /// The bootstrap policy's parameters: the one `ConfigMap` the controller reads.
    #[arg(
        long,
        env = "FAUBOURG_POLICY_NAME",
        default_value = "faubourg-bootstrap-policy"
    )]
    pub policy_name: String,
    /// At most this many re-bootstrap Jobs run at once, across the cluster (ADR-0013 decision 5).
    #[arg(long, env = "FAUBOURG_MAX_REBOOTSTRAPS", default_value_t = 1)]
    pub max_rebootstraps: u32,
}

/// A validated configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// How bootstrap Jobs are built, and where the hosts' Providers live.
    pub bootstrap: BootstrapConfig,
    /// Where the bootstrap policy's parameters are.
    pub policy: PolicyRef,
}

/// The bootstrap policy's parameters `ConfigMap` (A-8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyRef {
    /// Its namespace.
    pub namespace: String,
    /// Its name.
    pub name: String,
}

/// Why the controller refuses to start.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// The bootstrap image is not `repo@sha256:<64 hex>`.
    #[error("--bootstrap-image must be pinned by digest (repo@sha256:<64 hex>)")]
    ImageNotPinned,
    /// A namespace flag is not a DNS label.
    #[error("--{0} is not a valid namespace name")]
    InvalidNamespace(&'static str),
    /// The bootstrap `ServiceAccount` name is not a valid object name.
    #[error("--bootstrap-service-account is not a valid name")]
    InvalidServiceAccount,
    /// The `ProviderClass` name is not a valid object name.
    #[error("--provider-class is not a valid name")]
    InvalidProviderClass,
    /// The API server address is not an `https://` URL with a host.
    #[error("--provider-api-server must be an https:// URL")]
    InvalidApiServer,
    /// The policy parameters' name is not a valid object name.
    #[error("--policy-name is not a valid name")]
    InvalidPolicyName,
    /// The rollout limit is zero.
    #[error("--max-rebootstraps must be at least 1")]
    InvalidMaxRebootstraps,
    /// The default script is not `ConfigMap/<name>[:<key>]` or `Secret/<name>[:<key>]`.
    #[error("--default-script must be ConfigMap/<name>[:<key>] or Secret/<name>[:<key>]")]
    InvalidDefaultScript,
}

impl Args {
    /// Validates the flags.
    pub fn validate(self) -> Result<Config, ConfigError> {
        if !is_digest_pinned(&self.bootstrap_image) {
            return Err(ConfigError::ImageNotPinned);
        }
        if !is_dns_label(&self.credentials_namespace) {
            return Err(ConfigError::InvalidNamespace("credentials-namespace"));
        }
        if !is_dns_label(&self.provider_namespace) {
            return Err(ConfigError::InvalidNamespace("provider-namespace"));
        }
        if !is_dns_subdomain(&self.bootstrap_service_account) {
            return Err(ConfigError::InvalidServiceAccount);
        }
        if !is_dns_subdomain(&self.provider_class) {
            return Err(ConfigError::InvalidProviderClass);
        }
        if !is_https_url(&self.provider_api_server) {
            return Err(ConfigError::InvalidApiServer);
        }
        if !is_dns_label(&self.policy_namespace) {
            return Err(ConfigError::InvalidNamespace("policy-namespace"));
        }
        if !is_dns_subdomain(&self.policy_name) {
            return Err(ConfigError::InvalidPolicyName);
        }
        if self.max_rebootstraps == 0 {
            return Err(ConfigError::InvalidMaxRebootstraps);
        }
        let default_script = self
            .default_script
            .as_deref()
            .map(parse_script_ref)
            .transpose()?;
        Ok(Config {
            bootstrap: BootstrapConfig {
                namespace: self.credentials_namespace,
                image: self.bootstrap_image,
                service_account: self.bootstrap_service_account,
                default_script,
                provider: ProviderConfig {
                    class: self.provider_class,
                    namespace: self.provider_namespace,
                    api_server: self.provider_api_server,
                },
                max_rebootstraps: self.max_rebootstraps,
            },
            policy: PolicyRef {
                namespace: self.policy_namespace,
                name: self.policy_name,
            },
        })
    }
}

/// Parses `ConfigMap/<name>[:<key>]` or `Secret/<name>[:<key>]`.
pub fn parse_script_ref(s: &str) -> Result<ScriptRef, ConfigError> {
    let (kind, rest) = s.split_once('/').ok_or(ConfigError::InvalidDefaultScript)?;
    let kind = match kind {
        "ConfigMap" => ScriptSourceKind::ConfigMap,
        "Secret" => ScriptSourceKind::Secret,
        _ => return Err(ConfigError::InvalidDefaultScript),
    };
    let (name, key) = rest.split_once(':').unwrap_or((rest, DEFAULT_SCRIPT_KEY));
    if !is_dns_subdomain(name) || !is_plain_key(key) {
        return Err(ConfigError::InvalidDefaultScript);
    }
    Ok(ScriptRef {
        kind,
        name: name.into(),
        key: key.into(),
    })
}

/// `https://<host>[:<port>][/<path>]`, with no whitespace, userinfo, query or fragment.
fn is_https_url(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or_default();
    !authority.is_empty()
        && !authority.starts_with(':')
        && !s.contains(['@', '?', '#'])
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// A `ConfigMap` or `Secret` key that is also a plain file name.
fn is_plain_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 253
        && k != "."
        && k != ".."
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMAGE: &str = "ghcr.io/firestoned/faubourg-bootstrap@sha256:\
                         0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn args(extra: &[&str]) -> Result<Args, clap::Error> {
        let mut v = vec![
            "faubourg-controller",
            "--bootstrap-image",
            IMAGE,
            "--provider-api-server",
            "https://kube.home.example:6443",
        ];
        v.extend_from_slice(extra);
        Args::try_parse_from(v)
    }

    #[test]
    fn the_defaults_are_the_shipped_manifests() {
        let c = args(&[]).unwrap().validate().unwrap();
        assert_eq!(c.bootstrap.namespace, "faubourg-credentials");
        assert_eq!(c.bootstrap.service_account, "faubourg-bootstrap");
        assert_eq!(c.bootstrap.image, IMAGE);
        assert_eq!(c.bootstrap.default_script, None);
        assert_eq!(c.bootstrap.provider.namespace, "banlieue-system");
        assert_eq!(c.bootstrap.provider.class, "cloud-hypervisor");
        assert_eq!(
            c.bootstrap.provider.api_server,
            "https://kube.home.example:6443"
        );
        assert_eq!(c.policy.namespace, "faubourg-system");
        assert_eq!(c.policy.name, "faubourg-bootstrap-policy");
        assert_eq!(c.bootstrap.max_rebootstraps, 1);
    }

    #[test]
    fn the_policy_and_the_rollout_limit_are_checked() {
        assert_eq!(
            args(&["--policy-namespace", "Not_A_Label"])
                .unwrap()
                .validate(),
            Err(ConfigError::InvalidNamespace("policy-namespace"))
        );
        assert_eq!(
            args(&["--policy-name", "Not_Valid"]).unwrap().validate(),
            Err(ConfigError::InvalidPolicyName)
        );
        assert_eq!(
            args(&["--max-rebootstraps", "0"]).unwrap().validate(),
            Err(ConfigError::InvalidMaxRebootstraps),
            "zero would stop every re-bootstrap for good"
        );
        let c = args(&["--max-rebootstraps", "3"])
            .unwrap()
            .validate()
            .unwrap();
        assert_eq!(c.bootstrap.max_rebootstraps, 3);
    }

    #[test]
    fn the_image_is_required_and_must_be_pinned() {
        assert!(Args::try_parse_from(["faubourg-controller"]).is_err());
        for image in [
            "ghcr.io/firestoned/faubourg-bootstrap:latest",
            "ghcr.io/firestoned/faubourg-bootstrap@sha256:abc",
            "",
        ] {
            let a = Args::try_parse_from([
                "faubourg-controller",
                "--bootstrap-image",
                image,
                "--provider-api-server",
                "https://kube.home.example:6443",
            ]);
            assert_eq!(
                a.unwrap().validate(),
                Err(ConfigError::ImageNotPinned),
                "{image}"
            );
        }
    }

    #[test]
    fn names_are_validated() {
        assert_eq!(
            args(&["--credentials-namespace", "Not_A_Namespace"])
                .unwrap()
                .validate(),
            Err(ConfigError::InvalidNamespace("credentials-namespace"))
        );
        assert_eq!(
            args(&["--provider-namespace", "a.b"]).unwrap().validate(),
            Err(ConfigError::InvalidNamespace("provider-namespace"))
        );
        assert_eq!(
            args(&["--bootstrap-service-account=-x"])
                .unwrap()
                .validate(),
            Err(ConfigError::InvalidServiceAccount)
        );
    }

    #[test]
    fn the_default_script_parses() {
        assert_eq!(
            parse_script_ref("ConfigMap/faubourg-install"),
            Ok(ScriptRef {
                kind: ScriptSourceKind::ConfigMap,
                name: "faubourg-install".into(),
                key: DEFAULT_SCRIPT_KEY.into(),
            })
        );
        assert_eq!(
            parse_script_ref("Secret/install:run.sh"),
            Ok(ScriptRef {
                kind: ScriptSourceKind::Secret,
                name: "install".into(),
                key: "run.sh".into(),
            })
        );
        let c = args(&["--default-script", "ConfigMap/faubourg-install"])
            .unwrap()
            .validate()
            .unwrap();
        assert!(c.bootstrap.default_script.is_some());
    }

    #[test]
    fn a_malformed_default_script_is_refused() {
        for s in [
            "faubourg-install",
            "Pod/x",
            "configmap/x",
            "ConfigMap/",
            "ConfigMap/Bad_Name",
            "ConfigMap/x:",
            "ConfigMap/x:../etc",
            "ConfigMap/x:..",
            "ConfigMap/x:a/b",
        ] {
            assert_eq!(
                parse_script_ref(s),
                Err(ConfigError::InvalidDefaultScript),
                "{s}"
            );
        }
    }

    fn with_api_server(url: &str) -> Args {
        Args::try_parse_from([
            "faubourg-controller",
            "--bootstrap-image",
            IMAGE,
            "--provider-api-server",
            url,
        ])
        .unwrap()
    }

    #[test]
    fn the_api_server_is_required_and_must_be_https() {
        assert!(Args::try_parse_from(["faubourg-controller", "--bootstrap-image", IMAGE]).is_err());
        for url in [
            "http://kube.home.example:6443",
            "https://",
            "https://:6443",
            "https://octocat@kube.home.example",
            "https://kube.home.example/?x=1",
            "https://kube home.example",
            "kube.home.example:6443",
        ] {
            assert_eq!(
                with_api_server(url).validate(),
                Err(ConfigError::InvalidApiServer),
                "{url}"
            );
        }
        assert!(
            with_api_server("https://192.0.2.10:6443")
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn the_provider_class_is_validated() {
        assert_eq!(
            args(&["--provider-class", "Cloud_Hypervisor"])
                .unwrap()
                .validate(),
            Err(ConfigError::InvalidProviderClass)
        );
    }
}
