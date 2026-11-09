// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! What the bootstrap reads before it connects anywhere (ADR-0010 decision 3): the Job's
//! literal environment and its mounted volumes. Anything missing or malformed is refused here,
//! before a connection is opened. Errors name the input, never its value.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use faubourg_api::bootstrap::{
    ENV_ACCEPTED_HOST_KEY, ENV_ADDRESS, ENV_HOST, ENV_HOST_KEY_POLICY, ENV_PORT, ENV_PROVIDER,
    ENV_PROVIDER_API_SERVER, ENV_PROVIDER_CA_SHA256, ENV_PROVIDER_NAMESPACE,
    ENV_PROVIDER_SERVICE_ACCOUNT, ENV_REVOKE_PLACEMENT, ENV_SCRIPT_SHA256, KUBE_API_CA_FILE,
    KUBE_API_TOKEN_FILE, MOUNT_CREDENTIALS, MOUNT_KUBE_API, MOUNT_PLACEMENT, MOUNT_PROVIDER_CA,
    MOUNT_SCRIPT, PROVIDER_CA_FILE, SCRIPT_FILE, is_fingerprint,
};
use faubourg_api::validation::{is_dns_label, is_dns_subdomain};

use crate::provider::{PodAccess, Target};
use faubourg_api::credentials::{
    CredentialError, KEY_CERTIFICATE, KEY_KNOWN_HOSTS, KEY_PASSPHRASE, KEY_PASSWORD,
    KEY_PRIVATE_KEY, KEY_USERNAME, SshCredentials,
};
use faubourg_api::host::HostKeyPolicy;

/// The keys of a credential Secret that are read. Nothing else in the volume is opened.
const SECRET_KEYS: [&str; 6] = [
    KEY_USERNAME,
    KEY_PRIVATE_KEY,
    KEY_PASSPHRASE,
    KEY_CERTIFICATE,
    KEY_PASSWORD,
    KEY_KNOWN_HOSTS,
];

/// Everything one bootstrap run needs. `Debug` prints names and sizes, never the script or a
/// credential.
#[derive(Clone)]
pub struct Config {
    /// The Host's name, for logs.
    pub host: String,
    /// Where to connect.
    pub address: String,
    /// SSH port.
    pub port: u16,
    /// The host key policy the Host asks for. A run credential with `knownHosts` overrides
    /// it (see [`crate::hostkey::Verifier::new`]).
    pub policy: HostKeyPolicy,
    /// The fingerprint pinned by an earlier first-use run.
    pub accepted_host_key: Option<String>,
    /// Remove the placement key once placement succeeds.
    pub revoke_placement: bool,
    /// The scoped run credential.
    pub run: SshCredentials,
    /// The privileged placement credential, when mounted.
    pub placement: Option<SshCredentials>,
    /// The install script's content.
    pub script: Vec<u8>,
    /// The host's Provider and the pod's own API access, when the Job carries them
    /// (ADR-0011). Without them no provider token is minted.
    pub provider: Option<ProviderInputs>,
}

/// What minting the host's provider token needs.
#[derive(Clone, Debug)]
pub struct ProviderInputs {
    /// The Provider and its `ServiceAccount`.
    pub target: Target,
    /// The pod's own token and CA, as files.
    pub access: PodAccess,
    /// What the host trusts for the API server, written into its kubeconfig: the bundle the
    /// bootstrap policy pins (ADR-0016), or the cluster CA when it pins none.
    pub ca_pem: Vec<u8>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("policy", &self.policy)
            .field("accepted_host_key", &self.accepted_host_key)
            .field("revoke_placement", &self.revoke_placement)
            .field("run_user", &self.run.username)
            .field(
                "placement_user",
                &self.placement.as_ref().map(|p| &p.username),
            )
            .field("script_bytes", &self.script.len())
            .field(
                "provider",
                &self.provider.as_ref().map(|p| &p.target.provider),
            )
            .finish_non_exhaustive()
    }
}

