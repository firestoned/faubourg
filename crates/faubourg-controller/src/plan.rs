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

use banlieue_api::banlieue::{Provider, ProviderClass, ProviderDeployment};
use faubourg_api::host::{
    BootstrapMethod, Host, HostKeyPolicy, HostPhase, HostStatus, REASON_INVALID_NAME,
};
use faubourg_api::policy::Policy;
use faubourg_api::zone::AvailabilityZone;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};

use crate::backend::{
    Backend, REASON_BACKEND_UNSUPPORTED, REASON_CLASS_MISMATCH, REASON_CONNECTION_INVALID,
    class_name,
};
use crate::drain::{CONDITION_DRAINED, Drained};
use crate::health::ProviderHealth;
use crate::job::{
    BootstrapConfig, JobError, LABEL_REBOOTSTRAP, bootstrap_job_for, job_generation, job_inputs,
    job_state, owned_by, reported_host_key, with_provider_identity,
};
use crate::phase::{Decision, JobState, Observed, REASON_AWAITING_JOB, ZoneState, decide};
use crate::provider::{
    REASON_AWAITING_IDENTITY, identity_ready, labels_body, migrated_spec_body,
    provider_service_account, spec_body,
};

/// The Host condition the reconciler writes.
pub const CONDITION_READY: &str = "Ready";
/// The Host condition saying whether it runs its current bootstrap inputs (ADR-0013).
pub const CONDITION_BOOTSTRAP_CURRENT: &str = "BootstrapCurrent";
/// Reason: the last successful bootstrap was under the current inputs.
pub const REASON_BOOTSTRAP_CURRENT: &str = "Current";
/// Reason: the current inputs' Job has not succeeded yet.
pub const REASON_BOOTSTRAP_PENDING: &str = "BootstrapPending";
/// Reason: a re-bootstrap waits for a rollout slot.
pub const REASON_ROLLOUT_WAITING: &str = "RolloutWaiting";
/// Reason: another Host's re-bootstrap failed, so no re-bootstrap starts.
pub const REASON_ROLLOUT_HALTED: &str = "RolloutHalted";
/// Reason: neither the Host nor the controller names a script source.
pub const REASON_NO_SCRIPT_SOURCE: &str = "NoScriptSource";
/// Reason: the controller's bootstrap image is not pinned by digest.
pub const REASON_IMAGE_NOT_PINNED: &str = "ImageNotPinned";
/// Reason: the bootstrap policy pins no digest for the Host's script (ADR-0013 decision 2).
pub const REASON_SCRIPT_NOT_PINNED: &str = "ScriptNotPinned";
/// Reason: another Host has the same literal address (ADR-0018 decision 10).
pub const REASON_DUPLICATE_ADDRESS: &str = "DuplicateAddress";
/// Reason: the API server refused faubourg's write of the host's Provider, at admission or by
/// RBAC; the message is the API server's (it names objects, never a Secret's content).
pub const REASON_PROVIDER_APPLY_REFUSED: &str = "ProviderApplyRefused";
/// At most this much of the API server's refusal is kept in the Host's status.
const MAX_REFUSAL_MESSAGE: usize = 512;

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
    /// The Provider named after the Host, labelled or not (ADR-0011 decision 1).
    pub own_provider: Option<&'a Provider>,
    /// The zone's `ProviderClass` (or the configured default's), if it exists (ADR-0018).
    pub class: Option<&'a ProviderClass>,
    /// The controller's configuration.
    pub cfg: &'a BootstrapConfig,
    /// What the bootstrap policy pins (scripts, and the provider CA); `None` when its
    /// parameters are missing or malformed (ADR-0013 decision 2, ADR-0016).
    pub pins: Option<&'a Policy>,
    /// Every bootstrap Job in the credentials namespace, for the rollout (ADR-0013 decision 5).
    pub fleet: &'a [Job],
    /// Every Host, for `DuplicateAddress` (ADR-0018 decision 10). The Host itself may be in it.
    pub peers: &'a [Host],
    /// The time to stamp on a condition that changes.
    pub now: &'a Time,
    /// The drain's `Drained` condition, while the Host is draining (ADR-0017 decision 4).
    pub drained: Option<&'a Drained>,
    /// Why the API server refused this pass's write of the host's Provider, when it did: the
    /// reconcile is planned again with it, so the Host says so instead of retrying in silence.
    pub provider_refused: Option<&'a str>,
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
    /// The labels' apply body for the host's Provider, under `LABELS_MANAGER`, if one is to
    /// be applied (ADR-0011 decisions 2 and 3).
    pub apply_provider: Option<serde_json::Value>,
    /// The spec's apply body, under `SPEC_MANAGER`, applied before the labels: a bootstrapped
    /// Host's class and endpoint, or an existing spec moved off the labels' manager
    /// (ADR-0011, amended).
    pub apply_provider_spec: Option<serde_json::Value>,
    /// The backend that drives the Host, when its zone's class is one faubourg drives
    /// (ADR-0018). The drain reads it: only some backends' VMs can be moved (decision 9).
    pub backend: Option<Backend>,
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

    let class = class_name(w.zone, &w.cfg.provider.class);
    let backend = Backend::of(class, w.class);
    // Only a provider that runs on the host gets a token, and so needs banlieue to have given
    // its Provider an identity first (ADR-0011 decision 2, ADR-0018 decision 7).
    let mints = backend.as_ref().map_or(true, |b| b.mints_token());
    let owned: Vec<&Job> = w.jobs.iter().filter(|j| owned_by(j, uid)).collect();
    let (desired, refusal) = match bootstrap_job_for(host, w.cfg, w.pins, mints) {
        Ok(job) => (Some(job), None),
        // The phase machine reports these itself.
        Err(JobError::NotScript | JobError::InvalidSpec(_)) => (None, None),
        Err(e) => (None, Some(refused(&e))),
    };
    let want = desired.as_ref().and_then(job_inputs).map(str::to_string);
    let current = owned
        .iter()
        .copied()
        .filter(|j| want.is_some() && job_inputs(j) == want.as_deref())
        .max_by_key(|j| job_generation(j));
    let job = current.map_or(JobState::Absent, |j| job_state(j, w.pods));
    let Progress {
        bootstrapped_inputs,
        rebootstrap,
        hold,
    } = progress(w, uid, &owned, want.as_deref(), job);
    let accepted_host_key = accepted_host_key(host, current, job, w.pods);
    let zone = zone_state(w.zone);
    let script = host.spec.bootstrap.method == BootstrapMethod::Script;
    let valid = host.spec.validate().is_ok();
    let identity = !mints || identity_ready(w.own_provider);
    // A held re-bootstrap keeps running what it has: judge it by its last success.
    let judged = if hold.is_some() {
        JobState::Succeeded
    } else {
        job
    };
    let (refusal, decision) = phase(w, refusal, judged, zone, identity, class, &backend);
    let (apply_provider_spec, apply_provider) = match (&backend, valid && refusal.is_none()) {
        (Ok(b), true) => provider_bodies(w, script, *b, class),
        _ => (None, None),
    };

    // A Job that succeeded may be the only record of that success until the status, as read
    // back, holds its inputs: deleting it sooner turns the next reconcile's re-bootstrap into
    // a first bootstrap, which the rollout does not hold (ADR-0013 decision 5).
    let recorded = host
        .status
        .as_ref()
        .is_some_and(|s| s.bootstrapped_inputs.is_some());
    let delete: Vec<String> = stale(&owned, current)
        .into_iter()
        .filter(|name| {
            recorded
                || !owned.iter().any(|j| {
                    j.metadata.name.as_deref() == Some(name.as_str())
                        && job_state(j, w.pods) == JobState::Succeeded
                })
        })
        .collect();

    // A Job holding the name the new one needs goes first (now, or once its success is
    // recorded); the new one is created on a later reconcile, which that deletion triggers.
    let create = desired.filter(|d| {
        hold.is_none()
            && job == JobState::Absent
            && zone == ZoneState::Supported
            && refusal.is_none()
            && identity
            && d.metadata
                .name
                .as_deref()
                .is_some_and(|n| !owned.iter().any(|j| j.metadata.name.as_deref() == Some(n)))
    });

    let provider_ref = match w.providers {
        [one] => one.metadata.name.clone(),
        _ => None,
    };
    let create = create.and_then(|j| finish(j, w.own_provider, rebootstrap, mints));
    let bootstrap = (script && want.is_some()).then_some(bootstrap_condition(job, hold));
    let status = status(
        host.status.as_ref(),
        &decision,
        generation,
        Recorded {
            provider_ref,
            accepted_host_key,
            bootstrapped_inputs,
            bootstrap,
            drained: w.drained.cloned(),
        },
        w.now,
    );
    let status_changed = host.status.as_ref() != Some(&status);
    Ok(Plan {
        create,
        delete,
        status,
        status_changed,
        decision,
        apply_provider,
        apply_provider_spec,
        backend: backend.ok(),
    })
}

