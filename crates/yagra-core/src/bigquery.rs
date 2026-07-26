// SPDX-License-Identifier: AGPL-3.0-only
//! Google BigQuery client for forwarding destinations (ADR-034 Increment 3).
//!
//! Every other forwarding destination reproduces a datagram on a socket. This one streams
//! **normalized rows** into a table via `tabledata.insertAll`, which is what makes BigQuery the
//! right tool when the question is analytical ("which devices logged an auth failure last week")
//! rather than operational ("mirror my syslog to the SIEM").
//!
//! **Authentication has two shapes and no third.**
//!
//! * A service-account key (the JSON Google hands out), stored envelope-encrypted like any other
//!   credential (ADR-018). The key never leaves this process: it is turned into a signing key at
//!   sender start and the JSON is dropped. Each hour a self-signed RS256 assertion is exchanged at
//!   Google's token endpoint for an access token (the `jwt-bearer` grant).
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
//! **The dataset is never created; the table is.** A dataset carries a region that cannot be
//! changed afterwards, so silently creating one would pick data residency on the operator's behalf
//! and be irreversible. A missing dataset is an error that says so. A missing table is created with
//! DAY partitioning and clustering from [`yagra_forward::bqrow`].
//!
//! **Log discipline** (security.md): the private key, the signed assertion and the access token are
//! never logged at any level, and neither are BigQuery's per-row error `message` strings — those
//! can echo the offending row, and a forwarded row can contain a syslog body with a credential in
//! it. Only the machine-readable `reason`/`location` pair is surfaced.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use yagra_forward::SourceKind;

/// Most rows put in one `insertAll` request. BigQuery's documented recommendation is 500; its hard
/// cap is 10 000 rows / 10 MB per request.
pub const MAX_ROWS_PER_INSERT: usize = 500;
/// Byte ceiling for one batch's rows, well under BigQuery's 10 MB request limit so the JSON
/// envelope and headers cannot push a legal batch over it.
pub const MAX_INSERT_BYTES: usize = 5 * 1024 * 1024;
/// How long a partly-filled batch waits for company before being sent anyway.
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(5);

/// Assertion lifetime. Google accepts up to one hour.
const ASSERTION_TTL_SECS: i64 = 3600;
/// Refresh a cached access token once this much of its life has elapsed, so a token never expires
/// mid-request.
const TOKEN_REFRESH_RATIO: f64 = 0.9;
/// Bound on every call to Google so a hung endpoint cannot park the sender task.
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest error text kept from a response body.
const MAX_ERROR_CHARS: usize = 300;

/// Google's public API host. Not configurable: there is no legitimate reason to point a BigQuery
/// destination at another host, and making it settable would turn a config field into an
/// exfiltration channel for rows that may contain credentials.
const API_BASE: &str = "https://bigquery.googleapis.com/bigquery/v2";
/// The GCE/GKE metadata server (link-local, only reachable on Google infrastructure).
const METADATA_BASE: &str = "http://metadata.google.internal";
/// Scope covering both `tables.insert` (schema creation) and `tabledata.insertAll`.
const SCOPE: &str = "https://www.googleapis.com/auth/bigquery";

// ── Target ───────────────────────────────────────────────────────────────────────────────────

/// A destination's `project.dataset.table`, as typed by the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigQueryTarget {
    /// GCP project id. May itself contain dots for a legacy domain-scoped project.
    pub project: String,
    /// Dataset id.
    pub dataset: String,
    /// Table id.
    pub table: String,
}

