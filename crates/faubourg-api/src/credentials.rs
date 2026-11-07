// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! The SSH Secret contract (ADR-0006): which keys a host's Secret holds and which
//! combinations are accepted. Read by the bootstrap Job, never by the controller.
//!
//! Secret material is held in [`Sensitive`], whose `Debug` is redacted, so a stray `{:?}`
//! cannot print it.

use std::collections::BTreeMap;
use std::fmt;

/// Secret key: account used for bootstrap. Required.
pub const KEY_USERNAME: &str = "username";
/// Secret key: OpenSSH private key. Preferred.
pub const KEY_PRIVATE_KEY: &str = "privateKey";
/// Secret key: passphrase for an encrypted `privateKey`.
pub const KEY_PASSPHRASE: &str = "passphrase";
/// Secret key: SSH certificate for `privateKey`, from an SSH CA.
pub const KEY_CERTIFICATE: &str = "certificate";
/// Secret key: password. Supported, discouraged.
pub const KEY_PASSWORD: &str = "password";
/// Secret key: `known_hosts` lines pinning the host key.
pub const KEY_KNOWN_HOSTS: &str = "knownHosts";

/// Bytes that must never be printed.
#[derive(Clone, PartialEq, Eq)]
pub struct Sensitive(Vec<u8>);

impl Sensitive {
    /// The raw bytes, for handing to the SSH client.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Sensitive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// How the Job authenticates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Auth {
    /// A private key, optionally encrypted, optionally with an SSH certificate.
    Key {
        /// OpenSSH private key.
        private_key: Sensitive,
        /// Passphrase, if the key is encrypted.
        passphrase: Option<Sensitive>,
        /// SSH certificate (public material, but kept with the key).
        certificate: Option<Vec<u8>>,
    },
    /// A password. Flagged with a `WeakCredential` condition.
    Password(Sensitive),
}

/// A parsed, validated SSH credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshCredentials {
    /// Login name.
    pub username: String,
    /// Authentication material.
    pub auth: Auth,
    /// `known_hosts` lines, if pinned.
    pub known_hosts: Option<String>,
}

