//! Durable poller inventory persistence (ADR-009).
//!
//! The `pollers` table is the *durable* record of which pollers have ever registered — first/last
//! seen, and the pool/version/incarnation of their most recent heartbeat. It is what lets the
//! Pollers view show a poller that is currently **offline** (its live liveness/assignment live in
//! Redis, ADR-004, and vanish on TTL expiry). This is the I/O adapter only; the coordinator
//! decides *when* to upsert (throttled, so a 10s heartbeat doesn't write every beat) and how the
//! rows are merged with the live view for the API — both in a later step.
//!
//! Runtime `sqlx::query` (not the compile-time macro) so the build needs no live database, and all
//! inputs are bound parameters (security.md). Mirrors the [`crate::audit`] repo's shape.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// One `pollers` row (API shape). Timestamps are RFC 3339 text at the edge, matching how the
/// audit repo exposes its `at` column.
#[derive(Debug, Clone, Serialize)]
pub struct PollerRow {
    /// Sanitized poller id (the NATS-subject-safe identifier, stable across restarts).
    pub id: String,
    /// Pool the poller last reported serving.
    pub pool: String,
    /// When this poller was first seen (RFC 3339).
    pub first_seen: String,
    /// When this poller was last seen (RFC 3339).
    pub last_seen: String,
    /// Build version from its most recent heartbeat, if reported.
    pub last_version: Option<String>,
    /// Per-process incarnation from its most recent heartbeat, if reported.
    pub last_incarnation: Option<Uuid>,
}

/// One `monitoring_gaps` row (API shape). A gap is one core↔poller **visibility outage**: core
/// stopped hearing from the poller (partition or the poller went down) and later saw it again. If the
/// poller was alive but partitioned, its store-and-forward buffer backfills the metrics for the
/// window on reconnect (Phase 3); alerts are *not* backfilled (they resume from "now").
#[derive(Debug, Clone, Serialize)]
pub struct MonitoringGapRow {
    /// Row id.
    pub id: Uuid,
    /// The poller whose visibility lapsed.
    pub poller_id: String,
    /// Pool it serves.
    pub pool: String,
    /// Start of the gap window (RFC 3339 — core's last contact before the outage).
    pub started_at: String,
    /// End of the gap window (RFC 3339 — core heard from it again).
    pub ended_at: String,
    /// Gap length in seconds (UI convenience).
    pub duration_secs: i64,
    /// When core recorded the gap (RFC 3339).
    pub recorded_at: String,
}

/// PostgreSQL-backed durable poller inventory (`pollers`).
pub struct PollerRepo {
    pool: PgPool,
}

impl PollerRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Record that a poller was seen: insert it (first contact) or refresh `last_seen` and the
    /// pool/version/incarnation of its latest heartbeat. `first_seen` is preserved on update.
    /// Call-site throttling (so a 10s heartbeat isn't a write per beat) is the coordinator's job.
    pub async fn upsert_seen(
        &self,
        id: &str,
        pool: &str,
        version: &str,
        incarnation: Uuid,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO pollers (id, pool, last_version, last_incarnation) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (id) DO UPDATE SET \
               last_seen = now(), \
               pool = EXCLUDED.pool, \
               last_version = EXCLUDED.last_version, \
               last_incarnation = EXCLUDED.last_incarnation",
        )
        .bind(id)
        .bind(pool)
        .bind(version)
        .bind(incarnation)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every poller in the inventory, ordered by id.
    pub async fn list(&self) -> anyhow::Result<Vec<PollerRow>> {
        let rows = sqlx::query(
            "SELECT id, pool, first_seen, last_seen, last_version, last_incarnation \
             FROM pollers ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let first_seen: DateTime<Utc> = row.try_get("first_seen")?;
                let last_seen: DateTime<Utc> = row.try_get("last_seen")?;
                Ok(PollerRow {
                    id: row.try_get("id")?,
                    pool: row.try_get("pool")?,
                    first_seen: first_seen.to_rfc3339(),
                    last_seen: last_seen.to_rfc3339(),
                    last_version: row.try_get("last_version")?,
                    last_incarnation: row.try_get("last_incarnation")?,
                })
            })
            .collect()
    }

    /// Delete a poller by id (operator removing a decommissioned poller). Returns whether a row
    /// was removed.
    pub async fn delete(&self, id: &str) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM pollers WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Record one detected monitoring gap (a known poller reappeared after being offline). `started`
    /// and `ended` are Unix milliseconds. Best-effort: a failed insert just means the gap isn't
    /// listed. The coordinator calls this once per offline→online transition (one row per gap).
    pub async fn insert_monitoring_gap(
        &self,
        poller_id: &str,
        pool: &str,
        started_ms: i64,
        ended_ms: i64,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO monitoring_gaps (poller_id, pool, started_at_unix_ms, ended_at_unix_ms) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(poller_id)
        .bind(pool)
        .bind(started_ms)
        .bind(ended_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The most recent monitoring gaps, newest first (capped). Powers the Pollers page's "Recent
    /// monitoring gaps" section.
    pub async fn list_monitoring_gaps(&self, limit: i64) -> anyhow::Result<Vec<MonitoringGapRow>> {
        let rows = sqlx::query(
            "SELECT id, poller_id, pool, started_at_unix_ms, ended_at_unix_ms, recorded_at \
             FROM monitoring_gaps ORDER BY recorded_at DESC LIMIT $1",
        )
        .bind(limit.clamp(1, 1000))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let started: i64 = row.try_get("started_at_unix_ms")?;
                let ended: i64 = row.try_get("ended_at_unix_ms")?;
                let recorded_at: DateTime<Utc> = row.try_get("recorded_at")?;
                Ok(MonitoringGapRow {
                    id: row.try_get("id")?,
                    poller_id: row.try_get("poller_id")?,
                    pool: row.try_get("pool")?,
                    started_at: ms_to_rfc3339(started),
                    ended_at: ms_to_rfc3339(ended),
                    duration_secs: (ended - started).max(0) / 1000,
                    recorded_at: recorded_at.to_rfc3339(),
                })
            })
            .collect()
    }
}

/// Format Unix milliseconds as RFC 3339 UTC (matching how the rest of this repo exposes timestamps).
fn ms_to_rfc3339(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}
