// SPDX-License-Identifier: AGPL-3.0-only
//! Stateless signed session tokens (Core HA active/active, ADR-016 Increment 2a).
//!
//! When a mounted session key is configured (`YAGRA_SESSION_KEY_FILE`), a successful login mints an
//! **HMAC-SHA256-signed** bearer token that carries its own claims (principal, user id, expiry)
//! instead of a random opaque token backed by per-process memory. Any core sharing the key can
//! verify it with a **synchronous, I/O-free** signature check — so the API `authorize()` path stays
//! synchronous (no `.await` ripple across its ~110 call sites) and a token minted on one core is
//! accepted on every core (and survives a core restart / failover). Revocation is handled separately
//! (a denylist fed by the `yagra.auth.revoke` fan-out + the `auth_revocations` table), because a
//! self-validating token cannot otherwise be invalidated before it expires.
//!
//! Wire format: `y2.<base64url(claims_json)>.<base64url(hmac)>`, where the HMAC is taken over the
//! `y2.<base64url(claims_json)>` prefix. The `y2.` prefix and the two dots distinguish a signed
//! token from the legacy 64-hex opaque token (which contains no dots), so both can coexist during a
//! rolling upgrade. **The signing key is never logged** (security.md / ADR-018).

use anyhow::{bail, Context};
use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use yagra_common::Principal;

type HmacSha256 = Hmac<Sha256>;

/// Version prefix / first subject-less token segment. A bump here is a token-format change.
const TOKEN_PREFIX: &str = "y2";

/// The self-contained claims embedded in a signed token. Serialized to compact JSON, base64url-
/// encoded, and HMAC-signed. `Principal`/`Role`/`Scope` are already `Serialize`/`Deserialize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// The account's user id (so a session can be revoked when that account changes).
    pub uid: Uuid,
    /// The authenticated principal (role + visibility scope).
    pub principal: Principal,
    /// The account name the token was minted for (audit attribution).
    pub username: String,
    /// Issued-at, Unix seconds. Used by the `revoke_user` cutoff (revoke tokens issued ≤ cutoff).
    pub iat: u64,
    /// Absolute expiry, Unix seconds. There is no sliding idle window for signed tokens (a stateless
    /// token cannot refresh itself) — the absolute lifetime is the only bound.
    pub exp: u64,
}

/// HMAC-SHA256 token signer/verifier over a mounted 32-byte key. Deliberately **not `Debug`** so the
/// key can never be printed.
#[derive(Clone)]
pub struct TokenSigner {
    key: [u8; 32],
}

impl TokenSigner {
    /// Build a signer from a raw 32-byte key.
    #[must_use]
    pub fn new(key: [u8; 32]) -> Self {
        Self { key }
    }

    /// Mint a signed token for `claims`.
    #[must_use]
    pub fn sign(&self, claims: &Claims) -> String {
        // to_vec on a plain struct of primitives cannot fail; encode defensively regardless.
        let json = serde_json::to_vec(claims).unwrap_or_default();
        let payload = BASE64URL_NOPAD.encode(&json);
        let signing_input = format!("{TOKEN_PREFIX}.{payload}");
        let sig = self.mac(signing_input.as_bytes());
        format!("{signing_input}.{}", BASE64URL_NOPAD.encode(&sig))
    }

    /// Verify a token's signature and structure, returning its claims. Does **not** check `exp` or
    /// the denylist — the caller (session store) applies those, so signature verification is testable
    /// independent of the clock. Constant-time comparison via `Mac::verify_slice`.
    #[must_use]
    pub fn verify(&self, token: &str) -> Option<Claims> {
        let mut segs = token.split('.');
        let p0 = segs.next()?;
        let p1 = segs.next()?;
        let p2 = segs.next()?;
        if segs.next().is_some() || p0 != TOKEN_PREFIX {
            return None; // exactly three segments, correct prefix
        }
        let sig = BASE64URL_NOPAD.decode(p2.as_bytes()).ok()?;
        let signing_input = format!("{p0}.{p1}");
        let mut mac = HmacSha256::new_from_slice(&self.key).ok()?;
        mac.update(signing_input.as_bytes());
        mac.verify_slice(&sig).ok()?; // constant-time; wrong key / tamper ⇒ None
        let json = BASE64URL_NOPAD.decode(p1.as_bytes()).ok()?;
        serde_json::from_slice::<Claims>(&json).ok()
    }

    fn mac(&self, msg: &[u8]) -> Vec<u8> {
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("HMAC accepts any key length");
        mac.update(msg);
        mac.finalize().into_bytes().to_vec()
    }
}

/// Whether `token` has the signed-token shape (`y2.` prefix). A cheap check used both to route a
/// bearer token to signature verification and to accept it at the well-formed-token edge.
#[must_use]
pub fn is_signed_shape(token: &str) -> bool {
    token.starts_with("y2.")
}

/// SHA-256 hex of a token — the stable, non-reversible key used in the revocation denylist and on the
/// `yagra.auth.revoke` wire, so the raw token is never stored or transmitted for revocation.
#[must_use]
pub fn token_hash(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    HEXLOWER.encode(&h.finalize())
}

