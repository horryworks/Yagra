// SPDX-License-Identifier: AGPL-3.0-only
//! Credential store: encrypted monitoring credentials at rest (ADR-018, Workstream D).
//!
//! Secrets are sealed with [`yagra_secrets`] envelope encryption (per-secret DEK under a
//! KEK) and only ciphertext + wrapped DEK are persisted. The KEK is loaded once from a mounted
//! file (`YAGRA_KEK_FILE`) and shared by every envelope store; with **no file configured** we fall
//! back to an ephemeral dev key (logged loudly — secrets won't survive a restart, which is correct
//! for local dev only), but a file that is configured and unreadable **stops startup** rather than
//! booting on a key that cannot open anything already stored. The API never returns a secret value;
//! `secret()` exists for core-side credential resolution (used when SNMP polling lands).

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::HttpAuth;
use yagra_secrets::{key_provider_from_file, EnvelopeCipher, SealedSecret, StaticKeyProvider};

/// The shared KEK handle every envelope-encrypted store is built with.
pub type Kek = Arc<StaticKeyProvider>;

/// Credential metadata returned by the API — never includes the secret value.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct CredentialSummary {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    /// How many nodes reference this credential — answers "is it safe to delete?". Counted at
    /// list time from `nodes.credential_id`; `0` means the credential is unused.
    pub used_by: i64,
}

/// Credential kind for SNMP v3 USM parameters (the secret is a [`SnmpV3Secret`] JSON doc).
/// Any other kind's secret is treated as an opaque string (e.g. `snmp_v2c` community).
pub const KIND_SNMP_V3: &str = "snmp_v3";

/// The sealed JSON shape of an `snmp_v3` credential. Field tokens mirror the bus check
/// (`SnmpV3Check`): `security_level` ∈ noauth|auth|authpriv; protocols are lowercase
/// tokens (`sha256`, `aes128`, …). Validated at the API edge on create and parsed again
/// at schedule time — never logged.
///
/// # Why this is still its own struct after ADR-084
///
/// ADR-084 folded eleven copies of these six fields into [`yagra_common::SnmpV3Auth`], and this is
/// the **one copy deliberately left standing**. The others were all in-flight shapes — a job on
/// the bus, parameters handed to a walker — where the only contract is "core wrote it, a poller
/// reads it back". This one is a **storage** shape: it is what a credential decrypts to, so its
/// deserializer is the last thing between an encrypted blob written by some earlier version and a
/// scheduler that will poll with the result. Folding it would put the at-rest format and the
/// on-the-wire format on one type, and a change made for one reason would then land on the other.
/// The failure mode is not symmetric either: get the wire wrong and one rollout stumbles; get this
/// wrong and **every v3 credential stops parsing, so every v3 node silently stops being polled**.
///
/// It also carries validation the wire shape must not have — [`Self::parse`] refuses a document
/// whose `security_level` does not match the keys present. A bus check has to accept whatever an
/// N-1 core sent; a stored credential does not.
///
/// So the copy stays, and [`Self::auth`] is the single crossing point between the two.
#[derive(Debug, Clone, Deserialize)]
pub struct SnmpV3Secret {
    pub user: String,
    pub security_level: String,
    #[serde(default)]
    pub auth_protocol: Option<String>,
    #[serde(default)]
    pub auth_key: Option<String>,
    #[serde(default)]
    pub priv_protocol: Option<String>,
    #[serde(default)]
    pub priv_key: Option<String>,
}

impl SnmpV3Secret {
    /// The USM credentials this document carries, in the shape the bus and the transport speak.
    ///
    /// The **only** place the stored form crosses into the wire form (ADR-084). Before it existed
    /// the same six-line copy sat in each of the eight `build_snmp_v3_*_check` builders, so adding
    /// a USM field meant editing eight call sites that no compiler compared with each other.
    ///
    /// Clones: the caller owns a job it is about to publish, and this type is behind a shared
    /// handle. ⚠️ **The result holds plaintext passphrases — never log it.**
    #[must_use]
    pub fn auth(&self) -> yagra_common::SnmpV3Auth {
        // Destructured, not field-accessed: a seventh field on either side has to be routed here
        // by hand rather than silently dropped on the way to the poller — which would present as
        // "v3 polling authenticates with the wrong parameters", with every test green.
        let Self {
            user,
            security_level,
            auth_protocol,
            auth_key,
            priv_protocol,
            priv_key,
        } = self;
        yagra_common::SnmpV3Auth {
            user: user.clone(),
            security_level: security_level.clone(),
            auth_protocol: auth_protocol.clone(),
            auth_key: auth_key.clone(),
            priv_protocol: priv_protocol.clone(),
            priv_key: priv_key.clone(),
        }
    }

