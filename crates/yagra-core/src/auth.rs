//! Local authentication + RBAC enforcement (Workstream E).
//!
//! Passwords are Argon2id-verified ([`yagra_secrets::password`]); a successful login mints
//! an opaque bearer token held in an in-memory [`SessionStore`] mapped to a [`Principal`]
//! (role + scope). Mutating API endpoints call [`SessionStore::authorize`] with the
//! required [`Permission`]. Read endpoints require `View` by default, but can be opened
//! to anonymous access via `YAGRA_PUBLIC_DASHBOARD` (a public read-only dashboard);
//! group-scope filtering of reads is Phase 2 (ADR-014). Tokens are process-local (lost on restart), which is
//! acceptable for the single-core MVP; shared/persistent sessions come with HA.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::{Permission, Principal, Role, Scope};
use yagra_secrets::password::{hash_password, verify_password};

/// Idle session lifetime: a token unused for this long is expired and purged on next touch.
/// Sliding window — each authorized request refreshes it — so an active operator stays logged in
/// while an abandoned/stolen token dies on its own.
const SESSION_IDLE_TTL: Duration = Duration::from_secs(8 * 60 * 60);
/// Absolute session lifetime: an eternally-refreshed token is still forced to re-authenticate
/// after this long, bounding the blast radius of a leaked token even under continuous use.
const SESSION_ABSOLUTE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Why authorization failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    /// No bearer token supplied.
    Missing,
    /// Token not recognized (unknown/expired).
    Invalid,
    /// Authenticated but lacks the required permission.
    Forbidden,
}

/// An authenticated session: the principal, the account name it was minted for (the audit log
/// records *who*, not just which role), and the user id (so a session can be revoked when that
/// account is disabled, demoted, password-reset, or deleted). `issued_at`/`last_seen` back the
/// absolute + idle expiry.
#[derive(Debug, Clone)]
pub struct Session {
    pub principal: Principal,
    pub username: String,
    pub user_id: Uuid,
    issued_at: Instant,
    last_seen: Instant,
}

/// In-memory bearer-token → session store.
pub struct SessionStore {
    sessions: Mutex<HashMap<String, Session>>,
}

impl SessionStore {
    /// New empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Mint a fresh opaque token for `principal` (account `user_id`, logged in as `username`).
    pub fn issue(&self, user_id: Uuid, principal: Principal, username: &str) -> String {
        let token = random_token();
        let now = Instant::now();
        self.sessions
            .lock()
            .expect("sessions mutex poisoned")
            .insert(
                token.clone(),
                Session {
                    principal,
                    username: username.to_owned(),
                    user_id,
                    issued_at: now,
                    last_seen: now,
                },
            );
        token
    }

    /// Resolve a token to its session, enforcing the idle + absolute TTL. An expired token is
    /// purged and treated as a miss; a live token has its idle window refreshed (sliding).
    #[must_use]
    pub fn lookup(&self, token: &str) -> Option<Session> {
        let now = Instant::now();
        let mut map = self.sessions.lock().expect("sessions mutex poisoned");
        let s = map.get(token)?;
        let expired = now.duration_since(s.issued_at) > SESSION_ABSOLUTE_TTL
            || now.duration_since(s.last_seen) > SESSION_IDLE_TTL;
        if expired {
            map.remove(token);
            return None;
        }
        let session = map.get_mut(token).expect("token present after live check");
        session.last_seen = now;
        Some(session.clone())
    }

    /// Revoke a single token (server-side logout). No-op if unknown.
    pub fn revoke_token(&self, token: &str) {
        self.sessions
            .lock()
            .expect("sessions mutex poisoned")
            .remove(token);
    }

    /// Revoke every session belonging to `user_id` — called when the account is disabled,
    /// demoted, password-reset, or deleted, so those admin controls take effect on already-issued
    /// tokens instead of only on future logins. Returns how many sessions were dropped.
    pub fn revoke_user(&self, user_id: Uuid) -> usize {
        let mut map = self.sessions.lock().expect("sessions mutex poisoned");
        let before = map.len();
        map.retain(|_, s| s.user_id != user_id);
        before - map.len()
    }