/// Why the bootstrap refused to start. Names the input, never its value.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// An environment variable is missing or empty.
    #[error("{0} is not set")]
    MissingEnv(&'static str),
    /// An environment variable has a value the bootstrap does not accept.
    #[error("{0} is not valid")]
    InvalidEnv(&'static str),
    /// A credential Secret is refused (ADR-0006).
    #[error("the {volume} credential: {source}")]
    Credential {
        /// `credentials` or `placement`.
        volume: &'static str,
        /// Why.
        source: CredentialError,
    },
    /// A mounted file could not be read.
    #[error("cannot read {0}")]
    Unreadable(PathBuf),
    /// The install script is empty.
    #[error("the install script is empty")]
    EmptyScript,
    /// The mounted install script is not the one the bootstrap policy pins (ADR-0013).
    #[error("the mounted install script is not the one pinned in the bootstrap policy")]
    ScriptUnpinned,
    /// The mounted provider CA is not the one the bootstrap policy pins, or holds no
    /// certificate (ADR-0016).
    #[error("the mounted provider CA is not the one pinned in the bootstrap policy")]
    ProviderCaUnpinned,
}

impl Config {
    /// Reads the environment through `env` and the mounts under `root` (`/` in the image).
    pub fn load(env: &dyn Fn(&str) -> Option<String>, root: &Path) -> Result<Self, ConfigError> {
        let get = |k: &'static str| env(k).filter(|v| !v.is_empty());
        let need = |k: &'static str| get(k).ok_or(ConfigError::MissingEnv(k));

        let host = need(ENV_HOST)?;
        let address = need(ENV_ADDRESS)?;
        if address.starts_with('-') || address.chars().any(char::is_whitespace) {
            return Err(ConfigError::InvalidEnv(ENV_ADDRESS));
        }
        let port = need(ENV_PORT)?
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or(ConfigError::InvalidEnv(ENV_PORT))?;
        let policy = match need(ENV_HOST_KEY_POLICY)?.as_str() {
            "Strict" => HostKeyPolicy::Strict,
            "TrustOnFirstUse" => HostKeyPolicy::TrustOnFirstUse,
            _ => return Err(ConfigError::InvalidEnv(ENV_HOST_KEY_POLICY)),
        };
        let accepted_host_key = get(ENV_ACCEPTED_HOST_KEY);
        if accepted_host_key
            .as_deref()
            .is_some_and(|f| !is_fingerprint(f))
        {
            return Err(ConfigError::InvalidEnv(ENV_ACCEPTED_HOST_KEY));
        }
        let revoke_placement = match get(ENV_REVOKE_PLACEMENT).as_deref() {
            None | Some("false") => false,
            Some("true") => true,
            Some(_) => return Err(ConfigError::InvalidEnv(ENV_REVOKE_PLACEMENT)),
        };

        let run =
            credential(root, MOUNT_CREDENTIALS, "credentials")?.ok_or(ConfigError::Credential {
                volume: "credentials",
                source: CredentialError::Missing(KEY_USERNAME),
            })?;
        let placement = credential(root, MOUNT_PLACEMENT, "placement")?;
        let script_path = under(root, MOUNT_SCRIPT).join(SCRIPT_FILE);
        let script =
            std::fs::read(&script_path).map_err(|_| ConfigError::Unreadable(script_path))?;
        if script.is_empty() {
            return Err(ConfigError::EmptyScript);
        }
        let pinned = need(ENV_SCRIPT_SHA256)?;
        if pinned.len() != 64
            || !pinned
                .bytes()
                .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(ConfigError::InvalidEnv(ENV_SCRIPT_SHA256));
        }
        if crate::sha256_hex(&script) != pinned {
            return Err(ConfigError::ScriptUnpinned);
        }
        let provider = provider_inputs(&get, env, root)?;
        Ok(Self {
            host,
            address,
            port,
            policy,
            accepted_host_key,
            revoke_placement,
            run,
            placement,
            script,
            provider,
        })
    }
}

