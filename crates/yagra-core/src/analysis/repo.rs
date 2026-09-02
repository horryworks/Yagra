// SPDX-License-Identifier: AGPL-3.0-only
//! Job and finding persistence — PostgreSQL (ADR-004, ADR-089).
//!
//! Job metadata, findings and schedules are metadata, so they live in PostgreSQL rather than the
//! TSDB. This file holds the statements and the row mappers; what to *run* is [`super::runner`]'s
//! side of the module.
//!
//! ⚠️ `analysis_jobs.state` is written by statement literals rather than by a bind, so every
//! statement interpolates [`AnalysisJobState`]'s token instead of spelling it — pinned by
//! `guards::the_job_state_sql_is_built_from_the_enum`, which reads this file as text.

use super::*;

/// Columns selected for a job row (timestamps projected to epoch-millis).
/// What narrows the runs list. Every field optional; all of them ANDed.
///
/// A struct rather than three `Option` parameters, because the order of three optionals is exactly
/// the call-site mistake that compiles, runs, and answers a different question.
#[derive(Debug, Default, Clone, Copy)]
pub struct JobFilter<'a> {
    /// Only runs of this tool. Validated against [`AnalysisTool`] at the API edge.
    pub tool: Option<&'a str>,
    /// Only runs in this state. Typed, so the vocabulary the filter accepts is the vocabulary the
    /// writers produce — there is no second list to keep in step.
    pub state: Option<AnalysisJobState>,
    /// Only runs started at or after this instant.
    pub since: Option<DateTime<Utc>>,
}

/// The runs filter's predicate: one const, every clause always present, every value a nullable
/// bind. Not assembled conditionally — a `WHERE` built by pushing clauses has a branch per filter
/// that can be forgotten, and a forgotten one fails open, showing runs the operator did not ask for.
pub(super) const JOB_FILTER_WHERE: &str = "($1::text IS NULL OR tool = $1) \
     AND ($2::text IS NULL OR state = $2) \
     AND ($3::timestamptz IS NULL OR created_at >= $3)";

pub(super) const JOB_COLS: &str =
    "id, tool, scope_kind, scope_id, scope_label, params, state, pct, phase, \
     finding_count, summary, error, \
     (EXTRACT(EPOCH FROM created_at) * 1000)::bigint AS created_ms, \
     (EXTRACT(EPOCH FROM started_at) * 1000)::bigint AS started_ms, \
     (EXTRACT(EPOCH FROM finished_at) * 1000)::bigint AS finished_ms";

/// The `WHERE` of the cross-run findings search — one always-present clause per filter, `NULL`
/// meaning "no filter".
///
/// Written this way rather than appended per filter because of `$7`, the caller's group scope: a
/// conditionally-added restriction has a branch that can be forgotten, and forgetting *that* one
/// fails **open**, returning the whole fleet's findings. `NodeRepo::SCOPE_PREDICATE` and
/// `EVENT_FILTER_WHERE` are the same shape for the same reason.
///
/// ⚠️ `$7` and `$8` look alike and are not alike. `$7` is what the caller **may** see and is never
/// optional; `$8` is the group they **asked** to narrow to, and is dropped when they don't. Binding
/// a request into `$7` would let a caller widen their own scope by omitting a query parameter.
///
/// A finding with no `node_id` (the flow-tier-off notice, a fleet-level summary row) matches
/// neither group clause — `NULL IN (…)` is never true — so it is visible only to a caller with no
/// group restriction at all. That is the same rule the per-job endpoint applies in Rust, and the
/// same one `Scope::allows` applies to an ungrouped node.
pub(super) const FINDING_SEARCH_WHERE: &str = "\
     ($1::timestamptz IS NULL OR (f.created_at, f.id) < \
        ($1, coalesce($2::uuid, '00000000-0000-0000-0000-000000000000'::uuid))) \
     AND ($3::timestamptz IS NULL OR f.created_at >= $3) \
     AND ($4::text[] IS NULL OR j.tool = ANY($4)) \
     AND ($5::text[] IS NULL OR f.severity = ANY($5)) \
     AND ($6::uuid IS NULL OR f.node_id = $6) \
     AND ($7::uuid[] IS NULL OR f.node_id IN (SELECT id FROM nodes WHERE group_id = ANY($7))) \
     AND ($8::uuid[] IS NULL OR f.node_id IN (SELECT id FROM nodes WHERE group_id = ANY($8))) \
     AND ($9::text IS NULL \
          OR (f.metric ILIKE '%' || $9 || '%' OR f.kind ILIKE '%' || $9 || '%')) \
     AND ($10::text IS NULL \
          OR f.node_id IN (SELECT id FROM nodes WHERE name ILIKE '%' || $10 || '%')) \
     AND ($11::double precision IS NULL OR f.score >= $11) \
     AND ($12::double precision IS NULL OR f.score <= $12)";

/// The cross-run findings query. `ORDER BY` matches the cursor in [`FINDING_SEARCH_WHERE`] column
/// for column, and both match `analysis_findings_created_idx` (migration 0058) — if those three
/// ever disagree the paging silently drops rows, which is why a test pins them together.
pub(super) fn finding_search_sql() -> String {
    format!(
        "SELECT f.id, f.job_id, j.tool, f.score, f.severity, f.node_id, f.node_name, \
         f.metric, f.kind, f.when_label, f.duration, f.created_at \
         FROM analysis_findings f JOIN analysis_jobs j ON j.id = f.job_id \
         WHERE {FINDING_SEARCH_WHERE} \
         ORDER BY f.created_at DESC, f.id DESC LIMIT ${}",
        FINDING_SEARCH_BINDS + 1
    )
}

/// How many placeholders [`FINDING_SEARCH_WHERE`] uses. The page size is the one *after* them.
///
/// Derived rather than written twice, for the reason `EVENT_FILTER_BINDS` records: renumbering by
/// hand after widening the predicate is neither a compile error nor a crash — the page size lands in
/// a filter's slot and the query answers a different question. Here that would be `LIMIT` binding
/// into `max_score`, i.e. "findings scoring at most 100" returned unpaged.
pub(super) const FINDING_SEARCH_BINDS: usize = 12;