impl BigQueryTarget {
    /// Parse `project.dataset.table`.
    ///
    /// Split from the **right**, because a legacy domain-scoped project id (`example.com:proj`)
    /// contains dots of its own — splitting from the left would put half the project name in the
    /// dataset field and produce a 404 nobody could explain.
    ///
    /// # Errors
    /// Returns operator-facing text when the shape or the identifier characters are wrong.
    pub fn parse(target: &str) -> Result<Self, String> {
        let mut parts = target.trim().rsplitn(3, '.');
        let table = parts.next().unwrap_or_default();
        let dataset = parts.next().unwrap_or_default();
        let project = parts.next().unwrap_or_default();
        if project.is_empty() || dataset.is_empty() || table.is_empty() {
            return Err("expected project.dataset.table".to_owned());
        }
        for (label, id) in [("dataset", dataset), ("table", table)] {
            if id.len() > 1024 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(format!(
                    "{label} name may only contain letters, digits and underscores"
                ));
            }
        }
        if project.len() > 100
            || !project
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        {
            return Err("project id contains characters GCP does not allow".to_owned());
        }
        Ok(Self {
            project: project.to_owned(),
            dataset: dataset.to_owned(),
            table: table.to_owned(),
        })
    }

    fn dataset_url(&self, base: &str) -> String {
        format!(
            "{base}/projects/{}/datasets/{}",
            enc(&self.project),
            enc(&self.dataset)
        )
    }

    fn tables_url(&self, base: &str) -> String {
        format!("{}/tables", self.dataset_url(base))
    }

    fn table_url(&self, base: &str) -> String {
        format!("{}/{}", self.tables_url(base), enc(&self.table))
    }
}

/// Percent-encode the characters that can legally appear in a GCP id and would otherwise change the
/// URL's shape. Identifier charsets are already validated by [`BigQueryTarget::parse`]; this exists
/// so the one legal oddity (`:` in a domain-scoped project) survives the round trip.
fn enc(s: &str) -> String {
    s.replace(':', "%3A")
}

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

// ── Client ───────────────────────────────────────────────────────────────────────────────────

struct CachedToken {
    value: String,
    refresh_at: Instant,
}

/// One destination's BigQuery client: credentials, token cache, and whether the table has been
/// checked. Owned by a single sender task, so it needs no locking.
pub struct BigQueryClient {
    http: reqwest::Client,
    target: BigQueryTarget,
    creds: Credentials,
    api_base: String,
    metadata_base: String,
    token: Option<CachedToken>,
    table_ready: bool,
}

