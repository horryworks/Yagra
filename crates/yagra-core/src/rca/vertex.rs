// SPDX-License-Identifier: AGPL-3.0-only
//! Vertex AI adapter — Gemini inside the operator's own GCP project (ADR-029).
//!
//! **This is the privacy-preserving default.** The same model, reached through the operator's
//! project in a region they choose: hostnames, addresses, topology and syslog stay inside their
//! GCP boundary and can be fenced further with VPC-SC. [`super::gemini`] and [`super::claude`] are
//! the deliberate opt-outs from that, and the UI labels them as such.
//!
//! Two things are reused rather than rewritten:
//!
//! * the **request/response codec** from [`super::gemini`] — Vertex serves the identical
//!   `generateContent` shape, and a second copy would drift;
//! * the **OAuth token source** from [`crate::gcp`] — service-account key or Workload Identity,
//!   the same handshake the BigQuery forwarder uses, differing only in scope.

use tokio::sync::Mutex;

use async_trait::async_trait;

use super::gemini::{build_body, error_detail, read_response, GenerateResponse};
use super::provider::{
    http_client, status_error, transport_error, validate_path_segment, LlmError, LlmProvider,
    LlmRequest, LlmResponse,
};
use crate::gcp::TokenSource;

/// Scope for the Vertex prediction service. Broader than BigQuery's because Google does not publish
/// a narrower one for `aiplatform`.
const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
/// Names this provider in errors.
const SERVICE: &str = "Vertex AI";

/// Suggested model for the Settings placeholder — a starting point, not an allowlist.
pub const SUGGESTED_MODEL: &str = "gemini-2.5-pro";
/// Suggested location. `global` is accepted too and routes to the unprefixed host, but naming a
/// region is the point of choosing Vertex — it is what keeps the data somewhere known.
pub const SUGGESTED_LOCATION: &str = "us-central1";

/// Vertex AI client. Owns its token source, so a token minted for one destination is never reused
/// for another.
pub struct VertexProvider {
    http: reqwest::Client,
    /// `&mut` for the token cache; the orchestrator holds this behind an `Arc`, so the lock makes
    /// the shared handle usable without making the trait take `&mut self`.
    tokens: Mutex<TokenSource>,
    url: String,
}

impl VertexProvider {
    /// Build a client for `model` in `project`/`location`. `service_account_json` is the stored
    /// key; `None` selects Workload Identity via the GCE/GKE metadata server, which is the better
    /// deployment when core runs on Google infrastructure.
    ///
    /// # Errors
    /// Returns operator-facing text when an identifier is missing or malformed, the key is
    /// unusable, or the HTTP client cannot be built.
    pub fn new(
        project: &str,
        location: &str,
        model: &str,
        service_account_json: Option<&str>,
    ) -> Result<Self, String> {
        let project = project.trim();
        let location = location.trim();
        let model = model.trim();
        // These land in the URL path. The host is derived from `location`, so a value carrying `/`
        // or `@` could point the request somewhere else entirely — hence the strict charset.
        validate_path_segment("project", project)?;
        validate_path_segment("location", location)?;
        validate_path_segment("model", model)?;
        let tokens = TokenSource::new(SERVICE, SCOPE, service_account_json)?;
        Ok(Self {
            http: http_client()?,
            tokens: Mutex::new(tokens),
            url: endpoint(&host(location), project, location, model),
        })
    }

    #[cfg(test)]
    fn with_endpoints(mut self, base: &str, project: &str, location: &str, model: &str) -> Self {
        self.url = endpoint(base, project, location, model);
        self.tokens
            .get_mut()
            .set_metadata_base(base.trim_end_matches("/v1").to_owned());
        self
    }
}

/// The regional host for `location`. `global` has no prefix; every region does.
fn host(location: &str) -> String {
    if location == "global" {
        "https://aiplatform.googleapis.com".to_owned()
    } else {
        format!("https://{location}-aiplatform.googleapis.com")
    }
}

fn endpoint(base: &str, project: &str, location: &str, model: &str) -> String {
    format!(
        "{base}/v1/projects/{project}/locations/{location}/publishers/google/models/{model}:generateContent"
    )
}

