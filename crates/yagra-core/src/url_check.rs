//! URL-check persistence (the per-node HTTP/HTTPS monitor configuration).
//!
//! Metadata, so it lives in PostgreSQL (store separation). One row per node (1:1), keyed by
//! `node_id`. This is the I/O adapter; the config type ([`UrlCheckConfig`]) and its validation
//! live in `yagra-common` (tested there). Runtime `sqlx::query` (not the compile-time macro) so
//! the build needs no live database — consistent with [`crate::repo`].

use sqlx::types::Json;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::url_check::{ExpectedStatus, HttpMethod, UrlCheckConfig};
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
                    credential_id \
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
        Ok(Some(UrlCheckConfig {
            url: row.try_get("url")?,
            method: HttpMethod::from_token(&method).unwrap_or_default(),
            expected_status: expected.0,
            verify_tls: row.try_get("verify_tls")?,
            follow_redirects: row.try_get("follow_redirects")?,
            timeout_ms: u32::try_from(timeout_ms).unwrap_or(5000),
            credential: credential_id.map(CredentialId::from),
        }))
    }

    /// Create or replace a node's URL check (idempotent upsert).
    pub async fn upsert(&self, node_id: Uuid, cfg: &UrlCheckConfig) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO url_checks \
                (node_id, url, method, expected_status, verify_tls, follow_redirects, timeout_ms, \
                 credential_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (node_id) DO UPDATE SET \
                url = EXCLUDED.url, method = EXCLUDED.method, \
                expected_status = EXCLUDED.expected_status, verify_tls = EXCLUDED.verify_tls, \
                follow_redirects = EXCLUDED.follow_redirects, timeout_ms = EXCLUDED.timeout_ms, \
                credential_id = EXCLUDED.credential_id, updated_at = now()",
        )
        .bind(node_id)
        .bind(&cfg.url)
        .bind(cfg.method.as_str())
        .bind(Json(&cfg.expected_status))
        .bind(cfg.verify_tls)
        .bind(cfg.follow_redirects)
        .bind(i32::try_from(cfg.timeout_ms).unwrap_or(5000))
        .bind(cfg.credential.map(|c| c.as_uuid()))
        .execute(&self.pool)
        .await?;
        Ok(())
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
