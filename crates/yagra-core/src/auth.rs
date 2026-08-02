// SPDX-License-Identifier: AGPL-3.0-only
//! Local authentication + RBAC enforcement (Workstream E).
//!
//! Passwords are Argon2id-verified ([`yagra_secrets::password`]); a successful login mints
//! an opaque bearer token held in an in-memory [`SessionStore`] mapped to a [`Principal`]
//! (role + scope). Mutating API endpoints call [`SessionStore::authorize`] with the
//! required [`Permission`]. Read endpoints require `View` by default, but can be opened
//! to anonymous access via `YAGRA_PUBLIC_DASHBOARD` (a public read-only dashboard). The scope half
//! is enforced too (ADR-014): it is captured in the session at issue time and resolved per request
//! by `api::scope`, which is why every mutation that narrows an account — a role change, a scope
//! change, a disable — must call [`SessionStore::revoke_user`]. Tokens are process-local (lost on
//! restart) unless a signing key is configured; shared/persistent sessions come with HA.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use sqlx::{PgPool, Row};
use tokio::sync::mpsc;
use uuid::Uuid;
use yagra_bus::AuthRevoke;
use yagra_common::{Permission, Principal, Role, Scope, UserKind};
use yagra_secrets::password::{hash_password, verify_password};

use crate::token::{self, Claims, TokenSigner};

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

/// The revocation denylist for stateless signed tokens (Core HA active/active, ADR-016 Increment
/// 2a). A signed token is self-validating, so logout / account-disable must be recorded here to take
/// effect before the token's own expiry. Entries self-prune once past their bounding expiry.
#[derive(Default)]
struct Denylist {
    /// Individually-revoked tokens: SHA-256 hex → the token's own `exp` (Unix seconds).
    tokens: HashMap<String, u64>,
    /// User-wide revocations: uid → (cutoff `iat`, entry expiry). Tokens for the user issued at or
    /// before `cutoff` are denied until the entry expires.
    users: HashMap<Uuid, (u64, u64)>,
}

impl Denylist {
    /// Whether the given claims/token are currently revoked (checked after signature + `exp`).
    fn is_denied(&self, claims: &Claims, token_hash: &str, now: u64) -> bool {
        if self.tokens.get(token_hash).is_some_and(|&exp| exp > now) {
            return true;
        }
        self.users
            .get(&claims.uid)
            .is_some_and(|&(cutoff, exp)| exp > now && claims.iat <= cutoff)
    }
}

/// Bearer-token → session store.
///
/// Two modes, selected by whether a signing key is configured:
/// - **Opaque (default / single-core, byte-identical to pre-HA):** a random 64-hex token backed by
///   the in-memory `sessions` map (lost on restart, not shared across cores).
/// - **Signed (Core HA active/active, ADR-016 Increment 2a):** a stateless HMAC-signed token
///   verified synchronously on any core that shares the key; revocation rides the `denylist` +
///   `revoke_sink` (fanned out on `yagra.auth.revoke`, persisted to `auth_revocations`).
pub struct SessionStore {
    sessions: Mutex<HashMap<String, Session>>,
    /// When set, tokens are stateless signed tokens instead of opaque map entries.
    signer: Option<TokenSigner>,
    /// Revocation denylist (signed mode only).
    denylist: Mutex<Denylist>,
    /// Sink to the background revocation writer (persist to PG + fan out on the bus). `None` in
    /// opaque mode — nothing to propagate.
    revoke_sink: Option<mpsc::UnboundedSender<AuthRevoke>>,
}

