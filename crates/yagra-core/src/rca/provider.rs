// SPDX-License-Identifier: AGPL-3.0-only
//! The LLM seam for AI-assisted RCA (ADR-029): one trait, one active provider behind it.
//!
//! This is the same discipline as [`yagra_transport::Transport`] for device I/O and the notify
//! channels for alert delivery — the protocol differences live behind the trait, and nothing above
//! it knows which vendor answered.
//!
//! **There is deliberately no failover.** ADR-029 fixes a single active provider chosen by config:
//! if it is unreachable, the RCA for that incident is skipped and the alert fires as it always
//! would. Chaining providers would quietly move hostnames, addresses, topology and syslog across a
//! trust boundary the operator picked on purpose — a privacy decision, not a reliability one.
//!
//! **Every adapter's host is a constant.** No config field selects an endpoint (`bigquery.rs`
//! applies the same rule for the same reason): a settable base URL turns a settings page into an
//! exfiltration channel for a prompt that carries the operator's network inventory.

use std::time::Duration;

use async_trait::async_trait;

/// Bound on one call so a hung provider cannot park the request. Generous compared with the rest of
/// the tree (`bigquery.rs` uses 20s) because a bounded RCA completion legitimately takes tens of
/// seconds — the orchestrator's own concurrency cap is what protects core, not this timeout.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Longest error text kept from a provider response body. The bodies can quote the prompt back,
/// and the prompt carries device-supplied text, so what survives is truncated and never logged.
const MAX_ERROR_CHARS: usize = 200;

/// One bounded completion request. There is no conversation: ADR-029 Increment 1 is a single shot,
/// so the whole incident context arrives in `user` and nothing is carried between calls.
#[derive(Debug, Clone)]
pub struct LlmRequest {
    /// Role, output contract and standing prohibitions. Stable across calls, so a provider that
    /// bills prompt caching can cache it.
    pub system: String,
    /// The rendered incident context. Bounded by `prompt.rs` before it gets here.
    pub user: String,
    /// Ceiling on the answer. On models that think, thinking is drawn from the same budget — see
    /// [`crate::rca::claude`].
    pub max_output_tokens: u32,
}

/// What came back. Token counts are `None` when the provider does not report them; they feed
/// metrics only, never control flow.
#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub text: String,
    pub in_tokens: Option<u32>,
    pub out_tokens: Option<u32>,
}

/// Why a completion did not happen. The variants exist to be *counted* — `reason()` is the metric
/// label — and to tell a retryable condition from a permanent one.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// Credentials rejected or absent. Operator action required; retrying will not help.
    #[error("{0}")]
    Auth(String),
    /// The provider asked us to slow down.
    #[error("the provider is rate limiting us")]
    RateLimited,
    /// No answer inside [`REQUEST_TIMEOUT`].
    #[error("the provider did not answer within {}s", REQUEST_TIMEOUT.as_secs())]
    Timeout,
    /// Reached, but unhappy — 5xx, or a 4xx that is not auth or rate limiting.
    #[error("{0}")]
    Upstream(String),
    /// Answered with something this adapter could not read.
    #[error("{0}")]
    Malformed(String),
    /// Declined to answer. Not a failure of ours: the model's safety layer stopped it, and the
    /// operator-facing message should say so rather than implying an outage.
    #[error("the model declined to answer{}", opt_suffix(.0))]
    Refused(Option<String>),
}

fn opt_suffix(detail: &Option<String>) -> String {
    match detail {
        Some(d) => format!(" ({d})"),
        None => String::new(),
    }
}

impl LlmError {
    /// Stable label for `yagra_llm_errors_total{reason}`. Keep the set small and closed — a label
    /// derived from a provider's error text would be unbounded cardinality (monitoring-conventions).
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Auth(_) => "auth",
            Self::RateLimited => "rate_limited",
            Self::Timeout => "timeout",
            Self::Upstream(_) => "upstream",
            Self::Malformed(_) => "malformed",
            Self::Refused(_) => "refused",
        }
    }
}

/// A single LLM vendor. Implementors hold their own credentials and endpoint; the orchestrator
/// holds one of these and knows nothing else about the provider.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Stable label for `yagra_llm_calls_total{provider}` — `"vertex"`, `"gemini"`, `"claude"`.
    fn name(&self) -> &'static str;

    /// Run one bounded completion.
    ///
    /// # Errors
    /// Returns [`LlmError`] on auth, transport, rate-limit, refusal or unreadable-response failure.
    /// Implementors must map a transport timeout to [`LlmError::Timeout`] rather than `Upstream`,
    /// so the two are distinguishable in metrics.
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError>;
}

// ── Shared helpers for the adapters ──────────────────────────────────────────────────────────

/// Build the HTTP client every adapter uses. One place so the timeout cannot drift between
/// providers, and so a future proxy/TLS setting lands on all three at once.
///
/// # Errors
/// Returns operator-facing text when the client cannot be constructed.
pub(crate) fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("HTTP client: {e}"))
}

/// Map a `reqwest` failure onto the typed error. A timeout is called out because "the provider was
/// slow" and "the provider was broken" lead to different operator actions.
pub(crate) fn transport_error(service: &str, e: &reqwest::Error) -> LlmError {
    if e.is_timeout() {
        LlmError::Timeout
    } else {
        LlmError::Upstream(format!("{service} is unreachable: {e}"))
    }
}

