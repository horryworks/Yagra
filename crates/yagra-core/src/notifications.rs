//! Notification channels + routing rules (Phase A).
//!
//! A **channel** is a place alerts can be delivered (webhook/email); its connection config is
//! a secret, sealed with the same envelope cipher + KEK as credentials (ADR-018) — only
//! ciphertext is stored and the API never returns it. A **routing rule** selects which alerts
//! (by severity) fan out to which channels. The live snapshot is loaded into the [`Notifier`]
//! periodically (like thresholds) so edits take effect without a restart.
//!
//! [`Notifier`]: crate::alerts::Notifier

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::Severity;
use yagra_secrets::{EnvelopeCipher, SealedSecret, StaticKeyProvider};

use crate::secrets::load_key_provider;

/// A delivery channel kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Webhook,
    Email,
    /// PagerDuty Events API v2 — native fire/resolve lifecycle via `dedup_key`.
    /// (snake_case would derive `pager_duty`; the wire/DB token is `pagerduty`.)
    #[serde(rename = "pagerduty")]
    PagerDuty,
    /// Jira Service Management Alerts (Opsgenie-compatible) — dedup/close via `alias`.
    Jsm,
}

impl ChannelKind {
    fn as_str(self) -> &'static str {
        match self {
            ChannelKind::Webhook => "webhook",
            ChannelKind::Email => "email",
            ChannelKind::PagerDuty => "pagerduty",
            ChannelKind::Jsm => "jsm",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s {
            "webhook" => Some(ChannelKind::Webhook),
            "email" => Some(ChannelKind::Email),
            "pagerduty" => Some(ChannelKind::PagerDuty),
            "jsm" => Some(ChannelKind::Jsm),
            _ => None,
        }
    }
}

/// The (secret) connection config for a channel — sealed at rest, never returned by the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChannelConfig {
    Webhook {
        url: String,
    },
    Email {
        host: String,
        #[serde(default)]
        port: Option<u16>,
        from: String,
        to: String,
        #[serde(default)]
        user: Option<String>,
        #[serde(default)]
        pass: Option<String>,
    },
    /// PagerDuty Events API v2. `routing_key` is the integration key (a secret).
    /// `api_url` overrides the default US endpoint (EU: `https://events.eu.pagerduty.com/v2/enqueue`);
    /// allowed hosts are pinned at the API edge (`validate_channel_config`).
    #[serde(rename = "pagerduty")]
    PagerDuty {
        routing_key: String,
        #[serde(default)]
        api_url: Option<String>,
    },
    /// JSM Alerts / Opsgenie-compatible API. `api_url` is the integration base
    /// (e.g. `https://api.atlassian.com/jsm/ops/integration/v2`); `api_key` is the
    /// GenieKey (a secret).
    Jsm {
        api_url: String,
        api_key: String,
    },
}

impl ChannelConfig {
    /// The kind this config is for (must match the channel's declared kind).
    #[must_use]
    pub fn kind(&self) -> ChannelKind {
        match self {
            ChannelConfig::Webhook { .. } => ChannelKind::Webhook,
            ChannelConfig::Email { .. } => ChannelKind::Email,
            ChannelConfig::PagerDuty { .. } => ChannelKind::PagerDuty,
            ChannelConfig::Jsm { .. } => ChannelKind::Jsm,
        }
    }
}

/// Channel metadata for the API — never the secret config.
#[derive(Debug, Clone, Serialize)]
pub struct ChannelSummary {
    pub id: Uuid,
    pub name: String,
    pub kind: ChannelKind,
    pub enabled: bool,
}

/// A channel with its config decrypted — core-side only, for building live delivery channels.
/// (Only enabled channels are loaded, so there's no `enabled` flag here.)
#[derive(Debug, Clone)]
pub struct OpenChannel {
    pub id: Uuid,
    pub config: ChannelConfig,
}

/// A routing rule: enabled alerts of `severity` (None = any) fan out to `channel_ids`.
#[derive(Debug, Clone, Serialize)]
pub struct RoutingRule {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub severity: Option<Severity>,
    pub channel_ids: Vec<Uuid>,
}

