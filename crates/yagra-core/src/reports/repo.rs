// SPDX-License-Identifier: AGPL-3.0-only
//! **Where reports are kept** — definitions, schedules and runs in PostgreSQL (ADR-004).
//!
//! 🚨 **The only file in this module allowed to name a table**, and \`super::guards\` enforces it
//! both ways. Report generation reads stores through their own repositories; a \`sqlx::query\` in
//! [\`super::runner\`] or [\`super::sections\`] would be the store-separation rule breaking in the
//! place nobody looks.
//!
//! ⚠️ **No test in this module builds a \`ReportsRepo\`.** It needs a live database, and the seams
//! that would let it be faked are a separate job (ADR-102 decision 5) — the gate on these 399 lines
//! is generating a report on a real deployment.

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
