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
//! * **The cache** removes the call entirely. The same incident within the TTL ⇒ the stored report
//!   (ADR-172 決定 3 — it was the same *evidence* until then, which during an outage changes most
//!   minutes).
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
/// Default provider round-trips one explanation may take (`YAGRA_RCA_MAX_TURNS`, ADR-028 WS-G).
///
/// **Set it to 1 to get Increment 1 back exactly**: one turn means no tools are offered, so the
/// request is byte-identical to the single-shot one and no adapter emits a `tools` field. Six is
/// enough for "look at the alert, check the poller is up, pull the interface series, check what
/// syslog said, read the threshold, answer" without being enough to wander.
const DEFAULT_MAX_TURNS: usize = 6;
/// Default wall-clock ceiling for one whole explanation (`YAGRA_RCA_TASK_BUDGET_SECS`).
///
/// The 60s HTTP timeout bounds one *call*; six of them plus tool work does not fit in it, and a
/// concurrency permit is held for the whole task. This is what stops two slow explanations parking
/// both slots for ten minutes.
const DEFAULT_TASK_BUDGET_SECS: u64 = 240;
/// Total tool output one explanation may accumulate, in characters.
///
/// `prompt::MAX_PROMPT_CHARS` bounds the **seed**, and bounded exactly one thing while the call was
/// single-shot. Tool results are appended after it, so without this a six-turn run could carry six
/// times `MAX_TOOL_RESULT_CHARS` on top of the seed and walk into the provider's context window —
/// which fails as a 400 mid-incident rather than as a smaller answer.
const MAX_TOOL_CHARS_TOTAL: usize = 60_000;

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
    /// What the caller may see (ADR-028 WS-G).
    ///
    /// Carried rather than resolved here, because it is the *caller's* scope and the only place it
    /// can be resolved is the edge that authenticated them. Both edges already had it and threw it
    /// away after `require_visible_node`; the agent needs it for every tool it runs, and a
    /// "privileged in-process client" that defaulted to [`NodeScope::All`] would hand a
    /// group-scoped operator the whole fleet.
    pub scope: crate::api::scope::NodeScope,
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

/// What a generation hands back, to every caller waiting on it.
type Outcome = Result<RcaReport, Arc<RcaError>>;

/// One incident's explanation: the node it is filed under, the check, the language.
type RunKey = (uuid::Uuid, uuid::Uuid, Language);

/// A generation any number of callers can wait on. It runs in a task of its own, so dropping every
/// handle does not stop it.
type SharedRun = futures::future::Shared<futures::future::BoxFuture<'static, Outcome>>;

/// What admission decided.
enum Admitted {
    Cached(RcaReport),
    Running(SharedRun),
}

fn generation_panicked() -> Outcome {
    Err(Arc::new(RcaError::Internal(anyhow::anyhow!(
        "the explanation task stopped unexpectedly"
    ))))
}

/// The work running right now, keyed so a second request for the same thing waits for the first
/// instead of starting another (ADR-172 決定 3).
///
/// Only this process's runs: a request that reaches another core does not join, and finds the
/// report in the store once the first one lands.
pub(crate) struct InFlight<K, V: Clone> {
    runs: Mutex<std::collections::HashMap<K, (u64, SharedFuture<V>)>>,
    next: std::sync::atomic::AtomicU64,
}

type SharedFuture<V> = futures::future::Shared<futures::future::BoxFuture<'static, V>>;

