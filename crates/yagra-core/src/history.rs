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
