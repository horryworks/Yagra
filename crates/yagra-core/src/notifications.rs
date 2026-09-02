// SPDX-License-Identifier: AGPL-3.0-only
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
use yagra_secrets::{EnvelopeCipher, SealedSecret};

use crate::notify_render::ChannelTemplate;
use crate::secrets::Kek;

/// A delivery channel kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ChannelSummary {
    pub id: Uuid,
    pub name: String,
    pub kind: ChannelKind,
    pub enabled: bool,
    /// Template for the notification subject. Absent means Yagra's built-in wording is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_template: Option<String>,
    /// Template for the notification body. Absent means Yagra's built-in format is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_template: Option<String>,
}

/// A channel with its config decrypted — core-side only, for building live delivery channels.
/// (Only enabled channels are loaded, so there's no `enabled` flag here.)
#[derive(Debug, Clone)]
pub struct OpenChannel {
    pub id: Uuid,
    pub config: ChannelConfig,
    /// The operator's subject/body override (ADR-039). Empty = the built-in format.
    pub template: ChannelTemplate,
}

/// A routing rule: enabled alerts of `severity` (None = any) fan out to `channel_ids`.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
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
    s.as_deref().and_then(Severity::from_token)
}

/// PostgreSQL-backed store for notification channels + routing rules.
pub struct NotificationRepo {
    pool: PgPool,
    cipher: EnvelopeCipher<Kek>,
}

impl NotificationRepo {
    /// Build the store, reusing the same KEK as the credential store.
    #[must_use]
    pub fn new(pool: PgPool, kek: Kek) -> Self {
        Self {
            pool,
            cipher: EnvelopeCipher::new(kek),
        }
    }

    // ── Channels ──────────────────────────────────────────────────────────────────────

