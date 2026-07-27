// SPDX-License-Identifier: AGPL-3.0-only
//! Google Cloud authentication: a stored service-account key — or the instance's own Workload
//! Identity — turned into a bearer token for a Google API.
//!
//! This started inside the BigQuery forwarding sender (ADR-034 Increment 3) and moved here when the
//! LLM RCA work (ADR-029) needed the identical handshake for Vertex AI. The two callers differ only
//! in the **OAuth scope**, so the scope is a parameter and everything else is shared: assertion
//! signing, the token cache, the metadata-server fallback, and the rule that a pasted key may only
//! name a Google token endpoint.
//!
//! **Authentication has two shapes and no third.**
//!
//! * A service-account key (the JSON Google hands out), stored envelope-encrypted like any other
//!   credential (ADR-018). The key never leaves this process: it is turned into a signing key when
//!   the [`TokenSource`] is built and the JSON is dropped. Each hour a self-signed RS256 assertion
//!   is exchanged at Google's token endpoint for an access token (the `jwt-bearer` grant).
//! * No key at all — then the GCE/GKE **metadata server** is asked for the instance's token, which
//!   is how Workload Identity works. This is the better deployment: nothing to store, nothing to
//!   rotate, nothing to leak. It only works when core actually runs on Google infrastructure.
//!
//! RS256 signing uses **`ring`**, not the `rsa` crate. `ring` is already in this tree (it is the
//! rustls crypto provider named explicitly by the forwarder's TLS setup), its RSA signing is
//! constant-time, and it sidesteps RUSTSEC-2023-0071 — the Marvin timing side-channel that
//! `deny.toml` currently ignores on the grounds that Yagra performs no RSA private-key operations.
//! Signing here with `rsa` would have quietly invalidated that rationale.
//!
//! **Log discipline** (security.md): the private key, the signed assertion and the access token are
//! never logged at any level, and neither are the free-text `message` strings in a Google error
//! body — those can echo the offending request, which may carry a syslog line with a credential in
//! it. Only the machine-readable `reason`/`location` pair is surfaced.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

/// Assertion lifetime. Google accepts up to one hour.
const ASSERTION_TTL_SECS: i64 = 3600;
/// Refresh a cached access token once this much of its life has elapsed, so a token never expires
/// mid-request.
const TOKEN_REFRESH_RATIO: f64 = 0.9;
/// Longest error text kept from a response body.
const MAX_ERROR_CHARS: usize = 300;

/// The GCE/GKE metadata server (link-local, only reachable on Google infrastructure).
const METADATA_BASE: &str = "http://metadata.google.internal";

/// Scope covering both `tables.insert` (schema creation) and `tabledata.insertAll` — BigQuery
/// forwarding destinations (ADR-034).
pub const SCOPE_BIGQUERY: &str = "https://www.googleapis.com/auth/bigquery";

// ── Credentials ──────────────────────────────────────────────────────────────────────────────

