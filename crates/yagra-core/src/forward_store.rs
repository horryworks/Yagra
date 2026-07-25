// SPDX-License-Identifier: AGPL-3.0-only
//! PostgreSQL store for forwarding destinations (ADR-034).
//!
//! A destination says "everything on this stream matching this filter also goes to that collector".
//! The table is metadata, so it lives in PostgreSQL (ADR-004); the forwarder reloads the enabled
//! rows whenever the config generation changes, so edits apply without a restart.
//!
//! Per-destination secrets are sealed with the same envelope cipher + KEK as monitoring credentials
//! (ADR-018) and **never** appear in [`ForwardDestination`], which is what the API serializes.

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_forward::{DestKind, FilterExpr, SourceKind};
use yagra_secrets::{EnvelopeCipher, SealedSecret, StaticKeyProvider};

use crate::secrets::load_key_provider;

/// Ceiling on configured destinations. Bounds both the forwarder's per-message fan-out cost and the
/// cardinality of the `dest` metric label (monitoring-conventions: labels must be bounded).
pub const MAX_DESTINATIONS: i64 = 64;

/// The (secret) part of a destination's config — sealed at rest, never returned by the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DestSecret {
    /// Community placed on a re-encoded SNMPv2c trap. Only used when the original PDU is
    /// unavailable (an N-1 poller sent no raw payload) — a verbatim relay carries the device's own.
    SnmpCommunity {
        /// The community string. Treated as a credential: never logged, never returned.
        community: String,
    },
}

/// A destination as the API exposes it — no secrets, ever.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ForwardDestination {
    /// Stable id.
    pub id: Uuid,
    /// Human label (unique).
    pub name: String,
    /// Whether the forwarder currently sends to it.
    pub enabled: bool,
    /// Which received stream is teed.
    pub source_kind: SourceKind,
    /// How the collector is spoken to.
    pub dest_kind: DestKind,
    /// `host:port` of the collector.
    pub target: String,
    /// Restrict to one poller pool; `None` = every pool.
    pub pool: Option<String>,
    /// `true` = relay original bytes, `false` = rebuild from parsed fields.
    pub verbatim: bool,
    /// The filter; `{}` forwards the whole stream.
    pub filter: FilterExpr,
    /// Optional messages/second ceiling.
    pub rate_limit_per_sec: Option<u32>,
    /// Extra PEM certificate(s) trusted for a TLS destination, on top of the system roots. Not a
    /// secret — a CA certificate is public — so it round-trips through the API like any other field.
    pub ca_cert: Option<String>,
    /// Whether a sealed secret is stored (so the UI can show "configured" without revealing it).
    pub has_secret: bool,
}

/// What a create/update writes. Separated from [`ForwardDestination`] because the secret only
/// travels inbound.
#[derive(Debug, Clone)]
pub struct ForwardDestinationInput {
    /// Human label (unique).
    pub name: String,
    /// Whether the forwarder should send to it.
    pub enabled: bool,
    /// Which received stream to tee.
    pub source_kind: SourceKind,
    /// How to speak to the collector.
    pub dest_kind: DestKind,
    /// `host:port` of the collector.
    pub target: String,
    /// Restrict to one poller pool; `None` = every pool.
    pub pool: Option<String>,
    /// `true` = relay original bytes.
    pub verbatim: bool,
    /// The filter.
    pub filter: FilterExpr,
    /// Optional messages/second ceiling.
    pub rate_limit_per_sec: Option<u32>,
    /// Extra PEM certificate(s) to trust for a TLS destination.
    pub ca_cert: Option<String>,
    /// `None` on update means "keep the stored secret"; `Some` replaces it.
    pub secret: Option<DestSecret>,
}

/// A destination with its secret decrypted — core-side only, for building live senders.
#[derive(Debug, Clone)]
pub struct OpenDestination {
    /// The public part.
    pub dest: ForwardDestination,
    /// The decrypted secret, if one is stored and could be opened.
    pub secret: Option<DestSecret>,
}