/// Classify a non-success HTTP status. `detail` is already-truncated body text, or `None` when the
/// body was unreadable — never the raw body, which can echo the prompt.
pub(crate) fn status_error(
    service: &str,
    status: reqwest::StatusCode,
    detail: Option<String>,
) -> LlmError {
    let with = |what: &str| match &detail {
        Some(d) => format!("{service} {what}: {d}"),
        None => format!("{service} {what} ({status})"),
    };
    match status {
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
            LlmError::Auth(with("rejected the credentials"))
        }
        reqwest::StatusCode::TOO_MANY_REQUESTS => LlmError::RateLimited,
        _ => LlmError::Upstream(with("returned an error")),
    }
}

/// Keep at most [`MAX_ERROR_CHARS`] of a provider's error text, collapsing whitespace so a
/// multi-line body stays one readable line in the UI.
pub(crate) fn clip(text: &str) -> Option<String> {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    Some(flat.chars().take(MAX_ERROR_CHARS).collect())
}

/// Reject an identifier that is about to be interpolated into a URL path.
///
/// The adapters pin their hosts, but a project/model/location still lands in the path — and a value
/// carrying `/`, `..`, `@` or `:` could redirect the request within (or away from) that host. Only
/// the characters GCP actually allows in these ids get through.
///
/// # Errors
/// Returns operator-facing text naming the offending field.
pub(crate) fn validate_path_segment(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{label} is required"));
    }
    if value.len() > 128 {
        return Err(format!("{label} is too long"));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(format!(
            "{label} may only contain letters, digits, dots, dashes and underscores"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in provider for the orchestrator's tests: no HTTP, scripted outcome.
    pub(crate) struct FakeProvider {
        pub reply: std::sync::Mutex<Option<Result<LlmResponse, LlmError>>>,
    }

    #[async_trait]
    impl LlmProvider for FakeProvider {
        fn name(&self) -> &'static str {
            "fake"
        }
        async fn complete(&self, _req: &LlmRequest) -> Result<LlmResponse, LlmError> {
            self.reply
                .lock()
                .unwrap()
                .take()
                .unwrap_or(Err(LlmError::Upstream("no scripted reply".to_owned())))
        }
    }

    #[tokio::test]
    async fn a_provider_is_usable_behind_a_trait_object() {
        // The orchestrator holds `Arc<dyn LlmProvider>`; if the trait stops being object-safe this
        // fails to compile, which is the point of the test.
        let p: std::sync::Arc<dyn LlmProvider> = std::sync::Arc::new(FakeProvider {
            reply: std::sync::Mutex::new(Some(Ok(LlmResponse {
                text: "ok".to_owned(),
                in_tokens: Some(1),
                out_tokens: Some(2),
            }))),
        });
        let req = LlmRequest {
            system: "s".to_owned(),
            user: "u".to_owned(),
            max_output_tokens: 16,
        };
        assert_eq!(p.complete(&req).await.unwrap().text, "ok");
    }

    #[test]
    fn every_error_has_a_stable_metric_reason() {
        // Bounded cardinality: the label set is closed and derived from the variant, never from a
        // provider's error text.
        let all = [
            LlmError::Auth(String::new()),
            LlmError::RateLimited,
            LlmError::Timeout,
            LlmError::Upstream(String::new()),
            LlmError::Malformed(String::new()),
            LlmError::Refused(None),
        ];
        let reasons: Vec<&str> = all.iter().map(LlmError::reason).collect();
        assert_eq!(
            reasons,
            [
                "auth",
                "rate_limited",
                "timeout",
                "upstream",
                "malformed",
                "refused"
            ]
        );
    }

    #[test]
    fn a_status_maps_to_the_action_the_operator_must_take() {
        assert!(matches!(
            status_error("X", reqwest::StatusCode::UNAUTHORIZED, None),
            LlmError::Auth(_)
        ));
        assert!(matches!(
            status_error("X", reqwest::StatusCode::FORBIDDEN, None),
            LlmError::Auth(_)
        ));
        assert!(matches!(
            status_error("X", reqwest::StatusCode::TOO_MANY_REQUESTS, None),
            LlmError::RateLimited
        ));
        assert!(matches!(
            status_error("X", reqwest::StatusCode::INTERNAL_SERVER_ERROR, None),
            LlmError::Upstream(_)
        ));
        assert!(matches!(
            status_error("X", reqwest::StatusCode::BAD_REQUEST, None),
            LlmError::Upstream(_)
        ));
    }

    #[test]
    fn error_text_is_flattened_and_capped() {
        let long = "a ".repeat(400);
        let clipped = clip(&long).unwrap();
        assert_eq!(clipped.chars().count(), MAX_ERROR_CHARS);
        assert!(!clipped.contains('\n'));
        assert_eq!(
            clip("  line one\n\n  line two  ").unwrap(),
            "line one line two"
        );
        assert!(clip("   \n ").is_none());
    }

    #[test]
    fn a_path_segment_cannot_reshape_the_url() {
        for bad in [
            "",
            "../../etc",
            "proj/other",
            "proj@evil.example.com",
            "us-central1:8080",
            "a b",
            "%2e%2e",
        ] {
            assert!(
                validate_path_segment("project", bad).is_err(),
                "{bad} must be refused"
            );
        }
        for good in [
            "my-project",
            "us-central1",
            "gemini-2.5-pro",
            "claude_opus_5",
        ] {
            validate_path_segment("value", good).unwrap();
        }
    }
}