    /// Parse and structurally validate a v3 secret document. `Err` carries a static
    /// description only — never any field content.
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        let secret: Self =
            serde_json::from_slice(bytes).map_err(|_| "not a valid SNMPv3 JSON document")?;
        if secret.user.trim().is_empty() {
            return Err("user must not be empty");
        }
        match secret.security_level.as_str() {
            "noauth" => {}
            "auth" => {
                if secret.auth_key.as_deref().unwrap_or("").is_empty() {
                    return Err("auth level requires an auth passphrase");
                }
            }
            "authpriv" => {
                if secret.auth_key.as_deref().unwrap_or("").is_empty() {
                    return Err("authpriv level requires an auth passphrase");
                }
                if secret.priv_key.as_deref().unwrap_or("").is_empty() {
                    return Err("authpriv level requires a privacy passphrase");
                }
            }
            _ => return Err("security_level must be noauth, auth, or authpriv"),
        }
        Ok(secret)
    }
}

/// Credential kind for a Cisco Meraki Dashboard API key (the secret is a [`MerakiApiSecret`] JSON
/// doc holding **only** the key). The `org_id` binding lives on the `meraki_orgs` row, not in the
/// sealed secret, so one credential can back many orgs (an MSP/admin key spanning orgs). Read-only:
/// the key is inlined into a collect job at dispatch time and never logged.
pub const KIND_MERAKI_API: &str = "meraki_api";

/// The sealed JSON shape of a `meraki_api` credential: just the API key. Validated at the API edge
/// on create and parsed again at schedule time; `Err` carries a static description only — never the
/// key bytes.
#[derive(Debug, Clone, Deserialize)]
pub struct MerakiApiSecret {
    pub api_key: String,
}

impl MerakiApiSecret {
    /// Parse and structurally validate a Meraki API secret document. `Err` carries a static
    /// description only — never any field content (the key must never be echoed).
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        let secret: Self =
            serde_json::from_slice(bytes).map_err(|_| "not a valid Meraki API JSON document")?;
        if secret.api_key.trim().is_empty() {
            return Err("api_key must not be empty");
        }
        Ok(secret)
    }
}

/// Credential kind for HTTP auth on a URL monitor (the secret is an [`HttpAuth`] JSON doc).
///
/// One kind with an internal `scheme` discriminator, mirroring how `snmp_v3` carries
/// `security_level` — three separate kinds would triple the kind list, the kind filter and the
/// per-kind form for no gain.
pub const KIND_HTTP_AUTH: &str = "http_auth";

/// Legacy credential kind that predates [`KIND_HTTP_AUTH`].
///
/// It has been creatable in the UI since the credentials page shipped and was consumed by nothing —
/// an operator could store one and it would never be used by anything. Rather than leave the orphan
/// (or migrate rows), a URL monitor accepts it as a bearer token: the stored bytes *are* the token,
/// which is what an operator storing an "API token" meant. New credentials should use
/// [`KIND_HTTP_AUTH`], which can also express Basic and custom headers.
pub const KIND_API_TOKEN: &str = "api_token";

/// Parse a sealed credential into the [`HttpAuth`] a poll job carries.
///
/// `Err` carries a static description only — never any field content, since the message reaches
/// the operator through the API and the log.
pub fn parse_http_auth(kind: &str, bytes: &[u8]) -> Result<HttpAuth, &'static str> {
    if kind == KIND_API_TOKEN {
        let token = std::str::from_utf8(bytes).map_err(|_| "api_token is not valid UTF-8")?;
        if token.trim().is_empty() {
            return Err("api_token must not be empty");
        }
        return Ok(HttpAuth::Bearer {
            token: token.to_owned(),
        });
    }
    if kind != KIND_HTTP_AUTH {
        return Err("credential kind cannot be used for HTTP authentication");
    }
    let auth: HttpAuth =
        serde_json::from_slice(bytes).map_err(|_| "not a valid http_auth JSON document")?;
    match &auth {
        HttpAuth::Basic { username, password } => {
            if username.is_empty() || password.is_empty() {
                return Err("basic auth needs both a username and a password");
            }
        }
        HttpAuth::Bearer { token } => {
            if token.trim().is_empty() {
                return Err("bearer auth needs a token");
            }
        }
        HttpAuth::Header { name, value } => {
            if !is_valid_header_name(name) {
                return Err("header name must be a valid HTTP token and not a reserved header");
            }
            if value.trim().is_empty() {
                return Err("header auth needs a value");
            }
        }
    }
    Ok(auth)
}