/// Current Unix time in seconds (saturating on the impossible pre-epoch case).
#[must_use]
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Load the 32-byte session signing key from a mounted file (`YAGRA_SESSION_KEY_FILE`), accepting
/// either 64 hex chars or 32 raw bytes (surrounding whitespace trimmed). Like the KEK
/// (`YAGRA_KEK_FILE`), the *path* is the env var; the secret itself is a mounted file, never an env
/// var (security.md / ADR-018). Unlike the KEK, a configured-but-unreadable/invalid key is a hard
/// error (fail-closed): silently falling back to per-core in-memory sessions under a multi-core
/// expectation would be unsafe.
pub fn load_session_key(path: &str) -> anyhow::Result<[u8; 32]> {
    let raw = std::fs::read(path).with_context(|| format!("read session key file {path}"))?;
    parse_session_key(&raw).with_context(|| format!("parse session key file {path}"))
}

fn parse_session_key(raw: &[u8]) -> anyhow::Result<[u8; 32]> {
    // Trim ASCII whitespace (a trailing newline from `echo`/an editor is common).
    let start = raw.iter().position(|b| !b.is_ascii_whitespace());
    let end = raw.iter().rposition(|b| !b.is_ascii_whitespace());
    let trimmed = match (start, end) {
        (Some(s), Some(e)) => &raw[s..=e],
        _ => &[],
    };
    if trimmed.len() == 64 && trimmed.iter().all(u8::is_ascii_hexdigit) {
        let bytes = HEXLOWER
            .decode(trimmed.to_ascii_lowercase().as_slice())
            .context("session key is not valid hex")?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("decoded session key is not 32 bytes"))?;
        return Ok(arr);
    }
    if trimmed.len() == 32 {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(trimmed);
        return Ok(arr);
    }
    bail!(
        "session key must be 64 hex chars or 32 raw bytes (got {} bytes)",
        trimmed.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::{Role, Scope};

    fn signer() -> TokenSigner {
        TokenSigner::new([7u8; 32])
    }

    fn claims() -> Claims {
        Claims {
            uid: Uuid::from_u128(1),
            principal: Principal::new(Role::Operator, Scope::All),
            username: "alice".into(),
            iat: 1_700_000_000,
            exp: 1_700_086_400,
        }
    }

    #[test]
    fn sign_then_verify_round_trips() {
        let s = signer();
        let c = claims();
        let token = s.sign(&c);
        assert!(is_signed_shape(&token));
        let got = s.verify(&token).expect("valid token verifies");
        assert_eq!(got.uid, c.uid);
        assert_eq!(got.username, c.username);
        assert_eq!(got.iat, c.iat);
        assert_eq!(got.exp, c.exp);
        assert!(got.principal.can(yagra_common::Permission::View));
    }

    #[test]
    fn tampered_payload_fails_verification() {
        let s = signer();
        let token = s.sign(&claims());
        // Flip a character in the payload segment.
        let mut segs: Vec<&str> = token.split('.').collect();
        let mut payload: Vec<u8> = segs[1].bytes().collect();
        payload[0] ^= 0x01;
        let bad_payload = String::from_utf8_lossy(&payload).into_owned();
        segs[1] = &bad_payload;
        let tampered = segs.join(".");
        assert!(s.verify(&tampered).is_none());
    }

    #[test]
    fn wrong_key_fails_verification() {
        let token = signer().sign(&claims());
        let other = TokenSigner::new([9u8; 32]);
        assert!(other.verify(&token).is_none());
    }

    #[test]
    fn malformed_shapes_are_rejected() {
        let s = signer();
        assert!(s.verify("").is_none());
        assert!(s.verify("y2.only-two").is_none());
        assert!(s.verify("y2.a.b.c").is_none()); // four segments
        assert!(s.verify("xx.a.b").is_none()); // wrong prefix
                                               // A 64-hex opaque token is not signed-shaped and never verifies as a signed token.
        let opaque = "a".repeat(64);
        assert!(!is_signed_shape(&opaque));
        assert!(s.verify(&opaque).is_none());
    }

    #[test]
    fn token_hash_is_stable_and_hides_the_token() {
        let h = token_hash("secret-token");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(h, token_hash("secret-token"));
        assert_ne!(h, token_hash("secret-token2"));
        assert!(!h.contains("secret"));
    }

    #[test]
    fn parses_hex_and_raw_keys_and_rejects_bad_lengths() {
        let hex = "0".repeat(64);
        assert_eq!(parse_session_key(hex.as_bytes()).unwrap(), [0u8; 32]);
        // Trailing newline is trimmed.
        let with_nl = format!("{hex}\n");
        assert_eq!(parse_session_key(with_nl.as_bytes()).unwrap(), [0u8; 32]);
        // 32 raw bytes.
        let raw = [5u8; 32];
        assert_eq!(parse_session_key(&raw).unwrap(), raw);
        // Wrong length errors.
        assert!(parse_session_key(b"tooshort").is_err());
    }
}