impl<K, V> InFlight<K, V>
where
    K: Eq + std::hash::Hash + Clone + Send + 'static,
    V: Clone + Send + Sync + 'static,
{
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            runs: Mutex::new(std::collections::HashMap::new()),
            next: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// The run for `key`, if one is going.
    pub(crate) fn join(&self, key: &K) -> Option<SharedFuture<V>> {
        self.runs
            .lock()
            .expect("in-flight mutex poisoned")
            .get(key)
            .map(|(_, run)| run.clone())
    }

    /// Start `work` in a task of its own and register it under `key`, replacing any run already
    /// there (a forced regeneration). The entry leaves when the run ends — however it ends.
    pub(crate) fn start<F>(
        self: &Arc<Self>,
        key: K,
        work: F,
        on_panic: fn() -> V,
    ) -> SharedFuture<V>
    where
        F: std::future::Future<Output = V> + Send + 'static,
    {
        use futures::FutureExt as _;
        // Held across the spawn and the insert, so a run that finishes at once cannot remove its
        // entry before the entry exists — and so leave a finished run registered for ever.
        let mut runs = self.runs.lock().expect("in-flight mutex poisoned");
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let done = Arc::clone(self);
        let done_key = key.clone();
        let task = tokio::spawn(async move {
            let v = work.await;
            done.finish(&done_key, id);
            v
        });
        let failed = Arc::clone(self);
        let failed_key = key.clone();
        let run = async move {
            match task.await {
                Ok(v) => v,
                Err(_) => {
                    failed.finish(&failed_key, id);
                    on_panic()
                }
            }
        }
        .boxed()
        .shared();
        runs.insert(key, (id, run.clone()));
        run
    }

    /// Remove `key`'s entry if it is still run `id` — a forced run may have replaced it.
    fn finish(&self, key: &K, id: u64) {
        let mut runs = self.runs.lock().expect("in-flight mutex poisoned");
        if runs.get(key).is_some_and(|(current, _)| *current == id) {
            runs.remove(key);
        }
    }
}

/// The stores, caps and provider cache behind `POST /api/v1/rca`.
pub struct RcaOrchestrator {
    repo: Arc<RcaRepo>,
    nodes: Arc<NodeRepo>,
    alerts: Arc<AlertManager>,
    analysis: Arc<AnalysisRunner>,
    audit: Arc<AuditRepo>,
    /// The folder tree, read once per gathered context to resolve inherited labels.
    groups: Arc<crate::groups::GroupRepo>,
    slots: Arc<Semaphore>,
    max_concurrent: usize,
    recent_starts: Mutex<VecDeque<Instant>>,
    max_per_window: usize,
    cache_secs: i64,
    max_turns: usize,
    task_budget: Duration,
    client: Mutex<Option<CachedClient>>,
    /// Generations running now, so a second request for the same incident waits for the first.
    running: Arc<InFlight<RunKey, Outcome>>,
}

impl RcaOrchestrator {
    #[must_use]
    pub fn new(
        repo: Arc<RcaRepo>,
        nodes: Arc<NodeRepo>,
        alerts: Arc<AlertManager>,
        analysis: Arc<AnalysisRunner>,
        audit: Arc<AuditRepo>,
        groups: Arc<crate::groups::GroupRepo>,
    ) -> Self {
        let max_concurrent = env_cap("YAGRA_RCA_MAX_CONCURRENT", DEFAULT_MAX_CONCURRENT);
        let max_per_window = env_cap("YAGRA_RCA_RATE_PER_MIN", DEFAULT_RATE_PER_MIN);
        let cache_secs = i64::try_from(env_cap(
            "YAGRA_RCA_CACHE_SECS",
            usize::try_from(DEFAULT_CACHE_SECS).unwrap_or(900),
        ))
        .unwrap_or(DEFAULT_CACHE_SECS);
        // Floored at 1 rather than allowed to be 0: `env_cap` clamps to at least 1, and a zero here
        // would mean "never call the provider", which is not a configuration anyone wants and would
        // read as a silent outage.
        let max_turns = env_cap("YAGRA_RCA_MAX_TURNS", DEFAULT_MAX_TURNS);
        let task_budget = Duration::from_secs(
            u64::try_from(env_cap(
                "YAGRA_RCA_TASK_BUDGET_SECS",
                usize::try_from(DEFAULT_TASK_BUDGET_SECS).unwrap_or(240),
            ))
            .unwrap_or(DEFAULT_TASK_BUDGET_SECS),
        );
        Self {
            repo,
            nodes,
            alerts,
            analysis,
            audit,
            groups,
            slots: Arc::new(Semaphore::new(max_concurrent)),
            max_concurrent,
            recent_starts: Mutex::new(VecDeque::new()),
            max_per_window,
            cache_secs,
            max_turns,
            task_budget,
            client: Mutex::new(None),
            running: InFlight::new(),
        }
    }

