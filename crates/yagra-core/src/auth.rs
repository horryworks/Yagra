//! Local authentication + RBAC enforcement (Workstream E).
//!
//! Passwords are Argon2id-verified ([`yagra_secrets::password`]); a successful login mints
//! an opaque bearer token held in an in-memory [`SessionStore`] mapped to a [`Principal`]
//! (role + scope). Mutating API endpoints call [`SessionStore::authorize`] with the
//! required [`Permission`]. Read endpoints stay open in this MVP — group-scope filtering
//! of reads is Phase 2 (ADR-014). Tokens are process-local (lost on restart), which is
//! acceptable for the single-core MVP; shared/persistent sessions come with HA.

use std::collections::HashMap;
use std::sync::Mutex;

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

/// In-memory bearer-token → principal sessions.
pub struct SessionStore {
    sessions: Mutex<HashMap<String, Principal>>,
}

impl SessionStore {
    /// New empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Mint a fresh opaque token for `principal`.
    pub fn issue(&self, principal: Principal) -> String {
        let token = random_token();
        self.sessions
            .lock()
            .expect("sessions mutex poisoned")
            .insert(token.clone(), principal);
        token
    }

    /// Resolve a token to its principal.
    #[must_use]
    pub fn lookup(&self, token: &str) -> Option<Principal> {
        self.sessions
            .lock()
            .expect("sessions mutex poisoned")
            .get(token)
            .cloned()
    }

    /// Authorize a request: require a valid token whose principal has `perm`.
    pub fn authorize(
        &self,
        bearer: Option<&str>,
        perm: Permission,
    ) -> Result<Principal, AuthError> {
        let token = bearer.ok_or(AuthError::Missing)?;
        let principal = self.lookup(token).ok_or(AuthError::Invalid)?;
        if principal.can(perm) {
            Ok(principal)
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
        tracing::warn!("seeded default 'admin' user — change its password");
        Ok(())
    }

    /// Verify a username/password and return the principal on success.
    pub async fn verify(
        &self,
        username: &str,
        password: &str,
    ) -> anyhow::Result<Option<Principal>> {
        let row = sqlx::query("SELECT password_hash, role FROM users WHERE username = $1")
            .bind(username)
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let hash: String = row.try_get("password_hash")?;
        let role: String = row.try_get("role")?;
        let ok = verify_password(password, &hash).unwrap_or(false);
        if ok {
            // MVP: every account has unrestricted scope; group-scope filtering is Phase 2.
            Ok(Some(Principal::new(parse_role(&role), Scope::All)))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issued_token_resolves_and_authorizes_by_role() {
        let store = SessionStore::new();
        let token = store.issue(Principal::new(Role::Operator, Scope::All));

        // Operator can ack alerts but not manage config.
        assert!(store.authorize(Some(&token), Permission::AckAlerts).is_ok());
        assert_eq!(
            store.authorize(Some(&token), Permission::ManageConfig),
            Err(AuthError::Forbidden)
        );
    }

    #[test]
    fn missing_and_invalid_tokens_are_rejected() {
        let store = SessionStore::new();
        assert_eq!(
            store.authorize(None, Permission::View),
            Err(AuthError::Missing)
        );
        assert_eq!(
            store.authorize(Some("deadbeef"), Permission::View),
            Err(AuthError::Invalid)
        );
    }

    #[test]
    fn tokens_are_unique_and_long() {
        let store = SessionStore::new();
        let a = store.issue(Principal::new(Role::Viewer, Scope::All));
        let b = store.issue(Principal::new(Role::Viewer, Scope::All));
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
    }
}
