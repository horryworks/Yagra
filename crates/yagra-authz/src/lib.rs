// SPDX-License-Identifier: AGPL-3.0-only
//! Yagra-authz — NATS **Auth Callout** credential scoping (ADR-030).
//!
//! Monitoring credentials (SNMP communities, SNMPv3 keys, API tokens) are decrypted by core and ride
//! the bus inside working-set/job messages (ADR-020). The application layer already addresses each
//! poller's assignment to its own subject (`yagra.poller.assign.{id}`), but the shared `poller` NATS
//! account can *subscribe* to `yagra.poller.assign.>` / `yagra.jobs.>` — so any authenticated poller
//! could read every pool's credentials. This crate closes that at the transport-authz layer: core acts
//! as a NATS **auth service**, and for each poller connection mints a signed **user JWT** scoped to only
//! that poller's own subjects.
//!
//! This crate is the *pure* half — nkey signing + the NATS-JWT wire format + the allow-list — with no
//! async-nats/IO, so the security-critical scoping is unit-testable. The live `$SYS.REQ.USER.AUTH`
//! responder wiring lives in `yagra-core` (`authcallout.rs`). The subject names come from
//! [`yagra_bus::subjects`] so the granted allow-list can never drift from what the poller subscribes to
//! (drift = a silent scoping hole).
//!
//! **No secret or minted JWT is ever logged** (security.md): callers surface counts, not values.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use yagra_bus::subjects;

use data_encoding::{BASE32_NOPAD, BASE64URL_NOPAD};

/// Errors from JWT parsing / nkey signing. Deliberately opaque — never embeds a secret.
#[derive(Debug, thiserror::Error)]
pub enum AuthzError {
    #[error("nkey error")]
    Nkey,
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("malformed request jwt")]
    MalformedJwt,
}

// ── Public scope + allow-list ───────────────────────────────────────────────────────────────────

/// The per-poller pub/sub permission set granted by a callout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Permissions {
    pub publish: Vec<String>,
    pub subscribe: Vec<String>,
}

/// A poller's self-asserted identity, **sanitized** so a hostile `user`/`name` (e.g. `*` or `x.>`)
/// cannot smuggle a NATS wildcard into a granted subject. Sanitization matches
/// [`subjects::sanitize_token`], which is exactly what the poller applies to its own id — so the
/// granted `yagra.poller.assign.{id}` lines up with the subject the poller actually subscribes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PollerScope {
    pub id: String,
    pub pool: String,
}

impl PollerScope {
    #[must_use]
    pub fn new(id: &str, pool: &str) -> Self {
        Self {
            id: subjects::sanitize_token(id),
            pool: subjects::sanitize_token(pool),
        }
    }
}

/// The exact allow-list a poller is granted: only its own assignment subject and its own pool's jobs /
/// discovery, plus the fixed publish set and the client reply inbox. Crucially there are **no**
/// wildcards over other pollers' or other pools' subjects — that is the whole point of the feature.
#[must_use]
pub(crate) fn allow_list(scope: &PollerScope) -> Permissions {
    Permissions {
        // Poller → core. A fixed set; `results.backfill` is included (store-and-forward, ADR — the
        // shared static account historically omitted it, which blocked backfill on the remote bus).
        // `flows` is the edge-aggregated flow-batch subject (ADR-031): a remote-site poller running a
        // flow listener publishes here, so it must be granted or its batches are rejected on the bus.
        // `flows.raw` is the verbatim datagram relay that feeds forwarding (ADR-034 Increment 2) —
        // omitting it would silently strand flow forwarding for every remote site.
        publish: vec![
            subjects::results(),
            subjects::results_backfill(),
            subjects::events(),
            subjects::flows(),
            subjects::flows_raw(),
            subjects::discovery_results(),
            subjects::heartbeat(),
            subjects::sync_request(),
            // Support-log chunks (ADR-045 Inc.4). Publish only, and on the *reply* subject only —
            // the request family `yagra.poller.logs.{id}` is subscribe-side below and must never
            // appear here, or a compromised site could ask another site for its log body.
            subjects::poller_log_reply(),
        ],
        // core → this poller. The working-set (`assign.{id}`) and pool jobs — which carry the
        // continuous monitoring credentials — are scoped to this poller / its pool only, so a
        // compromised poller can't read another poller's device credentials. Discovery is granted
        // both pool-scoped AND the legacy all-pools `yagra.discovery.jobs` subject, because core still
        // publishes pool-less sweeps there (`discovery.rs`); tightening discovery to pool-only is a
        // follow-up (it needs core to always route discovery pool-scoped). `_INBOX.>` is the client's
        // own reply inbox.
        subscribe: vec![
            subjects::assignment_for(&scope.id),
            subjects::jobs_for_pool(&scope.pool),
            subjects::discovery_jobs_for_pool(&scope.pool),
            subjects::discovery_jobs(),
            // Its own upgrade command, and only its own (ADR-051). Note where this is **not**:
            // nowhere in `publish`. Core is the only thing that may issue one, and a poller holding
            // publish rights here could order an upgrade — or a downgrade — at another site.
            // Appended rather than inserted because the JWT-shape test names `sub.allow[0]`.
            subjects::upgrade_for(&scope.id),
            // Its own support-log request, and only its own (ADR-045 Inc.4). A wildcard here would
            // let one site receive another site's request and answer it — the reply carries a log
            // body, so the mis-delivery would be a disclosure rather than a routing annoyance.
            // Appended rather than inserted because the JWT-shape test names `sub.allow[0]`.
            subjects::poller_logs_for(&scope.id),
            "_INBOX.>".to_owned(),
        ],
    }
}

