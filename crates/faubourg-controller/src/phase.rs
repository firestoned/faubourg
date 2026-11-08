// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! The Host phase machine (ADR-0005 decision 6, ADR-0007 decision 1), as a pure function.
//!
//! | Phase | When |
//! | --- | --- |
//! | `Failed` | the spec is refused, or the Host's current bootstrap Job failed |
//! | `Pending` | nothing started: no Job yet, the zone is missing or unsupported, or an adopted host whose `Provider` never came up |
//! | `Bootstrapping` | the Job runs, or it succeeded and the `Provider` has not come up yet |
//! | `Ready` | the Job succeeded (or the host is adopted) and the `Provider` is healthy |
//! | `Degraded` | was `Ready` or `Degraded`, and the `Provider` is no longer healthy |
//!
//! Success is the provider coming up, not an exit code: no input reaches `Ready` unless
//! [`ProviderHealth::Healthy`]. Validation is checked first, so a refused spec is `Failed`
//! whatever the cluster says. A running or failed Job is reported before the zone, which gates
//! only what has not started (ADR-0009 decision 4).

use faubourg_api::host::{BootstrapMethod, HostPhase, HostSpec};

use crate::health::ProviderHealth;

/// Reason: the spec failed validation.
pub const REASON_INVALID_SPEC: &str = "InvalidSpec";
/// Reason: waiting for the bootstrap Job to be created.
pub const REASON_AWAITING_JOB: &str = "AwaitingBootstrapJob";
/// Reason: the bootstrap Job is running.
pub const REASON_JOB_RUNNING: &str = "BootstrapJobRunning";
/// Reason: the bootstrap Job failed.
pub const REASON_JOB_FAILED: &str = "BootstrapJobFailed";
/// Reason: the on-host script differs from the source and there is no placement credential
/// (ADR-0005 decision 4). Nothing ran.
pub const REASON_SCRIPT_DRIFT: &str = "ScriptDrift";
/// Reason: bootstrapped or adopted, waiting for the `Provider` to come up.
pub const REASON_AWAITING_PROVIDER: &str = "AwaitingProvider";
/// Reason: the `Provider` is `Ready=True` with an `Alive` heartbeat.
pub const REASON_PROVIDER_HEALTHY: &str = "ProviderHealthy";
/// Reason: the bootstrap image refused the host's key (ADR-0006 decision 3). Nothing ran.
pub const REASON_HOST_KEY_REFUSED: &str = "HostKeyRefused";
/// Reason: the mounted script was not the one the bootstrap policy pins (ADR-0013 decision 4).
/// Nothing was connected to.
pub const REASON_SCRIPT_UNPINNED: &str = "ScriptUnpinned";
/// Reason: the Host's `AvailabilityZone` does not exist.
pub const REASON_ZONE_NOT_FOUND: &str = "ZoneNotFound";
/// Reason: the Host's `AvailabilityZone` is refused by its own validation (ADR-0004).
pub const REASON_ZONE_UNSUPPORTED: &str = "ZoneUnsupported";
/// Reason: more than one `Provider` carries the host's label; faubourg does not pick one.
pub const REASON_PROVIDER_AMBIGUOUS: &str = "ProviderAmbiguous";
/// Reason: the host's `Provider` is gone.
pub const REASON_PROVIDER_MISSING: &str = "ProviderMissing";
/// Reason: the `Provider` is not `Ready=True`.
pub const REASON_PROVIDER_NOT_READY: &str = "ProviderNotReady";
/// Reason: the `Provider`'s heartbeat is not `Alive`.
pub const REASON_HEARTBEAT_NOT_ALIVE: &str = "HeartbeatNotAlive";

/// The current generation's bootstrap Job, as the reconciler observed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    /// No Job for this generation.
    Absent,
    /// Created and not finished.
    Running,
    /// Completed successfully.
    Succeeded,
    /// Failed, and why.
    Failed(JobFailure),
}

/// Why a bootstrap Job failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobFailure {
    /// The on-host script differs from the source and no placement credential is set.
    ScriptDrift,
    /// The host's key was refused before authenticating.
    HostKeyRefused,
    /// The mounted script was not the pinned one: its source was edited without a re-pin.
    ScriptUnpinned,
    /// Anything else: an unreachable host, a refused login, a non-zero script exit.
    Other,
}

/// The Host's `AvailabilityZone`, as the reconciler observed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoneState {
    /// The zone exists and its spec validates.
    Supported,
    /// No zone of that name.
    NotFound,
    /// The zone's own validation refuses it (a `Shared` zone, ADR-0004).
    Unsupported,
}

