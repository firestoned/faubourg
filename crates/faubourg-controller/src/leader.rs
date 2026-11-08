// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! Leader election through a `coordination.k8s.io/v1` Lease (ADR-0014).
//!
//! Only the holder of the Lease runs the controller. Acquiring and taking over are conditional
//! writes (a create, or a replace carrying the `resourceVersion` that was read), so of two
//! candidates exactly one wins. The leader must renew within [`RENEW_DEADLINE`], shorter than
//! [`LEASE_DURATION`], or it stops: a standby can take over only after the old leader stopped
//! acting, given clock skew under the difference.
//!
//! Every decision is a pure function here; the loops only read, write and sleep.

use std::time::{Duration, Instant};

use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, ObjectMeta};
use k8s_openapi::jiff::Timestamp;
use kube::Api;
use kube::api::PostParams;
use tracing::{debug, info, warn};

/// The Lease's name, in the controller's namespace.
pub const LEASE_NAME: &str = "faubourg-controller";
/// How long a holder's lease lasts after its last renewal.
pub const LEASE_DURATION: Duration = Duration::from_secs(15);
/// How often the leader renews.
pub const RENEW_PERIOD: Duration = Duration::from_secs(5);
/// The leader stops if it has not renewed for this long: before its lease can lapse.
pub const RENEW_DEADLINE: Duration = Duration::from_secs(10);
/// How often a candidate, or a leader whose renewal failed, tries again.
pub const RETRY_PERIOD: Duration = Duration::from_secs(2);

/// A candidate: where the Lease is, and who it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Election {
    /// The Lease's namespace.
    pub namespace: String,
    /// This replica's identity, its pod name.
    pub identity: String,
}

/// What a candidate does next, from the Lease it read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// There is no Lease: create it.
    Create,
    /// It is ours: renew it.
    Renew,
    /// Another holder renewed within its lease: wait.
    Wait,
    /// Released, or its holder has not renewed within its lease: replace it.
    TakeOver,
}

/// Decides the next step at `now`. A lease whose expiry is exactly `now` is still held: a
/// strict comparison would race the holder's renewal.
pub fn next(now: Timestamp, lease: Option<&Lease>, identity: &str) -> Next {
    let Some(lease) = lease else {
        return Next::Create;
    };
    let spec = lease.spec.as_ref();
    let holder = spec
        .and_then(|s| s.holder_identity.as_deref())
        .unwrap_or_default();
    if holder.is_empty() {
        return Next::TakeOver;
    }
    if holder == identity {
        return Next::Renew;
    }
    let Some(MicroTime(renewed)) = spec.and_then(|s| s.renew_time.clone()) else {
        return Next::TakeOver;
    };
    // The holder's own declared duration, never less than a second.
    let duration = spec
        .and_then(|s| s.lease_duration_seconds)
        .filter(|d| *d > 0)
        .map_or(
            i64::try_from(LEASE_DURATION.as_secs()).unwrap_or(15),
            i64::from,
        );
    if now.as_second() <= renewed.as_second() + duration {
        Next::Wait
    } else {
        Next::TakeOver
    }
}

/// True when `identity` holds `lease`.
pub fn held(lease: &Lease, identity: &str) -> bool {
    lease
        .spec
        .as_ref()
        .and_then(|s| s.holder_identity.as_deref())
        == Some(identity)
}

/// True while the leader may still act: it renewed within [`RENEW_DEADLINE`].
pub fn within_deadline(last_renewal: Instant, now: Instant) -> bool {
    now.saturating_duration_since(last_renewal) < RENEW_DEADLINE
}

/// The Lease that claims it for `e`: a new one, or `current` taken over, keeping the
/// `resourceVersion` read so the write is conditional.
pub fn claim(current: Option<&Lease>, e: &Election, now: Timestamp) -> Lease {
    let previous = current.and_then(|l| l.spec.as_ref());
    let transitions = previous.and_then(|s| s.lease_transitions).unwrap_or(0);
    let changes_hands = previous
        .and_then(|s| s.holder_identity.as_deref())
        .is_some_and(|h| !h.is_empty() && h != e.identity);
    Lease {
        metadata: ObjectMeta {
            name: Some(LEASE_NAME.into()),
            namespace: Some(e.namespace.clone()),
            resource_version: current.and_then(|l| l.metadata.resource_version.clone()),
            ..ObjectMeta::default()
        },
        spec: Some(LeaseSpec {
            holder_identity: Some(e.identity.clone()),
            lease_duration_seconds: Some(i32::try_from(LEASE_DURATION.as_secs()).unwrap_or(15)),
            acquire_time: Some(MicroTime(now)),
            renew_time: Some(MicroTime(now)),
            lease_transitions: Some(transitions + i32::from(changes_hands)),
            ..LeaseSpec::default()
        }),
    }
}

