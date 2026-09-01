// SPDX-License-Identifier: AGPL-3.0-only
//! **How a report gets made** — insert the run row, spawn the task, drive the sections, persist,
//! broadcast.
//!
//! Generation never touches a device, so (like the analysis runner, ADR-022) it is a background
//! \`tokio\` task inside core rather than a poller/bus job. [\`ReportRunner::run_now\`] returns as
//! soon as the row exists; everything after that happens on the spawned task.
//!
//! 🚨 **[\`ReportRunner::render_section\`] delegates and nothing else**, and \`super::guards\` refuses
//! a seam inside it — the handle names it looks for are **derived from the struct below**, so a
//! renamed or folded-away field cannot quietly turn that check into a needle matching nothing
//! (ADR-112 decision 5 — which is precisely what folding \`history\` into \`alerts\` would have
//! done). A dispatch
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

use super::seams::{AlertFacts, FleetInventory, LiveAlerts, RunStore};

// ── Runner ─────────────────────────────────────────────────────────────────────────────

/// Orchestrates report generation: create a run → background task → render sections (progress
/// persisted + broadcast over SSE) → persist the result. Holds the read seams the renderers use.
pub struct ReportRunner {
    pub(super) runs: Arc<dyn RunStore>,
    pub(super) store: Arc<dyn MetricStore>,
    pub(super) nodes: Arc<dyn FleetInventory>,
    pub(super) alerts: Arc<dyn AlertFacts>,
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
        Self::from_seams(
            repo,
            store,
            nodes,
            Arc::new(LiveAlerts::new(alerts, history)),
        )
    }

    /// The same runner over arbitrary seams — what a behaviour test builds (ADR-112).
    /// Production goes through [`ReportRunner::new`], which wires the live stores into this.
    pub(super) fn from_seams(
        runs: Arc<dyn RunStore>,
        store: Arc<dyn MetricStore>,
        nodes: Arc<dyn FleetInventory>,
        alerts: Arc<dyn AlertFacts>,
    ) -> Self {
        let (tx, _) = broadcast::channel(EVENT_BUFFER);
        Self {
            runs,
            store,
            nodes,
            alerts,
            tx,
        }
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
        let Some(def) = self.runs.get_definition(definition_id).await? else {
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
            .runs
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
        if let Err(e) = self.runs.set_run_progress(id, pct).await {
            tracing::warn!(error = %e, run = %id, "report progress update failed");
        }
        if let Ok(Some(run)) = self.runs.get_run(id).await {
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

        match self.runs.finish_run(id, &result, &html).await {
            Ok(()) => {}
            Err(e) => {
                tracing::error!(error = %e, run = %id, "failed to persist report result");
                let _ = self.runs.fail_run(id, "failed to persist report").await;
            }
        }
        if let Ok(Some(run)) = self.runs.get_run(id).await {
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

#[cfg(test)]
mod tests {
    use super::testkit::{definition, harness, n, uid};
    use super::*;

    /// A spec with the given section kinds and no per-section settings.
    fn spec_of(kinds: &[&str]) -> ReportSpec {
        ReportSpec {
            params: ReportParams { range_secs: None },
            sections: kinds
                .iter()
                .map(|k| SectionSpec {
                    id: None,
                    kind: (*k).to_owned(),
                    settings: serde_json::Value::Null,
                })
                .collect(),
        }
    }

    /// The window `run_now` decided on, in seconds, for a definition asking for `range_secs`.
    async fn window_for(range_secs: Option<i64>) -> i64 {
        let spec = range_secs.map_or_else(
            || serde_json::json!({ "sections": [] }),
            |s| serde_json::json!({ "params": { "range_secs": s }, "sections": [] }),
        );
        let h = harness().definition(definition(uid(7), spec)).build();
        let run = h
            .runner
            .run_now(uid(7), ReportRunTrigger::Manual, None)
            .await
            .expect("the definition read succeeded")
            .expect("a run row");
        (run.range_to_ms.expect("to") - run.range_from_ms.expect("from")) / 1000
    }

    // ── The run's life ───────────────────────────────────────────────────────────────────────

    /// 🎯 The refusal must happen **before** the insert. A run row for a definition that does not
    /// exist would sit in the saved list forever: nothing generates it, and `fail_orphans` only
    /// sweeps rows a previous process left `running`.
    #[tokio::test]
    async fn an_unknown_definition_returns_nothing_and_inserts_no_run() {
        let h = harness().build();
        let out = h
            .runner
            .run_now(uid(1), ReportRunTrigger::Manual, None)
            .await
            .expect("the read itself succeeded");
        assert!(out.is_none(), "a run was produced for no definition");
        assert_eq!(n(&h.calls.get_definition), 1);
        assert_eq!(
            n(&h.calls.insert_run),
            0,
            "a row was inserted for a definition that does not exist"
        );
    }

    /// The accept side of the test above: a definition that *does* exist inserts exactly one row.
    #[tokio::test]
    async fn a_known_definition_inserts_exactly_one_run() {
        let h = harness()
            .definition(definition(uid(7), serde_json::json!({ "sections": [] })))
            .build();
        let run = h
            .runner
            .run_now(
                uid(7),
                ReportRunTrigger::Scheduled,
                Some("alice".to_owned()),
            )
            .await
            .expect("read")
            .expect("a run row");
        assert_eq!(n(&h.calls.insert_run), 1);
        assert_eq!(run.definition_id, Some(uid(7)));
        assert_eq!(run.created_by.as_deref(), Some("alice"));
        assert_eq!(run.trigger, ReportRunTrigger::Scheduled);
    }

    #[tokio::test]
    async fn a_definition_that_names_no_window_gets_seven_days() {
        assert_eq!(window_for(None).await, 7 * 86_400);
    }

    #[tokio::test]
    async fn a_window_shorter_than_five_minutes_is_widened_to_five() {
        assert_eq!(window_for(Some(10)).await, 300);
    }

    #[tokio::test]
    async fn a_window_longer_than_a_year_is_narrowed_to_one() {
        assert_eq!(window_for(Some(10 * 365 * 86_400)).await, 365 * 86_400);
    }

    /// The spec is an opaque document owned by the WebUI, so a shape this core cannot read has to
    /// degrade rather than fail — and the degraded shape is "no sections", which is the input the
    /// `.max(1)` in [`ReportRunner::generate`] exists for.
    #[tokio::test]
    async fn a_spec_that_does_not_parse_produces_a_run_with_no_sections() {
        let h = harness()
            .definition(definition(uid(7), serde_json::json!("not a spec at all")))
            .build();
        let run = h
            .runner
            .run_now(uid(7), ReportRunTrigger::Manual, None)
            .await
            .expect("read")
            .expect("a run row");
        assert_eq!(run.section_count, 0);
    }

    /// 🎯 Zero sections is a real input (above), and `i * 90 / total` divides by it.
    #[tokio::test]
    async fn a_report_with_no_sections_generates_rather_than_dividing_by_zero() {
        let h = harness().started_run().build();
        h.runner
            .clone()
            .generate(uid(900), "Empty".to_owned(), ReportSpec::default(), 0, 60)
            .await;
        assert_eq!(n(&h.calls.finish_run), 1);
        assert_eq!(n(&h.calls.fail_run), 0);
    }

    #[tokio::test]
    async fn progress_is_reported_once_per_section_and_once_before_persisting() {
        let h = harness().started_run().build();
        h.runner
            .clone()
            .generate(
                uid(900),
                "R".to_owned(),
                spec_of(&["alert-summary", "alert-summary", "alert-summary"]),
                0,
                60,
            )
            .await;
        assert_eq!(
            *h.runs.progress.lock().expect("poisoned"),
            vec![0, 30, 60, 95]
        );
    }

    /// Every persisted tick is also pushed to the SSE stream, which is what moves the progress bar
    /// in the WebUI. The receiver is taken in `build`, before anything can send.
    #[tokio::test]
    async fn every_progress_tick_reaches_the_live_stream() {
        let mut h = harness().started_run().build();
        h.runner
            .clone()
            .generate(uid(900), "R".to_owned(), spec_of(&["alert-summary"]), 0, 60)
            .await;
        let mut frames = 0;
        while h.events.try_recv().is_ok() {
            frames += 1;
        }
        assert_eq!(
            frames, 3,
            "one frame per progress tick (2) plus the terminal one"
        );
    }

    /// 🚨 The only path in the product that ever writes `failed`, and until ADR-112 nothing outside
    /// production had run it.
    #[tokio::test]
    async fn a_run_whose_result_cannot_be_persisted_is_marked_failed() {
        let h = harness().started_run().finish_fails().build();
        h.runner
            .clone()
            .generate(uid(900), "R".to_owned(), ReportSpec::default(), 0, 60)
            .await;
        assert_eq!(n(&h.calls.finish_run), 1);
        assert_eq!(
            n(&h.calls.fail_run),
            1,
            "the persist failed and nothing recorded it"
        );
        assert_eq!(
            h.runs.failed.lock().expect("poisoned").len(),
            1,
            "fail_run was called without a reason"
        );
    }

    #[tokio::test]
    async fn a_successful_run_persists_one_entry_and_one_heading_per_section() {
        let h = harness().started_run().build();
        h.runner
            .clone()
            .generate(
                uid(900),
                "Weekly".to_owned(),
                spec_of(&["alert-summary", "inventory-listing"]),
                0,
                60,
            )
            .await;
        assert_eq!(n(&h.calls.fail_run), 0);
        let (data, html) = h
            .runs
            .finished
            .lock()
            .expect("poisoned")
            .clone()
            .expect("finish_run was handed a document");
        assert_eq!(data["title"], "Weekly");
        assert_eq!(
            data["sections"]
                .as_array()
                .expect("sections is an array")
                .len(),
            2
        );
        assert_eq!(html.matches("<h2>").count(), 2);
    }

    // ── The dispatch ─────────────────────────────────────────────────────────────────────────

    /// 🎯 **Every catalogued kind has a renderer**, checked by running the dispatch rather than by
    /// reading it.
    ///
    /// `catalog_kinds_are_known` claims this in a comment — "every catalog kind round-trips through
    /// the renderer's match (no unknown placeholder)" — and then asserts only that the catalog has
    /// eight entries, which a kind with no arm satisfies perfectly. Nothing could check it before:
    /// `render_section` is a method on a value no test could build (ADR-112).
    #[tokio::test]
    async fn every_catalogued_section_kind_reaches_a_renderer_of_its_own() {
        let h = harness().build();
        for def in section_catalog() {
            let sec = h
                .runner
                .render_section(
                    &SectionSpec {
                        id: None,
                        kind: def.kind.to_owned(),
                        settings: serde_json::Value::Null,
                    },
                    0,
                    60,
                )
                .await;
            assert_eq!(sec.kind, def.kind);
            assert!(
                !sec.title.starts_with("Unknown"),
                "`{}` is offered by the catalog and falls to the placeholder",
                def.kind
            );
        }
    }

    /// The accept side: a kind the catalog does not offer still renders, as a placeholder, and the
    /// run around it succeeds. An older core reading a newer WebUI's spec depends on this.
    #[tokio::test]
    async fn a_kind_this_build_does_not_know_renders_a_placeholder_and_the_run_still_succeeds() {
        let h = harness().started_run().build();
        h.runner
            .clone()
            .generate(
                uid(900),
                "R".to_owned(),
                spec_of(&["invented-by-a-newer-webui"]),
                0,
                60,
            )
            .await;
        assert_eq!(n(&h.calls.fail_run), 0);
        let (data, _) = h
            .runs
            .finished
            .lock()
            .expect("poisoned")
            .clone()
            .expect("a document");
        assert_eq!(data["sections"][0]["kind"], "invented-by-a-newer-webui");
        assert!(data["sections"][0]["title"]
            .as_str()
            .expect("a title")
            .starts_with("Unknown section"));
    }

    /// A section may name its own id; without one the kind is the id. Two sections of the same kind
    /// in one report are told apart by it.
    #[tokio::test]
    async fn a_section_without_an_id_is_identified_by_its_kind() {
        let h = harness().build();
        let named = h
            .runner
            .render_section(
                &SectionSpec {
                    id: Some("first".to_owned()),
                    kind: "alert-summary".to_owned(),
                    settings: serde_json::Value::Null,
                },
                0,
                60,
            )
            .await;
        let unnamed = h
            .runner
            .render_section(
                &SectionSpec {
                    id: None,
                    kind: "alert-summary".to_owned(),
                    settings: serde_json::Value::Null,
                },
                0,
                60,
            )
            .await;
        assert_eq!(named.id, "first");
        assert_eq!(unnamed.id, "alert-summary");
    }
}
