// SPDX-License-Identifier: AGPL-3.0-only
//! Long-lived API tokens (PATs) for non-browser clients — the durable credential both the MCP tool
//! surface and the REST API authenticate with (ADR-028, Phase 4).
//!
//! Session tokens ([`crate::auth`]) are login-minted and short-lived, which suits a browser but not
//! an unattended client — an MCP assistant, a CI job, a script — whose stored credential must
//! survive restarts. An **API token** is an admin-issued, durable credential; the auth gates resolve
//! it to the same [`Principal`] a session yields, so authorization goes through the one RBAC path
//! (ADR-014). Issuance and revocation are admin actions, audited via the API's `audit_mw`.
//!
//! ## A token acts as an account
//!
//! It did not always. Originally a token carried its own role and scope and had **no link to
//! `users` at all** — `created_by` was a username string kept for the audit trail, and verification
//! never consulted the accounts table. Deleting, disabling or demoting the issuer changed nothing
//! about the token. That held together while a PAT authenticated `/mcp` alone (read-mostly,
//! default-OFF), but the REST API is the whole configuration surface, and a credential that outlives
//! the account it came from is exactly what an offboarding process cannot see.
//!
//! So a token now names an **owner**, and [`ApiTokenStore::verify`] resolves the two together: a
//! disabled or deleted owner takes its tokens with it, and the effective role is capped at the
//! owner's current role. The owner is usually a **service account**
//! ([`yagra_common::UserKind::Service`]) — a machine identity that cannot sign in — precisely so an
//! integration does not die when a person changes teams.
//!
//! SECURITY (security.md / ADR-018): the raw `yat_…` token is returned to the admin **once** at
//! creation and never stored — only its SHA-256 hex (`token_hash`) is persisted, so a DB dump can't
//! recover a usable credential. Keep it least-privileged: the narrowest role, and only the surfaces
//! it actually needs.

use chrono::{DateTime, Utc};
use data_encoding::BASE64URL_NOPAD;
use rand::RngCore;
use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::{Principal, Role, Scope, TokenSurface, UserKind};

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
    /// Which auth surfaces this token may be presented at. Never empty in practice — a token that
    /// named no surface could authenticate nowhere — but stored as a list so it can grow.
    pub surfaces: Vec<TokenSurface>,
    /// When the token stops authenticating, or `None` for no expiry.
    pub expires_at: Option<DateTime<Utc>>,
    /// The account the token acts as. `None` means the owner was deleted before this column
    /// existed, or could not be matched during the 0057 backfill — such a token no longer
    /// authenticates and is shown so an admin can revoke it deliberately.
    pub owner: Option<String>,
    /// Whether the owner account is currently able to authenticate (enabled). Surfaced so the
    /// listing can explain a token that is live by its own dates yet refused.
    pub owner_active: bool,
    /// The owner's last interactive sign-in, when the owner authenticates through an external IdP.
    /// This is the only signal Yagra has that an SSO account is still live (see [`ApiTokenStore`]),
    /// so the listing shows it; `None` for local and service accounts, where it means nothing.
    pub owner_last_login_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// A verified API token: who it acts as, and what it may do.
///
/// Returned instead of a bare [`Principal`] because a PAT now authenticates the REST API, where two
/// more things matter: **who** to write in the audit log (a token is not a session, so the actor
/// would otherwise be anonymous), and the fact that the principal was *derived* rather than stored.
/// Deliberately narrow: the token's id and its owner's id are both resolved by [`ApiTokenStore::verify`]
/// and both left out, because nothing downstream needs them. A token caller cannot reach any
/// account-scoped endpoint (`api::extract::Caller` refuses one), so there is no handler asking "which
/// account am I". Add a field when something reads it, not before.
#[derive(Debug, Clone)]
pub struct TokenAuth {
    /// The token's human label, used in audit attribution.
    pub token_name: String,
    /// The owning account's username — the other half of the audit actor.
    pub owner_username: String,
    /// Role and scope, with the role **capped at the owner's current role**.
    pub principal: Principal,
}

