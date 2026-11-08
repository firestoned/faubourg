// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! The bootstrap image's one binary (ADR-0010): verify the host key, place the install script
//! and the sudo rule when they differ, run `sudo -n /opt/faubourg/install.sh`.
//!
//! - [`config`]: the Job's environment and mounts, refused before any connection if malformed.
//! - [`hostkey`]: which host keys a run accepts.
//! - [`provider`]: the host's first provider token, and the bundle that carries it.
//! - [`remote`]: the constant commands sent to the host.
//! - [`steps`]: check, place, check again, revoke, run, against a [`steps::Remote`].
//! - [`ssh`]: [`steps::Remote`] over SSH, on russh.

pub mod config;
pub mod hostkey;
pub mod provider;
pub mod remote;
pub mod ssh;
pub mod steps;

use faubourg_api::bootstrap::{EXIT_FAILED, EXIT_HOST_KEY_REFUSED, EXIT_SCRIPT_DRIFT};
use sha2::{Digest, Sha256};

use crate::steps::Outcome;

/// The process exit code for an outcome (ADR-0010 decision 7).
pub fn exit_code(o: Outcome) -> i32 {
    match o {
        Outcome::Succeeded => 0,
        Outcome::ScriptDrift => EXIT_SCRIPT_DRIFT,
        Outcome::HostKeyRefused => EXIT_HOST_KEY_REFUSED,
        Outcome::Failed(..) => EXIT_FAILED,
    }
}

/// sha256 of `bytes`, lowercase hex, as `sha256sum` prints it.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steps::Step;

    #[test]
    fn exit_codes_are_the_ones_the_job_fails_fast_on() {
        assert_eq!(exit_code(Outcome::Succeeded), 0);
        assert_eq!(exit_code(Outcome::ScriptDrift), 64);
        assert_eq!(exit_code(Outcome::HostKeyRefused), 65);
        assert_eq!(
            exit_code(Outcome::Failed(Step::Run, 64)),
            1,
            "never the script's own 64"
        );
    }

    #[test]
    fn sha256_matches_sha256sum() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
