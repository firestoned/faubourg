// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! The Host phase machine (ADR-0005 decision 6, ADR-0007 decision 1), as a pure function.
//!
//! | Phase | When |
//! | --- | --- |
//! | `Failed` | the spec is refused, or the current generation's bootstrap Job failed |
//! | `Pending` | nothing started: no Job yet, or an adopted host whose `Provider` never came up |
//! | `Bootstrapping` | the Job runs, or it succeeded and the `Provider` has not come up yet |
//! | `Ready` | the Job succeeded (or the host is adopted) and the `Provider` is healthy |
//! | `Degraded` | was `Ready` or `Degraded`, and the `Provider` is no longer healthy |
//!
//! Success is the provider coming up, not an exit code: no input reaches `Ready` unless
//! [`ProviderHealth::Healthy`]. Validation is checked first, so a refused spec is `Failed`
//! whatever the cluster says.

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
    /// Anything else: unreachable host, refused host key, a non-zero script exit.
    Other,
}

/// Everything the phase depends on.
#[derive(Clone, Copy, Debug)]
pub struct Observed<'a> {
    /// The Host's spec.
    pub spec: &'a HostSpec,
    /// The phase in the Host's status before this reconcile.
    pub previous: Option<HostPhase>,
    /// The current generation's bootstrap Job.
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
    let was_ready = matches!(previous, Some(HostPhase::Ready | HostPhase::Degraded));
    if !was_ready {
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
    const ALL_JOBS: [JobState; 5] = [
        JobState::Absent,
        JobState::Running,
        JobState::Succeeded,
        JobState::Failed(JobFailure::ScriptDrift),
        JobState::Failed(JobFailure::Other),
    ];
    const ALL_HEALTH: [ProviderHealth; 4] = [
        ProviderHealth::Missing,
        ProviderHealth::NotReady,
        ProviderHealth::NoHeartbeat,
        ProviderHealth::Healthy,
    ];
    const UNHEALTHY: [ProviderHealth; 3] = [
        ProviderHealth::Missing,
        ProviderHealth::NotReady,
        ProviderHealth::NoHeartbeat,
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
        decide(&Observed {
            spec,
            previous,
            job,
            provider,
        })
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
                assert_eq!(d.reason, REASON_AWAITING_PROVIDER);
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
                assert_eq!(d.reason, REASON_AWAITING_PROVIDER);
                let d = run(&adopted(), Some(HostPhase::Ready), job, provider);
                assert_eq!(d.phase, HostPhase::Degraded, "{job:?} {provider:?}");
            }
        }
    }
}
