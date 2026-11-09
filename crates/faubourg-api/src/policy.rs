// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! The pinned script sources in the bootstrap policy's parameters (ADR-0013 decision 1).
//!
//! `scriptSources` is a comma-separated list of `<Kind>/<name>/<key>@sha256:<64 lowercase
//! hex>`, where `<Kind>` is `ConfigMap` or `Secret`. The admission policy checks every Job
//! against it; the controller reads it to learn each script's pinned digest. Parsing fails
//! closed: one malformed or repeated entry rejects the whole list.

use crate::host::{ScriptRef, ScriptSourceKind, is_config_key};
use crate::validation::is_dns_subdomain;

/// The policy parameter holding the pinned script sources.
pub const SCRIPT_SOURCES_KEY: &str = "scriptSources";
/// The policy parameter pinning the CA the hosts trust for the API server (ADR-0016).
pub const PROVIDER_CA_SOURCE_KEY: &str = "providerCASource";

/// What the bootstrap policy's parameters pin: the scripts, and optionally the CA the hosts
/// trust for the API server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    /// The pinned install scripts (ADR-0013).
    pub scripts: Vec<PinnedScript>,
    /// The pinned CA bundle, a `ConfigMap` key; `None` means the cluster CA (ADR-0016).
    pub provider_ca: Option<PinnedScript>,
}

/// Parses the two parameters. `provider_ca` empty or absent means the cluster CA.
///
/// # Errors
/// [`PolicyError`] for a malformed or repeated script entry, or a provider CA that is not one
/// pinned `ConfigMap/<name>/<key>@sha256:<64 lowercase hex>`.
pub fn parse_policy(
    script_sources: &str,
    provider_ca: Option<&str>,
) -> Result<Policy, PolicyError> {
    let scripts = parse_script_sources(script_sources)?;
    let provider_ca = match provider_ca.filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => {
            let pin = parse_entry(s).ok_or(PolicyError::MalformedProviderCa)?;
            // A CA bundle is not secret: only a ConfigMap may hold it.
            if pin.source.kind != ScriptSourceKind::ConfigMap {
                return Err(PolicyError::MalformedProviderCa);
            }
            Some(pin)
        }
    };
    Ok(Policy {
        scripts,
        provider_ca,
    })
}

/// One pinned script: a source, the key a Job mounts, and the sha256 of that key's content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedScript {
    /// The source and key.
    pub source: ScriptRef,
    /// The sha256 of the key's content, 64 lowercase hex.
    pub sha256: String,
}

/// Why `scriptSources` was refused. Never carries the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    /// An entry is not `<Kind>/<name>/<key>@sha256:<64 lowercase hex>`.
    #[error("scriptSources entry {0} is not <Kind>/<name>/<key>@sha256:<64 lowercase hex>")]
    Malformed(usize),
    /// A source and key appear twice: only one script may be pinned per source.
    #[error("scriptSources entry {0} pins a source and key already pinned")]
    Repeated(usize),
    /// `providerCASource` is not one `ConfigMap/<name>/<key>@sha256:<64 lowercase hex>`.
    #[error("providerCASource is not ConfigMap/<name>/<key>@sha256:<64 lowercase hex>")]
    MalformedProviderCa,
}

/// Parses `scriptSources`. An empty value pins nothing.
///
/// # Errors
/// [`PolicyError`] for a malformed or repeated entry, numbered from 1.
pub fn parse_script_sources(s: &str) -> Result<Vec<PinnedScript>, PolicyError> {
    let mut pins: Vec<PinnedScript> = Vec::new();
    if s.is_empty() {
        return Ok(pins);
    }
    for (i, entry) in s.split(',').enumerate() {
        let n = i + 1;
        let pin = parse_entry(entry).ok_or(PolicyError::Malformed(n))?;
        if pins.iter().any(|p| p.source == pin.source) {
            return Err(PolicyError::Repeated(n));
        }
        pins.push(pin);
    }
    Ok(pins)
}

fn parse_entry(entry: &str) -> Option<PinnedScript> {
    let (source, digest) = entry.split_once("@sha256:")?;
    let mut parts = source.split('/');
    let kind = match parts.next()? {
        "ConfigMap" => ScriptSourceKind::ConfigMap,
        "Secret" => ScriptSourceKind::Secret,
        _ => return None,
    };
    let name = parts.next()?;
    let key = parts.next()?;
    let digest_ok = digest.len() == 64
        && digest
            .bytes()
            .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'));
    if parts.next().is_some() || !is_dns_subdomain(name) || !is_config_key(key) || !digest_ok {
        return None;
    }
    Some(PinnedScript {
        source: ScriptRef {
            kind,
            name: name.into(),
            key: key.into(),
        },
        sha256: digest.into(),
    })
}

