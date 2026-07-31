// SPDX-License-Identifier: AGPL-3.0-only
//! Long-lived API tokens (PATs) for non-browser clients — the durable credential the MCP tool
//! surface authenticates with (ADR-028, Phase 4).
//!
//! Session tokens ([`crate::auth`]) are login-minted and short-lived, which suits a browser but not
//! an unattended MCP client (Claude Code/Desktop) whose stored credential must survive restarts. An
//! **API token** is an admin-issued, durable credential carrying a [`Role`] + [`Scope`]; the MCP
//! auth gate resolves it to the same [`Principal`] a session yields, so authorization goes through
//! the one RBAC path (ADR-014). Issuance and revocation are admin actions, audited via the API's
//! `audit_mw`.
//!
//! SECURITY (security.md / ADR-018): the raw `yat_…` token is returned to the admin **once** at
//! creation and never stored — only its SHA-256 hex (`token_hash`) is persisted, so a DB dump can't
//! recover a usable credential. The token grants exactly its `role`/`scope`; keep it least-privileged
//! (Viewer + a group scope) for a read-only MCP client.

use chrono::{DateTime, Utc};
use data_encoding::BASE64URL_NOPAD;
use rand::RngCore;
use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::{Principal, Role, Scope};

use crate::token::token_hash;

/// Prefix that distinguishes an API token from a session token (`y2.` signed / 64-hex opaque). The
/// MCP auth gate routes on it: `yat_…` → this store, otherwise → the [`crate::auth::SessionStore`].
pub const TOKEN_PREFIX: &str = "yat_";

/// Bytes of entropy in a token secret (256-bit). Base64url-encoded after the prefix.
const TOKEN_ENTROPY_BYTES: usize = 32;

/// Whether a bearer token has the API-token shape (`yat_` prefix). Cheap routing check used by the
/// MCP auth gate before touching the database.
#[must_use]
pub fn is_api_token_shape(token: &str) -> bool {
    token.starts_with(TOKEN_PREFIX)
}

/// One API token's metadata for the admin listing — **never** the raw token or its hash
/// (security.md: a credential is never returned in an API response after issuance).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ApiTokenInfo {
    pub id: Uuid,
    pub name: String,
    pub role: Role,
    pub scope: Scope,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// PostgreSQL-backed store of API tokens.
pub struct ApiTokenStore {
    pool: PgPool,
}

