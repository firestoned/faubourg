// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! What a host's banlieue `Provider` says about the host (ADR-0007).
//!
//! faubourg adds no heartbeat of its own. A host is healthy while its `Provider` is
//! `Ready=True` for the generation it carries **and** a second signal says the host itself is
//! up (ADR-0018 decision 8):
//!
//! - an `External` provider runs on the host and renews its Lease there, so banlieue's
//!   operator reporting that heartbeat `Alive` is the host's (banlieue ADR-0102);
//! - a `Managed` provider runs in the cluster, so its Lease is a pod's. The host is up when the
//!   provider reports `ProviderReachable=True` for the generation it carries.
//!
//! Anything else, including a signal that is simply absent, is unhealthy: fail closed.

use banlieue_api::banlieue::{Heartbeat, Provider, ProviderDeployment};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;

/// The `Provider` condition type faubourg reads.
pub const CONDITION_READY: &str = "Ready";
/// The condition a Managed provider sets when it reaches its endpoint (banlieue's
/// `PROVIDER_REACHABLE`).
pub const CONDITION_PROVIDER_REACHABLE: &str = "ProviderReachable";

/// A host's health as its `Provider` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderHealth {
    /// There is no `Provider` for the host.
    Missing,
    /// The `Provider` exists but is not `Ready=True` for its current generation.
    NotReady,
    /// `Ready=True`, but the heartbeat is `Stale`, `Absent`, or not yet evaluated.
    NoHeartbeat,
    /// `Ready=True`, but a Managed provider does not report reaching the host.
    Unreachable,
    /// More than one `Provider` carries the host's label (ADR-0009 decision 5).
    Ambiguous,
    /// `Ready=True` and the heartbeat is `Alive`.
    Healthy,
}

impl ProviderHealth {
    /// Reads the `Provider`s labelled with a host's name: none is [`Self::Missing`], more
    /// than one is [`Self::Ambiguous`]. faubourg never picks one of several.
    pub fn of_labelled(providers: &[Provider]) -> Self {
        Self::of_labelled_as(providers, ProviderDeployment::External)
    }

    /// [`Self::of_labelled`], for a provider deployed as `mode`.
    pub fn of_labelled_as(providers: &[Provider], mode: ProviderDeployment) -> Self {
        match providers {
            [] => Self::Missing,
            [one] => Self::of_as(Some(one), mode),
            _ => Self::Ambiguous,
        }
    }

    /// Reads a host's `Provider`, or its absence, for a provider that runs on the host.
    pub fn of(provider: Option<&Provider>) -> Self {
        Self::of_as(provider, ProviderDeployment::External)
    }

    /// Reads a host's `Provider`, or its absence, for a provider deployed as `mode`.
    pub fn of_as(provider: Option<&Provider>, mode: ProviderDeployment) -> Self {
        let Some(p) = provider else {
            return Self::Missing;
        };
        let Some(status) = p.status.as_ref() else {
            return Self::NotReady;
        };
        let generation = p.metadata.generation;
        let is_true = |type_: &str| {
            status
                .conditions
                .iter()
                .any(|c| c.type_ == type_ && current_and_true(c, generation))
        };
        if !is_true(CONDITION_READY) {
            return Self::NotReady;
        }
        if mode == ProviderDeployment::Managed {
            return if is_true(CONDITION_PROVIDER_REACHABLE) {
                Self::Healthy
            } else {
                Self::Unreachable
            };
        }
        let heartbeat = status.workload.as_ref().and_then(|w| w.heartbeat);
        if heartbeat == Some(Heartbeat::Alive) {
            Self::Healthy
        } else {
            Self::NoHeartbeat
        }
    }

    /// True only for [`ProviderHealth::Healthy`].
    pub fn is_healthy(self) -> bool {
        self == Self::Healthy
    }
}

