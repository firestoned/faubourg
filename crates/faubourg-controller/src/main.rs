// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! `faubourg-controller`: the Host controller (ADR-0009), run only by the replica that holds
//! the leader Lease (ADR-0014).

use std::process::ExitCode;

use clap::Parser;
use faubourg_controller::config::Args;
use faubourg_controller::{controller, leader};
use k8s_openapi::api::coordination::v1::Lease;
use kube::Api;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    // Fail closed: a configuration the controller cannot use stops it (ADR-0009 decision 11).
    let cfg = match Args::parse().validate() {
        Ok(cfg) => cfg,
        Err(e) => {
            error!(error = %e, "refusing to start");
            return ExitCode::FAILURE;
        }
    };
    let client = match kube::Client::try_default().await {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "no Kubernetes client");
            return ExitCode::FAILURE;
        }
    };
    let election = cfg.election.clone();
    let leases: Api<Lease> = Api::namespaced(client.clone(), &election.namespace);

    // A standby only waits; it can still be stopped while it does.
    tokio::select! {
        () = leader::acquire(&leases, &election) => {}
        () = shutdown() => {
            info!("stopped while waiting to lead");
            return ExitCode::SUCCESS;
        }
    }
    info!(
        identity = %election.identity,
        credentials_namespace = %cfg.bootstrap.namespace,
        provider_namespace = %cfg.bootstrap.provider.namespace,
        provider_class = %cfg.bootstrap.provider.class,
        "starting the Host controller"
    );
    tokio::select! {
        () = controller::run(client, cfg) => {
            // A clean stop: hand over now rather than when the lease runs out.
            leader::release(&leases, &election).await;
            info!("stopped");
            ExitCode::SUCCESS
        }
        lost = leader::hold(&leases, &election) => {
            // Stop at once: another replica may act from here (ADR-0014 decision 4).
            error!(reason = ?lost, "lost the leader Lease; stopping");
            ExitCode::FAILURE
        }
    }
}

/// SIGTERM or SIGINT.
async fn shutdown() {
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        () = term => {}
    }
}
