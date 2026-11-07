// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! Writes faubourg's CRDs as YAML: to stdout, or one file per CRD into `--out-dir` under the
//! kubebuilder name `<group>_<plural>.yaml`, which is how `make crds` populates
//! `deploy/crds/`.

use std::path::PathBuf;

use clap::Parser;
use faubourg_api::crd;

#[derive(Parser)]
#[command(
    name = "crdgen",
    about = "Render faubourg's CRDs from their Rust types"
)]
struct Cli {
    /// Write `<group>_<plural>.yaml` files here instead of printing to stdout.
    #[arg(long)]
    out_dir: Option<PathBuf>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    for c in crd::crds() {
        let rendered = crd::render(&c)?;
        match &cli.out_dir {
            Some(dir) => {
                std::fs::create_dir_all(dir)?;
                let path = dir.join(crd::file_name(&c));
                std::fs::write(&path, rendered)?;
                eprintln!("wrote {}", path.display());
            }
            None => print!("{rendered}"),
        }
    }
    Ok(())
}
