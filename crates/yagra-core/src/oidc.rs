// SPDX-License-Identifier: AGPL-3.0-only
//! External IdP login via OpenID Connect (authorization-code + PKCE, ADR-010 Phase 3).
//!
//! A single operator-configured OIDC provider lets users sign in with the org SSO alongside local
//! accounts. The provider (issuer/client_id + **envelope-encrypted client_secret**, ADR-018) is
//! managed at Settings ▸ Auth. On login the user's IdP `groups` claim is mapped to a Yagra role via
//! the provider's `role_map` (highest match wins; `default_role`, else deny). The account is
//! JIT-provisioned in `users` (`auth_source = 'oidc'`) and a normal [`crate::auth::SessionStore`]
//! session is issued — the session model is authentication-method agnostic.
//!
//! The flow's transient anti-forgery state (CSRF `state`, `nonce`, PKCE verifier) lives in an
//! in-memory [`OidcFlight`] (like `SessionStore`); authorize→callback hit the same core (single
//! origin, and under HA the readiness probe routes to the leader). Sharing it across replicas is
//! the same active/active concern as shared sessions (deferred with HA Increment 2).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use openidconnect::core::{
    CoreAuthDisplay, CoreAuthPrompt, CoreErrorResponseType, CoreGenderClaim, CoreJsonWebKey,
    CoreJweContentEncryptionAlgorithm, CoreJwsSigningAlgorithm, CoreProviderMetadata,
    CoreResponseType, CoreRevocableToken, CoreRevocationErrorResponse,
    CoreTokenIntrospectionResponse, CoreTokenType,
};
use openidconnect::{
    AdditionalClaims, AuthenticationFlow, AuthorizationCode, Client, ClientId, ClientSecret,
    CsrfToken, EmptyExtraTokenFields, EndpointNotSet, IdTokenFields, IssuerUrl, Nonce,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, StandardErrorResponse,
    StandardTokenResponse,
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::Role;
use yagra_secrets::{EnvelopeCipher, SealedSecret};

use crate::secrets::Kek;

/// How long an in-flight authorization (state→nonce/PKCE) is honored before it's pruned.
const FLIGHT_TTL: Duration = Duration::from_secs(600);

// ── Custom ID-token claims: the non-standard `groups` claim ─────────────────────────────────────

/// All non-standard ID-token claims, captured via [`AdditionalClaims`] so the configurable groups
/// claim survives typed deserialization (the core claim set would drop it). The provider's
/// `groups_claim` names which claim holds the groups; a list or a single string is accepted.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct GroupsClaims {
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}
impl AdditionalClaims for GroupsClaims {}

impl GroupsClaims {
    /// Extract the groups from the configured claim (array of strings, or a single string).
    fn groups(&self, claim: &str) -> Vec<String> {
        match self.extra.get(claim) {
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            Some(serde_json::Value::String(s)) => vec![s.clone()],
            _ => Vec::new(),
        }
    }
}

// ── openidconnect client parameterized with our additional claims ───────────────────────────────
// Mirrors `CoreClient` / `CoreIdTokenFields`, substituting `GroupsClaims` for `EmptyAdditionalClaims`
// so the ID token exposes `groups` type-safely.

type GroupsIdTokenFields = IdTokenFields<
    GroupsClaims,
    EmptyExtraTokenFields,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJwsSigningAlgorithm,
>;
type GroupsTokenResponse = StandardTokenResponse<GroupsIdTokenFields, CoreTokenType>;
type GroupsClient<
    HasAuthUrl = EndpointNotSet,
    HasDeviceAuthUrl = EndpointNotSet,
    HasIntrospectionUrl = EndpointNotSet,
    HasRevocationUrl = EndpointNotSet,
    HasTokenUrl = EndpointNotSet,
    HasUserInfoUrl = EndpointNotSet,
> = Client<
    GroupsClaims,
    CoreAuthDisplay,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJsonWebKey,
    CoreAuthPrompt,
    StandardErrorResponse<CoreErrorResponseType>,
    GroupsTokenResponse,
    CoreTokenIntrospectionResponse,
    CoreRevocableToken,
    CoreRevocationErrorResponse,
    HasAuthUrl,
    HasDeviceAuthUrl,
    HasIntrospectionUrl,
    HasRevocationUrl,
    HasTokenUrl,
    HasUserInfoUrl,
>;