/// PostgreSQL-backed store for forwarding destinations.
pub struct ForwardStore {
    pool: PgPool,
    cipher: EnvelopeCipher<StaticKeyProvider>,
}

const PUBLIC_COLUMNS: &str = "id, name, enabled, source_kind, dest_kind, target, pool, verbatim, \
     filter, rate_limit_per_sec, ca_cert, (key_id IS NOT NULL) AS has_secret";

/// `COALESCE` on the five sealed columns is what makes "edit the target without re-entering the
/// secret" work — a plain assignment would wipe the stored secret whenever the form omitted it.
/// `ca_cert` is *not* coalesced: it is not a secret, the form always round-trips its current value,
/// and clearing it has to be possible.
const UPDATE_SQL: &str = "UPDATE forward_destinations SET \
       name = $2, enabled = $3, source_kind = $4, dest_kind = $5, target = $6, pool = $7, \
       verbatim = $8, filter = $9, rate_limit_per_sec = $10, ca_cert = $16, \
       key_id = COALESCE($11, key_id), wrapped_dek = COALESCE($12, wrapped_dek), \
       dek_nonce = COALESCE($13, dek_nonce), ciphertext = COALESCE($14, ciphertext), \
       ct_nonce = COALESCE($15, ct_nonce), updated_at = now() \
     WHERE id = $1";

impl ForwardStore {
    /// Build the store, reusing the same KEK as the credential store.
    #[must_use]
    pub fn from_env(pool: PgPool) -> Self {
        Self {
            pool,
            cipher: EnvelopeCipher::new(load_key_provider()),
        }
    }

    /// Every destination, newest last. No secrets.
    ///
    /// # Errors
    /// Propagates database and row-decode failures.
    pub async fn list(&self) -> anyhow::Result<Vec<ForwardDestination>> {
        let sql = format!("SELECT {PUBLIC_COLUMNS} FROM forward_destinations ORDER BY created_at");
        let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
        rows.into_iter().map(row_to_dest).collect()
    }