    /// Channel metadata (no secrets).
    ///
    /// Carries the notification template (ADR-039) because it is not a secret and the editor would
    /// otherwise need a per-channel round trip — which would also mean a second read endpoint on
    /// the ledger for something the list already had to fetch a row for.
    pub async fn list_channels(&self) -> anyhow::Result<Vec<ChannelSummary>> {
        let rows = sqlx::query(
            "SELECT id, name, kind, enabled, subject_template, body_template \
             FROM notification_channels ORDER BY created_at",
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
                    subject_template: row.try_get("subject_template")?,
                    body_template: row.try_get("body_template")?,
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

    /// Replace a channel's template override. Returns whether a row changed.
    ///
    /// The pair is replaced wholesale, and `None` on a field restores the built-in format for it —
    /// the dialog owns both fields, so a partial update would leave whichever one the operator
    /// cleared silently in place.
    pub async fn set_channel_template(
        &self,
        id: Uuid,
        template: &ChannelTemplate,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE notification_channels SET subject_template = $2, body_template = $3 \
             WHERE id = $1",
        )
        .bind(id)
        .bind(template.subject.as_deref())
        .bind(template.body.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// All enabled channels with their config decrypted — core-side, for building the live
    /// delivery snapshot. Channels whose config fails to decrypt are skipped (logged).
    pub async fn list_open_channels(&self) -> anyhow::Result<Vec<OpenChannel>> {
        let rows = sqlx::query(
            "SELECT id, enabled, key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce, \
                    subject_template, body_template \
             FROM notification_channels WHERE enabled = true",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::new();
        for row in rows {
            let id: Uuid = row.try_get("id")?;
            let key_id: i32 = row.try_get("key_id")?;
            let template = ChannelTemplate {
                subject: row.try_get("subject_template")?,
                body: row.try_get("body_template")?,
            };
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
                Some(config) => out.push(OpenChannel {
                    id,
                    config,
                    template,
                }),
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

    // ── Database tests (ADR-114) ───────────────────────────────────────────────────────
    //
    // A channel's config is a secret (a webhook URL, an SMTP password, a PagerDuty routing key),
    // so the first thing these check is that it is not in the row. The rest is the enabled/deleted
    // arithmetic that decides whether a notification is delivered — where every mistake is silent
    // in one direction (nothing arrives) and loud in the other (it arrives from a channel the
    // operator switched off).

    fn a_webhook(url: &str) -> ChannelConfig {
        ChannelConfig::Webhook {
            url: url.to_owned(),
        }
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_channels_config_is_sealed_and_never_comes_back_in_the_metadata(pool: sqlx::PgPool) {
        const SECRET: &str = "https://hooks.example.test/T000/B000/zzTOPSECRETzz";
        let repo = NotificationRepo::new(pool.clone(), crate::pgtest::kek());
        let id = repo
            .create_channel("Ops webhook", &a_webhook(SECRET))
            .await
            .expect("create");

        // Nothing on the row carries the URL in the clear. Read as text over every column the
        // table has, so a column added later that happens to hold it fails this rather than
        // shipping quietly.
        let dumped: String = sqlx::query_scalar(
            "SELECT string_agg(notification_channels::text, ' ') FROM notification_channels",
        )
        .fetch_one(&pool)
        .await
        .expect("dump");
        assert!(
            !dumped.contains("zzTOPSECRETzz"),
            "the channel config reached the database in the clear"
        );

        // The metadata list is what the API serves, and it carries no config at all — there is no
        // field on `ChannelSummary` for one, which is the point.
        let listed = repo.list_channels().await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert_eq!(listed[0].name, "Ops webhook");
        assert_eq!(listed[0].kind, ChannelKind::Webhook);
        assert!(listed[0].enabled, "a new channel is enabled");
        assert_eq!(listed[0].subject_template, None);
        assert_eq!(listed[0].body_template, None);

        // …and the core-side read opens it again, which is what says the seal is reversible with
        // the same key rather than merely opaque.
        let open = repo.list_open_channels().await.expect("open");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, id);
        assert_eq!(open[0].config, a_webhook(SECRET));

        // The declared kind is stored beside the sealed blob, so the list can group and filter
        // without opening anything. It has to be the config's own kind, not a separate argument.
        let jsm = repo
            .create_channel(
                "JSM",
                &ChannelConfig::Jsm {
                    api_url: "https://api.atlassian.test/jsm".into(),
                    api_key: "GenieKey zzALSOSECRETzz".into(),
                },
            )
            .await
            .expect("create");
        let listed = repo.list_channels().await.expect("list");
        assert_eq!(
            listed.iter().map(|c| (c.id, c.kind)).collect::<Vec<_>>(),
            [(id, ChannelKind::Webhook), (jsm, ChannelKind::Jsm)],
            "ordered by created_at"
        );
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_switched_off_channel_is_still_listed_and_no_longer_delivers(pool: sqlx::PgPool) {
        let repo = NotificationRepo::new(pool.clone(), crate::pgtest::kek());
        let on = repo
            .create_channel("Stays on", &a_webhook("https://a.example.test/hook"))
            .await
            .expect("create");
        let off = repo
            .create_channel("Switched off", &a_webhook("https://b.example.test/hook"))
            .await
            .expect("create");

        assert!(repo.set_channel_enabled(off, false).await.expect("disable"));

        // The editor still shows it — an operator has to be able to switch it back on.
        let listed = repo.list_channels().await.expect("list");
        assert_eq!(
            listed.iter().map(|c| (c.id, c.enabled)).collect::<Vec<_>>(),
            [(on, true), (off, false)]
        );

        // The delivery snapshot does not. `WHERE enabled = true` is the only thing standing
        // between "switched off" and a notification arriving from it anyway.
        let open = repo.list_open_channels().await.expect("open");
        assert_eq!(open.iter().map(|c| c.id).collect::<Vec<_>>(), [on]);

        // …and back on again, because a switch that only travels one way is half a switch.
        assert!(repo.set_channel_enabled(off, true).await.expect("enable"));
        let open = repo.list_open_channels().await.expect("open");
        assert_eq!(open.iter().map(|c| c.id).collect::<Vec<_>>(), [on, off]);

        // An id that is not there reports it rather than reporting success.
        assert!(!repo
            .set_channel_enabled(Uuid::new_v4(), false)
            .await
            .expect("disable"));
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn deleting_a_channel_takes_it_out_of_every_rule_that_named_it(pool: sqlx::PgPool) {
        let repo = NotificationRepo::new(pool.clone(), crate::pgtest::kek());
        let doomed = repo
            .create_channel("Doomed", &a_webhook("https://a.example.test/hook"))
            .await
            .expect("create");
        let kept = repo
            .create_channel("Kept", &a_webhook("https://b.example.test/hook"))
            .await
            .expect("create");
        let both = repo
            .create_rule("Everything", None, &[doomed, kept])
            .await
            .expect("rule");
        let only_doomed = repo
            .create_rule("Just the doomed one", Some(Severity::Critical), &[doomed])
            .await
            .expect("rule");

        assert!(repo.delete_channel(doomed).await.expect("delete"));

        // 🚨 This is two statements, and the first one is the one that matters. Without the
        // `array_remove` the rules keep an id that no longer names a channel, and the fan-out then
        // routes an alert to nothing — silently, because a missing channel is indistinguishable
        // from one that delivered.
        let rules = repo.list_rules().await.expect("rules");
        let by_id = |id: Uuid| rules.iter().find(|r| r.id == id).expect("rule");
        assert_eq!(by_id(both).channel_ids, vec![kept]);
        assert!(
            by_id(only_doomed).channel_ids.is_empty(),
            "a rule left pointing at a deleted channel: {:?}",
            by_id(only_doomed).channel_ids
        );

        // The rules themselves survive — deleting a channel is not deleting the routing an
        // operator built around it.
        assert_eq!(rules.len(), 2);
        assert_eq!(repo.list_channels().await.expect("list").len(), 1);
        assert!(!repo.delete_channel(doomed).await.expect("delete"));
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_template_override_is_replaced_as_a_pair_not_merged(pool: sqlx::PgPool) {
        let repo = NotificationRepo::new(pool.clone(), crate::pgtest::kek());
        let id = repo
            .create_channel("Ops", &a_webhook("https://a.example.test/hook"))
            .await
            .expect("create");

        assert!(repo
            .set_channel_template(
                id,
                &ChannelTemplate {
                    subject: Some("[{{severity}}] {{node}}".into()),
                    body: Some("{{metric}} = {{value}}".into()),
                },
            )
            .await
            .expect("template"));
        let listed = repo.list_channels().await.expect("list");
        assert_eq!(
            listed[0].subject_template.as_deref(),
            Some("[{{severity}}] {{node}}")
        );
        assert_eq!(
            listed[0].body_template.as_deref(),
            Some("{{metric}} = {{value}}")
        );

        // Clearing one field must clear it. The dialog owns both, so a partial update would leave
        // whichever one the operator cleared silently in place and the channel would keep sending
        // wording nobody could find in the editor.
        assert!(repo
            .set_channel_template(
                id,
                &ChannelTemplate {
                    subject: None,
                    body: Some("only the body now".into()),
                },
            )
            .await
            .expect("template"));
        let listed = repo.list_channels().await.expect("list");
        assert_eq!(listed[0].subject_template, None);
        assert_eq!(
            listed[0].body_template.as_deref(),
            Some("only the body now")
        );

        // The core-side read carries the same pair — the renderer reads it from there, not from
        // the metadata list, so the two must agree.
        let open = repo.list_open_channels().await.expect("open");
        assert_eq!(open[0].template.subject, None);
        assert_eq!(open[0].template.body.as_deref(), Some("only the body now"));

        assert!(!repo
            .set_channel_template(Uuid::new_v4(), &ChannelTemplate::default())
            .await
            .expect("template"));
    }
}
