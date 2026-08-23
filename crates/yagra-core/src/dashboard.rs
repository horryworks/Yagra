// SPDX-License-Identifier: AGPL-3.0-only
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

/// The single global "Shared Dashboard" layout (`shared_dashboard`, one row keyed `id = TRUE`).
///
/// Like [`DashboardRepo`] this round-trips an opaque JSON document the WebUI owns. Unlike it the
/// layout is global, not per-user: everyone reads the same row and only admins write it (the write
/// authorization is enforced at the API edge, not here).
pub struct SharedDashboardRepo {
    pool: PgPool,
}

impl SharedDashboardRepo {
    /// New store over the metadata pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The saved global layout, or `None` if no admin has ever saved one (the WebUI then falls
    /// back to its default layout).
    pub async fn get_shared(&self) -> anyhow::Result<Option<Value>> {
        let row = sqlx::query("SELECT layout_json FROM shared_dashboard WHERE id = TRUE")
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(row) => Ok(Some(row.try_get::<Value, _>("layout_json")?)),
            None => Ok(None),
        }
    }

    /// Upsert the global layout, recording which admin (`updated_by`) last saved it. The single
    /// row is replaced wholesale (the WebUI always sends the full layout, not a patch).
    pub async fn upsert_shared(&self, layout: &Value, updated_by: &str) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO shared_dashboard (id, layout_json, updated_by) VALUES (TRUE, $1, $2) \
             ON CONFLICT (id) DO UPDATE \
                 SET layout_json = EXCLUDED.layout_json, \
                     updated_by = EXCLUDED.updated_by, \
                     updated_at = now()",
        )
        .bind(layout)
        .bind(updated_by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    /// This module's code, comments stripped — see
    /// [`crate::module_source::code_no_comments`] for why both.
    fn production_source() -> String {
        crate::module_source::code_no_comments("src", "dashboard")
    }

    #[test]
    fn an_unknown_username_writes_nothing_instead_of_erroring() {
        // The `INSERT … SELECT … FROM users` is what turns "no such account" into zero rows
        // affected. A literal VALUES would raise a foreign-key violation instead, which the API
        // would have to render as a 500 rather than the 404 the caller deserves.
        let src = production_source();
        assert!(src.contains("SELECT id, $2 FROM users WHERE username = $1"));
        assert!(src.contains("Ok(res.rows_affected() > 0)"));
    }

    #[test]
    fn a_users_layout_is_resolved_by_name_through_a_subquery() {
        // Same reason on the read side: an unknown username yields no row rather than an error.
        assert!(production_source()
            .contains("WHERE user_id = (SELECT id FROM users WHERE username = $1)"));
    }

    #[test]
    fn every_layout_write_replaces_the_row_wholesale() {
        // The WebUI always sends the full document, so both writers upsert. Without this a second
        // save would fail on the primary key and the operator's edit would silently not persist.
        let src = production_source();
        assert_eq!(src.matches("ON CONFLICT").count(), 2);
        assert_eq!(
            src.matches("SET layout_json = EXCLUDED.layout_json")
                .count(),
            2
        );
    }

    #[test]
    fn the_shared_dashboard_is_a_single_row_keyed_true() {
        // One global layout, not one per admin: the boolean primary key is what makes "there is
        // exactly one" a database fact rather than a convention.
        let src = production_source();
        assert!(src.contains("FROM shared_dashboard WHERE id = TRUE"));
        assert!(src.contains(
            "INSERT INTO shared_dashboard (id, layout_json, updated_by) VALUES (TRUE, $1, $2)"
        ));
    }

    #[test]
    fn every_statement_binds_its_values_instead_of_interpolating_them() {
        // The layout is an opaque operator-authored JSON document.
        let src = production_source();
        for builder in ["format!(", "push_str("] {
            assert!(
                !src.contains(builder),
                "SQL may be being built by string concatenation ({builder}); bind the value instead"
            );
        }
    }
}
