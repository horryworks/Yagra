//! Per-user "My Dashboard" layout persistence.
//!
//! Stores one opaque JSON document per user (the ordered widget instances + their spans +
//! per-widget settings). The backend deliberately does **not** parse the widget shape — the
//! WebUI owns it and sanitizes/migrates on load — so this layer just round-trips a
//! `serde_json::Value` keyed by the account. Scoped to the caller by `username` (the session
//! carries the username, not the user id) via a `users` subquery, so a user can only ever
//! read/write their own row.

use serde_json::Value;
use sqlx::{PgPool, Row};

/// PostgreSQL-backed per-user dashboard layouts (`user_dashboards`).
pub struct DashboardRepo {
    pool: PgPool,
}

impl DashboardRepo {
    /// New store over the metadata pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The saved layout for `username`, or `None` if the user has never saved one (the WebUI
    /// then falls back to its default layout). Resolves the account by name in a subquery so an
    /// unknown username simply yields no row.
    pub async fn get_for_user(&self, username: &str) -> anyhow::Result<Option<Value>> {
        let row = sqlx::query(
            "SELECT layout_json FROM user_dashboards \
             WHERE user_id = (SELECT id FROM users WHERE username = $1)",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(row) => Ok(Some(row.try_get::<Value, _>("layout_json")?)),
            None => Ok(None),
        }
    }

    /// Upsert `layout` for `username`. Returns `false` if no such (enabled-or-not) account
    /// exists — the `INSERT … SELECT … FROM users` affects zero rows, never a foreign-key error.
    /// The whole row is replaced (the WebUI always sends the full layout, not a patch).
    pub async fn upsert_for_user(&self, username: &str, layout: &Value) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "INSERT INTO user_dashboards (user_id, layout_json) \
             SELECT id, $2 FROM users WHERE username = $1 \
             ON CONFLICT (user_id) DO UPDATE \
                 SET layout_json = EXCLUDED.layout_json, updated_at = now()",
        )
        .bind(username)
        .bind(layout)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }
}
