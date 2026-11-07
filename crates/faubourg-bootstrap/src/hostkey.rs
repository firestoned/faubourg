// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! Which host keys a run accepts (ADR-0010 decision 4). Called from russh's
//! `check_server_key`, which runs before any authentication is sent.
//!
//! - A run credential with `knownHosts` is `Strict`, whatever the Host's policy says
//!   (ADR-0008 decision 5): the key must be on a line naming the address.
//! - `Strict` with no `knownHosts` accepts nothing.
//! - `TrustOnFirstUse` with a pinned fingerprint accepts only that key; with none, the first.
//! - Every connection of one run must see the key the first one saw.
//! - RSA keys are refused (ADR-0010 decision 2).

use faubourg_api::host::HostKeyPolicy;
use russh::keys::{HashAlg, PublicKey, ssh_key::public::KeyData};

/// How keys are judged.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode {
    /// Only these keys, from `knownHosts`. Empty accepts nothing.
    Strict(Vec<KeyData>),
    /// Only the key with this fingerprint.
    Pinned(String),
    /// The first key seen.
    FirstUse,
}

/// The host-key decision for one run.
#[derive(Clone, Debug)]
pub struct Verifier {
    mode: Mode,
    seen: Option<KeyData>,
}

impl Verifier {
    /// The verifier for a run: `known_hosts` is the run credential's, if it has one.
    pub fn new(
        policy: HostKeyPolicy,
        accepted: Option<&str>,
        known_hosts: Option<&str>,
        address: &str,
        port: u16,
    ) -> Self {
        let mode = match (known_hosts, policy, accepted) {
            (Some(kh), _, _) => Mode::Strict(keys_for(kh, address, port)),
            (None, HostKeyPolicy::Strict, _) => Mode::Strict(Vec::new()),
            (None, HostKeyPolicy::TrustOnFirstUse, Some(fp)) => Mode::Pinned(fp.to_string()),
            (None, HostKeyPolicy::TrustOnFirstUse, None) => Mode::FirstUse,
        };
        Self { mode, seen: None }
    }

    /// True when the run may authenticate to a server presenting `key`. The first accepted
    /// key is remembered, and every later connection must present the same one.
    pub fn check(&mut self, key: &PublicKey) -> bool {
        if key.algorithm().is_rsa() {
            return false;
        }
        let data = key.key_data();
        if let Some(seen) = &self.seen {
            return seen == data;
        }
        let ok = match &self.mode {
            Mode::Strict(allowed) => allowed.iter().any(|k| k == data),
            Mode::Pinned(fp) => fingerprint(key) == *fp,
            Mode::FirstUse => true,
        };
        if ok {
            self.seen = Some(data.clone());
        }
        ok
    }

    /// The accepted key's fingerprint, once one was accepted.
    pub fn accepted(&self) -> Option<String> {
        self.seen.as_ref().map(|d| {
            PublicKey::from(d.clone())
                .fingerprint(HashAlg::Sha256)
                .to_string()
        })
    }
}