/// Columns selected for a schedule row (timestamps projected to epoch-millis, as the job rows are).
pub(super) const SCHED_COLS: &str =
    "id, tool, scope_kind, scope_id, scope_label, params, frequency, \
     day_of_week, day_of_month, at_hour, at_minute, enabled, last_status, \
     (EXTRACT(EPOCH FROM next_run_at) * 1000)::bigint AS next_run_ms, \
     (EXTRACT(EPOCH FROM last_run_at) * 1000)::bigint AS last_run_ms";

pub(super) fn sched_from_row(row: &sqlx::postgres::PgRow) -> anyhow::Result<AnalysisSchedule> {
    Ok(AnalysisSchedule {
        id: row.try_get("id")?,
        tool: row.try_get("tool")?,
        scope_kind: row.try_get("scope_kind")?,
        scope_id: row.try_get("scope_id")?,
        scope_label: row.try_get("scope_label")?,
        params: row.try_get("params")?,
        frequency: crate::cadence::Cadence::from_stored(row.try_get("frequency")?),
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
            .map(AnalysisScheduleStatus::from_stored),
    })
}

pub(super) fn job_from_row(row: &sqlx::postgres::PgRow) -> anyhow::Result<AnalysisJob> {
    Ok(AnalysisJob {
        id: row.try_get("id")?,
        tool: row.try_get("tool")?,
        scope_kind: row.try_get("scope_kind")?,
        scope_id: row.try_get("scope_id")?,
        scope_label: row.try_get("scope_label")?,
        params: row.try_get("params")?,
        state: AnalysisJobState::from_stored(row.try_get::<String, _>("state")?.as_str()),
        pct: row.try_get("pct")?,
        phase: row.try_get("phase")?,
        finding_count: row.try_get("finding_count")?,
        summary: row.try_get("summary")?,
        error: row.try_get("error")?,
        created_ms: row.try_get("created_ms")?,
        started_ms: row.try_get("started_ms")?,
        finished_ms: row.try_get("finished_ms")?,
    })
}

/// PostgreSQL-backed store for analysis jobs and their findings.
pub struct AnalysisRepo {
    pool: PgPool,
}

