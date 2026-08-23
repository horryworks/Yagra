// SPDX-License-Identifier: AGPL-3.0-only
//! Inbound alert acknowledgement reflection (ADR-015, A1).
//!
//! Yagra owns alert *quality* (detection / correlation / suppression / routing) but **not** the
//! ack action — acknowledgement, escalation and on-call live in the external incident tool
//! (PagerDuty / JSM). That tool calls `POST /api/v1/alerts/ack` to **mirror** ack state into
//! Yagra so the Active alerts and History screens can show a read-only "acked" indicator. This is
//! one-way inbound reflection: Yagra never writes ack back out (no two-way sync).
//!
//! Ack is keyed by the alert dedup identity `(subject, check, severity)` — the same key the engine
//! dedups on — so it tracks an incident, not a single history transition.
//!
//! The subject is stored in the column still named `node`, as its
//! [`storage_id`](yagra_alert::Subject::storage_id): a node's own id for a node alert (so no
//! existing acknowledgement moved), a derived one for a pool. Migration 0075 says why that beats a
//! nullable column, and `subject_kind`/`subject_ref` beside it are what say which it is.

use serde::Serialize;
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use uuid::Uuid;
use yagra_alert::Subject;

/// Map key for an ack: the alert dedup identity as `(subject-storage-id, check, severity-string)`.
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
        subject: &Subject,
        check: Uuid,
        severity: &str,
        view: &AckView,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO alert_acks \
             (node, subject_kind, subject_ref, check_id, severity, acked_at_unix_ms, acked_by, source, note) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (node, check_id, severity) DO UPDATE SET \
               subject_kind = EXCLUDED.subject_kind, \
               subject_ref = EXCLUDED.subject_ref, \
               acked_at_unix_ms = EXCLUDED.acked_at_unix_ms, \
               acked_by = EXCLUDED.acked_by, \
               source = EXCLUDED.source, \
               note = EXCLUDED.note, \
               recorded_at = now()",
        )
        .bind(subject.storage_id())
        .bind(subject.kind().as_str())
        .bind(subject.name())
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
    pub async fn clear(
        &self,
        subject: &Subject,
        check: Uuid,
        severity: &str,
    ) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM alert_acks WHERE node = $1 AND check_id = $2 AND severity = $3")
            .bind(subject.storage_id())
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

#[cfg(test)]
mod tests {

    /// This module's code, comments stripped — see
    /// [`crate::module_source::code_no_comments`] for why both.
    fn production_source() -> String {
        crate::module_source::code_no_comments("src", "ack")
    }

    #[test]
    fn the_ack_identity_is_the_alerts_dedup_key_on_every_statement() {
        // An ack belongs to (node, check, severity) — the same triple the alert engine dedups on.
        // If any statement keyed on fewer columns, acknowledging a warning would silently also
        // acknowledge the critical that follows it, which is the page an operator must still get.
        let src = production_source();
        assert!(src.contains("ON CONFLICT (node, check_id, severity) DO UPDATE SET"));
        assert!(src.contains(
            "DELETE FROM alert_acks WHERE node = $1 AND check_id = $2 AND severity = $3"
        ));
    }

    #[test]
    fn the_ack_is_stored_under_the_subjects_identity_not_a_node_reference() {
        // The column is still called `node` (migration 0075 says why), so the one thing that must
        // not creep back is a call site binding a raw node id: a pool alert would then either fail
        // to ack or land on whatever node that id names.
        let src = production_source();
        assert!(src.contains(".bind(subject.storage_id())"));
        assert!(src.contains(".bind(subject.kind().as_str())"));
        for writer in ["pub async fn set(", "pub async fn clear("] {
            let sig = src.split(writer).nth(1).expect("both writers exist");
            assert!(
                sig[..sig.find(')').unwrap_or(sig.len())].contains("subject: &Subject"),
                "{writer} takes a Subject, never a bare node id"
            );
        }
    }

    #[test]
    fn re_acking_refreshes_the_record_rather_than_failing_or_duplicating() {
        // The external tool re-sends an ack whenever its incident changes, so the write has to be
        // idempotent: same key ⇒ update who/when/source/note in place. `subject_kind`/`subject_ref`
        // are refreshed too: a row written before 0075 defaults to `node`, and a re-ack is the
        // moment that can be corrected.
        let src = production_source();
        for column in [
            "subject_kind = EXCLUDED.subject_kind",
            "subject_ref = EXCLUDED.subject_ref",
            "acked_at_unix_ms = EXCLUDED.acked_at_unix_ms",
            "acked_by = EXCLUDED.acked_by",
            "source = EXCLUDED.source",
            "note = EXCLUDED.note",
        ] {
            assert!(src.contains(column), "re-ack does not refresh {column}");
        }
    }

    #[test]
    fn the_in_memory_key_matches_the_columns_the_row_is_read_from() {
        // `all()` builds its map key positionally from three reads; a column renamed on one side
        // only would produce a key the alert lookup can never hit, so acks would silently vanish
        // from the UI while still being stored.
        let src = production_source();
        assert!(src.contains(r#"row.try_get("node")"#));
        assert!(src.contains(r#"row.try_get("check_id")"#));
        assert!(src.contains(r#"row.try_get("severity")"#));
        assert!(src.contains("out.insert((node, check, severity), view)"));
    }

    #[test]
    fn every_statement_binds_its_values_instead_of_interpolating_them() {
        // `source` and `note` are free text from an external incident tool.
        let src = production_source();
        for builder in ["format!(", "push_str("] {
            assert!(
                !src.contains(builder),
                "SQL may be being built by string concatenation ({builder}); bind the value instead"
            );
        }
    }
}