/// `True`, and written for the Provider's current generation: a condition written for an older
/// spec says nothing about this one.
fn current_and_true(c: &Condition, generation: Option<i64>) -> bool {
    c.status == "True"
        && match (c.observed_generation, generation) {
            (Some(observed), Some(current)) => observed >= current,
            _ => true,
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn provider(generation: i64, status: Option<Value>) -> Provider {
        let mut v = json!({
            "apiVersion": "banlieue.io/v1alpha1",
            "kind": "Provider",
            "metadata": { "name": "hv01", "namespace": "banlieue-system", "generation": generation },
            "spec": {
                "providerClassRef": { "name": "cloud-hypervisor" },
                "connection": { "endpoint": "hv01.home.example" }
            }
        });
        if let Some(s) = status {
            v["status"] = s;
        }
        serde_json::from_value(v).expect("a valid Provider")
    }

    fn ready(status: &str, observed_generation: Option<i64>) -> Value {
        let mut c = json!({
            "type": "Ready",
            "status": status,
            "reason": "Test",
            "message": "",
            "lastTransitionTime": "2026-11-07T00:00:00Z"
        });
        if let Some(g) = observed_generation {
            c["observedGeneration"] = json!(g);
        }
        c
    }

    fn status(ready: Option<Value>, heartbeat: Option<&str>) -> Value {
        let mut s = json!({});
        if let Some(r) = ready {
            s["conditions"] = json!([r]);
        }
        if let Some(h) = heartbeat {
            s["workload"] = json!({ "namespace": "banlieue-system", "heartbeat": h });
        }
        s
    }

    #[test]
    fn no_provider_is_missing() {
        assert_eq!(ProviderHealth::of(None), ProviderHealth::Missing);
    }

    #[test]
    fn ready_and_alive_is_healthy() {
        let p = provider(3, Some(status(Some(ready("True", Some(3))), Some("Alive"))));
        assert_eq!(ProviderHealth::of(Some(&p)), ProviderHealth::Healthy);
        assert!(ProviderHealth::of(Some(&p)).is_healthy());
    }

    #[test]
    fn a_ready_condition_without_observed_generation_counts() {
        let p = provider(3, Some(status(Some(ready("True", None)), Some("Alive"))));
        assert_eq!(ProviderHealth::of(Some(&p)), ProviderHealth::Healthy);
    }

    #[test]
    fn no_status_is_not_ready() {
        let p = provider(1, None);
        assert_eq!(ProviderHealth::of(Some(&p)), ProviderHealth::NotReady);
    }

    #[test]
    fn ready_false_or_unknown_is_not_ready_whatever_the_heartbeat() {
        for s in ["False", "Unknown", "true", ""] {
            let p = provider(1, Some(status(Some(ready(s, Some(1))), Some("Alive"))));
            assert_eq!(
                ProviderHealth::of(Some(&p)),
                ProviderHealth::NotReady,
                "Ready={s:?}"
            );
        }
    }

    #[test]
    fn no_ready_condition_is_not_ready() {
        let p = provider(1, Some(status(None, Some("Alive"))));
        assert_eq!(ProviderHealth::of(Some(&p)), ProviderHealth::NotReady);
    }

    #[test]
    fn a_ready_condition_for_an_older_generation_is_not_ready() {
        let p = provider(4, Some(status(Some(ready("True", Some(3))), Some("Alive"))));
        assert_eq!(ProviderHealth::of(Some(&p)), ProviderHealth::NotReady);
    }

    #[test]
    fn ready_without_a_live_heartbeat_has_no_heartbeat() {
        for hb in [Some("Stale"), Some("Absent"), None] {
            let p = provider(1, Some(status(Some(ready("True", Some(1))), hb)));
            assert_eq!(
                ProviderHealth::of(Some(&p)),
                ProviderHealth::NoHeartbeat,
                "heartbeat={hb:?}"
            );
        }
    }

    fn managed(generation: i64, conditions: &[Value]) -> Provider {
        // A Managed provider's Lease is a pod's: Alive says nothing about the host.
        provider(
            generation,
            Some(json!({ "conditions": conditions,
                         "workload": { "namespace": "banlieue-system", "heartbeat": "Alive" } })),
        )
    }

    fn reachable(status: &str, observed_generation: i64) -> Value {
        let mut c = ready(status, Some(observed_generation));
        c["type"] = json!("ProviderReachable");
        c
    }

    #[test]
    fn a_managed_provider_is_healthy_when_ready_and_reaching_its_host() {
        let p = managed(2, &[ready("True", Some(2)), reachable("True", 2)]);
        assert_eq!(
            ProviderHealth::of_as(Some(&p), ProviderDeployment::Managed),
            ProviderHealth::Healthy
        );
    }

    #[test]
    fn a_managed_provider_that_does_not_reach_its_host_is_unreachable_whatever_its_lease() {
        for conditions in [
            vec![ready("True", Some(2))],
            vec![ready("True", Some(2)), reachable("False", 2)],
            vec![ready("True", Some(2)), reachable("True", 1)],
        ] {
            let p = managed(2, &conditions);
            assert_eq!(
                ProviderHealth::of_as(Some(&p), ProviderDeployment::Managed),
                ProviderHealth::Unreachable,
                "{conditions:?}"
            );
        }
        let p = managed(2, &[ready("False", Some(2)), reachable("True", 2)]);
        assert_eq!(
            ProviderHealth::of_as(Some(&p), ProviderDeployment::Managed),
            ProviderHealth::NotReady
        );
    }

    #[test]
    fn only_healthy_is_healthy() {
        for h in [
            ProviderHealth::Missing,
            ProviderHealth::NotReady,
            ProviderHealth::NoHeartbeat,
            ProviderHealth::Unreachable,
        ] {
            assert!(!h.is_healthy(), "{h:?}");
        }
    }

    #[test]
    fn the_host_has_exactly_one_labelled_provider_or_none_counts() {
        let healthy = || provider(3, Some(status(Some(ready("True", Some(3))), Some("Alive"))));
        assert_eq!(ProviderHealth::of_labelled(&[]), ProviderHealth::Missing);
        assert_eq!(
            ProviderHealth::of_labelled(&[healthy()]),
            ProviderHealth::Healthy
        );
        assert_eq!(
            ProviderHealth::of_labelled(&[healthy(), healthy()]),
            ProviderHealth::Ambiguous
        );
        assert!(!ProviderHealth::Ambiguous.is_healthy());
    }
}
