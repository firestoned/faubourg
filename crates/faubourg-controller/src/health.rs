// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! What a host's banlieue `Provider` says about the host (ADR-0007).
//!
//! faubourg adds no heartbeat of its own. A host is healthy while its `Provider` is
//! `Ready=True` for the generation it carries **and** banlieue's operator reports its Lease
//! heartbeat `Alive` (banlieue ADR-0102). Anything else, including a signal that is simply
//! absent, is unhealthy: fail closed.

use banlieue_api::banlieue::{Heartbeat, Provider};

/// The `Provider` condition type faubourg reads.
pub const CONDITION_READY: &str = "Ready";

/// A host's health as its `Provider` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderHealth {
    /// There is no `Provider` for the host.
    Missing,
    /// The `Provider` exists but is not `Ready=True` for its current generation.
    NotReady,
    /// `Ready=True`, but the heartbeat is `Stale`, `Absent`, or not yet evaluated.
    NoHeartbeat,
    /// More than one `Provider` carries the host's label (ADR-0009 decision 5).
    Ambiguous,
    /// `Ready=True` and the heartbeat is `Alive`.
    Healthy,
}

impl ProviderHealth {
    /// Reads the `Provider`s labelled with a host's name: none is [`Self::Missing`], more
    /// than one is [`Self::Ambiguous`]. faubourg never picks one of several.
    pub fn of_labelled(providers: &[Provider]) -> Self {
        match providers {
            [] => Self::Missing,
            [one] => Self::of(Some(one)),
            _ => Self::Ambiguous,
        }
    }

    /// Reads a host's `Provider`, or its absence.
    pub fn of(provider: Option<&Provider>) -> Self {
        let Some(p) = provider else {
            return Self::Missing;
        };
        let Some(status) = p.status.as_ref() else {
            return Self::NotReady;
        };
        let generation = p.metadata.generation;
        let ready = status.conditions.iter().any(|c| {
            c.type_ == CONDITION_READY
                && c.status == "True"
                && match (c.observed_generation, generation) {
                    // A condition written for an older spec says nothing about this one.
                    (Some(observed), Some(current)) => observed >= current,
                    _ => true,
                }
        });
        if !ready {
            return Self::NotReady;
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

    #[test]
    fn only_healthy_is_healthy() {
        for h in [
            ProviderHealth::Missing,
            ProviderHealth::NotReady,
            ProviderHealth::NoHeartbeat,
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