impl BigQueryClient {
    /// Build a client. `service_account_json` is the stored key; `None` selects Workload Identity.
    ///
    /// # Errors
    /// Returns operator-facing text when the key is unusable or the target is malformed.
    pub fn new(target: &str, service_account_json: Option<&str>) -> Result<Self, String> {
        let target = BigQueryTarget::parse(target)?;
        let creds = match service_account_json
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(json) => parse_service_account(json)?,
            None => Credentials::Metadata,
        };
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|e| format!("HTTP client: {e}"))?;
        Ok(Self {
            http,
            target,
            creds,
            api_base: API_BASE.to_owned(),
            metadata_base: METADATA_BASE.to_owned(),
            token: None,
            table_ready: false,
        })
    }

    /// Point the client at a local stand-in for Google. Test-only: the production endpoints are
    /// constants precisely so a config field cannot redirect rows off to somewhere else.
    #[cfg(test)]
    fn with_endpoints(mut self, api_base: String, metadata_base: String) -> Self {
        self.api_base = api_base;
        self.metadata_base = metadata_base;
        self
    }

    /// Create the destination table if it is missing, once per client. The dataset must already
    /// exist — see the module docs for why this does not create one.
    ///
    /// # Errors
    /// Returns operator-facing text on auth, permission or API failure.
    pub async fn ensure_table(&mut self, source: SourceKind) -> Result<(), String> {
        if self.table_ready {
            return Ok(());
        }
        let token = self.access_token().await?;
        let url = self.target.table_url(&self.api_base);
        let res = self
            .http
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| format!("BigQuery: {e}"))?;
        if res.status().is_success() {
            self.table_ready = true;
            return Ok(());
        }
        if res.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(api_error("checking the table", res).await);
        }

        // Distinguish "no table" (we create it) from "no dataset" (the operator must, because the
        // dataset carries an unchangeable region).
        let ds = self
            .http
            .get(self.target.dataset_url(&self.api_base))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| format!("BigQuery: {e}"))?;
        if ds.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(format!(
                "dataset {} does not exist — create it first, in the region you want the data to \
                 live in (a dataset's location cannot be changed later)",
                self.target.dataset
            ));
        }
        if !ds.status().is_success() {
            return Err(api_error("checking the dataset", ds).await);
        }

        let (schema, partition, clustering) = table_shape(source);
        let body = json!({
            "tableReference": {
                "projectId": self.target.project,
                "datasetId": self.target.dataset,
                "tableId": self.target.table,
            },
            "description": "Written by Yagra forwarding (ADR-034).",
            "schema": { "fields": schema },
            "timePartitioning": { "type": "DAY", "field": partition },
            "clustering": { "fields": clustering },
        });
        let created = self
            .http
            .post(self.target.tables_url(&self.api_base))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("BigQuery: {e}"))?;
        // 409 means another core (or another sender for the same table) won the race — which is
        // exactly the outcome we wanted.
        if created.status().is_success() || created.status() == reqwest::StatusCode::CONFLICT {
            tracing::info!(
                table = %format!("{}.{}", self.target.dataset, self.target.table),
                "BigQuery destination table ready"
            );
            self.table_ready = true;
            return Ok(());
        }
        Err(api_error("creating the table", created).await)
    }

    /// Stream `rows` (already-built `insertAll` envelopes) into the table. Returns how many rows
    /// BigQuery rejected — the request itself succeeded, so those are a data problem, not a
    /// transport one.
    ///
    /// # Errors
    /// Returns operator-facing text when the request fails outright.
    pub async fn insert_rows(&mut self, rows: &[Value]) -> Result<usize, String> {
        if rows.is_empty() {
            return Ok(0);
        }
        let token = self.access_token().await?;
        let body = json!({
            "kind": "bigquery#tableDataInsertAllRequest",
            // A single bad row must not cost the batch. Forwarding is a best-effort tier; losing
            // 499 good rows because the 500th had an out-of-range value would be the worse failure.
            "skipInvalidRows": true,
            // ...and an older table (one created before a column was added) must still accept what
            // it does understand, rather than rejecting every batch until someone migrates it.
            "ignoreUnknownValues": true,
            "rows": rows,
        });
        let res = self
            .http
            .post(format!(
                "{}/insertAll",
                self.target.table_url(&self.api_base)
            ))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("BigQuery: {e}"))?;
        let status = res.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            // Drop the cached token so the next attempt re-mints rather than replaying a token the
            // server has stopped accepting (rotated key, revoked binding, clock skew).
            self.token = None;
            return Err(api_error("inserting rows", res).await);
        }
        if !status.is_success() {
            return Err(api_error("inserting rows", res).await);
        }
        let payload: Value = res
            .json()
            .await
            .map_err(|e| format!("BigQuery returned an unreadable response: {e}"))?;
        Ok(count_insert_errors(&payload))
    }

    async fn access_token(&mut self) -> Result<String, String> {
        if let Some(cached) = self.token.as_ref() {
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
                let assertion = signed_assertion(client_email, token_uri, key)?;
                let res = self
                    .http
                    .post(token_uri)
                    .form(&[
                        ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                        ("assertion", &assertion),
                    ])
                    .send()
                    .await
                    .map_err(|e| format!("Google token endpoint: {e}"))?;
                if !res.status().is_success() {
                    return Err(api_error("requesting an access token", res).await);
                }
                read_token(res).await?
            }
            Credentials::Metadata => {
                let url = format!(
                    "{}/computeMetadata/v1/instance/service-accounts/default/token",
                    self.metadata_base
                );
                let res = self
                    .http
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
                    return Err(api_error("asking the metadata server for a token", res).await);
                }
                read_token(res).await?
            }
        };
        let lifetime = Duration::from_secs(ttl).mul_f64(TOKEN_REFRESH_RATIO);
        self.token = Some(CachedToken {
            value: value.clone(),
            refresh_at: Instant::now() + lifetime,
        });
        Ok(value)
    }
}