// ── Provider config / API shapes ────────────────────────────────────────────────────────────────

/// A fully-resolved provider (client_secret decrypted, role_map/scopes parsed) — used at login.
/// The plaintext `client_secret` lives only in memory and is never logged or returned.
pub struct OidcProviderConfig {
    pub id: Uuid,
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub groups_claim: String,
    pub role_map: HashMap<String, Role>,
    pub default_role: Option<Role>,
}

/// Provider metadata for the admin API — **never** includes the client_secret.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct OidcProviderSummary {
    pub id: Uuid,
    pub name: String,
    pub issuer: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub scopes: String,
    pub groups_claim: String,
    pub role_map: HashMap<String, String>,
    pub default_role: Option<String>,
    pub enabled: bool,
    /// True once a client_secret has been stored (so the UI can show "set" without revealing it).
    pub has_secret: bool,
}

/// Create/update payload from the admin UI. `client_secret` is write-only: `None` on update keeps
/// the stored value (so editing other fields doesn't require re-entering the secret).
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub struct OidcProviderInput {
    pub name: String,
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    #[serde(default = "default_scopes")]
    pub scopes: String,
    #[serde(default = "default_groups_claim")]
    pub groups_claim: String,
    #[serde(default)]
    pub role_map: HashMap<String, String>,
    #[serde(default)]
    pub default_role: Option<String>,
    #[serde(default)]
    pub enabled: bool,
}

fn default_scopes() -> String {
    "openid profile email groups".to_owned()
}
fn default_groups_claim() -> String {
    "groups".to_owned()
}

/// The outcome of a successful OIDC login, resolved from the validated ID token.
pub struct OidcLogin {
    pub subject: String,
    pub username: String,
    pub role: Role,
}

// ── Role mapping ────────────────────────────────────────────────────────────────────────────────

/// Resolve the effective role for a set of directory groups: the **highest** (most privileged) role
/// among the mapped groups, falling back to `default_role`. `None` ⇒ the user has no Yagra role →
/// deny.
///
/// Shared with the LDAP path (ADR-041 decision 2: one group→role mechanism, not two). The lookup is
/// an exact `HashMap::get`, so **whatever normalisation a caller needs is the caller's job** — the
/// OIDC path hands over claim values verbatim, while the LDAP path expands each group DN into its
/// candidate keys and lowercases both sides before it gets here. Doing the folding inside this
/// function would silently change how existing OIDC deployments match their claims.
pub fn resolve_role(
    groups: &[String],
    role_map: &HashMap<String, Role>,
    default_role: Option<Role>,
) -> Option<Role> {
    groups
        .iter()
        .filter_map(|g| role_map.get(g).copied())
        .max()
        .or(default_role)
}

// ── In-memory in-flight authorization store ─────────────────────────────────────────────────────

struct PendingAuth {
    nonce: Nonce,
    verifier: PkceCodeVerifier,
    provider_id: Uuid,
    created: Instant,
}

/// Transient CSRF-state → (nonce, PKCE verifier) map for in-flight OIDC logins. One-shot: a state is
/// consumed on callback and expires after [`FLIGHT_TTL`].
pub struct OidcFlight {
    inner: Mutex<HashMap<String, PendingAuth>>,
}

impl Default for OidcFlight {
    fn default() -> Self {
        Self::new()
    }
}

impl OidcFlight {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    fn insert(&self, state: String, pending: PendingAuth, now: Instant) {
        let mut map = self.inner.lock().expect("oidc flight mutex");
        map.retain(|_, p| now.duration_since(p.created) < FLIGHT_TTL);
        map.insert(state, pending);
    }

    /// Consume the pending authorization for `state` if present and unexpired.
    fn take(&self, state: &str, now: Instant) -> Option<PendingAuth> {
        let mut map = self.inner.lock().expect("oidc flight mutex");
        let pending = map.remove(state)?;
        if now.duration_since(pending.created) < FLIGHT_TTL {
            Some(pending)
        } else {
            None
        }
    }
}

// ── HTTP client + flow orchestration ────────────────────────────────────────────────────────────

/// Build the outbound HTTP client for OIDC calls. `redirect(none)` is required by openidconnect to
/// prevent SSRF via redirects; TLS is the in-tree rustls (workspace reqwest).
fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| anyhow::anyhow!("build OIDC http client: {e}"))
}

