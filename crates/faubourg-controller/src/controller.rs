// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! The Host reconciler's I/O (ADR-0009): read what [`plan`] needs, carry out what it returns.
//! Every decision is in [`plan`]; nothing here decides anything.
//!
//! Logs carry Host, zone and Job names, phases and reasons, never a spec value beyond a name
//! (ADR-0009 decision 14).

use std::sync::Arc;
use std::time::Duration;

use banlieue_api::banlieue::{Provider, ProviderClass};
use faubourg_api::LABEL_HOST;
use faubourg_api::host::Host;
use faubourg_api::policy::{PinnedScript, SCRIPT_SOURCES_KEY, parse_script_sources};
use faubourg_api::validation::is_dns_subdomain;
use faubourg_api::zone::AvailabilityZone;
use futures::StreamExt;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::{ConfigMap, Pod};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use k8s_openapi::jiff::Timestamp;
use kube::api::{DeleteParams, ListParams, Patch, PatchParams, PostParams};
use kube::runtime::controller::Action;
use kube::runtime::reflector::ObjectRef;
use kube::runtime::{Controller, watcher};
use kube::{Api, Client, ResourceExt};
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::job::{CONTAINER, LABEL_COMPONENT, LABEL_MANAGED_BY, LABEL_REBOOTSTRAP};
use crate::plan::{NotPersisted, World, plan};
use crate::provider;

/// The field manager for everything the Host controller writes (ADR-0003, ADR-0009) but a
/// bootstrapped Host's Provider spec, which has its own (ADR-0011, amended).
pub const FIELD_MANAGER: &str = provider::LABELS_MANAGER;
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
    classes: Api<ProviderClass>,
    zones: Api<AvailabilityZone>,
    jobs: Api<Job>,
    pods: Api<Pod>,
    providers: Api<Provider>,
    /// The bootstrap policy's parameters' namespace: read for the one `ConfigMap` (ADR-0013).
    policy: Api<ConfigMap>,
    cfg: Config,
}

