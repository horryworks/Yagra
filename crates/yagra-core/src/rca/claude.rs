// SPDX-License-Identifier: AGPL-3.0-only
//! Anthropic Messages API adapter (ADR-029).
//!
//! **Egress boundary**: this provider sends the incident context to Anthropic — outside the
//! operator's own cloud. The UI marks it as such; Vertex is the in-region default for anyone who
//! needs the data to stay inside their GCP project.
//!
//! Two request-shape rules the current models enforce, both worth stating because getting them
//! wrong is a 400 rather than a degraded answer:
//!
//! * **No sampling parameters.** `temperature`, `top_p` and `top_k` were removed on the current
//!   Opus/Sonnet generation and are rejected. Behaviour is steered by the prompt alone.
//! * **`max_tokens` covers thinking as well as the answer.** Thinking is on by default on Opus 5,
//!   drawn from the same budget, so a ceiling sized around the visible answer truncates mid-reply.
//!   [`crate::rca::DEFAULT_MAX_OUTPUT_TOKENS`] leaves room for both.
//!
//! And one response rule: a request can come back **200 with `stop_reason: "refusal"`** and an
//! empty `content` array. Reading `content[0]` without checking `stop_reason` first panics on
//! exactly the case an operator most wants explained, so the refusal check comes first.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::provider::{
    clip, http_client, status_error, transport_error, LlmError, LlmProvider, LlmRequest,
    LlmResponse,
};

/// Anthropic's API host. A constant, not a setting — see the module docs on [`super::provider`].
const API_URL: &str = "https://api.anthropic.com/v1/messages";
/// The wire-format version this adapter is written against.
const API_VERSION: &str = "2023-06-01";
/// Names this provider in errors and in `yagra_llm_calls_total{provider}`.
const SERVICE: &str = "Claude";

/// Suggested model for the Settings placeholder. The operator can type any current id — this is a
/// starting point, not an allowlist, because model ids turn over faster than releases do.
pub const SUGGESTED_MODEL: &str = "claude-opus-5";

/// Anthropic Messages API client. Holds the API key for the process lifetime; it is never logged
/// and never returned by the API (ADR-018).
pub struct ClaudeProvider {
    http: reqwest::Client,
    api_key: String,
    model: String,
    api_url: String,
}

impl ClaudeProvider {
    /// Build a client for `model` authenticating with `api_key`.
    ///
    /// # Errors
    /// Returns operator-facing text when the key is empty or the HTTP client cannot be built.
    pub fn new(api_key: &str, model: &str) -> Result<Self, String> {
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Err("an Anthropic API key is required".to_owned());
        }
        if model.trim().is_empty() {
            return Err("a model id is required".to_owned());
        }
        Ok(Self {
            http: http_client()?,
            api_key: api_key.to_owned(),
            model: model.trim().to_owned(),
            api_url: API_URL.to_owned(),
        })
    }

    /// Point the client at a local stand-in. Test-only: the production endpoint is a constant so
    /// that no config field can redirect a prompt carrying the operator's inventory.
    #[cfg(test)]
    fn with_url(mut self, url: String) -> Self {
        self.api_url = url;
        self
    }
}

#[async_trait]
impl LlmProvider for ClaudeProvider {
    fn name(&self) -> &'static str {
        "claude"
    }

    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let body = build_body(&self.model, req);
        let res = self
            .http
            .post(&self.api_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|e| transport_error(SERVICE, &e))?;

        let status = res.status();
        if !status.is_success() {
            let detail = res.text().await.ok().and_then(|b| error_detail(&b));
            return Err(status_error(SERVICE, status, detail));
        }
        let payload: MessageResponse = res.json().await.map_err(|e| {
            LlmError::Malformed(format!("{SERVICE} sent an unreadable response: {e}"))
        })?;
        read_response(payload)
    }
}

/// Assemble the request body. Split out from the HTTP call so the shape is unit-testable without a
/// server: the sampling-parameter and `max_tokens` rules in the module docs are what this encodes.
fn build_body(model: &str, req: &LlmRequest) -> serde_json::Value {
    json!({
        "model": model,
        "max_tokens": req.max_output_tokens,
        "system": req.system,
        "messages": [{ "role": "user", "content": req.user }],
    })
}