/// The service-account fields used for the `jwt-bearer` grant. Everything else in the key file
/// (project id, key id, cert URLs) is ignored.
#[derive(Debug, Deserialize)]
struct ServiceAccountKey {
    // Defaulted rather than required so a near-miss (an OAuth *client* JSON, say) gets an error
    // naming the missing field instead of serde's generic "this is not a key".
    #[serde(default)]
    client_email: String,
    #[serde(default)]
    private_key: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

fn default_token_uri() -> String {
    "https://oauth2.googleapis.com/token".to_owned()
}

/// How an access token is obtained.
enum Credentials {
    /// A stored service-account key, already turned into a signing key.
    ServiceAccount {
        client_email: String,
        token_uri: String,
        key: Arc<ring::signature::RsaKeyPair>,
    },
    /// The instance's own identity, from the GCE/GKE metadata server (Workload Identity).
    Metadata,
}

/// Validate a service-account JSON without keeping any of it — used by the API edge so a mistyped
/// key is a 400 rather than a destination that fails every send.
///
/// # Errors
/// Returns operator-facing text naming what is wrong with the key.
pub fn validate_service_account(json: &str) -> Result<(), String> {
    parse_service_account(json).map(drop)
}

fn parse_service_account(json: &str) -> Result<Credentials, String> {
    let key: ServiceAccountKey = serde_json::from_str(json).map_err(|_| {
        "that is not a Google service-account key (expected its JSON file)".to_owned()
    })?;
    if key.client_email.is_empty() {
        return Err("the service-account key has no client_email".to_owned());
    }
    if key.private_key.is_empty() {
        return Err("the service-account key has no private_key".to_owned());
    }
    // A pasted key names its own token endpoint. Pinning it to Google means a hostile key file
    // cannot redirect the signed assertion — which is a bearer credential for the account — to a
    // third party. Defence in depth: only a ManageConfig admin can write one in the first place.
    let host = key
        .token_uri
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_default();
    if !(host == "googleapis.com" || host.ends_with(".googleapis.com")) {
        return Err("the key's token_uri must be a Google endpoint".to_owned());
    }
    let signing_key = signing_key_from_pem(&key.private_key)?;
    Ok(Credentials::ServiceAccount {
        client_email: key.client_email,
        token_uri: key.token_uri,
        key: Arc::new(signing_key),
    })
}

/// PKCS#8 PEM (`-----BEGIN PRIVATE KEY-----`, which is what Google issues) → a `ring` signing key.
fn signing_key_from_pem(pem: &str) -> Result<ring::signature::RsaKeyPair, String> {
    use rustls::pki_types::pem::{Error as PemError, PemObject};
    let der = rustls::pki_types::PrivateKeyDer::from_pem_slice(pem.as_bytes()).map_err(|e| {
        match e {
            // `rustls-pemfile` returned `Ok(None)` for "well-formed PEM, no key in it"; the
            // pki-types reader folds that into a typed error, so keep the two messages distinct.
            PemError::NoItemsFound => "the key's private_key contains no PRIVATE KEY block",
            _ => "the key's private_key is not valid PEM",
        }
        .to_owned()
    })?;
    let rustls::pki_types::PrivateKeyDer::Pkcs8(pkcs8) = der else {
        return Err("the key's private_key must be PKCS#8 (BEGIN PRIVATE KEY)".to_owned());
    };
    // The error is deliberately not interpolated: `ring`'s `KeyRejected` text is harmless, but this
    // value is key material and the habit of formatting it into a string is not one worth having.
    ring::signature::RsaKeyPair::from_pkcs8(pkcs8.secret_pkcs8_der())
        .map_err(|_| "the key's private_key was rejected (expected a 2048-bit RSA key)".to_owned())
}

// ── Token source ─────────────────────────────────────────────────────────────────────────────

struct CachedToken {
    value: String,
    refresh_at: Instant,
}

/// Mints and caches access tokens for one Google API. Owned by a single task, so it needs no
/// locking; `service` and `scope` are what distinguish one caller's source from another's.
pub struct TokenSource {
    /// Named in operator-facing errors ("BigQuery rejected …", "Vertex AI rejected …").
    service: &'static str,
    scope: &'static str,
    creds: Credentials,
    metadata_base: String,
    cached: Option<CachedToken>,
}

impl TokenSource {
    /// Build a token source. `service_account_json` is the stored key; `None` selects Workload
    /// Identity via the metadata server.
    ///
    /// # Errors
    /// Returns operator-facing text when the key is unusable.
    pub fn new(
        service: &'static str,
        scope: &'static str,
        service_account_json: Option<&str>,
    ) -> Result<Self, String> {
        let creds = match service_account_json
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(json) => parse_service_account(json)?,
            None => Credentials::Metadata,
        };
        Ok(Self {
            service,
            scope,
            creds,
            metadata_base: METADATA_BASE.to_owned(),
            cached: None,
        })
    }

    /// A valid access token, minted on first use and re-used until it nears expiry.
    ///
    /// # Errors
    /// Returns operator-facing text on signing, network or authorization failure.
    pub async fn token(&mut self, http: &reqwest::Client) -> Result<String, String> {
        if let Some(cached) = self.cached.as_ref() {
            if Instant::now() < cached.refresh_at {
                return Ok(cached.value.clone());
            }
        }
        let (value, ttl) = match &self.creds {
            Credentials::ServiceAccount {
                client_email,
                token_uri,
                key,
            } => {
                let assertion = signed_assertion(client_email, token_uri, self.scope, key)?;
                let res = http
                    .post(token_uri)
                    .form(&[
                        ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                        ("assertion", &assertion),
                    ])
                    .send()
                    .await
                    .map_err(|e| format!("Google token endpoint: {e}"))?;
                if !res.status().is_success() {
                    return Err(api_error(self.service, "requesting an access token", res).await);
                }
                read_token(res).await?
            }
            Credentials::Metadata => {
                let url = format!(
                    "{}/computeMetadata/v1/instance/service-accounts/default/token",
                    self.metadata_base
                );
                let res = http
                    .get(url)
                    .header("Metadata-Flavor", "Google")
                    .send()
                    .await
                    .map_err(|e| {
                        format!(
                            "no service-account key is set and the GCE metadata server is \
                             unreachable ({e}) — set a key, or run core on Google infrastructure \
                             with Workload Identity"
                        )
                    })?;
                if !res.status().is_success() {
                    return Err(api_error(
                        self.service,
                        "asking the metadata server for a token",
                        res,
                    )
                    .await);
                }
                read_token(res).await?
            }
        };
        let lifetime = Duration::from_secs(ttl).mul_f64(TOKEN_REFRESH_RATIO);
        self.cached = Some(CachedToken {
            value: value.clone(),
            refresh_at: Instant::now() + lifetime,
        });
        Ok(value)
    }

