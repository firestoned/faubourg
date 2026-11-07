// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! The Host reconciler's I/O (ADR-0009): read what [`plan`] needs, carry out what it returns.
//! Every decision is in [`plan`]; nothing here decides anything.
//!
//! Logs carry Host, zone and Job names, phases and reasons, never a spec value beyond a name
//! (ADR-0009 decision 14).

use std::sync::Arc;
use std::time::Duration;

use banlieue_api::banlieue::Provider;
use faubourg_api::LABEL_HOST;
use faubourg_api::host::Host;
use faubourg_api::validation::is_dns_subdomain;
use faubourg_api::zone::AvailabilityZone;
use futures::StreamExt;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use k8s_openapi::jiff::Timestamp;
use kube::api::{DeleteParams, ListParams, Patch, PatchParams, PostParams};
use kube::runtime::controller::Action;
use kube::runtime::reflector::ObjectRef;
use kube::runtime::{Controller, watcher};
use kube::{Api, Client, ResourceExt};
use tracing::{info, warn};

use crate::config::Config;
use crate::job::{CONTAINER, LABEL_COMPONENT, LABEL_MANAGED_BY};
use crate::plan::{NotPersisted, World, plan};

/// The field manager for everything the Host controller writes (ADR-0003, ADR-0009).
pub const FIELD_MANAGER: &str = "faubourg.firestoned.io/host-controller";
/// Re-read every Host this often even with no event, as a safety net.
pub const RESYNC: Duration = Duration::from_secs(600);
/// Retry a failed reconcile after this long.
pub const RETRY: Duration = Duration::from_secs(30);

/// Why a reconcile failed. Carries names and API errors, never a spec value.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A call to the API server failed.
    #[error("kube API: {0}")]
    Kube(#[from] kube::Error),
    /// The Host was not read from an API server.
    #[error(transparent)]
    NotPersisted(#[from] NotPersisted),
    /// The name of the Host's next Job is taken by a Job that is not its current one.
    #[error("a Job named {0} already exists in the credentials namespace")]
    JobNameTaken(String),
}

struct Ctx {
    hosts: Api<Host>,
    zones: Api<AvailabilityZone>,
    jobs: Api<Job>,
    pods: Api<Pod>,
    providers: Api<Provider>,
    cfg: Config,
}

/// Runs the Host controller until SIGTERM or SIGINT.
pub async fn run(client: Client, cfg: Config) {
    let ns = cfg.bootstrap.namespace.clone();
    let ctx = Arc::new(Ctx {
        hosts: Api::all(client.clone()),
        zones: Api::all(client.clone()),
        jobs: Api::namespaced(client.clone(), &ns),
        pods: Api::namespaced(client.clone(), &ns),
        providers: Api::namespaced(client.clone(), &cfg.provider_namespace),
        cfg,
    });
    let controller = Controller::new(ctx.hosts.clone(), watcher::Config::default());
    let hosts = controller.store();
    let bootstrap_jobs = format!(
        "{LABEL_MANAGED_BY}={},{LABEL_COMPONENT}={CONTAINER}",
        faubourg_api::PROJECT
    );
    controller
        .owns(
            ctx.jobs.clone(),
            watcher::Config::default().labels(&bootstrap_jobs),
        )
        .watches(
            ctx.zones.clone(),
            watcher::Config::default(),
            move |zone: AvailabilityZone| {
                let name = zone.name_any();
                hosts
                    .state()
                    .into_iter()
                    .filter(|h| h.spec.zone_ref == name)
                    .map(|h| ObjectRef::from_obj(&*h))
                    .collect::<Vec<_>>()
            },
        )
        .watches(
            ctx.providers.clone(),
            watcher::Config::default().labels(LABEL_HOST),
            |p: Provider| p.labels().get(LABEL_HOST).map(|h| ObjectRef::new(h)),
        )
        .shutdown_on_signal()
        .run(reconcile, error_policy, ctx)
        .for_each(|r| async move {
            if let Err(e) = r {
                warn!(error = %e, "reconcile failed");
            }
        })
        .await;
}

async fn reconcile(host: Arc<Host>, ctx: Arc<Ctx>) -> Result<Action, Error> {
    let name = host.name_any();
    let by_host = ListParams::default().labels(&format!("{LABEL_HOST}={name}"));
    // An invalid zoneRef is reported by validation; never send it to the API server.
    let zone = if is_dns_subdomain(&host.spec.zone_ref) {
        ctx.zones.get_opt(&host.spec.zone_ref).await?
    } else {
        None
    };
    let jobs = ctx.jobs.list(&by_host).await?.items;
    let pods = ctx.pods.list(&by_host).await?.items;
    let providers = ctx.providers.list(&by_host).await?.items;
    let now = Time(Timestamp::now());

    let p = plan(&World {
        host: &host,
        zone: zone.as_ref(),
        jobs: &jobs,
        pods: &pods,
        providers: &providers,
        cfg: &ctx.cfg.bootstrap,
        now: &now,
    })?;

    for job in &p.delete {
        match ctx.jobs.delete(job, &DeleteParams::background()).await {
            Ok(_) => info!(host = %name, job = %job, "deleted stale bootstrap Job"),
            Err(kube::Error::Api(s)) if s.code == 404 => {}
            Err(e) => return Err(e.into()),
        }
    }
    if let Some(job) = &p.create {
        let job_name = job.name_any();
        match ctx.jobs.create(&PostParams::default(), job).await {
            Ok(_) => info!(host = %name, job = %job_name, "created bootstrap Job"),
            Err(kube::Error::Api(s)) if s.code == 409 => {
                return Err(Error::JobNameTaken(job_name));
            }
            Err(e) => return Err(e.into()),
        }
    }
    if p.status_changed {
        let patch = serde_json::json!({
            "apiVersion": format!("{}/{}", faubourg_api::GROUP, faubourg_api::VERSION),
            "kind": "Host",
            "status": p.status,
        });
        ctx.hosts
            .patch_status(
                &name,
                &PatchParams::apply(FIELD_MANAGER).force(),
                &Patch::Apply(&patch),
            )
            .await?;
        info!(
            host = %name,
            zone = %host.spec.zone_ref,
            phase = ?p.decision.phase,
            reason = p.decision.reason,
            "phase"
        );
    }
    Ok(Action::requeue(RESYNC))
}

#[allow(clippy::needless_pass_by_value)] // the signature Controller::run requires
fn error_policy(host: Arc<Host>, e: &Error, _ctx: Arc<Ctx>) -> Action {
    warn!(host = %host.name_any(), error = %e, "reconcile failed; retrying");
    Action::requeue(RETRY)
}