impl AnalysisRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Insert a new job in the `running` state (started_at = now) and return its row.
    pub async fn insert(
        &self,
        params: &JobParams,
        created_by: Option<&str>,
    ) -> anyhow::Result<AnalysisJob> {
        let id = Uuid::new_v4();
        let row = sqlx::query(&format!(
            "INSERT INTO analysis_jobs \
             (id, tool, scope_kind, scope_id, scope_label, params, state, pct, phase, \
              finding_count, created_by, started_at) \
             VALUES ($1, $2, $3, $4, $5, $6, '{}', 0, $7, 0, $8, now()) \
             RETURNING {JOB_COLS}",
            AnalysisJobState::Running.as_str()
        ))
        .bind(id)
        .bind(params.tool.as_str())
        .bind(params.scope_kind.as_str())
        .bind(params.scope_id)
        .bind(&params.scope_label)
        .bind(params.to_json())
        .bind("Queued — fetching history…")
        .bind(created_by)
        .fetch_one(&self.pool)
        .await?;
        job_from_row(&row)
    }

    /// Update progress (percent + phase caption) of a running job.
    pub async fn set_progress(&self, id: Uuid, pct: i32, phase: &str) -> anyhow::Result<()> {
        sqlx::query(&format!(
            "UPDATE analysis_jobs SET pct = $2, phase = $3 WHERE id = $1 AND state = '{}'",
            AnalysisJobState::Running.as_str()
        ))
        .bind(id)
        .bind(pct.clamp(0, 100))
        .bind(phase)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark a job done with its result summary (findings inserted separately).
    pub async fn finish(&self, id: Uuid, finding_count: i32, summary: &str) -> anyhow::Result<()> {
        sqlx::query(&format!(
            "UPDATE analysis_jobs SET state = '{}', pct = 100, phase = NULL, \
             finding_count = $2, summary = $3, finished_at = now() WHERE id = $1",
            AnalysisJobState::Done.as_str()
        ))
        .bind(id)
        .bind(finding_count)
        .bind(summary)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark a job failed with a reason.
    pub async fn fail(&self, id: Uuid, error: &str) -> anyhow::Result<()> {
        sqlx::query(&format!(
            "UPDATE analysis_jobs SET state = '{}', phase = NULL, error = $2, \
             finished_at = now() WHERE id = $1",
            AnalysisJobState::Failed.as_str()
        ))
        .bind(id)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Mark a job cancelled (set by the runner when its cancel flag was tripped).
    pub async fn mark_cancelled(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query(&format!(
            "UPDATE analysis_jobs SET state = '{}', phase = NULL, finished_at = now() \
             WHERE id = $1 AND state = '{}'",
            AnalysisJobState::Cancelled.as_str(),
            AnalysisJobState::Running.as_str()
        ))
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Drop analysis runs older than `retention_secs`, and with them their findings.
    ///
    /// `retention::Subject::AnalysisRuns`. Migration 0026 shipped this table with "no auto-trim
    /// yet" written into it, and 0059 later added *scheduled* analyses — so the table nothing
    /// pruned became one that fills on a cadence. `analysis_findings` needs no statement of its
    /// own: it is `ON DELETE CASCADE` from here, which is also why the cascade never fired before.
    pub async fn prune_jobs(&self, retention_secs: i64) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "DELETE FROM analysis_jobs WHERE created_at < now() - make_interval(secs => $1)",
        )
        .bind(retention_secs as f64)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Insert a batch of findings for a job.
    pub(super) async fn insert_findings(
        &self,
        job_id: Uuid,
        findings: &[NewFinding],
    ) -> anyhow::Result<()> {
        for f in findings {
            sqlx::query(
                "INSERT INTO analysis_findings \
                 (id, job_id, score, severity, node_id, node_name, metric, kind, when_label, duration, detail) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            )
            .bind(Uuid::new_v4())
            .bind(job_id)
            .bind(f.score)
            .bind(&f.severity)
            .bind(f.node_id)
            .bind(&f.node_name)
            .bind(&f.metric)
            .bind(&f.kind)
            .bind(&f.when_label)
            .bind(&f.duration)
            .bind(&f.detail)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    /// Recent jobs, newest first (the runs list). `limit` clamped by the caller.
    pub async fn list(
        &self,
        limit: i64,
        filter: &JobFilter<'_>,
    ) -> anyhow::Result<Vec<AnalysisJob>> {
        let rows = sqlx::query(&format!(
            "SELECT {JOB_COLS} FROM analysis_jobs WHERE {JOB_FILTER_WHERE} \
             ORDER BY created_at DESC LIMIT $4"
        ))
        .bind(filter.tool.map(str::to_owned))
        .bind(filter.state.map(|s| s.as_str().to_owned()))
        .bind(filter.since)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(job_from_row).collect()
    }

    /// One job by id.
    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<AnalysisJob>> {
        let row = sqlx::query(&format!(
            "SELECT {JOB_COLS} FROM analysis_jobs WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(job_from_row).transpose()
    }

    /// A job's findings, highest score first.
    pub async fn findings(&self, job_id: Uuid) -> anyhow::Result<Vec<AnalysisFinding>> {
        let rows = sqlx::query(
            "SELECT id, score, severity, node_id, node_name, metric, kind, when_label, duration, detail \
             FROM analysis_findings WHERE job_id = $1 ORDER BY score DESC",
        )
        .bind(job_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(AnalysisFinding {
                    id: row.try_get("id")?,
                    score: row.try_get("score")?,
                    severity: row.try_get("severity")?,
                    node_id: row.try_get("node_id")?,
                    node_name: row.try_get("node_name")?,
                    metric: row.try_get("metric")?,
                    kind: row.try_get("kind")?,
                    when_label: row.try_get("when_label")?,
                    duration: row.try_get("duration")?,
                    detail: row.try_get("detail")?,
                })
            })
            .collect()
    }

    /// Findings across **every** run, newest first — the Saved-findings search.
    ///
    /// The join to `analysis_jobs` is what makes `?tool=` possible at all: a finding row records
    /// what was found, never which diagnostic found it.
    pub async fn search_findings(
        &self,
        q: &FindingSearch<'_>,
    ) -> anyhow::Result<Vec<SavedFinding>> {
        // An empty set means *unfiltered*, which is a NULL bind — an empty array would make
        // `= ANY(…)` match nothing and turn "no filter" into "no results".
        fn set<'s>(tokens: impl Iterator<Item = &'s str>) -> Option<Vec<String>> {
            let v: Vec<String> = tokens.map(str::to_owned).collect();
            (!v.is_empty()).then_some(v)
        }
        let rows = sqlx::query(&finding_search_sql())
            .bind(q.before)
            .bind(q.before_id)
            .bind(q.since)
            .bind(set(q.tool.iter().map(|t| t.as_str())))
            .bind(set(q.severity.iter().copied()))
            .bind(q.node_id)
            .bind(q.groups.map(<[Uuid]>::to_vec))
            .bind(q.in_group.map(<[Uuid]>::to_vec))
            .bind(q.q)
            .bind(q.node_q)
            .bind(q.min_score)
            .bind(q.max_score)
            .bind(q.limit)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                let at: DateTime<Utc> = row.try_get("created_at")?;
                Ok(SavedFinding {
                    id: row.try_get("id")?,
                    job_id: row.try_get("job_id")?,
                    tool: row.try_get("tool")?,
                    score: row.try_get("score")?,
                    severity: row.try_get("severity")?,
                    node_id: row.try_get("node_id")?,
                    node_name: row.try_get("node_name")?,
                    metric: row.try_get("metric")?,
                    kind: row.try_get("kind")?,
                    when_label: row.try_get("when_label")?,
                    duration: row.try_get("duration")?,
                    at: at.to_rfc3339(),
                })
            })
            .collect()
    }

    // — Schedules —

    /// Every schedule, soonest first.
    pub async fn list_schedules(&self) -> anyhow::Result<Vec<AnalysisSchedule>> {
        let rows = sqlx::query(&format!(
            "SELECT {SCHED_COLS} FROM analysis_schedules ORDER BY next_run_at"
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
            "INSERT INTO analysis_schedules \
             (id, tool, scope_kind, scope_id, scope_label, params, frequency, day_of_week, \
              day_of_month, at_hour, at_minute, enabled, next_run_at, updated_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        )
        .bind(id)
        .bind(input.params.tool.as_str())
        .bind(input.params.scope_kind.as_str())
        .bind(input.params.scope_id)
        .bind(&input.params.scope_label)
        .bind(input.params.to_json())
        .bind(input.cadence.frequency.as_str())
        .bind(input.cadence.day_of_week)
        .bind(input.cadence.day_of_month)
        .bind(input.cadence.at_hour)
        .bind(input.cadence.at_minute)
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
            "UPDATE analysis_schedules SET tool = $2, scope_kind = $3, scope_id = $4, \
             scope_label = $5, params = $6, frequency = $7, day_of_week = $8, day_of_month = $9, \
             at_hour = $10, at_minute = $11, enabled = $12, next_run_at = $13, updated_by = $14, \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(input.params.tool.as_str())
        .bind(input.params.scope_kind.as_str())
        .bind(input.params.scope_id)
        .bind(&input.params.scope_label)
        .bind(input.params.to_json())
        .bind(input.cadence.frequency.as_str())
        .bind(input.cadence.day_of_week)
        .bind(input.cadence.day_of_month)
        .bind(input.cadence.at_hour)
        .bind(input.cadence.at_minute)
        .bind(input.enabled)
        .bind(next_run_at)
        .bind(updated_by)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// One schedule by id — the read the API edge does before letting a scoped caller edit it.
    pub async fn get_schedule(&self, id: Uuid) -> anyhow::Result<Option<AnalysisSchedule>> {
        let row = sqlx::query(&format!(
            "SELECT {SCHED_COLS} FROM analysis_schedules WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(sched_from_row).transpose()
    }

    pub async fn delete_schedule(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM analysis_schedules WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Enabled schedules whose `next_run_at` has passed (the scheduler's due-query).
    pub async fn due_schedules(&self) -> anyhow::Result<Vec<AnalysisSchedule>> {
        let rows = sqlx::query(&format!(
            "SELECT {SCHED_COLS} FROM analysis_schedules \
             WHERE enabled = true AND next_run_at <= now() ORDER BY next_run_at"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(sched_from_row).collect()
    }

    /// Record a fire that produced a run: stamp `last_run_at`/`last_status` and advance to `next`.
    pub async fn mark_fired(
        &self,
        id: Uuid,
        status: AnalysisScheduleStatus,
        next: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE analysis_schedules SET last_run_at = now(), last_status = $2, next_run_at = $3 \
             WHERE id = $1",
        )
        .bind(id)
        .bind(status.as_str())
        .bind(next)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record an attempt admission control refused: **leave `next_run_at` where it is** so the
    /// schedule stays due and the next tick retries.
    ///
    /// `last_run_at` is deliberately not stamped either — nothing ran, and a schedule reporting a
    /// last run that produced no row is the confusing half of this failure mode. The status alone
    /// says what happened.
    pub async fn mark_deferred(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query("UPDATE analysis_schedules SET last_status = $2 WHERE id = $1")
            .bind(id)
            .bind(AnalysisScheduleStatus::Busy.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// On startup, fail any job left `running` by a previous core process (it can't resume).
    pub async fn fail_orphans(&self) -> anyhow::Result<u64> {
        let res = sqlx::query(&format!(
            "UPDATE analysis_jobs SET state = '{}', phase = NULL, \
             error = 'core restarted while running', finished_at = now() WHERE state = '{}'",
            AnalysisJobState::Failed.as_str(),
            AnalysisJobState::Running.as_str()
        ))
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }
}

// ── Runner ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_findings_search_orders_on_exactly_the_columns_its_cursor_pages_on() {
        // The three that must agree or paging silently drops rows: the cursor predicate, the
        // ORDER BY, and the index in migration 0058. The events list has its own version of this
        // test because the same disagreement shipped there once.
        let sql = finding_search_sql();
        // The page size is derived from `FINDING_SEARCH_BINDS`, so this asserts the derivation is
        // right rather than re-hardcoding the number the derivation exists to stop anyone writing.
        assert!(
            sql.contains("ORDER BY f.created_at DESC, f.id DESC LIMIT $13"),
            "{sql}"
        );
        assert_eq!(FINDING_SEARCH_BINDS, 12);
        assert!(sql.contains(FINDING_SEARCH_WHERE), "{sql}");
        assert!(
            FINDING_SEARCH_WHERE.contains("(f.created_at, f.id) <"),
            "the cursor must be the row value, not the timestamp alone: {FINDING_SEARCH_WHERE}"
        );
        // Findings carry no tool of their own; `?tool=` is only answerable through the run.
        assert!(
            sql.contains("JOIN analysis_jobs j ON j.id = f.job_id"),
            "{sql}"
        );
    }

    #[test]
    fn the_findings_search_restricts_by_scope_unconditionally() {
        // The inversion that would be a privilege escalation: the caller's scope must be a clause
        // that is always in the statement, with NULL — not absence — meaning unrestricted. It must
        // also be bound, never interpolated (security.md).
        assert!(FINDING_SEARCH_WHERE.contains(
            "($7::uuid[] IS NULL OR f.node_id IN (SELECT id FROM nodes WHERE group_id = ANY($7)))"
        ));
        // …and the group the caller *asked* for is a separate bind, so dropping the request cannot
        // drop the restriction.
        assert!(FINDING_SEARCH_WHERE.contains("ANY($8)"));
        // Two kinds of quoted literal are allowed here and nothing else: the cursor's nil-uuid
        // floor, and the `'%'` wildcards the substring filters concatenate around a *bound* value.
        // Anything left after removing those means a request value reached SQL as text.
        let without_wildcards = FINDING_SEARCH_WHERE.replace("'%'", "");
        assert_eq!(
            without_wildcards.matches('\'').count(),
            2,
            "the nil-uuid cursor floor is the only non-wildcard literal that belongs here: \
             {FINDING_SEARCH_WHERE}"
        );
        // …and each wildcard sits beside a placeholder, never beside inlined text.
        for bind in ["$9", "$10"] {
            assert!(
                FINDING_SEARCH_WHERE.contains(&format!("'%' || {bind} || '%'")),
                "{bind} must be concatenated as a bound value: {FINDING_SEARCH_WHERE}"
            );
        }
    }

    #[test]
    fn the_score_bounds_are_inclusive_and_every_placeholder_is_used_once() {
        // Inclusive at both ends. `>` instead of `>=` is the version of this that looks right and
        // drops exactly the rows sitting on the bound the operator typed — invisible unless you go
        // looking, because the answer is still plausible.
        assert!(
            FINDING_SEARCH_WHERE.contains("($11::double precision IS NULL OR f.score >= $11)"),
            "{FINDING_SEARCH_WHERE}"
        );
        assert!(
            FINDING_SEARCH_WHERE.contains("($12::double precision IS NULL OR f.score <= $12)"),
            "{FINDING_SEARCH_WHERE}"
        );
        // Every placeholder the predicate declares is actually written, and none beyond it — this is
        // what makes `FINDING_SEARCH_BINDS` a fact about the string rather than a hopeful constant.
        // (`search_findings` binds them in order, so a gap here would silently shift every later
        // filter's value into the wrong clause.)
        for i in 1..=FINDING_SEARCH_BINDS {
            assert!(
                FINDING_SEARCH_WHERE.contains(&format!("${i}")),
                "${i} is declared by FINDING_SEARCH_BINDS but never used: {FINDING_SEARCH_WHERE}"
            );
        }
        assert!(
            !FINDING_SEARCH_WHERE.contains(&format!("${}", FINDING_SEARCH_BINDS + 1)),
            "the predicate uses the slot reserved for LIMIT: {FINDING_SEARCH_WHERE}"
        );
    }

    // ── Database tests (ADR-114) ───────────────────────────────────────────────────────
    //
    // The five statements that decide what the runs list says a run is doing. Two of them carry a
    // state guard, so each is exercised in **both** directions: a test that only shows the guard
    // letting the intended write through is satisfied just as well by a statement with no guard at
    // all, and the guard is the whole point of those two.

    /// Run parameters for a job, so no test spells the ten fields out.
    ///
    /// A value constructor, not a schema copy — every field goes through the production writer.
    fn a_job(tool: AnalysisTool) -> JobParams {
        JobParams {
            tool,
            scope_kind: ScopeKind::All,
            scope_id: None,
            scope_label: "Whole fleet".into(),
            window_secs: 3_600,
            baseline_secs: 86_400,
            sensitivity: 3.0,
            depth: "standard".into(),
            family: "all".into(),
            notify: false,
        }
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_inserted_run_comes_back_running_with_the_values_it_was_given(pool: sqlx::PgPool) {
        let repo = AnalysisRepo::new(pool.clone());
        let job = repo
            .insert(&a_job(AnalysisTool::Capacity), Some("horry"))
            .await
            .expect("insert");

        // `state` is interpolated from the enum rather than bound, so this is the one place that
        // shows the token lands *in* the column rather than beside it.
        assert_eq!(job.state, AnalysisJobState::Running);
        assert_eq!(job.tool, "capacity");
        assert_eq!(job.scope_kind, "all");
        assert_eq!(job.scope_id, None);
        assert_eq!(job.scope_label, "Whole fleet");
        assert_eq!(job.pct, 0);
        assert_eq!(job.finding_count, 0);
        // `$7` is the phase caption and `$8` the username who launched it. Swapping the two
        // compiles, inserts, and puts the operator's name where the progress caption goes.
        assert_eq!(job.phase.as_deref(), Some("Queued — fetching history…"));
        assert_eq!(job.params["window_secs"], 3_600);
        assert_eq!(job.params["depth"], "standard");
        assert_eq!(job.params["notify"], false);
        // `started_at = now()` is in the INSERT; the other two are what the epoch-ms projections
        // do with a column that is still NULL.
        assert!(job.started_ms.is_some());
        assert!(job.finished_ms.is_none());
        assert!(job.created_ms > 0);

        // …and it reads back the same through `get`, which selects the same columns separately.
        let read = repo
            .get(job.id)
            .await
            .expect("get")
            .expect("the row just inserted");
        assert_eq!(read.id, job.id);
        assert_eq!(read.state, AnalysisJobState::Running);
        assert_eq!(read.phase, job.phase);
        assert_eq!(read.params, job.params);
        assert!(repo.get(Uuid::new_v4()).await.expect("get").is_none());
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn progress_moves_a_running_run_and_leaves_a_finished_one_alone(pool: sqlx::PgPool) {
        let repo = AnalysisRepo::new(pool.clone());
        let job = repo
            .insert(&a_job(AnalysisTool::Anomaly), None)
            .await
            .expect("insert");

        repo.set_progress(job.id, 40, "Scanning 120 nodes…")
            .await
            .expect("progress");
        let running = repo.get(job.id).await.expect("get").expect("row");
        assert_eq!(running.pct, 40);
        assert_eq!(running.phase.as_deref(), Some("Scanning 120 nodes…"));

        // Out of range is clamped before it reaches the column, so the runs list can draw the bar
        // without checking it first.
        repo.set_progress(job.id, 250, "Still scanning…")
            .await
            .expect("progress");
        assert_eq!(repo.get(job.id).await.expect("get").expect("row").pct, 100);

        // The other direction of `AND state = 'running'`. The runner's last progress message can
        // arrive after its own completion; without the guard it would pull `pct` back off 100 and
        // put a caption back on a run that has already reported its findings.
        repo.finish(job.id, 7, "7 anomalies · 3 nodes")
            .await
            .expect("finish");
        repo.set_progress(job.id, 55, "Scanning…")
            .await
            .expect("progress");
        let done = repo.get(job.id).await.expect("get").expect("row");
        assert_eq!(done.state, AnalysisJobState::Done);
        assert_eq!(done.pct, 100);
        assert_eq!(done.phase, None);
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn finishing_clears_the_caption_and_records_what_the_run_produced(pool: sqlx::PgPool) {
        let repo = AnalysisRepo::new(pool.clone());
        let job = repo
            .insert(&a_job(AnalysisTool::Flap), None)
            .await
            .expect("insert");
        repo.set_progress(job.id, 60, "Scoring…")
            .await
            .expect("progress");

        repo.finish(job.id, 23, "23 flaps · 8 nodes")
            .await
            .expect("finish");
        let done = repo.get(job.id).await.expect("get").expect("row");
        assert_eq!(done.state, AnalysisJobState::Done);
        assert_eq!(done.pct, 100);
        // Cleared, not left holding the last phase it reached: a finished run still captioned
        // "Scoring…" reads as one that is still working.
        assert_eq!(done.phase, None);
        assert_eq!(done.finding_count, 23);
        assert_eq!(done.summary.as_deref(), Some("23 flaps · 8 nodes"));
        assert_eq!(done.error, None);
        assert!(done.finished_ms.is_some());
        assert!(done.finished_ms >= done.started_ms);
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn failing_keeps_the_progress_the_run_reached_and_says_why_it_stopped(
        pool: sqlx::PgPool,
    ) {
        let repo = AnalysisRepo::new(pool.clone());
        let job = repo
            .insert(&a_job(AnalysisTool::Correlation), None)
            .await
            .expect("insert");
        repo.set_progress(job.id, 40, "Fetching series…")
            .await
            .expect("progress");

        repo.fail(job.id, "metric store unreachable")
            .await
            .expect("fail");
        let failed = repo.get(job.id).await.expect("get").expect("row");
        assert_eq!(failed.state, AnalysisJobState::Failed);
        assert_eq!(failed.error.as_deref(), Some("metric store unreachable"));
        assert_eq!(failed.phase, None);
        assert!(failed.finished_ms.is_some());
        // `fail` deliberately does not touch `pct`, unlike `finish`. How far a run got before it
        // broke is the first thing anyone asks of a failed run, and forcing it to 0 or to 100
        // would answer with a number nothing measured.
        assert_eq!(failed.pct, 40);
        assert_eq!(failed.summary, None);
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn cancelling_stops_a_running_run_and_cannot_rewrite_a_finished_one(pool: sqlx::PgPool) {
        let repo = AnalysisRepo::new(pool.clone());

        let running = repo
            .insert(&a_job(AnalysisTool::Anomaly), None)
            .await
            .expect("insert");
        repo.mark_cancelled(running.id).await.expect("cancel");
        let stopped = repo.get(running.id).await.expect("get").expect("row");
        assert_eq!(stopped.state, AnalysisJobState::Cancelled);
        assert_eq!(stopped.phase, None);
        assert!(stopped.finished_ms.is_some());

        // The other direction. An operator pressing Cancel on a run that completed while the page
        // was open must not turn a finished run into a cancelled one — the findings are already
        // written and still shown, so the runs list would then disagree with them.
        let finished = repo
            .insert(&a_job(AnalysisTool::Capacity), None)
            .await
            .expect("insert");
        repo.finish(finished.id, 4, "4 forecasts")
            .await
            .expect("finish");
        let before = repo.get(finished.id).await.expect("get").expect("row");
        repo.mark_cancelled(finished.id).await.expect("cancel");
        let after = repo.get(finished.id).await.expect("get").expect("row");
        assert_eq!(after.state, AnalysisJobState::Done);
        assert_eq!(after.summary.as_deref(), Some("4 forecasts"));
        assert_eq!(after.pct, 100);
        // The guarded statement also writes `finished_at = now()`, so this is what says the write
        // did not happen at all rather than happening and being overwritten by something else.
        assert_eq!(after.finished_ms, before.finished_ms);
    }

    /// One finding, with the fields a test cares about named and the rest filled in.
    fn a_finding(score: f64, node: Option<Uuid>, metric: &str) -> NewFinding {
        NewFinding {
            score,
            severity: if score >= 80.0 { SEV_CRIT } else { SEV_WARN }.into(),
            node_id: node,
            node_name: "core-sw-01".into(),
            metric: metric.into(),
            kind: "spike".into(),
            when_label: "today 03:12".into(),
            duration: "6 min".into(),
            detail: serde_json::json!({ "expected": 1.0, "actual": 9.5 }),
        }
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn findings_come_back_by_score_and_only_for_the_run_that_produced_them(
        pool: sqlx::PgPool,
    ) {
        let repo = AnalysisRepo::new(pool.clone());
        let node = crate::pgtest::node(&pool, "core-sw-01", 11, None).await;
        let mine = repo
            .insert(&a_job(AnalysisTool::Anomaly), None)
            .await
            .expect("insert");
        let other = repo
            .insert(&a_job(AnalysisTool::Anomaly), None)
            .await
            .expect("insert");

        repo.insert_findings(
            mine.id,
            &[
                a_finding(20.0, Some(node), "if_in_octets"),
                a_finding(91.0, Some(node), "cpu_load"),
                // A fleet-level finding carries no node. The column is nullable and the row
                // mapper reads it as an `Option`; a `NOT NULL` here would drop exactly the rows
                // that say something about the fleet rather than about one device.
                a_finding(55.0, None, "fleet_reachability"),
            ],
        )
        .await
        .expect("insert findings");
        repo.insert_findings(other.id, &[a_finding(99.0, Some(node), "not_mine")])
            .await
            .expect("insert findings");

        let got = repo.findings(mine.id).await.expect("findings");
        assert_eq!(got.len(), 3, "the other run's finding must not be here");
        // Highest score first — the report reads this order and does not re-sort.
        assert_eq!(
            got.iter().map(|f| f.metric.as_str()).collect::<Vec<_>>(),
            ["cpu_load", "fleet_reachability", "if_in_octets"]
        );

        let top = &got[0];
        assert!((top.score - 91.0).abs() < f64::EPSILON);
        assert_eq!(top.severity, SEV_CRIT);
        assert_eq!(top.node_id, Some(node));
        assert_eq!(top.node_name, "core-sw-01");
        assert_eq!(top.kind, "spike");
        assert_eq!(top.when_label, "today 03:12");
        assert_eq!(top.duration, "6 min");
        // The report draws its chart out of this blob, so it has to survive the JSONB round trip
        // rather than arriving as the string `{"expected":1.0,...}`.
        assert_eq!(top.detail["actual"], 9.5);
        assert_eq!(got[1].node_id, None);

        assert!(repo
            .findings(Uuid::new_v4())
            .await
            .expect("findings")
            .is_empty());
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_runs_list_is_newest_first_and_each_filter_narrows_it_alone(pool: sqlx::PgPool) {
        let repo = AnalysisRepo::new(pool.clone());
        let first = repo
            .insert(&a_job(AnalysisTool::Anomaly), None)
            .await
            .expect("insert");
        // `created_at` is the server's `now()`, and the `since` filter below asks for an instant
        // strictly between the two rows — so they have to be measurably apart for that assertion
        // to be about anything.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let second = repo
            .insert(&a_job(AnalysisTool::Capacity), None)
            .await
            .expect("insert");
        repo.finish(second.id, 2, "2 forecasts")
            .await
            .expect("finish");

        // No filter is not "no clause": every clause is in the statement with a NULL bind. If the
        // predicate were assembled conditionally instead, this is the branch that would be missing.
        let all = repo.list(50, &JobFilter::default()).await.expect("list");
        assert_eq!(
            all.iter().map(|j| j.id).collect::<Vec<_>>(),
            [second.id, first.id],
            "newest first"
        );

        // …and the limit is applied after that ordering, not before it.
        let one = repo.list(1, &JobFilter::default()).await.expect("list");
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].id, second.id);

        let by_tool = repo
            .list(
                50,
                &JobFilter {
                    tool: Some("anomaly"),
                    ..Default::default()
                },
            )
            .await
            .expect("list");
        assert_eq!(by_tool.iter().map(|j| j.id).collect::<Vec<_>>(), [first.id]);

        let by_state = repo
            .list(
                50,
                &JobFilter {
                    state: Some(AnalysisJobState::Done),
                    ..Default::default()
                },
            )
            .await
            .expect("list");
        assert_eq!(
            by_state.iter().map(|j| j.id).collect::<Vec<_>>(),
            [second.id]
        );

        // Inclusive at the bound: `>=`, so a run created at exactly the instant the operator asked
        // about is in the answer rather than one row off the edge of it.
        //
        // ⚠️ The instant is read out of the raw column, **not** from `created_ms` beside it. That
        // projection is `(EXTRACT(EPOCH FROM created_at) * 1000)::bigint` and the cast to bigint
        // *rounds*, so it names an instant up to half a millisecond after the row it describes —
        // filtering on it excludes that row about half the time, which is what this assertion did
        // on its first run. It is a display value; the findings cursor pages on `created_at`
        // itself for the same reason.
        let exactly_first =
            crate::pgtest::timestamp_of(&pool, "analysis_jobs", "created_at", "id", first.id).await;
        let inclusive = repo
            .list(
                50,
                &JobFilter {
                    since: Some(exactly_first),
                    ..Default::default()
                },
            )
            .await
            .expect("list");
        assert_eq!(
            inclusive.iter().map(|j| j.id).collect::<Vec<_>>(),
            [second.id, first.id]
        );

        // …and one microsecond past it — the column's own resolution — drops that row and no other.
        let by_since = repo
            .list(
                50,
                &JobFilter {
                    since: Some(exactly_first + chrono::Duration::microseconds(1)),
                    ..Default::default()
                },
            )
            .await
            .expect("list");
        assert_eq!(
            by_since.iter().map(|j| j.id).collect::<Vec<_>>(),
            [second.id]
        );

        // Two filters that cannot both hold answer nothing, rather than one of them winning.
        let neither = repo
            .list(
                50,
                &JobFilter {
                    tool: Some("anomaly"),
                    state: Some(AnalysisJobState::Done),
                    ..Default::default()
                },
            )
            .await
            .expect("list");
        assert!(neither.is_empty());
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn pruning_a_run_takes_its_findings_with_it(pool: sqlx::PgPool) {
        let repo = AnalysisRepo::new(pool.clone());
        let job = repo
            .insert(&a_job(AnalysisTool::Flap), None)
            .await
            .expect("insert");
        repo.insert_findings(job.id, &[a_finding(70.0, None, "flap")])
            .await
            .expect("insert findings");

        // Inside the retention window nothing goes — the sweep runs on a cadence, so a statement
        // that deleted regardless of age would empty the table on its first tick.
        assert_eq!(repo.prune_jobs(3_600).await.expect("prune"), 0);
        assert_eq!(crate::pgtest::rows(&pool, "analysis_jobs").await, 1);

        // Past it, the run goes — and `analysis_findings` has no statement of its own here: it is
        // `ON DELETE CASCADE` from the job, which is why the findings table went untrimmed for as
        // long as the jobs table did.
        assert_eq!(repo.prune_jobs(0).await.expect("prune"), 1);
        assert_eq!(crate::pgtest::rows(&pool, "analysis_jobs").await, 0);
        assert_eq!(crate::pgtest::rows(&pool, "analysis_findings").await, 0);
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_restart_fails_the_runs_still_in_flight_and_leaves_the_rest(pool: sqlx::PgPool) {
        let repo = AnalysisRepo::new(pool.clone());
        let running = repo
            .insert(&a_job(AnalysisTool::Anomaly), None)
            .await
            .expect("insert");
        let done = repo
            .insert(&a_job(AnalysisTool::Capacity), None)
            .await
            .expect("insert");
        repo.finish(done.id, 3, "3 forecasts")
            .await
            .expect("finish");
        let already_failed = repo
            .insert(&a_job(AnalysisTool::Flap), None)
            .await
            .expect("insert");
        repo.fail(already_failed.id, "metric store unreachable")
            .await
            .expect("fail");

        assert_eq!(repo.fail_orphans().await.expect("orphans"), 1);

        let after = repo.get(running.id).await.expect("get").expect("row");
        assert_eq!(after.state, AnalysisJobState::Failed);
        assert_eq!(after.error.as_deref(), Some("core restarted while running"));
        assert_eq!(after.phase, None);
        assert!(after.finished_ms.is_some());

        // The other direction of `WHERE state = 'running'`, and the reason it is there: a startup
        // sweep that touched every row would relabel completed runs as crashes and overwrite the
        // reason a genuinely failed one gives.
        let untouched = repo.get(done.id).await.expect("get").expect("row");
        assert_eq!(untouched.state, AnalysisJobState::Done);
        assert_eq!(untouched.summary.as_deref(), Some("3 forecasts"));
        let kept = repo
            .get(already_failed.id)
            .await
            .expect("get")
            .expect("row");
        assert_eq!(kept.error.as_deref(), Some("metric store unreachable"));

        // Idempotent by construction: nothing is left running, so a second sweep finds nothing.
        assert_eq!(repo.fail_orphans().await.expect("orphans"), 0);
    }

    /// A schedule input: `tool`, daily at 03:30.
    fn a_schedule(tool: AnalysisTool, enabled: bool) -> ScheduleInput {
        ScheduleInput {
            params: a_job(tool),
            cadence: crate::cadence::Schedule {
                frequency: crate::cadence::Cadence::Daily,
                day_of_week: None,
                day_of_month: None,
                at_hour: 3,
                at_minute: 30,
            },
            enabled,
        }
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_schedule_round_trips_and_an_update_replaces_every_field_it_names(
        pool: sqlx::PgPool,
    ) {
        let repo = AnalysisRepo::new(pool.clone());
        let at = Utc::now() + chrono::Duration::hours(3);
        let id = repo
            .create_schedule(&a_schedule(AnalysisTool::Anomaly, true), at, Some("horry"))
            .await
            .expect("create");

        let got = repo.get_schedule(id).await.expect("get").expect("row");
        assert_eq!(got.id, id);
        assert_eq!(got.tool, "anomaly");
        assert_eq!(got.scope_kind, "all");
        assert_eq!(got.scope_label, "Whole fleet");
        assert_eq!(got.frequency, crate::cadence::Cadence::Daily);
        assert_eq!(got.at_hour, 3);
        assert_eq!(got.at_minute, 30);
        assert_eq!(got.day_of_week, None);
        assert_eq!(got.day_of_month, None);
        assert!(got.enabled);
        assert_eq!(got.params["depth"], "standard");
        // A schedule that has never fired says so, rather than saying it fired at the epoch.
        assert_eq!(got.last_run_ms, None);
        assert_eq!(got.last_status, None);

        // The weekly form fills a column the daily one leaves NULL, which is what makes "replaces
        // every field it names" worth pinning: an UPDATE that left `day_of_week` behind would put
        // the old cadence's day beside the new cadence's frequency, and the schedule then fires on
        // a day nobody chose.
        let mut weekly = a_schedule(AnalysisTool::Capacity, false);
        weekly.cadence.frequency = crate::cadence::Cadence::Weekly;
        weekly.cadence.day_of_week = Some(2);
        weekly.cadence.at_hour = 19;
        weekly.cadence.at_minute = 5;
        let later = at + chrono::Duration::days(1);
        assert!(repo
            .update_schedule(id, &weekly, later, Some("horry_op"))
            .await
            .expect("update"));

        let after = repo.get_schedule(id).await.expect("get").expect("row");
        assert_eq!(after.tool, "capacity");
        assert_eq!(after.frequency, crate::cadence::Cadence::Weekly);
        assert_eq!(after.day_of_week, Some(2));
        assert_eq!(after.at_hour, 19);
        assert_eq!(after.at_minute, 5);
        assert!(!after.enabled);
        assert!(after.next_run_ms > got.next_run_ms);

        // A write to an id that is not there reports it rather than reporting success — the API
        // edge is what turns that `false` into a 404, so a statement that always claimed a row
        // would make every edit of a deleted schedule look like it worked.
        assert!(!repo
            .update_schedule(Uuid::new_v4(), &weekly, later, None)
            .await
            .expect("update"));
        assert!(repo
            .get_schedule(Uuid::new_v4())
            .await
            .expect("get")
            .is_none());

        assert!(repo.delete_schedule(id).await.expect("delete"));
        assert!(repo.get_schedule(id).await.expect("get").is_none());
        assert!(!repo.delete_schedule(id).await.expect("delete"));
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_due_query_takes_only_enabled_schedules_whose_time_has_come(pool: sqlx::PgPool) {
        let repo = AnalysisRepo::new(pool.clone());
        let now = Utc::now();
        let overdue = repo
            .create_schedule(
                &a_schedule(AnalysisTool::Anomaly, true),
                now - chrono::Duration::minutes(5),
                None,
            )
            .await
            .expect("create");
        // Disabled **and** overdue. Both halves of `enabled = true AND next_run_at <= now()` have
        // to hold: a schedule an operator switched off must not fire because its time passed while
        // it was off, which is the shape a one-clause due-query has.
        let switched_off = repo
            .create_schedule(
                &a_schedule(AnalysisTool::Flap, false),
                now - chrono::Duration::minutes(2),
                None,
            )
            .await
            .expect("create");
        let not_yet = repo
            .create_schedule(
                &a_schedule(AnalysisTool::Capacity, true),
                now + chrono::Duration::hours(6),
                None,
            )
            .await
            .expect("create");

        let due = repo.due_schedules().await.expect("due");
        assert_eq!(due.iter().map(|s| s.id).collect::<Vec<_>>(), [overdue]);

        // The full list carries all three, soonest first — the settings page shows what is
        // scheduled, not what is runnable, so this query deliberately has no predicate.
        let all = repo.list_schedules().await.expect("list");
        assert_eq!(
            all.iter().map(|s| s.id).collect::<Vec<_>>(),
            [overdue, switched_off, not_yet]
        );
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_fire_advances_the_schedule_and_a_deferral_deliberately_does_not(pool: sqlx::PgPool) {
        let repo = AnalysisRepo::new(pool.clone());
        let id = repo
            .create_schedule(
                &a_schedule(AnalysisTool::Anomaly, true),
                Utc::now() - chrono::Duration::minutes(1),
                None,
            )
            .await
            .expect("create");
        let before = repo.get_schedule(id).await.expect("get").expect("row");
        assert_eq!(repo.due_schedules().await.expect("due").len(), 1);

        // Admission control was full. The status is recorded and **nothing else moves**: the
        // schedule has to stay due so the next tick retries it. Advancing `next_run_at` here is
        // how a whole cycle disappears — nothing ran, and nothing afterwards says so.
        repo.mark_deferred(id).await.expect("defer");
        let deferred = repo.get_schedule(id).await.expect("get").expect("row");
        assert_eq!(deferred.last_status, Some(AnalysisScheduleStatus::Busy));
        assert_eq!(deferred.next_run_ms, before.next_run_ms);
        // …and `last_run_at` stays unstamped: nothing ran, and a schedule reporting a last run
        // that produced no row is the confusing half of this failure mode.
        assert_eq!(deferred.last_run_ms, None);
        assert_eq!(
            repo.due_schedules().await.expect("due").len(),
            1,
            "a deferred schedule is still due"
        );

        // The other direction: a fire that produced a run does move it, and stamps both columns.
        let next = Utc::now() + chrono::Duration::days(1);
        repo.mark_fired(id, AnalysisScheduleStatus::Queued, next)
            .await
            .expect("fire");
        let fired = repo.get_schedule(id).await.expect("get").expect("row");
        assert_eq!(fired.last_status, Some(AnalysisScheduleStatus::Queued));
        assert!(fired.last_run_ms.is_some());
        assert!(fired.next_run_ms > before.next_run_ms);
        assert!(repo.due_schedules().await.expect("due").is_empty());

        // Neither write touches `enabled` — a deferral is not a switch-off, and a fire is not one
        // either. A schedule that quietly disabled itself would never be seen to stop.
        assert!(fired.enabled);
    }
}