    /// Drop the cached token so the next call re-mints, rather than replaying one the server has
    /// stopped accepting (rotated key, revoked binding, clock skew).
    pub fn invalidate(&mut self) {
        self.cached = None;
    }

    /// Point the source at a local stand-in for the metadata server. Test-only: the production
    /// endpoint is a constant precisely so no config field can redirect a token request.
    #[cfg(test)]
    pub fn set_metadata_base(&mut self, base: String) {
        self.metadata_base = base;
    }

    /// Redirect the assertion exchange at a local stand-in. Test-only, for the same reason — the
    /// real `token_uri` comes from the key and is pinned to a Google host by
    /// [`parse_service_account`].
    #[cfg(test)]
    pub fn set_token_uri(&mut self, uri: String) {
        if let Credentials::ServiceAccount { token_uri, .. } = &mut self.creds {
            *token_uri = uri;
        }
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default = "default_expiry")]
    expires_in: u64,
}

const fn default_expiry() -> u64 {
    3600
}

async fn read_token(res: reqwest::Response) -> Result<(String, u64), String> {
    let token: TokenResponse = res
        .json()
        .await
        .map_err(|_| "the token response was not readable JSON".to_owned())?;
    if token.access_token.is_empty() {
        return Err("the token response carried no access_token".to_owned());
    }
    Ok((token.access_token, token.expires_in.max(60)))
}

/// Build and sign the RS256 assertion Google exchanges for an access token.
fn signed_assertion(
    client_email: &str,
    token_uri: &str,
    scope: &str,
    key: &ring::signature::RsaKeyPair,
) -> Result<String, String> {
    let now = chrono::Utc::now().timestamp();
    let header = json!({ "alg": "RS256", "typ": "JWT" });
    let claims = json!({
        "iss": client_email,
        "scope": scope,
        "aud": token_uri,
        "iat": now,
        "exp": now + ASSERTION_TTL_SECS,
    });
    let signing_input = format!("{}.{}", b64(&header)?, b64(&claims)?);
    let mut signature = vec![0u8; key.public().modulus_len()];
    key.sign(
        &ring::signature::RSA_PKCS1_SHA256,
        &ring::rand::SystemRandom::new(),
        signing_input.as_bytes(),
        &mut signature,
    )
    .map_err(|_| "signing the Google assertion failed".to_owned())?;
    Ok(format!(
        "{signing_input}.{}",
        data_encoding::BASE64URL_NOPAD.encode(&signature)
    ))
}

fn b64(value: &Value) -> Result<String, String> {
    let bytes = serde_json::to_vec(value).map_err(|e| format!("encoding the assertion: {e}"))?;
    Ok(data_encoding::BASE64URL_NOPAD.encode(&bytes))
}

/// Turn a failed Google response into operator-facing text.
///
/// Google's error bodies are echoed back **selectively**: the `reason`/`location` pair describes the
/// schema, but a per-row `message` can quote the offending value — and a forwarded row can hold a
/// syslog body with a credential in it. So the message strings are dropped rather than surfaced or
/// logged (security.md).
pub async fn api_error(service: &str, what: &str, res: reqwest::Response) -> String {
    let status = res.status();
    let detail = res
        .json::<Value>()
        .await
        .ok()
        .and_then(|body| safe_reason(&body));
    match detail {
        Some(reason) => format!("{service} rejected {what}: {status} ({reason})"),
        None => format!("{service} rejected {what}: {status}"),
    }
}

