//! Alert history persistence (Workstream #3).
//!
//! Appends one row per alert fire/resolve so the lifecycle is durable beyond the in-memory
//! active set. Live-only (PostgreSQL); the read endpoint returns an empty list in skeleton
//! mode.

use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_alert::Alert;
use yagra_common::Severity;

fn severity_str(s: Severity) -> &'static str {
    match s {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Critical => "critical",
    }
}

/// One alert-history row for the API.
#[derive(Debug, Clone, Serialize)]
pub struct AlertHistoryRow {
    pub node: Uuid,
    pub check: Uuid,
    pub severity: String,
    pub state: String,
    pub at_unix_ms: i64,
    pub resolved: bool,
}

/// PostgreSQL-backed alert history.
pub struct AlertHistoryStore {
    pool: PgPool,
}

impl AlertHistoryStore {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Append a fire (`resolved=false`) or recovery (`resolved=true`) record.
    pub async fn record(&self, alert: &Alert, resolved: bool) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO alert_history \
             (id, node, check_id, severity, state, at_unix_ms, resolved) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(Uuid::new_v4())
        .bind(alert.node.as_uuid())
        .bind(alert.check.as_uuid())
        .bind(severity_str(alert.severity))
        .bind(alert.state.as_str())
        .bind(alert.at_unix_ms)
        .bind(resolved)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Nodes with the most alert **fires** (resolved=false) at or after `since_ms` (Unix ms),
    /// highest first. Powers the "Top alerting nodes" widget (chronic offenders).
    pub async fn top_nodes_by_fires(
        &self,
        since_ms: i64,
        limit: i64,
    ) -> anyhow::Result<Vec<(Uuid, i64)>> {
        let rows = sqlx::query(
            "SELECT node, count(*) AS n FROM alert_history \
             WHERE resolved = false AND at_unix_ms >= $1 \
             GROUP BY node ORDER BY n DESC LIMIT $2",
        )
        .bind(since_ms)
        .bind(limit.clamp(1, 100))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("node")?, row.try_get::<i64, _>("n")?)))
            .collect()
    }

    /// Alert-fire counts bucketed by weekday (0=Sun … 6=Sat, UTC) × hour (0–23) at or after
    /// `since_ms`. Powers the "Alert calendar" heatmap. UTC so buckets are stable regardless of
    /// the DB session timezone.
    pub async fn fires_by_weekday_hour(
        &self,
        since_ms: i64,
    ) -> anyhow::Result<Vec<(i32, i32, i64)>> {
        let rows = sqlx::query(
            "SELECT \
                extract(dow from to_timestamp(at_unix_ms / 1000.0) at time zone 'UTC')::int AS dow, \
                extract(hour from to_timestamp(at_unix_ms / 1000.0) at time zone 'UTC')::int AS hour, \
                count(*) AS n \
             FROM alert_history WHERE resolved = false AND at_unix_ms >= $1 \
             GROUP BY dow, hour",
        )
        .bind(since_ms)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    row.try_get("dow")?,
                    row.try_get("hour")?,
                    row.try_get::<i64, _>("n")?,
                ))
            })
            .collect()
    }

    /// The most recent `limit` history rows (newest first).
    pub async fn recent(&self, limit: i64) -> anyhow::Result<Vec<AlertHistoryRow>> {
        let rows = sqlx::query(
            "SELECT node, check_id, severity, state, at_unix_ms, resolved \
             FROM alert_history ORDER BY recorded_at DESC LIMIT $1",
        )
        .bind(limit.clamp(1, 1000))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(AlertHistoryRow {
                    node: row.try_get("node")?,
                    check: row.try_get("check_id")?,
                    severity: row.try_get("severity")?,
                    state: row.try_get("state")?,
                    at_unix_ms: row.try_get("at_unix_ms")?,
                    resolved: row.try_get("resolved")?,
                })
            })
            .collect()
    }
}