// ── Auth-callout request (server → auth service) ────────────────────────────────────────────────

/// The fields we need from a decoded `$SYS.REQ.USER.AUTH` request JWT.
#[derive(Debug, Clone)]
pub struct AuthRequest {
    /// The connecting server's id — the response's audience must echo it.
    pub server_id: String,
    /// The ephemeral user nkey the client generated for this connection — the minted user JWT's subject.
    pub user_nkey: String,
    /// `CONNECT` username — we treat it as the claimed poller id.
    pub connect_user: Option<String>,
    /// `CONNECT` password — we treat it as the bootstrap secret.
    pub connect_pass: Option<String>,
    /// `CONNECT` name — we treat it as the claimed pool.
    pub connect_name: Option<String>,
}

#[derive(Deserialize)]
struct ReqEnvelope {
    nats: ReqNats,
}
#[derive(Deserialize)]
struct ReqNats {
    server_id: ReqServerId,
    user_nkey: String,
    #[serde(default)]
    connect_opts: ReqConnectOpts,
}
#[derive(Deserialize)]
struct ReqServerId {
    id: String,
}
#[derive(Deserialize, Default)]
struct ReqConnectOpts {
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    pass: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

/// Decode (but do **not** cryptographically verify) an auth-callout request JWT. The server signs it and
/// we trust the local NATS server; we only need the claims. Empty `user`/`name`/`pass` normalize to `None`.
pub(crate) fn parse_auth_request(request_jwt: &str) -> Result<AuthRequest, AuthzError> {
    let mut parts = request_jwt.split('.');
    let (_header, body) = match (parts.next(), parts.next()) {
        (Some(h), Some(b)) if !h.is_empty() && !b.is_empty() => (h, b),
        _ => return Err(AuthzError::MalformedJwt),
    };
    let raw = BASE64URL_NOPAD
        .decode(body.as_bytes())
        .map_err(|_| AuthzError::MalformedJwt)?;
    let env: ReqEnvelope = serde_json::from_slice(&raw)?;
    let norm = |o: Option<String>| o.filter(|s| !s.is_empty());
    Ok(AuthRequest {
        server_id: env.nats.server_id.id,
        user_nkey: env.nats.user_nkey,
        connect_user: norm(env.nats.connect_opts.user),
        connect_pass: norm(env.nats.connect_opts.pass),
        connect_name: norm(env.nats.connect_opts.name),
    })
}

// ── JWT claim shapes (NATS JWT v2) ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct JwtHeader {
    typ: &'static str,
    alg: &'static str,
}
const HEADER: JwtHeader = JwtHeader {
    typ: "JWT",
    alg: "ed25519-nkey",
};

#[derive(Serialize)]
struct Perm {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    allow: Vec<String>,
}