    /// Authorize a request: require a valid token whose principal has `perm`.
    pub fn authorize(&self, bearer: Option<&str>, perm: Permission) -> Result<Session, AuthError> {
        let token = bearer.ok_or(AuthError::Missing)?;
        // Reject anything that isn't a well-formed token before touching the session map — parse
        // at the edge, don't feed arbitrary header bytes into the lookup.
        if !is_well_formed_token(token) {
            return Err(AuthError::Invalid);
        }
        let session = self.lookup(token).ok_or(AuthError::Invalid)?;
        if session.principal.can(perm) {
            Ok(session)
        } else {
            Err(AuthError::Forbidden)
        }
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Failed logins tolerated per account before the exponential lockout engages.
const LOGIN_FREE_ATTEMPTS: u32 = 5;
/// First lockout after the free attempts are spent; doubles per additional failure.
const LOGIN_BASE_LOCK: Duration = Duration::from_secs(2);
/// Ceiling on the per-account lockout so a legitimate user isn't shut out forever.
const LOGIN_MAX_LOCK: Duration = Duration::from_secs(15 * 60);
/// Forget an account's failure record after this much inactivity (memory bound).
const LOGIN_KEY_IDLE: Duration = Duration::from_secs(60 * 60);
/// Global failed/attempted-login token bucket: burst capacity and steady refill rate (per second).
/// Bounds total login throughput across all accounts, capping the Argon2id CPU cost an attacker can
/// impose regardless of how they spread usernames. Generous for humans, ruinous for a guessing run.
const LOGIN_GLOBAL_BURST: f64 = 20.0;
const LOGIN_GLOBAL_REFILL_PER_SEC: f64 = 10.0;

/// Why a login was refused before credentials were even checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottleReject {
    /// Suggested seconds to wait before retrying (for a `Retry-After` header).
    pub retry_after_secs: u64,
}

#[derive(Debug)]
struct Attempts {
    failures: u32,
    locked_until: Option<Instant>,
    last: Instant,
}

#[derive(Debug)]
struct GlobalBucket {
    tokens: f64,
    last_refill: Instant,
}

/// Brute-force guard for `POST /auth/login`. Two independent limits: a per-account exponential
/// lockout (stops targeted password guessing against one account) and a global token bucket
/// (stops username-spraying from exhausting CPU via Argon2id). Keyed by the *submitted* username
/// so unknown names are tracked too — the throttle never reveals whether an account exists.
pub struct LoginThrottle {
    keys: Mutex<HashMap<String, Attempts>>,
    global: Mutex<GlobalBucket>,
}

impl LoginThrottle {
    /// New throttle with a full global bucket.
    #[must_use]
    pub fn new() -> Self {
        Self {
            keys: Mutex::new(HashMap::new()),
            global: Mutex::new(GlobalBucket {
                tokens: LOGIN_GLOBAL_BURST,
                last_refill: Instant::now(),
            }),
        }
    }

    /// Case-folded key for an account name (so `Admin`/`admin` share a lockout).
    fn key_of(username: &str) -> String {
        username.trim().to_ascii_lowercase()
    }

