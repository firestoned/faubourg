// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! `faubourg-bootstrap`: the program in the bootstrap image (ADR-0010).
//!
//! Exit codes: 0 success, 64 `ScriptDrift`, 65 host key refused, 1 anything else. On success
//! and on drift, the accepted host key's fingerprint is written to the termination message.

use std::path::Path;
use std::process::ExitCode;

use faubourg_api::bootstrap::{EXIT_FAILED, Report};
use faubourg_api::credentials::Auth;
use faubourg_bootstrap::config::Config;
use faubourg_bootstrap::hostkey::Verifier;
use faubourg_bootstrap::ssh::Ssh;
use faubourg_bootstrap::steps::{self, Outcome, Placement, Plan};
use faubourg_bootstrap::{exit_code, provider, remote, sha256_hex};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

/// Where the pod's termination message goes (the kubelet's default `terminationMessagePath`).
const TERMINATION_LOG: &str = "/dev/termination-log";

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let code = run().await;
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

async fn run() -> i32 {
    let cfg = match Config::load(&|k| std::env::var(k).ok(), Path::new("/")) {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "refusing to start");
            return EXIT_FAILED;
        }
    };
    let span = tracing::info_span!("bootstrap", host = %cfg.host);
    let _guard = span.enter();

    let verifier = Verifier::new(
        cfg.policy,
        cfg.accepted_host_key.as_deref(),
        cfg.run.known_hosts.as_deref(),
        &cfg.address,
        cfg.port,
    );
    let placement = match cfg.placement.as_ref().map(placement_of).transpose() {
        Ok(p) => p.map(|mut p| {
            if !cfg.revoke_placement {
                p.revoke_blob = None;
            }
            p
        }),
        Err(e) => {
            error!(error = e, "refusing the placement credential");
            return EXIT_FAILED;
        }
    };
    // The provider token is minted before any connection: no token, nothing touches the host.
    let run_stdin = match &cfg.provider {
        None => Vec::new(),
        Some(p) => match provider::mint(&p.access, &p.target).await {
            Ok(m) => {
                info!(
                    service_account = %p.target.service_account,
                    expires = m.expires.as_deref().unwrap_or("unknown"),
                    "minted the host's provider token"
                );
                let kubeconfig =
                    provider::host_kubeconfig(&p.target.api_server, &p.ca_pem, &p.target.namespace);
                provider::bundle(&p.target, &m.token, &kubeconfig)
            }
            Err(e) => {
                error!(error = %e, "cannot mint the host's provider token");
                return EXIT_FAILED;
            }
        },
    };
    let plan = Plan {
        script_sha256: sha256_hex(&cfg.script),
        script: cfg.script.clone(),
        run_user: cfg.run.username.clone(),
        placement,
        run_stdin,
    };
    let mut ssh = Ssh::new(
        cfg.address.clone(),
        cfg.port,
        cfg.run.clone(),
        cfg.placement.clone(),
        verifier,
    );
    let outcome = steps::bootstrap(&mut ssh, &plan).await;
    let fingerprint = ssh.accepted_host_key();
    ssh.close().await;
    if let Some(fp) = &fingerprint {
        info!(fingerprint = %fp, "host key accepted");
    }
    let outcome = match outcome {
        Ok(o) => o,
        Err(e) => {
            error!(error = %e, "bootstrap failed");
            return EXIT_FAILED;
        }
    };
    match outcome {
        Outcome::Succeeded => info!("bootstrap succeeded"),
        Outcome::ScriptDrift => {
            warn!("ScriptDrift: the install script is not in place; nothing ran");
        }
        Outcome::HostKeyRefused => warn!("the host key was refused; nothing was authenticated"),
        Outcome::Failed(step, status) => warn!(step = ?step, status, "a step failed"),
    }
    if matches!(outcome, Outcome::Succeeded | Outcome::ScriptDrift) {
        report(fingerprint);
    }
    exit_code(outcome)
}

/// What the steps need of the placement credential: whether it is root, and its public key
/// blob for revoking. A password credential cannot be revoked by key.
fn placement_of(c: &faubourg_api::credentials::SshCredentials) -> Result<Placement, &'static str> {
    let blob = match &c.auth {
        Auth::Key {
            private_key,
            passphrase,
            ..
        } => {
            let text = std::str::from_utf8(private_key.expose()).map_err(|_| "not text")?;
            let pass = passphrase
                .as_ref()
                .map(|p| std::str::from_utf8(p.expose()))
                .transpose()
                .map_err(|_| "passphrase not UTF-8")?;
            let key = russh::keys::decode_secret_key(text, pass).map_err(|_| "unreadable key")?;
            let public = key
                .public_key()
                .to_openssh()
                .map_err(|_| "unreadable key")?;
            remote::key_blob(&public).map(str::to_string)
        }
        Auth::Password(_) => None,
    };
    Ok(Placement {
        as_root: c.username == "root",
        revoke_blob: blob,
    })
}

fn report(fingerprint: Option<String>) {
    let body = serde_json::to_string(&Report {
        host_key: fingerprint,
    })
    .unwrap_or_else(|_| "{}".into());
    if let Err(e) = std::fs::write(TERMINATION_LOG, body) {
        warn!(error = %e, "cannot write the termination message");
    }
}