/// `lease`, renewed at `now`.
pub fn renewed(mut lease: Lease, now: Timestamp) -> Lease {
    if let Some(s) = lease.spec.as_mut() {
        s.renew_time = Some(MicroTime(now));
    }
    lease
}

/// `lease`, released: no holder, so a standby takes it at its next try.
pub fn released(mut lease: Lease) -> Lease {
    if let Some(s) = lease.spec.as_mut() {
        s.holder_identity = None;
    }
    lease
}

/// Why the leader stopped leading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lost {
    /// Another replica holds the Lease.
    TakenOver,
    /// No renewal succeeded within [`RENEW_DEADLINE`].
    DeadlineMissed,
}

/// Waits until `e` holds the Lease. Errors are logged and retried: a candidate keeps trying.
pub async fn acquire(api: &Api<Lease>, e: &Election) {
    info!(identity = %e.identity, lease = LEASE_NAME, "waiting to lead");
    loop {
        let current = match api.get_opt(LEASE_NAME).await {
            Ok(c) => c,
            Err(err) => {
                warn!(error = %err, "cannot read the leader Lease; retrying");
                tokio::time::sleep(RETRY_PERIOD).await;
                continue;
            }
        };
        let step = next(Timestamp::now(), current.as_ref(), &e.identity);
        debug!(?step, "leader election");
        let written = match step {
            Next::Wait => {
                tokio::time::sleep(RETRY_PERIOD).await;
                continue;
            }
            Next::Create => {
                let lease = claim(None, e, Timestamp::now());
                api.create(&PostParams::default(), &lease).await
            }
            Next::Renew | Next::TakeOver => {
                let lease = claim(current.as_ref(), e, Timestamp::now());
                api.replace(LEASE_NAME, &PostParams::default(), &lease)
                    .await
            }
        };
        match written {
            Ok(_) => {
                info!(identity = %e.identity, ?step, "leading");
                return;
            }
            // Another candidate wrote first: it won.
            Err(kube::Error::Api(s)) if s.code == 409 => debug!("lost the race for the Lease"),
            Err(err) => warn!(error = %err, "cannot write the leader Lease; retrying"),
        }
        tokio::time::sleep(RETRY_PERIOD).await;
    }
}

/// Renews the Lease until leadership is lost; never returns otherwise. The caller stops.
pub async fn hold(api: &Api<Lease>, e: &Election) -> Lost {
    let mut last = Instant::now();
    let mut wait = RENEW_PERIOD;
    loop {
        tokio::time::sleep(wait).await;
        let left = RENEW_DEADLINE.saturating_sub(last.elapsed());
        match tokio::time::timeout(left, renew_once(api, e)).await {
            Ok(Ok(true)) => {
                last = Instant::now();
                wait = RENEW_PERIOD;
            }
            Ok(Ok(false)) => return Lost::TakenOver,
            Ok(Err(err)) => {
                warn!(error = %err, "cannot renew the leader Lease; retrying");
                wait = RETRY_PERIOD;
            }
            Err(_) => warn!("renewing the leader Lease timed out"),
        }
        if !within_deadline(last, Instant::now()) {
            return Lost::DeadlineMissed;
        }
    }
}

/// One renewal: `Ok(false)` when the Lease is no longer ours.
async fn renew_once(api: &Api<Lease>, e: &Election) -> Result<bool, kube::Error> {
    let Some(lease) = api.get_opt(LEASE_NAME).await? else {
        return Ok(false);
    };
    if !held(&lease, &e.identity) {
        return Ok(false);
    }
    api.replace(
        LEASE_NAME,
        &PostParams::default(),
        &renewed(lease, Timestamp::now()),
    )
    .await?;
    Ok(true)
}

