// SPDX-License-Identifier: AGPL-3.0-only
//! **Where reports are kept** — definitions, schedules and runs in PostgreSQL (ADR-004).
//!
//! 🚨 **The only file in this module allowed to name a table**, and `super::guards` enforces it
//! both ways. Report generation reads stores through their own repositories; a `sqlx::query` in
//! [`super::runner`] or [`super::sections`] would be the store-separation rule breaking in the
//! place nobody looks.
//!
//! ✅ **Its statements run against a real PostgreSQL** (ADR-116). They were blocked twice over —
//! faking a `ReportsRepo` needed seams (ADR-102 決定 5, done in ADR-112) and running one needed a
//! database (ADR-114, done) — and then stayed unwritten once both obstacles were gone, which is
//! how twenty-two statements reached eight releases with none of them ever executed by a test.
//! Four `#[sqlx::test]`s now cover definitions, the schedule clock, a run from insert to finish,
//! and the two janitors.
//!
//! 🔧 **`fail_orphans` has no age window** — measured, not assumed. The sweep runs at startup,
//! when a live run cannot exist, so every queued/running row is an orphan. Its test separates the
//! two *states* rather than two ages, because a fixture built on an age would have been asserting
//! a window that is not there.

use super::*;

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::cadence::Cadence;

// ── Repository ───────────────────────────────────────────────────────────────────────

const DEF_COLS: &str = "id, name, description, spec, updated_by, \
     (EXTRACT(EPOCH FROM created_at) * 1000)::bigint AS created_ms, \
     (EXTRACT(EPOCH FROM updated_at) * 1000)::bigint AS updated_ms";

/// What narrows the saved-runs list. Every field optional; all of them ANDed.
///
/// A struct rather than three `Option` parameters, for the reason [`crate::analysis::JobFilter`]
/// is one: three optionals in a row is the call-site mistake that compiles, runs, and answers a
/// different question.
#[derive(Debug, Default, Clone, Copy)]
pub struct RunFilter {
    /// Only runs generated from this report definition. A definition that has since been deleted
    /// leaves its runs behind with the id still on them, so this can name one that no longer
    /// exists — which returns those runs rather than an error, and is the useful answer.
    pub definition_id: Option<Uuid>,
    /// Only runs in this lifecycle state.
    pub state: Option<ReportRunState>,
    /// Only runs created at or after this instant.
    pub since: Option<DateTime<Utc>>,
}

/// The saved-runs predicate: one const, every clause always present, every value a nullable bind.
/// Not assembled conditionally — a `WHERE` built by pushing clauses has a branch per filter that
/// can be forgotten, and a forgotten one fails open.
pub(super) const RUN_FILTER_WHERE: &str = "($1::uuid IS NULL OR definition_id = $1) \
     AND ($2::text IS NULL OR state = $2) \
     AND ($3::timestamptz IS NULL OR created_at >= $3)";

const RUN_COLS: &str = "id, definition_id, name, trigger, state, pct, error, \
     (EXTRACT(EPOCH FROM range_from) * 1000)::bigint AS range_from_ms, \
     (EXTRACT(EPOCH FROM range_to) * 1000)::bigint AS range_to_ms, \
     section_count, created_by, \
     (EXTRACT(EPOCH FROM created_at) * 1000)::bigint AS created_ms, \
     (EXTRACT(EPOCH FROM started_at) * 1000)::bigint AS started_ms, \
     (EXTRACT(EPOCH FROM finished_at) * 1000)::bigint AS finished_ms";

const SCHED_COLS: &str = "s.id, s.definition_id, d.name AS definition_name, s.frequency, \
     s.day_of_week, s.day_of_month, s.at_hour, s.at_minute, s.enabled, \
     (EXTRACT(EPOCH FROM s.next_run_at) * 1000)::bigint AS next_run_ms, \
     (EXTRACT(EPOCH FROM s.last_run_at) * 1000)::bigint AS last_run_ms, s.last_status";

fn def_from_row(row: &sqlx::postgres::PgRow) -> anyhow::Result<ReportDefinition> {
    Ok(ReportDefinition {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        spec: row.try_get("spec")?,
        updated_by: row.try_get("updated_by")?,
        created_ms: row.try_get("created_ms")?,
        updated_ms: row.try_get("updated_ms")?,
    })
}