/// The provider inputs: all four variables and the `kube-api` volume, or none of them.
fn provider_inputs(
    get: &dyn Fn(&'static str) -> Option<String>,
    env: &dyn Fn(&str) -> Option<String>,
    root: &Path,
) -> Result<Option<ProviderInputs>, ConfigError> {
    let vars = [
        ENV_PROVIDER,
        ENV_PROVIDER_NAMESPACE,
        ENV_PROVIDER_SERVICE_ACCOUNT,
        ENV_PROVIDER_API_SERVER,
    ];
    if vars.iter().all(|k| get(k).is_none()) {
        return Ok(None);
    }
    let need = |k: &'static str| get(k).ok_or(ConfigError::MissingEnv(k));
    let provider = need(ENV_PROVIDER)?;
    let namespace = need(ENV_PROVIDER_NAMESPACE)?;
    let service_account = need(ENV_PROVIDER_SERVICE_ACCOUNT)?;
    let api_server = need(ENV_PROVIDER_API_SERVER)?;
    if !is_dns_subdomain(&provider) {
        return Err(ConfigError::InvalidEnv(ENV_PROVIDER));
    }
    if !is_dns_label(&namespace) {
        return Err(ConfigError::InvalidEnv(ENV_PROVIDER_NAMESPACE));
    }
    if !is_dns_subdomain(&service_account) {
        return Err(ConfigError::InvalidEnv(ENV_PROVIDER_SERVICE_ACCOUNT));
    }
    if !api_server.starts_with("https://")
        || api_server
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(ConfigError::InvalidEnv(ENV_PROVIDER_API_SERVER));
    }
    let dir = under(root, MOUNT_KUBE_API);
    let token_file = dir.join(KUBE_API_TOKEN_FILE);
    if !token_file.is_file() {
        return Err(ConfigError::Unreadable(token_file));
    }
    let ca_file = dir.join(KUBE_API_CA_FILE);
    let cluster_ca =
        std::fs::read(&ca_file).map_err(|_| ConfigError::Unreadable(ca_file.clone()))?;
    let ca_pem = match get(ENV_PROVIDER_CA_SHA256) {
        None => cluster_ca,
        Some(pin) => pinned_provider_ca(&pin, root)?,
    };
    Ok(Some(ProviderInputs {
        target: Target {
            provider,
            namespace,
            service_account,
            api_server,
        },
        access: PodAccess {
            server: in_cluster_server(env),
            token_file: token_file.to_string_lossy().into_owned(),
            ca_file: ca_file.to_string_lossy().into_owned(),
        },
        ca_pem,
    }))
}

/// The mounted provider CA, if it is the pinned one and holds a certificate (ADR-0016). Checked
/// before any connection, to the API server or to the host.
fn pinned_provider_ca(pin: &str, root: &Path) -> Result<Vec<u8>, ConfigError> {
    if pin.len() != 64 || !pin.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(ConfigError::InvalidEnv(ENV_PROVIDER_CA_SHA256));
    }
    let file = under(root, MOUNT_PROVIDER_CA).join(PROVIDER_CA_FILE);
    let bundle = std::fs::read(&file).map_err(|_| ConfigError::Unreadable(file))?;
    let has_certificate = bundle
        .windows(b"-----BEGIN CERTIFICATE-----".len())
        .any(|w| w == b"-----BEGIN CERTIFICATE-----");
    if crate::sha256_hex(&bundle) != pin || !has_certificate {
        return Err(ConfigError::ProviderCaUnpinned);
    }
    Ok(bundle)
}

/// The API server from inside the cluster: the kubelet's `KUBERNETES_SERVICE_*`, or the
/// `kubernetes.default.svc` name.
fn in_cluster_server(env: &dyn Fn(&str) -> Option<String>) -> String {
    match (
        env("KUBERNETES_SERVICE_HOST").filter(|h| !h.is_empty()),
        env("KUBERNETES_SERVICE_PORT").filter(|p| p.parse::<u16>().is_ok()),
    ) {
        (Some(h), Some(p)) if h.contains(':') => format!("https://[{h}]:{p}"),
        (Some(h), Some(p)) => format!("https://{h}:{p}"),
        _ => "https://kubernetes.default.svc".into(),
    }
}

fn under(root: &Path, mount: &str) -> PathBuf {
    root.join(mount.trim_start_matches('/'))
}

