// SPDX-License-Identifier: AGPL-3.0-only
//! Config backup (ADR-210 stage 2): which nodes the bundled oxidized fetches, and what it last
//! reported for each. Two tables, `node_config_backup` and `config_backup_status` (migration 0152).
//!
//! The fetching itself is oxidized's. This store answers the two questions Yagra owns: what a
//! person configured (a node, a `cli_login` credential, SSH or telnet, a model override), and the
//! last result oxidized gave, kept here so a screen can show it while oxidized is down.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::net::IpAddr;
use uuid::Uuid;

/// How oxidized reaches a node's command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BackupInput {
    /// SSH only.
    Ssh,
    /// Telnet only, for a device that has no SSH.
    Telnet,
    /// SSH, and telnet when SSH cannot connect.
    SshThenTelnet,
    /// A token a newer core wrote. Never sent to oxidized, and refused as input.
    Unknown,
}

crate::token_enum!(BackupInput, Unknown, "node_config_backup.input", [
    Ssh => "ssh",
    Telnet => "telnet",
    SshThenTelnet => "ssh_then_telnet",
    Unknown => "unknown",
]);

impl BackupInput {
    /// The `input` string oxidized reads: inputs tried in order, comma separated.
    #[must_use]
    pub const fn oxidized(self) -> Option<&'static str> {
        match self {
            Self::Ssh => Some("ssh"),
            Self::Telnet => Some("telnet"),
            Self::SshThenTelnet => Some("ssh, telnet"),
            Self::Unknown => None,
        }
    }
}

/// What a person configured for one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeConfigBackup {
    pub enabled: bool,
    pub credential_id: Option<Uuid>,
    pub input: BackupInput,
    /// The oxidized model a person picked; `None` means "from the node's profile".
    pub model_override: Option<String>,
}

/// One node oxidized should fetch, with what it needs to decide the model and sign in.
#[derive(Debug, Clone)]
pub struct BackupTarget {
    pub node_id: Uuid,
    pub address: IpAddr,
    /// The name of the node's profile, from which the model is looked up.
    pub profile_name: Option<String>,
    pub credential_id: Option<Uuid>,
    pub input: BackupInput,
    pub model_override: Option<String>,
    /// The node's address no longer matches the one it had when the configuration was saved; the
    /// login is withheld until someone saves it again (migration 0152's `bound_address`).
    pub address_changed: bool,
}

/// What oxidized last reported for a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupStatus {
    /// oxidized's own word: `success`, `no_connection`, `fail`, `timelimit` or `never`.
    pub status: String,
    /// When that fetch ended; `None` while oxidized has never fetched it.
    pub last_end: Option<DateTime<Utc>>,
    /// When Yagra last read it from oxidized.
    pub observed_at: DateTime<Utc>,
}

/// PostgreSQL-backed store for the two config-backup tables.
pub struct ConfigBackupRepo {
    pool: PgPool,
}