fn run_from_row(row: &sqlx::postgres::PgRow) -> anyhow::Result<ReportRun> {
    Ok(ReportRun {
        id: row.try_get("id")?,
        definition_id: row.try_get("definition_id")?,
        name: row.try_get("name")?,
        trigger: ReportRunTrigger::from_stored(row.try_get("trigger")?),
        state: ReportRunState::from_stored(row.try_get("state")?),
        pct: row.try_get("pct")?,
        error: row.try_get("error")?,
        range_from_ms: row.try_get("range_from_ms")?,
        range_to_ms: row.try_get("range_to_ms")?,
        section_count: row.try_get("section_count")?,
        created_by: row.try_get("created_by")?,
        created_ms: row.try_get("created_ms")?,
        started_ms: row.try_get("started_ms")?,
        finished_ms: row.try_get("finished_ms")?,
    })
}

fn sched_from_row(row: &sqlx::postgres::PgRow) -> anyhow::Result<ReportSchedule> {
    Ok(ReportSchedule {
        id: row.try_get("id")?,
        definition_id: row.try_get("definition_id")?,
        definition_name: row.try_get("definition_name")?,
        frequency: Cadence::from_stored(row.try_get("frequency")?),
        day_of_week: row.try_get("day_of_week")?,
        day_of_month: row.try_get("day_of_month")?,
        at_hour: row.try_get("at_hour")?,
        at_minute: row.try_get("at_minute")?,
        enabled: row.try_get("enabled")?,
        next_run_ms: row.try_get("next_run_ms")?,
        last_run_ms: row.try_get("last_run_ms")?,
        last_status: row
            .try_get::<Option<String>, _>("last_status")?
            .as_deref()
            .map(ReportScheduleStatus::from_stored),
    })
}

/// Validated fields for creating/updating a schedule (parsed at the API edge).
#[derive(Debug, Clone)]
pub struct ScheduleInput {
    pub definition_id: Uuid,
    pub frequency: Cadence,
    pub day_of_week: Option<i16>,
    pub day_of_month: Option<i16>,
    pub at_hour: i16,
    pub at_minute: i16,
    pub enabled: bool,
}

/// PostgreSQL store for report definitions, schedules, and runs.
pub struct ReportsRepo {
    pool: PgPool,
}