/// Schema, partition column and clustering for a destination's stream.
fn table_shape(source: SourceKind) -> (Value, &'static str, Vec<&'static str>) {
    match source {
        SourceKind::Flow => (
            yagra_forward::flow_schema(),
            yagra_forward::FLOW_PARTITION_FIELD,
            yagra_forward::FLOW_CLUSTERING.to_vec(),
        ),
        SourceKind::Syslog | SourceKind::Trap => (
            yagra_forward::event_schema(),
            yagra_forward::EVENT_PARTITION_FIELD,
            yagra_forward::EVENT_CLUSTERING.to_vec(),
        ),
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
    key: &ring::signature::RsaKeyPair,
) -> Result<String, String> {
    let now = chrono::Utc::now().timestamp();
    let header = json!({ "alg": "RS256", "typ": "JWT" });
    let claims = json!({
        "iss": client_email,
        "scope": SCOPE,
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

/// How many rows an `insertAll` response reports as rejected.
fn count_insert_errors(payload: &Value) -> usize {
    payload
        .get("insertErrors")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

/// Turn a failed response into operator-facing text.
///
/// Google's error bodies are echoed back **selectively**: the `reason`/`location` pair describes the
/// schema, but a per-row `message` can quote the offending value — and a forwarded row can hold a
/// syslog body with a credential in it. So the message strings are dropped rather than surfaced or
/// logged (security.md).
async fn api_error(what: &str, res: reqwest::Response) -> String {
    let status = res.status();
    let detail = res
        .json::<Value>()
        .await
        .ok()
        .and_then(|body| safe_reason(&body));
    match detail {
        Some(reason) => format!("BigQuery rejected {what}: {status} ({reason})"),
        None => format!("BigQuery rejected {what}: {status}"),
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
    use std::net::SocketAddr;
    use std::sync::Mutex;
    use tokio::net::TcpListener;

    // ── Target parsing ───────────────────────────────────────────────────────────────────────

    #[test]
    fn a_plain_target_splits_into_three() {
        let t = BigQueryTarget::parse("my-project.analytics.yagra_events").unwrap();
        assert_eq!(t.project, "my-project");
        assert_eq!(t.dataset, "analytics");
        assert_eq!(t.table, "yagra_events");
    }

    #[test]
    fn a_domain_scoped_project_keeps_its_dots() {
        // Splitting from the left would make the project "example", the dataset "com:proj" and
        // produce a 404 with no clue why. The last two components are always dataset and table.
        let t = BigQueryTarget::parse("example.com:proj.analytics.events").unwrap();
        assert_eq!(t.project, "example.com:proj");
        assert_eq!(t.dataset, "analytics");
        assert_eq!(t.table, "events");
        // ...and the colon is escaped rather than changing the URL's shape.
        assert!(t.table_url("https://x").contains("example.com%3Aproj"));
    }

    #[test]
    fn malformed_targets_are_rejected_with_operator_facing_text() {
        for bad in [
            "",
            "project",
            "project.dataset",
            "project..table",
            ".dataset.table",
            "project.data-set.table", // hyphens are legal in a project id, not a dataset id
            "project.dataset.tab le",
        ] {
            assert!(
                BigQueryTarget::parse(bad).is_err(),
                "{bad} should not parse"
            );
        }
        assert!(BigQueryTarget::parse("p.d.t").is_ok());
    }

    // ── Credentials ──────────────────────────────────────────────────────────────────────────

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
        let jwt = signed_assertion(&client_email, &token_uri, &key).unwrap();
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
        assert_eq!(claims["scope"], json!(SCOPE));
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

    // ── A fake Google, for the request-shaping tests ─────────────────────────────────────────

    /// Records every request it serves and replies from a scripted queue of `(status, body)`.
    #[derive(Default)]
    struct FakeGoogle {
        seen: Mutex<Vec<(String, String, String)>>, // method, path, body
        replies: Mutex<Vec<(u16, String)>>,
    }

    impl FakeGoogle {
        fn with(replies: Vec<(u16, String)>) -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(Vec::new()),
                replies: Mutex::new(replies),
            })
        }

        fn requests(&self) -> Vec<(String, String, String)> {
            self.seen.lock().unwrap().clone()
        }
    }

    /// Minimal HTTP/1.1 server: enough to answer reqwest, not a general implementation.
    async fn serve(fake: Arc<FakeGoogle>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let fake = fake.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    // Read until the body is complete (headers + Content-Length).
                    loop {
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf).to_string();
                        let Some(head_end) = text.find("\r\n\r\n") else {
                            continue;
                        };
                        let len: usize = text
                            .to_ascii_lowercase()
                            .split("content-length:")
                            .nth(1)
                            .and_then(|r| r.split("\r\n").next())
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(0);
                        if buf.len() >= head_end + 4 + len {
                            break;
                        }
                    }
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let mut lines = text.lines();
                    let first = lines.next().unwrap_or_default().to_owned();
                    let mut it = first.split_whitespace();
                    let method = it.next().unwrap_or_default().to_owned();
                    let path = it.next().unwrap_or_default().to_owned();
                    let body = text.split("\r\n\r\n").nth(1).unwrap_or_default().to_owned();
                    fake.seen.lock().unwrap().push((method, path, body));
                    let (status, payload) = {
                        let mut replies = fake.replies.lock().unwrap();
                        if replies.is_empty() {
                            (200, "{}".to_owned())
                        } else {
                            replies.remove(0)
                        }
                    };
                    let res = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = sock.write_all(res.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        addr
    }

    fn token_body() -> String {
        json!({ "access_token": "ya29.test", "expires_in": 3600 }).to_string()
    }

    async fn client_with(fake: &Arc<FakeGoogle>, sa: Option<String>) -> BigQueryClient {
        let addr = serve(fake.clone()).await;
        let base = format!("http://{addr}");
        BigQueryClient::new("proj.ds.tbl", sa.as_deref())
            .unwrap()
            .with_endpoints(format!("{base}/bigquery/v2"), base)
    }

    #[tokio::test]
    async fn workload_identity_asks_the_metadata_server_with_the_required_header() {
        // Without a key the only correct behaviour is to ask the instance for its own token —
        // silently doing nothing, or falling back to unauthenticated requests, would be worse.
        let fake = FakeGoogle::with(vec![(200, token_body()), (200, "{}".to_owned())]);
        let mut client = client_with(&fake, None).await;
        client.ensure_table(SourceKind::Syslog).await.unwrap();
        let reqs = fake.requests();
        assert!(
            reqs[0]
                .1
                .contains("/computeMetadata/v1/instance/service-accounts/default/token"),
            "{:?}",
            reqs[0]
        );
    }

    #[tokio::test]
    async fn a_service_account_exchanges_a_signed_assertion_for_a_token() {
        let fake = FakeGoogle::with(vec![(200, token_body()), (200, "{}".to_owned())]);
        let addr = serve(fake.clone()).await;
        let base = format!("http://{addr}");
        // The token_uri check requires a Google host, so the endpoint override is what lets the
        // exchange be exercised at all — the credential itself still has to name Google.
        let mut client = BigQueryClient::new(
            "proj.ds.tbl",
            Some(&sa_json("https://oauth2.googleapis.com/token")),
        )
        .unwrap()
        .with_endpoints(format!("{base}/bigquery/v2"), base.clone());
        let Credentials::ServiceAccount { key, .. } = &client.creds else {
            panic!("expected a service-account credential");
        };
        let key = key.clone();
        // Redirect the exchange at the fake by rewriting the credential's endpoint.
        client.creds = Credentials::ServiceAccount {
            client_email: "yagra@test-project.iam.gserviceaccount.com".to_owned(),
            token_uri: format!("{base}/token"),
            key,
        };
        client.ensure_table(SourceKind::Syslog).await.unwrap();
        let reqs = fake.requests();
        assert_eq!(reqs[0].0, "POST");
        assert!(reqs[0].1.ends_with("/token"));
        assert!(
            reqs[0]
                .2
                .contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer"),
            "{}",
            reqs[0].2
        );
        assert!(reqs[0].2.contains("assertion="), "{}", reqs[0].2);
    }

    #[tokio::test]
    async fn an_existing_table_is_not_recreated() {
        let fake = FakeGoogle::with(vec![(200, token_body()), (200, "{}".to_owned())]);
        let mut client = client_with(&fake, None).await;
        client.ensure_table(SourceKind::Syslog).await.unwrap();
        // token + tables.get, and nothing else.
        assert_eq!(fake.requests().len(), 2);
        assert_eq!(fake.requests()[1].0, "GET");
        // ...and a second call short-circuits entirely.
        client.ensure_table(SourceKind::Syslog).await.unwrap();
        assert_eq!(fake.requests().len(), 2);
    }

    #[tokio::test]
    async fn a_missing_table_is_created_partitioned_and_clustered() {
        let fake = FakeGoogle::with(vec![
            (200, token_body()),
            (404, "{}".to_owned()), // tables.get
            (200, "{}".to_owned()), // datasets.get — the dataset does exist
            (200, "{}".to_owned()), // tables.insert
        ]);
        let mut client = client_with(&fake, None).await;
        client.ensure_table(SourceKind::Flow).await.unwrap();
        let reqs = fake.requests();
        assert_eq!(reqs.len(), 4);
        assert_eq!(reqs[3].0, "POST");
        let body: Value = serde_json::from_str(&reqs[3].2).unwrap();
        assert_eq!(body["timePartitioning"]["type"], json!("DAY"));
        assert_eq!(
            body["timePartitioning"]["field"],
            json!(yagra_forward::FLOW_PARTITION_FIELD)
        );
        assert_eq!(
            body["clustering"]["fields"],
            json!(yagra_forward::FLOW_CLUSTERING.to_vec())
        );
        // The flow stream must get the flow schema, not the event one.
        assert_eq!(body["schema"]["fields"], yagra_forward::flow_schema());
    }

    #[tokio::test]
    async fn a_missing_dataset_says_so_instead_of_creating_one() {
        // A dataset's region is permanent. Creating one on the operator's behalf would choose data
        // residency for them, irreversibly.
        let fake = FakeGoogle::with(vec![
            (200, token_body()),
            (404, "{}".to_owned()), // tables.get
            (404, "{}".to_owned()), // datasets.get
        ]);
        let mut client = client_with(&fake, None).await;
        let err = client
            .ensure_table(SourceKind::Syslog)
            .await
            .expect_err("a missing dataset must not be papered over");
        assert!(err.contains("does not exist"), "{err}");
        assert!(err.contains("region"), "{err}");
        // Nothing was created.
        assert!(fake.requests().iter().all(|(m, _, _)| m != "POST"));
    }

    #[tokio::test]
    async fn insert_posts_the_rows_with_the_loss_tolerant_flags() {
        let fake = FakeGoogle::with(vec![(200, token_body()), (200, "{}".to_owned())]);
        let mut client = client_with(&fake, None).await;
        let rows = vec![json!({"insertId": "a", "json": {"kind": "syslog"}})];
        assert_eq!(client.insert_rows(&rows).await.unwrap(), 0);
        let reqs = fake.requests();
        assert!(reqs[1].1.ends_with("/insertAll"), "{}", reqs[1].1);
        let body: Value = serde_json::from_str(&reqs[1].2).unwrap();
        assert_eq!(body["skipInvalidRows"], json!(true));
        assert_eq!(body["ignoreUnknownValues"], json!(true));
        assert_eq!(body["rows"], json!(rows));
    }

    #[tokio::test]
    async fn rejected_rows_are_counted_but_the_batch_still_succeeds() {
        let fake = FakeGoogle::with(vec![
            (200, token_body()),
            (
                200,
                json!({ "insertErrors": [{ "index": 0, "errors": [{ "reason": "invalid" }] }] })
                    .to_string(),
            ),
        ]);
        let mut client = client_with(&fake, None).await;
        let rows = vec![
            json!({"insertId": "a", "json": {}}),
            json!({"insertId": "b", "json": {}}),
        ];
        assert_eq!(client.insert_rows(&rows).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn a_token_is_reused_across_inserts() {
        let fake = FakeGoogle::with(vec![
            (200, token_body()),
            (200, "{}".to_owned()),
            (200, "{}".to_owned()),
        ]);
        let mut client = client_with(&fake, None).await;
        let rows = vec![json!({"insertId": "a", "json": {}})];
        client.insert_rows(&rows).await.unwrap();
        client.insert_rows(&rows).await.unwrap();
        // One token request, two inserts — not a token exchange per batch.
        assert_eq!(fake.requests().len(), 3);
        assert_eq!(
            fake.requests()
                .iter()
                .filter(|(_, p, _)| p.contains("token"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn a_rejected_token_is_dropped_so_the_next_attempt_re_mints() {
        let fake = FakeGoogle::with(vec![
            (200, token_body()),
            (401, "{}".to_owned()), // insertAll rejects the token
            (200, token_body()),    // ...so the retry mints a fresh one
            (200, "{}".to_owned()),
        ]);
        let mut client = client_with(&fake, None).await;
        let rows = vec![json!({"insertId": "a", "json": {}})];
        assert!(client.insert_rows(&rows).await.is_err());
        client.insert_rows(&rows).await.unwrap();
        assert_eq!(
            fake.requests()
                .iter()
                .filter(|(_, p, _)| p.contains("token"))
                .count(),
            2
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

    #[test]
    fn an_insert_response_without_errors_counts_zero() {
        assert_eq!(count_insert_errors(&json!({})), 0);
        assert_eq!(count_insert_errors(&json!({ "insertErrors": [] })), 0);
    }
}
