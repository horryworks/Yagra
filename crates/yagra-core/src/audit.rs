// SPDX-License-Identifier: AGPL-3.0-only
//! Audit log persistence (security.md: who changed what, when).
//!
//! Rows are written by the API middleware in [`crate::api`] — one per mutating request
//! (method + path + status) plus login events from the auth handler. This is the I/O
//! adapter only; what gets recorded is decided at the API layer. Append-only: there is no
//! update or delete path, and reads are admin-gated (`ViewAudit`).

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// One audit row (API shape; `at` is RFC 3339 text at the edge).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct AuditRow {
    pub id: Uuid,
    pub at: String,
    pub username: String,
    pub action: String,
    pub status: i32,
}

/// Default / maximum page sizes for the listing endpoint.
pub const DEFAULT_LIMIT: i64 = 100;
pub const MAX_LIMIT: i64 = 500;

/// PostgreSQL-backed append-only audit log.
pub struct AuditRepo {
    pool: PgPool,
}

impl AuditRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Append one entry. Failures are the caller's to log — auditing must never take the
    /// API down, so callers treat this as best-effort.
    pub async fn record(&self, username: &str, action: &str, status: u16) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO audit_log (id, username, action, status) VALUES ($1, $2, $3, $4)")
            .bind(Uuid::new_v4())
            .bind(username)
            .bind(action)
            .bind(i32::from(status))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Newest-first page. `before` is a keyset cursor (rows strictly older than it);
    /// `limit` is clamped to [1, MAX_LIMIT].
    pub async fn list(
        &self,
        limit: i64,
        before: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<AuditRow>> {
        let limit = limit.clamp(1, MAX_LIMIT);
        let rows = match before {
            Some(before) => {
                sqlx::query(
                    "SELECT id, at, username, action, status FROM audit_log \
                     WHERE at < $1 ORDER BY at DESC LIMIT $2",
                )
                .bind(before)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT id, at, username, action, status FROM audit_log \
                     ORDER BY at DESC LIMIT $1",
                )
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.into_iter()
            .map(|row| {
                let at: DateTime<Utc> = row.try_get("at")?;
                Ok(AuditRow {
                    id: row.try_get("id")?,
                    at: at.to_rfc3339(),
                    username: row.try_get("username")?,
                    action: row.try_get("action")?,
                    status: row.try_get("status")?,
                })
            })
            .collect()
    }
}