/// Everything the phase depends on.
#[derive(Clone, Copy, Debug)]
pub struct Observed<'a> {
    /// The Host's spec.
    pub spec: &'a HostSpec,
    /// The phase in the Host's status before this reconcile.
    pub previous: Option<HostPhase>,
    /// The Host's `AvailabilityZone`.
    pub zone: ZoneState,
    /// The Host's current bootstrap Job: the one built from its current inputs.
    pub job: JobState,
    /// What the host's `Provider` says.
    pub provider: ProviderHealth,
}

/// The phase to write, with the reason and message for the Host's `Ready` condition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    /// New phase.
    pub phase: HostPhase,
    /// Machine-readable reason.
    pub reason: &'static str,
    /// Human-readable message. Never contains secret material or a command line.
    pub message: String,
}

/// Decides a Host's phase.
pub fn decide(o: &Observed<'_>) -> Decision {
    if let Err(invalid) = o.spec.validate() {
        return decision(HostPhase::Failed, REASON_INVALID_SPEC, invalid.to_string());
    }
    let script = o.spec.bootstrap.method == BootstrapMethod::Script;
    let started = script && matches!(o.job, JobState::Running | JobState::Failed(_));
    if !started && let Some(d) = zone_gate(o.previous, o.zone, &o.spec.zone_ref) {
        return d;
    }
    match o.spec.bootstrap.method {
        BootstrapMethod::Script => match o.job {
            JobState::Absent => decision(
                HostPhase::Pending,
                REASON_AWAITING_JOB,
                "waiting for the bootstrap Job to be created",
            ),
            JobState::Running => decision(
                HostPhase::Bootstrapping,
                REASON_JOB_RUNNING,
                "the bootstrap Job is running",
            ),
            JobState::Failed(JobFailure::ScriptDrift) => decision(
                HostPhase::Failed,
                REASON_SCRIPT_DRIFT,
                "the host's install script differs from its source and no placement \
                 credential is set; nothing ran",
            ),
            JobState::Failed(JobFailure::HostKeyRefused) => decision(
                HostPhase::Failed,
                REASON_HOST_KEY_REFUSED,
                "the host's key was refused before authenticating; nothing ran",
            ),
            JobState::Failed(JobFailure::ScriptUnpinned) => decision(
                HostPhase::Failed,
                REASON_SCRIPT_UNPINNED,
                "the script source holds content the bootstrap policy does not pin; nothing ran",
            ),
            JobState::Failed(JobFailure::Other) => decision(
                HostPhase::Failed,
                REASON_JOB_FAILED,
                "the bootstrap Job failed",
            ),
            JobState::Succeeded => from_provider(o.previous, o.provider, HostPhase::Bootstrapping),
        },
        // An adopted host has no Job; whatever Job state is observed is not its own.
        BootstrapMethod::None => from_provider(o.previous, o.provider, HostPhase::Pending),
    }
}

/// A missing or unsupported zone stops a host that has not started (ADR-0009 decision 4):
/// `Pending`, or `Degraded` for a host that was ready.
fn zone_gate(previous: Option<HostPhase>, zone: ZoneState, name: &str) -> Option<Decision> {
    let (reason, message) = match zone {
        ZoneState::Supported => return None,
        ZoneState::NotFound => (
            REASON_ZONE_NOT_FOUND,
            format!("AvailabilityZone {name} does not exist"),
        ),
        ZoneState::Unsupported => (
            REASON_ZONE_UNSUPPORTED,
            format!("AvailabilityZone {name} is not supported; see its conditions"),
        ),
    };
    let phase = if was_ready(previous) {
        HostPhase::Degraded
    } else {
        HostPhase::Pending
    };
    Some(decision(phase, reason, message))
}

fn was_ready(previous: Option<HostPhase>) -> bool {
    matches!(previous, Some(HostPhase::Ready | HostPhase::Degraded))
}