/// The machine-readable half of a Google error — `reason` (and `location` when present), never the
/// free-text `message`.
fn safe_reason(body: &Value) -> Option<String> {
    let err = body.get("error")?;
    let first = err.get("errors").and_then(Value::as_array)?.first()?;
    let reason = first.get("reason").and_then(Value::as_str)?;
    let text = match first.get("location").and_then(Value::as_str) {
        Some(loc) if !loc.is_empty() => format!("{reason} at {loc}"),
        _ => reason.to_owned(),
    };
    Some(text.chars().take(MAX_ERROR_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway 2048-bit RSA key in PKCS#8 PEM, generated for these tests only. It signs
    /// assertions against a fake token endpoint and has never been near a Google project.
    const TEST_KEY_PEM: &str = include_str!("testdata/bq_test_key.pem");

    fn sa_json(token_uri: &str) -> String {
        json!({
            "type": "service_account",
            "project_id": "test-project",
            "client_email": "yagra@test-project.iam.gserviceaccount.com",
            "private_key": TEST_KEY_PEM,
            "token_uri": token_uri,
        })
        .to_string()
    }

    #[test]
    fn a_well_formed_service_account_key_is_accepted() {
        validate_service_account(&sa_json("https://oauth2.googleapis.com/token")).unwrap();
    }

    #[test]
    fn a_key_whose_token_uri_is_not_google_is_refused() {
        // The assertion is a bearer credential for the account. A pasted key must not be able to
        // redirect it to a collector of someone else's choosing.
        let err = validate_service_account(&sa_json("https://evil.example.com/token"))
            .expect_err("a non-Google token_uri must be refused");
        assert!(err.contains("Google endpoint"), "{err}");
        // ...and neither may a plaintext one.
        assert!(validate_service_account(&sa_json("http://oauth2.googleapis.com/token")).is_err());
    }

    #[test]
    fn a_broken_key_is_refused_without_echoing_key_material() {
        let cases = [
            ("{}", "client_email"),
            (
                r#"{"client_email":"a@b.com","private_key":"not a pem"}"#,
                "PRIVATE KEY",
            ),
            (r#"{"private_key":"x"}"#, "client_email"),
            ("this is not json", "service-account key"),
        ];
        for (json, expect) in cases {
            let err = validate_service_account(json).expect_err("should be refused");
            assert!(err.contains(expect), "{err} (from {json})");
            assert!(
                !err.contains("BEGIN") && !err.contains("MII"),
                "the error must not quote key material: {err}"
            );
        }
    }

    #[test]
    fn an_assertion_is_three_base64url_parts_with_the_expected_claims() {
        let Credentials::ServiceAccount {
            client_email,
            token_uri,
            key,
        } = parse_service_account(&sa_json("https://oauth2.googleapis.com/token")).unwrap()
        else {
            panic!("expected a service-account credential");
        };
        let jwt = signed_assertion(&client_email, &token_uri, SCOPE_BIGQUERY, &key).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        // base64url, unpadded — a '+', '/' or '=' would be rejected by Google.
        assert!(!jwt.contains('+') && !jwt.contains('/') && !jwt.contains('='));
        let claims: Value = serde_json::from_slice(
            &data_encoding::BASE64URL_NOPAD
                .decode(parts[1].as_bytes())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(claims["iss"], json!(client_email));
        assert_eq!(claims["aud"], json!(token_uri));
        assert_eq!(claims["scope"], json!(SCOPE_BIGQUERY));
        assert_eq!(
            claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap(),
            ASSERTION_TTL_SECS
        );
        // The signature is 256 bytes for a 2048-bit key.
        assert_eq!(
            data_encoding::BASE64URL_NOPAD
                .decode(parts[2].as_bytes())
                .unwrap()
                .len(),
            256
        );
    }

    #[test]
    fn the_scope_travels_into_the_assertion() {
        // The whole reason this module is shared: BigQuery and Vertex differ only here, so a
        // caller's scope must reach the claim rather than a constant baked in at extraction time.
        let Credentials::ServiceAccount {
            client_email,
            token_uri,
            key,
        } = parse_service_account(&sa_json("https://oauth2.googleapis.com/token")).unwrap()
        else {
            panic!("expected a service-account credential");
        };
        let jwt = signed_assertion(
            &client_email,
            &token_uri,
            "https://www.googleapis.com/auth/cloud-platform",
            &key,
        )
        .unwrap();
        let claims: Value = serde_json::from_slice(
            &data_encoding::BASE64URL_NOPAD
                .decode(jwt.split('.').nth(1).unwrap().as_bytes())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            claims["scope"],
            json!("https://www.googleapis.com/auth/cloud-platform")
        );
    }

    #[test]
    fn api_errors_surface_the_reason_but_never_googles_message_text() {
        // A per-row `message` can quote the offending value, and a forwarded row can hold a syslog
        // body with a credential in it.
        let body = json!({
            "error": {
                "code": 400,
                "message": "Invalid value for field message: password=hunter2",
                "errors": [{
                    "reason": "invalid",
                    "location": "message",
                    "message": "Invalid value: password=hunter2",
                }]
            }
        });
        let reason = safe_reason(&body).unwrap();
        assert_eq!(reason, "invalid at message");
        assert!(!reason.contains("hunter2"));
        // A body with no structured error yields nothing rather than the free text.
        assert!(safe_reason(&json!({ "error": { "message": "boom" } })).is_none());
    }
}