/// Whether `name` is a header a URL monitor may set.
///
/// Rejects anything that is not an RFC 7230 token, plus the headers whose meaning the probe owns:
/// setting `Host` retargets the request, `Authorization` collides with the Basic/Bearer schemes,
/// and hop-by-hop headers belong to the connection rather than the request.
#[must_use]
pub fn is_valid_header_name(name: &str) -> bool {
    const RESERVED: [&str; 8] = [
        "host",
        "authorization",
        "connection",
        "content-length",
        "transfer-encoding",
        "upgrade",
        "te",
        "trailer",
    ];
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    if RESERVED.contains(&name.to_ascii_lowercase().as_str()) {
        return false;
    }
    name.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// Load the key-encryption-key provider from `YAGRA_KEK_FILE`, or fall back to an ephemeral dev key
/// when the variable is unset. Loaded **once** and shared (via `Arc`) by every envelope-encrypted
/// store — credentials, notification channels, forwarding destinations, OIDC, LLM config — so they
/// genuinely all use one KEK.
///
/// **A configured-but-unreadable KEK is a hard error, not a fallback.** Substituting a random key
/// there let core start and serve traffic while every stored credential was silently undecryptable:
/// the deployment looked healthy, and the damage only surfaced the next time something tried to use
/// a credential. Failing to start is loud, immediate, and recoverable. This is the same rule
/// `token::load_session_key` already applies to the session key.
pub fn load_key_provider() -> anyhow::Result<Arc<StaticKeyProvider>> {
    key_provider_from_env(std::env::var("YAGRA_KEK_FILE").ok())
}

/// The decision behind [`load_key_provider`], split from the environment read so it is testable
/// without mutating process-wide state.
pub(crate) fn key_provider_from_env(
    path: Option<String>,
) -> anyhow::Result<Arc<StaticKeyProvider>> {
    let Some(path) = path.map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()) else {
        tracing::warn!(
            "YAGRA_KEK_FILE not set — using EPHEMERAL dev KEK (credentials will not \
             survive a restart; set a mounted KEK file for real use)"
        );
        return Ok(Arc::new(StaticKeyProvider::single(
            rand::random::<[u8; 32]>(),
        )));
    };
    // `SecretError` carries a path and an io reason or a byte length — never key material — so it
    // is safe to surface in the startup error.
    let provider = key_provider_from_file(Path::new(&path))
        .with_context(|| format!("load KEK from {path}"))?;
    tracing::info!(%path, "loaded KEK from mounted file");
    Ok(Arc::new(provider))
}

/// PostgreSQL-backed, envelope-encrypted credential store.
pub struct CredentialStore {
    pool: PgPool,
    cipher: EnvelopeCipher<Kek>,
}

impl CredentialStore {
    /// Build the store on the process-wide KEK loaded at startup ([`load_key_provider`]).
    pub fn new(pool: PgPool, kek: Kek) -> Self {
        Self {
            pool,
            cipher: EnvelopeCipher::new(kek),
        }
    }