    /// Check whether an attempt for `username` may proceed. Consumes one global token and inspects
    /// the per-account lockout; on refusal returns the suggested retry delay. Call once per attempt,
    /// before verifying the password.
    ///
    /// # Errors
    /// Returns [`ThrottleReject`] when the account is in its lockout window or the global rate cap
    /// is exhausted.
    pub fn check(&self, username: &str) -> Result<(), ThrottleReject> {
        let now = Instant::now();
        // Per-account lockout first (most specific, and doesn't burn a global token when locked).
        {
            let keys = self.keys.lock().expect("login throttle keys poisoned");
            if let Some(a) = keys.get(&Self::key_of(username)) {
                if let Some(until) = a.locked_until {
                    if now < until {
                        return Err(ThrottleReject {
                            retry_after_secs: until.duration_since(now).as_secs().max(1),
                        });
                    }
                }
            }
        }
        // Global token bucket (CPU-exhaustion cap).
        let mut g = self.global.lock().expect("login throttle global poisoned");
        let elapsed = now.duration_since(g.last_refill).as_secs_f64();
        g.tokens = (g.tokens + elapsed * LOGIN_GLOBAL_REFILL_PER_SEC).min(LOGIN_GLOBAL_BURST);
        g.last_refill = now;
        if g.tokens < 1.0 {
            return Err(ThrottleReject {
                // Time until one token refills.
                retry_after_secs: (1.0 / LOGIN_GLOBAL_REFILL_PER_SEC).ceil() as u64,
            });
        }
        g.tokens -= 1.0;
        Ok(())
    }

    /// Record a failed login for `username`, arming/extending the exponential lockout.
    pub fn record_failure(&self, username: &str) {
        let now = Instant::now();
        let mut keys = self.keys.lock().expect("login throttle keys poisoned");
        // Opportunistic prune of stale entries so the map can't grow without bound.
        keys.retain(|_, a| {
            now.duration_since(a.last) < LOGIN_KEY_IDLE || a.locked_until.is_some_and(|u| u > now)
        });
        let entry = keys.entry(Self::key_of(username)).or_insert(Attempts {
            failures: 0,
            locked_until: None,
            last: now,
        });
        entry.failures += 1;
        entry.last = now;
        if entry.failures > LOGIN_FREE_ATTEMPTS {
            let steps = entry.failures - LOGIN_FREE_ATTEMPTS - 1;
            let lock = LOGIN_BASE_LOCK
                .saturating_mul(1u32.checked_shl(steps).unwrap_or(u32::MAX))
                .min(LOGIN_MAX_LOCK);
            entry.locked_until = Some(now + lock);
        }
    }