#[derive(Serialize)]
struct UserNats {
    #[serde(rename = "pub")]
    publish: Perm,
    #[serde(rename = "sub")]
    subscribe: Perm,
    subs: i64,
    data: i64,
    payload: i64,
    #[serde(rename = "type")]
    kind: &'static str,
    version: u8,
}

#[derive(Serialize)]
struct UserClaims {
    jti: String,
    iat: i64,
    iss: String,
    sub: String,
    aud: String,
    nats: UserNats,
}

#[derive(Serialize)]
struct RespNats {
    #[serde(skip_serializing_if = "Option::is_none")]
    jwt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RespError>,
    #[serde(rename = "type")]
    kind: &'static str,
    version: u8,
}

#[derive(Serialize)]
struct RespError {
    description: String,
}

#[derive(Serialize)]
struct RespClaims {
    jti: String,
    iat: i64,
    iss: String,
    sub: String,
    aud: String,
    nats: RespNats,
}

// ── Signer ──────────────────────────────────────────────────────────────────────────────────────

/// The account key that signs minted user JWTs and auth responses. Its public key is what the NATS
/// server config trusts as the auth-callout `issuer`. Load the seed from a mounted secret file
/// (`YAGRA_KEK_FILE`-style), never an env var (security.md, ADR-018).
pub struct AccountSigner {
    kp: nkeys::KeyPair,
    issuer: String,
    /// The account minted users are placed into (the user JWT's audience). Must match the server's
    /// callout account. Configurable because server-config vs named-account setups differ.
    account: String,
}

/// Why a request was refused. Surfaced to metrics/logs only — the response carries a generic message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    MissingSecret,
    BadSecret,
    MissingId,
    /// The connection named a poller id this deployment's inventory has never seen (ADR-065).
    ///
    /// Distinct from [`Self::BadSecret`] on purpose, and the distinction is the whole point of
    /// Increment 3: before it, the *only* check was a secret shared by every site, so a leak at one
    /// site let the holder claim **any** id — including one whose working set carries another
    /// site's device credentials. An id nobody registered is now refused before its secret is even
    /// compared.
    UnknownPoller,
}

impl DenyReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DenyReason::MissingSecret => "missing_secret",
            DenyReason::BadSecret => "bad_secret",
            DenyReason::MissingId => "missing_id",
            DenyReason::UnknownPoller => "unknown_poller",
        }
    }
}

