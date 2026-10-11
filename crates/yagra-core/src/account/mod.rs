// SPDX-License-Identifier: AGPL-3.0-only
//! One account's own WebUI state: its dashboards and preferences (ADR-058).

pub(crate) mod dashboard;
/// Per-account WebUI preferences — one opaque JSON document per account (ADR-058).
pub(crate) mod preferences;