impl ReportsRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    // — Definitions —

    pub async fn list_definitions(&self) -> anyhow::Result<Vec<ReportDefinition>> {
        let rows = sqlx::query(&format!(
            "SELECT {DEF_COLS} FROM report_definitions ORDER BY name"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(def_from_row).collect()
    }

    pub async fn get_definition(&self, id: Uuid) -> anyhow::Result<Option<ReportDefinition>> {
        let row = sqlx::query(&format!(
            "SELECT {DEF_COLS} FROM report_definitions WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(def_from_row).transpose()
    }

    pub async fn create_definition(
        &self,
        name: &str,
        description: Option<&str>,
        spec: &serde_json::Value,
        updated_by: Option<&str>,
    ) -> anyhow::Result<ReportDefinition> {
        let id = Uuid::new_v4();
        let row = sqlx::query(&format!(
            "INSERT INTO report_definitions (id, name, description, spec, updated_by) \
             VALUES ($1, $2, $3, $4, $5) RETURNING {DEF_COLS}"
        ))
        .bind(id)
        .bind(name)
        .bind(description)
        .bind(spec)
        .bind(updated_by)
        .fetch_one(&self.pool)
        .await?;
        def_from_row(&row)
    }

    pub async fn update_definition(
        &self,
        id: Uuid,
        name: &str,
        description: Option<&str>,
        spec: &serde_json::Value,
        updated_by: Option<&str>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE report_definitions SET name = $2, description = $3, spec = $4, \
             updated_by = $5, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(description)
        .bind(spec)
        .bind(updated_by)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn delete_definition(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM report_definitions WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    // — Schedules —

    pub async fn list_schedules(&self) -> anyhow::Result<Vec<ReportSchedule>> {
        let rows = sqlx::query(&format!(
            "SELECT {SCHED_COLS} FROM report_schedules s \
             JOIN report_definitions d ON d.id = s.definition_id ORDER BY s.next_run_at"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(sched_from_row).collect()
    }

    pub async fn create_schedule(
        &self,
        input: &ScheduleInput,
        next_run_at: DateTime<Utc>,
        updated_by: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO report_schedules \
             (id, definition_id, frequency, day_of_week, day_of_month, at_hour, at_minute, \
              enabled, next_run_at, updated_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(id)
        .bind(input.definition_id)
        .bind(input.frequency.as_str())
        .bind(input.day_of_week)
        .bind(input.day_of_month)
        .bind(input.at_hour)
        .bind(input.at_minute)
        .bind(input.enabled)
        .bind(next_run_at)
        .bind(updated_by)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn update_schedule(
        &self,
        id: Uuid,
        input: &ScheduleInput,
        next_run_at: DateTime<Utc>,
        updated_by: Option<&str>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE report_schedules SET definition_id = $2, frequency = $3, day_of_week = $4, \
             day_of_month = $5, at_hour = $6, at_minute = $7, enabled = $8, next_run_at = $9, \
             updated_by = $10, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(input.definition_id)
        .bind(input.frequency.as_str())
        .bind(input.day_of_week)
        .bind(input.day_of_month)
        .bind(input.at_hour)
        .bind(input.at_minute)
        .bind(input.enabled)
        .bind(next_run_at)
        .bind(updated_by)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn delete_schedule(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM report_schedules WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Enabled schedules whose `next_run_at` has passed (the scheduler's due-query).
    pub async fn due_schedules(&self) -> anyhow::Result<Vec<ReportSchedule>> {
        let rows = sqlx::query(&format!(
            "SELECT {SCHED_COLS} FROM report_schedules s \
             JOIN report_definitions d ON d.id = s.definition_id \
             WHERE s.enabled = true AND s.next_run_at <= now() ORDER BY s.next_run_at"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(sched_from_row).collect()
    }

    /// Record a fire: stamp `last_run_at`/`last_status` and advance `next_run_at`.
    pub async fn mark_fired(
        &self,
        id: Uuid,
        status: ReportScheduleStatus,
        next_run_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE report_schedules SET last_run_at = now(), last_status = $2, next_run_at = $3 \
             WHERE id = $1",
        )
        .bind(id)
        .bind(status.as_str())
        .bind(next_run_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // — Runs —

    pub async fn list_runs(
        &self,
        limit: i64,
        filter: &RunFilter,
    ) -> anyhow::Result<Vec<ReportRun>> {
        let rows = sqlx::query(&format!(
            "SELECT {RUN_COLS} FROM report_runs WHERE {RUN_FILTER_WHERE} \
             ORDER BY created_at DESC LIMIT $4"
        ))
        .bind(filter.definition_id)
        .bind(filter.state.map(|s| s.as_str().to_owned()))
        .bind(filter.since)
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(run_from_row).collect()
    }

    pub async fn get_run(&self, id: Uuid) -> anyhow::Result<Option<ReportRun>> {
        let row = sqlx::query(&format!("SELECT {RUN_COLS} FROM report_runs WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(run_from_row).transpose()
    }

    /// A run plus its rendered payloads (the viewer / export endpoints).
    pub async fn get_run_detail(&self, id: Uuid) -> anyhow::Result<Option<ReportRunDetail>> {
        let row = sqlx::query(&format!(
            "SELECT {RUN_COLS}, result_json, result_html FROM report_runs WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(row) => {
                let run = run_from_row(&row)?;
                Ok(Some(ReportRunDetail {
                    run,
                    result_json: row.try_get("result_json")?,
                    result_html: row.try_get("result_html")?,
                }))
            }
            None => Ok(None),
        }
    }

    pub async fn delete_run(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM report_runs WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Insert a run in the `running` state and return its row.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn insert_run(
        &self,
        definition_id: Option<Uuid>,
        name: &str,
        trigger: ReportRunTrigger,
        from_s: i64,
        to_s: i64,
        section_count: i32,
        spec_snapshot: &serde_json::Value,
        created_by: Option<&str>,
    ) -> anyhow::Result<ReportRun> {
        let id = Uuid::new_v4();
        let row = sqlx::query(&format!(
            "INSERT INTO report_runs \
             (id, definition_id, name, trigger, state, pct, range_from, range_to, \
              section_count, spec_snapshot, created_by, started_at) \
             VALUES ($1, $2, $3, $4, '{running}', 0, to_timestamp($5), to_timestamp($6), \
                     $7, $8, $9, now()) \
             RETURNING {RUN_COLS}",
            running = ReportRunState::Running.as_str(),
        ))
        .bind(id)
        .bind(definition_id)
        .bind(name)
        .bind(trigger.as_str())
        .bind(from_s)
        .bind(to_s)
        .bind(section_count)
        .bind(spec_snapshot)
        .bind(created_by)
        .fetch_one(&self.pool)
        .await?;
        run_from_row(&row)
    }

    pub(super) async fn set_run_progress(&self, id: Uuid, pct: i32) -> anyhow::Result<()> {
        sqlx::query(&format!(
            "UPDATE report_runs SET pct = $2 WHERE id = $1 AND state = '{}'",
            ReportRunState::Running.as_str()
        ))
        .bind(id)
        .bind(pct.clamp(0, 100))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub(super) async fn finish_run(
        &self,
        id: Uuid,
        result_json: &serde_json::Value,
        result_html: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(&format!(
            "UPDATE report_runs SET state = '{}', pct = 100, result_json = $2, \
             result_html = $3, finished_at = now() WHERE id = $1",
            ReportRunState::Succeeded.as_str()
        ))
        .bind(id)
        .bind(result_json)
        .bind(result_html)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub(super) async fn fail_run(&self, id: Uuid, error: &str) -> anyhow::Result<()> {
        sqlx::query(&format!(
            "UPDATE report_runs SET state = '{}', error = $2, finished_at = now() WHERE id = $1",
            ReportRunState::Failed.as_str()
        ))
        .bind(id)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// On startup, fail any run left `running`/`queued` by a previous process (it can't resume).
    pub async fn fail_orphans(&self) -> anyhow::Result<u64> {
        let res = sqlx::query(&format!(
            "UPDATE report_runs SET state = '{failed}', \
             error = 'core restarted while running', finished_at = now() \
             WHERE state IN ('{queued}', '{running}')",
            failed = ReportRunState::Failed.as_str(),
            queued = ReportRunState::Queued.as_str(),
            running = ReportRunState::Running.as_str(),
        ))
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Delete runs older than `older_than_secs` (retention). Returns rows removed.
    pub async fn prune_runs(&self, older_than_secs: i64) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "DELETE FROM report_runs WHERE created_at < now() - ($1::double precision * interval '1 second')",
        )
        .bind(older_than_secs as f64)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> serde_json::Value {
        serde_json::json!({ "sections": [{ "kind": "fleet_summary" }] })
    }

    fn schedule_input(definition_id: Uuid) -> ScheduleInput {
        ScheduleInput {
            definition_id,
            frequency: Cadence::Weekly,
            day_of_week: Some(1),
            day_of_month: None,
            at_hour: 6,
            at_minute: 30,
            enabled: true,
        }
    }

    /// A definition through its whole life, including the two audit columns.
    ///
    /// `updated_by` is written by both writers and read by the list, so it is the one field that
    /// would go unnoticed if a projection dropped it — the shape ADR-115 found in `repo/settings.rs`.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_definition_is_created_read_edited_and_deleted(pool: sqlx::PgPool) {
        let repo = ReportsRepo::new(pool.clone());
        let made = repo
            .create_definition(
                "weekly health",
                Some("every monday"),
                &spec(),
                Some("alice"),
            )
            .await
            .unwrap();
        assert_eq!(made.name, "weekly health");
        assert_eq!(made.description.as_deref(), Some("every monday"));
        assert_eq!(made.updated_by.as_deref(), Some("alice"));
        assert_eq!(made.spec, spec());

        let got = repo
            .get_definition(made.id)
            .await
            .unwrap()
            .expect("read back");
        assert_eq!(got.id, made.id);
        assert_eq!(got.created_ms, made.created_ms);
        assert_eq!(repo.list_definitions().await.unwrap().len(), 1);

        let edited = serde_json::json!({ "sections": [] });
        assert!(repo
            .update_definition(made.id, "weekly health v2", None, &edited, Some("bob"))
            .await
            .unwrap());
        let updated = repo
            .get_definition(made.id)
            .await
            .unwrap()
            .expect("read back");
        assert_eq!(updated.name, "weekly health v2");
        assert_eq!(updated.description, None);
        assert_eq!(updated.updated_by.as_deref(), Some("bob"));
        assert_eq!(updated.spec, edited);

        // A row that is not there is None / false, never an error: the API edge turns both into a
        // 404, so the distinction has to survive this far.
        assert!(repo.get_definition(Uuid::new_v4()).await.unwrap().is_none());
        assert!(!repo
            .update_definition(Uuid::new_v4(), "ghost", None, &edited, None)
            .await
            .unwrap());
        assert!(repo.delete_definition(made.id).await.unwrap());
        assert!(!repo.delete_definition(made.id).await.unwrap());
        assert!(repo.list_definitions().await.unwrap().is_empty());
    }

    /// A schedule's clock: what is due, and what `mark_fired` does to it.
    ///
    /// 🚨 The **positive** side is the one worth having. `due_schedules` returning nothing is also
    /// what a broken predicate returns, so a test that only checks "a future schedule is not due"
    /// passes against a query that is due for nothing, ever
    /// (`rejection-only-tests-pass-when-everything-rejects`).
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_schedule_becomes_due_and_marking_it_fired_moves_it_on(pool: sqlx::PgPool) {
        let repo = ReportsRepo::new(pool.clone());
        let def = repo
            .create_definition("weekly health", None, &spec(), None)
            .await
            .unwrap();
        let past = Utc::now() - chrono::Duration::hours(1);
        let future = Utc::now() + chrono::Duration::hours(1);

        let id = repo
            .create_schedule(&schedule_input(def.id), past, Some("alice"))
            .await
            .unwrap();
        let listed = repo.list_schedules().await.unwrap();
        assert_eq!(listed.len(), 1);
        let made = &listed[0];
        assert_eq!(made.id, id);
        assert_eq!(made.definition_id, def.id);
        assert_eq!(made.definition_name, "weekly health");
        assert_eq!(made.frequency, Cadence::Weekly);
        assert!(made.enabled);
        assert!(made.last_status.is_none());

        let due = repo.due_schedules().await.unwrap();
        assert_eq!(
            due.len(),
            1,
            "a schedule whose next run is in the past is due"
        );
        assert_eq!(due[0].id, id);

        repo.mark_fired(id, ReportScheduleStatus::Queued, future)
            .await
            .unwrap();
        assert!(
            repo.due_schedules().await.unwrap().is_empty(),
            "marking it fired moved the next run into the future"
        );
        let after = repo.list_schedules().await.unwrap();
        assert_eq!(after[0].last_status, Some(ReportScheduleStatus::Queued));
        assert!(after[0].last_run_ms.is_some());

        // Disabled is not the same as not due: the predicate must exclude it even in the past.
        let mut off = schedule_input(def.id);
        off.enabled = false;
        assert!(repo
            .update_schedule(id, &off, past, Some("bob"))
            .await
            .unwrap());
        assert!(repo.due_schedules().await.unwrap().is_empty());

        assert!(!repo
            .update_schedule(Uuid::new_v4(), &off, past, None)
            .await
            .unwrap());
        assert!(repo.delete_schedule(id).await.unwrap());
        assert!(!repo.delete_schedule(id).await.unwrap());
    }

    /// A run from insert to finish, read back through all three readers, and filtered three ways.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_run_is_inserted_progressed_finished_and_read_back(pool: sqlx::PgPool) {
        let repo = ReportsRepo::new(pool.clone());
        let def = repo
            .create_definition("weekly health", None, &spec(), None)
            .await
            .unwrap();

        let run = repo
            .insert_run(
                Some(def.id),
                "weekly health",
                ReportRunTrigger::Manual,
                1_760_000_000,
                1_760_086_400,
                3,
                &spec(),
                Some("alice"),
            )
            .await
            .unwrap();
        assert_eq!(run.state, ReportRunState::Running);
        assert_eq!(run.pct, 0);
        assert_eq!(run.section_count, 3);
        assert_eq!(run.created_by.as_deref(), Some("alice"));
        assert_eq!(run.trigger, ReportRunTrigger::Manual);
        assert!(run.started_ms.is_some());
        assert!(run.finished_ms.is_none());

        repo.set_run_progress(run.id, 40).await.unwrap();
        assert_eq!(repo.get_run(run.id).await.unwrap().unwrap().pct, 40);

        let html = "<h1>weekly health</h1>";
        repo.finish_run(run.id, &spec(), html).await.unwrap();
        let done = repo.get_run(run.id).await.unwrap().unwrap();
        assert_eq!(done.state, ReportRunState::Succeeded);
        assert_eq!(done.pct, 100);
        assert!(done.finished_ms.is_some());

        // Progress only moves a *running* run, so a finished one is not walked backwards by a
        // late tick from its own task.
        repo.set_run_progress(run.id, 10).await.unwrap();
        assert_eq!(repo.get_run(run.id).await.unwrap().unwrap().pct, 100);

        let detail = repo.get_run_detail(run.id).await.unwrap().expect("detail");
        assert_eq!(detail.run.id, run.id);
        assert_eq!(detail.result_json.as_ref(), Some(&spec()));
        assert_eq!(detail.result_html.as_deref(), Some(html));
        assert!(repo.get_run(Uuid::new_v4()).await.unwrap().is_none());
        assert!(repo.get_run_detail(Uuid::new_v4()).await.unwrap().is_none());

        let failed = repo
            .insert_run(
                None,
                "ad hoc",
                ReportRunTrigger::Scheduled,
                1_760_000_000,
                1_760_086_400,
                1,
                &spec(),
                None,
            )
            .await
            .unwrap();
        repo.fail_run(failed.id, "the metric store said no")
            .await
            .unwrap();
        let failed_row = repo.get_run(failed.id).await.unwrap().unwrap();
        assert_eq!(failed_row.state, ReportRunState::Failed);
        assert_eq!(
            failed_row.error.as_deref(),
            Some("the metric store said no")
        );

        // Every clause of RUN_FILTER_WHERE is always present and bound; each one on its own must
        // narrow, and an empty filter must not.
        let all = repo.list_runs(50, &RunFilter::default()).await.unwrap();
        assert_eq!(all.len(), 2);
        let by_def = repo
            .list_runs(
                50,
                &RunFilter {
                    definition_id: Some(def.id),
                    ..RunFilter::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(by_def.len(), 1);
        assert_eq!(by_def[0].id, run.id);
        let by_state = repo
            .list_runs(
                50,
                &RunFilter {
                    state: Some(ReportRunState::Failed),
                    ..RunFilter::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(by_state.len(), 1);
        assert_eq!(by_state[0].id, failed.id);
        let future = repo
            .list_runs(
                50,
                &RunFilter {
                    since: Some(Utc::now() + chrono::Duration::hours(1)),
                    ..RunFilter::default()
                },
            )
            .await
            .unwrap();
        assert!(future.is_empty());

        assert!(repo.delete_run(run.id).await.unwrap());
        assert!(!repo.delete_run(run.id).await.unwrap());
        assert_eq!(
            repo.list_runs(50, &RunFilter::default())
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// The two janitors: a run left `running` by a core that died, and runs older than the window.
    ///
    /// 🚨 Both directions on both, because each one deletes or rewrites rows. A janitor that takes
    /// everything satisfies "the stale row is gone" perfectly.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_janitors_take_the_stale_rows_and_leave_the_rest(pool: sqlx::PgPool) {
        let repo = ReportsRepo::new(pool.clone());
        let stale = repo
            .insert_run(
                None,
                "interrupted",
                ReportRunTrigger::Scheduled,
                1_760_000_000,
                1_760_086_400,
                1,
                &spec(),
                None,
            )
            .await
            .unwrap();
        let live = repo
            .insert_run(
                None,
                "in flight",
                ReportRunTrigger::Manual,
                1_760_000_000,
                1_760_086_400,
                1,
                &spec(),
                None,
            )
            .await
            .unwrap();

        // 🔧 **Measured, not assumed.** There is no age window: the sweep runs at startup, when a
        // live run cannot exist by definition, so every queued/running row is an orphan. The
        // fixture therefore separates the two states rather than two ages.
        repo.finish_run(live.id, &spec(), "<p>done</p>")
            .await
            .unwrap();

        assert_eq!(repo.fail_orphans().await.unwrap(), 1);
        assert_eq!(
            repo.get_run(stale.id).await.unwrap().unwrap().state,
            ReportRunState::Failed
        );
        assert_eq!(
            repo.get_run(live.id).await.unwrap().unwrap().state,
            ReportRunState::Succeeded,
            "a run that had already finished is not rewritten"
        );
        // Idempotent: the second sweep finds nothing, because the first one left it `failed`.
        assert_eq!(repo.fail_orphans().await.unwrap(), 0);

        sqlx::query("UPDATE report_runs SET created_at = now() - interval '40 days' WHERE id = $1")
            .bind(stale.id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(repo.prune_runs(60 * 86_400).await.unwrap(), 0);
        assert_eq!(repo.prune_runs(30 * 86_400).await.unwrap(), 1);
        assert_eq!(
            repo.list_runs(50, &RunFilter::default())
                .await
                .unwrap()
                .len(),
            1
        );
    }
}