/// The phase once the host is bootstrapped or adopted: the `Provider` decides. `waiting` is
/// the phase of a host that has never been ready.
fn from_provider(
    previous: Option<HostPhase>,
    provider: ProviderHealth,
    waiting: HostPhase,
) -> Decision {
    if provider.is_healthy() {
        return decision(
            HostPhase::Ready,
            REASON_PROVIDER_HEALTHY,
            "the host's Provider is Ready with an Alive heartbeat",
        );
    }
    if !was_ready(previous) {
        if provider == ProviderHealth::Ambiguous {
            return decision(
                waiting,
                REASON_PROVIDER_AMBIGUOUS,
                "more than one Provider carries the host's label; faubourg does not pick one",
            );
        }
        return decision(
            waiting,
            REASON_AWAITING_PROVIDER,
            "waiting for the host's Provider to be Ready with an Alive heartbeat",
        );
    }
    match provider {
        ProviderHealth::Missing => decision(
            HostPhase::Degraded,
            REASON_PROVIDER_MISSING,
            "the host's Provider no longer exists",
        ),
        ProviderHealth::NotReady => decision(
            HostPhase::Degraded,
            REASON_PROVIDER_NOT_READY,
            "the host's Provider is not Ready",
        ),
        ProviderHealth::Ambiguous => decision(
            HostPhase::Degraded,
            REASON_PROVIDER_AMBIGUOUS,
            "more than one Provider carries the host's label",
        ),
        ProviderHealth::NoHeartbeat | ProviderHealth::Healthy => decision(
            HostPhase::Degraded,
            REASON_HEARTBEAT_NOT_ALIVE,
            "the host's Provider heartbeat is not Alive",
        ),
    }
}