    /// Whether a provider is configured *and* enabled — drives the UI's decision to offer the
    /// button at all, so an operator is not shown an action that can only 503.
    pub async fn available(&self) -> bool {
        matches!(self.repo.active().await, Ok(Some(_)))
    }

    /// Explain the incident containing `req.node`/`req.check`.
    ///
    /// Three stages (ADR-172 決定 3). **Admission** runs in the caller: the config, the context, the
    /// cache, the concurrency permit and the rate window — everything that can refuse. **The
    /// generation** — the provider round trips and the stored report — runs in a task of its own,
    /// holding the permit. **The caller** then only waits for that task.
    ///
    /// 🚨 Why the split: the generation used to run inside the HTTP request, and a tab closed or
    /// reloaded mid-way dropped the request and the generation with it — tokens billed, nothing
    /// stored. Now a dropped caller leaves the task running; the report is stored, and reopening
    /// the dialog finds it in the cache or joins the run still going.
    ///
    /// # Errors
    /// See [`RcaError`]. Every variant is a clean refusal — nothing here can leave the alert engine
    /// in a different state than it was before the call. Shared behind an `Arc` because a run that
    /// two callers joined fails for both of them.
    pub async fn explain(
        self: &Arc<Self>,
        req: &RcaRequest,
        tools: super::agent::AgentTools,
    ) -> Result<RcaReport, Arc<RcaError>> {
        match self.admit(req, tools).await? {
            Admitted::Cached(report) => Ok(report),
            Admitted::Running(run) => run.await,
        }
    }

    /// Everything that can refuse, in the caller; then the generation, started or joined.
    async fn admit(
        self: &Arc<Self>,
        req: &RcaRequest,
        tools: super::agent::AgentTools,
    ) -> Result<Admitted, RcaError> {
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
            groups: &self.groups,
        };
        let ctx = context::gather(&sources, req.node, req.check, window, SENSITIVITY, now_s)
            .await
            .map_err(RcaError::NoIncident)?;

        // Whether the model gets tools. One turn means it does not, which is Increment 1 exactly.
        //
        // The *retrieval* is what varies between the two modes, not the question, so the seed
        // context stays deterministic and the digest keeps working as a record of what was asked.
        let agentic = self.max_turns > 1;
        let digest = digest_of(&ctx, &config, req.language, agentic);
        let key: RunKey = (ctx.root_node_id, req.check.0, req.language);

        // The cache check comes before admission on purpose: a served-from-store report costs
        // nothing external, so charging it against the rate limit would punish the cheap path.
        //
        // ⚠️ Keyed by the incident, no longer by the digest (ADR-172 決定 3): the digest moves with
        // the evidence, which during an outage is most minutes, so a report generated after the
        // dialog was closed would be billed again on reopening instead of shown.
        if !req.force {
            if let Some(mut hit) = self.repo.latest_for_incident(key.0, key.1, key.2).await? {
                if self.is_fresh(&hit, now_s) {
                    hit.cached = true;
                    metrics::counter!("yagra_rca_reports_total", "cached" => "true").increment(1);
                    return Ok(Admitted::Cached(hit));
                }
            }
            // The same incident is being explained right now — most often by this operator, before
            // they closed the dialog. Wait for that run rather than paying for a second one.
            if let Some(run) = self.running.join(&key) {
                metrics::counter!("yagra_rca_reports_joined_total").increment(1);
                return Ok(Admitted::Running(run));
            }
        }

        // The permit before the rate window, the order `AnalysisRunner::create` takes them in: the
        // other way round, a request refused for concurrency had already spent a rate slot.
        let permit = Arc::clone(&self.slots)
            .try_acquire_owned()
            .map_err(|_| RcaError::TooManyConcurrent(self.max_concurrent))?;
        self.admit_rate()?;
        // Built here, not in the task, so a provider saved without its key refuses the request
        // rather than failing a run nobody is waiting on.
        let provider = self.provider_for(&config)?;

