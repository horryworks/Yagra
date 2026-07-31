// SPDX-License-Identifier: AGPL-3.0-only
//! The on-demand RCA path (ADR-029 Increment 1): click → context → prompt → provider → report.
//!
//! **Nothing in the alert path calls in here.** Hysteresis, dependency suppression, dedup and
//! notification behave identically whether a provider is configured, answers, refuses or times out.
//! An RCA is an explanation attached to an incident, never a precondition for raising one — which
//! is what lets every failure below degrade to "no explanation this time" instead of a missed page.
//!
//! Three things bound what an operator's click can cost, and they are deliberately different
//! mechanisms because they fail differently:
//!
//! * **The cache** removes the call entirely. Same evidence ⇒ same digest ⇒ the stored report.
//!   Beyond saving money this is a *correctness* property: model output is non-deterministic, and
//!   an explanation that rewords itself every time you reopen it is one nobody comes to trust.
//! * **The rate limit** bounds arrival — ten new generations a minute, shared across all callers.
//! * **The concurrency cap** bounds simultaneity, so a burst queues at two in flight rather than
//!   opening thirty sockets and thirty prompts' worth of billing at once.
//!
//! This mirrors the admission control on Troubleshoot analyses, down to sharing
//! [`crate::ratelimit::charge_window`]: both are reads that happen to be expensive, and ADR-028
//! WS-A already concluded that the guard rail for those is a cap rather than a role.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use uuid::Uuid;
use yagra_common::{CheckId, NodeId};

use super::answer;
use super::context::{self, IncidentContext};
use super::prompt::{self, Language};
use super::provider::{LlmError, LlmProvider};
use super::store::{ActiveConfig, NewReport, RcaRepo, RcaReport, ReportBody};
use crate::alerts::AlertManager;
use crate::analysis::AnalysisRunner;
use crate::audit::AuditRepo;
use crate::ratelimit::{charge_window, env_cap};
use crate::repo::NodeRepo;

/// Default simultaneous generations (`YAGRA_RCA_MAX_CONCURRENT`). Two, not four: unlike an analysis
/// this call is billed and leaves the building.
const DEFAULT_MAX_CONCURRENT: usize = 2;
/// Default new generations admitted per [`RATE_WINDOW`] (`YAGRA_RCA_RATE_PER_MIN`).
const DEFAULT_RATE_PER_MIN: usize = 10;
/// The window the arrival rate is measured over.
const RATE_WINDOW: Duration = Duration::from_secs(60);
/// Default cache lifetime (`YAGRA_RCA_CACHE_SECS`). Fifteen minutes is long enough to cover reading,
/// closing and reopening the modal during one incident, and short enough that a genuinely evolving
/// outage is re-explained — though evolution usually changes the evidence, which misses the cache on
/// its own.
const DEFAULT_CACHE_SECS: i64 = 900;
/// Timeline window when the caller does not name one, and the bounds it is clamped to.
const DEFAULT_WINDOW_SECS: i64 = 3_600;
const MIN_WINDOW_SECS: i64 = 600;
const MAX_WINDOW_SECS: i64 = 86_400;
/// σ for the timeline's anomaly scorer. The same default the Troubleshoot slider starts at.
const SENSITIVITY: f64 = 3.0;

