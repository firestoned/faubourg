// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! One reconcile of a Host, as a pure function (ADR-0009 decision 2).
//!
//! The reconciler reads the Host, its zone, the Jobs and pods labelled with its name, and the
//! Providers labelled with its name. [`plan`] returns what to do: the Job to create, the Jobs
//! to delete, and the status to write. The I/O around it does nothing else.
//!
//! - Only Jobs the Host controls count (ADR-0009 decision 3).
//! - The Host's current Job is its newest owned Job built from its current bootstrap inputs.
//!   Every other owned Job is stale and deleted (decision 6).
//! - A Job is created only when there is no current Job and the phase machine is waiting for
//!   one: a valid spec in a supported zone. A failed current Job is kept, never replaced
//!   (decision 7).

use banlieue_api::banlieue::Provider;
use faubourg_api::host::{Host, HostPhase, HostStatus, REASON_INVALID_NAME};
use faubourg_api::zone::AvailabilityZone;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};

use crate::health::ProviderHealth;
use crate::job::{
    BootstrapConfig, JobError, bootstrap_job, job_generation, job_inputs, job_state, owned_by,
};
use crate::phase::{Decision, JobState, Observed, ZoneState, decide};

/// The Host condition the reconciler writes.
pub const CONDITION_READY: &str = "Ready";
/// Reason: neither the Host nor the controller names a script source.
pub const REASON_NO_SCRIPT_SOURCE: &str = "NoScriptSource";
/// Reason: the controller's bootstrap image is not pinned by digest.
pub const REASON_IMAGE_NOT_PINNED: &str = "ImageNotPinned";

/// Everything one reconcile reads.
#[derive(Clone, Copy, Debug)]
pub struct World<'a> {
    /// The Host, as read from the API server.
    pub host: &'a Host,
    /// Its `AvailabilityZone`, if one of that name exists.
    pub zone: Option<&'a AvailabilityZone>,
    /// Jobs in the credentials namespace labelled with the Host's name, whoever made them.
    pub jobs: &'a [Job],
    /// Pods in the credentials namespace labelled with the Host's name.
    pub pods: &'a [Pod],
    /// Providers in banlieue's namespace labelled with the Host's name.
    pub providers: &'a [Provider],
    /// The controller's configuration.
    pub cfg: &'a BootstrapConfig,
    /// The time to stamp on a condition that changes.
    pub now: &'a Time,
}

/// What one reconcile does.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// The Job to create, if any.
    pub create: Option<Job>,
    /// Names of the Host's stale Jobs, to delete.
    pub delete: Vec<String>,
    /// The Host's status after this reconcile.
    pub status: HostStatus,
    /// False when `status` equals what the Host already carries: nothing to write.
    pub status_changed: bool,
    /// The phase decision `status` was built from.
    pub decision: Decision,
}

