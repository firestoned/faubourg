// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! The controller's configuration (ADR-0009 decision 11): flags, each with an environment
//! variable, validated before the controller starts. A value it cannot use stops it: fail
//! closed, never a permissive default.

use clap::Parser;
use faubourg_api::host::{DEFAULT_SCRIPT_KEY, ScriptRef, ScriptSourceKind};
use faubourg_api::validation::{is_dns_label, is_dns_subdomain};

use crate::job::{BootstrapConfig, is_digest_pinned};

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
}

/// A validated configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// How bootstrap Jobs are built.
    pub bootstrap: BootstrapConfig,
    /// banlieue's namespace.
    pub provider_namespace: String,
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
            },
            provider_namespace: self.provider_namespace,
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
        let mut v = vec!["faubourg-controller", "--bootstrap-image", IMAGE];
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
        assert_eq!(c.provider_namespace, "banlieue-system");
    }

    #[test]
    fn the_image_is_required_and_must_be_pinned() {
        assert!(Args::try_parse_from(["faubourg-controller"]).is_err());
        for image in [
            "ghcr.io/firestoned/faubourg-bootstrap:latest",
            "ghcr.io/firestoned/faubourg-bootstrap@sha256:abc",
            "",
        ] {
            let a = Args::try_parse_from(["faubourg-controller", "--bootstrap-image", image]);
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
}
