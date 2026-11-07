// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! Prints banlieue's `Provider` CRD, as the pinned `banlieue-api` defines it, so
//! `scripts/controller-test.sh` can install it in a throwaway cluster without banlieue.
//!
//! Usage: `cargo run -q -p faubourg-controller --example provider-crd`

use banlieue_api::banlieue::Provider;
use kube::CustomResourceExt;

fn main() -> Result<(), serde_json::Error> {
    println!("{}", serde_json::to_string(&Provider::crd())?);
    Ok(())
}
