// SPDX-License-Identifier: AGPL-3.0-only
//! AI-assisted root-cause analysis (ADR-029).
//!
//! Yagra already assembles the evidence for an incident: dependency suppression attributes a
//! cascade to its root cause ([`crate::alerts`]), and the incident timeline gathers the metric
//! anomaly, the passive events and the dominant flow around it
//! ([`crate::analysis::AnalysisRunner::incident_signals`]). What no amount of correlation produces
//! is the sentence a human writes at the end: *why* it broke and *what to look at next*. This
//! module asks a language model for that sentence.
//!
//! **It is additive and best-effort, by construction.** Nothing in the alert path calls into here:
//! hysteresis, dependency suppression, dedup and notification are unchanged whether the provider
//! answers, refuses, times out or was never configured. An RCA is an explanation attached to an
//! incident, never a precondition for raising one.
//!
//! **Default off.** With no provider row there is no client, no credentials and no egress.
//!
//! Layout, in the order a request moves through it:
//! * [`context`] — what the model is told about an incident. Reads the stores; holds no secrets.
//! * [`prompt`] — that context rendered to text. Pure, bounded, and where device output is fenced.
//! * [`provider`] — the [`provider::LlmProvider`] trait, its typed errors, and shared HTTP helpers.
//!   * [`vertex`] — Gemini inside the operator's GCP project. The recommended, in-region choice.
//!   * [`gemini`] — the public Gemini API. Also owns the `generateContent` codec Vertex reuses.
//!   * [`claude`] — the Anthropic Messages API.
//! * [`answer`] — reading the reply back, degrading to raw text rather than failing.
//! * [`store`] — the provider config (sealed credential) and the generated reports.
//! * [`orchestrator`] — the click-to-report path, and the caps that bound what a click can cost.

pub(crate) mod agent;
pub(crate) mod answer;
pub(crate) mod claude;
pub(crate) mod context;
pub(crate) mod gemini;
pub(crate) mod orchestrator;
pub(crate) mod prompt;
pub(crate) mod provider;
pub(crate) mod store;
#[cfg(test)]
pub(crate) mod testsupport;
pub(crate) mod vertex;

use std::sync::Arc;

use provider::LlmProvider;

/// Output ceiling for one RCA when the operator has not set one.
///
/// Sized well above the ~600 tokens an RCA actually needs because on current models **thinking is
/// drawn from the same budget** — a ceiling fitted to the visible answer truncates the reply on
/// exactly the hard incidents where the reasoning is worth having.
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 8192;

/// Which vendor is active. ADR-029 fixes **one** at a time: this is a selection, not a chain, and
/// there is deliberately no failover between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// Gemini inside the operator's own GCP project, in a region they choose. Recommended: the
    /// incident context does not leave their cloud boundary.
    Vertex,
    /// The public Gemini API. Leaves the operator's boundary.
    Gemini,
    /// The Anthropic Messages API. Leaves the operator's boundary.
    Claude,
}

impl ProviderKind {
    /// Stable wire/DB spelling. Used as a metric label, so the set must stay closed.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vertex => "vertex",
            Self::Gemini => "gemini",
            Self::Claude => "claude",
        }
    }

    /// Parse the stored spelling. `None` for anything else — an unknown provider is a config error
    /// at the API edge, never a silent fallback to a different vendor.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "vertex" => Some(Self::Vertex),
            "gemini" => Some(Self::Gemini),
            "claude" => Some(Self::Claude),
            _ => None,
        }
    }

    /// Whether choosing this provider sends incident context outside the operator's own cloud.
    /// Drives the warning the Settings page shows: the operator should be picking their egress
    /// boundary deliberately, not discovering it later.
    #[must_use]
    pub fn leaves_operator_boundary(self) -> bool {
        !matches!(self, Self::Vertex)
    }

    /// Placeholder model id for the Settings form. A hint, not an allowlist — model ids turn over
    /// far faster than Yagra releases, so nothing here validates against a hardcoded list.
    #[must_use]
    pub fn suggested_model(self) -> &'static str {
        match self {
            Self::Vertex => vertex::SUGGESTED_MODEL,
            Self::Gemini => gemini::SUGGESTED_MODEL,
            Self::Claude => claude::SUGGESTED_MODEL,
        }
    }

    /// Placeholder region. Only Vertex has one — the others have no region to choose, which is the
    /// same thing as saying the operator does not get to decide where their data is processed.
    #[must_use]
    pub fn suggested_location(self) -> Option<&'static str> {
        match self {
            Self::Vertex => Some(vertex::SUGGESTED_LOCATION),
            Self::Gemini | Self::Claude => None,
        }
    }
}