impl ApiTokenStore {
    /// New store over the shared metadata pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Mint a new token granting `role`/`scope`, issued by `created_by` (audited username), labeled
    /// `name`. Returns `(id, raw_token)` — the **raw token is shown once and never recoverable**; only
    /// its hash is stored. A duplicate `name` (or the astronomically unlikely hash collision) surfaces
    /// as an error so the caller can map it to a 409.
    pub async fn create(
        &self,
        name: &str,
        role: Role,
        scope: &Scope,
        created_by: &str,
    ) -> anyhow::Result<(Uuid, String)> {
        let id = Uuid::new_v4();
        let raw = mint_raw_token();
        let hash = token_hash(&raw);
        let role_key = role.key();
        let scope_json = serde_json::to_value(scope)?;
        sqlx::query(
            "INSERT INTO api_tokens (id, name, token_hash, role, scope, created_by) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id)
        .bind(name)
        .bind(&hash)
        .bind(role_key)
        .bind(&scope_json)
        .bind(created_by)
        .execute(&self.pool)
        .await?;
        Ok((id, raw))
    }

    /// All tokens (active and revoked), newest first, for the admin listing. No secrets.
    pub async fn list(&self) -> anyhow::Result<Vec<ApiTokenInfo>> {
        let rows = sqlx::query(
            "SELECT id, name, role, scope, created_by, created_at, last_used_at, revoked_at \
             FROM api_tokens ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_info).collect()
    }

    /// Soft-revoke a token by id (idempotent): sets `revoked_at` if it was still active. Returns
    /// `true` when a live token was revoked, `false` when the id is unknown or already revoked.
    pub async fn revoke(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE api_tokens SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Resolve a raw bearer token to its [`Principal`], or `None` if the token is malformed, unknown,
    /// or revoked. On a hit, `last_used_at` is refreshed (throttled to ~1/min) as a best-effort side
    /// effect — a write failure there never fails the auth. Reads never touch the raw token again; the
    /// lookup is by hash among non-revoked rows.
    pub async fn verify(&self, raw: &str) -> Option<Principal> {
        if !is_api_token_shape(raw) {
            return None;
        }
        let hash = token_hash(raw);
        let row = sqlx::query(
            "SELECT role, scope FROM api_tokens WHERE token_hash = $1 AND revoked_at IS NULL",
        )
        .bind(&hash)
        .fetch_optional(&self.pool)
        .await
        .ok()??;
        let principal = row_to_principal(&row).ok()?;
        // Throttled last-used bump: skip the write unless it's stale, so a busy token doesn't write
        // on every call. Fire-and-forget — auth already succeeded.
        let _ = sqlx::query(
            "UPDATE api_tokens SET last_used_at = now() \
             WHERE token_hash = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '60 seconds')",
        )
        .bind(&hash)
        .execute(&self.pool)
        .await;
        Some(principal)
    }
}

/// Generate a raw `yat_<base64url(32 random bytes)>` token from the OS CSPRNG.
fn mint_raw_token() -> String {
    let mut bytes = [0u8; TOKEN_ENTROPY_BYTES];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("{TOKEN_PREFIX}{}", BASE64URL_NOPAD.encode(&bytes))
}

/// Parse a `role` (snake_case key) + `scope` (JSONB) pair from a row into a [`Principal`].
fn row_to_principal(row: &sqlx::postgres::PgRow) -> anyhow::Result<Principal> {
    let role_key: String = row.try_get("role")?;
    let role = parse_role(&role_key)?;
    let scope_json: serde_json::Value = row.try_get("scope")?;
    let scope: Scope = serde_json::from_value(scope_json)?;
    Ok(Principal::new(role, scope))
}

/// Map one listing row to its `ApiTokenInfo` (metadata only).
fn row_to_info(row: sqlx::postgres::PgRow) -> anyhow::Result<ApiTokenInfo> {
    let role_key: String = row.try_get("role")?;
    let scope_json: serde_json::Value = row.try_get("scope")?;
    Ok(ApiTokenInfo {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        role: parse_role(&role_key)?,
        scope: serde_json::from_value(scope_json)?,
        created_by: row.try_get("created_by")?,
        created_at: row.try_get("created_at")?,
        last_used_at: row.try_get("last_used_at")?,
        revoked_at: row.try_get("revoked_at")?,
    })
}

/// Parse the stored snake_case role key back into a [`Role`] (the mirror of [`Role::key`]).
fn parse_role(key: &str) -> anyhow::Result<Role> {
    match key {
        "viewer" => Ok(Role::Viewer),
        "operator" => Ok(Role::Operator),
        "admin" => Ok(Role::Admin),
        other => anyhow::bail!("unknown role key {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_token_has_prefix_and_entropy() {
        let t = mint_raw_token();
        assert!(is_api_token_shape(&t));
        // yat_ + base64url(32 bytes) → prefix + 43 chars (no padding).
        assert_eq!(t.len(), TOKEN_PREFIX.len() + 43);
        // Two mints must differ (CSPRNG, not a constant).
        assert_ne!(mint_raw_token(), mint_raw_token());
    }

    #[test]
    fn non_api_shape_is_rejected_by_routing() {
        assert!(!is_api_token_shape("y2.abc.def")); // signed session token
        assert!(!is_api_token_shape("deadbeef")); // opaque session token
        assert!(is_api_token_shape("yat_whatever"));
    }

    #[test]
    fn role_key_roundtrips() {
        for role in Role::ALL {
            assert_eq!(parse_role(role.key()).unwrap(), role);
        }
        assert!(parse_role("root").is_err());
    }
}