#[async_trait]
impl LlmProvider for VertexProvider {
    fn name(&self) -> &'static str {
        "vertex"
    }

    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let token = {
            let mut tokens = self.tokens.lock().await;
            tokens.token(&self.http).await.map_err(LlmError::Auth)?
        };
        let res = self
            .http
            .post(&self.url)
            .bearer_auth(&token)
            .json(&build_body(req))
            .send()
            .await
            .map_err(|e| transport_error(SERVICE, &e))?;
        let status = res.status();
        if !status.is_success() {
            if matches!(
                status,
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
            ) {
                // Drop the cached token so the next attempt re-mints rather than replaying one the
                // server has stopped accepting (rotated key, revoked binding, clock skew) — the
                // same rule the BigQuery sender applies.
                self.tokens.lock().await.invalidate();
            }
            let detail = res.text().await.ok().and_then(|b| error_detail(&b));
            return Err(status_error(SERVICE, status, detail));
        }
        let payload: GenerateResponse = res.json().await.map_err(|e| {
            LlmError::Malformed(format!("{SERVICE} sent an unreadable response: {e}"))
        })?;
        read_response(SERVICE, payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req() -> LlmRequest {
        LlmRequest::single("sys".to_owned(), "usr".to_owned(), 2048)
    }

    #[test]
    fn a_region_selects_its_own_host() {
        assert_eq!(
            host("us-central1"),
            "https://us-central1-aiplatform.googleapis.com"
        );
        assert_eq!(
            host("europe-west4"),
            "https://europe-west4-aiplatform.googleapis.com"
        );
        // `global` is the one location with no prefix.
        assert_eq!(host("global"), "https://aiplatform.googleapis.com");
    }

    #[test]
    fn the_endpoint_is_the_documented_publisher_path() {
        let url = endpoint(
            &host("us-central1"),
            "my-project",
            "us-central1",
            "gemini-2.5-pro",
        );
        assert_eq!(
            url,
            "https://us-central1-aiplatform.googleapis.com/v1/projects/my-project/locations/us-central1/publishers/google/models/gemini-2.5-pro:generateContent"
        );
    }

    #[test]
    fn an_identifier_cannot_reshape_the_url() {
        // Every one of these would otherwise change which host or path the prompt is posted to.
        for (project, location, model) in [
            ("../evil", "us-central1", "gemini-2.5-pro"),
            ("p", "us-central1/../..", "gemini-2.5-pro"),
            ("p", "evil.example.com", "m/../../x"),
            ("", "us-central1", "gemini-2.5-pro"),
            ("p", "", "gemini-2.5-pro"),
            ("p", "us-central1", ""),
            ("p@evil", "us-central1", "gemini-2.5-pro"),
        ] {
            assert!(
                VertexProvider::new(project, location, model, None).is_err(),
                "{project}/{location}/{model} must be refused"
            );
        }
    }

    #[test]
    fn a_valid_config_builds_and_names_itself() {
        let p =
            VertexProvider::new(" my-project ", " us-central1 ", " gemini-2.5-pro ", None).unwrap();
        assert_eq!(p.name(), "vertex");
        assert!(p
            .url
            .contains("/projects/my-project/locations/us-central1/"));
    }

    #[tokio::test]
    async fn a_completion_mints_a_token_then_posts_with_it() {
        // First reply is the metadata server's token (Workload Identity path), second the model's.
        let (addr, seen) = super::super::testsupport::serve(vec![
            (
                200,
                json!({ "access_token": "ya29.test", "expires_in": 3600 }).to_string(),
            ),
            (
                200,
                json!({
                    "candidates": [{
                        "content": { "parts": [{ "text": "root cause: power" }] },
                        "finishReason": "STOP",
                    }],
                    "usageMetadata": { "promptTokenCount": 5, "candidatesTokenCount": 7 },
                })
                .to_string(),
            ),
        ])
        .await;
        let base = format!("http://{addr}");
        let out = VertexProvider::new("p", "us-central1", "gemini-2.5-pro", None)
            .unwrap()
            .with_endpoints(&base, "p", "us-central1", "gemini-2.5-pro")
            .complete(&req())
            .await
            .unwrap();
        assert_eq!(out.text(), "root cause: power");
        assert_eq!((out.in_tokens, out.out_tokens), (Some(5), Some(7)));

        let reqs = seen.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        // Header names arrive lowercased on the wire, so match case-insensitively.
        let head0 = reqs[0].head.to_ascii_lowercase();
        let head1 = reqs[1].head.to_ascii_lowercase();
        // The metadata server is asked first, with the header it requires.
        assert!(
            head0.contains("/computemetadata/v1/instance/service-accounts/default/token"),
            "{}",
            reqs[0].head
        );
        assert!(
            head0.contains("metadata-flavor: google"),
            "{}",
            reqs[0].head
        );
        // ...then the model call carries the minted bearer.
        assert!(
            head1.contains("authorization: bearer ya29.test"),
            "{}",
            reqs[1].head
        );
        assert!(head1.contains(":generatecontent"), "{}", reqs[1].head);
        // The body is the shared Gemini shape.
        let body: serde_json::Value = serde_json::from_str(&reqs[1].body).unwrap();
        assert_eq!(body["contents"][0]["parts"][0]["text"], json!("usr"));
    }

    #[tokio::test]
    async fn a_rejected_token_is_dropped_so_the_next_attempt_re_mints() {
        let (addr, seen) = super::super::testsupport::serve(vec![
            (200, json!({ "access_token": "t1", "expires_in": 3600 }).to_string()),
            (401, json!({ "error": { "status": "UNAUTHENTICATED" } }).to_string()),
            (200, json!({ "access_token": "t2", "expires_in": 3600 }).to_string()),
            (
                200,
                json!({
                    "candidates": [{ "content": { "parts": [{ "text": "ok" }] }, "finishReason": "STOP" }],
                })
                .to_string(),
            ),
        ])
        .await;
        let base = format!("http://{addr}");
        let p = VertexProvider::new("p", "us-central1", "m", None)
            .unwrap()
            .with_endpoints(&base, "p", "us-central1", "m");
        assert!(matches!(p.complete(&req()).await, Err(LlmError::Auth(_))));
        assert_eq!(p.complete(&req()).await.unwrap().text(), "ok");
        // Two token requests, not one — the rejected token was not replayed.
        let reqs = seen.lock().unwrap();
        assert_eq!(reqs.iter().filter(|r| r.head.contains("/token")).count(), 2);
    }
}