/// Reads a credential volume, or `None` when it is not mounted.
fn credential(
    root: &Path,
    mount: &str,
    volume: &'static str,
) -> Result<Option<SshCredentials>, ConfigError> {
    let dir = under(root, mount);
    if !dir.is_dir() {
        return Ok(None);
    }
    let mut data = BTreeMap::new();
    for key in SECRET_KEYS {
        let path = dir.join(key);
        if path.exists() {
            let bytes = std::fs::read(&path).map_err(|_| ConfigError::Unreadable(path))?;
            data.insert(key.to_string(), bytes);
        }
    }
    SshCredentials::from_secret_data(&data)
        .map(Some)
        .map_err(|source| ConfigError::Credential { volume, source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);
    /// sha256 of the script `root()` mounts, `#!/bin/sh\n`.
    const SCRIPT_SHA256: &str = "a8076d3d28d21e02012b20eaf7dbf75409a6277134439025f282e368e3305abf";

    /// A throwaway root with the run credential and the script mounted.
    fn root() -> PathBuf {
        let r = std::env::temp_dir().join(format!(
            "faubourg-bootstrap-config-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let creds = under(&r, MOUNT_CREDENTIALS);
        std::fs::create_dir_all(&creds).unwrap();
        std::fs::write(creds.join(KEY_USERNAME), "faubourg").unwrap();
        std::fs::write(creds.join(KEY_PASSWORD), "not-a-real-password").unwrap();
        let script = under(&r, MOUNT_SCRIPT);
        std::fs::create_dir_all(&script).unwrap();
        std::fs::write(script.join(SCRIPT_FILE), "#!/bin/sh\n").unwrap();
        r
    }

    fn env(overrides: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let mut m: BTreeMap<String, String> = [
            (ENV_HOST, "hv01"),
            (ENV_ADDRESS, "hv01.home.example"),
            (ENV_PORT, "22"),
            (ENV_HOST_KEY_POLICY, "Strict"),
            (ENV_REVOKE_PLACEMENT, "false"),
            (ENV_SCRIPT_SHA256, SCRIPT_SHA256),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        for (k, v) in overrides {
            m.insert((*k).to_string(), (*v).to_string());
        }
        move |k| m.get(k).cloned()
    }

    #[test]
    fn a_complete_job_loads() {
        let c = Config::load(&env(&[]), &root()).unwrap();
        assert_eq!(c.host, "hv01");
        assert_eq!(c.port, 22);
        assert_eq!(c.policy, HostKeyPolicy::Strict);
        assert_eq!(c.run.username, "faubourg");
        assert!(c.placement.is_none());
        assert!(!c.revoke_placement);
        assert_eq!(c.script, b"#!/bin/sh\n");
    }

    #[test]
    fn the_placement_credential_is_read_when_mounted() {
        let r = root();
        let p = under(&r, MOUNT_PLACEMENT);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join(KEY_USERNAME), "root").unwrap();
        std::fs::write(p.join(KEY_PASSWORD), "not-a-real-password").unwrap();
        let c = Config::load(&env(&[(ENV_REVOKE_PLACEMENT, "true")]), &r).unwrap();
        assert_eq!(c.placement.unwrap().username, "root");
        assert!(c.revoke_placement);
    }

    #[test]
    fn malformed_environment_is_refused() {
        for (k, v) in [
            (ENV_HOST, ""),
            (ENV_ADDRESS, ""),
            (ENV_ADDRESS, "-oProxyCommand=sh"),
            (ENV_ADDRESS, "hv01 hv02"),
            (ENV_PORT, "0"),
            (ENV_PORT, "65536"),
            (ENV_PORT, "ssh"),
            (ENV_HOST_KEY_POLICY, "strict"),
            (ENV_HOST_KEY_POLICY, "None"),
            (ENV_ACCEPTED_HOST_KEY, "MD5:aa"),
            (ENV_REVOKE_PLACEMENT, "yes"),
        ] {
            assert!(Config::load(&env(&[(k, v)]), &root()).is_err(), "{k}={v:?}");
        }
    }

    #[test]
    fn errors_never_carry_the_value() {
        let e = Config::load(&env(&[(ENV_ADDRESS, "-oProxyCommand=sh")]), &root()).unwrap_err();
        assert!(!e.to_string().contains("ProxyCommand"), "{e}");
    }

    #[test]
    fn the_mounted_script_must_be_the_pinned_one() {
        assert_eq!(crate::sha256_hex(b"#!/bin/sh\n"), SCRIPT_SHA256);
        let other = "0".repeat(64);
        assert!(matches!(
            Config::load(&env(&[(ENV_SCRIPT_SHA256, &other)]), &root()),
            Err(ConfigError::ScriptUnpinned)
        ));
        let r = root();
        std::fs::write(
            under(&r, MOUNT_SCRIPT).join(SCRIPT_FILE),
            "#!/bin/sh\nevil\n",
        )
        .unwrap();
        assert!(matches!(
            Config::load(&env(&[]), &r),
            Err(ConfigError::ScriptUnpinned)
        ));
    }

    #[test]
    fn the_pinned_digest_is_required_and_well_formed() {
        assert!(matches!(
            Config::load(&env(&[(ENV_SCRIPT_SHA256, "")]), &root()),
            Err(ConfigError::MissingEnv(ENV_SCRIPT_SHA256))
        ));
        for bad in [
            &SCRIPT_SHA256[..63],
            &SCRIPT_SHA256.to_uppercase(),
            "<digest>",
        ] {
            assert!(
                matches!(
                    Config::load(&env(&[(ENV_SCRIPT_SHA256, bad)]), &root()),
                    Err(ConfigError::InvalidEnv(ENV_SCRIPT_SHA256))
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn no_run_credential_or_script_is_refused() {
        let r = root();
        std::fs::remove_dir_all(under(&r, MOUNT_CREDENTIALS)).unwrap();
        assert!(Config::load(&env(&[]), &r).is_err());
        let r = root();
        std::fs::write(under(&r, MOUNT_SCRIPT).join(SCRIPT_FILE), "").unwrap();
        assert!(matches!(
            Config::load(&env(&[]), &r),
            Err(ConfigError::EmptyScript)
        ));
        let r = root();
        std::fs::remove_file(under(&r, MOUNT_SCRIPT).join(SCRIPT_FILE)).unwrap();
        assert!(Config::load(&env(&[]), &r).is_err());
    }

    #[test]
    fn a_refused_secret_is_reported_with_its_volume() {
        let r = root();
        std::fs::write(
            under(&r, MOUNT_CREDENTIALS).join(KEY_PRIVATE_KEY),
            "not-a-real-key",
        )
        .unwrap();
        let e = Config::load(&env(&[]), &r).unwrap_err();
        assert!(
            e.to_string().starts_with("the credentials credential"),
            "{e}"
        );
        assert!(!e.to_string().contains("not-a-real"), "{e}");
    }

    #[test]
    fn debug_shows_no_script_and_no_credential() {
        let c = Config::load(&env(&[]), &root()).unwrap();
        let d = format!("{c:?}");
        assert!(!d.contains("#!/bin/sh"), "{d}");
        assert!(!d.contains("not-a-real-password"), "{d}");
        assert!(!d.contains("hv01.home.example"), "{d}");
    }

    fn with_kube_api(r: &Path) {
        let d = under(r, MOUNT_KUBE_API);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(KUBE_API_TOKEN_FILE), "not-a-real-token").unwrap();
        std::fs::write(d.join(KUBE_API_CA_FILE), "-----BEGIN CERTIFICATE-----\n").unwrap();
    }

    fn provider_env() -> Vec<(&'static str, &'static str)> {
        vec![
            (ENV_PROVIDER, "hv01"),
            (ENV_PROVIDER_NAMESPACE, "banlieue-system"),
            (
                ENV_PROVIDER_SERVICE_ACCOUNT,
                "banlieue-provider-cloud-hypervisor-hv01",
            ),
            (ENV_PROVIDER_API_SERVER, "https://kube.home.example:6443"),
            ("KUBERNETES_SERVICE_HOST", "192.0.2.1"),
            ("KUBERNETES_SERVICE_PORT", "443"),
        ]
    }

    #[test]
    fn the_provider_inputs_are_read_when_the_job_carries_them() {
        let r = root();
        with_kube_api(&r);
        let c = Config::load(&env(&provider_env()), &r).unwrap();
        let p = c.provider.unwrap();
        assert_eq!(
            p.target.service_account,
            "banlieue-provider-cloud-hypervisor-hv01"
        );
        assert_eq!(p.access.server, "https://192.0.2.1:443");
        assert!(
            p.access
                .token_file
                .ends_with("/var/run/faubourg/kube-api/token")
        );
        assert_eq!(p.ca_pem, b"-----BEGIN CERTIFICATE-----\n");
    }

    const CA: &str = "-----BEGIN CERTIFICATE-----\nTHE LOAD BALANCER\n-----END CERTIFICATE-----\n";

    fn with_provider_ca(r: &Path, content: &str) -> String {
        let d = under(r, MOUNT_PROVIDER_CA);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(PROVIDER_CA_FILE), content).unwrap();
        crate::sha256_hex(content.as_bytes())
    }

    #[test]
    fn a_pinned_provider_ca_is_what_the_host_trusts() {
        let r = root();
        with_kube_api(&r);
        let pin = with_provider_ca(&r, CA);
        let mut vars = provider_env();
        vars.push((ENV_PROVIDER_CA_SHA256, &pin));
        let p = Config::load(&env(&vars), &r).unwrap().provider.unwrap();
        assert_eq!(
            p.ca_pem,
            CA.as_bytes(),
            "the pinned bundle, not the cluster CA"
        );
        assert!(
            p.access
                .ca_file
                .ends_with("/var/run/faubourg/kube-api/ca.crt"),
            "the pod's own calls still trust the cluster CA"
        );
    }

    #[test]
    fn a_provider_ca_that_is_not_the_pinned_one_is_refused() {
        let r = root();
        with_kube_api(&r);
        let pin = with_provider_ca(&r, CA);
        // Edited after pinning.
        with_provider_ca(
            &r,
            "-----BEGIN CERTIFICATE-----\nSOMEONE ELSE\n-----END CERTIFICATE-----\n",
        );
        let mut vars = provider_env();
        vars.push((ENV_PROVIDER_CA_SHA256, &pin));
        assert!(matches!(
            Config::load(&env(&vars), &r),
            Err(ConfigError::ProviderCaUnpinned)
        ));
        // Pinned, but no certificate in it.
        let r = root();
        with_kube_api(&r);
        let pin = with_provider_ca(&r, "not a certificate\n");
        let mut vars = provider_env();
        vars.push((ENV_PROVIDER_CA_SHA256, &pin));
        assert!(matches!(
            Config::load(&env(&vars), &r),
            Err(ConfigError::ProviderCaUnpinned)
        ));
        // Pinned, but not mounted.
        let r = root();
        with_kube_api(&r);
        let mut vars = provider_env();
        vars.push((ENV_PROVIDER_CA_SHA256, &pin));
        assert!(matches!(
            Config::load(&env(&vars), &r),
            Err(ConfigError::Unreadable(_))
        ));
        // A malformed pin.
        let r = root();
        with_kube_api(&r);
        with_provider_ca(&r, CA);
        let mut vars = provider_env();
        vars.push((ENV_PROVIDER_CA_SHA256, "<your-ca-sha256>"));
        assert!(matches!(
            Config::load(&env(&vars), &r),
            Err(ConfigError::InvalidEnv(ENV_PROVIDER_CA_SHA256))
        ));
    }

    #[test]
    fn no_provider_variables_no_provider() {
        assert!(Config::load(&env(&[]), &root()).unwrap().provider.is_none());
    }

    #[test]
    fn some_provider_variables_or_no_kube_api_volume_is_refused() {
        let r = root();
        with_kube_api(&r);
        for drop in [
            ENV_PROVIDER,
            ENV_PROVIDER_NAMESPACE,
            ENV_PROVIDER_SERVICE_ACCOUNT,
            ENV_PROVIDER_API_SERVER,
        ] {
            let vars: Vec<_> = provider_env()
                .into_iter()
                .filter(|(k, _)| *k != drop)
                .collect();
            assert!(Config::load(&env(&vars), &r).is_err(), "without {drop}");
        }
        assert!(
            Config::load(&env(&provider_env()), &root()).is_err(),
            "no kube-api volume"
        );
        for (k, v) in [
            (ENV_PROVIDER_API_SERVER, "http://kube.home.example"),
            (ENV_PROVIDER_NAMESPACE, "Not_A_Namespace"),
            (ENV_PROVIDER_SERVICE_ACCOUNT, "-x"),
        ] {
            let mut vars = provider_env();
            vars.push((k, v));
            assert!(Config::load(&env(&vars), &r).is_err(), "{k}={v}");
        }
    }

    #[test]
    fn the_in_cluster_server_brackets_ipv6_and_falls_back_to_the_service_name() {
        let e = |h: &str, p: &str| {
            let (h, p) = (h.to_string(), p.to_string());
            move |k: &str| match k {
                "KUBERNETES_SERVICE_HOST" => Some(h.clone()),
                "KUBERNETES_SERVICE_PORT" => Some(p.clone()),
                _ => None,
            }
        };
        assert_eq!(
            in_cluster_server(&e("2001:db8::1", "443")),
            "https://[2001:db8::1]:443"
        );
        assert_eq!(
            in_cluster_server(&e("", "443")),
            "https://kubernetes.default.svc"
        );
        assert_eq!(
            in_cluster_server(&e("192.0.2.1", "x")),
            "https://kubernetes.default.svc"
        );
    }
}
