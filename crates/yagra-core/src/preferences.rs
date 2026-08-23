// SPDX-License-Identifier: AGPL-3.0-only
//! Per-account WebUI preferences persistence (ADR-058).
//!
//! Stores one opaque JSON document per account — the settings that follow a person between
//! machines rather than living in one browser's `localStorage`. The backend deliberately does
//! **not** parse it: the WebUI owns the shape and migrates it client-side, which is what makes the
//! *second* preference cost nothing on this side. Scoped to the caller by `username` (the session
//! carries the username, not the user id) via a `users` subquery, so an account can only ever
//! read/write its own row.
//!
//! ⚠️ Anything the **backend** must read does not belong here — it cannot validate, query or
//! migrate the contents. Such a value wants its own typed column and its own migration.
//!
//! ⚠️ This is the second copy of a ~25-line shape; `dashboard.rs` is the first. A generic
//! `UserJsonDocRepo` was considered and rejected: a table name cannot be a bind parameter, so the
//! generic would have to build its SQL with `format!`, which
//! `every_statement_binds_its_values_instead_of_interpolating_them` forbids in both files. **If a
//! third copy appears, that trade changes** — revisit rather than adding it.

use serde_json::Value;
use sqlx::{PgPool, Row};

/// PostgreSQL-backed per-account WebUI preferences (`user_preferences`).
pub struct UserPrefsRepo {
    pool: PgPool,
}

impl UserPrefsRepo {
    /// New store over the metadata pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The saved preferences for `username`, or `None` if the account has never saved any (the
    /// WebUI then keeps its browser-local values). Resolves the account by name in a subquery so an
    /// unknown username simply yields no row.
    pub async fn get_for_user(&self, username: &str) -> anyhow::Result<Option<Value>> {
        let row = sqlx::query(
            "SELECT prefs_json FROM user_preferences \
             WHERE user_id = (SELECT id FROM users WHERE username = $1)",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(row) => Ok(Some(row.try_get::<Value, _>("prefs_json")?)),
            None => Ok(None),
        }
    }

    /// Upsert `prefs` for `username`. Returns `false` if no such account exists — the
    /// `INSERT … SELECT … FROM users` affects zero rows, never a foreign-key error, which is what
    /// lets the API answer 404 instead of 500 for a session whose account has been deleted.
    /// The whole row is replaced (the WebUI always sends the full document, not a patch).
    pub async fn upsert_for_user(&self, username: &str, prefs: &Value) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "INSERT INTO user_preferences (user_id, prefs_json) \
             SELECT id, $2 FROM users WHERE username = $1 \
             ON CONFLICT (user_id) DO UPDATE \
                 SET prefs_json = EXCLUDED.prefs_json, updated_at = now()",
        )
        .bind(username)
        .bind(prefs)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {

    /// This module's code, comments stripped — see
    /// [`crate::module_source::code_no_comments`] for why both.
    fn production_source() -> String {
        crate::module_source::code_no_comments("src", "preferences")
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
    fn preferences_are_resolved_by_name_through_a_subquery() {
        // Same reason on the read side: an unknown username yields no row rather than an error.
        assert!(production_source()
            .contains("WHERE user_id = (SELECT id FROM users WHERE username = $1)"));
    }

    #[test]
    fn every_preferences_write_replaces_the_row_wholesale() {
        // The WebUI always sends the full document, so the writer upserts. Without this a second
        // save would fail on the primary key and the operator's change would silently not persist.
        let src = production_source();
        assert_eq!(src.matches("ON CONFLICT").count(), 1);
        assert_eq!(
            src.matches("SET prefs_json = EXCLUDED.prefs_json").count(),
            1
        );
    }

    #[test]
    fn every_statement_binds_its_values_instead_of_interpolating_them() {
        // The document is opaque and client-supplied; it never reaches SQL as text.
        let src = production_source();
        for builder in ["format!(", "push_str("] {
            assert!(
                !src.contains(builder),
                "SQL may be being built by string concatenation ({builder}); bind the value instead"
            );
        }
    }
}