/// Render a severity to its snake_case DB token.
fn severity_str(s: Severity) -> &'static str {
    match s {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Critical => "critical",
    }
}

/// Parse a severity DB token (None for NULL/unknown ⇒ matches all severities).
fn parse_severity(s: Option<String>) -> Option<Severity> {
    match s.as_deref() {
        Some("info") => Some(Severity::Info),
        Some("warning") => Some(Severity::Warning),
        Some("critical") => Some(Severity::Critical),
        _ => None,
    }
}

/// PostgreSQL-backed store for notification channels + routing rules.
pub struct NotificationRepo {
    pool: PgPool,
    cipher: EnvelopeCipher<StaticKeyProvider>,
}

impl NotificationRepo {
    /// Build the store, reusing the same KEK as the credential store.
    #[must_use]
    pub fn from_env(pool: PgPool) -> Self {
        Self {
            pool,
            cipher: EnvelopeCipher::new(load_key_provider()),
        }
    }

    // ── Channels ──────────────────────────────────────────────────────────────────────

    /// Channel metadata (no secrets).
    pub async fn list_channels(&self) -> anyhow::Result<Vec<ChannelSummary>> {
        let rows = sqlx::query(
            "SELECT id, name, kind, enabled FROM notification_channels ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let kind: String = row.try_get("kind")?;
                Ok(ChannelSummary {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    kind: ChannelKind::parse(&kind).unwrap_or(ChannelKind::Webhook),
                    enabled: row.try_get("enabled")?,
                })
            })
            .collect()
    }

    /// Seal and store a new channel; returns its id. The config is encrypted before it
    /// touches the database and is never logged.
    pub async fn create_channel(&self, name: &str, config: &ChannelConfig) -> anyhow::Result<Uuid> {
        let plaintext = serde_json::to_vec(config)?;
        let sealed = self
            .cipher
            .seal(&plaintext)
            .map_err(|e| anyhow::anyhow!("seal channel config: {e}"))?;
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO notification_channels \
             (id, name, kind, enabled, key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce) \
             VALUES ($1, $2, $3, true, $4, $5, $6, $7, $8)",
        )
        .bind(id)
        .bind(name)
        .bind(config.kind().as_str())
        .bind(i64::from(sealed.key_id))
        .bind(&sealed.wrapped_dek)
        .bind(&sealed.dek_nonce)
        .bind(&sealed.ciphertext)
        .bind(&sealed.ct_nonce)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Enable/disable a channel. Returns whether a row changed.
    pub async fn set_channel_enabled(&self, id: Uuid, enabled: bool) -> anyhow::Result<bool> {
        let res = sqlx::query("UPDATE notification_channels SET enabled = $2 WHERE id = $1")
            .bind(id)
            .bind(enabled)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Delete a channel (and drop it from any rule's channel list). Returns whether removed.
    pub async fn delete_channel(&self, id: Uuid) -> anyhow::Result<bool> {
        sqlx::query("UPDATE routing_rules SET channel_ids = array_remove(channel_ids, $1)")
            .bind(id)
            .execute(&self.pool)
            .await?;
        let res = sqlx::query("DELETE FROM notification_channels WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// All enabled channels with their config decrypted — core-side, for building the live
    /// delivery snapshot. Channels whose config fails to decrypt are skipped (logged).
    pub async fn list_open_channels(&self) -> anyhow::Result<Vec<OpenChannel>> {
        let rows = sqlx::query(
            "SELECT id, enabled, key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce \
             FROM notification_channels WHERE enabled = true",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::new();
        for row in rows {
            let id: Uuid = row.try_get("id")?;
            let key_id: i64 = row.try_get("key_id")?;
            let sealed = SealedSecret {
                key_id: u32::try_from(key_id).unwrap_or(0),
                wrapped_dek: row.try_get("wrapped_dek")?,
                dek_nonce: row.try_get("dek_nonce")?,
                ciphertext: row.try_get("ciphertext")?,
                ct_nonce: row.try_get("ct_nonce")?,
            };
            match self
                .cipher
                .open(&sealed)
                .ok()
                .and_then(|pt| serde_json::from_slice::<ChannelConfig>(&pt).ok())
            {
                Some(config) => out.push(OpenChannel { id, config }),
                None => {
                    tracing::warn!(channel = %id, "notification channel config decrypt failed; skipping")
                }
            }
        }
        Ok(out)
    }

    // ── Routing rules ─────────────────────────────────────────────────────────────────

    /// All routing rules.
    pub async fn list_rules(&self) -> anyhow::Result<Vec<RoutingRule>> {
        let rows = sqlx::query(
            "SELECT id, name, enabled, severity, channel_ids FROM routing_rules ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(RoutingRule {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    enabled: row.try_get("enabled")?,
                    severity: parse_severity(row.try_get("severity")?),
                    channel_ids: row.try_get("channel_ids")?,
                })
            })
            .collect()
    }

    /// Create a routing rule. `severity` None ⇒ matches all severities.
    pub async fn create_rule(
        &self,
        name: &str,
        severity: Option<Severity>,
        channel_ids: &[Uuid],
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO routing_rules (id, name, enabled, severity, channel_ids) \
             VALUES ($1, $2, true, $3, $4)",
        )
        .bind(id)
        .bind(name)
        .bind(severity.map(severity_str))
        .bind(channel_ids)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Enable/disable a rule. Returns whether a row changed.
    pub async fn set_rule_enabled(&self, id: Uuid, enabled: bool) -> anyhow::Result<bool> {
        let res = sqlx::query("UPDATE routing_rules SET enabled = $2 WHERE id = $1")
            .bind(id)
            .bind(enabled)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Delete a rule. Returns whether a row was removed.
    pub async fn delete_rule(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM routing_rules WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_config_webhook_round_trips_with_kind_tag() {
        let cfg = ChannelConfig::Webhook {
            url: "http://hook.test".to_owned(),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("\"kind\":\"webhook\""));
        let back: ChannelConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
        assert_eq!(back.kind(), ChannelKind::Webhook);
    }

    #[test]
    fn channel_config_email_defaults_optional_fields() {
        // Only required fields present → optional port/user/pass default to None.
        let json = r#"{"kind":"email","host":"smtp.test","from":"a@test","to":"b@test"}"#;
        let cfg: ChannelConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.kind(), ChannelKind::Email);
        match cfg {
            ChannelConfig::Email { port, user, .. } => {
                assert!(port.is_none() && user.is_none());
            }
            _ => panic!("expected email"),
        }
    }

    #[test]
    fn channel_config_pagerduty_and_jsm_round_trip() {
        let pd = ChannelConfig::PagerDuty {
            routing_key: "rk".to_owned(),
            api_url: None,
        };
        let json = serde_json::to_string(&pd).unwrap();
        assert!(json.contains("\"kind\":\"pagerduty\""));
        let back: ChannelConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, pd);
        assert_eq!(back.kind(), ChannelKind::PagerDuty);

        // api_url is optional (defaults at the delivery layer, N-1 tolerant).
        let json = r#"{"kind":"pagerduty","routing_key":"rk"}"#;
        let cfg: ChannelConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(
            cfg,
            ChannelConfig::PagerDuty { api_url: None, .. }
        ));

        let jsm = ChannelConfig::Jsm {
            api_url: "https://api.atlassian.com/jsm/ops/integration/v2".to_owned(),
            api_key: "key".to_owned(),
        };
        let json = serde_json::to_string(&jsm).unwrap();
        assert!(json.contains("\"kind\":\"jsm\""));
        let back: ChannelConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind(), ChannelKind::Jsm);

        assert_eq!(
            ChannelKind::parse("pagerduty"),
            Some(ChannelKind::PagerDuty)
        );
        assert_eq!(ChannelKind::parse("jsm"), Some(ChannelKind::Jsm));
        assert_eq!(ChannelKind::PagerDuty.as_str(), "pagerduty");
        assert_eq!(ChannelKind::Jsm.as_str(), "jsm");
    }

    #[test]
    fn severity_token_round_trip() {
        assert_eq!(
            parse_severity(Some("critical".to_owned())),
            Some(Severity::Critical)
        );
        assert_eq!(parse_severity(Some("bogus".to_owned())), None);
        assert_eq!(parse_severity(None), None);
        assert_eq!(severity_str(Severity::Warning), "warning");
    }
}