/// Why no plan was made: the Host was not read from an API server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the Host has no {0}")]
pub struct NotPersisted(pub &'static str);

/// Plans one reconcile of a Host.
pub fn plan(w: &World<'_>) -> Result<Plan, NotPersisted> {
    let host = w.host;
    let uid = host.metadata.uid.as_deref().ok_or(NotPersisted("uid"))?;
    let generation = host.metadata.generation.ok_or(NotPersisted("generation"))?;

    let owned: Vec<&Job> = w.jobs.iter().filter(|j| owned_by(j, uid)).collect();
    let (desired, refusal) = match bootstrap_job(host, w.cfg) {
        Ok(job) => (Some(job), None),
        // The phase machine reports these itself.
        Err(JobError::NotScript | JobError::InvalidSpec(_)) => (None, None),
        Err(e) => (None, Some(refused(&e))),
    };
    let want = desired.as_ref().and_then(job_inputs);
    let current = owned
        .iter()
        .copied()
        .filter(|j| want.is_some() && job_inputs(j) == want)
        .max_by_key(|j| job_generation(j));
    let job = current.map_or(JobState::Absent, |j| job_state(j, w.pods));
    let zone = zone_state(w.zone);
    let provider = ProviderHealth::of_labelled(w.providers);
    let previous = host.status.as_ref().and_then(|s| s.phase);

    let decision = refusal.unwrap_or_else(|| {
        decide(&Observed {
            spec: &host.spec,
            previous,
            zone,
            job,
            provider,
        })
    });

    let current_name = current.and_then(|j| j.metadata.name.as_deref());
    let mut delete: Vec<String> = owned
        .iter()
        .filter_map(|j| j.metadata.name.as_deref())
        .filter(|n| Some(*n) != current_name)
        .map(str::to_string)
        .collect();
    delete.sort();
    delete.dedup();

    // A stale Job holding the name the new one needs is deleted first; the new one is created
    // on the next reconcile, which that deletion triggers.
    let create = desired.filter(|d| {
        job == JobState::Absent
            && zone == ZoneState::Supported
            && d.metadata
                .name
                .as_deref()
                .is_some_and(|n| !delete.iter().any(|x| x == n))
    });

    let provider_ref = match w.providers {
        [one] => one.metadata.name.clone(),
        _ => None,
    };
    let status = status(
        host.status.as_ref(),
        &decision,
        generation,
        provider_ref,
        w.now,
    );
    let status_changed = host.status.as_ref() != Some(&status);
    Ok(Plan {
        create,
        delete,
        status,
        status_changed,
        decision,
    })
}

/// The zone's state: missing, refused by its own validation, or supported.
pub fn zone_state(zone: Option<&AvailabilityZone>) -> ZoneState {
    match zone {
        None => ZoneState::NotFound,
        Some(z) if z.spec.validate().is_ok() => ZoneState::Supported,
        Some(_) => ZoneState::Unsupported,
    }
}

/// A Host the Job builder refuses for a reason the phase machine does not know.
fn refused(e: &JobError) -> Decision {
    let reason = match e {
        JobError::NameTooLong => REASON_INVALID_NAME,
        JobError::NoScriptSource => REASON_NO_SCRIPT_SOURCE,
        JobError::ImageNotPinned => REASON_IMAGE_NOT_PINNED,
        // Not reachable from plan(), which checks these first or leaves them to decide().
        JobError::NotPersisted(_) | JobError::NotScript | JobError::InvalidSpec(_) => {
            "BootstrapRefused"
        }
    };
    Decision {
        phase: HostPhase::Failed,
        reason,
        message: e.to_string(),
    }
}

/// The status a decision makes. Fields the reconciler does not decide are carried over.
fn status(
    previous: Option<&HostStatus>,
    d: &Decision,
    generation: i64,
    provider_ref: Option<String>,
    now: &Time,
) -> HostStatus {
    let ready = if d.phase == HostPhase::Ready {
        "True"
    } else {
        "False"
    };
    let last = previous
        .into_iter()
        .flat_map(|s| s.conditions.iter())
        .find(|c| c.type_ == CONDITION_READY);
    let last_transition_time = match last {
        Some(c) if c.status == ready => c.last_transition_time.clone(),
        _ => now.clone(),
    };
    HostStatus {
        phase: Some(d.phase),
        provider_ref,
        accepted_host_key: previous.and_then(|s| s.accepted_host_key.clone()),
        conditions: vec![Condition {
            type_: CONDITION_READY.into(),
            status: ready.into(),
            reason: d.reason.into(),
            message: d.message.clone(),
            observed_generation: Some(generation),
            last_transition_time,
        }],
        observed_generation: Some(generation),
        message: Some(d.message.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{ANNOTATION_INPUTS, EXIT_SCRIPT_DRIFT, job_name};
    use crate::phase::{
        REASON_AWAITING_JOB, REASON_JOB_FAILED, REASON_JOB_RUNNING, REASON_PROVIDER_AMBIGUOUS,
        REASON_PROVIDER_HEALTHY, REASON_SCRIPT_DRIFT, REASON_ZONE_NOT_FOUND,
        REASON_ZONE_UNSUPPORTED,
    };
    use faubourg_api::host::{
        BootstrapMethod, BootstrapSpec, DEFAULT_SCRIPT_KEY, HostKeyPolicy, HostSpec, ScriptRef,
        ScriptSourceKind, SecretName, SshSpec,
    };
    use k8s_openapi::jiff::Timestamp;
    use serde_json::json;

    const UID: &str = "8f6b8a52-0000-4000-8000-000000000001";
    const IMAGE: &str = "ghcr.io/firestoned/faubourg-bootstrap@sha256:\
                         0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn cfg() -> BootstrapConfig {
        BootstrapConfig {
            namespace: "faubourg-credentials".into(),
            image: IMAGE.into(),
            service_account: "faubourg-bootstrap".into(),
            default_script: Some(ScriptRef {
                kind: ScriptSourceKind::ConfigMap,
                name: "faubourg-install".into(),
                key: DEFAULT_SCRIPT_KEY.into(),
            }),
        }
    }

    fn host(generation: i64) -> Host {
        let mut h = Host::new(
            "hv01",
            HostSpec {
                zone_ref: "az1".into(),
                address: "hv01.home.example".into(),
                ssh: Some(SshSpec {
                    port: 22,
                    secret_ref: SecretName {
                        name: "hv01-ssh".into(),
                    },
                    placement_secret_ref: None,
                    host_key_policy: HostKeyPolicy::Strict,
                    revoke_after_bootstrap: false,
                }),
                bootstrap: BootstrapSpec::default(),
                cordoned: false,
            },
        );
        h.metadata.uid = Some(UID.into());
        h.metadata.generation = Some(generation);
        h
    }

    fn adopted() -> Host {
        let mut h = host(1);
        h.spec.ssh = None;
        h.spec.bootstrap = BootstrapSpec {
            method: BootstrapMethod::None,
            script_ref: None,
        };
        h
    }

    fn zone(mode: &str) -> AvailabilityZone {
        serde_json::from_value(json!({
            "apiVersion": "faubourg.firestoned.io/v1alpha1",
            "kind": "AvailabilityZone",
            "metadata": { "name": "az1" },
            "spec": { "storage": { "mode": mode } }
        }))
        .unwrap()
    }

    fn at(second: i64) -> Time {
        Time(Timestamp::from_second(second).unwrap())
    }

    /// The Job the controller would have created for `h`, as the API server returns it.
    fn job_for(h: &Host, uid: &str, conditions: &[&str]) -> Job {
        let mut j = bootstrap_job(h, &cfg()).unwrap();
        j.metadata.uid = Some(uid.into());
        let mut v = serde_json::to_value(&j).unwrap();
        v["status"] = json!({
            "conditions": conditions.iter().map(|t| json!({"type": t, "status": "True"})).collect::<Vec<_>>()
        });
        serde_json::from_value(v).unwrap()
    }

    fn provider(name: &str, healthy: bool) -> Provider {
        let status = if healthy {
            json!({
                "conditions": [{ "type": "Ready", "status": "True", "observedGeneration": 1,
                    "reason": "R", "message": "", "lastTransitionTime": "2026-11-07T00:00:00Z" }],
                "workload": { "namespace": "banlieue-system", "heartbeat": "Alive" }
            })
        } else {
            json!({})
        };
        serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "Provider",
            "metadata": { "name": name, "namespace": "banlieue-system", "generation": 1,
                "labels": { "faubourg.firestoned.io/host": "hv01" } },
            "spec": {
                "providerClassRef": { "name": "cloud-hypervisor" },
                "connection": { "endpoint": "hv01.home.example" }
            },
            "status": status
        }))
        .unwrap()
    }

    struct Fixture {
        host: Host,
        zone: Option<AvailabilityZone>,
        jobs: Vec<Job>,
        pods: Vec<Pod>,
        providers: Vec<Provider>,
        cfg: BootstrapConfig,
        now: Time,
    }

    impl Fixture {
        fn new(host: Host) -> Self {
            Self {
                host,
                zone: Some(zone("Local")),
                jobs: vec![],
                pods: vec![],
                providers: vec![],
                cfg: cfg(),
                now: at(1_000),
            }
        }

        fn plan(&self) -> Plan {
            plan(&World {
                host: &self.host,
                zone: self.zone.as_ref(),
                jobs: &self.jobs,
                pods: &self.pods,
                providers: &self.providers,
                cfg: &self.cfg,
                now: &self.now,
            })
            .unwrap()
        }
    }

    fn created_name(p: &Plan) -> Option<&str> {
        p.create.as_ref().and_then(|j| j.metadata.name.as_deref())
    }

    // ----- creating the Job ---------------------------------------------------------------

    #[test]
    fn a_new_host_gets_its_job() {
        let p = Fixture::new(host(1)).plan();
        assert_eq!(created_name(&p), Some("bootstrap-hv01-g1"));
        assert_eq!(p.delete, Vec::<String>::new());
        assert_eq!(p.decision.phase, HostPhase::Pending);
        assert_eq!(p.decision.reason, REASON_AWAITING_JOB);
        assert!(p.status_changed);
    }

    #[test]
    fn no_job_outside_a_supported_zone() {
        let mut f = Fixture::new(host(1));
        f.zone = None;
        let p = f.plan();
        assert_eq!(p.create, None);
        assert_eq!(p.decision.reason, REASON_ZONE_NOT_FOUND);
        f.zone = Some(zone("Shared"));
        let p = f.plan();
        assert_eq!(p.create, None);
        assert_eq!(p.decision.reason, REASON_ZONE_UNSUPPORTED);
    }

    #[test]
    fn a_running_job_is_left_alone() {
        let mut f = Fixture::new(host(1));
        f.jobs = vec![job_for(&f.host, "j1", &[])];
        let p = f.plan();
        assert_eq!(p.create, None);
        assert_eq!(p.delete, Vec::<String>::new());
        assert_eq!(p.decision.reason, REASON_JOB_RUNNING);
    }

    #[test]
    fn a_failed_job_is_kept_and_not_retried() {
        let mut f = Fixture::new(host(1));
        f.jobs = vec![job_for(&f.host, "j1", &["Failed"])];
        let p = f.plan();
        assert_eq!(p.create, None);
        assert_eq!(p.delete, Vec::<String>::new());
        assert_eq!(p.decision.phase, HostPhase::Failed);
        assert_eq!(p.decision.reason, REASON_JOB_FAILED);
    }

    #[test]
    fn a_failed_jobs_cause_comes_from_its_pod() {
        let mut f = Fixture::new(host(1));
        f.jobs = vec![job_for(&f.host, "j1", &["Failed"])];
        f.pods = vec![
            serde_json::from_value(json!({
                "metadata": { "name": "bootstrap-hv01-g1-x", "ownerReferences": [{
                    "apiVersion": "batch/v1", "kind": "Job", "name": "bootstrap-hv01-g1",
                    "uid": "j1", "controller": true }] },
                "status": { "containerStatuses": [{
                    "name": "bootstrap", "image": "x", "imageID": "", "ready": false,
                    "restartCount": 0,
                    "state": { "terminated": { "exitCode": EXIT_SCRIPT_DRIFT } } }] }
            }))
            .unwrap(),
        ];
        assert_eq!(f.plan().decision.reason, REASON_SCRIPT_DRIFT);
    }

    // ----- which Jobs are the Host's ------------------------------------------------------

    #[test]
    fn a_job_the_host_does_not_own_is_ignored_and_never_deleted() {
        let mut f = Fixture::new(host(1));
        let mut squatter = job_for(&f.host, "j1", &["Complete"]);
        squatter.metadata.owner_references = None;
        squatter.metadata.name = Some("bootstrap-hv01-other".into());
        f.jobs = vec![squatter];
        let p = f.plan();
        assert_eq!(
            p.delete,
            Vec::<String>::new(),
            "never delete a Job the Host does not own"
        );
        assert_eq!(created_name(&p), Some("bootstrap-hv01-g1"));
        assert_eq!(p.decision.reason, REASON_AWAITING_JOB);
    }

    #[test]
    fn a_job_of_a_recreated_host_is_not_its_own() {
        let mut f = Fixture::new(host(1));
        let mut old = job_for(&f.host, "j1", &["Complete"]);
        old.metadata.owner_references.as_mut().unwrap()[0].uid = "an-earlier-host".into();
        f.jobs = vec![old];
        let p = f.plan();
        assert_ne!(p.decision.phase, HostPhase::Ready);
        assert_eq!(p.delete, Vec::<String>::new());
    }

    // ----- re-bootstrap (ADR-0009 decision 6) ---------------------------------------------

    #[test]
    fn cordon_and_a_zone_move_run_nothing() {
        let first = host(1);
        let mut f = Fixture::new(host(3));
        f.host.spec.cordoned = true;
        f.jobs = vec![job_for(&first, "j1", &["Complete"])];
        f.providers = vec![provider("hv01", true)];
        let p = f.plan();
        assert_eq!(p.create, None);
        assert_eq!(p.delete, Vec::<String>::new());
        assert_eq!(p.decision.reason, REASON_PROVIDER_HEALTHY);
    }

    #[test]
    fn changed_inputs_replace_the_job() {
        let first = host(1);
        let mut f = Fixture::new(host(2));
        f.host.spec.address = "hv01b.home.example".into();
        f.jobs = vec![job_for(&first, "j1", &[])];
        let p = f.plan();
        assert_eq!(p.delete, vec!["bootstrap-hv01-g1".to_string()]);
        assert_eq!(created_name(&p), Some("bootstrap-hv01-g2"));
        assert_eq!(p.decision.reason, REASON_AWAITING_JOB);
    }

    #[test]
    fn a_stale_job_holding_the_name_is_deleted_before_the_new_one_is_made() {
        let mut f = Fixture::new(host(1));
        let mut stale = job_for(&f.host, "j1", &["Failed"]);
        stale
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert(ANNOTATION_INPUTS.into(), "0".repeat(64));
        f.jobs = vec![stale];
        let p = f.plan();
        assert_eq!(p.delete, vec![job_name("hv01", 1)]);
        assert_eq!(p.create, None, "created on the next reconcile");
    }

    #[test]
    fn the_newest_matching_job_is_current_and_older_ones_go() {
        let mut f = Fixture::new(host(4));
        let older = job_for(&host(2), "j2", &["Failed"]);
        let newer = job_for(&host(3), "j3", &["Complete"]);
        f.jobs = vec![newer, older];
        let p = f.plan();
        assert_eq!(p.delete, vec!["bootstrap-hv01-g2".to_string()]);
        assert_eq!(p.create, None);
        assert_eq!(p.decision.phase, HostPhase::Bootstrapping);
    }

    #[test]
    fn an_adopted_host_has_no_job_and_its_old_ones_go() {
        let mut f = Fixture::new(adopted());
        f.jobs = vec![job_for(&host(1), "j1", &[])];
        let p = f.plan();
        assert_eq!(p.create, None);
        assert_eq!(p.delete, vec!["bootstrap-hv01-g1".to_string()]);
    }

    #[test]
    fn an_invalid_spec_has_no_job_and_its_old_ones_go() {
        let mut f = Fixture::new(host(2));
        f.host.spec.address = "-oProxyCommand=sh".into();
        f.jobs = vec![job_for(&host(1), "j1", &[])];
        let p = f.plan();
        assert_eq!(p.create, None);
        assert_eq!(p.delete.len(), 1);
        assert_eq!(p.decision.phase, HostPhase::Failed);
    }

    #[test]
    fn no_script_source_fails_the_host_with_its_reason() {
        let mut f = Fixture::new(host(1));
        f.cfg.default_script = None;
        let p = f.plan();
        assert_eq!(p.create, None);
        assert_eq!(p.decision.phase, HostPhase::Failed);
        assert_eq!(p.decision.reason, REASON_NO_SCRIPT_SOURCE);
    }

    // ----- the Provider -------------------------------------------------------------------

    #[test]
    fn an_adopted_host_is_ready_from_its_labelled_provider() {
        let mut f = Fixture::new(adopted());
        f.providers = vec![provider("hv01", true)];
        let p = f.plan();
        assert_eq!(p.decision.phase, HostPhase::Ready);
        assert_eq!(p.status.provider_ref.as_deref(), Some("hv01"));
        let ready = &p.status.conditions[0];
        assert_eq!(ready.type_, CONDITION_READY);
        assert_eq!(ready.status, "True");
    }

    #[test]
    fn two_labelled_providers_are_never_picked_from() {
        let mut f = Fixture::new(adopted());
        f.providers = vec![provider("hv01", true), provider("hv01-copy", true)];
        let p = f.plan();
        assert_ne!(p.decision.phase, HostPhase::Ready);
        assert_eq!(p.decision.reason, REASON_PROVIDER_AMBIGUOUS);
        assert_eq!(p.status.provider_ref, None);
    }

    // ----- status -------------------------------------------------------------------------

    #[test]
    fn an_unchanged_status_is_not_written_again() {
        let mut f = Fixture::new(host(1));
        let first = f.plan();
        f.host.status = Some(first.status);
        f.now = at(2_000);
        let second = f.plan();
        assert!(!second.status_changed);
        assert_eq!(second.status.conditions[0].last_transition_time, at(1_000));
    }

    #[test]
    fn the_transition_time_moves_only_when_ready_flips() {
        let mut f = Fixture::new(adopted());
        let pending = f.plan();
        f.host.status = Some(pending.status);
        f.now = at(2_000);
        f.zone = None;
        let still_not_ready = f.plan();
        assert!(still_not_ready.status_changed, "the reason changed");
        assert_eq!(
            still_not_ready.status.conditions[0].last_transition_time,
            at(1_000)
        );
        f.zone = Some(zone("Local"));
        f.providers = vec![provider("hv01", true)];
        f.now = at(3_000);
        let ready = f.plan();
        assert_eq!(ready.status.conditions[0].status, "True");
        assert_eq!(ready.status.conditions[0].last_transition_time, at(3_000));
    }

    #[test]
    fn status_records_the_generation_and_keeps_the_accepted_host_key() {
        let mut f = Fixture::new(host(5));
        f.host.status = Some(HostStatus {
            accepted_host_key: Some("SHA256:<fingerprint>".into()),
            ..HostStatus::default()
        });
        let p = f.plan();
        assert_eq!(p.status.observed_generation, Some(5));
        assert_eq!(p.status.conditions[0].observed_generation, Some(5));
        assert_eq!(
            p.status.accepted_host_key.as_deref(),
            Some("SHA256:<fingerprint>")
        );
        assert_eq!(
            p.status.message.as_deref(),
            Some(p.decision.message.as_str())
        );
    }

    #[test]
    fn a_host_never_read_from_the_api_server_has_no_plan() {
        let mut h = host(1);
        h.metadata.uid = None;
        let f = Fixture::new(h);
        let r = plan(&World {
            host: &f.host,
            zone: f.zone.as_ref(),
            jobs: &f.jobs,
            pods: &f.pods,
            providers: &f.providers,
            cfg: &f.cfg,
            now: &f.now,
        });
        assert_eq!(r, Err(NotPersisted("uid")));
    }
}
