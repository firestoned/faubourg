// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! faubourg's controller logic, as pure functions first (roadmap 01).
//!
//! - [`health`]: what a host's banlieue `Provider` says about the host (ADR-0007).
//! - [`phase`]: the Host phase machine, from its spec, its bootstrap Job and that health.
//! - [`job`]: the bootstrap Job a Host needs, in the shape the admission policy admits
//!   (ADR-0008).
//!
//! Nothing here talks to a cluster. The reconcilers that do will call these functions, so
//! every decision a reconciler makes is covered by a unit test that needs no API server.

pub mod health;
pub mod job;
pub mod phase;
