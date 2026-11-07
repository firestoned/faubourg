<!-- Copyright (c) 2026 Erick Bourgeois, faubourg -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# ADR-0001: Record architecture decisions

**Status:** Accepted · **Date:** 2026-11-07

## Context

faubourg holds a root-equivalent credential for every hypervisor it manages and decides
which hosts banlieue may place VMs on. Decisions about where that trust lives are the
product; they must be written down before code.

## Decision

Use ADRs in this directory, numbered, one decision each, following the Architecture Driven
Development cycle shared with banlieue and mediatore: ADR, then CALM
(`docs/architecture/calm/architecture.json`), then TDD, then implementation, then docs,
then a full threat-model pass. A decision is not made until its ADR is Accepted, and not
implemented until the threat-model pass that follows it is stamped.

ADRs marked **Proposed** are recommendations awaiting the maintainer. They may be
implemented behind tests only once Accepted.

## Consequences

Every PR that changes a trust boundary, credential handling, an API surface or the contract
with banlieue links the ADR it implements or amends.