#[derive(Deserialize)]
struct MessageResponse {
    #[serde(default)]
    content: Vec<ContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    stop_details: Option<StopDetails>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct StopDetails {
    #[serde(default)]
    category: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: Option<u32>,
    #[serde(default)]
    output_tokens: Option<u32>,
}

/// Turn a successful HTTP response into text, or into the typed reason there is none.
///
/// Order matters: the refusal check runs **before** any indexing of `content`, because a refusal
/// carries an empty array.
fn read_response(payload: MessageResponse) -> Result<LlmResponse, LlmError> {
    if payload.stop_reason.as_deref() == Some("refusal") {
        // `stop_details` is informational and may be absent even on a refusal — never branch on it,
        // only decorate with it.
        return Err(LlmError::Refused(
            payload.stop_details.and_then(|d| d.category),
        ));
    }
    // A model that thinks emits non-text blocks first; concatenating only the text ones is what
    // makes this robust to that without the adapter caring whether thinking was on.
    let text = payload
        .content
        .iter()
        .filter(|b| b.kind == "text")
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("");
    if text.trim().is_empty() {
        return Err(LlmError::Malformed(format!(
            "{SERVICE} returned no text{}",
            match payload.stop_reason.as_deref() {
                // The common cause is a budget that thinking consumed — say so rather than making
                // the operator guess (see the module docs).
                Some("max_tokens") => " (the output budget ran out — raise it and retry)",
                _ => "",
            }
        )));
    }
    let (in_tokens, out_tokens) = payload
        .usage
        .map_or((None, None), |u| (u.input_tokens, u.output_tokens));
    Ok(LlmResponse {
        text,
        in_tokens,
        out_tokens,
    })
}

/// Pull the machine-useful part out of an Anthropic error body. The `message` is echoed because
/// Anthropic's error text describes the *request shape* (bad model id, malformed field) rather than
/// quoting the prompt — unlike BigQuery's per-row errors, which is why that adapter drops it.
fn error_detail(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let err = v.get("error")?;
    let kind = err.get("type").and_then(serde_json::Value::as_str);
    let message = err.get("message").and_then(serde_json::Value::as_str);
    match (kind, message) {
        (Some(k), Some(m)) => clip(&format!("{k}: {m}")),
        (Some(k), None) => clip(k),
        (None, Some(m)) => clip(m),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn req() -> LlmRequest {
        LlmRequest {
            system: "you are an NMS assistant".to_owned(),
            user: "explain this incident".to_owned(),
            max_output_tokens: 4096,
        }
    }

    fn parse(body: Value) -> Result<LlmResponse, LlmError> {
        read_response(serde_json::from_value(body).unwrap())
    }

    #[test]
    fn the_body_carries_no_sampling_parameters() {
        // temperature / top_p / top_k were removed on the current models and are a 400, so the
        // adapter must never grow them "for determinism".
        let body = build_body("claude-opus-5", &req());
        for banned in ["temperature", "top_p", "top_k"] {
            assert!(body.get(banned).is_none(), "{banned} must not be sent");
        }
        assert_eq!(body["model"], json!("claude-opus-5"));
        assert_eq!(body["max_tokens"], json!(4096));
        assert_eq!(body["system"], json!("you are an NMS assistant"));
        assert_eq!(body["messages"][0]["role"], json!("user"));
        assert_eq!(
            body["messages"][0]["content"],
            json!("explain this incident")
        );
        // Single-shot: exactly one turn goes over the wire.
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_refusal_is_reported_as_a_refusal_not_a_crash() {
        // 200 + empty content. Indexing content[0] here would panic on the very case an operator
        // most wants explained.
        let out = parse(json!({
            "content": [],
            "stop_reason": "refusal",
            "stop_details": { "type": "refusal", "category": "cyber" },
        }));
        match out {
            Err(LlmError::Refused(Some(c))) => assert_eq!(c, "cyber"),
            other => panic!("expected a categorised refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_refusal_without_details_is_still_a_refusal() {
        // stop_details is informational and can be absent — the branch is on stop_reason alone.
        assert!(matches!(
            parse(json!({ "content": [], "stop_reason": "refusal" })),
            Err(LlmError::Refused(None))
        ));
    }

    #[test]
    fn text_blocks_are_joined_and_non_text_blocks_ignored() {
        let out = parse(json!({
            "content": [
                { "type": "thinking", "thinking": "" },
                { "type": "text", "text": "root cause: " },
                { "type": "text", "text": "core-sw-01 lost power" },
            ],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 1200, "output_tokens": 300 },
        }))
        .unwrap();
        assert_eq!(out.text, "root cause: core-sw-01 lost power");
        assert_eq!(out.in_tokens, Some(1200));
        assert_eq!(out.out_tokens, Some(300));
    }

    #[test]
    fn an_exhausted_budget_says_so_instead_of_returning_nothing() {
        // Thinking shares max_tokens, so this is the failure an under-sized ceiling produces.
        let err = parse(json!({ "content": [], "stop_reason": "max_tokens" })).unwrap_err();
        assert!(matches!(err, LlmError::Malformed(_)));
        assert!(err.to_string().contains("output budget"), "{err}");
    }

    #[test]
    fn usage_is_optional() {
        let out = parse(json!({
            "content": [{ "type": "text", "text": "ok" }],
            "stop_reason": "end_turn",
        }))
        .unwrap();
        assert_eq!((out.in_tokens, out.out_tokens), (None, None));
    }

    #[test]
    fn an_error_body_surfaces_the_request_problem() {
        let detail = error_detail(
            &json!({ "type": "error", "error": { "type": "not_found_error", "message": "model: nope" } })
                .to_string(),
        )
        .unwrap();
        assert_eq!(detail, "not_found_error: model: nope");
        assert!(error_detail("not json").is_none());
        assert!(error_detail(&json!({ "nope": 1 }).to_string()).is_none());
    }

    #[test]
    fn a_client_needs_a_key_and_a_model() {
        assert!(ClaudeProvider::new("", "claude-opus-5").is_err());
        assert!(ClaudeProvider::new("   ", "claude-opus-5").is_err());
        assert!(ClaudeProvider::new("sk-ant-x", "  ").is_err());
        let p = ClaudeProvider::new("  sk-ant-x  ", " claude-opus-5 ").unwrap();
        assert_eq!(p.api_key, "sk-ant-x");
        assert_eq!(p.model, "claude-opus-5");
        assert_eq!(p.name(), "claude");
    }

    #[tokio::test]
    async fn a_completion_sends_the_documented_headers() {
        let (addr, seen) = super::super::testsupport::serve(vec![(
            200,
            json!({
                "content": [{ "type": "text", "text": "hello" }],
                "stop_reason": "end_turn",
            })
            .to_string(),
        )])
        .await;
        let out = ClaudeProvider::new("sk-ant-test", "claude-opus-5")
            .unwrap()
            .with_url(format!("http://{addr}/v1/messages"))
            .complete(&req())
            .await
            .unwrap();
        assert_eq!(out.text, "hello");
        let reqs = seen.lock().unwrap();
        let head = &reqs[0].head;
        assert!(head.contains("x-api-key: sk-ant-test"), "{head}");
        assert!(
            head.contains(&format!("anthropic-version: {API_VERSION}")),
            "{head}"
        );
    }

    #[tokio::test]
    async fn a_401_is_an_auth_error_carrying_the_providers_reason() {
        let (addr, _) = super::super::testsupport::serve(vec![(
            401,
            json!({ "error": { "type": "authentication_error", "message": "invalid x-api-key" } })
                .to_string(),
        )])
        .await;
        let err = ClaudeProvider::new("sk-ant-bad", "claude-opus-5")
            .unwrap()
            .with_url(format!("http://{addr}/v1/messages"))
            .complete(&req())
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Auth(_)), "{err:?}");
        assert!(err.to_string().contains("authentication_error"), "{err}");
    }

    #[tokio::test]
    async fn a_429_is_reported_as_rate_limiting() {
        let (addr, _) = super::super::testsupport::serve(vec![(429, "{}".to_owned())]).await;
        let err = ClaudeProvider::new("sk-ant-x", "claude-opus-5")
            .unwrap()
            .with_url(format!("http://{addr}/v1/messages"))
            .complete(&req())
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::RateLimited), "{err:?}");
    }
}
