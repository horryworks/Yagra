//! Credential store: encrypted monitoring credentials at rest (ADR-018, Workstream D).
//!
//! Secrets are sealed with [`yagra_secrets`] envelope encryption (per-secret DEK under a
//! KEK) and only ciphertext + wrapped DEK are persisted. The KEK is loaded from a mounted
//! file (`YAGRA_KEK_FILE`); with no file we fall back to an **ephemeral** dev key (logged
//! loudly — secrets won't survive a restart, which is correct for local dev only). The
//! API never returns a secret value; `secret()` exists for core-side credential
//! resolution (used when SNMP polling lands).

use std::path::Path;

use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_secrets::{key_provider_from_file, EnvelopeCipher, SealedSecret, StaticKeyProvider};

/// Credential metadata returned by the API — never includes the secret value.
#[derive(Debug, Clone, Serialize)]
pub struct CredentialSummary {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
}

/// PostgreSQL-backed, envelope-encrypted credential store.
pub struct CredentialStore {
    pool: PgPool,
    cipher: EnvelopeCipher<StaticKeyProvider>,
}

impl CredentialStore {
    /// Build the store, loading the KEK from `YAGRA_KEK_FILE` or falling back to an
    /// ephemeral dev key (never for production — secrets are lost on restart).
    pub fn from_env(pool: PgPool) -> Self {
        let provider = match std::env::var("YAGRA_KEK_FILE") {
            Ok(path) => match key_provider_from_file(Path::new(&path)) {
                Ok(p) => {
                    tracing::info!(%path, "loaded KEK from mounted file");
                    p
                }
                Err(e) => {
                    tracing::error!(error = %e, %path, "KEK file load failed; using EPHEMERAL dev key");
                    StaticKeyProvider::single(rand::random::<[u8; 32]>())
                }
            },
            Err(_) => {
                tracing::warn!(
                    "YAGRA_KEK_FILE not set — using EPHEMERAL dev KEK (credentials will not \
                     survive a restart; set a mounted KEK file for real use)"
                );
                StaticKeyProvider::single(rand::random::<[u8; 32]>())
            }
        };
        Self {
            pool,
            cipher: EnvelopeCipher::new(provider),
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
        let rows = sqlx::query("SELECT id, name, kind FROM credentials ORDER BY created_at")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(CredentialSummary {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    kind: row.try_get("kind")?,
                })
            })
            .collect()
    }

    /// Delete a credential. Returns whether a row was removed.
    pub async fn delete(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM credentials WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Decrypt a credential's secret in memory (core-side resolution; never exposed via
    /// the API). Returns `None` if no such credential. Used by SNMP credential resolution
    /// (Workstream C); allowed-dead until that path lands.
    #[allow(dead_code)]
    pub async fn secret(&self, id: Uuid) -> anyhow::Result<Option<Vec<u8>>> {
        let row = sqlx::query(
            "SELECT key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce \
             FROM credentials WHERE id = $1",
        )
        .bind(id)
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
        let plaintext = self
            .cipher
            .open(&sealed)
            .map_err(|e| anyhow::anyhow!("open credential: {e}"))?;
        Ok(Some(plaintext))
    }
}
