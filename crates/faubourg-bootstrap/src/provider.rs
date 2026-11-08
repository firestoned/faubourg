// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! The host's first provider token (ADR-0011 decisions 6 and 9).
//!
//! The bootstrap pod authenticates with its own short-lived projected token (the `kube-api`
//! volume) and requests, through the `TokenRequest` API, a day's token for the Provider's
//! `ServiceAccount`. The token goes to the host on the install script's stdin, in a small
//! text bundle with a kubeconfig that reads it from a file. It is never logged, never written
//! to disk here, and never seen by the controller.

use std::time::Duration;

use faubourg_api::bootstrap::{BUNDLE_HEADER, HOST_TOKEN_PATH, PROVIDER_TOKEN_SECONDS};
use k8s_openapi::api::authentication::v1::{TokenRequest, TokenRequestSpec};
use k8s_openapi::api::core::v1::ServiceAccount;
use kube::api::{Api, PostParams};
use kube::config::{
    AuthInfo, Cluster, Context, KubeConfigOptions, Kubeconfig, NamedAuthInfo, NamedCluster,
    NamedContext,
};

/// How long to wait for banlieue's operator to create the `ServiceAccount`.
const SERVICE_ACCOUNT_WAIT: Duration = Duration::from_secs(120);
const RETRY: Duration = Duration::from_secs(5);

/// Where the host's Provider is, and who it authenticates as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The Provider's name.
    pub provider: String,
    /// Its namespace.
    pub namespace: String,
    /// Its `ServiceAccount`.
    pub service_account: String,
    /// The API server address the host uses.
    pub api_server: String,
}

/// The pod's own way to the API server: its projected token and the cluster CA, as files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PodAccess {
    /// `https://<host>:<port>` of the API server, from inside the cluster.
    pub server: String,
    /// The projected token's path.
    pub token_file: String,
    /// The cluster CA's path.
    pub ca_file: String,
}

/// A minted token. `Debug` never prints it.
pub struct Minted {
    /// The token itself.
    pub token: String,
    /// When it expires, as the API server reports it.
    pub expires: Option<String>,
}

impl std::fmt::Debug for Minted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Minted")
            .field("token", &"<redacted>")
            .field("expires", &self.expires)
            .finish()
    }
}

/// Why no token was minted. Never carries the token.
#[derive(Debug, thiserror::Error)]
pub enum MintError {
    /// The pod's own API access could not be set up.
    #[error("the pod's API access: {0}")]
    Access(String),
    /// The API server refused, or the `ServiceAccount` never appeared.
    #[error("TokenRequest for {0}: {1}")]
    Request(String, String),
}

/// Requests the host's token for `target`'s `ServiceAccount`, waiting up to two minutes for
/// banlieue's operator to create it.
pub async fn mint(access: &PodAccess, target: &Target) -> Result<Minted, MintError> {
    let config =
        kube::Config::from_custom_kubeconfig(pod_kubeconfig(access), &KubeConfigOptions::default())
            .await
            .map_err(|e| MintError::Access(e.to_string()))?;
    let client = kube::Client::try_from(config).map_err(|e| MintError::Access(e.to_string()))?;
    let accounts: Api<ServiceAccount> = Api::namespaced(client, &target.namespace);
    let req = TokenRequest {
        spec: Some(TokenRequestSpec {
            expiration_seconds: Some(PROVIDER_TOKEN_SECONDS),
            ..TokenRequestSpec::default()
        }),
        ..TokenRequest::default()
    };
    let deadline = tokio::time::Instant::now() + SERVICE_ACCOUNT_WAIT;
    loop {
        match accounts
            .create_token_request(&target.service_account, &PostParams::default(), &req)
            .await
        {
            Ok(r) => {
                let status = r.status.unwrap_or_default();
                let token = status.token.filter(|t| !t.is_empty()).ok_or_else(|| {
                    MintError::Request(target.service_account.clone(), "no token".into())
                })?;
                return Ok(Minted {
                    token,
                    expires: status.expiration_timestamp.map(|t| t.0.to_string()),
                });
            }
            Err(kube::Error::Api(s)) if s.code == 404 && tokio::time::Instant::now() < deadline => {
                tracing::info!(
                    service_account = %target.service_account,
                    "waiting for banlieue to create the provider's ServiceAccount"
                );
                tokio::time::sleep(RETRY).await;
            }
            Err(e) => {
                return Err(MintError::Request(
                    target.service_account.clone(),
                    e.to_string(),
                ));
            }
        }
    }
}