/// A key's OpenSSH SHA256 fingerprint, `SHA256:<base64>`.
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// The keys `known_hosts` lists for `address` on `port`, minus any marked `@revoked`.
///
/// A line applies when one of its comma-separated host patterns is exactly `address` (port 22
/// only, as OpenSSH writes it) or `[address]:port`, and no negated pattern (`!p`) names it.
/// Hashed (`|1|`) and wildcard patterns are not read, so they never match: fail closed.
/// `@cert-authority` lines are ignored: host certificates are refused (ADR-0010 decision 2).
pub fn keys_for(known_hosts: &str, address: &str, port: u16) -> Vec<KeyData> {
    let wanted = if port == 22 {
        vec![address.to_string(), format!("[{address}]:22")]
    } else {
        vec![format!("[{address}]:{port}")]
    };
    let mut keys = Vec::new();
    let mut revoked = Vec::new();
    for line in known_hosts.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(mut first) = fields.next() else {
            continue;
        };
        let marker = first.strip_prefix('@').map(str::to_string);
        if marker.is_some() {
            let Some(next) = fields.next() else { continue };
            first = next;
        }
        let (Some(kind), Some(blob)) = (fields.next(), fields.next()) else {
            continue;
        };
        let mut named = false;
        let mut negated = false;
        for pattern in first.split(',') {
            if let Some(p) = pattern.strip_prefix('!') {
                negated |= wanted.iter().any(|n| n == p);
            } else {
                named |= wanted.iter().any(|n| n == pattern);
            }
        }
        if !named || negated {
            continue;
        }
        let Ok(key) = PublicKey::from_openssh(&format!("{kind} {blob}")) else {
            continue;
        };
        match marker.as_deref() {
            None => keys.push(key.key_data().clone()),
            Some("revoked") => revoked.push(key.key_data().clone()),
            Some(_) => {}
        }
    }
    keys.retain(|k| !revoked.contains(k));
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    const K1: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBF5qrTV0h+/FpWXMyEC6c4zU3AMcBQ729sJC+0QsZkq";
    const K1_FP: &str = "SHA256:w/MKP5qmOhlfmXHftrq9Q2lgVxsxOdNJo9vPBW8Li+E";
    const K2: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFs1C9ubBvNhN5ZFJcAG4eBv3U0FoXYZVovlLJGNfjFj";
    const K3: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBOjQIUEfNSIydd9FWGwZLJ9tpvGdWJ+n0IMV+atDxYvLhIeoiJQtfixga9I/fASJcYzUbRUIeoShg0enf8AQRDk=";
    const RSA: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQDWhcks5MwdZlWU7uKDC59ljn0by6rVa/RucMFtdqWD0kg0Wci5T48khxZv0wH2eaBeTkBEKA9NGcd3eTmCni1pdFh/cu1dk3KSyPxRLPm3+YBhz0/Ho/4waVYRBZZb3nIWGa1CSGXT63d2Q9txkxAfuHq3IEroxa257rQGKhrjoSR65IKqRQrdUvxbUOfHMq5cYOpGLwf7LmXr56ZtXLAe8lx4fXnrWCYJzfkvzfl6fwpXDRqD1W63gVPkmG/jzlfoiIHkMzl/Tou/dovOnsrn9qi+9fzxn4Zjpp+cfQmT34Cq62dSxXO1RViYOhR1rCVAver/hARlcSCWuiTqmRLZ";
    const HOST: &str = "hv01.home.example";

    fn key(s: &str) -> PublicKey {
        PublicKey::from_openssh(s).unwrap()
    }

    fn strict(kh: &str, port: u16) -> Verifier {
        Verifier::new(HostKeyPolicy::Strict, None, Some(kh), HOST, port)
    }

    #[test]
    fn known_hosts_pins_the_host() {
        let kh = format!("{HOST} {K1}\nhv02.home.example {K2}\n");
        let mut v = strict(&kh, 22);
        assert!(!strict(&kh, 22).check(&key(K2)), "another host's key");
        assert!(v.check(&key(K1)));
        assert_eq!(v.accepted().as_deref(), Some(K1_FP));
    }

    #[test]
    fn known_hosts_overrides_trust_on_first_use() {
        let kh = format!("{HOST} {K1}\n");
        let mut v = Verifier::new(HostKeyPolicy::TrustOnFirstUse, None, Some(&kh), HOST, 22);
        assert!(!v.check(&key(K2)));
    }

    #[test]
    fn strict_without_known_hosts_accepts_nothing() {
        let mut v = Verifier::new(HostKeyPolicy::Strict, None, None, HOST, 22);
        assert!(!v.check(&key(K1)));
        assert_eq!(v.accepted(), None);
    }

    #[test]
    fn first_use_accepts_the_first_key_then_only_it() {
        let mut v = Verifier::new(HostKeyPolicy::TrustOnFirstUse, None, None, HOST, 22);
        assert!(v.check(&key(K3)));
        assert!(v.check(&key(K3)));
        assert!(
            !v.check(&key(K1)),
            "the placement connection saw another key"
        );
    }

    #[test]
    fn a_pinned_fingerprint_accepts_only_its_key() {
        let mut v = Verifier::new(HostKeyPolicy::TrustOnFirstUse, Some(K1_FP), None, HOST, 22);
        assert!(!v.clone().check(&key(K2)));
        assert!(v.check(&key(K1)));
    }

    #[test]
    fn rsa_is_refused_even_when_pinned() {
        let kh = format!("{HOST} {RSA}\n");
        assert!(!strict(&kh, 22).check(&key(RSA)));
        let mut v = Verifier::new(HostKeyPolicy::TrustOnFirstUse, None, None, HOST, 22);
        assert!(!v.check(&key(RSA)));
    }

    #[test]
    fn a_non_default_port_needs_the_bracketed_form() {
        let plain = format!("{HOST} {K1}\n");
        assert!(!strict(&plain, 2222).check(&key(K1)));
        let bracketed = format!("[{HOST}]:2222 {K1}\n");
        assert!(strict(&bracketed, 2222).check(&key(K1)));
        assert!(!strict(&bracketed, 22).check(&key(K1)));
    }

    #[test]
    fn hashed_wildcard_and_negated_patterns_never_match() {
        for kh in [
            format!("|1|aGFzaA==|aGFzaA== {K1}"),
            format!("*.home.example {K1}"),
            format!("hv0?.home.example {K1}"),
            format!("{HOST},!{HOST} {K1}"),
        ] {
            assert!(!strict(&kh, 22).check(&key(K1)), "{kh}");
        }
    }

    #[test]
    fn a_revoked_key_is_refused_and_markers_are_read() {
        let kh = format!("{HOST} {K1}\n@revoked {HOST} {K1}\n@cert-authority {HOST} {K2}\n");
        assert_eq!(keys_for(&kh, HOST, 22), Vec::new());
        assert!(!strict(&kh, 22).check(&key(K2)));
    }

    #[test]
    fn several_keys_for_one_host_and_comments_are_read() {
        let kh = format!("# rotated in 2026\n{HOST},192.0.2.10 {K1} old\n\n{HOST} {K3} new\n");
        assert_eq!(keys_for(&kh, HOST, 22).len(), 2);
        assert!(strict(&kh, 22).check(&key(K3)));
    }

    #[test]
    fn garbage_lines_are_skipped() {
        let kh = format!("{HOST}\n{HOST} ssh-ed25519\n{HOST} ssh-ed25519 !!!\n{HOST} {K1}\n");
        assert_eq!(keys_for(&kh, HOST, 22).len(), 1);
    }
}