    /// How many destinations exist (checked against [`MAX_DESTINATIONS`] before a create).
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn count(&self) -> anyhow::Result<i64> {
        let row = sqlx::query("SELECT count(*) AS n FROM forward_destinations")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("n")?)
    }

    /// Insert a destination; returns its id. A duplicate name surfaces as a unique violation for
    /// the handler to map to 409.
    ///
    /// # Errors
    /// Propagates sealing and database failures.
    pub async fn create(&self, input: &ForwardDestinationInput) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        let sealed = self.seal(input.secret.as_ref())?;
        sqlx::query(
            "INSERT INTO forward_destinations \
             (id, name, enabled, source_kind, dest_kind, target, pool, verbatim, filter, \
              rate_limit_per_sec, key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce, ca_cert) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
        )
        .bind(id)
        .bind(&input.name)
        .bind(input.enabled)
        .bind(input.source_kind.as_str())
        .bind(input.dest_kind.as_str())
        .bind(&input.target)
        .bind(input.pool.as_deref())
        .bind(input.verbatim)
        .bind(sqlx::types::Json(&input.filter))
        .bind(input.rate_limit_per_sec.map(i64::from))
        .bind(sealed.as_ref().map(|s| i64::from(s.key_id)))
        .bind(sealed.as_ref().map(|s| s.wrapped_dek.clone()))
        .bind(sealed.as_ref().map(|s| s.dek_nonce.clone()))
        .bind(sealed.as_ref().map(|s| s.ciphertext.clone()))
        .bind(sealed.as_ref().map(|s| s.ct_nonce.clone()))
        .bind(input.ca_cert.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Update a destination in place. A `None` secret keeps whatever is stored, so an admin can
    /// edit a target without re-entering the community. Returns whether a row changed.
    ///
    /// # Errors
    /// Propagates sealing and database failures.
    pub async fn update(&self, id: Uuid, input: &ForwardDestinationInput) -> anyhow::Result<bool> {
        let sealed = self.seal(input.secret.as_ref())?;
        // The all-or-none CHECK stays satisfied because the five sealed columns are bound from the
        // same `Option` — they are either all replaced or all coalesced back to their stored value.
        let res = sqlx::query(UPDATE_SQL)
            .bind(id)
            .bind(&input.name)
            .bind(input.enabled)
            .bind(input.source_kind.as_str())
            .bind(input.dest_kind.as_str())
            .bind(&input.target)
            .bind(input.pool.as_deref())
            .bind(input.verbatim)
            .bind(sqlx::types::Json(&input.filter))
            .bind(input.rate_limit_per_sec.map(i64::from))
            .bind(sealed.as_ref().map(|s| i64::from(s.key_id)))
            .bind(sealed.as_ref().map(|s| s.wrapped_dek.clone()))
            .bind(sealed.as_ref().map(|s| s.dek_nonce.clone()))
            .bind(sealed.as_ref().map(|s| s.ciphertext.clone()))
            .bind(sealed.as_ref().map(|s| s.ct_nonce.clone()))
            .bind(input.ca_cert.as_deref())
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Delete a destination. Returns whether a row was removed.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn delete(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM forward_destinations WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Every **enabled** destination with its secret decrypted — the forwarder's live snapshot.
    /// A row whose secret fails to open is still returned (without it) rather than dropped: losing
    /// the community degrades a re-encoded trap, it should not silently stop forwarding.
    ///
    /// # Errors
    /// Propagates database and row-decode failures.
    pub async fn list_open(&self) -> anyhow::Result<Vec<OpenDestination>> {
        let sql = format!(
            "SELECT {PUBLIC_COLUMNS}, key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce \
             FROM forward_destinations WHERE enabled = true ORDER BY created_at"
        );
        let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
        rows.into_iter().map(|row| self.row_to_open(row)).collect()
    }

    /// One destination with its secret decrypted, **regardless of `enabled`**. Backs the Test
    /// button, whose whole point is validating a destination before turning it on — falling back to
    /// the public row there would test with the wrong SNMP community.
    ///
    /// # Errors
    /// Propagates database and row-decode failures.
    pub async fn get_open(&self, id: Uuid) -> anyhow::Result<Option<OpenDestination>> {
        let sql = format!(
            "SELECT {PUBLIC_COLUMNS}, key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce \
             FROM forward_destinations WHERE id = $1"
        );
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| self.row_to_open(row)).transpose()
    }

    fn row_to_open(&self, row: sqlx::postgres::PgRow) -> anyhow::Result<OpenDestination> {
        let key_id: Option<i64> = row.try_get("key_id")?;
        let secret = match key_id {
            None => None,
            Some(key_id) => {
                let sealed = SealedSecret {
                    key_id: u32::try_from(key_id).unwrap_or(0),
                    wrapped_dek: row.try_get("wrapped_dek")?,
                    dek_nonce: row.try_get("dek_nonce")?,
                    ciphertext: row.try_get("ciphertext")?,
                    ct_nonce: row.try_get("ct_nonce")?,
                };
                self.cipher
                    .open(&sealed)
                    .ok()
                    .and_then(|pt| serde_json::from_slice::<DestSecret>(&pt).ok())
            }
        };
        let dest = row_to_dest(row)?;
        if key_id.is_some() && secret.is_none() {
            tracing::warn!(destination = %dest.id, "forwarding destination secret decrypt failed; using defaults");
        }
        Ok(OpenDestination { dest, secret })
    }

    fn seal(&self, secret: Option<&DestSecret>) -> anyhow::Result<Option<SealedSecret>> {
        secret
            .map(|s| {
                let plaintext = serde_json::to_vec(s)?;
                self.cipher
                    .seal(&plaintext)
                    .map_err(|e| anyhow::anyhow!("seal forwarding secret: {e}"))
            })
            .transpose()
    }
}

fn row_to_dest(row: sqlx::postgres::PgRow) -> anyhow::Result<ForwardDestination> {
    let source_kind: String = row.try_get("source_kind")?;
    let dest_kind: String = row.try_get("dest_kind")?;
    let filter: sqlx::types::Json<FilterExpr> = row.try_get("filter")?;
    let rate_limit: Option<i64> = row.try_get("rate_limit_per_sec")?;
    Ok(ForwardDestination {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        enabled: row.try_get("enabled")?,
        // The DB CHECK constrains these columns, so an unknown value means the row was written by a
        // newer binary; falling back keeps the list rendering instead of failing the whole query.
        source_kind: SourceKind::parse(&source_kind).unwrap_or(SourceKind::Syslog),
        dest_kind: DestKind::parse(&dest_kind).unwrap_or(DestKind::SyslogUdp),
        target: row.try_get("target")?,
        pool: row.try_get("pool")?,
        verbatim: row.try_get("verbatim")?,
        filter: filter.0,
        rate_limit_per_sec: rate_limit.and_then(|n| u32::try_from(n).ok()),
        ca_cert: row.try_get("ca_cert")?,
        has_secret: row.try_get("has_secret")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_columns_never_select_a_sealed_field() {
        // The API serializes whatever `row_to_dest` builds, so the guard belongs on the projection:
        // a secret column must only ever be read by `list_open`, which adds them explicitly.
        for sealed in ["wrapped_dek", "dek_nonce", "ciphertext", "ct_nonce"] {
            assert!(
                !PUBLIC_COLUMNS.contains(sealed),
                "{sealed} must not be in the public projection"
            );
        }
        // `key_id` appears only inside the `has_secret` predicate, never as a value.
        assert!(PUBLIC_COLUMNS.contains("(key_id IS NOT NULL) AS has_secret"));
    }

    #[test]
    fn dest_secret_round_trips_through_json() {
        let secret = DestSecret::SnmpCommunity {
            community: "private".into(),
        };
        let json = serde_json::to_string(&secret).unwrap();
        assert!(json.contains("\"kind\":\"snmp_community\""), "{json}");
        assert_eq!(
            serde_json::from_str::<DestSecret>(&json).unwrap(),
            secret,
            "the sealed blob must survive a round trip through the envelope"
        );
    }

    #[test]
    fn destination_json_has_no_secret_material() {
        let dest = ForwardDestination {
            id: Uuid::nil(),
            name: "siem".into(),
            enabled: true,
            source_kind: SourceKind::Trap,
            dest_kind: DestKind::SnmpTrapUdp,
            target: "10.0.0.5:162".into(),
            pool: Some("tokyo".into()),
            verbatim: true,
            filter: FilterExpr::default(),
            rate_limit_per_sec: Some(500),
            ca_cert: None,
            has_secret: true,
        };
        let json = serde_json::to_string(&dest).unwrap();
        assert!(json.contains("\"has_secret\":true"));
        for leak in ["community", "wrapped_dek", "ciphertext"] {
            assert!(!json.contains(leak), "{leak} leaked into {json}");
        }
    }

    #[test]
    fn ca_cert_is_assigned_not_coalesced_so_it_can_be_cleared() {
        // Unlike the sealed columns, a CA certificate is not a secret the form omits — it round
        // trips, so a plain assignment is right and "remove the pinned CA" has to work.
        assert!(UPDATE_SQL.contains("ca_cert = $16"));
        assert!(!UPDATE_SQL.contains("ca_cert = COALESCE("));
        // ...and it is a public column, so the API list returns it.
        assert!(PUBLIC_COLUMNS.contains("ca_cert"));
    }

    #[test]
    fn update_coalesces_every_sealed_column_so_an_omitted_secret_survives() {
        // Editing a destination without re-entering its secret must not wipe it. A plain
        // `wrapped_dek = $12` would, silently, and only on the rows that had a secret.
        for column in [
            "key_id",
            "wrapped_dek",
            "dek_nonce",
            "ciphertext",
            "ct_nonce",
        ] {
            assert!(
                UPDATE_SQL.contains(&format!("{column} = COALESCE(")),
                "{column} must be coalesced, not assigned"
            );
        }
    }

    #[test]
    fn update_touches_updated_at_and_is_scoped_to_one_row() {
        assert!(UPDATE_SQL.contains("updated_at = now()"));
        assert!(UPDATE_SQL.trim_end().ends_with("WHERE id = $1"));
    }
}