/// Releases the Lease if `e` still holds it, so a standby takes over at once. Best effort:
/// failing, the lease runs out instead.
pub async fn release(api: &Api<Lease>, e: &Election) {
    let Ok(Some(lease)) = api.get_opt(LEASE_NAME).await else {
        return;
    };
    if !held(&lease, &e.identity) {
        return;
    }
    match api
        .replace(LEASE_NAME, &PostParams::default(), &released(lease))
        .await
    {
        Ok(_) => info!(identity = %e.identity, "released the leader Lease"),
        Err(err) => warn!(error = %err, "cannot release the leader Lease; it will run out"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(second).unwrap()
    }

    fn election(identity: &str) -> Election {
        Election {
            namespace: "faubourg-system".into(),
            identity: identity.into(),
        }
    }

    fn lease(holder: Option<&str>, renewed: Option<i64>, duration: Option<i32>) -> Lease {
        Lease {
            metadata: ObjectMeta {
                name: Some(LEASE_NAME.into()),
                resource_version: Some("42".into()),
                ..ObjectMeta::default()
            },
            spec: Some(LeaseSpec {
                holder_identity: holder.map(str::to_string),
                renew_time: renewed.map(|s| MicroTime(at(s))),
                lease_duration_seconds: duration,
                lease_transitions: Some(3),
                ..LeaseSpec::default()
            }),
        }
    }

    #[test]
    fn no_lease_is_created_ours_is_renewed() {
        assert_eq!(next(at(100), None, "a"), Next::Create);
        let ours = lease(Some("a"), Some(0), Some(15));
        assert_eq!(
            next(at(1_000), Some(&ours), "a"),
            Next::Renew,
            "even if it ran out"
        );
    }

    #[test]
    fn a_live_holder_is_waited_for_until_its_lease_runs_out() {
        let theirs = lease(Some("b"), Some(100), Some(15));
        assert_eq!(next(at(110), Some(&theirs), "a"), Next::Wait);
        assert_eq!(
            next(at(115), Some(&theirs), "a"),
            Next::Wait,
            "expiring now is still held"
        );
        assert_eq!(next(at(116), Some(&theirs), "a"), Next::TakeOver);
    }

    #[test]
    fn the_holders_declared_duration_counts() {
        let long = lease(Some("b"), Some(100), Some(60));
        assert_eq!(next(at(150), Some(&long), "a"), Next::Wait);
        for bad in [None, Some(0), Some(-5)] {
            let l = lease(Some("b"), Some(100), bad);
            assert_eq!(
                next(at(115), Some(&l), "a"),
                Next::Wait,
                "{bad:?}: the default"
            );
            assert_eq!(next(at(116), Some(&l), "a"), Next::TakeOver, "{bad:?}");
        }
    }

    #[test]
    fn a_released_or_unrenewed_lease_is_taken_over() {
        for l in [
            lease(None, Some(100), Some(15)),
            lease(Some(""), Some(100), Some(15)),
            lease(Some("b"), None, Some(15)),
        ] {
            assert_eq!(next(at(101), Some(&l), "a"), Next::TakeOver, "{l:?}");
        }
        let empty = Lease {
            spec: None,
            ..lease(None, None, None)
        };
        assert_eq!(next(at(101), Some(&empty), "a"), Next::TakeOver);
    }

    #[test]
    fn a_claim_is_conditional_and_counts_a_change_of_hands() {
        let theirs = lease(Some("b"), Some(100), Some(15));
        let c = claim(Some(&theirs), &election("a"), at(200));
        assert_eq!(
            c.metadata.resource_version.as_deref(),
            Some("42"),
            "a replace fails if anyone wrote since the read"
        );
        let s = c.spec.unwrap();
        assert_eq!(s.holder_identity.as_deref(), Some("a"));
        assert_eq!(s.renew_time, Some(MicroTime(at(200))));
        assert_eq!(s.lease_duration_seconds, Some(15));
        assert_eq!(s.lease_transitions, Some(4));
        let released = lease(None, Some(100), Some(15));
        let s = claim(Some(&released), &election("a"), at(200))
            .spec
            .unwrap();
        assert_eq!(
            s.lease_transitions,
            Some(3),
            "a released lease changes no hands"
        );
        let fresh = claim(None, &election("a"), at(200));
        assert_eq!(
            fresh.metadata.resource_version, None,
            "a create is conditional itself"
        );
        assert_eq!(fresh.metadata.namespace.as_deref(), Some("faubourg-system"));
        assert_eq!(fresh.spec.unwrap().lease_transitions, Some(0));
    }

    #[test]
    fn renewing_moves_only_the_renew_time_and_releasing_clears_the_holder() {
        let ours = lease(Some("a"), Some(100), Some(15));
        let r = renewed(ours.clone(), at(105));
        assert_eq!(r.metadata.resource_version.as_deref(), Some("42"));
        let s = r.spec.unwrap();
        assert_eq!(s.renew_time, Some(MicroTime(at(105))));
        assert_eq!(s.holder_identity.as_deref(), Some("a"));
        let gone = released(ours);
        assert!(!held(&gone, "a"));
        assert_eq!(next(at(101), Some(&gone), "b"), Next::TakeOver);
    }

    #[test]
    fn the_leader_stops_before_its_lease_can_run_out() {
        assert!(RENEW_PERIOD < RENEW_DEADLINE && RENEW_DEADLINE < LEASE_DURATION);
        let t = Instant::now();
        assert!(within_deadline(t, t + Duration::from_secs(9)));
        assert!(!within_deadline(t, t + RENEW_DEADLINE));
        assert!(
            within_deadline(t + Duration::from_secs(1), t),
            "a clock that went back"
        );
    }
}
