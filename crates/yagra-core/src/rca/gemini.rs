// SPDX-License-Identifier: AGPL-3.0-only
//! Gemini adapter — the direct `generativelanguage.googleapis.com` API (ADR-029).
//!
//! **Egress boundary**: this provider sends the incident context to Google's public Gemini
//! endpoint, outside the operator's own project. [`super::vertex`] speaks the same wire format
//! inside the operator's GCP project and is the recommended default; the two differ only in host
//! and authentication, so the request/response codec below is **shared** rather than written twice.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::provider::{
    clip, http_client, status_error, transport_error, validate_path_segment, LlmError, LlmProvider,
    LlmRequest, LlmResponse,
};

/// Google's public Generative Language host. A constant, not a setting.
const API_HOST: &str = "https://generativelanguage.googleapis.com";
/// Names this provider in errors.
const SERVICE: &str = "Gemini";

/// Suggested model for the Settings placeholder — a starting point, not an allowlist.
pub const SUGGESTED_MODEL: &str = "gemini-2.5-pro";

/// Gemini API client. The API key lives in memory only and is never logged (ADR-018).
pub struct GeminiProvider {
    http: reqwest::Client,
    api_key: String,
    model: String,
    base: String,
}

impl GeminiProvider {
    /// Build a client for `model` authenticating with `api_key`.
    ///
    /// # Errors
    /// Returns operator-facing text when the key or model is missing, the model id contains
    /// characters that could reshape the URL, or the HTTP client cannot be built.
    pub fn new(api_key: &str, model: &str) -> Result<Self, String> {
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Err("a Gemini API key is required".to_owned());
        }
        let model = model.trim();
        validate_path_segment("model", model)?;
        Ok(Self {
            http: http_client()?,
            api_key: api_key.to_owned(),
            model: model.to_owned(),
            base: API_HOST.to_owned(),
        })
    }

    #[cfg(test)]
    fn with_base(mut self, base: String) -> Self {
        self.base = base;
        self
    }

    fn url(&self) -> String {
        format!("{}/v1beta/models/{}:generateContent", self.base, self.model)
    }
}

#[async_trait]
impl LlmProvider for GeminiProvider {
    fn name(&self) -> &'static str {
        "gemini"
    }

    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let res = self
            .http
            .post(self.url())
            .header("x-goog-api-key", &self.api_key)
            .json(&build_body(req))
            .send()
            .await
            .map_err(|e| transport_error(SERVICE, &e))?;
        let status = res.status();
        if !status.is_success() {
            let detail = res.text().await.ok().and_then(|b| error_detail(&b));
            return Err(status_error(SERVICE, status, detail));
        }
        let payload: GenerateResponse = res.json().await.map_err(|e| {
            LlmError::Malformed(format!("{SERVICE} sent an unreadable response: {e}"))
        })?;
        read_response(SERVICE, payload)
    }
}

