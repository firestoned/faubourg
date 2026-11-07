// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! What the bootstrap reads before it connects anywhere (ADR-0010 decision 3): the Job's
//! literal environment and its mounted volumes. Anything missing or malformed is refused here,
//! before a connection is opened. Errors name the input, never its value.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use faubourg_api::bootstrap::{
    ENV_ACCEPTED_HOST_KEY, ENV_ADDRESS, ENV_HOST, ENV_HOST_KEY_POLICY, ENV_PORT,
    ENV_REVOKE_PLACEMENT, MOUNT_CREDENTIALS, MOUNT_PLACEMENT, MOUNT_SCRIPT, SCRIPT_FILE,
    is_fingerprint,
};
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
        })
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
}