async fn discover_metadata(
    cfg: &OidcProviderConfig,
    http: &reqwest::Client,
) -> anyhow::Result<CoreProviderMetadata> {
    Ok(CoreProviderMetadata::discover_async(IssuerUrl::new(cfg.issuer.clone())?, http).await?)
}

/// Begin an OIDC login: discover the provider, build the authorization URL (with PKCE + nonce), and
/// stash the anti-forgery state. Returns the URL the browser should be sent to.
pub async fn begin_authorize(
    cfg: &OidcProviderConfig,
    flight: &OidcFlight,
) -> anyhow::Result<String> {
    let http = http_client()?;
    let metadata = discover_metadata(cfg, &http).await?;
    // Client endpoint markers are inferred from `from_provider_metadata` (naming them is brittle).
    let client = GroupsClient::from_provider_metadata(
        metadata,
        ClientId::new(cfg.client_id.clone()),
        Some(ClientSecret::new(cfg.client_secret.clone())),
    )
    .set_redirect_uri(RedirectUrl::new(cfg.redirect_uri.clone())?);
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let mut req = client
        .authorize_url(
            AuthenticationFlow::<CoreResponseType>::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .set_pkce_challenge(pkce_challenge);
    for scope in &cfg.scopes {
        req = req.add_scope(Scope::new(scope.clone()));
    }
    let (url, csrf, nonce) = req.url();
    flight.insert(
        csrf.secret().clone(),
        PendingAuth {
            nonce,
            verifier: pkce_verifier,
            provider_id: cfg.id,
            created: Instant::now(),
        },
        Instant::now(),
    );
    Ok(url.to_string())
}

/// Complete an OIDC login: validate `state`, exchange `code` (with PKCE), verify the ID token
/// (signature/iss/aud/exp/nonce, done by the crate), and resolve the account + role. `state` is
/// one-shot. Returns an error (denied) if no group maps to a role.
pub async fn complete_callback(
    cfg: &OidcProviderConfig,
    flight: &OidcFlight,
    code: &str,
    state: &str,
) -> anyhow::Result<OidcLogin> {
    let pending = flight
        .take(state, Instant::now())
        .ok_or_else(|| anyhow::anyhow!("unknown or expired authorization state"))?;
    if pending.provider_id != cfg.id {
        anyhow::bail!("authorization state does not match the active provider");
    }
    let http = http_client()?;
    let metadata = discover_metadata(cfg, &http).await?;
    let client = GroupsClient::from_provider_metadata(
        metadata,
        ClientId::new(cfg.client_id.clone()),
        Some(ClientSecret::new(cfg.client_secret.clone())),
    )
    .set_redirect_uri(RedirectUrl::new(cfg.redirect_uri.clone())?);
    let token_response = client
        .exchange_code(AuthorizationCode::new(code.to_owned()))?
        .set_pkce_verifier(pending.verifier)
        .request_async(&http)
        .await?;
    let id_token = token_response
        .extra_fields()
        .id_token()
        .ok_or_else(|| anyhow::anyhow!("provider returned no ID token"))?;
    let claims = id_token.claims(&client.id_token_verifier(), &pending.nonce)?;

    let subject = claims.subject().as_str().to_owned();
    let username = claims
        .preferred_username()
        .map(|u| u.as_str().to_owned())
        .or_else(|| claims.email().map(|e| e.as_str().to_owned()))
        .unwrap_or_else(|| subject.clone());
    let groups = claims.additional_claims().groups(&cfg.groups_claim);

    let role = resolve_role(&groups, &cfg.role_map, cfg.default_role).ok_or_else(|| {
        anyhow::anyhow!("no IdP group maps to a Yagra role (login denied by role_map)")
    })?;
    Ok(OidcLogin {
        subject,
        username,
        role,
    })
}

// ── PostgreSQL-backed provider store ────────────────────────────────────────────────────────────

/// Store for the OIDC provider config. The client_secret is envelope-encrypted at rest (same KEK as
/// credentials/notification channels) and never returned by a read.
pub struct OidcRepo {
    pool: PgPool,
    cipher: EnvelopeCipher<Kek>,
}

impl OidcRepo {
    #[must_use]
    pub fn new(pool: PgPool, kek: Kek) -> Self {
        Self {
            pool,
            cipher: EnvelopeCipher::new(kek),
        }
    }

    fn row_to_summary(row: &sqlx::postgres::PgRow) -> anyhow::Result<OidcProviderSummary> {
        let role_map: serde_json::Value = row.try_get("role_map")?;
        let role_map: HashMap<String, String> =
            serde_json::from_value(role_map).unwrap_or_default();
        Ok(OidcProviderSummary {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            issuer: row.try_get("issuer")?,
            client_id: row.try_get("client_id")?,
            redirect_uri: row.try_get("redirect_uri")?,
            scopes: row.try_get("scopes")?,
            groups_claim: row.try_get("groups_claim")?,
            role_map,
            default_role: row.try_get("default_role")?,
            enabled: row.try_get("enabled")?,
            has_secret: true,
        })
    }

    /// All providers (metadata only — no secret).
    pub async fn list(&self) -> anyhow::Result<Vec<OidcProviderSummary>> {
        let rows = sqlx::query(
            "SELECT id, name, issuer, client_id, redirect_uri, scopes, groups_claim, role_map, \
             default_role, enabled FROM oidc_providers ORDER BY created_at, name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(Self::row_to_summary).collect()
    }

    /// Whether any provider is enabled (drives the WebUI's SSO button + `/config`).
    pub async fn sso_enabled(&self) -> anyhow::Result<bool> {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM oidc_providers WHERE enabled = true")
            .fetch_one(&self.pool)
            .await?;
        Ok(n > 0)
    }

    /// Check the parts of a provider definition the caller can get wrong.
    ///
    /// Public because the API edge calls it *before* [`Self::create`] / [`Self::update`], to keep
    /// the two kinds of failure apart. Everything this returns is about the submitted input and is
    /// safe to show; everything the write path returns afterwards (sealing the secret, the DB) is a
    /// fault the caller cannot act on and must not see (security.md).
    pub fn validate(input: &OidcProviderInput) -> anyhow::Result<()> {
        // Fail fast on inputs the flow needs to be well-formed (validated at the API edge).
        IssuerUrl::new(input.issuer.clone()).map_err(|e| anyhow::anyhow!("invalid issuer: {e}"))?;
        RedirectUrl::new(input.redirect_uri.clone())
            .map_err(|e| anyhow::anyhow!("invalid redirect_uri: {e}"))?;
        for (g, r) in &input.role_map {
            if Role::parse_lenient(r).is_none() {
                anyhow::bail!("role_map[{g}] has unknown role '{r}'");
            }
        }
        if let Some(r) = &input.default_role {
            if Role::parse_lenient(r).is_none() {
                anyhow::bail!("unknown default_role '{r}'");
            }
        }
        Ok(())
    }

    /// Insert a provider. Requires a client_secret (sealed before it touches the DB).
    pub async fn create(&self, input: &OidcProviderInput) -> anyhow::Result<Uuid> {
        Self::validate(input)?;
        let secret = input
            .client_secret
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("client_secret is required"))?;
        let sealed = self
            .cipher
            .seal(secret.as_bytes())
            .map_err(|e| anyhow::anyhow!("seal client_secret: {e}"))?;
        let id = Uuid::new_v4();
        let role_map = serde_json::to_value(&input.role_map)?;
        sqlx::query(
            "INSERT INTO oidc_providers \
             (id, name, issuer, client_id, key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce, \
              redirect_uri, scopes, groups_claim, role_map, default_role, enabled) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
        )
        .bind(id)
        .bind(&input.name)
        .bind(&input.issuer)
        .bind(&input.client_id)
        .bind(i64::from(sealed.key_id))
        .bind(&sealed.wrapped_dek)
        .bind(&sealed.dek_nonce)
        .bind(&sealed.ciphertext)
        .bind(&sealed.ct_nonce)
        .bind(&input.redirect_uri)
        .bind(&input.scopes)
        .bind(&input.groups_claim)
        .bind(role_map)
        .bind(&input.default_role)
        .bind(input.enabled)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Update a provider. `client_secret = None` keeps the stored secret; `Some(non-empty)` re-seals.
    pub async fn update(&self, id: Uuid, input: &OidcProviderInput) -> anyhow::Result<bool> {
        Self::validate(input)?;
        let role_map = serde_json::to_value(&input.role_map)?;
        let changed = if let Some(secret) = input.client_secret.as_deref().filter(|s| !s.is_empty())
        {
            let sealed = self
                .cipher
                .seal(secret.as_bytes())
                .map_err(|e| anyhow::anyhow!("seal client_secret: {e}"))?;
            sqlx::query(
                "UPDATE oidc_providers SET name=$2, issuer=$3, client_id=$4, redirect_uri=$5, \
                 scopes=$6, groups_claim=$7, role_map=$8, default_role=$9, enabled=$10, \
                 key_id=$11, wrapped_dek=$12, dek_nonce=$13, ciphertext=$14, ct_nonce=$15 \
                 WHERE id=$1",
            )
            .bind(id)
            .bind(&input.name)
            .bind(&input.issuer)
            .bind(&input.client_id)
            .bind(&input.redirect_uri)
            .bind(&input.scopes)
            .bind(&input.groups_claim)
            .bind(&role_map)
            .bind(&input.default_role)
            .bind(input.enabled)
            .bind(i64::from(sealed.key_id))
            .bind(&sealed.wrapped_dek)
            .bind(&sealed.dek_nonce)
            .bind(&sealed.ciphertext)
            .bind(&sealed.ct_nonce)
            .execute(&self.pool)
            .await?
        } else {
            sqlx::query(
                "UPDATE oidc_providers SET name=$2, issuer=$3, client_id=$4, redirect_uri=$5, \
                 scopes=$6, groups_claim=$7, role_map=$8, default_role=$9, enabled=$10 WHERE id=$1",
            )
            .bind(id)
            .bind(&input.name)
            .bind(&input.issuer)
            .bind(&input.client_id)
            .bind(&input.redirect_uri)
            .bind(&input.scopes)
            .bind(&input.groups_claim)
            .bind(&role_map)
            .bind(&input.default_role)
            .bind(input.enabled)
            .execute(&self.pool)
            .await?
        };
        Ok(changed.rows_affected() > 0)
    }

    /// Delete a provider. Returns whether a row was removed.
    pub async fn delete(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM oidc_providers WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The enabled provider fully resolved for login (client_secret decrypted, maps parsed). `None`
    /// if no provider is enabled. Unknown roles in `role_map` are skipped (logged), not fatal.
    pub async fn enabled_config(&self) -> anyhow::Result<Option<OidcProviderConfig>> {
        let row = sqlx::query(
            "SELECT id, issuer, client_id, key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce, \
             redirect_uri, scopes, groups_claim, role_map, default_role \
             FROM oidc_providers WHERE enabled = true ORDER BY created_at LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let key_id: i64 = row.try_get("key_id")?;
        let sealed = SealedSecret {
            key_id: u32::try_from(key_id).unwrap_or(0),
            wrapped_dek: row.try_get("wrapped_dek")?,
            dek_nonce: row.try_get("dek_nonce")?,
            ciphertext: row.try_get("ciphertext")?,
            ct_nonce: row.try_get("ct_nonce")?,
        };
        let secret_bytes = self
            .cipher
            .open(&sealed)
            .map_err(|e| anyhow::anyhow!("open client_secret: {e}"))?;
        let client_secret = String::from_utf8(secret_bytes)
            .map_err(|_| anyhow::anyhow!("client_secret is not valid UTF-8"))?;

        let scopes: String = row.try_get("scopes")?;
        let role_map_json: serde_json::Value = row.try_get("role_map")?;
        let role_map_raw: HashMap<String, String> =
            serde_json::from_value(role_map_json).unwrap_or_default();
        let mut role_map = HashMap::new();
        for (group, role) in role_map_raw {
            match Role::parse_lenient(&role) {
                Some(r) => {
                    role_map.insert(group, r);
                }
                None => {
                    tracing::warn!(group = %group, role = %role, "OIDC role_map has unknown role; skipping")
                }
            }
        }
        let default_role = row
            .try_get::<Option<String>, _>("default_role")?
            .and_then(|s| Role::parse_lenient(&s));

        Ok(Some(OidcProviderConfig {
            id: row.try_get("id")?,
            issuer: row.try_get("issuer")?,
            client_id: row.try_get("client_id")?,
            client_secret,
            redirect_uri: row.try_get("redirect_uri")?,
            scopes: scopes.split_whitespace().map(str::to_owned).collect(),
            groups_claim: row.try_get("groups_claim")?,
            role_map,
            default_role,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, Role)]) -> HashMap<String, Role> {
        pairs.iter().map(|(g, r)| ((*g).to_owned(), *r)).collect()
    }

    #[test]
    fn resolve_role_picks_the_highest_mapped_role() {
        let m = map(&[("net-admins", Role::Admin), ("noc", Role::Operator)]);
        let groups = vec!["noc".to_owned(), "net-admins".to_owned()];
        assert_eq!(resolve_role(&groups, &m, None), Some(Role::Admin));
    }

    #[test]
    fn resolve_role_ignores_unmapped_groups() {
        let m = map(&[("noc", Role::Operator)]);
        let groups = vec!["noc".to_owned(), "random".to_owned()];
        assert_eq!(resolve_role(&groups, &m, None), Some(Role::Operator));
    }

    #[test]
    fn resolve_role_falls_back_to_default_then_denies() {
        let m = map(&[("noc", Role::Operator)]);
        let groups = vec!["random".to_owned()];
        assert_eq!(
            resolve_role(&groups, &m, Some(Role::Viewer)),
            Some(Role::Viewer)
        );
        assert_eq!(resolve_role(&groups, &m, None), None);
    }

    // The role *name* an operator types into `role_map` is still matched case-insensitively — that
    // behaviour used to live in a hand-written `role_from_str` here and now comes from
    // `Role::parse_lenient`, which must not have tightened it on the way.
    #[test]
    fn a_typed_role_name_is_still_parsed_case_insensitively() {
        assert_eq!(Role::parse_lenient("viewer"), Some(Role::Viewer));
        assert_eq!(Role::parse_lenient("operator"), Some(Role::Operator));
        assert_eq!(Role::parse_lenient("ADMIN"), Some(Role::Admin));
        assert_eq!(Role::parse_lenient("root"), None);
    }

    // ADR-041 adds case-insensitive, DN-or-CN group matching **for LDAP only**, by normalising the
    // candidate keys before they reach `resolve_role`. The OIDC side hands over claim values
    // verbatim, so its matching must stay exact: an existing deployment whose IdP emits `NetOps`
    // and whose map says `netops` is currently *not* matching, and silently starting to match would
    // hand out a role nobody granted.
    #[test]
    fn oidc_group_matching_is_still_exact_and_case_sensitive() {
        let m = map(&[("NetOps", Role::Admin)]);
        assert_eq!(resolve_role(&["netops".to_owned()], &m, None), None);
        assert_eq!(resolve_role(&["NetOps ".to_owned()], &m, None), None);
        assert_eq!(
            resolve_role(&["NetOps".to_owned()], &m, None),
            Some(Role::Admin)
        );
    }

    #[test]
    fn flight_is_one_shot_and_matches_provider() {
        let flight = OidcFlight::new();
        let now = Instant::now();
        let pid = Uuid::new_v4();
        flight.insert(
            "state-a".to_owned(),
            PendingAuth {
                nonce: Nonce::new("n".to_owned()),
                verifier: PkceCodeVerifier::new("v".to_owned()),
                provider_id: pid,
                created: now,
            },
            now,
        );
        let taken = flight.take("state-a", now).expect("present");
        assert_eq!(taken.provider_id, pid);
        // Second take of the same state yields nothing (one-shot).
        assert!(flight.take("state-a", now).is_none());
    }

    #[test]
    fn flight_expires_after_ttl() {
        let flight = OidcFlight::new();
        let t0 = Instant::now();
        flight.insert(
            "s".to_owned(),
            PendingAuth {
                nonce: Nonce::new("n".to_owned()),
                verifier: PkceCodeVerifier::new("v".to_owned()),
                provider_id: Uuid::new_v4(),
                created: t0,
            },
            t0,
        );
        let later = t0 + FLIGHT_TTL + Duration::from_secs(1);
        assert!(flight.take("s", later).is_none());
    }

    #[test]
    fn groups_claim_accepts_string_or_array_from_configured_claim() {
        // A configurable claim name ("roles" here), as an array…
        let many: GroupsClaims = serde_json::from_str(r#"{"roles":["a","b"]}"#).unwrap();
        assert_eq!(many.groups("roles"), vec!["a".to_owned(), "b".to_owned()]);
        // …or a single string…
        let one: GroupsClaims = serde_json::from_str(r#"{"groups":"noc"}"#).unwrap();
        assert_eq!(one.groups("groups"), vec!["noc".to_owned()]);
        // …or absent.
        let none: GroupsClaims = serde_json::from_str(r#"{"sub":"x"}"#).unwrap();
        assert!(none.groups("groups").is_empty());
    }
}