/// What the Settings form needs to render one provider choice.
///
/// Served from the backend rather than hardcoded in the WebUI so the egress warning cannot drift
/// from what the code actually does: [`ProviderKind::leaves_operator_boundary`] is the single
/// definition, and the checkbox that warns about it reads the same value the adapter obeys.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct ProviderChoice {
    pub key: &'static str,
    pub suggested_model: &'static str,
    pub suggested_location: Option<&'static str>,
    /// True when picking this sends hostnames, addresses, topology and syslog outside the
    /// operator's own cloud.
    pub leaves_operator_boundary: bool,
    /// Whether this provider needs a GCP project + region (Vertex) rather than just an API key.
    pub needs_project: bool,
    /// Whether the credential may be omitted (Vertex on GKE/GCE uses Workload Identity instead).
    pub credential_optional: bool,
}

/// Every provider the operator may choose, in recommended-first order.
#[must_use]
pub fn provider_choices() -> Vec<ProviderChoice> {
    [
        ProviderKind::Vertex,
        ProviderKind::Gemini,
        ProviderKind::Claude,
    ]
    .into_iter()
    .map(|kind| ProviderChoice {
        key: kind.as_str(),
        suggested_model: kind.suggested_model(),
        suggested_location: kind.suggested_location(),
        leaves_operator_boundary: kind.leaves_operator_boundary(),
        needs_project: matches!(kind, ProviderKind::Vertex),
        credential_optional: matches!(kind, ProviderKind::Vertex),
    })
    .collect()
}

/// Everything needed to build a client, as the operator entered it.
#[derive(Debug, Clone, Default)]
pub struct ProviderConfig {
    pub model: String,
    /// Vertex only: the GCP project that will be billed and whose boundary the data stays in.
    pub project: String,
    /// Vertex only: the region. `global` is accepted; naming a region is the point of Vertex.
    pub location: String,
    /// The decrypted secret: an API key for Gemini/Claude, or a service-account JSON for Vertex.
    /// Empty on Vertex means Workload Identity via the metadata server. Never logged, never
    /// returned by the API (ADR-018).
    pub secret: Option<String>,
}

/// Build the active provider.
///
/// # Errors
/// Returns operator-facing text when a required field is missing or malformed — surfaced as a 400
/// on the config endpoint so a mistyped setting fails at save time rather than on every incident.
pub fn build(kind: ProviderKind, cfg: &ProviderConfig) -> Result<Arc<dyn LlmProvider>, String> {
    let secret = cfg.secret.as_deref().unwrap_or_default();
    Ok(match kind {
        ProviderKind::Vertex => Arc::new(vertex::VertexProvider::new(
            &cfg.project,
            &cfg.location,
            &cfg.model,
            // Empty means "use the instance's own identity" — the better deployment on GKE/GCE.
            Some(secret).filter(|s| !s.trim().is_empty()),
        )?),
        ProviderKind::Gemini => Arc::new(gemini::GeminiProvider::new(secret, &cfg.model)?),
        ProviderKind::Claude => Arc::new(claude::ClaudeProvider::new(secret, &cfg.model)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_kinds_round_trip_their_stored_spelling() {
        for kind in [
            ProviderKind::Vertex,
            ProviderKind::Gemini,
            ProviderKind::Claude,
        ] {
            assert_eq!(ProviderKind::parse(kind.as_str()), Some(kind));
        }
        // An unknown value is a config error, never a silent switch to another vendor.
        assert_eq!(ProviderKind::parse("openai"), None);
        assert_eq!(ProviderKind::parse(""), None);
        assert_eq!(ProviderKind::parse("Vertex"), None);
    }

    #[test]
    fn only_vertex_keeps_the_data_inside_the_operators_boundary() {
        assert!(!ProviderKind::Vertex.leaves_operator_boundary());
        assert!(ProviderKind::Gemini.leaves_operator_boundary());
        assert!(ProviderKind::Claude.leaves_operator_boundary());
    }

    #[test]
    fn a_missing_credential_fails_at_build_time_for_the_key_providers() {
        let cfg = ProviderConfig {
            model: "m".to_owned(),
            ..Default::default()
        };
        assert!(build(ProviderKind::Gemini, &cfg).is_err());
        assert!(build(ProviderKind::Claude, &cfg).is_err());
    }

    #[test]
    fn vertex_builds_without_a_key_because_that_selects_workload_identity() {
        let cfg = ProviderConfig {
            model: "gemini-2.5-pro".to_owned(),
            project: "my-project".to_owned(),
            location: "us-central1".to_owned(),
            // Whitespace is treated as absent, not as an unusable key.
            secret: Some("   ".to_owned()),
        };
        let p = build(ProviderKind::Vertex, &cfg).unwrap();
        assert_eq!(p.name(), "vertex");
    }

    #[test]
    fn vertex_still_requires_its_identifiers() {
        let cfg = ProviderConfig {
            model: "gemini-2.5-pro".to_owned(),
            location: "us-central1".to_owned(),
            ..Default::default()
        };
        assert!(
            build(ProviderKind::Vertex, &cfg).is_err(),
            "project is required"
        );
    }

    #[test]
    fn each_kind_offers_a_placeholder_model() {
        for kind in [
            ProviderKind::Vertex,
            ProviderKind::Gemini,
            ProviderKind::Claude,
        ] {
            assert!(!kind.suggested_model().is_empty());
        }
    }
}
