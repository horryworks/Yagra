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
}