impl ConfigBackupRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// A node's configuration, or `None` when nobody configured one (the node is not fetched).
    pub async fn get(&self, node: Uuid) -> anyhow::Result<Option<NodeConfigBackup>> {
        let row = sqlx::query(
            "SELECT enabled, credential_id, input, model_override FROM node_config_backup \
             WHERE node_id = $1",
        )
        .bind(node)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            Ok(NodeConfigBackup {
                enabled: r.try_get("enabled")?,
                credential_id: r.try_get("credential_id")?,
                input: BackupInput::from_stored(&r.try_get::<String, _>("input")?),
                model_override: r.try_get("model_override")?,
            })
        })
        .transpose()
    }

    /// Replace a node's configuration, recording the node's current address as the one its login
    /// may be handed over to. `false` when the node does not exist.
    pub async fn put(&self, node: Uuid, cfg: &NodeConfigBackup) -> anyhow::Result<bool> {
        let done = sqlx::query(
            "INSERT INTO node_config_backup \
               (node_id, enabled, credential_id, input, model_override, bound_address) \
             SELECT id, $2, $3, $4, $5, address FROM nodes WHERE id = $1 \
             ON CONFLICT (node_id) DO UPDATE SET enabled = EXCLUDED.enabled, \
               credential_id = EXCLUDED.credential_id, input = EXCLUDED.input, \
               model_override = EXCLUDED.model_override, \
               bound_address = EXCLUDED.bound_address, updated_at = now()",
        )
        .bind(node)
        .bind(cfg.enabled)
        .bind(cfg.credential_id)
        .bind(cfg.input.as_str())
        .bind(cfg.model_override.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Every node switched on, with its address and profile name.
    pub async fn targets(&self) -> anyhow::Result<Vec<BackupTarget>> {
        let rows = sqlx::query(
            "SELECT b.node_id, host(n.address) AS address, p.name AS profile_name, \
                    b.credential_id, b.input, b.model_override, \
                    (b.bound_address IS DISTINCT FROM n.address) AS address_changed \
             FROM node_config_backup b \
             JOIN nodes n ON n.id = b.node_id \
             LEFT JOIN profiles p ON p.id = n.profile_id \
             WHERE b.enabled ORDER BY b.node_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let address: String = r.try_get("address")?;
                Ok(BackupTarget {
                    node_id: r.try_get("node_id")?,
                    address: address.parse().map_err(|e| {
                        anyhow::anyhow!("unparseable node address {address:?}: {e}")
                    })?,
                    profile_name: r.try_get("profile_name")?,
                    credential_id: r.try_get("credential_id")?,
                    input: BackupInput::from_stored(&r.try_get::<String, _>("input")?),
                    model_override: r.try_get("model_override")?,
                    address_changed: r.try_get("address_changed")?,
                })
            })
            .collect()
    }

    /// Whether a node's address has changed since its configuration was saved. `false` for a node
    /// with no configuration.
    pub async fn address_changed(&self, node: Uuid) -> anyhow::Result<bool> {
        let changed: Option<bool> = sqlx::query_scalar(
            "SELECT b.bound_address IS DISTINCT FROM n.address FROM node_config_backup b \
             JOIN nodes n ON n.id = b.node_id WHERE b.node_id = $1",
        )
        .bind(node)
        .fetch_optional(&self.pool)
        .await?;
        Ok(changed.unwrap_or(false))
    }

    /// The name of a node's profile — what its model is looked up from.
    pub async fn profile_name(&self, node: Uuid) -> anyhow::Result<Option<String>> {
        let name = sqlx::query_scalar(
            "SELECT p.name FROM nodes n JOIN profiles p ON p.id = n.profile_id WHERE n.id = $1",
        )
        .bind(node)
        .fetch_optional(&self.pool)
        .await?;
        Ok(name)
    }

    /// What oxidized last reported for a node, if it ever has.
    pub async fn status(&self, node: Uuid) -> anyhow::Result<Option<BackupStatus>> {
        let row = sqlx::query(
            "SELECT status, last_end, observed_at FROM config_backup_status WHERE node_id = $1",
        )
        .bind(node)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            Ok(BackupStatus {
                status: r.try_get("status")?,
                last_end: r.try_get("last_end")?,
                observed_at: r.try_get("observed_at")?,
            })
        })
        .transpose()
    }

    /// Record what oxidized reported, one row per node. A node oxidized does not know (deleted
    /// since, or never configured) is skipped by the join rather than failing the batch.
    pub async fn record_statuses(
        &self,
        rows: &[(Uuid, String, Option<DateTime<Utc>>)],
    ) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
        let statuses: Vec<String> = rows.iter().map(|r| r.1.clone()).collect();
        let ends: Vec<Option<DateTime<Utc>>> = rows.iter().map(|r| r.2).collect();
        sqlx::query(
            "INSERT INTO config_backup_status (node_id, status, last_end, observed_at) \
             SELECT u.id, u.status, u.last_end, now() \
             FROM unnest($1::uuid[], $2::text[], $3::timestamptz[]) AS u(id, status, last_end) \
             JOIN nodes n ON n.id = u.id \
             ON CONFLICT (node_id) DO UPDATE SET status = EXCLUDED.status, \
               last_end = EXCLUDED.last_end, observed_at = now()",
        )
        .bind(&ids)
        .bind(&statuses)
        .bind(&ends)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_input_has_a_token_and_an_oxidized_spelling() {
        for input in BackupInput::ALL {
            assert_eq!(BackupInput::from_token(input.as_str()), Some(*input));
            assert_eq!(
                input.oxidized().is_some(),
                *input != BackupInput::Unknown,
                "{input:?}"
            );
            let serde = serde_json::to_value(input).expect("serialize");
            assert_eq!(serde, input.as_str(), "token and serde tag disagree");
        }
        assert_eq!(
            BackupInput::from_stored("ssh_over_carrier_pigeon"),
            BackupInput::Unknown
        );
        assert_eq!(BackupInput::Unknown.oxidized(), None);
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_configured_node_is_a_target_until_it_is_switched_off(pool: PgPool) {
        let repo = ConfigBackupRepo::new(pool.clone());
        let node = crate::pgtest::node(&pool, "sw-01", 1, None).await;
        assert!(repo.get(node).await.expect("get").is_none());
        let mut cfg = NodeConfigBackup {
            enabled: true,
            credential_id: None,
            input: BackupInput::Telnet,
            model_override: Some("ios".to_owned()),
        };
        assert!(repo.put(node, &cfg).await.expect("put"));
        assert_eq!(repo.get(node).await.expect("get"), Some(cfg.clone()));
        let targets = repo.targets().await.expect("targets");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].input, BackupInput::Telnet);
        assert!(!targets[0].address_changed);

        // The node moves: its login is withheld until the configuration is saved again.
        sqlx::query("UPDATE nodes SET address = '10.0.0.99' WHERE id = $1")
            .bind(node)
            .execute(&pool)
            .await
            .expect("move");
        assert!(repo.targets().await.expect("targets")[0].address_changed);
        assert!(repo.address_changed(node).await.expect("changed"));
        assert!(repo.put(node, &cfg).await.expect("put"));
        assert!(!repo.address_changed(node).await.expect("changed"));

        cfg.enabled = false;
        assert!(repo.put(node, &cfg).await.expect("put"));
        assert!(repo.targets().await.expect("targets").is_empty());
        // A node that does not exist is not configured.
        assert!(!repo.put(Uuid::new_v4(), &cfg).await.expect("put"));
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_status_is_recorded_and_an_unknown_node_is_skipped(pool: PgPool) {
        let repo = ConfigBackupRepo::new(pool.clone());
        let node = crate::pgtest::node(&pool, "sw-02", 2, None).await;
        let end = Utc::now();
        repo.record_statuses(&[
            (node, "success".to_owned(), Some(end)),
            (Uuid::new_v4(), "fail".to_owned(), None),
        ])
        .await
        .expect("record");
        let got = repo.status(node).await.expect("status").expect("a row");
        assert_eq!(got.status, "success");
        assert_eq!(crate::pgtest::rows(&pool, "config_backup_status").await, 1);
    }
}
