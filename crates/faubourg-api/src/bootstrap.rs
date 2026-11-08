// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! The contract between the bootstrap Job the controller builds (ADR-0008) and the bootstrap
//! image that runs in it (ADR-0010): container and volume names, mount paths, environment
//! variables, exit codes and the termination message. Both sides use these constants, so they
//! cannot disagree.

use serde::{Deserialize, Serialize};

/// Name of the Job's one container.
pub const CONTAINER: &str = "bootstrap";
/// Volume holding the scoped run credential.
pub const VOLUME_CREDENTIALS: &str = "credentials";
/// Volume holding the optional placement credential.
pub const VOLUME_PLACEMENT: &str = "placement";
/// Volume holding the install script.
pub const VOLUME_SCRIPT: &str = "script";
/// Where the run credential is mounted.
pub const MOUNT_CREDENTIALS: &str = "/var/run/faubourg/credentials";
/// Where the placement credential is mounted.
pub const MOUNT_PLACEMENT: &str = "/var/run/faubourg/placement";
/// Where the install script is mounted.
pub const MOUNT_SCRIPT: &str = "/var/run/faubourg/script";
/// The script's file name inside [`MOUNT_SCRIPT`], whatever its key in the source.
pub const SCRIPT_FILE: &str = "install.sh";
/// Volume holding the bootstrap pod's own short-lived API token and the cluster CA
/// (ADR-0011 decision 6).
pub const VOLUME_KUBE_API: &str = "kube-api";
/// Where [`VOLUME_KUBE_API`] is mounted.
pub const MOUNT_KUBE_API: &str = "/var/run/faubourg/kube-api";
/// The token's file name inside [`MOUNT_KUBE_API`].
pub const KUBE_API_TOKEN_FILE: &str = "token";
/// The cluster CA's file name inside [`MOUNT_KUBE_API`].
pub const KUBE_API_CA_FILE: &str = "ca.crt";
/// The `ConfigMap` the kubelet publishes the cluster CA in, in every namespace.
pub const KUBE_ROOT_CA_CONFIGMAP: &str = "kube-root-ca.crt";
/// Lifetime of the bootstrap pod's own token: the API server's minimum.
pub const KUBE_API_TOKEN_SECONDS: i64 = 600;
/// Lifetime of the provider token the bootstrap mints for the host: banlieue's default
/// (banlieue ADR-0060 decision 5). The admission policy refuses longer.
pub const PROVIDER_TOKEN_SECONDS: i64 = 86_400;

/// Env: the Host's name, for logs.
pub const ENV_HOST: &str = "FAUBOURG_HOST";
/// Env: the address to reach over SSH.
pub const ENV_ADDRESS: &str = "FAUBOURG_ADDRESS";
/// Env: the SSH port.
pub const ENV_PORT: &str = "FAUBOURG_PORT";
/// Env: `Strict` or `TrustOnFirstUse`. A Secret with `knownHosts` is always strict
/// (ADR-0008 decision 5).
pub const ENV_HOST_KEY_POLICY: &str = "FAUBOURG_HOST_KEY_POLICY";
/// Env: the fingerprint pinned by an earlier `TrustOnFirstUse` run, when there is one.
pub const ENV_ACCEPTED_HOST_KEY: &str = "FAUBOURG_ACCEPTED_HOST_KEY";
/// Env: `true` to remove the placement key from the host once placement succeeds.
pub const ENV_REVOKE_PLACEMENT: &str = "FAUBOURG_REVOKE_PLACEMENT";
/// Env: the host's Provider (ADR-0011 decision 1).
pub const ENV_PROVIDER: &str = "FAUBOURG_PROVIDER";
/// Env: the Provider's namespace.
pub const ENV_PROVIDER_NAMESPACE: &str = "FAUBOURG_PROVIDER_NAMESPACE";
/// Env: the Provider's `ServiceAccount`, which the bootstrap mints the host's token for.
pub const ENV_PROVIDER_SERVICE_ACCOUNT: &str = "FAUBOURG_PROVIDER_SERVICE_ACCOUNT";
/// Env: the API server address the host's provider uses (`https://...`).
pub const ENV_PROVIDER_API_SERVER: &str = "FAUBOURG_PROVIDER_API_SERVER";

/// First line of the credential bundle on the install script's stdin (ADR-0011 decision 9).
pub const BUNDLE_HEADER: &str = "faubourg-provider-credential v1";
/// Where the host's provider reads its token (banlieue's path).
pub const HOST_TOKEN_PATH: &str = "/etc/banlieue/credentials/token";

/// Exit code: anything that is not drift or a refused host key, from a malformed input to a
/// failing install script. The Job's `backoffLimit` retries it.
pub const EXIT_FAILED: i32 = 1;
/// Exit code: the on-host script differs from the source and there is no placement
/// credential (ADR-0005 decision 4). Fails the Job at once, without retries.
pub const EXIT_SCRIPT_DRIFT: i32 = 64;
/// Exit code: the host key was refused. Fails the Job at once: a retry cannot fix it.
pub const EXIT_HOST_KEY_REFUSED: i32 = 65;

/// The bootstrap container's termination message (ADR-0010 decision 7), as JSON.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// The host key's fingerprint, `SHA256:<base64>`, when a key was seen and accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_key: Option<String>,
}

/// True for an OpenSSH SHA256 fingerprint: `SHA256:` and 43 characters of unpadded base64.
/// Anything else read back from a pod is refused, so a pod cannot pin junk into a Host.
pub fn is_fingerprint(s: &str) -> bool {
    s.strip_prefix("SHA256:").is_some_and(|b| {
        b.len() == 43
            && b.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fingerprint_is_sha256_and_unpadded_base64() {
        assert!(is_fingerprint(
            "SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s"
        ));
        for bad in [
            "",
            "SHA256:",
            "MD5:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s",
            "SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s=",
            "SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2",
            "SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3t\n2s",
        ] {
            assert!(!is_fingerprint(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_report_is_small_json() {
        let r = Report {
            host_key: Some("SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s".into()),
        };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"hostKey":"SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s"}"#
        );
        assert_eq!(serde_json::to_string(&Report::default()).unwrap(), "{}");
    }
}
