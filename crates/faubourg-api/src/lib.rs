// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! faubourg's API: the `AvailabilityZone` and `Host` CRDs (ADR-0003), their validation
//! (fail closed, ADR-0004 and ADR-0005), and the SSH Secret contract (ADR-0006).
//!
//! The API group lives in the constants below and in the two `#[kube(group = ...)]`
//! attributes; a test keeps them in step.

pub mod banlieue;
pub mod bootstrap;
pub mod crd;
pub mod credentials;
pub mod host;
pub mod policy;
pub mod validation;
pub mod zone;

/// The project's name.
pub const PROJECT: &str = "faubourg";
/// The API group of every faubourg CRD.
pub const GROUP: &str = "faubourg.firestoned.io";
/// The served and stored version.
pub const VERSION: &str = "v1alpha1";

/// Label faubourg owns on each host's banlieue `Provider`: the zone it belongs to (ADR-0003).
pub const LABEL_ZONE: &str = "faubourg.firestoned.io/zone";
/// Label faubourg owns on each host's banlieue `Provider`: the `Host` it was created for.
pub const LABEL_HOST: &str = "faubourg.firestoned.io/host";
/// Label faubourg owns on each host's banlieue `Provider` while the host is cordoned.
pub const LABEL_CORDONED: &str = "faubourg.firestoned.io/cordoned";
/// banlieue's own cordon (banlieue ADR-0124): on a Provider, `"true"` keeps every new VM off it.
/// faubourg sets it with [`LABEL_CORDONED`]. A copy of `banlieue_api::banlieue::LABEL_UNSCHEDULABLE`,
/// which faubourg-api does not depend on; the controller's tests fail if the two differ.
pub const LABEL_BANLIEUE_UNSCHEDULABLE: &str = "banlieue.io/unschedulable";
/// Prefix of every label key faubourg owns; a zone's `providerLabels` may not use it.
pub const LABEL_PREFIX: &str = "faubourg.firestoned.io/";

#[cfg(test)]
mod tests {
    use super::*;
    use kube::Resource;

    #[test]
    fn group_constant_matches_the_derived_types() {
        assert_eq!(zone::AvailabilityZone::group(&()), GROUP);
        assert_eq!(host::Host::group(&()), GROUP);
        assert_eq!(zone::AvailabilityZone::version(&()), VERSION);
        assert_eq!(host::Host::version(&()), VERSION);
    }

    #[test]
    fn label_keys_live_under_the_group() {
        for key in [LABEL_ZONE, LABEL_HOST, LABEL_CORDONED] {
            assert!(key.starts_with(LABEL_PREFIX), "{key}");
        }
        assert_eq!(LABEL_PREFIX, format!("{GROUP}/"));
        assert!(GROUP.starts_with(PROJECT));
    }
}