fn pod_kubeconfig(a: &PodAccess) -> Kubeconfig {
    Kubeconfig {
        clusters: vec![NamedCluster {
            name: "in-cluster".into(),
            cluster: Some(Cluster {
                server: Some(a.server.clone()),
                certificate_authority: Some(a.ca_file.clone()),
                ..Cluster::default()
            }),
            ..NamedCluster::default()
        }],
        auth_infos: vec![NamedAuthInfo {
            name: "bootstrap".into(),
            auth_info: Some(AuthInfo {
                token_file: Some(a.token_file.clone()),
                ..AuthInfo::default()
            }),
            ..NamedAuthInfo::default()
        }],
        contexts: vec![NamedContext {
            name: "bootstrap".into(),
            context: Some(Context {
                cluster: "in-cluster".into(),
                user: Some("bootstrap".into()),
                ..Context::default()
            }),
            ..NamedContext::default()
        }],
        current_context: Some("bootstrap".into()),
        ..Kubeconfig::default()
    }
}

/// The host's kubeconfig: the API server, the cluster CA, and a user reading its token from
/// [`HOST_TOKEN_PATH`]. It holds no secret.
pub fn host_kubeconfig(api_server: &str, ca_pem: &[u8], namespace: &str) -> String {
    let ca = base64(ca_pem);
    format!(
        "apiVersion: v1\n\
         kind: Config\n\
         clusters:\n\
         - name: banlieue\n  cluster:\n    server: {api_server}\n    certificate-authority-data: {ca}\n\
         users:\n\
         - name: provider\n  user:\n    tokenFile: {HOST_TOKEN_PATH}\n\
         contexts:\n\
         - name: provider\n  context:\n    cluster: banlieue\n    user: provider\n    namespace: {namespace}\n\
         current-context: provider\n"
    )
}

/// The bundle on the install script's stdin (ADR-0011 decision 9).
pub fn bundle(target: &Target, token: &str, kubeconfig: &str) -> Vec<u8> {
    format!(
        "{BUNDLE_HEADER}\nprovider {}\nnamespace {}\ntoken {token}\nkubeconfig\n{kubeconfig}",
        target.provider, target.namespace
    )
    .into_bytes()
}

/// Standard base64, padded, as kubeconfig's `*-data` fields expect.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> shift) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Target {
        Target {
            provider: "hv01".into(),
            namespace: "banlieue-system".into(),
            service_account: "banlieue-provider-cloud-hypervisor-hv01".into(),
            api_server: "https://kube.home.example:6443".into(),
        }
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xff, 0xfe, 0xfd, 0x00]), "//79AA==");
    }

    #[test]
    fn the_host_kubeconfig_reads_its_token_from_a_file_and_holds_no_secret() {
        let k = host_kubeconfig(
            "https://kube.home.example:6443",
            b"-----BEGIN CERTIFICATE-----\n",
            "banlieue-system",
        );
        assert!(
            k.contains("server: https://kube.home.example:6443\n"),
            "{k}"
        );
        assert!(
            k.contains("tokenFile: /etc/banlieue/credentials/token\n"),
            "{k}"
        );
        assert!(k.contains("namespace: banlieue-system\n"), "{k}");
        assert!(
            k.contains("certificate-authority-data: LS0tLS1CRUdJTiBDRVJUSUZJQ0FURS0tLS0tCg==\n")
        );
        assert!(!k.contains("token:"), "no inline token");
    }

    #[test]
    fn the_bundle_is_the_documented_format() {
        let b = String::from_utf8(bundle(&target(), "eyJ.x.y", "apiVersion: v1\n")).unwrap();
        assert_eq!(
            b,
            "faubourg-provider-credential v1\nprovider hv01\nnamespace banlieue-system\n\
             token eyJ.x.y\nkubeconfig\napiVersion: v1\n"
        );
    }

    #[test]
    fn a_minted_token_is_never_printed() {
        let m = Minted {
            token: "eyJ.secret.part".into(),
            expires: None,
        };
        assert!(!format!("{m:?}").contains("secret"));
    }

    #[test]
    fn the_pods_kubeconfig_uses_its_projected_files() {
        let k = pod_kubeconfig(&PodAccess {
            server: "https://192.0.2.1:443".into(),
            token_file: "/var/run/faubourg/kube-api/token".into(),
            ca_file: "/var/run/faubourg/kube-api/ca.crt".into(),
        });
        let user = k.auth_infos[0].auth_info.as_ref().unwrap();
        assert_eq!(
            user.token_file.as_deref(),
            Some("/var/run/faubourg/kube-api/token")
        );
        assert!(user.token.is_none());
        let cluster = k.clusters[0].cluster.as_ref().unwrap();
        assert_eq!(
            cluster.certificate_authority.as_deref(),
            Some("/var/run/faubourg/kube-api/ca.crt")
        );
    }
}