    /// Seal and store a new credential; returns its id. The plaintext is encrypted before
    /// it touches the database and is never logged.
    pub async fn create(&self, name: &str, kind: &str, secret: &[u8]) -> anyhow::Result<Uuid> {
        let sealed = self
            .cipher
            .seal(secret)
            .map_err(|e| anyhow::anyhow!("seal credential: {e}"))?;
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO credentials \
             (id, name, kind, key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(id)
        .bind(name)
        .bind(kind)
        .bind(i64::from(sealed.key_id))
        .bind(&sealed.wrapped_dek)
        .bind(&sealed.dek_nonce)
        .bind(&sealed.ciphertext)
        .bind(&sealed.ct_nonce)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Credential metadata (no secret values).
    pub async fn list(&self) -> anyhow::Result<Vec<CredentialSummary>> {
        // LEFT JOIN so unused credentials still appear (used_by = 0). Grouping by the credential
        // PK lets us order by its created_at (functionally dependent on the PK in PostgreSQL).
        let rows = sqlx::query(
            "SELECT c.id, c.name, c.kind, count(n.id) AS used_by \
             FROM credentials c LEFT JOIN nodes n ON n.credential_id = c.id \
             GROUP BY c.id ORDER BY c.created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(CredentialSummary {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    kind: row.try_get("kind")?,
                    used_by: row.try_get("used_by")?,
                })
            })
            .collect()
    }

    /// Update a credential. The name is always set; `reseal` carries `(kind, secret)` when the
    /// caller is replacing the secret — it is sealed (encrypted) before it touches the database
    /// and never logged, exactly like [`create`](Self::create). When `reseal` is `None` only the
    /// name changes and the sealed secret/kind are left intact (rename-only). Returns whether a
    /// row matched.
    pub async fn update(
        &self,
        id: Uuid,
        name: &str,
        reseal: Option<(&str, &[u8])>,
    ) -> anyhow::Result<bool> {
        let res = match reseal {
            Some((kind, secret)) => {
                let sealed = self
                    .cipher
                    .seal(secret)
                    .map_err(|e| anyhow::anyhow!("seal credential: {e}"))?;
                sqlx::query(
                    "UPDATE credentials SET name = $2, kind = $3, key_id = $4, \
                     wrapped_dek = $5, dek_nonce = $6, ciphertext = $7, ct_nonce = $8 \
                     WHERE id = $1",
                )
                .bind(id)
                .bind(name)
                .bind(kind)
                .bind(i64::from(sealed.key_id))
                .bind(&sealed.wrapped_dek)
                .bind(&sealed.dek_nonce)
                .bind(&sealed.ciphertext)
                .bind(&sealed.ct_nonce)
                .execute(&self.pool)
                .await?
            }
            None => {
                sqlx::query("UPDATE credentials SET name = $2 WHERE id = $1")
                    .bind(id)
                    .bind(name)
                    .execute(&self.pool)
                    .await?
            }
        };
        Ok(res.rows_affected() > 0)
    }

    /// Delete a credential. Returns whether a row was removed.
    pub async fn delete(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM credentials WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Decrypt a credential's secret in memory along with its `kind`, so the caller can
    /// pick the protocol path (v2c community vs v3 USM doc). Never exposed via the API.
    pub async fn open(&self, id: Uuid) -> anyhow::Result<Option<(String, Vec<u8>)>> {
        let row = sqlx::query(
            "SELECT kind, key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce \
             FROM credentials WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let kind: String = row.try_get("kind")?;
        let key_id: i64 = row.try_get("key_id")?;
        let sealed = SealedSecret {
            key_id: u32::try_from(key_id).unwrap_or(0),
            wrapped_dek: row.try_get("wrapped_dek")?,
            dek_nonce: row.try_get("dek_nonce")?,
            ciphertext: row.try_get("ciphertext")?,
            ct_nonce: row.try_get("ct_nonce")?,
        };
        let plaintext = self
            .cipher
            .open(&sealed)
            .map_err(|e| anyhow::anyhow!("open credential: {e}"))?;
        Ok(Some((kind, plaintext)))
    }

    /// Try to unseal **every** stored credential and report which ones the current KEK cannot open.
    ///
    /// This is the "did the restore actually work" check (ADR-040). Losing the KEK is the one
    /// failure a database restore cannot reveal on its own: rows come back, row counts match, the
    /// API answers 200, and yet nothing can be polled because the ciphertext is unreadable. Nothing
    /// short of attempting a decrypt proves otherwise.
    ///
    /// Plaintext is dropped immediately and never returned, logged, or counted by length — the
    /// answer is a boolean per credential and nothing more (security.md).
    pub async fn decrypt_report(&self) -> anyhow::Result<Vec<(Uuid, String, String, bool)>> {
        let rows = sqlx::query(
            "SELECT id, name, kind, key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce \
             FROM credentials ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let id: Uuid = row.try_get("id")?;
            let name: String = row.try_get("name")?;
            let kind: String = row.try_get("kind")?;
            let key_id: i64 = row.try_get("key_id")?;
            let sealed = SealedSecret {
                key_id: u32::try_from(key_id).unwrap_or(0),
                wrapped_dek: row.try_get("wrapped_dek")?,
                dek_nonce: row.try_get("dek_nonce")?,
                ciphertext: row.try_get("ciphertext")?,
                ct_nonce: row.try_get("ct_nonce")?,
            };
            out.push((id, name, kind, self.cipher.open(&sealed).is_ok()));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_secrets::KeyProvider;

    /// The regression this whole change exists for: a KEK file that is *configured* but cannot be
    /// read used to log an error and boot on a fresh random key, leaving every stored credential
    /// undecryptable while the deployment looked healthy. It must stop startup instead.
    #[test]
    fn a_configured_but_missing_kek_file_is_an_error() {
        let missing = std::env::temp_dir().join("yagra-no-such-kek-file-9d3f1a");
        // `expect_err` would need `Debug` on the provider, and a key provider must never have one.
        let Err(err) = key_provider_from_env(Some(missing.display().to_string())) else {
            panic!("a missing KEK file must not fall back to an ephemeral key");
        };
        // The path is in the message so an operator can see which file to fix; the error chain
        // carries an io reason, never key material.
        assert!(err.to_string().contains("load KEK from"));
    }

    #[test]
    fn a_kek_file_of_the_wrong_length_is_an_error() {
        let path = std::env::temp_dir().join("yagra-short-kek-9d3f1a.bin");
        std::fs::write(&path, b"too short").unwrap();
        let Err(err) = key_provider_from_env(Some(path.display().to_string())) else {
            panic!("a 9-byte KEK is not an AES-256 key");
        };
        assert!(err.to_string().contains("load KEK from"));
        let _ = std::fs::remove_file(&path);
    }

    /// Unset stays a warning and an ephemeral key: that is the local-dev path, and `docker-compose.yml`
    /// (dev) sets no `YAGRA_KEK_FILE` at all, so this is what keeps dev byte-identical.
    #[test]
    fn an_unset_kek_still_yields_an_ephemeral_provider() {
        assert!(key_provider_from_env(None).is_ok());
        // A blank value is "unset", not "a file named empty string".
        assert!(key_provider_from_env(Some("   ".to_owned())).is_ok());
    }

    /// The property the shared `Arc` buys, and the one the restore-verification script depends on:
    /// all five envelope stores open each other's seals because there is genuinely one key. Before
    /// this, each store called the loader itself, so on the ephemeral path they got five different
    /// keys while the doc comment claimed they shared one.
    #[test]
    fn two_stores_built_from_one_provider_share_the_key() {
        let kek = key_provider_from_env(None).unwrap();
        let a = EnvelopeCipher::new(kek.clone());
        let b = EnvelopeCipher::new(kek.clone());
        let sealed = a.seal(b"community-string").unwrap();
        let opened = b.open(&sealed).unwrap();
        assert_eq!(opened, b"community-string");
        assert_eq!(kek.active_key_id(), 1);
    }

    #[test]
    fn v3_secret_parses_and_validates_levels() {
        let ok = br#"{"user":"monitor","security_level":"authpriv","auth_protocol":"sha256",
                      "auth_key":"a-pass","priv_protocol":"aes256","priv_key":"p-pass"}"#;
        let parsed = SnmpV3Secret::parse(ok).expect("valid authpriv doc");
        assert_eq!(parsed.user, "monitor");

        // noauth needs no keys.
        assert!(SnmpV3Secret::parse(br#"{"user":"u","security_level":"noauth"}"#).is_ok());

        // Missing passphrases for the declared level are rejected with a static message.
        let err = SnmpV3Secret::parse(br#"{"user":"u","security_level":"auth"}"#)
            .expect_err("auth without passphrase");
        assert!(err.contains("auth passphrase"));
        let err =
            SnmpV3Secret::parse(br#"{"user":"u","security_level":"authpriv","auth_key":"a"}"#)
                .expect_err("authpriv without priv passphrase");
        assert!(err.contains("privacy passphrase"));

        // Garbage and unknown levels are rejected.
        assert!(SnmpV3Secret::parse(b"not json").is_err());
        assert!(SnmpV3Secret::parse(br#"{"user":"u","security_level":"max"}"#).is_err());
        assert!(SnmpV3Secret::parse(br#"{"user":"  ","security_level":"noauth"}"#).is_err());
    }

    #[test]
    fn meraki_secret_requires_non_empty_key() {
        let ok = MerakiApiSecret::parse(br#"{"api_key":"0123deadbeef"}"#).expect("valid key");
        assert_eq!(ok.api_key, "0123deadbeef");
        // Empty / whitespace-only / missing keys are rejected with a static message.
        assert!(MerakiApiSecret::parse(br#"{"api_key":""}"#).is_err());
        assert!(MerakiApiSecret::parse(br#"{"api_key":"   "}"#).is_err());
        assert!(MerakiApiSecret::parse(br#"{}"#).is_err());
        assert!(MerakiApiSecret::parse(b"not json").is_err());
    }

    /// 🚨 **A sealed secret's `key_id` must be decoded at the width its column declares.**
    ///
    /// sqlx checks the column type at **runtime**, not at compile time: reading an `INTEGER` column
    /// into `i64` fails the whole row with `mismatched types`, and every caller above turns that
    /// into a degraded read rather than an error anyone sees. Measured on the lab core 2026-08-30 —
    /// `notification_channels` held exactly one row, `list_open_channels` failed every 30 seconds,
    /// and `load_routing` degrades a failed read to an *empty* snapshot, so the deployment delivered
    /// **no notification at all** and said so only in a `WARN` line. Three readers were wrong the
    /// same way — notification channels, OIDC providers, forwarding destinations — and each one
    /// silently disables a whole feature the moment a row exists.
    ///
    /// Nothing else could have caught it. These are live PostgreSQL reads, so no unit test reaches
    /// them, and the lab held zero rows in all three tables — which is exactly what ADR-097's own
    /// remnant said about notification channels, and it stood for eight releases.
    ///
    /// The rule is **derived, not written down**: `migrations/` says which width each `key_id`
    /// column has, and exactly one of them is `BIGINT` (`credentials`, migration 0002). So exactly
    /// one file may read it as `i64`, and that file is named here rather than in nine doc comments.
    #[test]
    fn every_sealed_key_id_is_read_at_the_width_its_column_declares() {
        use std::path::Path;
        use yagra_common::srcread as sr;

        // What the schema declares.
        let migrations = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
        let (mut wide_cols, mut narrow_cols) = (0usize, 0usize);
        for entry in std::fs::read_dir(&migrations).expect("migrations/ is readable") {
            let path = entry.expect("a readable directory entry").path();
            if path.extension().is_none_or(|x| x != "sql") {
                continue;
            }
            let sql = std::fs::read_to_string(&path)
                .expect("migration is readable")
                .to_lowercase();
            // A declaration, not a reference: the type word follows the column name.
            for tail in sql.split("key_id").skip(1) {
                let word: String = tail
                    .trim_start()
                    .chars()
                    .take_while(|c| c.is_ascii_alphabetic())
                    .collect();
                match word.as_str() {
                    "bigint" => wide_cols += 1,
                    "integer" => narrow_cols += 1,
                    _ => {}
                }
            }
        }
        assert_eq!(
            wide_cols, 1,
            "exactly one key_id column is BIGINT (credentials, migration 0002). A second one means \
             the exemption below has to name its reader too"
        );
        assert!(
            narrow_cols >= 7,
            "only {narrow_cols} INTEGER key_id columns found — the migration scan stopped matching, \
             and a scan that sees nothing is indistinguishable from a schema that is fine"
        );

        // What the code does. Read through `srcread` so this test cannot see its own needles.
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut paths = Vec::new();
        sr::rs_files(&src, &mut paths);
        assert!(
            paths.len() >= 150,
            "only {} source files walked; the crate is larger than that",
            paths.len()
        );
        let mut inspected = 0usize;
        let mut offenders: Vec<String> = Vec::new();
        for path in &paths {
            let name = sr::file_name(path);
            let code = sr::strip_and_check(&name, &sr::read(path));
            for line in code.lines().filter(|l| {
                l.contains("key_id") && (l.contains("try_get") || l.contains(".get::<"))
            }) {
                inspected += 1;
                if line.contains("i64") && name != "secrets.rs" {
                    offenders.push(format!("{name}: {}", line.trim()));
                }
            }
        }
        assert!(
            inspected >= 9,
            "only {inspected} key_id reads inspected — the needle stopped matching, which reads the \
             same as every reader being correct"
        );
        assert!(
            offenders.is_empty(),
            "these decode an INTEGER key_id as i64, which fails the whole row at runtime and \
             degrades to an empty read:\n  {}",
            offenders.join("\n  ")
        );
    }
}
