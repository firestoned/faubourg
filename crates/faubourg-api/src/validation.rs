// Copyright (c) 2026 Erick Bourgeois, faubourg
// SPDX-License-Identifier: Apache-2.0

//! Shared validation primitives. Every check fails closed: anything not positively
//! recognised as valid is refused.

use std::fmt;

/// One refused field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldError {
    /// JSON path of the field, e.g. `spec.storage.mode`.
    pub field: String,
    /// Machine-readable reason, used as a condition reason.
    pub reason: &'static str,
    /// Human-readable detail. Never contains secret material.
    pub message: String,
}

/// A spec that failed validation, with every refused field.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub struct Invalid(pub Vec<FieldError>);

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self
            .0
            .iter()
            .map(|e| format!("{}: {} ({})", e.field, e.message, e.reason))
            .collect();
        write!(f, "{}", parts.join("; "))
    }
}

impl Invalid {
    /// The reasons, in order, for tests and conditions.
    pub fn reasons(&self) -> Vec<&'static str> {
        self.0.iter().map(|e| e.reason).collect()
    }
}

/// Collects field errors, then turns them into a `Result`.
#[derive(Default)]
pub(crate) struct Errors(Vec<FieldError>);

impl Errors {
    pub(crate) fn push(&mut self, field: &str, reason: &'static str, message: impl Into<String>) {
        self.0.push(FieldError {
            field: field.to_string(),
            reason,
            message: message.into(),
        });
    }

    pub(crate) fn finish(self) -> Result<(), Invalid> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(Invalid(self.0))
        }
    }
}

/// An RFC 1123 DNS subdomain, as Kubernetes object names require.
pub fn is_dns_subdomain(s: &str) -> bool {
    !s.is_empty() && s.len() <= 253 && s.split('.').all(is_dns_label)
}

/// An RFC 1123 DNS label: lowercase alphanumerics and `-`, alphanumeric at both ends.
pub fn is_dns_label(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 63
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && b[0] != b'-'
        && b[b.len() - 1] != b'-'
}
