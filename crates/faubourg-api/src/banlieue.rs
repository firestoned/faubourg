// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! What faubourg must know of banlieue's naming (ADR-0011 decision 6).
//!
//! banlieue's operator names the `ServiceAccount` of an `External` Provider with
//! `banlieue_provider_sdk::naming::workload_name`, which is not published. This is the same
//! algorithm, pinned by a test against outputs of banlieue's own function. Ask banlieue to
//! publish the name in the Provider's status, or the function in `banlieue-api`, and delete
//! this copy.

/// Prefix of every provider workload name (banlieue's `WORKLOAD_NAME_PREFIX`).
pub const WORKLOAD_NAME_PREFIX: &str = "banlieue-provider";
/// Longest generated name: a label value.
const MAX_NAME_LEN: usize = 63;
const HASH_SUFFIX_LEN: usize = 8;

/// The `ServiceAccount` banlieue's operator creates for a Provider of `class`.
pub fn provider_service_account(class: &str, provider: &str) -> String {
    let name = format!("{WORKLOAD_NAME_PREFIX}-{class}-{provider}");
    if name.len() <= MAX_NAME_LEN {
        return name;
    }
    let keep = MAX_NAME_LEN - HASH_SUFFIX_LEN - 1;
    // Names are DNS labels, so ASCII: slicing by byte is slicing by character.
    let head = name[..keep].trim_end_matches('-');
    format!("{head}-{:08x}", fnv1a32(&name))
}

/// The prefix every `ServiceAccount` of `class` starts with: what the token policy admits.
pub fn service_account_prefix(class: &str) -> String {
    format!("{WORKLOAD_NAME_PREFIX}-{class}-")
}

fn fnv1a32(value: &str) -> u32 {
    value.bytes().fold(2_166_136_261_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(16_777_619)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Printed by banlieue's own `workload_name` (banlieue-provider-sdk at 3cb4387).
    const BANLIEUE: [(&str, &str, &str); 4] = [
        (
            "cloud-hypervisor",
            "hv01",
            "banlieue-provider-cloud-hypervisor-hv01",
        ),
        (
            "cloud-hypervisor",
            "a-host-name-long-enough-to-need-the-hash-suffix-x",
            "banlieue-provider-cloud-hypervisor-a-host-name-long-en-b9771b32",
        ),
        (
            "cloud-hypervisor",
            "hv-0123456789abcdef0123456789abcdef0123456789",
            "banlieue-provider-cloud-hypervisor-hv-0123456789abcdef-0d12ad11",
        ),
        ("other", "hv01", "banlieue-provider-other-hv01"),
    ];

    #[test]
    fn names_match_banlieues_own() {
        for (class, provider, want) in BANLIEUE {
            assert_eq!(
                provider_service_account(class, provider),
                want,
                "{provider}"
            );
        }
    }

    #[test]
    fn every_name_starts_with_the_class_prefix() {
        for (class, provider, _) in BANLIEUE {
            let name = provider_service_account(class, provider);
            assert!(name.starts_with(&service_account_prefix(class)), "{class} {provider}");
            assert!(name.len() <= 63);
        }
    }
}
