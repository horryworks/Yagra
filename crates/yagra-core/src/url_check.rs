// SPDX-License-Identifier: AGPL-3.0-only
//! URL-check persistence (the per-node HTTP/HTTPS monitor configuration).
//!
//! Metadata, so it lives in PostgreSQL (store separation). One row per node (1:1), keyed by
//! `node_id`. This is the I/O adapter; the config type ([`UrlCheckConfig`]) and its validation
//! live in `yagra-common` (tested there). Runtime `sqlx::query` (not the compile-time macro) so
//! the build needs no live database — consistent with [`crate::repo`].

use sqlx::types::Json;
use sqlx::{PgPool, Row};
use std::collections::HashSet;
use uuid::Uuid;
use yagra_common::url_check::{
    BodyMatch, ExpectedStatus, HttpMethod, JsonExtract, UrlCheckConfig, DEFAULT_BODY_MAX_BYTES,
};
use yagra_common::CredentialId;

/// PostgreSQL-backed store for per-node URL-check configs.
pub struct UrlCheckRepo {
    pool: PgPool,
}

impl UrlCheckRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The URL check for a node, if it has one (None ⇒ not a URL monitor).
    pub async fn get(&self, node_id: Uuid) -> anyhow::Result<Option<UrlCheckConfig>> {
        let row = sqlx::query(
            "SELECT url, method, expected_status, verify_tls, follow_redirects, timeout_ms, \
                    credential_id, body_match, json_extract, body_max_bytes \
             FROM url_checks WHERE node_id = $1",
        )
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let method: String = row.try_get("method")?;
        let expected: Json<ExpectedStatus> = row.try_get("expected_status")?;
        let timeout_ms: i32 = row.try_get("timeout_ms")?;
        let credential_id: Option<Uuid> = row.try_get("credential_id")?;
        let body_match: Option<Json<BodyMatch>> = row.try_get("body_match")?;
        let json_extract: Option<Json<Vec<JsonExtract>>> = row.try_get("json_extract")?;
        let body_max_bytes: i32 = row.try_get("body_max_bytes")?;
        Ok(Some(UrlCheckConfig {
            url: row.try_get("url")?,
            method: HttpMethod::from_token(&method).unwrap_or_default(),
            expected_status: expected.0,
            verify_tls: row.try_get("verify_tls")?,
            follow_redirects: row.try_get("follow_redirects")?,
            timeout_ms: u32::try_from(timeout_ms).unwrap_or(5000),
            credential: credential_id.map(CredentialId::from),
            body_match: body_match.map(|j| j.0),
            json_extract: json_extract.map(|j| j.0).unwrap_or_default(),
            // Same i32↔u32 shape as `timeout_ms` above, and the same documented fallback on both
            // sides of the round trip so a clamped write reads back as what was stored.
            body_max_bytes: u32::try_from(body_max_bytes).unwrap_or(DEFAULT_BODY_MAX_BYTES),
        }))
    }

    /// Create or replace a node's URL check (idempotent upsert).
    pub async fn upsert(&self, node_id: Uuid, cfg: &UrlCheckConfig) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO url_checks \
                (node_id, url, method, expected_status, verify_tls, follow_redirects, timeout_ms, \
                 credential_id, body_match, json_extract, body_max_bytes) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
             ON CONFLICT (node_id) DO UPDATE SET \
                url = EXCLUDED.url, method = EXCLUDED.method, \
                expected_status = EXCLUDED.expected_status, verify_tls = EXCLUDED.verify_tls, \
                follow_redirects = EXCLUDED.follow_redirects, timeout_ms = EXCLUDED.timeout_ms, \
                credential_id = EXCLUDED.credential_id, body_match = EXCLUDED.body_match, \
                json_extract = EXCLUDED.json_extract, \
                body_max_bytes = EXCLUDED.body_max_bytes, updated_at = now()",
        )
        .bind(node_id)
        .bind(&cfg.url)
        .bind(cfg.method.as_str())
        .bind(Json(&cfg.expected_status))
        .bind(cfg.verify_tls)
        .bind(cfg.follow_redirects)
        .bind(i32::try_from(cfg.timeout_ms).unwrap_or(5000))
        .bind(cfg.credential.map(|c| c.as_uuid()))
        // The PUT is a replace, so an absent rule must clear a stored one — `None` binds SQL NULL
        // and the upsert copies it over. Skipping the bind when there is no rule would make
        // "remove the content check" impossible through the only endpoint that edits it.
        .bind(cfg.body_match.as_ref().map(Json))
        // An empty rule set stores SQL NULL rather than `[]`, so "no extraction" has one
        // representation in the column instead of two that read the same.
        .bind((!cfg.json_extract.is_empty()).then_some(Json(&cfg.json_extract)))
        .bind(i32::try_from(cfg.body_max_bytes).unwrap_or(DEFAULT_BODY_MAX_BYTES as i32))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every node that has a URL check.
    ///
    /// Preloaded once per scheduler sweep so the sweep never adds a per-node round trip at fleet
    /// scale — the same trick [`crate::dns_check::DnsCheckRepo::node_ids`] and
    /// `MerakiDeviceRepo::node_ids` exist for. Until this landed the URL lookup ran unconditionally
    /// for every node on every sweep (50k nodes ⇒ 50k round trips a round), which the scheduler's
    /// own doc comment had been carrying as a named debt.
    pub async fn node_ids(&self) -> anyhow::Result<HashSet<Uuid>> {
        let rows = sqlx::query("SELECT node_id FROM url_checks")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|r| r.try_get::<Uuid, _>("node_id").map_err(Into::into))
            .collect()
    }

    /// Of the given node ids, which are URL monitors — the page-scoped variant of
    /// [`Self::node_ids`], for the inventory list's kind badge.
    ///
    /// Bounded by the page size rather than by how many monitors exist: `/nodes` keyset-pages a
    /// 50k fleet and calls this once per page, so the full-table read would grow without limit
    /// while returning rows the page cannot use. Empty input short-circuits so we never send
    /// `= ANY('{}')`. Mirrors `MerakiDeviceRepo::filter_meraki`.
    pub async fn filter_url(&self, node_ids: &[Uuid]) -> anyhow::Result<HashSet<Uuid>> {
        if node_ids.is_empty() {
            return Ok(HashSet::new());
        }
        let rows = sqlx::query("SELECT node_id FROM url_checks WHERE node_id = ANY($1)")
            .bind(node_ids)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|r| r.try_get::<Uuid, _>("node_id").map_err(Into::into))
            .collect()
    }

    /// Delete a node's URL check. Returns whether a row was removed.
    pub async fn delete(&self, node_id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM url_checks WHERE node_id = $1")
            .bind(node_id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = include_str!("url_check.rs");

    /// Executable code above the tests, comments stripped — see `dns_check.rs` for why both.
    fn production_source() -> String {
        SRC.split("#[cfg(test)]")
            .next()
            .expect("split always yields a first element")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn an_unknown_method_reads_as_the_default_rather_than_failing() {
        // Written by a newer binary, read by this one: keep probing with the default verb instead
        // of dropping the monitor.
        assert_eq!(HttpMethod::from_token("PATCH"), None);
        assert_eq!(
            HttpMethod::from_token("PATCH").unwrap_or_default(),
            HttpMethod::default()
        );
        // Known tokens still resolve, so the fallback is not swallowing everything.
        assert_eq!(HttpMethod::from_token("HEAD"), Some(HttpMethod::Head));
    }

    #[test]
    fn an_out_of_range_timeout_falls_back_on_both_the_read_and_the_write() {
        // The column is i32 and the config is u32, so the conversion can fail in either
        // direction. Both sides land on the same documented 5000 ms — otherwise a row written
        // with a fallback would read back as a different timeout than the one stored.
        assert_eq!(u32::try_from(-1_i32).unwrap_or(5000), 5000);
        assert_eq!(u32::try_from(2_500_i32).unwrap_or(5000), 2_500);
        assert_eq!(i32::try_from(u32::MAX).unwrap_or(5000), 5000);
        let src = production_source();
        assert_eq!(src.matches("unwrap_or(5000)").count(), 2);
    }

    #[test]
    fn the_config_is_one_row_per_node_so_saving_twice_edits_rather_than_duplicates() {
        // `PUT .../url-check` is a replace; without the upsert a second save would either fail on
        // the primary key or leave two configs for one node, only one of which is ever polled.
        let src = production_source();
        assert!(src.contains("ON CONFLICT (node_id) DO UPDATE SET"));
        assert!(src.contains("DELETE FROM url_checks WHERE node_id = $1"));
    }

    #[test]
    fn the_page_scoped_filter_is_a_set_query_not_a_per_row_lookup() {
        // `/nodes` keyset-pages a 50k fleet and asks this once per page to badge the rows. A
        // per-node `WHERE node_id = $1` would be a round trip per row, and the unscoped
        // `SELECT node_id FROM url_checks` grows with how many monitors exist while returning rows
        // the page cannot use. The empty guard matters too: `= ANY('{}')` is a query sent to answer
        // nothing, on every page of a fleet with no URL monitors at all.
        let src = production_source();
        assert!(src.contains("WHERE node_id = ANY($1)"));
        assert!(src.contains("if node_ids.is_empty()"));
    }

    #[test]
    fn every_statement_binds_its_values_instead_of_interpolating_them() {
        // The URL is operator-supplied text that reaches this store directly.
        let src = production_source();
        for builder in ["format!(", "push_str("] {
            assert!(
                !src.contains(builder),
                "SQL may be being built by string concatenation ({builder}); bind the value instead"
            );
        }
    }
}