/// Why a Secret was refused. Never carries secret values.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    /// A required key is absent or empty.
    #[error("secret key {0} is missing or empty")]
    Missing(&'static str),
    /// Both `privateKey` and `password` are set; the choice must be explicit.
    #[error("set exactly one of privateKey or password")]
    Ambiguous,
    /// A key that only makes sense with `privateKey` was given with `password`.
    #[error("secret key {0} requires privateKey")]
    RequiresPrivateKey(&'static str),
    /// A text key is not UTF-8.
    #[error("secret key {0} is not UTF-8")]
    NotUtf8(&'static str),
    /// The username is not a plain login name.
    #[error("username must be a plain login name")]
    InvalidUsername,
}

impl SshCredentials {
    /// Parses a Secret's `data` (decoded bytes) per ADR-0006.
    pub fn from_secret_data(data: &BTreeMap<String, Vec<u8>>) -> Result<Self, CredentialError> {
        let get = |k: &str| data.get(k).filter(|v| !v.is_empty());
        let text = |k: &'static str| -> Result<Option<String>, CredentialError> {
            get(k)
                .map(|v| String::from_utf8(v.clone()).map_err(|_| CredentialError::NotUtf8(k)))
                .transpose()
        };

        let username = text(KEY_USERNAME)?.ok_or(CredentialError::Missing(KEY_USERNAME))?;
        if !is_login_name(&username) {
            return Err(CredentialError::InvalidUsername);
        }

        let auth = match (get(KEY_PRIVATE_KEY), get(KEY_PASSWORD)) {
            (Some(_), Some(_)) => return Err(CredentialError::Ambiguous),
            (None, None) => return Err(CredentialError::Missing(KEY_PRIVATE_KEY)),
            (Some(k), None) => Auth::Key {
                private_key: Sensitive(k.clone()),
                passphrase: get(KEY_PASSPHRASE).map(|p| Sensitive(p.clone())),
                certificate: get(KEY_CERTIFICATE).cloned(),
            },
            (None, Some(p)) => {
                for k in [KEY_PASSPHRASE, KEY_CERTIFICATE] {
                    if get(k).is_some() {
                        return Err(CredentialError::RequiresPrivateKey(k));
                    }
                }
                Auth::Password(Sensitive(p.clone()))
            }
        };

        Ok(Self {
            username,
            auth,
            known_hosts: text(KEY_KNOWN_HOSTS)?,
        })
    }

    /// True for password authentication (ADR-0006: supported, discouraged).
    pub fn is_weak(&self) -> bool {
        matches!(self.auth, Auth::Password(_))
    }
}

/// A POSIX-portable login name that cannot be read as an option.
fn is_login_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && (b[0].is_ascii_lowercase() || b[0] == b'_')
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE_KEY: &[u8] = b"-----BEGIN OPENSSH PRIVATE KEY-----\nnot-a-real-key\n";

    fn data(pairs: &[(&str, &[u8])]) -> BTreeMap<String, Vec<u8>> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.to_vec()))
            .collect()
    }

    #[test]
    fn key_auth_parses_and_is_not_weak() {
        let c = SshCredentials::from_secret_data(&data(&[
            (KEY_USERNAME, b"hexley"),
            (KEY_PRIVATE_KEY, FAKE_KEY),
            (KEY_KNOWN_HOSTS, b"hv01.home.example ssh-ed25519 AAAA"),
        ]))
        .unwrap();
        assert_eq!(c.username, "hexley");
        assert!(!c.is_weak());
        assert!(c.known_hosts.is_some());
    }

    #[test]
    fn password_auth_is_flagged_weak() {
        let c = SshCredentials::from_secret_data(&data(&[
            (KEY_USERNAME, b"hexley"),
            (KEY_PASSWORD, b"pw"),
        ]))
        .unwrap();
        assert!(c.is_weak());
    }

    #[test]
    fn refusals() {
        type Case<'a> = (&'a [(&'a str, &'a [u8])], CredentialError);
        let cases: &[Case<'_>] = &[
            (
                &[(KEY_PRIVATE_KEY, FAKE_KEY)],
                CredentialError::Missing(KEY_USERNAME),
            ),
            (
                &[(KEY_USERNAME, b"")],
                CredentialError::Missing(KEY_USERNAME),
            ),
            (
                &[(KEY_USERNAME, b"hexley")],
                CredentialError::Missing(KEY_PRIVATE_KEY),
            ),
            (
                &[
                    (KEY_USERNAME, b"hexley"),
                    (KEY_PRIVATE_KEY, FAKE_KEY),
                    (KEY_PASSWORD, b"pw"),
                ],
                CredentialError::Ambiguous,
            ),
            (
                &[
                    (KEY_USERNAME, b"hexley"),
                    (KEY_PASSWORD, b"pw"),
                    (KEY_PASSPHRASE, b"x"),
                ],
                CredentialError::RequiresPrivateKey(KEY_PASSPHRASE),
            ),
            (
                &[(KEY_USERNAME, b"-oProxyCommand=x"), (KEY_PASSWORD, b"pw")],
                CredentialError::InvalidUsername,
            ),
            (
                &[(KEY_USERNAME, b"root@hv"), (KEY_PASSWORD, b"pw")],
                CredentialError::InvalidUsername,
            ),
            (
                &[(KEY_USERNAME, &[0xff, 0xfe]), (KEY_PASSWORD, b"pw")],
                CredentialError::NotUtf8(KEY_USERNAME),
            ),
        ];
        for (pairs, want) in cases {
            assert_eq!(
                SshCredentials::from_secret_data(&data(pairs)).unwrap_err(),
                *want
            );
        }
    }

    #[test]
    fn debug_never_prints_secret_material() {
        let c = SshCredentials::from_secret_data(&data(&[
            (KEY_USERNAME, b"hexley"),
            (KEY_PRIVATE_KEY, FAKE_KEY),
            (KEY_PASSPHRASE, b"hunter2"),
        ]))
        .unwrap();
        let shown = format!("{c:?}");
        assert!(!shown.contains("not-a-real-key"));
        assert!(!shown.contains("hunter2"));
        assert!(shown.contains("<redacted>"));
    }
}
