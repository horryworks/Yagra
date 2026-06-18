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

use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::{Permission, Principal, Role, Scope};
use yagra_secrets::password::{hash_password, verify_password};

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

/// An authenticated session: the principal plus the account name it was minted for (the
/// audit log records *who*, not just which role).
#[derive(Debug, Clone)]
pub struct Session {
    pub principal: Principal,
    pub username: String,
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

    /// Mint a fresh opaque token for `principal` logged in as `username`.
    pub fn issue(&self, principal: Principal, username: &str) -> String {
        let token = random_token();
        self.sessions
            .lock()
            .expect("sessions mutex poisoned")
            .insert(
                token.clone(),
                Session {
                    principal,
                    username: username.to_owned(),
                },
            );
        token
    }

    /// Resolve a token to its session.
    #[must_use]
    pub fn lookup(&self, token: &str) -> Option<Session> {
        self.sessions
            .lock()
            .expect("sessions mutex poisoned")
            .get(token)
            .cloned()
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
    /// (never stored or logged in plaintext).
    pub async fn ensure_default_admin(&self, password: &str) -> anyhow::Result<()> {
        let count: i64 = sqlx::query("SELECT count(*) AS n FROM users")
            .fetch_one(&self.pool)
            .await?
            .try_get("n")?;
        if count > 0 {
            return Ok(());
        }
        let hash = hash_password(password).map_err(|e| anyhow::anyhow!("hash password: {e}"))?;
        sqlx::query("INSERT INTO users (id, username, password_hash, role) VALUES ($1,$2,$3,$4)")
            .bind(Uuid::new_v4())
            .bind("admin")
            .bind(hash)
            .bind("admin")
            .execute(&self.pool)
            .await?;
        // Intentionally best-effort (not fail-fast) for first-boot UX, but make the log loud:
        // a default credential left in place is a security risk.
        tracing::warn!(
            "SECURITY: seeded default 'admin' user with the bootstrap password — \
             CHANGE THE DEFAULT ADMIN PASSWORD before exposing this instance"
        );
        Ok(())
    }

    /// Verify a username/password and return the principal on success.
    pub async fn verify(
        &self,
        username: &str,
        password: &str,
    ) -> anyhow::Result<Option<Principal>> {
        let row = sqlx::query("SELECT password_hash, role, enabled FROM users WHERE username = $1")
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
        let hash: String = row.try_get("password_hash")?;
        let role: String = row.try_get("role")?;
        let ok = verify_password(password, &hash).unwrap_or(false);
        if ok {
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
            Ok(Some(Principal::new(parse_role(&role), Scope::All)))
        } else {
            Ok(None)
        }
    }

    /// All accounts (metadata only — the password hash is never selected or returned).
    pub async fn list(&self) -> anyhow::Result<Vec<UserSummary>> {
        let rows = sqlx::query(
            "SELECT id, username, role, created_at::text AS created_at, \
             last_login_at::text AS last_login_at, enabled \
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
        let token = store.issue(Principal::new(Role::Operator, Scope::All), "op1");

        // Operator can ack alerts but not manage config; the session carries the username.
        let session = store
            .authorize(Some(&token), Permission::AckAlerts)
            .expect("operator can ack");
        assert_eq!(session.username, "op1");
        assert!(matches!(
            store.authorize(Some(&token), Permission::ManageConfig),
            Err(AuthError::Forbidden)
        ));
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
        let token = store.issue(Principal::new(Role::Viewer, Scope::All), "v1");
        assert!(is_well_formed_token(&token));
        // Wrong length, non-hex, and embedded junk are all rejected before lookup.
        assert!(!is_well_formed_token(""));
        assert!(!is_well_formed_token("zz"));
        assert!(!is_well_formed_token(&"a".repeat(63)));
        assert!(!is_well_formed_token(&"a".repeat(65)));
        assert!(!is_well_formed_token(&format!("{}!", "a".repeat(63))));
    }

    #[test]
    fn tokens_are_unique_and_long() {
        let store = SessionStore::new();
        let a = store.issue(Principal::new(Role::Viewer, Scope::All), "v1");
        let b = store.issue(Principal::new(Role::Viewer, Scope::All), "v1");
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
    }
}