/// Why an explanation was not produced. Each variant maps to one HTTP status at the API edge; the
/// separation exists so "you are asking too fast" never looks like "the vendor is down".
#[derive(Debug, thiserror::Error)]
pub enum RcaError {
    /// No provider configured, or configured but disabled. The default state — 503.
    #[error("AI-assisted analysis is not configured")]
    NotConfigured,
    /// Configured, but the settings cannot produce a working client — most often a provider saved
    /// without its API key. Distinct from [`Self::NotConfigured`] because the operator's next step
    /// is different ("finish the form", not "fill it in") and distinct from
    /// [`Self::Internal`] because it is not our bug. Also 503.
    #[error("{0}")]
    Misconfigured(String),
    /// The concurrency cap is full (value = the cap) — 429.
    #[error("AI analysis capacity reached ({0} already running) — retry shortly")]
    TooManyConcurrent(usize),
    /// The arrival cap is exhausted (value = the cap) — 429.
    #[error("AI analysis rate limit reached (max {0}/minute) — retry shortly")]
    RateLimited(usize),
    /// The incident could not be assembled (usually: the node is gone) — 404.
    #[error("{0}")]
    NoIncident(String),
    /// The provider failed. Carried rather than flattened so the API can distinguish a refusal from
    /// an outage — 502.
    #[error(transparent)]
    Provider(#[from] LlmError),
    /// Ours: a store or serialization failure — 500.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

/// One request for an explanation, as parsed at the API edge.
#[derive(Debug, Clone)]
pub struct RcaRequest {
    /// The node whose alert the operator clicked. May be a symptom — the context builder hops to
    /// the root cause on its own.
    pub node: NodeId,
    pub check: CheckId,
    /// How far back the timeline reaches. Clamped.
    pub window_secs: Option<i64>,
    /// What language the answer is written in (from the caller's UI locale).
    pub language: Language,
    /// Skip the cache and generate again. Still rate-limited: `force` bypasses the saving, not the
    /// protection.
    pub force: bool,
    /// Who asked, for the stored report's `created_by`.
    pub username: String,
}

/// A built provider plus the config fingerprint it was built from.
///
/// Kept between calls so a Vertex OAuth token survives, rather than paying a fresh token exchange
/// per incident. Keyed by fingerprint so a settings edit takes effect on the very next request with
/// no invalidation message to forget to send.
struct CachedClient {
    fingerprint: String,
    provider: Arc<dyn LlmProvider>,
}

/// The stores, caps and provider cache behind `POST /api/v1/rca`.
pub struct RcaOrchestrator {
    repo: Arc<RcaRepo>,
    nodes: Arc<NodeRepo>,
    alerts: Arc<AlertManager>,
    analysis: Arc<AnalysisRunner>,
    audit: Arc<AuditRepo>,
    slots: Arc<Semaphore>,
    max_concurrent: usize,
    recent_starts: Mutex<VecDeque<Instant>>,
    max_per_window: usize,
    cache_secs: i64,
    client: Mutex<Option<CachedClient>>,
}

impl RcaOrchestrator {
    #[must_use]
    pub fn new(
        repo: Arc<RcaRepo>,
        nodes: Arc<NodeRepo>,
        alerts: Arc<AlertManager>,
        analysis: Arc<AnalysisRunner>,
        audit: Arc<AuditRepo>,
    ) -> Self {
        let max_concurrent = env_cap("YAGRA_RCA_MAX_CONCURRENT", DEFAULT_MAX_CONCURRENT);
        let max_per_window = env_cap("YAGRA_RCA_RATE_PER_MIN", DEFAULT_RATE_PER_MIN);
        let cache_secs = i64::try_from(env_cap(
            "YAGRA_RCA_CACHE_SECS",
            usize::try_from(DEFAULT_CACHE_SECS).unwrap_or(900),
        ))
        .unwrap_or(DEFAULT_CACHE_SECS);
        Self {
            repo,
            nodes,
            alerts,
            analysis,
            audit,
            slots: Arc::new(Semaphore::new(max_concurrent)),
            max_concurrent,
            recent_starts: Mutex::new(VecDeque::new()),
            max_per_window,
            cache_secs,
            client: Mutex::new(None),
        }
    }

    /// Whether a provider is configured *and* enabled — drives the UI's decision to offer the
    /// button at all, so an operator is not shown an action that can only 503.
    pub async fn available(&self) -> bool {
        matches!(self.repo.active().await, Ok(Some(_)))
    }

    /// The stored report with this id (no generation, no provider call).
    ///
    /// # Errors
    /// [`RcaError::Internal`] when the store fails.
    pub async fn get(&self, id: Uuid) -> Result<Option<RcaReport>, RcaError> {
        Ok(self.repo.get(id).await?)
    }

    /// Explain the incident containing `req.node`/`req.check`.
    ///
    /// # Errors
    /// See [`RcaError`]. Every variant is a clean refusal — nothing here can leave the alert engine
    /// in a different state than it was before the call.
    pub async fn explain(&self, req: &RcaRequest) -> Result<RcaReport, RcaError> {
        let config = self.repo.active().await?.ok_or(RcaError::NotConfigured)?;

        let window = req
            .window_secs
            .unwrap_or(DEFAULT_WINDOW_SECS)
            .clamp(MIN_WINDOW_SECS, MAX_WINDOW_SECS);
        let now_s = chrono::Utc::now().timestamp();

        let sources = context::Sources {
            nodes: &self.nodes,
            alerts: &self.alerts,
            analysis: &self.analysis,
            audit: &self.audit,
        };
        let ctx = context::gather(&sources, req.node, req.check, window, SENSITIVITY, now_s)
            .await
            .map_err(RcaError::NoIncident)?;

        let digest = digest_of(&ctx, &config, req.language);

        // The cache check comes before admission on purpose: a served-from-store report costs
        // nothing external, so charging it against the rate limit would punish the cheap path.
        if !req.force {
            if let Some(mut hit) = self.repo.latest_for_digest(&digest).await? {
                if self.is_fresh(&hit, now_s) {
                    hit.cached = true;
                    metrics::counter!("yagra_rca_reports_total", "cached" => "true").increment(1);
                    return Ok(hit);
                }
            }
        }

        self.admit_rate()?;
        let _permit = Arc::clone(&self.slots)
            .try_acquire_owned()
            .map_err(|_| RcaError::TooManyConcurrent(self.max_concurrent))?;

        let provider = self.provider_for(&config)?;
        let request = prompt::render(&ctx, req.language, config.max_output_tokens);

        // Only the shape of the call is traced. The prompt carries hostnames, addresses and syslog
        // bodies, and the reply carries whatever the model made of them; neither belongs in a log.
        let started = Instant::now();
        let result = provider.complete(&request).await;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let name = provider.name();
        metrics::histogram!("yagra_llm_latency_ms", "provider" => name).record(elapsed_ms);

        let response = match result {
            Ok(r) => {
                metrics::counter!("yagra_llm_calls_total", "provider" => name, "outcome" => "ok")
                    .increment(1);
                if let Some(n) = r.in_tokens {
                    metrics::counter!("yagra_llm_tokens_total", "provider" => name, "dir" => "in")
                        .increment(u64::from(n));
                }
                if let Some(n) = r.out_tokens {
                    metrics::counter!("yagra_llm_tokens_total", "provider" => name, "dir" => "out")
                        .increment(u64::from(n));
                }
                r
            }
            Err(e) => {
                metrics::counter!("yagra_llm_calls_total", "provider" => name, "outcome" => "error")
                    .increment(1);
                metrics::counter!("yagra_llm_errors_total", "provider" => name, "reason" => e.reason())
                    .increment(1);
                // An auth failure means the operator has to go fix the settings, so it is worth a
                // log line; the rest are transient and already counted.
                if matches!(e, LlmError::Auth(_)) {
                    tracing::warn!(provider = name, "the LLM provider rejected our credentials");
                }
                // A failed call still invalidates a cached client when the cause was auth: the
                // token may simply have gone stale under a rotated service account.
                if matches!(e, LlmError::Auth(_)) {
                    self.forget_client();
                }
                return Err(RcaError::Provider(e));
            }
        };

        let answer = answer::parse(&response.text);
        let body = ReportBody {
            answer: answer.clone(),
            evidence: serde_json::to_value(&ctx).map_err(|e| anyhow::anyhow!(e))?,
            language: req.language,
        };
        let report = self
            .repo
            .insert(&NewReport {
                // Filed under the node the incident was attributed to, not the one clicked — that
                // is the node this report explains.
                node_id: ctx.root_node_id,
                check_id: req.check.0,
                context_digest: &digest,
                provider: config.kind.as_str(),
                model: &config.provider.model,
                summary: &answer.summary,
                body: serde_json::to_value(&body).map_err(|e| anyhow::anyhow!(e))?,
                created_by: &req.username,
            })
            .await?;
        metrics::counter!("yagra_rca_reports_total", "cached" => "false").increment(1);
        Ok(report)
    }

    /// Send one minimal prompt to the configured provider and report what happened.
    ///
    /// Uses [`RcaRepo::configured`], not `active`: the point of a Test button is validating a
    /// provider *before* enabling it. Deliberately outside the rate limit and the cache — it is an
    /// admin action on a settings form, not an incident path.
    ///
    /// # Errors
    /// [`RcaError::NotConfigured`] when nothing is saved; [`RcaError::Provider`] on failure.
    pub async fn test(&self) -> Result<(u128, String), RcaError> {
        let config = self
            .repo
            .configured()
            .await?
            .ok_or(RcaError::NotConfigured)?;
        let provider = self.provider_for(&config)?;
        let request = super::provider::LlmRequest {
            system: "You are a connectivity test. Reply with exactly: ok".to_owned(),
            user: "Reply with exactly: ok".to_owned(),
            // Small but not tiny: a thinking model draws its reasoning from this same budget, and
            // a two-token ceiling would come back empty and read as a failure.
            max_output_tokens: 256,
        };
        let started = Instant::now();
        let name = provider.name();
        match provider.complete(&request).await {
            Ok(r) => {
                metrics::counter!("yagra_llm_calls_total", "provider" => name, "outcome" => "ok")
                    .increment(1);
                Ok((started.elapsed().as_millis(), r.text.trim().to_owned()))
            }
            Err(e) => {
                metrics::counter!("yagra_llm_calls_total", "provider" => name, "outcome" => "error")
                    .increment(1);
                metrics::counter!("yagra_llm_errors_total", "provider" => name, "reason" => e.reason())
                    .increment(1);
                self.forget_client();
                Err(RcaError::Provider(e))
            }
        }
    }

    /// Whether a stored report is still inside the cache TTL.
    fn is_fresh(&self, report: &RcaReport, now_s: i64) -> bool {
        chrono::DateTime::parse_from_rfc3339(&report.generated_at)
            .map(|t| now_s - t.timestamp() < self.cache_secs)
            .unwrap_or(false)
    }

    /// Charge one generation against the sliding window.
    fn admit_rate(&self) -> Result<(), RcaError> {
        let mut q = self
            .recent_starts
            .lock()
            .expect("rca recent_starts mutex poisoned");
        if charge_window(&mut q, Instant::now(), RATE_WINDOW, self.max_per_window) {
            Ok(())
        } else {
            Err(RcaError::RateLimited(self.max_per_window))
        }
    }

    /// The client for this config, reusing the cached one when the config has not changed.
    fn provider_for(&self, config: &ActiveConfig) -> Result<Arc<dyn LlmProvider>, RcaError> {
        let fingerprint = config_fingerprint(config);
        let mut slot = self.client.lock().expect("rca client mutex poisoned");
        if let Some(cached) = slot.as_ref() {
            if cached.fingerprint == fingerprint {
                return Ok(Arc::clone(&cached.provider));
            }
        }
        // A build failure is the operator's settings, not our bug: the text names the missing or
        // malformed field, so it goes back to them verbatim rather than becoming a 500.
        let provider =
            super::build(config.kind, &config.provider).map_err(RcaError::Misconfigured)?;
        *slot = Some(CachedClient {
            fingerprint,
            provider: Arc::clone(&provider),
        });
        Ok(provider)
    }

    /// Drop the cached client so the next call rebuilds it (and re-runs any token exchange).
    fn forget_client(&self) {
        *self.client.lock().expect("rca client mutex poisoned") = None;
    }
}

/// The cache key: the evidence, plus everything else that changes the answer.
///
/// Provider, model and language are folded in because switching any of them produces a genuinely
/// different report — serving a Gemini answer to someone who has since moved to Claude would make
/// the stored `provider` column a lie.
fn digest_of(ctx: &IncidentContext, config: &ActiveConfig, lang: Language) -> String {
    let mut h = Sha256::new();
    h.update(ctx.fingerprint().as_bytes());
    h.update(b"\x00");
    h.update(config.kind.as_str().as_bytes());
    h.update(b"\x00");
    h.update(config.provider.model.as_bytes());
    h.update(b"\x00");
    h.update(format!("{lang:?}").as_bytes());
    hex(&h.finalize())
}

/// Identity of a provider configuration, for the client cache.
///
/// The credential is **hashed, never stored** in the fingerprint: a rotated key must invalidate the
/// cached client, but a plaintext key sitting in a long-lived struct field is exactly the kind of
/// incidental copy security.md exists to prevent.
fn config_fingerprint(config: &ActiveConfig) -> String {
    let mut h = Sha256::new();
    h.update(config.kind.as_str().as_bytes());
    h.update(b"\x00");
    h.update(config.provider.model.as_bytes());
    h.update(b"\x00");
    h.update(config.provider.project.as_bytes());
    h.update(b"\x00");
    h.update(config.provider.location.as_bytes());
    h.update(b"\x00");
    h.update(
        config
            .provider
            .secret
            .as_deref()
            .unwrap_or_default()
            .as_bytes(),
    );
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rca::context::{AlertFacts, Dependents, NodeFacts};
    use crate::rca::{ProviderConfig, ProviderKind};
    use std::net::IpAddr;

    fn node_facts(name: &str) -> NodeFacts {
        NodeFacts {
            name: name.to_owned(),
            address: "192.168.10.1".parse::<IpAddr>().unwrap(),
            vendor: Some("Cisco".to_owned()),
            model: Some("C9300".to_owned()),
            pool: None,
            tags: Vec::new(),
        }
    }

    fn ctx() -> IncidentContext {
        IncidentContext {
            generated_at_s: 1_000_000,
            window_secs: 3_600,
            root_node_id: Uuid::from_u128(7),
            node: node_facts("core-sw-01"),
            alert: AlertFacts {
                severity: "critical".to_owned(),
                state: "unreachable".to_owned(),
                metric: "__liveness__".to_owned(),
                at_unix_ms: 999_400_000,
                flapping: false,
                breach: None,
                asked_about: None,
            },
            dependents: Dependents::default(),
            upstream: Vec::new(),
            timeline: Vec::new(),
            recent_changes: Vec::new(),
        }
    }

    fn config(kind: ProviderKind, model: &str) -> ActiveConfig {
        ActiveConfig {
            kind,
            provider: ProviderConfig {
                model: model.to_owned(),
                project: "p".to_owned(),
                location: "global".to_owned(),
                secret: Some("key-1".to_owned()),
            },
            max_output_tokens: 8192,
        }
    }

    #[test]
    fn the_same_incident_asked_twice_hits_the_cache() {
        let a = digest_of(
            &ctx(),
            &config(ProviderKind::Claude, "m"),
            Language::English,
        );
        let b = digest_of(
            &ctx(),
            &config(ProviderKind::Claude, "m"),
            Language::English,
        );
        assert_eq!(a, b);
    }

    #[test]
    fn the_clock_alone_does_not_miss_the_cache() {
        // The whole point of keying on evidence rather than on the request: reopening the modal
        // five minutes later must not re-bill the operator for the same question.
        let mut later = ctx();
        later.generated_at_s += 300;
        later.window_secs = 7_200;
        assert_eq!(
            digest_of(
                &ctx(),
                &config(ProviderKind::Claude, "m"),
                Language::English
            ),
            digest_of(
                &later,
                &config(ProviderKind::Claude, "m"),
                Language::English
            )
        );
    }

    #[test]
    fn an_anomaly_score_wobble_does_not_miss_the_cache() {
        use crate::analysis::IncidentSignal;
        let mut a = ctx();
        a.timeline.push(IncidentSignal {
            at_s: 999_000,
            severity: 4.11,
            kind: "metric",
            label: "icmp_rtt_ms spike".to_owned(),
        });
        let mut b = a.clone();
        b.timeline[0].severity = 4.37; // recomputed over a slightly shifted window
        assert_eq!(
            digest_of(&a, &config(ProviderKind::Claude, "m"), Language::English),
            digest_of(&b, &config(ProviderKind::Claude, "m"), Language::English)
        );
    }

    #[test]
    fn new_evidence_misses_the_cache() {
        use crate::analysis::IncidentSignal;
        let mut grown = ctx();
        grown.timeline.push(IncidentSignal {
            at_s: 999_500,
            severity: 9.0,
            kind: "event",
            label: "LINK-3-UPDOWN".to_owned(),
        });
        assert_ne!(
            digest_of(
                &ctx(),
                &config(ProviderKind::Claude, "m"),
                Language::English
            ),
            digest_of(
                &grown,
                &config(ProviderKind::Claude, "m"),
                Language::English
            )
        );
    }

    #[test]
    fn more_dependents_miss_the_cache() {
        // A cascade that grew from 3 nodes to 40 is a different incident to explain.
        let mut grown = ctx();
        grown.dependents = Dependents {
            named: vec!["a".to_owned()],
            total: 40,
        };
        assert_ne!(
            digest_of(
                &ctx(),
                &config(ProviderKind::Claude, "m"),
                Language::English
            ),
            digest_of(
                &grown,
                &config(ProviderKind::Claude, "m"),
                Language::English
            )
        );
    }

    #[test]
    fn switching_model_provider_or_language_misses_the_cache() {
        let base = digest_of(
            &ctx(),
            &config(ProviderKind::Claude, "m"),
            Language::English,
        );
        assert_ne!(
            base,
            digest_of(
                &ctx(),
                &config(ProviderKind::Claude, "m2"),
                Language::English
            )
        );
        assert_ne!(
            base,
            digest_of(
                &ctx(),
                &config(ProviderKind::Gemini, "m"),
                Language::English
            )
        );
        assert_ne!(
            base,
            digest_of(
                &ctx(),
                &config(ProviderKind::Claude, "m"),
                Language::Japanese
            )
        );
    }

    #[test]
    fn a_rotated_credential_invalidates_the_cached_client() {
        let mut rotated = config(ProviderKind::Gemini, "m");
        rotated.provider.secret = Some("key-2".to_owned());
        assert_ne!(
            config_fingerprint(&config(ProviderKind::Gemini, "m")),
            config_fingerprint(&rotated)
        );
    }

    #[test]
    fn the_client_fingerprint_does_not_contain_the_credential() {
        let fp = config_fingerprint(&config(ProviderKind::Gemini, "m"));
        assert!(
            !fp.contains("key-1"),
            "the fingerprint is a hash, not a copy"
        );
        assert_eq!(fp.len(), 64, "sha256 hex");
    }

    #[test]
    fn two_symptoms_of_one_cascade_share_a_cache_entry() {
        // Both clicks hop to the same root, so both produce the same context and the second is
        // served from the first's report — the cascade is explained once, not once per victim.
        let mut from_a = ctx();
        from_a.alert.asked_about = Some("access-sw-09".to_owned());
        let mut from_b = from_a.clone();
        from_b.alert.asked_about = Some("access-sw-09".to_owned());
        assert_eq!(
            digest_of(
                &from_a,
                &config(ProviderKind::Claude, "m"),
                Language::English
            ),
            digest_of(
                &from_b,
                &config(ProviderKind::Claude, "m"),
                Language::English
            )
        );
        // …but the node the operator clicked is named in the prompt, so two *different* symptoms
        // legitimately get differently-worded answers.
        from_b.alert.asked_about = Some("access-sw-11".to_owned());
        assert_ne!(
            digest_of(
                &from_a,
                &config(ProviderKind::Claude, "m"),
                Language::English
            ),
            digest_of(
                &from_b,
                &config(ProviderKind::Claude, "m"),
                Language::English
            )
        );
    }
}