/// Assemble the `generateContent` body. Shared with [`super::vertex`], which posts the identical
/// shape to a different host — one definition so the two cannot drift.
pub(crate) fn build_body(req: &LlmRequest) -> serde_json::Value {
    json!({
        "systemInstruction": { "parts": [{ "text": req.system }] },
        "contents": [{ "role": "user", "parts": [{ "text": req.user }] }],
        "generationConfig": { "maxOutputTokens": req.max_output_tokens },
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GenerateResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(default)]
    prompt_feedback: Option<PromptFeedback>,
    #[serde(default)]
    usage_metadata: Option<UsageMetadata>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    #[serde(default)]
    content: Option<Content>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Content {
    #[serde(default)]
    parts: Vec<Part>,
}

#[derive(Deserialize)]
struct Part {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromptFeedback {
    #[serde(default)]
    block_reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageMetadata {
    #[serde(default)]
    prompt_token_count: Option<u32>,
    #[serde(default)]
    candidates_token_count: Option<u32>,
}

/// Turn a successful response into text, or the typed reason there is none.
///
/// The safety check runs **before** reading any candidate: a blocked prompt comes back 200 with an
/// empty `candidates` array and the reason in `promptFeedback`, so indexing first would report
/// "malformed" for what is really a refusal.
pub(crate) fn read_response(
    service: &str,
    payload: GenerateResponse,
) -> Result<LlmResponse, LlmError> {
    if let Some(reason) = payload.prompt_feedback.and_then(|f| f.block_reason) {
        return Err(LlmError::Refused(Some(reason)));
    }
    let Some(candidate) = payload.candidates.into_iter().next() else {
        return Err(LlmError::Malformed(format!(
            "{service} returned no candidates"
        )));
    };
    let finish = candidate.finish_reason.unwrap_or_default();
    if matches!(
        finish.as_str(),
        "SAFETY" | "PROHIBITED_CONTENT" | "BLOCKLIST"
    ) {
        return Err(LlmError::Refused(Some(finish)));
    }
    let text = candidate
        .content
        .map(|c| {
            c.parts
                .into_iter()
                .filter_map(|p| p.text)
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    if text.trim().is_empty() {
        return Err(LlmError::Malformed(format!(
            "{service} returned no text{}",
            if finish == "MAX_TOKENS" {
                " (the output budget ran out — raise it and retry)"
            } else {
                ""
            }
        )));
    }
    let (in_tokens, out_tokens) = payload.usage_metadata.map_or((None, None), |u| {
        (u.prompt_token_count, u.candidates_token_count)
    });
    Ok(LlmResponse {
        text,
        in_tokens,
        out_tokens,
    })
}

/// Pull the status/message pair out of a Google API error body. Google's message describes the
/// request (bad model, bad argument) rather than quoting the prompt.
pub(crate) fn error_detail(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let err = v.get("error")?;
    let status = err.get("status").and_then(serde_json::Value::as_str);
    let message = err.get("message").and_then(serde_json::Value::as_str);
    match (status, message) {
        (Some(s), Some(m)) => clip(&format!("{s}: {m}")),
        (Some(s), None) => clip(s),
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
            system: "sys".to_owned(),
            user: "usr".to_owned(),
            max_output_tokens: 2048,
        }
    }

    pub(crate) fn parse(body: Value) -> Result<LlmResponse, LlmError> {
        read_response(SERVICE, serde_json::from_value(body).unwrap())
    }

    #[test]
    fn the_body_matches_the_generate_content_shape() {
        let body = build_body(&req());
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], json!("sys"));
        assert_eq!(body["contents"][0]["role"], json!("user"));
        assert_eq!(body["contents"][0]["parts"][0]["text"], json!("usr"));
        assert_eq!(body["generationConfig"]["maxOutputTokens"], json!(2048));
    }

    #[test]
    fn a_blocked_prompt_is_a_refusal_not_a_parse_failure() {
        // 200 + empty candidates + promptFeedback. Reading candidates[0] first would misreport it.
        assert!(matches!(
            parse(json!({ "candidates": [], "promptFeedback": { "blockReason": "SAFETY" } })),
            Err(LlmError::Refused(Some(r))) if r == "SAFETY"
        ));
    }

    #[test]
    fn a_safety_finish_reason_is_a_refusal() {
        for reason in ["SAFETY", "PROHIBITED_CONTENT", "BLOCKLIST"] {
            let out = parse(json!({ "candidates": [{ "finishReason": reason }] }));
            assert!(
                matches!(out, Err(LlmError::Refused(Some(ref r))) if r == reason),
                "{reason}: {out:?}"
            );
        }
    }

    #[test]
    fn parts_are_joined_and_usage_read() {
        let out = parse(json!({
            "candidates": [{
                "content": { "role": "model", "parts": [{ "text": "a" }, { "text": "b" }] },
                "finishReason": "STOP",
            }],
            "usageMetadata": { "promptTokenCount": 10, "candidatesTokenCount": 20 },
        }))
        .unwrap();
        assert_eq!(out.text, "ab");
        assert_eq!((out.in_tokens, out.out_tokens), (Some(10), Some(20)));
    }

    #[test]
    fn an_exhausted_budget_says_so() {
        let err = parse(json!({ "candidates": [{ "finishReason": "MAX_TOKENS" }] })).unwrap_err();
        assert!(err.to_string().contains("output budget"), "{err}");
    }

    #[test]
    fn no_candidates_is_malformed() {
        assert!(matches!(parse(json!({})), Err(LlmError::Malformed(_))));
    }

    #[test]
    fn a_model_id_cannot_reshape_the_url() {
        assert!(GeminiProvider::new("k", "../../v1beta/models/x").is_err());
        assert!(GeminiProvider::new("k", "m:generateContent?x=1").is_err());
        assert!(GeminiProvider::new("", "gemini-2.5-pro").is_err());
        let p = GeminiProvider::new("k", " gemini-2.5-pro ").unwrap();
        assert!(p
            .url()
            .ends_with("/v1beta/models/gemini-2.5-pro:generateContent"));
        assert!(p.url().starts_with(API_HOST));
        assert_eq!(p.name(), "gemini");
    }

    #[tokio::test]
    async fn a_completion_sends_the_api_key_header() {
        let (addr, seen) = super::super::testsupport::serve(vec![(
            200,
            json!({
                "candidates": [{ "content": { "parts": [{ "text": "hi" }] }, "finishReason": "STOP" }],
            })
            .to_string(),
        )])
        .await;
        let out = GeminiProvider::new("AIza-test", "gemini-2.5-pro")
            .unwrap()
            .with_base(format!("http://{addr}"))
            .complete(&req())
            .await
            .unwrap();
        assert_eq!(out.text, "hi");
        let reqs = seen.lock().unwrap();
        assert!(
            reqs[0].head.contains("x-goog-api-key: AIza-test"),
            "{}",
            reqs[0].head
        );
        assert!(
            reqs[0]
                .head
                .contains("/v1beta/models/gemini-2.5-pro:generateContent"),
            "{}",
            reqs[0].head
        );
    }

    #[tokio::test]
    async fn a_403_is_an_auth_error_with_googles_status() {
        let (addr, _) = super::super::testsupport::serve(vec![(
            403,
            json!({ "error": { "status": "PERMISSION_DENIED", "message": "API key not valid" } })
                .to_string(),
        )])
        .await;
        let err = GeminiProvider::new("AIza-bad", "gemini-2.5-pro")
            .unwrap()
            .with_base(format!("http://{addr}"))
            .complete(&req())
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Auth(_)), "{err:?}");
        assert!(err.to_string().contains("PERMISSION_DENIED"), "{err}");
    }
}
