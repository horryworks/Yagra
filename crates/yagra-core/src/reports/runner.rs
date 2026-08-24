// SPDX-License-Identifier: AGPL-3.0-only
//! **How a report gets made** — insert the run row, spawn the task, drive the sections, persist,
//! broadcast.
//!
//! Generation never touches a device, so (like the analysis runner, ADR-022) it is a background
//! \`tokio\` task inside core rather than a poller/bus job. [\`ReportRunner::run_now\`] returns as
//! soon as the row exists; everything after that happens on the spawned task.
//!
//! 🚨 **[\`ReportRunner::render_section\`] delegates and nothing else**, and \`super::guards\` refuses
//! a seam (\`self.store\` / \`self.nodes\` / \`self.alerts\` / \`self.history\`) inside it. A dispatch
//! that is allowed to answer a case itself grows one: \`worker::execute\`'s HTTP arm reached 101
//! lines that way, measured on the day it was split (ADR-099).

use super::*;

use std::sync::Arc;

use tokio::sync::broadcast;
use uuid::Uuid;

use crate::alerts::AlertManager;
use crate::history::AlertHistoryStore;
use crate::repo::NodeRepo;
use crate::store::MetricStore;

// ── Runner ─────────────────────────────────────────────────────────────────────────────

/// Orchestrates report generation: create a run → background task → render sections (progress
/// persisted + broadcast over SSE) → persist the result. Holds the read seams the renderers use.
pub struct ReportRunner {
    pub(super) repo: Arc<ReportsRepo>,
    pub(super) store: Arc<dyn MetricStore>,
    pub(super) nodes: Arc<NodeRepo>,
    pub(super) alerts: Arc<AlertManager>,
    pub(super) history: Arc<AlertHistoryStore>,
    pub(super) tx: broadcast::Sender<String>,
}

impl ReportRunner {
    #[must_use]
    pub fn new(
        repo: Arc<ReportsRepo>,
        store: Arc<dyn MetricStore>,
        nodes: Arc<NodeRepo>,
        alerts: Arc<AlertManager>,
        history: Arc<AlertHistoryStore>,
    ) -> Self {
        let (tx, _) = broadcast::channel(EVENT_BUFFER);
        Self {
            repo,
            store,
            nodes,
            alerts,
            history,
            tx,
        }
    }

    /// The underlying repo (definitions/schedules/runs CRUD for the API handlers).
    #[must_use]
    pub fn repo(&self) -> Arc<ReportsRepo> {
        self.repo.clone()
    }

    /// Subscribe to the live run-status stream (SSE).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    fn broadcast_run(&self, run: &ReportRun) {
        if let Ok(json) = serde_json::to_string(run) {
            let _ = self.tx.send(json);
        }
    }

    /// Generate a report from a definition: insert a run row, spawn the background task, and return
    /// the freshly-inserted row. `Ok(None)` ⇒ no such definition.
    pub async fn run_now(
        self: &Arc<Self>,
        definition_id: Uuid,
        trigger: ReportRunTrigger,
        created_by: Option<String>,
    ) -> anyhow::Result<Option<ReportRun>> {
        let Some(def) = self.repo.get_definition(definition_id).await? else {
            return Ok(None);
        };
        let spec: ReportSpec = serde_json::from_value(def.spec.clone()).unwrap_or_default();
        let range = spec
            .params
            .range_secs
            .unwrap_or(DEFAULT_RANGE_SECS)
            .clamp(300, 365 * 86_400);
        let to_s = now_s();
        let from_s = to_s - range;
        let section_count = i32::try_from(spec.sections.len()).unwrap_or(i32::MAX);

        let run = self
            .repo
            .insert_run(
                Some(def.id),
                &def.name,
                trigger,
                from_s,
                to_s,
                section_count,
                &def.spec,
                created_by.as_deref(),
            )
            .await?;
        self.broadcast_run(&run);

        let runner = self.clone();
        let id = run.id;
        let name = def.name.clone();
        tokio::spawn(async move {
            runner.generate(id, name, spec, from_s, to_s).await;
        });
        Ok(Some(run))
    }

    /// Persist a progress tick and broadcast the updated row.
    async fn progress(&self, id: Uuid, pct: i32) {
        if let Err(e) = self.repo.set_run_progress(id, pct).await {
            tracing::warn!(error = %e, run = %id, "report progress update failed");
        }
        if let Ok(Some(run)) = self.repo.get_run(id).await {
            self.broadcast_run(&run);
        }
    }

    /// The whole job: render each section, assemble the document, persist, finalize.
    async fn generate(
        self: Arc<Self>,
        id: Uuid,
        name: String,
        spec: ReportSpec,
        from_s: i64,
        to_s: i64,
    ) {
        let total = spec.sections.len().max(1);
        let mut sections_data: Vec<serde_json::Value> = Vec::new();
        let mut body = String::new();

        for (i, sec) in spec.sections.iter().enumerate() {
            self.progress(id, (i * 90 / total) as i32).await;
            let section = self.render_section(sec, from_s, to_s).await;
            sections_data.push(section.to_data());
            body.push_str(&section.to_html());
        }
        self.progress(id, 95).await;

        let window_label = format!("{} → {}", fmt_day(from_s), fmt_day(to_s));
        let generated_label = format!("Generated {}", fmt_minute(now_s()));
        let html = render_document(&name, &window_label, &generated_label, &body);
        let result = serde_json::json!({
            "title": name,
            "generated_ms": now_ms(),
            "range_from_ms": from_s * 1000,
            "range_to_ms": to_s * 1000,
            "sections": sections_data,
        });

        match self.repo.finish_run(id, &result, &html).await {
            Ok(()) => {}
            Err(e) => {
                tracing::error!(error = %e, run = %id, "failed to persist report result");
                let _ = self.repo.fail_run(id, "failed to persist report").await;
            }
        }
        if let Ok(Some(run)) = self.repo.get_run(id).await {
            self.broadcast_run(&run);
        }
    }

    /// Render one section to its structured + HTML output. Unknown kinds render a placeholder.
    async fn render_section(&self, sec: &SectionSpec, from_s: i64, to_s: i64) -> Section {
        let id = sec.id.clone().unwrap_or_else(|| sec.kind.clone());
        match sec.kind.as_str() {
            "availability-summary" => self.render_availability(id, from_s, to_s).await,
            "alert-summary" => self.render_alert_summary(id, from_s, to_s).await,
            "top-alerting-nodes" => self.render_top_alerting(id, &sec.settings, from_s).await,
            "top-cpu" | "top-rtt" | "top-memory" => {
                self.render_top_metric(id, &sec.kind, &sec.settings).await
            }
            "throughput-trend" => self.render_throughput(id, from_s, to_s).await,
            "inventory-listing" => self.render_inventory(id, &sec.settings).await,
            other => Section {
                id,
                kind: other.to_owned(),
                title: format!("Unknown section: {other}"),
                note: Some("This section type is not supported by this server version.".to_owned()),
                ..Default::default()
            },
        }
    }
}