fn decision(phase: HostPhase, reason: &'static str, message: impl Into<String>) -> Decision {
    Decision {
        phase,
        reason,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faubourg_api::host::{BootstrapSpec, SecretName, SshSpec};

    const ALL_PHASES: [Option<HostPhase>; 6] = [
        None,
        Some(HostPhase::Pending),
        Some(HostPhase::Bootstrapping),
        Some(HostPhase::Ready),
        Some(HostPhase::Degraded),
        Some(HostPhase::Failed),
    ];
    const ALL_JOBS: [JobState; 7] = [
        JobState::Absent,
        JobState::Running,
        JobState::Succeeded,
        JobState::Failed(JobFailure::ScriptDrift),
        JobState::Failed(JobFailure::HostKeyRefused),
        JobState::Failed(JobFailure::ScriptUnpinned),
        JobState::Failed(JobFailure::Other),
    ];
    const ALL_HEALTH: [ProviderHealth; 5] = [
        ProviderHealth::Missing,
        ProviderHealth::NotReady,
        ProviderHealth::NoHeartbeat,
        ProviderHealth::Ambiguous,
        ProviderHealth::Healthy,
    ];
    const UNHEALTHY: [ProviderHealth; 4] = [
        ProviderHealth::Missing,
        ProviderHealth::NotReady,
        ProviderHealth::NoHeartbeat,
        ProviderHealth::Ambiguous,
    ];
    const ALL_ZONES: [ZoneState; 3] = [
        ZoneState::Supported,
        ZoneState::NotFound,
        ZoneState::Unsupported,
    ];

    fn script() -> HostSpec {
        HostSpec {
            zone_ref: "az1".into(),
            address: "hv01.home.example".into(),
            ssh: Some(SshSpec {
                port: 22,
                secret_ref: SecretName {
                    name: "hv01-ssh".into(),
                },
                ..SshSpec::default()
            }),
            bootstrap: BootstrapSpec::default(),
            cordoned: false,
        }
    }

    fn adopted() -> HostSpec {
        HostSpec {
            ssh: None,
            bootstrap: BootstrapSpec {
                method: BootstrapMethod::None,
                script_ref: None,
            },
            ..script()
        }
    }

    fn invalid() -> HostSpec {
        HostSpec {
            address: "-oProxyCommand=sh".into(),
            ..script()
        }
    }

    fn run(
        spec: &HostSpec,
        previous: Option<HostPhase>,
        job: JobState,
        provider: ProviderHealth,
    ) -> Decision {
        in_zone(spec, ZoneState::Supported, previous, job, provider)
    }

    fn in_zone(
        spec: &HostSpec,
        zone: ZoneState,
        previous: Option<HostPhase>,
        job: JobState,
        provider: ProviderHealth,
    ) -> Decision {
        decide(&Observed {
            spec,
            previous,
            zone,
            job,
            provider,
        })
    }

    /// The reason of a host still waiting for its Provider.
    fn awaiting(provider: ProviderHealth) -> &'static str {
        if provider == ProviderHealth::Ambiguous {
            REASON_PROVIDER_AMBIGUOUS
        } else {
            REASON_AWAITING_PROVIDER
        }
    }

    #[test]
    fn the_fixtures_validate_as_intended() {
        assert!(script().validate().is_ok());
        assert!(adopted().validate().is_ok());
        assert!(invalid().validate().is_err());
    }

    #[test]
    fn a_refused_spec_fails_whatever_the_cluster_says() {
        let spec = invalid();
        for previous in ALL_PHASES {
            for job in ALL_JOBS {
                for provider in ALL_HEALTH {
                    let d = run(&spec, previous, job, provider);
                    assert_eq!(
                        d.phase,
                        HostPhase::Failed,
                        "{previous:?} {job:?} {provider:?}"
                    );
                    assert_eq!(d.reason, REASON_INVALID_SPEC);
                    assert!(d.message.contains("spec.address"), "{}", d.message);
                }
            }
        }
    }

    #[test]
    fn nothing_reaches_ready_outside_a_supported_zone() {
        for spec in [script(), adopted()] {
            for zone in [ZoneState::NotFound, ZoneState::Unsupported] {
                for previous in ALL_PHASES {
                    for job in ALL_JOBS {
                        for provider in ALL_HEALTH {
                            let d = in_zone(&spec, zone, previous, job, provider);
                            assert_ne!(d.phase, HostPhase::Ready, "{zone:?} {job:?}");
                            assert_ne!(d.message, "");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_missing_zone_holds_a_host_that_has_not_started() {
        for (zone, reason) in [
            (ZoneState::NotFound, REASON_ZONE_NOT_FOUND),
            (ZoneState::Unsupported, REASON_ZONE_UNSUPPORTED),
        ] {
            for job in [JobState::Absent, JobState::Succeeded] {
                let d = in_zone(&script(), zone, None, job, ProviderHealth::Healthy);
                assert_eq!(d.phase, HostPhase::Pending, "{zone:?} {job:?}");
                assert_eq!(d.reason, reason);
                assert!(d.message.contains("az1"), "{}", d.message);
            }
            let d = in_zone(
                &adopted(),
                zone,
                None,
                JobState::Absent,
                ProviderHealth::Healthy,
            );
            assert_eq!(d.phase, HostPhase::Pending);
            assert_eq!(d.reason, reason);
        }
    }

    #[test]
    fn a_ready_host_whose_zone_goes_is_degraded() {
        for previous in [Some(HostPhase::Ready), Some(HostPhase::Degraded)] {
            let d = in_zone(
                &script(),
                ZoneState::NotFound,
                previous,
                JobState::Succeeded,
                ProviderHealth::Healthy,
            );
            assert_eq!(d.phase, HostPhase::Degraded);
            assert_eq!(d.reason, REASON_ZONE_NOT_FOUND);
        }
    }

    #[test]
    fn a_started_job_is_reported_whatever_the_zone() {
        for zone in ALL_ZONES {
            let d = in_zone(
                &script(),
                zone,
                None,
                JobState::Running,
                ProviderHealth::Missing,
            );
            assert_eq!(d.phase, HostPhase::Bootstrapping, "{zone:?}");
            let d = in_zone(
                &script(),
                zone,
                None,
                JobState::Failed(JobFailure::Other),
                ProviderHealth::Missing,
            );
            assert_eq!(d.phase, HostPhase::Failed, "{zone:?}");
        }
    }

    #[test]
    fn a_refused_host_key_has_its_own_reason() {
        let d = run(
            &script(),
            Some(HostPhase::Bootstrapping),
            JobState::Failed(JobFailure::HostKeyRefused),
            ProviderHealth::Missing,
        );
        assert_eq!(d.phase, HostPhase::Failed);
        assert_eq!(d.reason, REASON_HOST_KEY_REFUSED);
    }

    #[test]
    fn an_ambiguous_provider_is_never_picked() {
        let d = run(
            &adopted(),
            None,
            JobState::Absent,
            ProviderHealth::Ambiguous,
        );
        assert_eq!(d.phase, HostPhase::Pending);
        assert_eq!(d.reason, REASON_PROVIDER_AMBIGUOUS);
        let d = run(
            &adopted(),
            Some(HostPhase::Ready),
            JobState::Absent,
            ProviderHealth::Ambiguous,
        );
        assert_eq!(d.phase, HostPhase::Degraded);
        assert_eq!(d.reason, REASON_PROVIDER_AMBIGUOUS);
    }

    #[test]
    fn nothing_reaches_ready_without_a_healthy_provider() {
        for spec in [script(), adopted(), invalid()] {
            for previous in ALL_PHASES {
                for job in ALL_JOBS {
                    for provider in UNHEALTHY {
                        let d = run(&spec, previous, job, provider);
                        assert_ne!(
                            d.phase,
                            HostPhase::Ready,
                            "{previous:?} {job:?} {provider:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn every_decision_has_a_message() {
        for spec in [script(), adopted(), invalid()] {
            for previous in ALL_PHASES {
                for job in ALL_JOBS {
                    for provider in ALL_HEALTH {
                        let d = run(&spec, previous, job, provider);
                        assert!(!d.message.is_empty(), "{d:?}");
                        assert!(!d.reason.is_empty(), "{d:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn no_job_yet_is_pending() {
        for provider in ALL_HEALTH {
            let d = run(&script(), None, JobState::Absent, provider);
            assert_eq!(d.phase, HostPhase::Pending, "{provider:?}");
            assert_eq!(d.reason, REASON_AWAITING_JOB);
        }
    }

    #[test]
    fn a_running_job_is_bootstrapping_even_after_ready() {
        for previous in ALL_PHASES {
            let d = run(
                &script(),
                previous,
                JobState::Running,
                ProviderHealth::Healthy,
            );
            assert_eq!(d.phase, HostPhase::Bootstrapping, "{previous:?}");
            assert_eq!(d.reason, REASON_JOB_RUNNING);
        }
    }

    #[test]
    fn a_failed_job_fails_the_host() {
        for provider in ALL_HEALTH {
            let d = run(
                &script(),
                Some(HostPhase::Bootstrapping),
                JobState::Failed(JobFailure::Other),
                provider,
            );
            assert_eq!(d.phase, HostPhase::Failed);
            assert_eq!(d.reason, REASON_JOB_FAILED);
        }
    }

    #[test]
    fn script_drift_has_its_own_reason() {
        let d = run(
            &script(),
            Some(HostPhase::Bootstrapping),
            JobState::Failed(JobFailure::ScriptDrift),
            ProviderHealth::Missing,
        );
        assert_eq!(d.phase, HostPhase::Failed);
        assert_eq!(d.reason, REASON_SCRIPT_DRIFT);
    }

    #[test]
    fn success_is_the_provider_coming_up_not_an_exit_code() {
        for provider in UNHEALTHY {
            for previous in [
                None,
                Some(HostPhase::Pending),
                Some(HostPhase::Bootstrapping),
            ] {
                let d = run(&script(), previous, JobState::Succeeded, provider);
                assert_eq!(
                    d.phase,
                    HostPhase::Bootstrapping,
                    "{previous:?} {provider:?}"
                );
                assert_eq!(d.reason, awaiting(provider));
            }
        }
        let d = run(
            &script(),
            Some(HostPhase::Bootstrapping),
            JobState::Succeeded,
            ProviderHealth::Healthy,
        );
        assert_eq!(d.phase, HostPhase::Ready);
        assert_eq!(d.reason, REASON_PROVIDER_HEALTHY);
    }

    #[test]
    fn a_ready_host_degrades_with_the_providers_reason() {
        for previous in [Some(HostPhase::Ready), Some(HostPhase::Degraded)] {
            for (provider, reason) in [
                (ProviderHealth::Missing, REASON_PROVIDER_MISSING),
                (ProviderHealth::NotReady, REASON_PROVIDER_NOT_READY),
                (ProviderHealth::NoHeartbeat, REASON_HEARTBEAT_NOT_ALIVE),
            ] {
                let d = run(&script(), previous, JobState::Succeeded, provider);
                assert_eq!(d.phase, HostPhase::Degraded, "{previous:?} {provider:?}");
                assert_eq!(d.reason, reason);
            }
        }
    }

    #[test]
    fn a_degraded_host_recovers() {
        let d = run(
            &script(),
            Some(HostPhase::Degraded),
            JobState::Succeeded,
            ProviderHealth::Healthy,
        );
        assert_eq!(d.phase, HostPhase::Ready);
    }

    #[test]
    fn an_adopted_host_follows_its_provider_and_ignores_jobs() {
        for job in ALL_JOBS {
            let d = run(&adopted(), None, job, ProviderHealth::Healthy);
            assert_eq!(d.phase, HostPhase::Ready, "{job:?}");
            for provider in UNHEALTHY {
                let d = run(&adopted(), Some(HostPhase::Pending), job, provider);
                assert_eq!(d.phase, HostPhase::Pending, "{job:?} {provider:?}");
                assert_eq!(d.reason, awaiting(provider));
                let d = run(&adopted(), Some(HostPhase::Ready), job, provider);
                assert_eq!(d.phase, HostPhase::Degraded, "{job:?} {provider:?}");
            }
        }
    }
}