/// A first-use key is pinned from the Host's own succeeded Job (ADR-0010 decision 8), and a
/// pinned one is never replaced.
fn accepted_host_key(
    host: &Host,
    current: Option<&Job>,
    job: JobState,
    pods: &[Pod],
) -> Option<String> {
    let first_use = host
        .spec
        .ssh
        .as_ref()
        .is_some_and(|s| s.host_key_policy == HostKeyPolicy::TrustOnFirstUse);
    let pinned = host
        .status
        .as_ref()
        .and_then(|s| s.accepted_host_key.clone());
    match (pinned, current) {
        (Some(fp), _) => Some(fp),
        (None, Some(j)) if first_use && job == JobState::Succeeded => reported_host_key(j, pods),
        (None, _) => None,
    }
}

/// Every owned Job but the current one, by name, sorted.
fn stale(owned: &[&Job], current: Option<&Job>) -> Vec<String> {
    let current_name = current.and_then(|j| j.metadata.name.as_deref());
    let mut delete: Vec<String> = owned
        .iter()
        .filter_map(|j| j.metadata.name.as_deref())
        .filter(|n| Some(*n) != current_name)
        .map(str::to_string)
        .collect();
    delete.sort();
    delete.dedup();
    delete
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
        JobError::ScriptNotPinned(_) => REASON_SCRIPT_NOT_PINNED,
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

/// The refusal, if any, and the phase decision (ADR-0009, ADR-0011 decision 2, ADR-0018).
fn phase(
    w: &World<'_>,
    refusal: Option<Decision>,
    job: JobState,
    zone: ZoneState,
    identity: bool,
    class: &str,
    backend: &Result<Backend, String>,
) -> (Option<Decision>, Decision) {
    let host = w.host;
    let script = host.spec.bootstrap.method == BootstrapMethod::Script;
    let valid = host.spec.validate().is_ok();
    // Before anything is created or labelled for a Host: no other Host at its address, a class
    // faubourg drives, and no Provider of another class already there.
    let refusal = refusal.or_else(|| {
        if !valid {
            return None;
        }
        w.provider_refused
            .map(|why| Decision {
                phase: HostPhase::Failed,
                reason: REASON_PROVIDER_APPLY_REFUSED,
                message: format!(
                    "the API server refused the host's Provider: {}",
                    truncate(why, MAX_REFUSAL_MESSAGE)
                ),
            })
            .or_else(|| duplicate_address(host, w.peers))
            .or_else(|| {
                backend.as_ref().err().map(|why| Decision {
                    phase: HostPhase::Failed,
                    reason: REASON_BACKEND_UNSUPPORTED,
                    message: why.clone(),
                })
            })
            .or_else(|| {
                backend
                    .as_ref()
                    .ok()
                    .and_then(|b| b.check_zone(w.zone).err())
                    .map(|why| Decision {
                        phase: HostPhase::Failed,
                        reason: REASON_CONNECTION_INVALID,
                        message: why,
                    })
            })
            .or_else(|| class_mismatch(w.own_provider, class))
    });
    let mut decision = refusal.clone().unwrap_or_else(|| {
        decide(&Observed {
            spec: &host.spec,
            previous: host.status.as_ref().and_then(|s| s.phase),
            zone,
            job,
            provider: ProviderHealth::of_labelled_as(
                w.providers,
                backend
                    .as_ref()
                    .map_or(ProviderDeployment::External, |b| b.deployment()),
            ),
        })
    });
    // No Job until banlieue's operator has given the Provider its identity (decision 2).
    if script && decision.reason == REASON_AWAITING_JOB && !identity {
        decision = Decision {
            phase: HostPhase::Pending,
            reason: REASON_AWAITING_IDENTITY,
            message: "waiting for banlieue to give the host's Provider its identity".into(),
        };
    }
    (refusal, decision)
}

/// At most `max` bytes of `s`, cut at a character boundary.
fn truncate(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// `DuplicateAddress`: another Host, not being deleted, has the same `spec.address`, compared
/// literally and case-folded, never resolved (ADR-0018 decision 10). Both are refused.
fn duplicate_address(host: &Host, peers: &[Host]) -> Option<Decision> {
    let mut others: Vec<&str> = peers
        .iter()
        .filter(|p| p.metadata.uid != host.metadata.uid)
        .filter(|p| p.metadata.deletion_timestamp.is_none())
        .filter(|p| p.spec.address.eq_ignore_ascii_case(&host.spec.address))
        .filter_map(|p| p.metadata.name.as_deref())
        .collect();
    if others.is_empty() {
        return None;
    }
    others.sort_unstable();
    Some(Decision {
        phase: HostPhase::Failed,
        reason: REASON_DUPLICATE_ADDRESS,
        message: format!(
            "the same address as Host {}: one machine is one Host",
            others.join(", ")
        ),
    })
}

/// `ProviderClassMismatch`: the Provider named after the Host references another class than its
/// zone's. faubourg never moves a Provider between classes (ADR-0018 decision 3).
fn class_mismatch(own: Option<&Provider>, class: &str) -> Option<Decision> {
    let own = own?;
    let theirs = own.spec.provider_class_ref.name.as_str();
    (theirs != class).then(|| Decision {
        phase: HostPhase::Failed,
        reason: REASON_CLASS_MISMATCH,
        message: format!(
            "Provider {} references ProviderClass {theirs}, not the zone's {class}",
            own.metadata.name.as_deref().unwrap_or_default()
        ),
    })
}

/// The Job to create, completed: for a backend that mints a token, the `ServiceAccount` banlieue
/// published for the host's Provider (banlieue ADR-0126; the identity gate holds until it has),
/// and for every Job the re-bootstrap mark the rollout counts (ADR-0013 decision 5).
fn finish(
    mut job: Job,
    own_provider: Option<&Provider>,
    rebootstrap: bool,
    mints: bool,
) -> Option<Job> {
    if rebootstrap {
        job.metadata
            .labels
            .get_or_insert_default()
            .insert(LABEL_REBOOTSTRAP.into(), "true".into());
    }
    if !mints {
        return Some(job);
    }
    provider_service_account(own_provider).map(|sa| with_provider_identity(job, sa))
}

/// Where a Host is in its bootstraps (ADR-0013 decision 5).
struct Progress {
    /// What the status records as the last successful bootstrap's inputs.
    bootstrapped_inputs: Option<String>,
    /// The Host has bootstrapped before and its inputs changed since.
    rebootstrap: bool,
    /// Why its re-bootstrap may not start now, if it may not.
    hold: Option<Hold>,
}

fn progress(
    w: &World<'_>,
    uid: &str,
    owned: &[&Job],
    want: Option<&str>,
    job: JobState,
) -> Progress {
    // The last bootstrap that succeeded: recorded, or, for a Host bootstrapped before the
    // record existed, its newest owned Job that completed.
    let recorded = w
        .host
        .status
        .as_ref()
        .and_then(|s| s.bootstrapped_inputs.clone())
        .or_else(|| {
            owned
                .iter()
                .filter(|j| job_state(j, w.pods) == JobState::Succeeded)
                .max_by_key(|j| job_generation(j))
                .and_then(|j| job_inputs(j))
                .map(str::to_string)
        });
    let rebootstrap = want.is_some() && recorded.is_some() && recorded.as_deref() != want;
    let hold = (rebootstrap && job == JobState::Absent)
        .then(|| rollout(w.fleet, uid, w.cfg.max_rebootstraps))
        .flatten();
    let bootstrapped_inputs = if job == JobState::Succeeded {
        want.map(str::to_string)
    } else {
        recorded
    };
    Progress {
        bootstrapped_inputs,
        rebootstrap,
        hold,
    }
}

/// `BootstrapCurrent`: status, reason, message.
fn bootstrap_condition(job: JobState, hold: Option<Hold>) -> (bool, &'static str, &'static str) {
    match (job, hold) {
        (JobState::Succeeded, _) => (
            true,
            REASON_BOOTSTRAP_CURRENT,
            "the last bootstrap ran the current inputs",
        ),
        (_, Some(Hold::Waiting)) => (
            false,
            REASON_ROLLOUT_WAITING,
            "waiting for a rollout slot to re-bootstrap",
        ),
        (_, Some(Hold::Halted)) => (
            false,
            REASON_ROLLOUT_HALTED,
            "another host's re-bootstrap failed; the rollout is halted",
        ),
        (_, None) => (
            false,
            REASON_BOOTSTRAP_PENDING,
            "the current inputs' bootstrap has not succeeded yet",
        ),
    }
}

/// The Provider's spec and labels bodies, for a Host that validates (ADR-0011, amended), with
/// the zone's class and its backend's endpoint (ADR-0018).
fn provider_bodies(
    w: &World<'_>,
    script: bool,
    backend: Backend,
    class: &str,
) -> (Option<serde_json::Value>, Option<serde_json::Value>) {
    let ns = &w.cfg.provider.namespace;
    if script {
        (
            spec_body(w.host, backend, class, w.zone, ns),
            labels_body(w.host, w.zone, ns),
        )
    } else if let Some(own) = w.own_provider {
        (migrated_spec_body(own), labels_body(w.host, w.zone, ns))
    } else {
        (None, None)
    }
}

/// Why a re-bootstrap is held (ADR-0013 decision 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hold {
    /// Every slot is taken.
    Waiting,
    /// Another Host's re-bootstrap failed.
    Halted,
}

/// Whether a re-bootstrap may start now, from every bootstrap Job but this Host's own.
fn rollout(fleet: &[Job], uid: &str, max: u32) -> Option<Hold> {
    let others = fleet.iter().filter(|j| {
        let marked = j
            .metadata
            .labels
            .as_ref()
            .and_then(|l| l.get(LABEL_REBOOTSTRAP));
        !owned_by(j, uid) && marked.map(String::as_str) == Some("true")
    });
    let mut running = 0usize;
    for j in others {
        match job_state(j, &[]) {
            JobState::Failed(_) => return Some(Hold::Halted),
            JobState::Running => running += 1,
            JobState::Succeeded | JobState::Absent => {}
        }
    }
    (running >= usize::try_from(max).unwrap_or(usize::MAX)).then_some(Hold::Waiting)
}

/// What the status records besides the phase.
struct Recorded {
    provider_ref: Option<String>,
    accepted_host_key: Option<String>,
    bootstrapped_inputs: Option<String>,
    /// `BootstrapCurrent`, for a bootstrapped Host: status, reason, message.
    bootstrap: Option<(bool, &'static str, &'static str)>,
    /// `Drained`, while the Host is draining.
    drained: Option<Drained>,
}

/// The status a decision makes. Fields the reconciler does not decide are carried over.
fn status(
    previous: Option<&HostStatus>,
    d: &Decision,
    generation: i64,
    r: Recorded,
    now: &Time,
) -> HostStatus {
    let mut conditions = vec![condition(
        previous,
        CONDITION_READY,
        d.phase == HostPhase::Ready,
        d.reason,
        &d.message,
        generation,
        now,
    )];
    if let Some((current, reason, message)) = r.bootstrap {
        conditions.push(condition(
            previous,
            CONDITION_BOOTSTRAP_CURRENT,
            current,
            reason,
            message,
            generation,
            now,
        ));
    }
    if let Some(drained) = &r.drained {
        conditions.push(condition(
            previous,
            CONDITION_DRAINED,
            drained.status,
            drained.reason,
            &drained.message,
            generation,
            now,
        ));
    }
    HostStatus {
        phase: Some(d.phase),
        provider_ref: r.provider_ref,
        accepted_host_key: r.accepted_host_key,
        bootstrapped_inputs: r.bootstrapped_inputs,
        conditions,
        observed_generation: Some(generation),
        message: Some(d.message.clone()),
    }
}

/// A condition whose transition time moves only when its status flips.
fn condition(
    previous: Option<&HostStatus>,
    type_: &str,
    status: bool,
    reason: &str,
    message: &str,
    generation: i64,
    now: &Time,
) -> Condition {
    let status = if status { "True" } else { "False" };
    let last = previous
        .into_iter()
        .flat_map(|s| s.conditions.iter())
        .find(|c| c.type_ == type_);
    let last_transition_time = match last {
        Some(c) if c.status == status => c.last_transition_time.clone(),
        _ => now.clone(),
    };
    Condition {
        type_: type_.into(),
        status: status.into(),
        reason: reason.into(),
        message: message.into(),
        observed_generation: Some(generation),
        last_transition_time,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{ANNOTATION_INPUTS, EXIT_SCRIPT_DRIFT, bootstrap_job, job_name};
    use crate::phase::REASON_PROVIDER_UNREACHABLE;
    use crate::phase::{
        REASON_JOB_FAILED, REASON_JOB_RUNNING, REASON_PROVIDER_AMBIGUOUS, REASON_PROVIDER_HEALTHY,
        REASON_SCRIPT_DRIFT, REASON_ZONE_NOT_FOUND, REASON_ZONE_UNSUPPORTED,
    };
    use faubourg_api::host::Maintenance;
    use faubourg_api::host::{
        BootstrapMethod, BootstrapSpec, DEFAULT_SCRIPT_KEY, HostKeyPolicy, HostSpec, ScriptRef,
        ScriptSourceKind, SecretName, SshSpec,
    };
    use faubourg_api::policy::PinnedScript;
    use faubourg_api::zone::ProviderClassRef;
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
            provider: crate::job::ProviderConfig {
                class: "cloud-hypervisor".into(),
                namespace: "banlieue-system".into(),
                api_server: "https://kube.home.example:6443".into(),
            },
            max_rebootstraps: 1,
        }
    }

    const SCRIPT_SHA256: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    /// The default script, pinned as the bootstrap policy would (ADR-0013).
    fn pins() -> Vec<PinnedScript> {
        vec![PinnedScript {
            source: cfg().default_script.unwrap(),
            sha256: SCRIPT_SHA256.into(),
        }]
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
                maintenance: Maintenance::None,
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
        let mut j = bootstrap_job(
            h,
            &cfg(),
            Some(&Policy {
                scripts: pins(),
                provider_ca: None,
            }),
        )
        .unwrap();
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
        own_provider: Option<Provider>,
        class: Option<ProviderClass>,
        cfg: BootstrapConfig,
        pins: Option<Policy>,
        fleet: Vec<Job>,
        peers: Vec<Host>,
        now: Time,
        drained: Option<Drained>,
        provider_refused: Option<String>,
    }

    impl Fixture {
        fn new(host: Host) -> Self {
            Self {
                host,
                zone: Some(zone("Local")),
                jobs: vec![],
                pods: vec![],
                providers: vec![],
                own_provider: Some(own(true)),
                class: Some(class("External")),
                cfg: cfg(),
                pins: Some(Policy {
                    scripts: pins(),
                    provider_ca: None,
                }),
                fleet: vec![],
                peers: vec![],
                now: at(1_000),
                drained: None,
                provider_refused: None,
            }
        }

        fn plan(&self) -> Plan {
            plan(&World {
                host: &self.host,
                zone: self.zone.as_ref(),
                jobs: &self.jobs,
                pods: &self.pods,
                providers: &self.providers,
                own_provider: self.own_provider.as_ref(),
                class: self.class.as_ref(),
                cfg: &self.cfg,
                pins: self.pins.as_ref(),
                fleet: &self.fleet,
                peers: &self.peers,
                now: &self.now,
                drained: self.drained.as_ref(),
                provider_refused: self.provider_refused.as_deref(),
            })
            .unwrap()
        }
    }

    fn class(deployment: &str) -> ProviderClass {
        class_of("cloud-hypervisor", "cloud-hypervisor", deployment)
    }

    fn class_of(name: &str, backend: &str, deployment: &str) -> ProviderClass {
        serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1", "kind": "ProviderClass",
            "metadata": { "name": name },
            "spec": { "backend": backend, "deployment": deployment }
        }))
        .unwrap()
    }

    fn zone_with_class(class: &str) -> AvailabilityZone {
        let mut z = zone("Local");
        z.spec.provider_class_ref = Some(ProviderClassRef { name: class.into() });
        z
    }

    /// The Provider named after the Host, with or without its identity.
    fn own(identity: bool) -> Provider {
        let status = if identity {
            json!({ "workload": { "mode": "External", "namespace": "banlieue-system",
                                  "serviceAccountName": "banlieue-provider-cloud-hypervisor-hv01" } })
        } else {
            json!({})
        };
        serde_json::from_value(json!({
            "apiVersion": "banlieue.io/v1alpha1", "kind": "Provider",
            "metadata": { "name": "hv01", "namespace": "banlieue-system" },
            "spec": { "providerClassRef": { "name": "cloud-hypervisor" },
                      "connection": { "endpoint": "ch://hv01.home.example" } },
            "status": status
        }))
        .unwrap()
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

    /// ADR-0017 decision 4: a draining Host carries `Drained` beside `Ready`, and loses it when
    /// the drain ends.
    #[test]
    fn a_draining_host_reports_drained_beside_ready() {
        let mut f = Fixture::new(host(1));
        f.drained = Some(Drained {
            status: false,
            reason: crate::drain::REASON_MOVES_BLOCKED,
            message: "banlieue will not move apps/web-0 (VTPMNotMovable)".into(),
        });
        let p = f.plan();
        let d = p
            .status
            .conditions
            .iter()
            .find(|c| c.type_ == CONDITION_DRAINED)
            .expect("Drained");
        assert_eq!(d.status, "False");
        assert_eq!(d.reason, crate::drain::REASON_MOVES_BLOCKED);
        assert!(d.message.contains("apps/web-0"));
        assert_eq!(
            p.status.conditions[0].type_, CONDITION_READY,
            "Ready stays first"
        );
        f.drained = None;
        assert!(
            !f.plan()
                .status
                .conditions
                .iter()
                .any(|c| c.type_ == CONDITION_DRAINED)
        );
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
            own_provider: f.own_provider.as_ref(),
            class: f.class.as_ref(),
            cfg: &f.cfg,
            pins: f.pins.as_ref(),
            fleet: &f.fleet,
            peers: &f.peers,
            now: &f.now,
            drained: None,
            provider_refused: None,
        });
        assert_eq!(r, Err(NotPersisted("uid")));
    }

    // ----- pinned scripts (ADR-0013) -------------------------------------------------------

    #[test]
    fn an_unpinned_script_fails_the_host_and_creates_nothing() {
        for pins in [None, Some(Policy::default())] {
            let mut f = Fixture::new(host(1));
            f.pins = pins.clone();
            let p = f.plan();
            assert_eq!(p.decision.phase, HostPhase::Failed, "{pins:?}");
            assert_eq!(p.decision.reason, REASON_SCRIPT_NOT_PINNED);
            assert_eq!(p.create, None);
        }
    }

    #[test]
    fn an_adopted_host_needs_no_pinned_script() {
        let mut f = Fixture::new(adopted());
        f.pins = None;
        assert_ne!(f.plan().decision.reason, REASON_SCRIPT_NOT_PINNED);
    }

    // ----- the rollout (ADR-0013 decision 5) ---------------------------------------------

    /// A Host that completed a bootstrap under other inputs, is Ready, and is due again.
    fn due_again() -> Fixture {
        let mut f = Fixture::new(host(2));
        f.host.status = Some(HostStatus {
            phase: Some(HostPhase::Ready),
            bootstrapped_inputs: Some("0".repeat(64)),
            ..HostStatus::default()
        });
        f.providers = vec![provider("hv01", true)];
        f
    }

    /// Another Host's re-bootstrap Job, in the given state.
    fn rebootstrap_of(other: &str, conditions: &[&str]) -> Job {
        let mut h = host(1);
        h.metadata.name = Some(other.into());
        h.metadata.uid = Some(format!("uid-{other}"));
        let mut j = job_for(&h, &format!("job-{other}"), conditions);
        j.metadata
            .labels
            .get_or_insert_default()
            .insert(LABEL_REBOOTSTRAP.into(), "true".into());
        j
    }

    fn bootstrap_current(p: &Plan) -> Option<(&str, &str)> {
        p.status
            .conditions
            .iter()
            .find(|c| c.type_ == CONDITION_BOOTSTRAP_CURRENT)
            .map(|c| (c.status.as_str(), c.reason.as_str()))
    }

    #[test]
    fn a_free_slot_runs_a_rebootstrap_and_marks_its_job() {
        let p = due_again().plan();
        let job = p.create.expect("a slot is free");
        assert_eq!(
            job.metadata
                .labels
                .unwrap()
                .get(LABEL_REBOOTSTRAP)
                .map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn a_rebootstrap_waits_for_a_slot_and_keeps_its_phase() {
        let mut f = due_again();
        f.fleet = vec![rebootstrap_of("hv02", &[])];
        let p = f.plan();
        assert_eq!(p.create, None);
        assert_eq!(
            p.decision.phase,
            HostPhase::Ready,
            "it keeps running what it has"
        );
        assert_eq!(
            bootstrap_current(&p),
            Some(("False", REASON_ROLLOUT_WAITING))
        );
        f.cfg.max_rebootstraps = 2;
        assert!(f.plan().create.is_some(), "a second slot");
        f.fleet = vec![rebootstrap_of("hv02", &["Complete"])];
        f.cfg.max_rebootstraps = 1;
        assert!(f.plan().create.is_some(), "a finished one holds no slot");
    }

    #[test]
    fn a_failed_rebootstrap_halts_the_rollout() {
        let mut f = due_again();
        f.fleet = vec![rebootstrap_of("hv02", &["Failed"])];
        f.cfg.max_rebootstraps = 5;
        let p = f.plan();
        assert_eq!(p.create, None);
        assert_eq!(p.decision.phase, HostPhase::Ready);
        assert_eq!(
            bootstrap_current(&p),
            Some(("False", REASON_ROLLOUT_HALTED))
        );
    }

    #[test]
    fn a_first_bootstrap_is_not_limited() {
        let mut f = Fixture::new(host(1));
        f.fleet = vec![
            rebootstrap_of("hv02", &[]),
            rebootstrap_of("hv03", &["Failed"]),
        ];
        let job = f.plan().create.expect("nothing to lose on a fresh host");
        assert!(!job.metadata.labels.unwrap().contains_key(LABEL_REBOOTSTRAP));
    }

    #[test]
    fn a_host_bootstrapped_before_this_version_counts_as_bootstrapped() {
        // No recorded inputs, but it owns a Job that completed under other inputs.
        let first = host(1);
        let mut f = Fixture::new(host(2));
        f.host.spec.address = "hv01b.home.example".into();
        f.jobs = vec![job_for(&first, "j1", &["Complete"])];
        f.fleet = vec![rebootstrap_of("hv02", &[])];
        let p = f.plan();
        assert_eq!(p.create, None, "held like any re-bootstrap");
        assert_eq!(
            p.status.bootstrapped_inputs,
            job_inputs(&f.jobs[0]).map(str::to_string),
            "recorded before its old Job goes"
        );
    }

    #[test]
    fn the_last_success_is_kept_until_the_status_records_it() {
        // As at an upgrade: the Host predates bootstrappedInputs, its inputs changed (here a
        // re-pin) but not its generation, so its completed Job holds the new Job's name.
        let old = Fixture::new(host(1));
        let mut f = Fixture::new(host(1));
        f.jobs = vec![job_for(&old.host, "j1", &["Complete"])];
        f.pins.as_mut().unwrap().scripts[0].sha256 = SCRIPT_SHA256.replace("fedc", "0123");
        let p = f.plan();
        assert!(
            p.delete.is_empty(),
            "the only record of the success stays until it is read back"
        );
        assert_eq!(p.create, None);
        assert_eq!(
            p.status.bootstrapped_inputs,
            job_inputs(&f.jobs[0]).map(str::to_string)
        );
        // The next reconcile reads the record back: the old Job may go now.
        f.host.status = Some(p.status);
        assert_eq!(f.plan().delete, vec!["bootstrap-hv01-g1".to_string()]);
        // And once it is gone, the new Job is a re-bootstrap, through the rollout.
        f.jobs = vec![];
        let job = f.plan().create.expect("a slot is free");
        assert_eq!(
            job.metadata
                .labels
                .unwrap()
                .get(LABEL_REBOOTSTRAP)
                .map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn a_succeeded_job_records_its_inputs_and_the_bootstrap_is_current() {
        let mut f = due_again();
        let current = job_for(&f.host, "j2", &["Complete"]);
        let want = job_inputs(&current).map(str::to_string);
        f.jobs = vec![current];
        let p = f.plan();
        assert_eq!(p.status.bootstrapped_inputs, want);
        assert_eq!(
            bootstrap_current(&p),
            Some(("True", REASON_BOOTSTRAP_CURRENT))
        );
        assert_eq!(
            p.status.conditions[0].type_, CONDITION_READY,
            "Ready stays first"
        );
    }

    #[test]
    fn a_running_or_failed_rebootstrap_is_not_current() {
        let mut f = due_again();
        f.jobs = vec![job_for(&f.host, "j2", &[])];
        assert_eq!(
            bootstrap_current(&f.plan()),
            Some(("False", REASON_BOOTSTRAP_PENDING))
        );
        f.jobs = vec![job_for(&f.host, "j2", &["Failed"])];
        let p = f.plan();
        assert_eq!(
            bootstrap_current(&p),
            Some(("False", REASON_BOOTSTRAP_PENDING))
        );
        assert_eq!(
            p.status.bootstrapped_inputs.as_deref(),
            Some("0".repeat(64).as_str())
        );
    }

    #[test]
    fn an_adopted_host_has_no_bootstrap_condition() {
        let mut f = Fixture::new(adopted());
        f.providers = vec![provider("hv01", true)];
        assert_eq!(bootstrap_current(&f.plan()), None);
    }

    // ----- first-use host keys (ADR-0010 decision 8) --------------------------------------

    const FP: &str = "SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s";

    fn reported(job_uid: &str, message: &str) -> Pod {
        serde_json::from_value(json!({
            "metadata": { "name": "bootstrap-hv01-g1-x", "ownerReferences": [{
                "apiVersion": "batch/v1", "kind": "Job", "name": "bootstrap-hv01-g1",
                "uid": job_uid, "controller": true }] },
            "status": { "containerStatuses": [{
                "name": "bootstrap", "image": "x", "imageID": "", "ready": false,
                "restartCount": 0,
                "state": { "terminated": { "exitCode": 0, "message": message } } }] }
        }))
        .unwrap()
    }

    fn first_use() -> Host {
        let mut h = host(1);
        h.spec.ssh.as_mut().unwrap().host_key_policy = HostKeyPolicy::TrustOnFirstUse;
        h
    }

    #[test]
    fn a_first_use_key_is_pinned_from_a_succeeded_job() {
        let mut f = Fixture::new(first_use());
        f.jobs = vec![job_for(&f.host, "j1", &["Complete"])];
        f.pods = vec![reported("j1", &format!("{{\"hostKey\":\"{FP}\"}}"))];
        let p = f.plan();
        assert_eq!(p.status.accepted_host_key.as_deref(), Some(FP));
        assert_eq!(p.create, None, "pinning the key re-runs nothing");
    }

    #[test]
    fn a_pinned_key_is_never_replaced() {
        let mut f = Fixture::new(first_use());
        f.host.status = Some(HostStatus {
            accepted_host_key: Some("SHA256:w/MKP5qmOhlfmXHftrq9Q2lgVxsxOdNJo9vPBW8Li+E".into()),
            ..HostStatus::default()
        });
        f.jobs = vec![job_for(&f.host, "j1", &["Complete"])];
        f.pods = vec![reported("j1", &format!("{{\"hostKey\":\"{FP}\"}}"))];
        assert_eq!(
            f.plan().status.accepted_host_key.as_deref(),
            Some("SHA256:w/MKP5qmOhlfmXHftrq9Q2lgVxsxOdNJo9vPBW8Li+E")
        );
    }

    #[test]
    fn no_key_is_pinned_from_a_failed_job_a_strict_host_or_another_jobs_pod() {
        let msg = format!("{{\"hostKey\":\"{FP}\"}}");
        let mut f = Fixture::new(first_use());
        f.jobs = vec![job_for(&f.host, "j1", &["Failed"])];
        f.pods = vec![reported("j1", &msg)];
        assert_eq!(f.plan().status.accepted_host_key, None, "failed");

        let mut f = Fixture::new(host(1));
        f.jobs = vec![job_for(&f.host, "j1", &["Complete"])];
        f.pods = vec![reported("j1", &msg)];
        assert_eq!(f.plan().status.accepted_host_key, None, "strict");

        let mut f = Fixture::new(first_use());
        f.jobs = vec![job_for(&f.host, "j1", &["Complete"])];
        f.pods = vec![reported("other", &msg)];
        assert_eq!(f.plan().status.accepted_host_key, None, "another job's pod");
    }

    #[test]
    fn a_malformed_report_pins_nothing() {
        for msg in [
            "",
            "not json",
            "{\"hostKey\":\"MD5:aa\"}",
            "{\"hostKey\":\"SHA256:short\"}",
            "{}",
        ] {
            let mut f = Fixture::new(first_use());
            f.jobs = vec![job_for(&f.host, "j1", &["Complete"])];
            f.pods = vec![reported("j1", msg)];
            assert_eq!(f.plan().status.accepted_host_key, None, "{msg:?}");
        }
    }

    // ----- the host's Provider (ADR-0011) -------------------------------------------------

    #[test]
    fn a_bootstrapped_host_gets_its_provider_applied() {
        let p = Fixture::new(host(1)).plan();
        let s = p
            .apply_provider_spec
            .expect("the Provider's spec is applied");
        assert_eq!(s["metadata"]["name"], "hv01");
        assert_eq!(s["spec"]["providerClassRef"]["name"], "cloud-hypervisor");
        let l = p.apply_provider.expect("and its labels");
        assert!(l.get("spec").is_none());
        assert_eq!(
            l["metadata"]["labels"]["faubourg.firestoned.io/host"],
            "hv01"
        );
    }

    #[test]
    fn a_host_switched_to_none_stops_writing_the_spec_and_keeps_labelling() {
        let mut f = Fixture::new(adopted());
        f.own_provider = Some(own(true));
        let p = f.plan();
        assert_eq!(p.apply_provider_spec, None, "the spec is left as it is");
        assert!(p.apply_provider.is_some());
        // As a v0.1 controller left it: the labels' manager owns the spec too.
        let mut legacy = own(true);
        legacy.metadata.managed_fields = Some(
            serde_json::from_value(json!([{
                "manager": crate::provider::LABELS_MANAGER, "operation": "Apply",
                "fieldsV1": { "f:spec": { "f:providerClassRef": { "f:name": {} },
                                          "f:connection": { "f:endpoint": {} } } }
            }]))
            .unwrap(),
        );
        f.own_provider = Some(legacy);
        let s = f
            .plan()
            .apply_provider_spec
            .expect("moved before the labels' apply");
        assert_eq!(
            s["spec"]["connection"]["endpoint"],
            "ch://hv01.home.example"
        );
    }

    #[test]
    fn no_job_until_banlieue_gives_the_provider_its_identity() {
        let mut f = Fixture::new(host(1));
        f.own_provider = None;
        let p = f.plan();
        assert_eq!(p.create, None);
        assert_eq!(p.decision.phase, HostPhase::Pending);
        assert_eq!(p.decision.reason, REASON_AWAITING_IDENTITY);
        assert!(
            p.apply_provider_spec.is_some(),
            "the Provider is created first"
        );
        f.own_provider = Some(own(false));
        assert_eq!(f.plan().create, None);
        f.own_provider = Some(own(true));
        assert!(f.plan().create.is_some());
    }

    #[test]
    fn a_class_faubourg_cannot_drive_fails_the_host_and_creates_nothing() {
        // Missing, in the wrong mode, or a backend with no driver yet (ADR-0018 decision 2);
        // for a bootstrapped and an adopted Host alike.
        for h in [host(1), adopted()] {
            for class in [
                None,
                Some(class("Managed")),
                Some(class_of("cloud-hypervisor", "libvirt", "External")),
                Some(class_of("cloud-hypervisor", "firecracker", "External")),
            ] {
                let mut f = Fixture::new(h.clone());
                f.class = class.clone();
                let p = f.plan();
                assert_eq!(p.decision.phase, HostPhase::Failed, "{class:?}");
                assert_eq!(p.decision.reason, REASON_BACKEND_UNSUPPORTED);
                assert_eq!(p.create, None);
                assert_eq!(p.apply_provider, None);
                assert_eq!(p.apply_provider_spec, None);
            }
        }
    }

    #[test]
    fn the_provider_references_the_zones_class() {
        let mut f = Fixture::new(host(1));
        f.zone = Some(zone_with_class("ch-canary"));
        f.class = Some(class_of("ch-canary", "cloud-hypervisor", "External"));
        f.own_provider = None;
        let b = f.plan().apply_provider_spec.unwrap();
        assert_eq!(b["spec"]["providerClassRef"]["name"], "ch-canary");
        assert_eq!(
            b["spec"]["connection"]["endpoint"],
            "ch://hv01.home.example"
        );
    }

    #[test]
    fn a_provider_of_another_class_fails_the_host_and_is_left_alone() {
        for h in [host(1), adopted()] {
            let mut f = Fixture::new(h);
            f.zone = Some(zone_with_class("ch-canary"));
            f.class = Some(class_of("ch-canary", "cloud-hypervisor", "External"));
            // own() references cloud-hypervisor.
            let p = f.plan();
            assert_eq!(p.decision.phase, HostPhase::Failed);
            assert_eq!(p.decision.reason, REASON_CLASS_MISMATCH);
            assert!(
                p.decision.message.contains("ch-canary"),
                "{}",
                p.decision.message
            );
            assert_eq!(p.create, None);
            assert_eq!(p.apply_provider, None, "faubourg never rewrites it");
            assert_eq!(p.apply_provider_spec, None);
        }
    }

    // ----- libvirt (ADR-0018 decisions 6 to 9) ---------------------------------------------

    fn libvirt_zone() -> AvailabilityZone {
        let mut z = zone_with_class("libvirt");
        z.spec.libvirt = serde_json::from_value(json!({
            "credentialsRef": { "name": "faubourg-kvm-client" },
            "caBundleRef": { "name": "faubourg-kvm-ca" } }))
        .unwrap();
        z
    }

    /// A libvirt Host's fixture: its zone, a Managed libvirt class, and a Provider of that
    /// class with no identity (banlieue gives a Managed Provider none faubourg uses).
    fn libvirt_fixture(h: Host) -> Fixture {
        let mut f = Fixture::new(h);
        f.zone = Some(libvirt_zone());
        f.class = Some(class_of("libvirt", "libvirt", "Managed"));
        let mut own = own(false);
        own.spec.provider_class_ref.name = "libvirt".into();
        f.own_provider = Some(own);
        f
    }

    /// The labelled libvirt Provider: `Ready`, a pod's `Alive` heartbeat, and `reachable`.
    fn libvirt_provider(reachable: bool) -> Provider {
        let mut p = provider("hv01", true);
        let s = p.status.as_mut().unwrap();
        s.conditions.push(
            serde_json::from_value(json!({ "type": "ProviderReachable",
                "status": if reachable { "True" } else { "False" }, "observedGeneration": 1,
                "reason": "R", "message": "", "lastTransitionTime": "2026-11-07T00:00:00Z" }))
            .unwrap(),
        );
        p
    }

    #[test]
    fn a_libvirt_host_gets_the_zones_connection_and_a_job_that_mints_nothing() {
        let p = libvirt_fixture(host(1)).plan();
        assert_eq!(p.backend, Some(Backend::Libvirt));
        let spec = p.apply_provider_spec.unwrap();
        assert_eq!(spec["spec"]["providerClassRef"]["name"], "libvirt");
        assert_eq!(
            spec["spec"]["connection"]["endpoint"],
            "qemu+tls://hv01.home.example/system"
        );
        assert_eq!(
            spec["spec"]["connection"]["credentialsRef"]["name"],
            "faubourg-kvm-client"
        );
        // No identity to wait for, and none to hand over (decision 7).
        let j = p.create.expect("a Job without waiting for an identity");
        let c = &j.spec.unwrap().template.spec.unwrap().containers[0];
        let env: Vec<&str> = c.env.iter().flatten().map(|e| e.name.as_str()).collect();
        assert!(
            !env.contains(&crate::job::ENV_PROVIDER_SERVICE_ACCOUNT),
            "{env:?}"
        );
        assert!(!env.contains(&crate::job::ENV_PROVIDER), "{env:?}");
        let mounts: Vec<&str> = c
            .volume_mounts
            .iter()
            .flatten()
            .map(|m| m.name.as_str())
            .collect();
        assert!(!mounts.contains(&crate::job::VOLUME_KUBE_API), "{mounts:?}");
    }

    #[test]
    fn a_zone_whose_connection_does_not_fit_its_backend_fails_its_hosts() {
        // A libvirt zone without spec.libvirt, and a cloud-hypervisor zone with one.
        let mut bare = libvirt_fixture(host(1));
        bare.zone.as_mut().unwrap().spec.libvirt = None;
        let mut ch = Fixture::new(host(1));
        ch.zone.as_mut().unwrap().spec.libvirt = libvirt_zone().spec.libvirt;
        for f in [bare, ch] {
            let p = f.plan();
            assert_eq!(p.decision.phase, HostPhase::Failed);
            assert_eq!(p.decision.reason, REASON_CONNECTION_INVALID);
            assert_eq!(p.create, None);
            assert_eq!(p.apply_provider, None);
            assert_eq!(p.apply_provider_spec, None);
        }
    }

    #[test]
    fn a_libvirt_host_is_ready_only_while_its_provider_reaches_it() {
        let mut f = libvirt_fixture(adopted());
        f.providers = vec![libvirt_provider(false)];
        let p = f.plan();
        assert_ne!(
            p.decision.phase,
            HostPhase::Ready,
            "a pod's Alive heartbeat is not the host's (decision 8)"
        );
        f.providers = vec![libvirt_provider(true)];
        assert_eq!(f.plan().decision.phase, HostPhase::Ready);
        f.host.status = Some(f.plan().status);
        f.providers = vec![libvirt_provider(false)];
        let p = f.plan();
        assert_eq!(p.decision.phase, HostPhase::Degraded);
        assert_eq!(p.decision.reason, REASON_PROVIDER_UNREACHABLE);
    }

    #[test]
    fn a_provider_the_api_server_refuses_fails_the_host_and_says_why() {
        // Found live: banlieue's credentialsRef policy refused faubourg's apply, and the Host had
        // no status at all while the controller retried.
        for h in [host(1), adopted()] {
            let mut f = libvirt_fixture(h);
            f.provider_refused = Some(format!(
                "spec.connection.credentialsRef names Secret faubourg-kvm-client {}",
                "x".repeat(2_000)
            ));
            let p = f.plan();
            assert_eq!(p.decision.phase, HostPhase::Failed);
            assert_eq!(p.decision.reason, REASON_PROVIDER_APPLY_REFUSED);
            assert!(p.decision.message.contains("faubourg-kvm-client"));
            assert!(p.decision.message.len() < 600, "the refusal is bounded");
            assert!(p.status_changed, "the status is written");
            assert_eq!(
                p.create, None,
                "no Job for a host whose Provider was refused"
            );
            assert_eq!(
                p.apply_provider, None,
                "the refused write is not planned again"
            );
            assert_eq!(p.apply_provider_spec, None);
        }
        assert_eq!(truncate("héllo", 2), "h", "never cut inside a character");
    }

    fn peer(name: &str, address: &str) -> Host {
        let mut h = host(1);
        h.metadata.name = Some(name.into());
        h.metadata.uid = Some(format!("uid-{name}"));
        h.spec.address = address.into();
        h
    }

    #[test]
    fn two_hosts_with_one_address_both_fail() {
        // Literal and case-folded (ADR-0018 decision 10); the Host itself is not its own peer.
        // A DNS name is lowercase by validation, so case matters for an IPv6 address.
        for (own, address, duplicate) in [
            ("hv01.home.example", "hv01.home.example", true),
            ("2001:db8::1", "2001:DB8::1", true),
            ("hv01.home.example", "hv02.home.example", false),
            // Not resolved: a name and an IP for one machine are not caught.
            ("hv01.home.example", "192.0.2.10", false),
        ] {
            for mut h in [host(1), adopted()] {
                h.spec.address = own.into();
                let mut f = Fixture::new(h);
                f.peers = vec![f.host.clone(), peer("hv02", address)];
                let p = f.plan();
                if duplicate {
                    assert_eq!(p.decision.phase, HostPhase::Failed, "{address}");
                    assert_eq!(p.decision.reason, REASON_DUPLICATE_ADDRESS);
                    assert!(
                        p.decision.message.contains("hv02"),
                        "{}",
                        p.decision.message
                    );
                    assert_eq!(p.create, None);
                    assert_eq!(p.apply_provider, None);
                    assert_eq!(p.apply_provider_spec, None);
                } else {
                    assert_ne!(p.decision.reason, REASON_DUPLICATE_ADDRESS, "{address}");
                }
            }
        }
    }

    #[test]
    fn a_host_being_deleted_is_no_duplicate() {
        let mut f = Fixture::new(host(1));
        let mut gone = peer("hv02", "hv01.home.example");
        gone.metadata.deletion_timestamp = Some(at(1));
        f.peers = vec![gone];
        assert_ne!(f.plan().decision.reason, REASON_DUPLICATE_ADDRESS);
    }

    #[test]
    fn an_adopted_host_labels_its_existing_provider_only() {
        let mut f = Fixture::new(adopted());
        f.own_provider = None;
        assert_eq!(
            f.plan().apply_provider,
            None,
            "never created for an adopted host"
        );
        f.own_provider = Some(own(true));
        assert_eq!(
            f.plan().apply_provider_spec,
            None,
            "never an adopted Provider's spec"
        );
        let b = f.plan().apply_provider.unwrap();
        assert!(b.get("spec").is_none());
        assert_eq!(
            b["metadata"]["labels"]["faubourg.firestoned.io/host"],
            "hv01"
        );
        f.class = None;
        assert_eq!(
            f.plan().apply_provider,
            None,
            "an adopted host needs its zone's class too (ADR-0018 decision 2)"
        );
    }

    #[test]
    fn an_invalid_spec_touches_no_provider() {
        let mut f = Fixture::new(host(1));
        f.host.spec.address = "-oProxyCommand=sh".into();
        assert_eq!(f.plan().apply_provider, None);
        assert_eq!(f.plan().apply_provider_spec, None);
    }
}
