// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! faubourg's Host controller (ADR-0009, roadmap 01).
//!
//! Pure functions first:
//!
//! - [`health`]: what a host's banlieue `Provider` says about the host (ADR-0007).
//! - [`phase`]: the Host phase machine, from its spec, zone, bootstrap Job and that health.
//! - [`job`]: the bootstrap Job a Host needs, in the shape the admission policy admits
//!   (ADR-0008), and how to read one back.
//! - [`provider`]: the host's banlieue Provider, as faubourg applies it (ADR-0011).
//! - [`plan`]: one reconcile, from everything it reads to everything it writes.
//! - [`config`]: the controller's flags, validated before it starts.
//!
//! [`controller`] is the only module that talks to a cluster. It reads what [`plan`] needs and
//! carries out what it returns, so every decision the controller makes is covered by a unit
//! test that needs no API server.

pub mod config;
pub mod controller;
pub mod health;
pub mod job;
pub mod leader;
pub mod phase;
pub mod plan;
pub mod provider;
pub mod zone;