    /// Clear an account's failure record on a successful login.
    pub fn record_success(&self, username: &str) {
        self.keys
            .lock()
            .expect("login throttle keys poisoned")
            .remove(&Self::key_of(username));
    }
}

impl Default for LoginThrottle {
    fn default() -> Self {
        Self::new()
    }
}

/// A session token is exactly 64 lowercase-hex chars (32 bytes). Cheap shape check at the auth
/// edge so malformed `Authorization` headers are rejected before the session-map lookup.
fn is_well_formed_token(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 32 random bytes, hex-encoded — an opaque, unguessable session token.
fn random_token() -> String {
    let bytes: [u8; 32] = rand::random();
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// A random one-time bootstrap password (12 bytes / 24 hex chars ≈ 96 bits) used to seed the
/// first `admin` account when no `YAGRA_ADMIN_PASSWORD` is supplied — so the instance never boots
/// with a well-known default credential. Strong and unguessable; surfaced to the operator once.
#[must_use]
pub fn generate_bootstrap_password() -> String {
    let bytes: [u8; 12] = rand::random();
    let mut s = String::with_capacity(24);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Placeholder stored in `users.password_hash` for OIDC accounts (which have no local password).
/// Deliberately not a valid Argon2 PHC string, so it can never verify; combined with the
/// `auth_source = 'oidc'` guard in [`UserStore::verify`], an OIDC user can never local-login. Keeping
/// the column NOT NULL (rather than nullable) keeps a rolling upgrade N-1 safe (ADR-017): an old
/// binary still reads a valid `String` it simply can't verify against.
pub const OIDC_PASSWORD_SENTINEL: &str = "!oidc-no-local-login";

/// The stored text form of a role (matches [`parse_role`]).
fn role_text(role: Role) -> &'static str {
    match role {
        Role::Admin => "admin",
        Role::Operator => "operator",
        Role::Viewer => "viewer",
    }
}

/// Parse a stored role string (defaults to the least-privileged role on garbage).
fn parse_role(s: &str) -> Role {
    match s {
        "admin" => Role::Admin,
        "operator" => Role::Operator,
        _ => Role::Viewer,
    }
}

/// User-account metadata for the API — never includes the password hash.
#[derive(Debug, Clone, Serialize)]
pub struct UserSummary {
    pub id: Uuid,
    pub username: String,
    pub role: String,
    /// Account creation time (RFC 3339 text; no chrono types cross the API edge).
    pub created_at: String,
    /// Most recent successful login (RFC 3339 text), or `None` if the account has never
    /// logged in.
    pub last_login_at: Option<String>,
    /// Account status: a disabled account is retained for the audit trail but cannot
    /// authenticate (defaults to `true` for accounts created before this column existed).
    pub enabled: bool,
    /// How the account authenticates: `"local"` (password) or `"oidc"` (external IdP).
    pub auth_source: String,
}

/// Outcome of creating a user — a duplicate username is a normal 409, not a 500.
pub enum UserCreateOutcome {
    /// Created with this id.
    Created(Uuid),
    /// The username already exists.
    UsernameTaken,
}

/// Outcome of a user mutation (delete / role change) that must not lock out the last admin.
pub enum UserMutation {
    /// Applied.
    Done,
    /// No such user.
    NotFound,
    /// Refused: this is the only remaining admin (removing/demoting it would orphan the system).
    LastAdmin,
}

/// PostgreSQL-backed user accounts for local auth.
pub struct UserStore {
    pool: PgPool,
}

impl UserStore {
    /// New store over the metadata pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Seed a default `admin` account if the users table is empty. The password is hashed
    /// (never stored or logged in plaintext). Returns `true` if it actually seeded a new admin,
    /// `false` if one already existed — the caller uses this to announce a generated bootstrap
    /// password exactly once (and only when it was used).
    pub async fn ensure_default_admin(&self, password: &str) -> anyhow::Result<bool> {
        let count: i64 = sqlx::query("SELECT count(*) AS n FROM users")
            .fetch_one(&self.pool)
            .await?
            .try_get("n")?;
        if count > 0 {
            return Ok(false);
        }
        let hash = hash_password(password).map_err(|e| anyhow::anyhow!("hash password: {e}"))?;
        sqlx::query("INSERT INTO users (id, username, password_hash, role) VALUES ($1,$2,$3,$4)")
            .bind(Uuid::new_v4())
            .bind("admin")
            .bind(hash)
            .bind("admin")
            .execute(&self.pool)
            .await?;
        Ok(true)
    }

    /// Verify a username/password and return the account id + principal on success. The id lets
    /// the caller bind the session to the account so it can later be revoked.
    pub async fn verify(
        &self,
        username: &str,
        password: &str,
    ) -> anyhow::Result<Option<(Uuid, Principal)>> {
        let row = sqlx::query(
            "SELECT id, password_hash, role, enabled, auth_source FROM users WHERE username = $1",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        // A disabled account cannot authenticate. Treat it like a credential miss so we don't
        // leak account state to an unauthenticated caller.
        let enabled: bool = row.try_get("enabled")?;
        if !enabled {
            return Ok(None);
        }
        // OIDC accounts have no local password — they must sign in via the IdP. Reject before the
        // password check (their stored hash is a non-verifiable sentinel anyway).
        let auth_source: String = row.try_get("auth_source")?;
        if auth_source == "oidc" {
            return Ok(None);
        }
        let hash: String = row.try_get("password_hash")?;
        let role: String = row.try_get("role")?;
        let ok = verify_password(password, &hash).unwrap_or(false);
        if ok {
            let id: Uuid = row.try_get("id")?;
            // Record the successful login time. Best-effort metadata: a failure here must not
            // block an otherwise-valid login.
            let touch = sqlx::query("UPDATE users SET last_login_at = now() WHERE username = $1")
                .bind(username)
                .execute(&self.pool)
                .await;
            if let Err(e) = touch {
                tracing::warn!(error = %e, "failed to record last_login_at");
            }
            // MVP: every account has unrestricted scope; group-scope filtering is Phase 2.
            Ok(Some((id, Principal::new(parse_role(&role), Scope::All))))
        } else {
            Ok(None)
        }
    }

    /// All accounts (metadata only — the password hash is never selected or returned).
    pub async fn list(&self) -> anyhow::Result<Vec<UserSummary>> {
        let rows = sqlx::query(
            "SELECT id, username, role, created_at::text AS created_at, \
             last_login_at::text AS last_login_at, enabled, auth_source \
             FROM users ORDER BY created_at, username",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(UserSummary {
                    id: row.try_get("id")?,
                    username: row.try_get("username")?,
                    role: row.try_get("role")?,
                    created_at: row.try_get("created_at")?,
                    last_login_at: row.try_get("last_login_at")?,
                    enabled: row.try_get("enabled")?,
                    auth_source: row.try_get("auth_source")?,
                })
            })
            .collect()
    }

    /// Create an account. The password is Argon2id-hashed before it touches the database and
    /// is never logged. A duplicate username surfaces as [`UserCreateOutcome::UsernameTaken`]
    /// (the `users.username` UNIQUE constraint), not an opaque 500.
    pub async fn create(
        &self,
        username: &str,
        password: &str,
        role: &str,
    ) -> anyhow::Result<UserCreateOutcome> {
        let hash = hash_password(password).map_err(|e| anyhow::anyhow!("hash password: {e}"))?;
        let id = Uuid::new_v4();
        let res = sqlx::query(
            "INSERT INTO users (id, username, password_hash, role) VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(username)
        .bind(hash)
        .bind(role)
        .execute(&self.pool)
        .await;
        match res {
            Ok(_) => Ok(UserCreateOutcome::Created(id)),
            // 23505 = unique_violation (PostgreSQL) — the username is taken.
            Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("23505") => {
                Ok(UserCreateOutcome::UsernameTaken)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Look up (or JIT-provision) the local account for a validated OIDC identity, returning its
    /// `(id, principal)`. Keyed on `(provider, subject)` so a renamed user keeps one account; the
    /// role is refreshed from the IdP on every login (the IdP is authoritative). A new account stores
    /// the non-verifiable [`OIDC_PASSWORD_SENTINEL`] and `auth_source = 'oidc'`; a colliding username
    /// is disambiguated with a short subject suffix rather than hijacking an existing account.
    pub async fn upsert_oidc_user(
        &self,
        provider_id: Uuid,
        subject: &str,
        username: &str,
        role: Role,
    ) -> anyhow::Result<(Uuid, Principal)> {
        let role_str = role_text(role);
        // Existing identity → refresh role + last_login.
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM users WHERE oidc_provider_id = $1 AND oidc_subject = $2",
        )
        .bind(provider_id)
        .bind(subject)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(id) = existing {
            sqlx::query("UPDATE users SET role = $2, last_login_at = now() WHERE id = $1")
                .bind(id)
                .bind(role_str)
                .execute(&self.pool)
                .await?;
            return Ok((id, Principal::new(role, Scope::All)));
        }
        // New identity → JIT-provision. Disambiguate a colliding username.
        let taken: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE username = $1)")
                .bind(username)
                .fetch_one(&self.pool)
                .await?;
        let uname = if taken {
            let suffix: String = subject.chars().take(8).collect();
            format!("{username} ({suffix})")
        } else {
            username.to_owned()
        };
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO users \
             (id, username, password_hash, role, auth_source, oidc_subject, oidc_provider_id, last_login_at) \
             VALUES ($1, $2, $3, $4, 'oidc', $5, $6, now())",
        )
        .bind(id)
        .bind(&uname)
        .bind(OIDC_PASSWORD_SENTINEL)
        .bind(role_str)
        .bind(subject)
        .bind(provider_id)
        .execute(&self.pool)
        .await?;
        Ok((id, Principal::new(role, Scope::All)))
    }

    /// Delete an account, refusing to remove the **last** admin (which would lock everyone out
    /// of user/credential/config management). Runs in a transaction so the count and delete are
    /// consistent under concurrent edits.
    pub async fn delete(&self, id: Uuid) -> anyhow::Result<UserMutation> {
        let mut tx = self.pool.begin().await?;
        let Some(row) = sqlx::query("SELECT role FROM users WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Ok(UserMutation::NotFound);
        };
        let role: String = row.try_get("role")?;
        if role == "admin" && admin_count(&mut tx).await? <= 1 {
            return Ok(UserMutation::LastAdmin);
        }
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(UserMutation::Done)
    }

    /// Change an account's role, refusing to demote the **last** admin (same lock-out guard as
    /// [`Self::delete`]).
    pub async fn set_role(&self, id: Uuid, new_role: &str) -> anyhow::Result<UserMutation> {
        let mut tx = self.pool.begin().await?;
        let Some(row) = sqlx::query("SELECT role FROM users WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Ok(UserMutation::NotFound);
        };
        let current: String = row.try_get("role")?;
        if current == "admin" && new_role != "admin" && admin_count(&mut tx).await? <= 1 {
            return Ok(UserMutation::LastAdmin);
        }
        sqlx::query("UPDATE users SET role = $2 WHERE id = $1")
            .bind(id)
            .bind(new_role)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(UserMutation::Done)
    }

    /// Enable or disable an account, refusing to disable the **last** account that can still
    /// administer the system (the only enabled admin) — same lock-out guard as
    /// [`Self::set_role`]/[`Self::delete`]. Enabling is always allowed.
    pub async fn set_enabled(&self, id: Uuid, enabled: bool) -> anyhow::Result<UserMutation> {
        let mut tx = self.pool.begin().await?;
        let Some(row) = sqlx::query("SELECT role, enabled FROM users WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Ok(UserMutation::NotFound);
        };
        let role: String = row.try_get("role")?;
        let currently_enabled: bool = row.try_get("enabled")?;
        // Disabling the only remaining enabled admin would lock everyone out of management.
        if !enabled
            && currently_enabled
            && role == "admin"
            && enabled_admin_count(&mut tx).await? <= 1
        {
            return Ok(UserMutation::LastAdmin);
        }
        sqlx::query("UPDATE users SET enabled = $2 WHERE id = $1")
            .bind(id)
            .bind(enabled)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(UserMutation::Done)
    }

    /// Reset an account's password (Argon2id-hashed; the plaintext is never stored or logged).
    /// Returns whether the account exists.
    pub async fn set_password(&self, id: Uuid, password: &str) -> anyhow::Result<bool> {
        let hash = hash_password(password).map_err(|e| anyhow::anyhow!("hash password: {e}"))?;
        let res = sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
            .bind(id)
            .bind(hash)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

/// Count of admin accounts within an open transaction (lock-out guard helper).
async fn admin_count(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> anyhow::Result<i64> {
    let n: i64 = sqlx::query("SELECT count(*) AS n FROM users WHERE role = 'admin'")
        .fetch_one(&mut **tx)
        .await?
        .try_get("n")?;
    Ok(n)
}

/// Count of admins that can still authenticate (enabled) — the guard for disabling an account,
/// since a disabled admin can't log in to manage the system.
async fn enabled_admin_count(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> anyhow::Result<i64> {
    let n: i64 = sqlx::query("SELECT count(*) AS n FROM users WHERE role = 'admin' AND enabled")
        .fetch_one(&mut **tx)
        .await?
        .try_get("n")?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issued_token_resolves_and_authorizes_by_role() {
        let store = SessionStore::new();
        let uid = Uuid::new_v4();
        let token = store.issue(uid, Principal::new(Role::Operator, Scope::All), "op1");

        // Operator can ack alerts but not manage config; the session carries the username + id.
        let session = store
            .authorize(Some(&token), Permission::AckAlerts)
            .expect("operator can ack");
        assert_eq!(session.username, "op1");
        assert_eq!(session.user_id, uid);
        assert!(matches!(
            store.authorize(Some(&token), Permission::ManageConfig),
            Err(AuthError::Forbidden)
        ));
    }

    #[test]
    fn revoking_a_user_invalidates_that_users_tokens_only() {
        let store = SessionStore::new();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        let a1 = store.issue(alice, Principal::new(Role::Operator, Scope::All), "alice");
        let a2 = store.issue(alice, Principal::new(Role::Operator, Scope::All), "alice");
        let b1 = store.issue(bob, Principal::new(Role::Viewer, Scope::All), "bob");

        // Disabling/deleting alice drops both of her sessions, leaving bob's intact.
        assert_eq!(store.revoke_user(alice), 2);
        assert!(store.lookup(&a1).is_none());
        assert!(store.lookup(&a2).is_none());
        assert!(store.lookup(&b1).is_some());
    }

    #[test]
    fn revoke_token_is_a_server_side_logout() {
        let store = SessionStore::new();
        let token = store.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "v1",
        );
        assert!(store.lookup(&token).is_some());
        store.revoke_token(&token);
        assert!(store.lookup(&token).is_none());
        // Revoking an unknown token is a harmless no-op.
        store.revoke_token("does-not-exist");
    }

    #[test]
    fn missing_and_invalid_tokens_are_rejected() {
        let store = SessionStore::new();
        assert!(matches!(
            store.authorize(None, Permission::View),
            Err(AuthError::Missing)
        ));
        assert!(matches!(
            store.authorize(Some("deadbeef"), Permission::View),
            Err(AuthError::Invalid)
        ));
    }

    #[test]
    fn token_shape_validation() {
        // A real issued token is 64 lowercase-hex chars.
        let store = SessionStore::new();
        let token = store.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "v1",
        );
        assert!(is_well_formed_token(&token));
        // Wrong length, non-hex, and embedded junk are all rejected before lookup.
        assert!(!is_well_formed_token(""));
        assert!(!is_well_formed_token("zz"));
        assert!(!is_well_formed_token(&"a".repeat(63)));
        assert!(!is_well_formed_token(&"a".repeat(65)));
        assert!(!is_well_formed_token(&format!("{}!", "a".repeat(63))));
    }

    #[test]
    fn login_throttle_locks_out_after_free_attempts_and_clears_on_success() {
        let t = LoginThrottle::new();
        // The free attempts all pass the per-account gate (each also consumes a global token).
        for _ in 0..LOGIN_FREE_ATTEMPTS {
            assert!(t.check("admin").is_ok());
            t.record_failure("admin");
        }
        // The next failure arms the lockout; a subsequent check is refused with a retry hint.
        assert!(t.check("admin").is_ok());
        t.record_failure("admin");
        let reject = t.check("admin").expect_err("account should be locked out");
        assert!(reject.retry_after_secs >= 1);
        // Case-folding means Admin shares the lockout…
        assert!(t.check("ADMIN").is_err());
        // …but a different account is unaffected.
        assert!(t.check("someone-else").is_ok());
        // A success clears the record (in the handler this only runs when the password matched).
        t.record_success("admin");
        assert!(t.check("admin").is_ok());
    }

    #[test]
    fn login_throttle_global_bucket_caps_total_throughput() {
        let t = LoginThrottle::new();
        // Distinct usernames dodge the per-account lock, but the global bucket still bounds the
        // burst — after the burst capacity is spent, further attempts are refused.
        let mut allowed = 0;
        for i in 0..(LOGIN_GLOBAL_BURST as usize + 10) {
            if t.check(&format!("user{i}")).is_ok() {
                allowed += 1;
            }
        }
        assert_eq!(allowed, LOGIN_GLOBAL_BURST as usize);
    }

    #[test]
    fn tokens_are_unique_and_long() {
        let store = SessionStore::new();
        let a = store.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "v1",
        );
        let b = store.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "v1",
        );
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
    }
}
