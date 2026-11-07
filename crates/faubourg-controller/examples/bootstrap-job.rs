// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! Prints, as JSON, the bootstrap Job the builder makes for a fixture Host, so
//! `scripts/vap-test.sh` can submit the controller's real output to a real API server.
//!
//! Usage: `cargo run -q -p faubourg-controller --example bootstrap-job -- <image@sha256:...>`

use faubourg_api::host::{
    BootstrapSpec, DEFAULT_SCRIPT_KEY, Host, HostKeyPolicy, HostSpec, ScriptRef, ScriptSourceKind,
    SecretName, SshSpec,
};
use faubourg_controller::job::{BootstrapConfig, bootstrap_job};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let image = std::env::args()
        .nth(1)
        .ok_or("usage: bootstrap-job <image@sha256:...>")?;
    let mut host = Host::new(
        "hv01",
        HostSpec {
            zone_ref: "az1".into(),
            address: "hv01.home.example".into(),
            ssh: Some(SshSpec {
                port: 22,
                secret_ref: SecretName {
                    name: "hv01-ssh".into(),
                },
                placement_secret_ref: Some(SecretName {
                    name: "hv01-ssh-placement".into(),
                }),
                host_key_policy: HostKeyPolicy::Strict,
                revoke_after_bootstrap: true,
            }),
            bootstrap: BootstrapSpec::default(),
            cordoned: false,
        },
    );
    host.metadata.uid = Some("8f6b8a52-0000-4000-8000-000000000001".into());
    host.metadata.generation = Some(1);
    let cfg = BootstrapConfig {
        namespace: "faubourg-credentials".into(),
        image,
        service_account: "faubourg-bootstrap".into(),
        default_script: Some(ScriptRef {
            kind: ScriptSourceKind::ConfigMap,
            name: "faubourg-install".into(),
            key: DEFAULT_SCRIPT_KEY.into(),
        }),
    };
    let job = bootstrap_job(&host, &cfg)?;
    println!("{}", serde_json::to_string_pretty(&job)?);
    Ok(())
}