/// What the connecting poller's secret must match (ADR-065).
///
/// The caller resolves this from the inventory — this crate stays free of a database — but the
/// **comparison stays here**, so there is exactly one place that decides what "the secret matched"
/// means and it is constant-time in every branch.
#[derive(Debug, Clone, Copy)]
pub enum Expected<'a> {
    /// This poller has a token of its own: compare `SHA-256(presented)` against this hex digest.
    TokenHash(&'a str),
    /// This poller is in the inventory but has no token yet, so the deployment-wide bootstrap
    /// secret still admits it.
    ///
    /// This is the N-1 path and it is deliberately not removed: every poller that connected before
    /// tokens existed is in this state, and refusing them would take a working fleet down on
    /// upgrade. Issuing a token is what moves an id out of it — which is why the WebUI shows, per
    /// poller, whether one has been issued.
    Shared(&'a str),
    /// Nothing in the inventory claims this id. Refused without comparing anything.
    Unknown,
}

/// A parsed callout request, waiting on the caller's inventory lookup.
///
/// Holds everything needed to answer — including the server id and user nkey a reply is addressed
/// with — so nothing has to be re-parsed after the lookup.
#[derive(Debug)]
pub struct PendingRequest {
    req: AuthRequest,
}

impl PendingRequest {
    /// The poller id the connection is claiming, if it presented one.
    ///
    /// **Self-asserted.** It is the key the caller looks up, and it is exactly why the lookup
    /// exists: nothing about the connection proves this id until its secret has been checked
    /// against the row it names.
    #[must_use]
    pub fn poller_id(&self) -> Option<&str> {
        self.req.connect_user.as_deref()
    }

    /// The pool the connection is claiming, defaulting the way the scope builder does.
    #[must_use]
    pub fn pool(&self) -> &str {
        self.req.connect_name.as_deref().unwrap_or("default")
    }
}

/// `SHA-256` of a presented secret, lowercase hex — the form the inventory stores.
///
/// Here rather than in the caller so the hash a token is *checked* against is produced by the same
/// code that decides the check passed. The issuing side calls this too (through core's
/// `token::token_hash`, which is byte-identical and asserted to be by a test in this crate).
#[must_use]
pub fn secret_hash(secret: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(secret.as_bytes());
    data_encoding::HEXLOWER.encode(&h.finalize())
}

/// Outcome of handling one callout request. `response_jwt` is always present (an approval or a signed
/// rejection) so the caller can always reply.
#[derive(Debug)]
pub struct Handled {
    pub response_jwt: String,
    pub decision: Decision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Issued { poller_id: String, pool: String },
    Denied { reason: DenyReason },
}

impl AccountSigner {
    /// Build a signer from an account seed (`SA...`). `account` is the NATS account minted users join.
    pub fn from_seed(seed: &str, account: impl Into<String>) -> Result<Self, AuthzError> {
        let kp = nkeys::KeyPair::from_seed(seed).map_err(|_| AuthzError::Nkey)?;
        let issuer = kp.public_key();
        Ok(Self {
            kp,
            issuer,
            account: account.into(),
        })
    }

    /// The account public key (`A...`) to put in the server config's `auth_callout { issuer }`.
    #[must_use]
    pub fn issuer_public_key(&self) -> &str {
        &self.issuer
    }

    /// Full flow for one request against the deployment-wide bootstrap secret.
    ///
    /// The pre-ADR-065 behaviour, kept because it is exactly what a deployment with no inventory
    /// lookup should do. A caller that *can* consult the inventory uses [`Self::parse`] +
    /// [`Self::decide`] instead, which is the only way to refuse an id nobody registered.
    ///
    /// # Errors
    /// A parse failure — we cannot even address a reply. An auth failure is not an error: it is a
    /// signed rejection (`Decision::Denied`) so the client gets a clean refusal instead of a hang.
    pub fn handle_request(
        &self,
        request_jwt: &str,
        bootstrap_secret: &str,
        iat: i64,
    ) -> Result<Handled, AuthzError> {
        let pending = Self::parse(request_jwt)?;
        self.decide(&pending, Expected::Shared(bootstrap_secret), iat)
    }

    /// Parse a callout request far enough to learn **who is claiming to connect**, without deciding
    /// anything.
    ///
    /// Split from [`Self::decide`] because the answer to "may this connection proceed?" now depends
    /// on a database row keyed by the id inside the request, and this crate holds no database. The
    /// caller parses, looks the id up, and hands the answer back as an [`Expected`].
    ///
    /// # Errors
    /// The request is not a callout request this build can read.
    pub fn parse(request_jwt: &str) -> Result<PendingRequest, AuthzError> {
        Ok(PendingRequest {
            req: parse_auth_request(request_jwt)?,
        })
    }

    /// Decide a parsed request: check the secret against `expected` → scope → mint + wrap.
    ///
    /// # Errors
    /// Signing or encoding failed. A refusal is a `Decision::Denied`, not an error.
    pub fn decide(
        &self,
        pending: &PendingRequest,
        expected: Expected<'_>,
        iat: i64,
    ) -> Result<Handled, AuthzError> {
        let req = &pending.req;

        let deny = |signer: &Self, reason: DenyReason| -> Result<Handled, AuthzError> {
            let response_jwt = signer.build_response(
                &req.server_id,
                &req.user_nkey,
                Err("authorization denied"),
                iat,
            )?;
            Ok(Handled {
                response_jwt,
                decision: Decision::Denied { reason },
            })
        };

        let Some(secret) = req.connect_pass.as_deref() else {
            return deny(self, DenyReason::MissingSecret);
        };
        // Before the secret comparison, deliberately. An id nobody registered is refused whatever
        // it presents, so a leaked bootstrap secret cannot be turned into "any id I like".
        let ok = match expected {
            Expected::Unknown => return deny(self, DenyReason::UnknownPoller),
            Expected::Shared(shared) => ct_eq(secret.as_bytes(), shared.as_bytes()),
            // Hashed rather than compared raw: the inventory stores only a digest, for the same
            // reason `api_tokens` does — a database dump must not yield a working credential.
            Expected::TokenHash(hash) => ct_eq(secret_hash(secret).as_bytes(), hash.as_bytes()),
        };
        if !ok {
            return deny(self, DenyReason::BadSecret);
        }
        let Some(id) = req.connect_user.as_deref() else {
            return deny(self, DenyReason::MissingId);
        };
        let pool = req.connect_name.as_deref().unwrap_or("default");
        let scope = PollerScope::new(id, pool);
        let perms = allow_list(&scope);
        let user_jwt = self.mint_user_jwt(&req.user_nkey, &perms, iat)?;
        let response_jwt =
            self.build_response(&req.server_id, &req.user_nkey, Ok(&user_jwt), iat)?;
        Ok(Handled {
            response_jwt,
            decision: Decision::Issued {
                poller_id: scope.id,
                pool: scope.pool,
            },
        })
    }

    /// Mint a signed NATS user JWT granting exactly `perms`, bound to `user_nkey`.
    pub(crate) fn mint_user_jwt(
        &self,
        user_nkey: &str,
        perms: &Permissions,
        iat: i64,
    ) -> Result<String, AuthzError> {
        let claims = UserClaims {
            jti: String::new(),
            iat,
            iss: self.issuer.clone(),
            sub: user_nkey.to_owned(),
            aud: self.account.clone(),
            nats: UserNats {
                publish: Perm {
                    allow: perms.publish.clone(),
                },
                subscribe: Perm {
                    allow: perms.subscribe.clone(),
                },
                subs: -1,
                data: -1,
                payload: -1,
                kind: "user",
                version: 2,
            },
        };
        self.encode_and_sign(&claims)
    }

    /// Wrap a minted user JWT (or an error) in the authorization-response envelope the server expects.
    fn build_response(
        &self,
        server_id: &str,
        user_nkey: &str,
        user_jwt: Result<&str, &str>,
        iat: i64,
    ) -> Result<String, AuthzError> {
        let nats = match user_jwt {
            Ok(jwt) => RespNats {
                jwt: Some(jwt.to_owned()),
                error: None,
                kind: "authorization_response",
                version: 2,
            },
            Err(msg) => RespNats {
                jwt: None,
                error: Some(RespError {
                    description: msg.to_owned(),
                }),
                kind: "authorization_response",
                version: 2,
            },
        };
        let claims = RespClaims {
            jti: String::new(),
            iat,
            iss: self.issuer.clone(),
            sub: user_nkey.to_owned(),
            aud: server_id.to_owned(),
            nats,
        };
        self.encode_and_sign(&claims)
    }

    /// Encode `claims` (which must carry a blank `jti`) as a NATS JWT: fill `jti` with the
    /// base32(sha256(body)) hash, then `base64url(header).base64url(body).base64url(ed25519_sig)`.
    fn encode_and_sign<T: Serialize>(&self, claims: &T) -> Result<String, AuthzError> {
        let mut val = serde_json::to_value(claims)?;
        // NATS derives `jti` from the hash of the claims with a blank id; the server does not
        // re-validate it, but we compute it in the canonical way for hygiene.
        let blank = serde_json::to_vec(&val)?;
        let hash = Sha256::digest(&blank);
        let jti = BASE32_NOPAD.encode(&hash);
        if let Some(obj) = val.as_object_mut() {
            obj.insert("jti".to_owned(), serde_json::Value::String(jti));
        }
        let header_b64 = BASE64URL_NOPAD.encode(&serde_json::to_vec(&HEADER)?);
        let body_b64 = BASE64URL_NOPAD.encode(&serde_json::to_vec(&val)?);
        let signing_input = format!("{header_b64}.{body_b64}");
        let sig = self
            .kp
            .sign(signing_input.as_bytes())
            .map_err(|_| AuthzError::Nkey)?;
        let sig_b64 = BASE64URL_NOPAD.encode(&sig);
        Ok(format!("{signing_input}.{sig_b64}"))
    }
}

/// Constant-time byte compare (length-equality is allowed to leak; content is not).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn signer() -> AccountSigner {
        let seed = nkeys::KeyPair::new_account().seed().unwrap();
        AccountSigner::from_seed(&seed, "$G").unwrap()
    }

    /// Decode a JWT body segment to a JSON value (tests only; no signature check).
    fn body(jwt: &str) -> Value {
        let seg = jwt.split('.').nth(1).unwrap();
        let raw = BASE64URL_NOPAD.decode(seg.as_bytes()).unwrap();
        serde_json::from_slice(&raw).unwrap()
    }

    fn fake_request_jwt(
        server_id: &str,
        user_nkey: &str,
        user: Option<&str>,
        pass: Option<&str>,
        name: Option<&str>,
    ) -> String {
        let mut connect = serde_json::Map::new();
        if let Some(u) = user {
            connect.insert("user".into(), u.into());
        }
        if let Some(p) = pass {
            connect.insert("pass".into(), p.into());
        }
        if let Some(n) = name {
            connect.insert("name".into(), n.into());
        }
        let claims = serde_json::json!({
            "nats": {
                "server_id": { "id": server_id },
                "user_nkey": user_nkey,
                "connect_opts": Value::Object(connect),
                "type": "authorization_request",
                "version": 2
            }
        });
        let h = BASE64URL_NOPAD.encode(br#"{"typ":"JWT","alg":"ed25519-nkey"}"#);
        let b = BASE64URL_NOPAD.encode(&serde_json::to_vec(&claims).unwrap());
        format!("{h}.{b}.fakesig")
    }

    #[test]
    fn allow_list_scopes_to_own_id_and_pool_only() {
        let perms = allow_list(&PollerScope::new("edge-1", "tokyo"));
        assert!(perms
            .subscribe
            .contains(&"yagra.poller.assign.edge-1".to_owned()));
        assert!(perms.subscribe.contains(&"yagra.jobs.tokyo".to_owned()));
        assert!(perms
            .subscribe
            .contains(&"yagra.discovery.jobs.tokyo".to_owned()));
        assert!(perms.subscribe.contains(&"_INBOX.>".to_owned()));
        // Global discovery is intentionally granted (core still publishes pool-less sweeps there).
        assert!(perms.subscribe.contains(&"yagra.discovery.jobs".to_owned()));
        // But NO cross-poller / cross-pool wildcard over the credential-bearing assign/jobs subjects.
        for s in &perms.subscribe {
            assert_ne!(s, "yagra.poller.assign.>");
            assert_ne!(s, "yagra.jobs.>");
            assert_ne!(s, "yagra.jobs.*");
        }
        // Publish set includes results.backfill (the store-and-forward valve) and flows (ADR-031 —
        // a remote-site poller running a flow listener must be able to publish edge-aggregated batches).
        assert!(perms.publish.contains(&"yagra.results".to_owned()));
        assert!(perms.publish.contains(&"yagra.results.backfill".to_owned()));
        assert!(perms.publish.contains(&"yagra.flows".to_owned()));
        // ...and flows.raw (ADR-034 Increment 2 — the verbatim relay that feeds flow forwarding).
        assert!(perms.publish.contains(&"yagra.flows.raw".to_owned()));
        assert!(perms.publish.contains(&"yagra.poller.heartbeat".to_owned()));
    }

    /// The support-log family (ADR-045 Inc.4), whose asymmetry is the security property: a poller
    /// may **answer** on the shared reply subject and may **be asked** only on its own id. The
    /// inverse of either would let one site pull another site's log body off the bus.
    #[test]
    fn a_poller_may_answer_a_log_request_but_never_issue_one() {
        let perms = allow_list(&PollerScope::new("edge-1", "tokyo"));
        assert!(perms
            .subscribe
            .contains(&"yagra.poller.logs.edge-1".to_owned()));
        assert!(perms.publish.contains(&"yagra.poller.logreply".to_owned()));
        // Never the other way round: no publish on any request subject…
        assert!(
            !perms
                .publish
                .iter()
                .any(|s| s.starts_with("yagra.poller.logs")),
            "publishing a request would let this site order another site to send its log"
        );
        // …and no subscribe that reaches beyond this poller's own request subject.
        for s in &perms.subscribe {
            assert_ne!(s, "yagra.poller.logs.>");
            assert_ne!(s, "yagra.poller.logs.*");
        }
    }

    #[test]
    fn allow_list_neutralizes_wildcard_injection() {
        // A hostile poller presenting `*` / `x.>` must not smuggle a wildcard into a granted subject.
        let perms = allow_list(&PollerScope::new("*", "x.>"));
        for s in perms.subscribe.iter().chain(perms.publish.iter()) {
            if s == "_INBOX.>" {
                continue; // the one legitimate wildcard (the client reply inbox)
            }
            assert!(!s.contains('*'), "wildcard leaked into {s}");
            assert!(!s.contains('>'), "wildcard leaked into {s}");
        }
    }

    #[test]
    fn minted_user_jwt_verifies_and_is_scoped() {
        let s = signer();
        let user = nkeys::KeyPair::new_user().public_key();
        let perms = allow_list(&PollerScope::new("edge-1", "tokyo"));
        let jwt = s.mint_user_jwt(&user, &perms, 1_700_000_000).unwrap();

        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "header.body.sig");
        // Signature verifies against the account public key over `header.body`.
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let sig = BASE64URL_NOPAD.decode(parts[2].as_bytes()).unwrap();
        let kp = nkeys::KeyPair::from_public_key(s.issuer_public_key()).unwrap();
        kp.verify(signing_input.as_bytes(), &sig).unwrap();

        let b = body(&jwt);
        assert_eq!(b["iss"], s.issuer_public_key());
        assert_eq!(b["sub"], user);
        assert_eq!(b["aud"], "$G");
        assert_eq!(b["nats"]["type"], "user");
        assert_eq!(b["nats"]["version"], 2);
        assert_eq!(b["nats"]["sub"]["allow"][0], "yagra.poller.assign.edge-1");
        assert!(!b["jti"].as_str().unwrap().is_empty());
    }

    #[test]
    fn handle_request_issues_for_good_secret() {
        let s = signer();
        let user = nkeys::KeyPair::new_user().public_key();
        let req = fake_request_jwt("NDX1", &user, Some("edge-1"), Some("s3cret"), Some("tokyo"));
        let out = s.handle_request(&req, "s3cret", 1_700_000_000).unwrap();
        assert_eq!(
            out.decision,
            Decision::Issued {
                poller_id: "edge-1".to_owned(),
                pool: "tokyo".to_owned()
            }
        );
        let resp = body(&out.response_jwt);
        assert_eq!(resp["nats"]["type"], "authorization_response");
        assert_eq!(resp["sub"], user);
        assert_eq!(resp["aud"], "NDX1");
        assert!(resp["nats"]["jwt"].is_string());
        assert!(resp["nats"]["error"].is_null());
        // The wrapped inner user JWT is scoped to this poller.
        let inner = body(resp["nats"]["jwt"].as_str().unwrap());
        assert_eq!(
            inner["nats"]["sub"]["allow"][0],
            "yagra.poller.assign.edge-1"
        );
    }

    #[test]
    fn handle_request_denies_bad_secret_without_jwt() {
        let s = signer();
        let user = nkeys::KeyPair::new_user().public_key();
        let req = fake_request_jwt("NDX1", &user, Some("edge-1"), Some("wrong"), Some("tokyo"));
        let out = s.handle_request(&req, "s3cret", 1_700_000_000).unwrap();
        assert_eq!(
            out.decision,
            Decision::Denied {
                reason: DenyReason::BadSecret
            }
        );
        let resp = body(&out.response_jwt);
        assert!(resp["nats"]["jwt"].is_null());
        assert_eq!(resp["nats"]["error"]["description"], "authorization denied");
    }

    #[test]
    fn handle_request_denies_missing_secret_and_missing_id() {
        let s = signer();
        let user = nkeys::KeyPair::new_user().public_key();
        let no_secret = fake_request_jwt("NDX1", &user, Some("edge-1"), None, Some("tokyo"));
        assert_eq!(
            s.handle_request(&no_secret, "s3cret", 1).unwrap().decision,
            Decision::Denied {
                reason: DenyReason::MissingSecret
            }
        );
        let no_id = fake_request_jwt("NDX1", &user, None, Some("s3cret"), Some("tokyo"));
        assert_eq!(
            s.handle_request(&no_id, "s3cret", 1).unwrap().decision,
            Decision::Denied {
                reason: DenyReason::MissingId
            }
        );
    }

    /// The whole point of ADR-065 Increment 3: an id nobody registered is refused **before** its
    /// secret is compared, so a bootstrap secret leaked at one site cannot be replayed under an
    /// arbitrary id — including one whose working set carries another site's device credentials.
    #[test]
    fn an_unregistered_id_is_refused_even_with_the_right_shared_secret() {
        let s = signer();
        let user = nkeys::KeyPair::new_user().public_key();
        let req = fake_request_jwt("NDX1", &user, Some("nobody"), Some("s3cret"), Some("tokyo"));
        let pending = AccountSigner::parse(&req).unwrap();
        assert_eq!(pending.poller_id(), Some("nobody"));
        assert_eq!(pending.pool(), "tokyo");
        let out = s.decide(&pending, Expected::Unknown, 1).unwrap();
        assert_eq!(
            out.decision,
            Decision::Denied {
                reason: DenyReason::UnknownPoller
            }
        );
        // The refusal reads differently from a wrong password in the metrics, which is what lets an
        // operator tell "somebody is guessing ids" from "one site has a stale .env".
        assert_eq!(DenyReason::UnknownPoller.as_str(), "unknown_poller");
        let resp = body(&out.response_jwt);
        assert!(resp["nats"]["jwt"].is_null());
    }

    /// A poller that has been issued a token presents that token, and the inventory holds only its
    /// digest — a database dump must not yield a working credential.
    #[test]
    fn a_poller_with_a_token_is_admitted_by_its_hash_and_by_nothing_else() {
        let s = signer();
        let user = nkeys::KeyPair::new_user().public_key();
        let token = "ypt_ThisIsAPollerToken";
        let hash = secret_hash(token);
        assert_ne!(hash, token, "the digest must not be the secret");

        let good = fake_request_jwt("NDX1", &user, Some("edge-1"), Some(token), Some("tokyo"));
        let out = s
            .decide(
                &AccountSigner::parse(&good).unwrap(),
                Expected::TokenHash(&hash),
                1,
            )
            .unwrap();
        assert_eq!(
            out.decision,
            Decision::Issued {
                poller_id: "edge-1".to_owned(),
                pool: "tokyo".to_owned()
            },
            "a poller presenting its own token must be admitted — a rejection-only test would pass \
             with everything refused"
        );

        // The deployment's shared bootstrap secret must NOT open a poller that has its own token.
        // Otherwise issuing one would be decoration: the weaker credential would still work.
        let shared = fake_request_jwt("NDX1", &user, Some("edge-1"), Some("s3cret"), Some("tokyo"));
        assert_eq!(
            s.decide(
                &AccountSigner::parse(&shared).unwrap(),
                Expected::TokenHash(&hash),
                1
            )
            .unwrap()
            .decision,
            Decision::Denied {
                reason: DenyReason::BadSecret
            }
        );
    }

    /// The hash this crate checks against has to be the one core's issuer produced. They are two
    /// implementations of `hex(sha256(x))` in two crates, and nothing else compares them.
    #[test]
    fn the_digest_is_lowercase_hex_sha256_the_way_core_stores_it() {
        // Known-answer, so a change to either side has something to fail against rather than
        // agreeing with itself.
        assert_eq!(
            secret_hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(secret_hash("").len(), 64);
        assert!(secret_hash("x")
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn missing_pool_defaults_to_default() {
        let s = signer();
        let user = nkeys::KeyPair::new_user().public_key();
        let req = fake_request_jwt("NDX1", &user, Some("edge-1"), Some("s3cret"), None);
        let out = s.handle_request(&req, "s3cret", 1).unwrap();
        assert_eq!(
            out.decision,
            Decision::Issued {
                poller_id: "edge-1".to_owned(),
                pool: "default".to_owned()
            }
        );
    }

    #[test]
    fn parse_auth_request_extracts_fields() {
        let req = fake_request_jwt("NDX9", "UABC", Some("edge-1"), Some("pw"), Some("osaka"));
        let parsed = parse_auth_request(&req).unwrap();
        assert_eq!(parsed.server_id, "NDX9");
        assert_eq!(parsed.user_nkey, "UABC");
        assert_eq!(parsed.connect_user.as_deref(), Some("edge-1"));
        assert_eq!(parsed.connect_pass.as_deref(), Some("pw"));
        assert_eq!(parsed.connect_name.as_deref(), Some("osaka"));
    }

    #[test]
    fn parse_rejects_malformed() {
        assert!(matches!(
            parse_auth_request("not-a-jwt"),
            Err(AuthzError::MalformedJwt)
        ));
    }

    #[test]
    fn ct_eq_matches_and_rejects() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
    }
}