/// The digest pinned for `source`, if any.
pub fn pinned_digest<'a>(pins: &'a [PinnedScript], source: &ScriptRef) -> Option<&'a str> {
    pins.iter()
        .find(|p| &p.source == source)
        .map(|p| p.sha256.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const B: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    fn r(kind: ScriptSourceKind, name: &str, key: &str) -> ScriptRef {
        ScriptRef {
            kind,
            name: name.into(),
            key: key.into(),
        }
    }

    #[test]
    fn entries_parse_into_source_key_and_digest() {
        let pins = parse_script_sources(&format!(
            "ConfigMap/faubourg-install/install.sh@sha256:{A},Secret/site-install/run.sh@sha256:{B}"
        ))
        .unwrap();
        assert_eq!(
            pins,
            vec![
                PinnedScript {
                    source: r(
                        ScriptSourceKind::ConfigMap,
                        "faubourg-install",
                        "install.sh"
                    ),
                    sha256: A.into()
                },
                PinnedScript {
                    source: r(ScriptSourceKind::Secret, "site-install", "run.sh"),
                    sha256: B.into()
                },
            ]
        );
        let cm = r(
            ScriptSourceKind::ConfigMap,
            "faubourg-install",
            "install.sh",
        );
        assert_eq!(pinned_digest(&pins, &cm), Some(A));
        let other_key = r(ScriptSourceKind::ConfigMap, "faubourg-install", "other.sh");
        assert_eq!(
            pinned_digest(&pins, &other_key),
            None,
            "the key is part of the pin"
        );
        let other_kind = r(ScriptSourceKind::Secret, "faubourg-install", "install.sh");
        assert_eq!(pinned_digest(&pins, &other_kind), None, "so is the kind");
    }

    #[test]
    fn an_empty_list_pins_nothing() {
        assert_eq!(parse_script_sources(""), Ok(vec![]));
    }

    #[test]
    fn a_malformed_entry_rejects_the_whole_list() {
        let ok = format!("ConfigMap/faubourg-install/install.sh@sha256:{A}");
        for bad in [
            "ConfigMap/faubourg-install".to_string(), // the v0.1 form: no key, no digest
            "ConfigMap/faubourg-install/install.sh".to_string(),
            "ConfigMap/faubourg-install/install.sh@sha256:<your-script-sha256>".to_string(), // shipped
            format!("ConfigMap/faubourg-install/install.sh@sha256:{}", &A[..63]),
            format!(
                "ConfigMap/faubourg-install/install.sh@sha256:{}",
                A.to_uppercase()
            ),
            format!("Pod/faubourg-install/install.sh@sha256:{A}"),
            format!("ConfigMap/Faubourg/install.sh@sha256:{A}"),
            format!("ConfigMap/faubourg-install/@sha256:{A}"),
            format!("ConfigMap/faubourg-install/a/b@sha256:{A}"),
            format!("ConfigMap/faubourg-install/..@sha256:{A}"),
            format!(" ConfigMap/faubourg-install/install.sh@sha256:{A}"),
            format!("ConfigMap/faubourg-install/install.sh@sha512:{A}"),
        ] {
            assert_eq!(
                parse_script_sources(&format!("{ok},{bad}")),
                Err(PolicyError::Malformed(2)),
                "{bad}"
            );
        }
        assert_eq!(
            parse_script_sources(&format!("{ok},")),
            Err(PolicyError::Malformed(2)),
            "a trailing comma"
        );
    }

    #[test]
    fn one_source_and_key_is_pinned_once() {
        assert_eq!(
            parse_script_sources(&format!(
                "ConfigMap/s/install.sh@sha256:{A},ConfigMap/s/install.sh@sha256:{B}"
            )),
            Err(PolicyError::Repeated(2))
        );
        assert!(
            parse_script_sources(&format!(
                "ConfigMap/s/install.sh@sha256:{A},ConfigMap/s/other.sh@sha256:{B}"
            ))
            .is_ok(),
            "another key of the same source is another script"
        );
    }

    #[test]
    fn no_provider_ca_means_the_cluster_ca() {
        for none in [None, Some("")] {
            let p = parse_policy("", none).unwrap();
            assert_eq!(p.provider_ca, None, "{none:?}");
        }
    }

    #[test]
    fn a_provider_ca_is_one_pinned_configmap_key() {
        let p = parse_policy("", Some(&format!("ConfigMap/api-ca/ca.crt@sha256:{A}"))).unwrap();
        assert_eq!(
            p.provider_ca,
            Some(PinnedScript {
                source: r(ScriptSourceKind::ConfigMap, "api-ca", "ca.crt"),
                sha256: A.into()
            })
        );
        for bad in [
            format!("Secret/api-ca/ca.crt@sha256:{A}"),
            "ConfigMap/api-ca/ca.crt".to_string(),
            "ConfigMap/api-ca/ca.crt@sha256:<your-ca-sha256>".to_string(),
            format!("ConfigMap/api-ca/ca.crt@sha256:{A},ConfigMap/b/ca.crt@sha256:{B}"),
        ] {
            assert_eq!(
                parse_policy("", Some(&bad)),
                Err(PolicyError::MalformedProviderCa),
                "{bad}"
            );
        }
    }

    #[test]
    fn errors_never_carry_the_value() {
        let e = parse_script_sources("ConfigMap/secret-name-here").unwrap_err();
        assert!(!e.to_string().contains("secret-name-here"));
    }
}