impl SessionStore {
    /// New empty store in **opaque** mode (byte-identical to pre-HA: in-memory, per-process tokens).
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            signer: None,
            denylist: Mutex::new(Denylist::default()),
            revoke_sink: None,
        }
    }

    /// New store in **signed** mode (Core HA active/active): mint/verify stateless HMAC tokens and
    /// propagate revocations through `revoke_sink`.
    #[must_use]
    pub fn with_signer(
        signer: TokenSigner,
        revoke_sink: mpsc::UnboundedSender<AuthRevoke>,
    ) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            signer: Some(signer),
            denylist: Mutex::new(Denylist::default()),
            revoke_sink: Some(revoke_sink),
        }
    }

    /// Mint a fresh token for `principal` (account `user_id`, logged in as `username`). Signed mode
    /// returns a stateless HMAC token (no map entry); opaque mode stores a random token in memory.
    pub fn issue(&self, user_id: Uuid, principal: Principal, username: &str) -> String {
        if let Some(signer) = &self.signer {
            let iat = token::unix_now();
            let claims = Claims {
                uid: user_id,
                principal,
                username: username.to_owned(),
                iat,
                // No sliding idle window for a stateless token — the absolute lifetime is the bound.
                exp: iat + SESSION_ABSOLUTE_TTL.as_secs(),
            };
            return signer.sign(&claims);
        }
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

    /// Resolve a token to its session. A signed token is verified (signature + absolute `exp` +
    /// denylist) with no I/O; an opaque token is looked up in the in-memory map, enforcing the idle
    /// + absolute TTL (an expired token is purged, a live one has its idle window refreshed).
    #[must_use]
    pub fn lookup(&self, token: &str) -> Option<Session> {
        // Signed mode: a signed-shaped token is verified statelessly. An opaque-shaped token here is
        // a legacy in-memory token minted by another/older core — fall through to the map (a miss
        // for a cross-core token ⇒ a clean 401 → re-login during a rolling upgrade).
        if let Some(signer) = &self.signer {
            if token::is_signed_shape(token) {
                return self.lookup_signed(token, signer);
            }
        }
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

    /// Verify a signed token: signature, absolute expiry, then the revocation denylist. All in-
    /// memory / CPU-only, so the API `authorize()` path stays synchronous.
    fn lookup_signed(&self, token: &str, signer: &TokenSigner) -> Option<Session> {
        let claims = signer.verify(token)?;
        let now = token::unix_now();
        if claims.exp <= now {
            return None; // expired
        }
        let hash = token::token_hash(token);
        if self
            .denylist
            .lock()
            .expect("denylist mutex poisoned")
            .is_denied(&claims, &hash, now)
        {
            return None; // logged out / user revoked
        }
        Some(Session {
            principal: claims.principal,
            username: claims.username,
            user_id: claims.uid,
            // Instants are cosmetic for signed tokens (validity comes from `claims.exp`); the
            // downstream callers read only principal/username/user_id.
            issued_at: Instant::now(),
            last_seen: Instant::now(),
        })
    }

    /// Revoke a single token (server-side logout). Opaque mode drops the in-memory entry; signed mode
    /// records the token hash in the denylist and propagates it (bus fan-out + durable table).
    pub fn revoke_token(&self, token: &str) {
        self.sessions
            .lock()
            .expect("sessions mutex poisoned")
            .remove(token);
        if let Some(signer) = &self.signer {
            if token::is_signed_shape(token) {
                let exp = signer
                    .verify(token)
                    .map(|c| c.exp)
                    .unwrap_or_else(|| token::unix_now() + SESSION_ABSOLUTE_TTL.as_secs());
                self.propagate(AuthRevoke::Token {
                    hash: token::token_hash(token),
                    exp_unix: exp,
                });
            }
        }
    }

    /// Revoke every session belonging to `user_id` — called when the account is disabled, demoted,
    /// password-reset, or deleted, so those admin controls take effect on already-issued tokens
    /// instead of only on future logins. Opaque mode drops the in-memory sessions (returns the count);
    /// signed mode records a user-wide denylist cutoff and propagates it.
    pub fn revoke_user(&self, user_id: Uuid) -> usize {
        let dropped = {
            let mut map = self.sessions.lock().expect("sessions mutex poisoned");
            let before = map.len();
            map.retain(|_, s| s.user_id != user_id);
            before - map.len()
        };
        if self.signer.is_some() {
            let now = token::unix_now();
            self.propagate(AuthRevoke::User {
                uid: user_id,
                cutoff_iat: now,
                exp_unix: now + SESSION_ABSOLUTE_TTL.as_secs(),
            });
        }
        dropped
    }

    /// Apply a revocation locally, then push it to the writer for durable persist + bus fan-out.
    fn propagate(&self, revoke: AuthRevoke) {
        self.apply_local(&revoke);
        if let Some(tx) = &self.revoke_sink {
            // Non-blocking; a closed channel (shutdown) is ignored — the local denylist already
            // reflects the revocation on this core.
            let _ = tx.send(revoke);
        }
    }

    /// Apply a revocation received from another core (bus) or loaded from the durable table on
    /// startup — updates the local denylist only (no re-propagation, so there is no fan-out loop).
    pub fn apply_remote_revoke(&self, revoke: &AuthRevoke) {
        self.apply_local(revoke);
    }

    fn apply_local(&self, revoke: &AuthRevoke) {
        let mut d = self.denylist.lock().expect("denylist mutex poisoned");
        match revoke {
            AuthRevoke::Token { hash, exp_unix } => {
                d.tokens.insert(hash.clone(), *exp_unix);
            }
            AuthRevoke::User {
                uid,
                cutoff_iat,
                exp_unix,
            } => {
                d.users
                    .entry(*uid)
                    .and_modify(|(c, e)| {
                        *c = (*c).max(*cutoff_iat);
                        *e = (*e).max(*exp_unix);
                    })
                    .or_insert((*cutoff_iat, *exp_unix));
            }
        }
    }

    /// Drop denylist entries whose bounding expiry has passed (bounded memory). Called periodically.
    pub fn prune_denylist(&self) {
        let now = token::unix_now();
        let mut d = self.denylist.lock().expect("denylist mutex poisoned");
        d.tokens.retain(|_, exp| *exp > now);
        d.users.retain(|_, (_, exp)| *exp > now);
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

// ── Durable revocations (Core HA active/active, ADR-016 Increment 2a) ────────────────────────────
// A signed token outlives the process that minted it, so its revocation must be durable too — else a
// restart/failover would silently un-revoke a logged-out token. The `auth_revocations` table (mig
// 0046) is the source of truth a core cold-loads on start; the in-memory denylist is the hot path.

/// Upsert a revocation into the durable table (keep the strictest cutoff/expiry on conflict).
pub async fn persist_revocation(pool: &PgPool, revoke: &AuthRevoke) -> Result<(), sqlx::Error> {
    match revoke {
        AuthRevoke::Token { hash, exp_unix } => {
            sqlx::query(
                "INSERT INTO auth_revocations (kind, key, cutoff_iat, expires_at) \
                 VALUES ('token', $1, NULL, to_timestamp($2)) \
                 ON CONFLICT (kind, key) DO UPDATE \
                 SET expires_at = GREATEST(auth_revocations.expires_at, EXCLUDED.expires_at)",
            )
            .bind(hash)
            .bind(*exp_unix as i64)
            .execute(pool)
            .await?;
        }
        AuthRevoke::User {
            uid,
            cutoff_iat,
            exp_unix,
        } => {
            sqlx::query(
                "INSERT INTO auth_revocations (kind, key, cutoff_iat, expires_at) \
                 VALUES ('user', $1, $2, to_timestamp($3)) \
                 ON CONFLICT (kind, key) DO UPDATE \
                 SET cutoff_iat = GREATEST(auth_revocations.cutoff_iat, EXCLUDED.cutoff_iat), \
                     expires_at = GREATEST(auth_revocations.expires_at, EXCLUDED.expires_at)",
            )
            .bind(uid.to_string())
            .bind(*cutoff_iat as i64)
            .bind(*exp_unix as i64)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

/// Load all still-valid revocations from the durable table (called on startup so a restarted /
/// promoted core honors revocations made while it wasn't the writer).
pub async fn load_active_revocations(pool: &PgPool) -> Result<Vec<AuthRevoke>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT kind, key, cutoff_iat, extract(epoch FROM expires_at)::bigint AS exp_unix \
         FROM auth_revocations WHERE expires_at > now()",
    )
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let kind: String = row.get("kind");
        let key: String = row.get("key");
        let exp_unix = row.get::<i64, _>("exp_unix").max(0) as u64;
        match kind.as_str() {
            "token" => out.push(AuthRevoke::Token {
                hash: key,
                exp_unix,
            }),
            "user" => {
                let cutoff_iat = row.try_get::<i64, _>("cutoff_iat").unwrap_or(0).max(0) as u64;
                if let Ok(uid) = Uuid::parse_str(&key) {
                    out.push(AuthRevoke::User {
                        uid,
                        cutoff_iat,
                        exp_unix,
                    });
                }
            }
            _ => {} // unknown kind (forward-compat) — ignored
        }
    }
    Ok(out)
}

/// Delete expired rows from the durable table (bounded growth). Called periodically.
pub async fn prune_revocations(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let res = sqlx::query("DELETE FROM auth_revocations WHERE expires_at <= now()")
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
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

/// Cheap shape check at the auth edge so malformed `Authorization` headers are rejected before the
/// session-map lookup / signature verification. Accepts either an **opaque** token (exactly 64
/// lowercase-hex chars) or a **signed** token (`y2.` prefix, bounded length, no whitespace) — both
/// coexist during a rolling upgrade (ADR-016 Increment 2a).
fn is_well_formed_token(token: &str) -> bool {
    if token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return true;
    }
    token::is_signed_shape(token)
        && token.len() <= 4096
        && !token.bytes().any(|b| b.is_ascii_whitespace())
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

/// Placeholder stored in `users.password_hash` for **service accounts**, which have no password by
/// design. Same construction and same reasoning as [`OIDC_PASSWORD_SENTINEL`]: not a valid Argon2
/// PHC string, so it cannot verify even if the `auth_source` guard were somehow bypassed, and the
/// column stays NOT NULL so an N-1 binary reads a `String` it simply cannot match.
pub const SERVICE_PASSWORD_SENTINEL: &str = "!service-account-no-login";

/// Parse a stored role string via [`Role::key`] (defaults to the least-privileged role on
/// garbage). Derived from [`Role::ALL`] so the token list lives in one place.
fn parse_role(s: &str) -> Role {
    Role::ALL
        .into_iter()
        .find(|r| r.key() == s)
        .unwrap_or(Role::Viewer)
}

/// Parse the stored `users.scope` JSONB into a [`Scope`], failing **closed**.
///
/// A value this binary cannot read becomes `Groups([])` — an account that sees nothing — rather
/// than `All`. The two are not interchangeable: treating a corrupt or future-version row as
/// unrestricted would turn a storage fault into a privilege escalation, whereas failing closed
/// costs an operator a visible complaint. `Scope::group_uuids` makes the same choice for entries
/// inside the set, and for the same reason.
fn parse_scope(raw: serde_json::Value, user_id: Uuid) -> Scope {
    serde_json::from_value(raw).unwrap_or_else(|e| {
        tracing::error!(
            error = %e,
            user_id = %user_id,
            "account scope is unreadable; treating it as empty (sees nothing)"
        );
        Scope::Groups(std::collections::BTreeSet::new())
    })
}

/// User-account metadata for the API — never includes the password hash.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
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
    /// Which slice of the inventory this account may see: `"All"`, or the node groups it is
    /// limited to. An Admin account is always `"All"` — administration is fleet-wide.
    pub scope: Scope,
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
    /// Refused: the target is an Admin, and an Admin is unscoped by construction
    /// (see `ADMIN_IS_UNSCOPED`). Only [`UserStore::set_scope`] can return this.
    AdminIsUnscoped,
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
            "SELECT id, password_hash, role, enabled, auth_source, scope \
             FROM users WHERE username = $1",
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
        // Only a local account has a password to check. An OIDC account signs in through the IdP; a
        // service account cannot sign in at all. Reject before the password check — their stored
        // hashes are non-verifiable sentinels anyway, so this is defence in depth rather than the
        // only gate.
        let auth_source: String = row.try_get("auth_source")?;
        if UserKind::parse(&auth_source) != UserKind::Local {
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
            let scope = parse_scope(row.try_get("scope")?, id);
            Ok(Some((id, Principal::new(parse_role(&role), scope))))
        } else {
            Ok(None)
        }
    }

    /// All accounts (metadata only — the password hash is never selected or returned).
    pub async fn list(&self) -> anyhow::Result<Vec<UserSummary>> {
        let rows = sqlx::query(
            "SELECT id, username, role, created_at::text AS created_at, \
             last_login_at::text AS last_login_at, enabled, auth_source, scope \
             FROM users ORDER BY created_at, username",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let id: Uuid = row.try_get("id")?;
                Ok(UserSummary {
                    id,
                    username: row.try_get("username")?,
                    role: row.try_get("role")?,
                    created_at: row.try_get("created_at")?,
                    last_login_at: row.try_get("last_login_at")?,
                    enabled: row.try_get("enabled")?,
                    auth_source: row.try_get("auth_source")?,
                    scope: parse_scope(row.try_get("scope")?, id),
                })
            })
            .collect()
    }

    /// Create a local account. The password is Argon2id-hashed before it touches the database and
    /// is never logged. A duplicate username surfaces as [`UserCreateOutcome::UsernameTaken`]
    /// (the `users.username` UNIQUE constraint), not an opaque 500.
    pub async fn create(
        &self,
        username: &str,
        password: &str,
        role: &str,
    ) -> anyhow::Result<UserCreateOutcome> {
        let hash = hash_password(password).map_err(|e| anyhow::anyhow!("hash password: {e}"))?;
        self.insert(username, &hash, role, UserKind::Local).await
    }

    /// Create a **service account**: a machine identity with `role`, no password, and no way to sign
    /// in ([`UserKind::Service`]).
    ///
    /// It exists to own API tokens. Binding an unattended integration to a person means the
    /// credential dies when they change teams — and leaving it bound to nobody is what let a
    /// departed admin's token outlive the account. A service account is the third option.
    pub async fn create_service(
        &self,
        username: &str,
        role: &str,
    ) -> anyhow::Result<UserCreateOutcome> {
        self.insert(username, SERVICE_PASSWORD_SENTINEL, role, UserKind::Service)
            .await
    }

    /// Insert one account row. Shared by [`Self::create`] and [`Self::create_service`], which differ
    /// only in what goes in the password column and the `auth_source` — writing the INSERT twice is
    /// how the two would come to disagree about, say, the default `enabled`.
    async fn insert(
        &self,
        username: &str,
        password_hash: &str,
        role: &str,
        kind: UserKind,
    ) -> anyhow::Result<UserCreateOutcome> {
        let id = Uuid::new_v4();
        let res = sqlx::query(
            "INSERT INTO users (id, username, password_hash, role, auth_source) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(username)
        .bind(password_hash)
        .bind(role)
        .bind(kind.key())
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
        let role_str = role.key();
        // Existing identity → refresh role + last_login.
        let existing: Option<(Uuid, bool)> = sqlx::query_as(
            "SELECT id, enabled FROM users WHERE oidc_provider_id = $1 AND oidc_subject = $2",
        )
        .bind(provider_id)
        .bind(subject)
        .fetch_optional(&self.pool)
        .await?;
        if let Some((id, enabled)) = existing {
            // A disabled account cannot sign in — including through the IdP. This is checked here
            // rather than only in `verify` because the two login paths are separate: the local one
            // has always honoured `enabled`, while this one went straight from "the IdP says who you
            // are" to issuing a session. Disabling an SSO account therefore revoked its sessions and
            // then let the next SSO login mint a fresh one, which made the control a no-op for
            // exactly the accounts an operator is least able to switch off at the source.
            if !enabled {
                anyhow::bail!("account is disabled");
            }
            // The **stored** scope wins, not one derived from the IdP. The role is refreshed from
            // the directory on every login because the directory is authoritative about who someone
            // is; the scope is a Yagra-side assignment an admin made here, and re-deriving it would
            // silently widen it back to unrestricted on the user's next sign-in. Mapping IdP groups
            // to a scope is a later increment — when it lands it must replace this read, not race it.
            //
            // Returned by the same statement that refreshes the role, so the promotion and the
            // scope it invalidates cannot be observed apart (see `ADMIN_IS_UNSCOPED`).
            let scope: serde_json::Value = sqlx::query_scalar(&format!(
                "UPDATE users SET role = $2, last_login_at = now(), {ADMIN_IS_UNSCOPED} \
                 WHERE id = $1 RETURNING scope"
            ))
            .bind(id)
            .bind(role_str)
            .fetch_one(&self.pool)
            .await?;
            return Ok((id, Principal::new(role, parse_scope(scope, id))));
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
        // A just-provisioned account is unrestricted (the column default) — matching what every
        // account got before scopes could be issued. Narrowing it is an explicit admin action on
        // the account that now exists, which is also the only order that works: nobody can be given
        // a scope before their first sign-in reveals which account they are.
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
        // Promoting to Admin also clears any group scope — see `ADMIN_IS_UNSCOPED`. In the same
        // statement, so there is no window in which the account is an admin with a stale narrow
        // view of the fleet it can already reconfigure.
        sqlx::query(&format!(
            "UPDATE users SET role = $2, {ADMIN_IS_UNSCOPED} WHERE id = $1"
        ))
        .bind(id)
        .bind(new_role)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(UserMutation::Done)
    }

    /// One account's visibility scope, or `None` if there is no such account.
    ///
    /// Read on its own (rather than through [`Self::list`]) by API-token minting, which needs the
    /// prospective owner's scope to refuse a token that would claim more than the account it acts as.
    pub async fn scope_of(&self, id: Uuid) -> anyhow::Result<Option<Scope>> {
        let row = sqlx::query("SELECT scope FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(match row {
            Some(row) => Some(parse_scope(row.try_get("scope")?, id)),
            None => None,
        })
    }

    /// Replace an account's visibility scope, refusing to narrow an **Admin** account.
    ///
    /// The caller must revoke the account's sessions afterwards: the principal — scope included —
    /// is captured in the session token when it is issued, so a live token keeps the old, wider
    /// view until it is cut. That is the same rule a role change follows (`api/users.rs`).
    pub async fn set_scope(&self, id: Uuid, scope: &Scope) -> anyhow::Result<UserMutation> {
        let mut tx = self.pool.begin().await?;
        let Some(row) = sqlx::query("SELECT role FROM users WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Ok(UserMutation::NotFound);
        };
        let role: String = row.try_get("role")?;
        if role == "admin" && *scope != Scope::All {
            return Ok(UserMutation::AdminIsUnscoped);
        }
        sqlx::query("UPDATE users SET scope = $2 WHERE id = $1")
            .bind(id)
            .bind(serde_json::to_value(scope)?)
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

/// SQL fragment restricting a count to accounts a **person** can sign in with.
///
/// Both lock-out guards below ask "would this leave nobody able to administer the system", and the
/// answer has to be about humans. A service account can hold the Admin role — that is how an
/// automation gets write access — but nobody can log into it, so counting one would let the last
/// human admin be deleted or disabled and leave the WebUI unreachable with no way back in. Written
/// once because two guards that disagree about who counts is precisely that bug, half-fixed.
const CAN_LOG_IN: &str = "auth_source <> 'service'";

/// SQL `SET` fragment resetting an Admin account's scope to unrestricted. **Requires the new role
/// to be bound at `$2`** — PostgreSQL cannot read another `SET` column's new value, so the fragment
/// has to look at the parameter rather than at `role`.
///
/// Admin permissions are fleet-wide by construction: `ManageConfig` writes are not scope-filtered
/// (an ADR-014 non-goal), so an admin holding a group scope would read a narrowed inventory while
/// still being able to edit — and break — the nodes that inventory hides. Reads answering `404`
/// where writes answer `200` is not a safety property, it is a confusing one.
///
/// So the invariant is *"an Admin is unscoped"*, and it is enforced on every path that can reach
/// admin: `set_scope` refuses to narrow one, and both role-setting paths — [`UserStore::set_role`]
/// and the SSO role refresh in [`UserStore::upsert_oidc_user`] — clear the scope in the same
/// statement that grants the role. The ledger's `ADMIN_CFG` reason ("an Admin is unscoped by
/// construction") is true because of this constant.
const ADMIN_IS_UNSCOPED: &str =
    "scope = CASE WHEN $2 = 'admin' THEN '\"All\"'::jsonb ELSE scope END";

/// Count of admin accounts within an open transaction (lock-out guard helper).
async fn admin_count(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> anyhow::Result<i64> {
    let n: i64 = sqlx::query(&format!(
        "SELECT count(*) AS n FROM users WHERE role = 'admin' AND {CAN_LOG_IN}"
    ))
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
    let n: i64 = sqlx::query(&format!(
        "SELECT count(*) AS n FROM users WHERE role = 'admin' AND enabled AND {CAN_LOG_IN}"
    ))
    .fetch_one(&mut **tx)
    .await?
    .try_get("n")?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreadable_stored_scope_sees_nothing_rather_than_everything() {
        let id = Uuid::nil();
        // The two states that must never collapse into one another.
        assert_eq!(parse_scope(serde_json::json!("All"), id), Scope::All);
        assert_eq!(
            parse_scope(serde_json::json!({"Groups": ["a"]}), id),
            Scope::groups(["a"])
        );
        // Garbage, a null, or a variant this binary has never heard of: all fail **closed**.
        // Reading any of them as `All` would make a corrupt row a privilege escalation.
        for bad in [
            serde_json::json!("all"),
            serde_json::json!(null),
            serde_json::json!(7),
            serde_json::json!({"Tenants": ["x"]}),
        ] {
            let scope = parse_scope(bad.clone(), id);
            assert_ne!(scope, Scope::All, "{bad} was read as unrestricted");
            assert!(!scope.allows(&std::collections::BTreeSet::from(["a".to_owned()])));
        }
    }

    #[test]
    fn every_statement_that_sets_a_role_also_clears_an_admins_scope() {
        // An Admin holding a group scope reads a narrowed inventory while still being able to
        // reconfigure the nodes it hides. The invariant is enforced by one SQL fragment, so the
        // failure mode is a *new* role-setting statement that forgets it — which nothing else
        // catches, since it compiles and works.
        let src = include_str!("auth.rs");
        // Assembled at runtime: this test reads its own file, so a literal needle would match
        // itself and pass forever.
        let fragment = format!("{}_IS_{}", "ADMIN", "UNSCOPED");
        let setter = format!("SET {} = $2", "role");
        let sites: Vec<&str> = src.match_indices(&setter).map(|(i, _)| &src[i..]).collect();
        assert!(
            sites.len() >= 2,
            "expected the local role change and the SSO role refresh; found {}",
            sites.len()
        );
        for site in sites {
            let statement = &site[..site.find("WHERE").unwrap_or(site.len())];
            assert!(
                statement.contains(&fragment),
                "a statement setting `role` does not clear an admin's scope: {statement}"
            );
        }
    }

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

    // ── Signed-token mode (Core HA active/active, ADR-016 Increment 2a) ──────────────────────

    const TEST_KEY: [u8; 32] = [3u8; 32];

    fn signed_store() -> (SessionStore, mpsc::UnboundedReceiver<AuthRevoke>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            SessionStore::with_signer(TokenSigner::new(TEST_KEY), tx),
            rx,
        )
    }

    fn sign_at(uid: Uuid, iat: u64) -> String {
        TokenSigner::new(TEST_KEY).sign(&Claims {
            uid,
            principal: Principal::new(Role::Operator, Scope::All),
            username: "u".into(),
            iat,
            exp: iat + 3600,
        })
    }

    #[test]
    fn signed_token_is_stateless_and_authorizes_by_role() {
        let (store, _rx) = signed_store();
        let uid = Uuid::new_v4();
        let token = store.issue(uid, Principal::new(Role::Operator, Scope::All), "op");
        assert!(token::is_signed_shape(&token));
        let s = store
            .authorize(Some(&token), Permission::AckAlerts)
            .expect("operator can ack");
        assert_eq!(s.user_id, uid);
        assert_eq!(s.username, "op");
        assert!(matches!(
            store.authorize(Some(&token), Permission::ManageConfig),
            Err(AuthError::Forbidden)
        ));
    }

    #[test]
    fn signed_token_verifies_on_another_core_with_the_same_key_but_not_a_different_key() {
        let (a, _ra) = signed_store();
        let token = a.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "v",
        );

        // A second "core" sharing the key accepts the token minted on the first.
        let (tx, _rx) = mpsc::unbounded_channel();
        let same_key = SessionStore::with_signer(TokenSigner::new(TEST_KEY), tx);
        assert!(same_key.authorize(Some(&token), Permission::View).is_ok());

        // A core with a different key rejects it (Invalid, not a panic).
        let (tx2, _rx2) = mpsc::unbounded_channel();
        let other_key = SessionStore::with_signer(TokenSigner::new([9u8; 32]), tx2);
        assert!(matches!(
            other_key.authorize(Some(&token), Permission::View),
            Err(AuthError::Invalid)
        ));
    }

    #[test]
    fn logout_denies_a_signed_token_and_enqueues_a_token_revocation() {
        let (store, mut rx) = signed_store();
        let token = store.issue(
            Uuid::new_v4(),
            Principal::new(Role::Operator, Scope::All),
            "op",
        );
        assert!(store.authorize(Some(&token), Permission::View).is_ok());

        store.revoke_token(&token);
        assert!(matches!(
            store.authorize(Some(&token), Permission::View),
            Err(AuthError::Invalid)
        ));
        // The revocation is enqueued (token *hash*, never the token) for durable persist + fan-out.
        match rx.try_recv() {
            Ok(AuthRevoke::Token { hash, .. }) => assert_eq!(hash, token::token_hash(&token)),
            other => panic!("expected a token revocation, got {other:?}"),
        }
    }

    #[test]
    fn revoke_user_denies_tokens_issued_at_or_before_the_cutoff_only() {
        let (store, _rx) = signed_store();
        let uid = Uuid::new_v4();
        let now = token::unix_now();

        let old = sign_at(uid, now - 100);
        assert!(store.authorize(Some(&old), Permission::View).is_ok());

        store.revoke_user(uid); // cutoff ≈ now

        // The pre-cutoff token is now denied...
        assert!(matches!(
            store.authorize(Some(&old), Permission::View),
            Err(AuthError::Invalid)
        ));
        // ...a token issued well after the cutoff (a fresh login) still works...
        let future = sign_at(uid, now + 100);
        assert!(store.authorize(Some(&future), Permission::View).is_ok());
        // ...and a different user is unaffected.
        let other = sign_at(Uuid::new_v4(), now - 100);
        assert!(store.authorize(Some(&other), Permission::View).is_ok());
    }

    #[test]
    fn remote_revocation_denies_locally_without_re_enqueue() {
        let (store, mut rx) = signed_store();
        let token = store.issue(
            Uuid::new_v4(),
            Principal::new(Role::Operator, Scope::All),
            "op",
        );
        // Simulate a revocation fanned out from another core.
        store.apply_remote_revoke(&AuthRevoke::Token {
            hash: token::token_hash(&token),
            exp_unix: token::unix_now() + 3600,
        });
        assert!(matches!(
            store.authorize(Some(&token), Permission::View),
            Err(AuthError::Invalid)
        ));
        // Applying a *remote* revocation must not re-enqueue it (no fan-out loop).
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn an_expired_revocation_does_not_deny_a_live_token() {
        let (store, _rx) = signed_store();
        let uid = Uuid::new_v4();
        let now = token::unix_now();
        let tok = sign_at(uid, now);
        // A user-revoke whose bounding expiry already passed must not affect a live token.
        store.apply_remote_revoke(&AuthRevoke::User {
            uid,
            cutoff_iat: now + 10,
            exp_unix: now.saturating_sub(1),
        });
        assert!(store.authorize(Some(&tok), Permission::View).is_ok());
        store.prune_denylist(); // must not panic
    }

    #[test]
    fn opaque_mode_is_unchanged_and_rejects_malformed_bearers() {
        // Byte-identical default: no signer ⇒ 64-hex opaque tokens, in-memory.
        let store = SessionStore::new();
        let t = store.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "v",
        );
        assert_eq!(t.len(), 64);
        assert!(t.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(store.authorize(Some(&t), Permission::View).is_ok());
        // The well-formed-token edge still rejects junk and a missing bearer.
        assert!(matches!(
            store.authorize(Some("not a token"), Permission::View),
            Err(AuthError::Invalid)
        ));
        assert!(matches!(
            store.authorize(None, Permission::View),
            Err(AuthError::Missing)
        ));
    }

    #[test]
    fn n_minus_1_opaque_token_on_a_signed_core_is_a_clean_invalid_not_a_panic() {
        // During a rolling upgrade a signed-mode core may receive an opaque token minted in another
        // core's memory: it isn't in this core's map ⇒ clean 401 → re-login (no panic).
        let (store, _rx) = signed_store();
        let stray_opaque = "a".repeat(64);
        assert!(matches!(
            store.authorize(Some(&stray_opaque), Permission::View),
            Err(AuthError::Invalid)
        ));
    }
}
