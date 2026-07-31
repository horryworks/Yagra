// SPDX-License-Identifier: AGPL-3.0-only
//! Inbound alert acknowledgement reflection (ADR-015, A1).
//!
//! Yagra owns alert *quality* (detection / correlation / suppression / routing) but **not** the
//! ack action — acknowledgement, escalation and on-call live in the external incident tool
//! (PagerDuty / JSM). That tool calls `POST /api/v1/alerts/ack` to **mirror** ack state into
//! Yagra so the Active alerts and History screens can show a read-only "acked" indicator. This is
//! one-way inbound reflection: Yagra never writes ack back out (no two-way sync).
//!
//! Ack is keyed by the alert dedup identity `(node, check, severity)` — the same key the engine
//! dedups on — so it tracks an incident, not a single history transition.

use serde::Serialize;
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use uuid::Uuid;

/// Map key for an ack: the alert dedup identity as `(node, check, severity-string)`.
pub type AckKey = (Uuid, Uuid, String);

/// The acknowledgement view attached to an alert / history row in API responses. Carries who
/// acked it, when, from which external tool, and an optional note. Never carries a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct AckView {
    /// When the external tool recorded the ack (Unix ms, UTC).
    pub at_unix_ms: i64,
    /// External actor reference (id / handle) — not a secret.
    pub by: String,
    /// Originating tool: `pagerduty` | `jsm` | `manual` | …
    pub source: String,
    /// Optional free-text note from the external tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// PostgreSQL-backed store of mirrored ack state (`alert_acks`).
pub struct AckRepo {
    pool: PgPool,
}

impl AckRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Upsert the ack for a dedup key (re-ack just refreshes who/when/source/note).
    pub async fn set(
        &self,
        node: Uuid,
        check: Uuid,
        severity: &str,
        view: &AckView,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO alert_acks \
             (node, check_id, severity, acked_at_unix_ms, acked_by, source, note) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (node, check_id, severity) DO UPDATE SET \
               acked_at_unix_ms = EXCLUDED.acked_at_unix_ms, \
               acked_by = EXCLUDED.acked_by, \
               source = EXCLUDED.source, \
               note = EXCLUDED.note, \
               recorded_at = now()",
        )
        .bind(node)
        .bind(check)
        .bind(severity)
        .bind(view.at_unix_ms)
        .bind(&view.by)
        .bind(&view.source)
        .bind(view.note.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Clear the ack for a dedup key (the external tool un-acked / the incident closed there).
    pub async fn clear(&self, node: Uuid, check: Uuid, severity: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM alert_acks WHERE node = $1 AND check_id = $2 AND severity = $3")
            .bind(node)
            .bind(check)
            .bind(severity)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// All current acks keyed by dedup identity. The active set and a bounded history page are
    /// both small, so loading the full (small) ack set and joining in memory is fine for the MVP.
    pub async fn all(&self) -> anyhow::Result<HashMap<AckKey, AckView>> {
        let rows = sqlx::query(
            "SELECT node, check_id, severity, acked_at_unix_ms, acked_by, source, note \
             FROM alert_acks",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = HashMap::with_capacity(rows.len());
        for row in rows {
            let node: Uuid = row.try_get("node")?;
            let check: Uuid = row.try_get("check_id")?;
            let severity: String = row.try_get("severity")?;
            let view = AckView {
                at_unix_ms: row.try_get("acked_at_unix_ms")?,
                by: row.try_get("acked_by")?,
                source: row.try_get("source")?,
                note: row.try_get("note")?,
            };
            out.insert((node, check, severity), view);
        }
        Ok(out)
    }
}
