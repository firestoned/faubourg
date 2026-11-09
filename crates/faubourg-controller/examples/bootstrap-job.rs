// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! Prints, as JSON, the bootstrap Job the builder makes for a fixture Host, so
//! `scripts/vap-test.sh` can submit the controller's real output to a real API server.
//!
//! Usage: `FAUBOURG_BOOTSTRAP_IMAGE=<image@sha256:...> FAUBOURG_SCRIPT_SOURCES=<scriptSources>
//! cargo run -q -p faubourg-controller --example bootstrap-job`
//!
//! The image and the script pins (the bootstrap policy's `scriptSources`, ADR-0013) come from
//! the environment, not from `argv`. The builder refuses an image not pinned by digest and a
//! script with no pinned digest, so nothing here trusts either value.

use faubourg_api::host::{
    BootstrapSpec, DEFAULT_SCRIPT_KEY, Host, HostKeyPolicy, HostSpec, ScriptRef, ScriptSourceKind,
    SecretName, SshSpec,
};
use faubourg_api::policy::parse_policy;
use faubourg_controller::job::{
    BootstrapConfig, ProviderConfig, bootstrap_job, with_provider_identity,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let image = std::env::var("FAUBOURG_BOOTSTRAP_IMAGE")
        .map_err(|_| "set FAUBOURG_BOOTSTRAP_IMAGE to an image pinned by digest")?;
    let pins = parse_policy(
        &std::env::var("FAUBOURG_SCRIPT_SOURCES")
            .map_err(|_| "set FAUBOURG_SCRIPT_SOURCES to the policy's scriptSources")?,
        std::env::var("FAUBOURG_PROVIDER_CA_SOURCE").ok().as_deref(),
    )?;
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
        provider: ProviderConfig {
            class: "cloud-hypervisor".into(),
            namespace: "banlieue-system".into(),
            api_server: "https://kube.home.example:6443".into(),
        },
        max_rebootstraps: 1,
    };
    // The name banlieue's operator would publish on the Provider (banlieue ADR-0126).
    let job = with_provider_identity(
        bootstrap_job(&host, &cfg, Some(&pins)).map_err(|e| e.to_string())?,
        "banlieue-provider-cloud-hypervisor-hv01",
    );
    println!("{}", serde_json::to_string_pretty(&job)?);
    Ok(())
}