        let me = Arc::clone(self);
        let req = req.clone();
        let generation = async move {
            // Held for the whole generation; dropping it at the end frees the slot.
            let _permit = permit;
            me.generate(&req, &tools, &config, provider, ctx, &digest, agentic)
                .await
                .map_err(Arc::new)
        };
        Ok(Admitted::Running(self.running.start(
            key,
            generation,
            generation_panicked,
        )))
    }

    /// The provider round trips and the stored report. Runs in its own task (see [`Self::explain`]).
    #[allow(clippy::too_many_arguments)] // one call site, and every argument is admission's output
    async fn generate(
        &self,
        req: &RcaRequest,
        tools: &super::agent::AgentTools,
        config: &ActiveConfig,
        provider: Arc<dyn LlmProvider>,
        ctx: IncidentContext,
        digest: &str,
        agentic: bool,
    ) -> Result<RcaReport, RcaError> {
        let mut request = prompt::render(&ctx, req.language, config.max_output_tokens);
        if agentic {
            request.tools = tools.schemas();
        }

        // The turn loop (ADR-028 WS-G). With `agentic == false` it runs exactly once and behaves
        // identically to Increment 1 — the model is offered no tools, so it cannot ask for one.
        let deadline = Instant::now() + self.task_budget;
        let mut transcript: Vec<super::store::ToolTurn> = Vec::new();
        let mut turn = 0usize;
        let mut tool_chars = 0usize;
        let response = loop {
            let response = self.one_call(provider.as_ref(), &request).await?;
            turn += 1;
            let calls = response.tool_calls();
            if calls.is_empty() {
                break response;
            }
            // Bounds checked *after* a turn produced calls rather than before the call, so hitting
            // the ceiling still returns the model's last answer instead of failing the request. A
            // partial explanation is worth more to an operator than a 502.
            if turn >= self.max_turns
                || Instant::now() >= deadline
                || tool_chars >= MAX_TOOL_CHARS_TOTAL
            {
                tracing::info!(
                    turns = turn,
                    max = self.max_turns,
                    tool_chars,
                    "RCA agent stopped at its budget with tool calls outstanding"
                );
                metrics::counter!("yagra_rca_agent_truncated_total").increment(1);
                break response;
            }
            // Replay what the model said, including its calls: a provider that does not see its own
            // tool_use block rejects the result that follows it.
            request
                .messages
                .push(super::provider::Turn::Assistant(response.parts.clone()));
            for (id, name, args) in calls {
                let out = tools.call(name, args.clone(), &req.scope).await;
                tool_chars += out.chars().count();
                // Fenced like any other device-supplied text. `search_events` returns syslog bodies
                // verbatim, and unlike the seed context this arrives *after* the system prompt.
                request.messages.push(super::provider::Turn::ToolResult {
                    id: id.to_owned(),
                    content: prompt::fence_tool_result(&out),
                });
                transcript.push(super::store::ToolTurn {
                    tool: name.to_owned(),
                    args: args.clone(),
                    result: out,
                });
            }
        };

        let answer = answer::parse(&response.text());
        let body = ReportBody {
            answer: answer.clone(),
            evidence: serde_json::to_value(&ctx).map_err(|e| anyhow::anyhow!(e))?,
            language: req.language,
            transcript,
        };
        let report = self
            .repo
            .insert(&NewReport {
                // Filed under the node the incident was attributed to, not the one clicked — that
                // is the node this report explains, and the node the incident cache looks under.
                node_id: ctx.root_node_id,
                check_id: req.check.0,
                context_digest: digest,
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

    /// One provider round-trip, with its metrics and its error handling.
    ///
    /// Split out of [`Self::explain`] when the single call became a loop: the counters have to be
    /// charged per **call**, not per generation, or an eight-turn explanation bills the same as a
    /// one-turn one in every metric this deployment has.
    async fn one_call(
        &self,
        provider: &dyn LlmProvider,
        request: &super::provider::LlmRequest,
    ) -> Result<super::provider::LlmResponse, RcaError> {
        // Only the shape of the call is traced. The prompt carries hostnames, addresses and syslog
        // bodies, and the reply carries whatever the model made of them; neither belongs in a log.
        let started = Instant::now();
        let result = provider.complete(request).await;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let name = provider.name();
        metrics::histogram!("yagra_llm_latency_ms", "provider" => name).record(elapsed_ms);

        match result {
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
                Ok(r)
            }
            Err(e) => {
                metrics::counter!("yagra_llm_calls_total", "provider" => name, "outcome" => "error")
                    .increment(1);
                metrics::counter!("yagra_llm_errors_total", "provider" => name, "reason" => e.reason())
                    .increment(1);
                // An auth failure means the operator has to go fix the settings, so it is worth a
                // log line; the rest are transient and already counted. It also invalidates the
                // cached client — the token may simply have gone stale under a rotated account.
                if matches!(e, LlmError::Auth(_)) {
                    tracing::warn!(provider = name, "the LLM provider rejected our credentials");
                    self.forget_client();
                }
                Err(RcaError::Provider(e))
            }
        }
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
        let request = super::provider::LlmRequest::single(
            "You are a connectivity test. Reply with exactly: ok".to_owned(),
            "Reply with exactly: ok".to_owned(),
            // Small but not tiny: a thinking model draws its reasoning from this same budget, and
            // a two-token ceiling would come back empty and read as a failure.
            256,
        );
        let started = Instant::now();
        let name = provider.name();
        match provider.complete(&request).await {
            Ok(r) => {
                metrics::counter!("yagra_llm_calls_total", "provider" => name, "outcome" => "ok")
                    .increment(1);
                Ok((started.elapsed().as_millis(), r.text().trim().to_owned()))
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
fn digest_of(
    ctx: &IncidentContext,
    config: &ActiveConfig,
    lang: Language,
    agentic: bool,
) -> String {
    let mut h = Sha256::new();
    h.update(ctx.fingerprint().as_bytes());
    h.update(b"\x00");
    h.update(config.kind.as_str().as_bytes());
    h.update(b"\x00");
    h.update(config.provider.model.as_bytes());
    h.update(b"\x00");
    h.update(format!("{lang:?}").as_bytes());
    // ADR-028 WS-G. The seed context is identical in both modes — agentic retrieval adds turns, not
    // a different question — so without this a deployment that turned tools on would keep serving
    // pre-tool answers from the cache for fifteen minutes and look like the feature had not shipped.
    h.update(b"\x00");
    h.update(if agentic { "agentic" } else { "single" }.as_bytes());
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
mod in_flight_tests {
    use super::InFlight;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn panicked() -> u32 {
        0
    }

    /// ADR-172 決定 3: a second request for the same incident waits for the first run rather than
    /// paying for another, and the run finishes even when nobody is left waiting.
    #[tokio::test]
    async fn a_second_caller_joins_the_run_and_the_run_outlives_its_callers() {
        let runs: Arc<InFlight<&'static str, u32>> = InFlight::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let (release, gate) = tokio::sync::oneshot::channel::<()>();

        let counted = Arc::clone(&calls);
        let first = runs.start(
            "incident",
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                let _ = gate.await;
                7
            },
            panicked,
        );
        let second = runs
            .join(&"incident")
            .expect("the run is registered while it runs");
        assert!(runs.join(&"another").is_none());

        // Every caller goes away — the tab was closed. The run must not go with them.
        drop(first);
        drop(second);
        release.send(()).expect("release");
        for _ in 0..100 {
            if runs.join(&"incident").is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            runs.join(&"incident").is_none(),
            "a finished run leaves the table, or the next request would be handed a stale answer"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "the work ran once");
    }

    #[tokio::test]
    async fn every_joiner_gets_the_same_answer() {
        let runs: Arc<InFlight<u8, u32>> = InFlight::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let (release, gate) = tokio::sync::oneshot::channel::<()>();
        let first = runs.start(
            1,
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                let _ = gate.await;
                42
            },
            panicked,
        );
        let second = runs.join(&1).expect("joined");
        release.send(()).expect("release");
        assert_eq!((first.await, second.await), (42, 42));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_forced_run_replaces_the_entry_and_the_old_one_does_not_remove_it() {
        let runs: Arc<InFlight<u8, u32>> = InFlight::new();
        let (release_old, old_gate) = tokio::sync::oneshot::channel::<()>();
        let old = runs.start(
            1,
            async move {
                let _ = old_gate.await;
                1
            },
            panicked,
        );
        let (release_new, new_gate) = tokio::sync::oneshot::channel::<()>();
        let new = runs.start(
            1,
            async move {
                let _ = new_gate.await;
                2
            },
            panicked,
        );
        release_old.send(()).expect("release");
        assert_eq!(old.await, 1);
        let joined = runs
            .join(&1)
            .expect("the older run ending must not remove the newer run's entry");
        release_new.send(()).expect("release");
        assert_eq!((new.await, joined.await), (2, 2));
    }

    #[tokio::test]
    async fn a_run_that_panics_answers_and_leaves_the_table() {
        let runs: Arc<InFlight<u8, u32>> = InFlight::new();
        let run = runs.start(1, async { panic!("boom") }, panicked);
        assert_eq!(run.await, 0);
        assert!(runs.join(&1).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rca::context::{AlertFacts, Dependents, NodeFacts};
    use crate::rca::{ProviderConfig, ProviderKind};
    use std::net::IpAddr;
    use uuid::Uuid;

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
            false,
        );
        let b = digest_of(
            &ctx(),
            &config(ProviderKind::Claude, "m"),
            Language::English,
            false,
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
                Language::English,
                false,
            ),
            digest_of(
                &later,
                &config(ProviderKind::Claude, "m"),
                Language::English,
                false,
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
            digest_of(
                &a,
                &config(ProviderKind::Claude, "m"),
                Language::English,
                false
            ),
            digest_of(
                &b,
                &config(ProviderKind::Claude, "m"),
                Language::English,
                false
            )
        );
    }

    /// The two retrieval modes must not share a cache entry (ADR-028 WS-G).
    ///
    /// The seed context is identical either way — agentic retrieval adds turns, not a different
    /// question — so without the mode in the digest, a deployment that turned tools on would keep
    /// serving pre-tool answers for the whole cache lifetime and look like the feature had not
    /// shipped. This is the one cache bug the loop could introduce.
    #[test]
    fn the_two_retrieval_modes_do_not_share_a_cache_entry() {
        let c = config(ProviderKind::Claude, "m");
        assert_ne!(
            digest_of(&ctx(), &c, Language::English, false),
            digest_of(&ctx(), &c, Language::English, true),
        );
    }

    /// …and each mode is still stable with itself, or the cache never hits at all.
    #[test]
    fn a_mode_is_stable_with_itself() {
        let c = config(ProviderKind::Claude, "m");
        for agentic in [false, true] {
            assert_eq!(
                digest_of(&ctx(), &c, Language::English, agentic),
                digest_of(&ctx(), &c, Language::English, agentic),
            );
        }
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
                Language::English,
                false,
            ),
            digest_of(
                &grown,
                &config(ProviderKind::Claude, "m"),
                Language::English,
                false,
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
                Language::English,
                false,
            ),
            digest_of(
                &grown,
                &config(ProviderKind::Claude, "m"),
                Language::English,
                false,
            )
        );
    }

    #[test]
    fn switching_model_provider_or_language_misses_the_cache() {
        let base = digest_of(
            &ctx(),
            &config(ProviderKind::Claude, "m"),
            Language::English,
            false,
        );
        assert_ne!(
            base,
            digest_of(
                &ctx(),
                &config(ProviderKind::Claude, "m2"),
                Language::English,
                false,
            )
        );
        assert_ne!(
            base,
            digest_of(
                &ctx(),
                &config(ProviderKind::Gemini, "m"),
                Language::English,
                false,
            )
        );
        assert_ne!(
            base,
            digest_of(
                &ctx(),
                &config(ProviderKind::Claude, "m"),
                Language::Japanese,
                false,
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
                Language::English,
                false,
            ),
            digest_of(
                &from_b,
                &config(ProviderKind::Claude, "m"),
                Language::English,
                false,
            )
        );
        // …but the node the operator clicked is named in the prompt, so two *different* symptoms
        // legitimately get differently-worded answers.
        from_b.alert.asked_about = Some("access-sw-11".to_owned());
        assert_ne!(
            digest_of(
                &from_a,
                &config(ProviderKind::Claude, "m"),
                Language::English,
                false,
            ),
            digest_of(
                &from_b,
                &config(ProviderKind::Claude, "m"),
                Language::English,
                false,
            )
        );
    }
}
