// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! Prints banlieue's `Provider` and `ProviderClass` CRDs, as the pinned `banlieue-api` defines
//! them, so the kind tests can install them in a throwaway cluster without banlieue.
//!
//! Usage: `cargo run -q -p faubourg-controller --example provider-crd`

use banlieue_api::banlieue::{Provider, ProviderClass};
use kube::CustomResourceExt;

fn main() -> Result<(), serde_json::Error> {
    println!("---\n{}", serde_json::to_string(&Provider::crd())?);
    println!("---\n{}", serde_json::to_string(&ProviderClass::crd())?);
    Ok(())
}
