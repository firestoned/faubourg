// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! `faubourg-controller`: the Host controller (ADR-0009).

use std::process::ExitCode;

use clap::Parser;
use faubourg_controller::config::Args;
use faubourg_controller::controller;
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
    info!(
        credentials_namespace = %cfg.bootstrap.namespace,
        provider_namespace = %cfg.provider_namespace,
        "starting the Host controller"
    );
    controller::run(client, cfg).await;
    info!("stopped");
    ExitCode::SUCCESS
}