/// Runs the Host controller until SIGTERM or SIGINT.
pub async fn run(client: Client, cfg: Config) {
    let ns = cfg.bootstrap.namespace.clone();
    let ctx = Arc::new(Ctx {
        hosts: Api::all(client.clone()),
        classes: Api::all(client.clone()),
        zones: Api::all(client.clone()),
        jobs: Api::namespaced(client.clone(), &ns),
        pods: Api::namespaced(client.clone(), &ns),
        providers: Api::namespaced(client.clone(), &cfg.bootstrap.provider.namespace),
        policy: Api::namespaced(client.clone(), &cfg.policy.namespace),
        cfg,
    });
    // One reconcile at a time: the rollout counts re-bootstrap Jobs across Hosts, and two
    // concurrent reconciles could each see the last free slot (ADR-0013 decision 5).
    let controller = Controller::new(ctx.hosts.clone(), watcher::Config::default())
        .with_config(kube::runtime::controller::Config::default().concurrency(1));
    let hosts = controller.store();
    let every_host = hosts.clone();
    let rollout_hosts = hosts.clone();
    let policy_name = format!("metadata.name={}", ctx.cfg.policy.name);
    let bootstrap_jobs = bootstrap_jobs();
    controller
        // Not `owns`: it maps a Job to its owner in the Job's namespace, and a Host is
        // cluster-scoped, so every Job event would miss.
        .watches(
            ctx.jobs.clone(),
            watcher::Config::default().labels(&bootstrap_jobs),
            // A re-bootstrap taking, freeing or failing a slot concerns every Host waiting
            // for one, not only its own (ADR-0013 decision 5).
            move |job: Job| {
                let mut refs: Vec<ObjectRef<Host>> = owner_host(&job)
                    .map(|h| ObjectRef::new(&h))
                    .into_iter()
                    .collect();
                if wakes_every_host(&job) {
                    refs.extend(
                        rollout_hosts
                            .state()
                            .into_iter()
                            .map(|h| ObjectRef::from_obj(&*h)),
                    );
                }
                refs
            },
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
        // A re-pin is a new bootstrap input for every Host using that script (ADR-0013).
        .watches(
            ctx.policy.clone(),
            watcher::Config::default().fields(&policy_name),
            move |_: ConfigMap| {
                every_host
                    .state()
                    .into_iter()
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
            match r {
                // A deleted Host's Jobs and Provider still send events, for a Host that is gone.
                Err(kube::runtime::controller::Error::ObjectNotFound(o)) => {
                    debug!(host = %o.name, "event for a Host that no longer exists");
                }
                Err(e) => warn!(error = %e, "reconcile failed"),
                Ok(_) => {}
            }
        })
        .await;
}

/// True for a re-bootstrap Job, whose every change can hold or release another Host.
fn wakes_every_host(job: &Job) -> bool {
    job.metadata
        .labels
        .as_ref()
        .and_then(|l| l.get(LABEL_REBOOTSTRAP))
        .is_some_and(|v| v == "true")
}

/// The label selector of every Job the controller makes.
fn bootstrap_jobs() -> String {
    format!(
        "{LABEL_MANAGED_BY}={},{LABEL_COMPONENT}={CONTAINER}",
        faubourg_api::PROJECT
    )
}

/// The script pins in the bootstrap policy's parameters; `None` when they are missing or
/// malformed, which fails every bootstrapped Host closed (ADR-0013 decision 2).
fn pins(params: Option<ConfigMap>) -> Option<Vec<PinnedScript>> {
    let Some(sources) = params
        .and_then(|cm| cm.data)
        .and_then(|mut d| d.remove(SCRIPT_SOURCES_KEY))
    else {
        warn!("the bootstrap policy's parameters have no scriptSources");
        return None;
    };
    match parse_script_sources(&sources) {
        Ok(p) => Some(p),
        Err(e) => {
            warn!(error = %e, "the bootstrap policy's scriptSources is refused");
            None
        }
    }
}

/// The Host controlling a Job, by name: a Host has no namespace.
fn owner_host(job: &Job) -> Option<String> {
    job.metadata
        .owner_references
        .iter()
        .flatten()
        .find(|r| {
            r.controller == Some(true)
                && r.kind == "Host"
                && r.api_version == format!("{}/{}", faubourg_api::GROUP, faubourg_api::VERSION)
        })
        .map(|r| r.name.clone())
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
    // Every bootstrap Job, for the rollout: how many re-bootstraps run, and whether one failed.
    let fleet = ctx
        .jobs
        .list(&ListParams::default().labels(&bootstrap_jobs()))
        .await?
        .items;
    let pods = ctx.pods.list(&by_host).await?.items;
    let providers = ctx.providers.list(&by_host).await?.items;
    let own_provider = ctx.providers.get_opt(&name).await?;
    let class = ctx
        .classes
        .get_opt(&ctx.cfg.bootstrap.provider.class)
        .await?;
    let pins = pins(ctx.policy.get_opt(&ctx.cfg.policy.name).await?);
    let now = Time(Timestamp::now());

    let p = plan(&World {
        host: &host,
        zone: zone.as_ref(),
        jobs: &jobs,
        pods: &pods,
        providers: &providers,
        own_provider: own_provider.as_ref(),
        class: class.as_ref(),
        cfg: &ctx.cfg.bootstrap,
        pins: pins.as_deref(),
        fleet: &fleet,
        now: &now,
    })?;

    // The Provider first: banlieue gives it its identity, and only then is a Job created. Its
    // spec before its labels, so the labels' apply never leaves a field without a manager.
    if let Some(body) = &p.apply_provider_spec {
        ctx.providers
            .patch(
                &name,
                &PatchParams::apply(provider::SPEC_MANAGER).force(),
                &Patch::Apply(body),
            )
            .await?;
        if own_provider.is_none() {
            info!(host = %name, provider = %name, "created the host's Provider");
        }
    }
    if let Some(body) = &p.apply_provider {
        ctx.providers
            .patch(
                &name,
                &PatchParams::apply(FIELD_MANAGER).force(),
                &Patch::Apply(body),
            )
            .await?;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn job(owners: &serde_json::Value) -> Job {
        serde_json::from_value(json!({
            "metadata": { "name": "bootstrap-hv01-g1", "namespace": "faubourg-credentials",
                          "ownerReferences": owners }
        }))
        .unwrap()
    }

    #[test]
    fn a_job_maps_to_its_cluster_scoped_host() {
        let j = job(
            &json!([{ "apiVersion": "faubourg.firestoned.io/v1alpha1", "kind": "Host",
                              "name": "hv01", "uid": "u", "controller": true }]),
        );
        assert_eq!(owner_host(&j).as_deref(), Some("hv01"));
        let r: ObjectRef<Host> = ObjectRef::new("hv01");
        assert_eq!(r.namespace, None, "a Host has no namespace");
    }

    #[test]
    fn only_a_rebootstrap_job_wakes_every_host() {
        let mut j = job(&json!([]));
        assert!(
            !wakes_every_host(&j),
            "a first bootstrap concerns its own Host"
        );
        j.metadata.labels = Some(
            [(LABEL_REBOOTSTRAP.to_string(), "true".to_string())]
                .into_iter()
                .collect(),
        );
        assert!(
            wakes_every_host(&j),
            "a re-bootstrap starting, finishing, failing or going frees or takes a slot"
        );
        j.metadata.labels = Some(
            [(LABEL_REBOOTSTRAP.to_string(), "false".to_string())]
                .into_iter()
                .collect(),
        );
        assert!(!wakes_every_host(&j));
    }

    #[test]
    fn a_job_with_another_owner_maps_to_nothing() {
        for owners in [
            json!([]),
            json!([{ "apiVersion": "batch/v1", "kind": "CronJob", "name": "x", "uid": "u",
                     "controller": true }]),
            json!([{ "apiVersion": "faubourg.firestoned.io/v1alpha1", "kind": "Host",
                     "name": "hv01", "uid": "u" }]),
        ] {
            assert_eq!(owner_host(&job(&owners)), None, "{owners}");
        }
    }
}