impl TokenAuth {
    /// How this caller is recorded in the audit log: the account it acts as, and the credential it
    /// used. Both halves matter — the account alone cannot be told from an interactive login, and
    /// the token alone loses who is answerable for it.
    #[must_use]
    pub fn audit_actor(&self) -> String {
        format!("{} (token:{})", self.owner_username, self.token_name)
    }
}

/// PostgreSQL-backed store of API tokens.
pub struct ApiTokenStore {
    pool: PgPool,
    /// How long an **externally-authenticated** owner may go without signing in before their tokens
    /// stop working. See [`ApiTokenStore::verify`] for why this exists at all.
    ///
    /// Applies to every [`UserKind::is_external`] kind — OIDC and LDAP alike. The environment
    /// variable that sets it is still `YAGRA_PAT_OIDC_IDLE_DAYS`: renaming it would break running
    /// deployments for no gain, so the name is historical in the same way `users.oidc_subject` is.
    external_idle: chrono::Duration,
}

impl ApiTokenStore {
    /// New store over the shared metadata pool.
    #[must_use]
    pub fn new(pool: PgPool, external_idle_days: i64) -> Self {
        Self {
            pool,
            external_idle: chrono::Duration::days(external_idle_days.max(1)),
        }
    }

    /// Mint a new token granting `role`/`scope` on `surfaces`, owned by `owner_user_id` and issued
    /// by `created_by` (audited username), labeled `name`, expiring at `expires_at` (`None` = never).
    /// Returns `(id, raw_token)` — the **raw token is shown once and never recoverable**; only its
    /// hash is stored. A duplicate `name` (or the astronomically unlikely hash collision) surfaces as
    /// an error so the caller can map it to a 409.
    #[allow(clippy::too_many_arguments)] // Every field is an independent property of the credential;
                                         // bundling them into a struct used by exactly one caller would move the argument list, not shrink it.
    pub async fn create(
        &self,
        name: &str,
        role: Role,
        scope: &Scope,
        surfaces: &[TokenSurface],
        expires_at: Option<DateTime<Utc>>,
        owner_user_id: Uuid,
        created_by: &str,
    ) -> anyhow::Result<(Uuid, String)> {
        let id = Uuid::new_v4();
        let raw = mint_raw_token();
        let hash = token_hash(&raw);
        let role_key = role.key();
        let scope_json = serde_json::to_value(scope)?;
        let surface_keys: Vec<String> = surfaces.iter().map(|s| s.key().to_owned()).collect();
        sqlx::query(
            "INSERT INTO api_tokens \
             (id, name, token_hash, role, scope, surfaces, expires_at, owner_user_id, created_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(id)
        .bind(name)
        .bind(&hash)
        .bind(role_key)
        .bind(&scope_json)
        .bind(&surface_keys)
        .bind(expires_at)
        .bind(owner_user_id)
        .bind(created_by)
        .execute(&self.pool)
        .await?;
        Ok((id, raw))
    }

    /// Revoke every live token owned by `user_id`, returning how many were revoked.
    ///
    /// Called when an account is deleted or disabled. Disabling is already covered by [`Self::verify`]
    /// (which JOINs `users.enabled`), but revoking makes the effect visible in the listing rather
    /// than leaving a token that looks live and silently is not — and a re-enabled account should
    /// not silently resurrect credentials that were taken away.
    pub async fn revoke_owned_by(&self, user_id: Uuid) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "UPDATE api_tokens SET revoked_at = now() \
             WHERE owner_user_id = $1 AND revoked_at IS NULL",
        )
        .bind(user_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// All tokens (active and revoked), newest first, for the admin listing. No secrets.
    ///
    /// LEFT JOIN, unlike [`Self::verify`]'s INNER: an orphaned token must not authenticate, but it
    /// must still be *visible* — a credential that vanished from the admin's list while remaining a
    /// row in the database is worse than one shown as unusable.
    pub async fn list(&self) -> anyhow::Result<Vec<ApiTokenInfo>> {
        let rows = sqlx::query(
            "SELECT t.id, t.name, t.role, t.scope, t.surfaces, t.expires_at, \
                    t.created_by, t.created_at, t.last_used_at, t.revoked_at, \
                    u.username AS owner_username, u.enabled AS owner_enabled, \
                    u.auth_source, u.last_login_at \
               FROM api_tokens t \
               LEFT JOIN users u ON u.id = t.owner_user_id \
              ORDER BY t.created_at DESC",
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

    /// Resolve a raw bearer token presented at `want` into a [`TokenAuth`], or `None` if it may not
    /// authenticate there.
    ///
    /// One `None` for every reason on purpose: malformed, unknown, revoked, expired, wrong surface,
    /// no owner, disabled owner, or an idle external owner all answer the same `401`. A caller
    /// holding a rejected credential learns nothing about *which* of those it is — the difference is
    /// exactly what someone probing with a stale token would want.
    ///
    /// Four of those are decided by the query itself, so a row that comes back is already live,
    /// unexpired, and owned by an enabled account. The rest are judgements this side of the wire:
    ///
    /// - **Surface.** A token names the surfaces it may be presented at; `/mcp` and `/api/v1` are
    ///   very different powers and a token minted for the first must not acquire the second.
    /// - **Role cap.** The effective role is `min(token role, owner's current role)`, so demoting an
    ///   account narrows its tokens at once. Storing the role on the token and reading it back would
    ///   leave a demoted admin holding an admin credential.
    /// - **Idle external owner.** Yagra cannot observe an IdP or a domain controller disabling an
    ///   account: `upsert_external_user` runs on *successful* login only, so a disabled external
    ///   user's row simply freezes with `enabled = true` and its last-known role. Sessions survive
    ///   that because they expire within a day; a no-expiry token would not. The only signal
    ///   available is that the owner stops signing in — so for a token owned by any
    ///   [`UserKind::is_external`] account (OIDC **or** LDAP), a `last_login_at` older than
    ///   `external_idle` is treated as the account being gone. Local and service accounts are
    ///   exempt: nothing outside Yagra can disagree about them.
    ///
    /// On a hit, `last_used_at` is refreshed (throttled to ~1/min) as a best-effort side effect — a
    /// write failure there never fails the auth. The raw token is never read back; the lookup is by
    /// hash.
    pub async fn verify(&self, raw: &str, want: TokenSurface) -> Option<TokenAuth> {
        if !is_api_token_shape(raw) {
            return None;
        }
        let hash = token_hash(raw);
        // INNER JOIN, so a token whose owner column is NULL — deleted account, or unmatched by the
        // 0057 backfill — resolves to no row at all. Fail-closed by construction rather than by a
        // check someone can forget to write.
        let row = sqlx::query(
            "SELECT t.id, t.name, t.role, t.scope, t.surfaces, \
                    u.id AS owner_id, u.username AS owner_username, u.role AS owner_role, \
                    u.scope AS owner_scope, u.auth_source, u.last_login_at \
               FROM api_tokens t \
               JOIN users u ON u.id = t.owner_user_id \
              WHERE t.token_hash = $1 \
                AND t.revoked_at IS NULL \
                AND (t.expires_at IS NULL OR t.expires_at > now()) \
                AND u.enabled",
        )
        .bind(&hash)
        .fetch_optional(&self.pool)
        .await
        .ok()??;

        let surfaces = row_surfaces(&row).ok()?;
        if !surfaces.contains(&want) {
            return None;
        }

        let owner_kind = UserKind::parse(&row.try_get::<String, _>("auth_source").ok()?);
        if owner_kind.is_external() {
            let last_login: Option<DateTime<Utc>> = row.try_get("last_login_at").ok()?;
            // No recorded login at all is treated as idle: the column is written on every external
            // login, so an account that has never had one cannot be vouched for either.
            if last_login.is_none_or(|t| Utc::now() - t > self.external_idle) {
                return None;
            }
        }

        let principal = row_to_principal(&row).ok()?;
        let owner_role = parse_role(&row.try_get::<String, _>("owner_role").ok()?).ok()?;
        // A token can never exceed its owner — in **either** dimension. The role is capped at the
        // owner's current role, so demoting the account narrows every credential it holds; the
        // scope is capped the same way, so narrowing the account's visibility narrows them too,
        // immediately and without anyone having to remember to re-issue.
        //
        // A scoped owner's scope simply *replaces* the token's rather than being intersected with
        // it, and `create` refuses to mint a non-`All` scope onto a scoped owner so the two can
        // never disagree. Intersecting would need the group tree in here — and would have to get
        // subtrees right, since a token naming a child of one of the owner's roots is inside the
        // owner's scope while sharing none of its ids. Whoever got that wrong would get it wrong in
        // the widening direction. To give a token a *narrower* view than its owner, own it with a
        // service account scoped to what the token should see.
        let owner_scope = owner_scope_of(&row);
        let capped = Principal::new(
            std::cmp::min(principal.role, owner_role),
            match owner_scope {
                Scope::All => principal.scope.clone(),
                narrowed => narrowed,
            },
        );

        // Throttled last-used bump: skip the write unless it's stale, so a busy token doesn't write
        // on every call. Fire-and-forget — auth already succeeded.
        let _ = sqlx::query(
            "UPDATE api_tokens SET last_used_at = now() \
             WHERE token_hash = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '60 seconds')",
        )
        .bind(&hash)
        .execute(&self.pool)
        .await;

        Some(TokenAuth {
            token_name: row.try_get("name").ok()?,
            owner_username: row.try_get("owner_username").ok()?,
            principal: capped,
        })
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

/// The owning account's scope, defaulting to the **empty** scope on an unreadable value.
///
/// Fails closed for the same reason `auth.rs::parse_scope` does — this one caps a credential, so
/// reading a corrupt owner row as `All` would let a storage fault widen a token rather than break
/// it. There is no `?` here on purpose: an error must not fall through to "no cap applied".
fn owner_scope_of(row: &sqlx::postgres::PgRow) -> Scope {
    let empty = || Scope::Groups(std::collections::BTreeSet::new());
    row.try_get::<serde_json::Value, _>("owner_scope")
        .ok()
        .map_or_else(empty, |raw| {
            serde_json::from_value(raw).unwrap_or_else(|_| empty())
        })
}

/// The surfaces a row names, dropping any this build does not recognise (N-1: a token written by a
/// newer core must not widen itself here — an unknown surface is one this core cannot enforce).
fn row_surfaces(row: &sqlx::postgres::PgRow) -> anyhow::Result<Vec<TokenSurface>> {
    let keys: Vec<String> = row.try_get("surfaces")?;
    Ok(keys.iter().filter_map(|k| TokenSurface::parse(k)).collect())
}

/// Map one listing row to its `ApiTokenInfo` (metadata only).
fn row_to_info(row: sqlx::postgres::PgRow) -> anyhow::Result<ApiTokenInfo> {
    let role_key: String = row.try_get("role")?;
    let scope_json: serde_json::Value = row.try_get("scope")?;
    let surfaces = row_surfaces(&row)?;
    let owner: Option<String> = row.try_get("owner_username")?;
    let auth_source: Option<String> = row.try_get("auth_source")?;
    let owner_kind = auth_source.as_deref().map(UserKind::parse);
    Ok(ApiTokenInfo {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        role: parse_role(&role_key)?,
        scope: serde_json::from_value(scope_json)?,
        surfaces,
        expires_at: row.try_get("expires_at")?,
        // An absent owner is not an active one — the LEFT JOIN yields NULL for both the username
        // and the flag, and a NULL `enabled` must read as "cannot authenticate", not as `false`
        // being unknown.
        owner_active: row
            .try_get::<Option<bool>, _>("owner_enabled")?
            .unwrap_or(false),
        owner,
        // Only meaningful for an externally-authenticated owner, which is the only kind whose
        // idleness ends its tokens. Showing it for a service account would invite reading a blank
        // as a problem when it is the normal state.
        owner_last_login_at: if owner_kind.is_some_and(UserKind::is_external) {
            row.try_get("last_login_at")?
        } else {
            None
        },
        created_by: row.try_get("created_by")?,
        created_at: row.try_get("created_at")?,
        last_used_at: row.try_get("last_used_at")?,
        revoked_at: row.try_get("revoked_at")?,
    })
}

/// Parse the stored snake_case role key back into a [`Role`] (the mirror of [`Role::key`]).
/// Derived from [`Role::ALL`] so the token list lives in one place.
fn parse_role(key: &str) -> anyhow::Result<Role> {
    Role::parse(key).ok_or_else(|| anyhow::anyhow!("unknown role key {key:?}"))
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
