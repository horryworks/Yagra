// SPDX-License-Identifier: AGPL-3.0-only
//! Config backup (ADR-210 stages 2 and 3): which nodes the bundled oxidized fetches, which login
//! each is handed, and every login attempt. Three tables: `node_config_backup`,
//! `config_backup_status` (migration 0152) and `config_backup_attempts` (migration 0153).
//!
//! The fetching itself is oxidized's. This store answers what Yagra owns: what a person configured
//! (a node, the order of CLI logins to try, SSH or telnet, a model override), which login is being
//! tried or has worked, and every attempt — which is what the lockout limits count, so the limits
//! hold across a restart and an HA failover.

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

/// How fast a node tries its CLI logins: each mode is one set of lockout limits (ADR-210 stage 3
/// decision 27). The numbers are core's (`oxidized::rotation::limits_of`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScanMode {
    /// A minute apart, for a device nothing locks out.
    Rapid,
    /// Ten minutes apart. The default.
    #[default]
    Normal,
    /// An hour apart, three a day, for an account a lockout would hurt.
    Slow,
    /// A token a newer core wrote. Enforced as the most careful mode, and refused as input.
    Unknown,
}

crate::token_enum!(ScanMode, Unknown, "node_config_backup.scan_mode", [
    Rapid => "rapid",
    Normal => "normal",
    Slow => "slow",
    Unknown => "unknown",
]);

/// How one login attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    /// The configuration was fetched.
    Success,
    /// The device refused the login.
    AuthFailed,
    /// The device did not answer, or refused the connection: no login was tried.
    Unreachable,
    /// Any other failure. Over telnet a refused login ends here, because the device only asks for
    /// the login again and oxidized reports a prompt it could not find.
    Other,
    /// Yagra asked for a fetch and never heard how it ended.
    Lost,
    /// A token a newer core wrote.
    Unknown,
}

crate::token_enum!(AttemptOutcome, Unknown, "config_backup_attempts.outcome", [
    Success => "success",
    AuthFailed => "auth_failed",
    Unreachable => "unreachable",
    Other => "other",
    Lost => "lost",
    Unknown => "unknown",
]);

impl AttemptOutcome {
    /// Whether the attempt may have been a refused login: it counts against the limits, and the
    /// next login is tried. Everything but a success and a device that did not answer — an outcome
    /// that cannot be told apart from a refused login is counted as one, because the limits exist
    /// to stop short of a lockout.
    #[must_use]
    pub const fn is_login_failure(self) -> bool {
        match self {
            Self::AuthFailed | Self::Other | Self::Lost | Self::Unknown => true,
            Self::Success | Self::Unreachable => false,
        }
    }
}

/// What a person configured for one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeConfigBackup {
    pub enabled: bool,
    /// The node's own order of CLI logins; `None` means every CLI login in the try order.
    pub login_order: Option<Vec<Uuid>>,
    pub input: BackupInput,
    /// The oxidized model a person picked; `None` means "from the node's profile".
    pub model_override: Option<String>,
    /// How fast it tries its logins.
    pub scan_mode: ScanMode,
}

/// Where a node is in trying its logins. Written by Yagra, never by a person.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rotation {
    /// The login handed to oxidized (an older core reads this column as the node's login).
    pub credential_id: Option<Uuid>,
    /// That login has fetched successfully.
    pub login_confirmed: bool,
    /// When the current pass through the candidates began.
    pub round_started_at: Option<DateTime<Utc>>,
    /// When Yagra last asked oxidized to fetch and has not heard back.
    pub requested_at: Option<DateTime<Utc>>,
    /// When the limits next allow a try, for the screen.
    pub next_try_at: Option<DateTime<Utc>>,
    /// The newest version Yagra has seen.
    pub last_version_oid: Option<String>,
}

/// One configured node with what is needed to hand it to oxidized and to choose its next login.
#[derive(Debug, Clone)]
pub struct BackupTarget {
    pub node_id: Uuid,
    pub address: IpAddr,
    /// The name of the node's profile, from which the model is looked up.
    pub profile_name: Option<String>,
    pub config: NodeConfigBackup,
    pub rotation: Rotation,
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

/// One login attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub node_id: Uuid,
    /// `None` when the login has since been deleted.
    pub credential_id: Option<Uuid>,
    pub outcome: AttemptOutcome,
    /// For a success: whether it stored a new version. `None` for a failure, or when oxidized
    /// could not be asked.
    pub new_version: Option<bool>,
    pub at: DateTime<Utc>,
}

/// One row of the fleet-wide list (`GET /api/v1/config-backups`).
#[derive(Debug, Clone)]
pub struct BackupOverview {
    pub node_id: Uuid,
    pub rotation: Rotation,
    pub last: Option<Attempt>,
    pub last_success_at: Option<DateTime<Utc>>,
}

/// How long attempts are kept.
pub const ATTEMPT_RETENTION_DAYS: i32 = 30;

const ROTATION_COLUMNS: &str = "b.credential_id, b.login_confirmed, b.round_started_at, \
                                b.requested_at, b.next_try_at, b.last_version_oid";

fn rotation_of(r: &sqlx::postgres::PgRow) -> Result<Rotation, sqlx::Error> {
    Ok(Rotation {
        credential_id: r.try_get("credential_id")?,
        login_confirmed: r.try_get("login_confirmed")?,
        round_started_at: r.try_get("round_started_at")?,
        requested_at: r.try_get("requested_at")?,
        next_try_at: r.try_get("next_try_at")?,
        last_version_oid: r.try_get("last_version_oid")?,
    })
}

fn config_of(r: &sqlx::postgres::PgRow) -> Result<NodeConfigBackup, sqlx::Error> {
    Ok(NodeConfigBackup {
        enabled: r.try_get("enabled")?,
        login_order: r.try_get("login_order")?,
        input: BackupInput::from_stored(&r.try_get::<String, _>("input")?),
        model_override: r.try_get("model_override")?,
        scan_mode: ScanMode::from_stored(&r.try_get::<String, _>("scan_mode")?),
    })
}

fn attempt_of(r: &sqlx::postgres::PgRow) -> Result<Attempt, sqlx::Error> {
    Ok(Attempt {
        node_id: r.try_get("node_id")?,
        credential_id: r.try_get("credential_id")?,
        outcome: AttemptOutcome::from_stored(&r.try_get::<String, _>("outcome")?),
        new_version: r.try_get("new_version")?,
        at: r.try_get("at")?,
    })
}

/// PostgreSQL-backed store for the config-backup tables.
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
        Ok(self.get_with_rotation(node).await?.map(|(c, _)| c))
    }

    /// A node's configuration and where it is in trying its logins.
    pub async fn get_with_rotation(
        &self,
        node: Uuid,
    ) -> anyhow::Result<Option<(NodeConfigBackup, Rotation)>> {
        let row = sqlx::query(&format!(
            "SELECT b.enabled, b.login_order, b.input, b.model_override, b.scan_mode, {ROTATION_COLUMNS} \
             FROM node_config_backup b WHERE b.node_id = $1"
        ))
        .bind(node)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row
            .map(|r| Ok::<_, sqlx::Error>((config_of(&r)?, rotation_of(&r)?)))
            .transpose()?)
    }

    /// Replace a node's configuration, recording the node's current address as the one its logins
    /// may be handed over to. With `restart`, the node starts trying its logins afresh: nothing is
    /// remembered as working and no candidate counts as tried. `false` when the node does not
    /// exist.
    pub async fn put(
        &self,
        node: Uuid,
        cfg: &NodeConfigBackup,
        restart: bool,
    ) -> anyhow::Result<bool> {
        let done = sqlx::query(
            "INSERT INTO node_config_backup \
               (node_id, enabled, login_order, input, model_override, bound_address, \
                round_started_at, scan_mode) \
             SELECT id, $2, $3, $4, $5, address, now(), $7 FROM nodes WHERE id = $1 \
             ON CONFLICT (node_id) DO UPDATE SET enabled = EXCLUDED.enabled, \
               login_order = EXCLUDED.login_order, input = EXCLUDED.input, \
               model_override = EXCLUDED.model_override, scan_mode = EXCLUDED.scan_mode, \
               bound_address = EXCLUDED.bound_address, updated_at = now(), \
               credential_id = CASE WHEN $6 THEN NULL ELSE node_config_backup.credential_id END, \
               login_confirmed = node_config_backup.login_confirmed AND NOT $6, \
               round_started_at = CASE WHEN $6 THEN now() \
                                  ELSE node_config_backup.round_started_at END, \
               requested_at = CASE WHEN $6 THEN NULL ELSE node_config_backup.requested_at END, \
               next_try_at = CASE WHEN $6 THEN NULL ELSE node_config_backup.next_try_at END",
        )
        .bind(node)
        .bind(cfg.enabled)
        .bind(cfg.login_order.as_deref())
        .bind(cfg.input.as_str())
        .bind(cfg.model_override.as_deref())
        .bind(restart)
        .bind(cfg.scan_mode.as_str())
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Every node switched on, with its address, profile name and rotation.
    pub async fn targets(&self) -> anyhow::Result<Vec<BackupTarget>> {
        let rows = sqlx::query(&format!(
            "SELECT b.node_id, host(n.address) AS address, p.name AS profile_name, \
                    b.enabled, b.login_order, b.input, b.model_override, b.scan_mode, {ROTATION_COLUMNS}, \
                    (b.bound_address IS DISTINCT FROM n.address) AS address_changed \
             FROM node_config_backup b \
             JOIN nodes n ON n.id = b.node_id \
             LEFT JOIN profiles p ON p.id = n.profile_id \
             WHERE b.enabled ORDER BY b.node_id"
        ))
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
                    config: config_of(r)?,
                    rotation: rotation_of(r)?,
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

    /// Hand `credential` to oxidized for `node` and remember that a fetch was asked for at `at` —
    /// unless a fetch asked for after `outstanding_after` is still out, in which case nothing
    /// changes and this answers `false`. The check and the write are one statement, so two callers
    /// (the leader's loop and a person's "fetch now") cannot both start a login at one device.
    pub async fn dispatch(
        &self,
        node: Uuid,
        credential: Uuid,
        at: DateTime<Utc>,
        outstanding_after: DateTime<Utc>,
    ) -> anyhow::Result<bool> {
        let done = sqlx::query(
            "UPDATE node_config_backup SET credential_id = $2, requested_at = $3, \
               next_try_at = NULL, \
               login_confirmed = login_confirmed AND credential_id IS NOT DISTINCT FROM $2 \
             WHERE node_id = $1 AND (requested_at IS NULL OR requested_at <= $4)",
        )
        .bind(node)
        .bind(credential)
        .bind(at)
        .bind(outstanding_after)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Whether any fetch asked for after `after` is still unanswered, at any node.
    pub async fn any_outstanding(&self, after: DateTime<Utc>) -> anyhow::Result<bool> {
        let out: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM node_config_backup WHERE requested_at > $1)",
        )
        .bind(after)
        .fetch_one(&self.pool)
        .await?;
        Ok(out)
    }

    /// Every fetch asked for after `after` and not yet answered, as an attempt with the login it
    /// was handed. The limits count these as if they will fail: until oxidized reports, a login
    /// sent to a device may be the one that locks it.
    pub async fn outstanding(&self, after: DateTime<Utc>) -> anyhow::Result<Vec<Attempt>> {
        let rows = sqlx::query(
            "SELECT node_id, credential_id, requested_at FROM node_config_backup \
             WHERE requested_at > $1 AND credential_id IS NOT NULL",
        )
        .bind(after)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(Attempt {
                    node_id: r.try_get("node_id")?,
                    credential_id: r.try_get("credential_id")?,
                    outcome: AttemptOutcome::Lost,
                    new_version: None,
                    at: r.try_get("requested_at")?,
                })
            })
            .collect()
    }

    /// Forget a fetch that oxidized was never actually asked for (it did not answer the request),
    /// so it is not later counted as a lost login.
    pub async fn clear_request(&self, node: Uuid) -> anyhow::Result<()> {
        sqlx::query("UPDATE node_config_backup SET requested_at = NULL WHERE node_id = $1")
            .bind(node)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Note when the limits next allow a try, for the screen. `None` clears it.
    pub async fn set_next_try(&self, node: Uuid, at: Option<DateTime<Utc>>) -> anyhow::Result<()> {
        sqlx::query("UPDATE node_config_backup SET next_try_at = $2 WHERE node_id = $1")
            .bind(node)
            .bind(at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Start a new pass through the candidates now: none counts as tried, and nothing is pending.
    /// What is remembered as working stays remembered.
    pub async fn restart_round(&self, node: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE node_config_backup SET round_started_at = now(), requested_at = NULL, \
               next_try_at = NULL WHERE node_id = $1",
        )
        .bind(node)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Start a new pass for every node with no working login — a CLI login was changed, so one
    /// that failed may work now.
    pub async fn restart_unconfirmed_rounds(&self) -> anyhow::Result<u64> {
        let done = sqlx::query(
            "UPDATE node_config_backup SET round_started_at = now(), next_try_at = NULL \
             WHERE NOT login_confirmed",
        )
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected())
    }

    /// Record how an attempt with the node's current login ended, and move the rotation on.
    ///
    /// A success remembers the login as working and, when `oid` is given, records it as the newest
    /// version — `new_version` says whether it differed from the last one seen. A refused login
    /// forgets a working one and starts a new pass from this failure, so the next candidate is
    /// tried rather than the same one again. Returns the attempt as recorded, or `None` when the
    /// node has no configuration.
    pub async fn record_result(
        &self,
        node: Uuid,
        outcome: AttemptOutcome,
        oid: Option<&str>,
    ) -> anyhow::Result<Option<Attempt>> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT credential_id, login_confirmed, last_version_oid, requested_at \
             FROM node_config_backup WHERE node_id = $1 FOR UPDATE",
        )
        .bind(node)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let confirmed: bool = row.try_get("login_confirmed")?;
        let requested: Option<DateTime<Utc>> = row.try_get("requested_at")?;
        // A result nobody is waiting for — a fetch reported after it was counted lost, while the
        // node is still looking for a login — cannot be tied to the login now on the row, which may
        // have moved on since. It is recorded, so the device's limit counts it, against no login,
        // and it moves nothing.
        let unrequested = !confirmed && requested.is_none() && outcome != AttemptOutcome::Lost;
        let credential: Option<Uuid> = if unrequested {
            None
        } else {
            row.try_get("credential_id")?
        };
        let previous: Option<String> = row.try_get("last_version_oid")?;
        let new_version = match (outcome, oid) {
            (AttemptOutcome::Success, Some(oid)) => Some(previous.as_deref() != Some(oid)),
            _ => None,
        };
        let at: DateTime<Utc> = sqlx::query_scalar(
            "INSERT INTO config_backup_attempts (node_id, credential_id, outcome, new_version) \
             VALUES ($1, $2, $3, $4) RETURNING at",
        )
        .bind(node)
        .bind(credential)
        .bind(outcome.as_str())
        .bind(new_version)
        .fetch_one(&mut *tx)
        .await?;
        if unrequested {
            // Nothing to move.
        } else if outcome == AttemptOutcome::Success {
            sqlx::query(
                "UPDATE node_config_backup SET login_confirmed = credential_id IS NOT NULL, \
                   requested_at = NULL, next_try_at = NULL, \
                   last_version_oid = COALESCE($2, last_version_oid) WHERE node_id = $1",
            )
            .bind(node)
            .bind(oid)
            .execute(&mut *tx)
            .await?;
        } else if outcome.is_login_failure() && confirmed {
            sqlx::query(
                "UPDATE node_config_backup SET login_confirmed = false, round_started_at = $2, \
                   requested_at = NULL WHERE node_id = $1",
            )
            .bind(node)
            .bind(at)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query("UPDATE node_config_backup SET requested_at = NULL WHERE node_id = $1")
                .bind(node)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(Some(Attempt {
            node_id: node,
            credential_id: credential,
            outcome,
            new_version,
            at,
        }))
    }

    /// Every attempt since `since`, across the fleet, oldest first — what the limits count.
    pub async fn attempts_since(&self, since: DateTime<Utc>) -> anyhow::Result<Vec<Attempt>> {
        let rows = sqlx::query(
            "SELECT node_id, credential_id, outcome, new_version, at FROM config_backup_attempts \
             WHERE at >= $1 ORDER BY at, id",
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(attempt_of).collect::<Result<_, _>>()?)
    }

    /// A node's newest attempts, newest first.
    pub async fn attempts_of(&self, node: Uuid, limit: i64) -> anyhow::Result<Vec<Attempt>> {
        let rows = sqlx::query(
            "SELECT node_id, credential_id, outcome, new_version, at FROM config_backup_attempts \
             WHERE node_id = $1 ORDER BY at DESC, id DESC LIMIT $2",
        )
        .bind(node)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(attempt_of).collect::<Result<_, _>>()?)
    }

    /// Every node switched on that `groups` lets the caller see (`None`: every one), with its
    /// rotation, its newest attempt and its newest success.
    pub async fn overview(&self, groups: Option<&[Uuid]>) -> anyhow::Result<Vec<BackupOverview>> {
        let rows = sqlx::query(&format!(
            "SELECT b.node_id, {ROTATION_COLUMNS}, \
                    a.credential_id AS a_credential_id, a.outcome AS a_outcome, \
                    a.new_version AS a_new_version, a.at AS a_at, s.at AS last_success_at \
             FROM node_config_backup b \
             JOIN nodes n ON n.id = b.node_id \
             LEFT JOIN LATERAL (SELECT credential_id, outcome, new_version, at \
                                FROM config_backup_attempts WHERE node_id = b.node_id \
                                ORDER BY at DESC, id DESC LIMIT 1) a ON true \
             LEFT JOIN LATERAL (SELECT at FROM config_backup_attempts \
                                WHERE node_id = b.node_id AND outcome = 'success' \
                                ORDER BY at DESC, id DESC LIMIT 1) s ON true \
             WHERE b.enabled AND {} ORDER BY n.name, b.node_id",
            crate::repo::scope_predicate(1, "n.group_id")
        ))
        .bind(groups)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let node_id: Uuid = r.try_get("node_id")?;
                let at: Option<DateTime<Utc>> = r.try_get("a_at")?;
                let last = match at {
                    Some(at) => Some(Attempt {
                        node_id,
                        credential_id: r.try_get("a_credential_id")?,
                        outcome: AttemptOutcome::from_stored(&r.try_get::<String, _>("a_outcome")?),
                        new_version: r.try_get("a_new_version")?,
                        at,
                    }),
                    None => None,
                };
                Ok(BackupOverview {
                    node_id,
                    rotation: rotation_of(r)?,
                    last,
                    last_success_at: r.try_get("last_success_at")?,
                })
            })
            .collect()
    }

    /// Remember the name of every node that has a backup configuration (ADR-210 stage 4 decision 39),
    /// so its configuration can still be filed under that name after the node is deleted. Called
    /// when oxidized reads its node list, every few minutes. A row is never deleted with its node.
    pub async fn remember_names(&self) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO config_backup_names (node_id, name, seen_at) \
             SELECT b.node_id, n.name, now() FROM node_config_backup b \
             JOIN nodes n ON n.id = b.node_id \
             ON CONFLICT (node_id) DO UPDATE SET name = EXCLUDED.name, seen_at = EXCLUDED.seen_at",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every node a configuration may be filed under: each remembered one, with its current name
    /// while it exists and its last known name once it does not.
    pub async fn export_names(&self) -> anyhow::Result<Vec<crate::config_remote::NameEntry>> {
        let rows = sqlx::query(
            "SELECT c.node_id, COALESCE(n.name, c.name) AS name, (n.id IS NOT NULL) AS registered \
             FROM config_backup_names c LEFT JOIN nodes n ON n.id = c.node_id \
             ORDER BY c.node_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(crate::config_remote::NameEntry {
                    node_id: r.try_get("node_id")?,
                    name: r.try_get("name")?,
                    registered: r.try_get("registered")?,
                })
            })
            .collect()
    }

    /// Delete attempts older than [`ATTEMPT_RETENTION_DAYS`].
    pub async fn prune_attempts(&self) -> anyhow::Result<u64> {
        let done = sqlx::query(
            "DELETE FROM config_backup_attempts WHERE at < now() - make_interval(days => $1)",
        )
        .bind(ATTEMPT_RETENTION_DAYS)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected())
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

    #[test]
    fn every_outcome_round_trips_and_only_success_and_silence_are_not_login_failures() {
        for o in AttemptOutcome::ALL {
            assert_eq!(AttemptOutcome::from_token(o.as_str()), Some(*o));
            assert_eq!(serde_json::to_value(o).expect("serialize"), o.as_str());
        }
        let not_failures: Vec<_> = AttemptOutcome::ALL
            .iter()
            .filter(|o| !o.is_login_failure())
            .collect();
        assert_eq!(
            not_failures,
            [&AttemptOutcome::Success, &AttemptOutcome::Unreachable]
        );
    }

    fn on(input: BackupInput, model: Option<&str>) -> NodeConfigBackup {
        NodeConfigBackup {
            enabled: true,
            login_order: None,
            input,
            model_override: model.map(str::to_owned),
            scan_mode: ScanMode::Normal,
        }
    }

    #[test]
    fn every_scan_mode_round_trips_and_an_unknown_one_is_unknown() {
        for m in ScanMode::ALL {
            assert_eq!(ScanMode::from_token(m.as_str()), Some(*m));
            assert_eq!(serde_json::to_value(m).expect("serialize"), m.as_str());
        }
        assert_eq!(ScanMode::from_stored("ludicrous"), ScanMode::Unknown);
        assert_eq!(ScanMode::default(), ScanMode::Normal);
    }

    /// A node's name is remembered, follows a rename, and outlives the node (ADR-210 stage 4
    /// decision 39).
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_name_is_remembered_after_its_node_is_deleted(pool: PgPool) {
        let repo = ConfigBackupRepo::new(pool.clone());
        let node = crate::pgtest::node(&pool, "sw-01", 1, None).await;
        let off = crate::pgtest::node(&pool, "sw-02", 2, None).await;
        assert!(repo
            .put(node, &on(BackupInput::Ssh, Some("ios")), true)
            .await
            .expect("put"));
        let mut stopped = on(BackupInput::Ssh, Some("ios"));
        stopped.enabled = false;
        assert!(repo.put(off, &stopped, false).await.expect("put"));
        repo.remember_names().await.expect("remember");
        let names = repo.export_names().await.expect("names");
        assert_eq!(
            names.len(),
            2,
            "a node with backup switched off is still filed by name"
        );
        assert!(names.iter().all(|e| e.registered));

        sqlx::query("UPDATE nodes SET name = 'sw-01a' WHERE id = $1")
            .bind(node)
            .execute(&pool)
            .await
            .expect("rename");
        let renamed = repo.export_names().await.expect("names");
        assert!(renamed
            .iter()
            .any(|e| e.node_id == node && e.name == "sw-01a"));

        sqlx::query("DELETE FROM nodes WHERE id = $1")
            .bind(node)
            .execute(&pool)
            .await
            .expect("delete");
        let after = repo.export_names().await.expect("names");
        let gone = after
            .iter()
            .find(|e| e.node_id == node)
            .expect("still there");
        assert!(!gone.registered);
        // The last name it was remembered under, not the one it had for a moment after.
        assert_eq!(gone.name, "sw-01");
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_configured_node_is_a_target_until_it_is_switched_off(pool: PgPool) {
        let repo = ConfigBackupRepo::new(pool.clone());
        let node = crate::pgtest::node(&pool, "sw-01", 1, None).await;
        assert!(repo.get(node).await.expect("get").is_none());
        let mut cfg = on(BackupInput::Telnet, Some("ios"));
        cfg.login_order = Some(vec![Uuid::from_u128(1)]);
        cfg.scan_mode = ScanMode::Slow;
        assert!(repo.put(node, &cfg, true).await.expect("put"));
        assert_eq!(repo.get(node).await.expect("get"), Some(cfg.clone()));
        let targets = repo.targets().await.expect("targets");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].config.input, BackupInput::Telnet);
        assert!(targets[0].rotation.round_started_at.is_some());
        assert!(!targets[0].address_changed);

        // The node moves: its login is withheld until the configuration is saved again.
        sqlx::query("UPDATE nodes SET address = '10.0.0.99' WHERE id = $1")
            .bind(node)
            .execute(&pool)
            .await
            .expect("move");
        assert!(repo.targets().await.expect("targets")[0].address_changed);
        assert!(repo.address_changed(node).await.expect("changed"));
        assert!(repo.put(node, &cfg, false).await.expect("put"));
        assert!(!repo.address_changed(node).await.expect("changed"));

        cfg.enabled = false;
        assert!(repo.put(node, &cfg, false).await.expect("put"));
        assert!(repo.targets().await.expect("targets").is_empty());
        // A node that does not exist is not configured.
        assert!(!repo.put(Uuid::new_v4(), &cfg, true).await.expect("put"));
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

    /// A success remembers the login and tells a new version from an unchanged one; a refused
    /// working login is forgotten and a new pass starts at that failure.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_result_moves_the_rotation_on(pool: PgPool) {
        let repo = ConfigBackupRepo::new(pool.clone());
        let node = crate::pgtest::node(&pool, "sw-03", 3, None).await;
        let login = crate::pgtest::credential(&pool, "ops", "cli_login").await;
        assert!(repo
            .put(node, &on(BackupInput::Ssh, Some("ios")), true)
            .await
            .expect("put"));
        assert!(repo
            .dispatch(
                node,
                login,
                Utc::now(),
                Utc::now() - chrono::TimeDelta::minutes(10)
            )
            .await
            .expect("dispatch"));

        let first = repo
            .record_result(node, AttemptOutcome::Success, Some("aaa"))
            .await
            .expect("record")
            .expect("configured");
        assert_eq!(first.new_version, Some(true));
        assert_eq!(first.credential_id, Some(login));
        let (_, rot) = repo
            .get_with_rotation(node)
            .await
            .expect("get")
            .expect("row");
        assert!(rot.login_confirmed);
        assert_eq!(rot.requested_at, None);
        let same = repo
            .record_result(node, AttemptOutcome::Success, Some("aaa"))
            .await
            .expect("record")
            .expect("configured");
        assert_eq!(same.new_version, Some(false));

        let refused = repo
            .record_result(node, AttemptOutcome::AuthFailed, None)
            .await
            .expect("record")
            .expect("configured");
        let (_, rot) = repo
            .get_with_rotation(node)
            .await
            .expect("get")
            .expect("row");
        assert!(!rot.login_confirmed);
        assert_eq!(rot.round_started_at, Some(refused.at));

        let all = repo.attempts_of(node, 10).await.expect("attempts");
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].outcome, AttemptOutcome::AuthFailed);
        let overview = repo.overview(None).await.expect("overview");
        assert_eq!(overview.len(), 1);
        assert_eq!(
            overview[0].last.as_ref().map(|a| a.outcome),
            Some(AttemptOutcome::AuthFailed)
        );
        assert_eq!(overview[0].last_success_at, Some(same.at));
        // A folder the node is not in sees nothing.
        assert!(repo
            .overview(Some(&[Uuid::new_v4()]))
            .await
            .expect("overview")
            .is_empty());
        assert!(repo
            .record_result(Uuid::new_v4(), AttemptOutcome::Success, None)
            .await
            .expect("record")
            .is_none());

        // With nothing asked for, a late success is recorded against no login and confirms none.
        let late = repo
            .record_result(node, AttemptOutcome::Success, Some("bbb"))
            .await
            .expect("record")
            .expect("configured");
        assert_eq!(late.credential_id, None);
        let (_, rot) = repo
            .get_with_rotation(node)
            .await
            .expect("get")
            .expect("row");
        assert!(!rot.login_confirmed);
        assert_eq!(rot.last_version_oid.as_deref(), Some("aaa"));
    }

    /// A second dispatch while one is out changes nothing, and the one out counts as an attempt
    /// and keeps oxidized's node list from being read again.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn one_fetch_at_a_time_and_it_counts_until_answered(pool: PgPool) {
        let repo = ConfigBackupRepo::new(pool.clone());
        let node = crate::pgtest::node(&pool, "sw-04", 4, None).await;
        let a = crate::pgtest::credential(&pool, "ops-a", "cli_login").await;
        let b = crate::pgtest::credential(&pool, "ops-b", "cli_login").await;
        assert!(repo
            .put(node, &on(BackupInput::Ssh, Some("ios")), true)
            .await
            .expect("put"));
        let now = Utc::now();
        let cutoff = now - chrono::TimeDelta::minutes(10);
        assert!(!repo.any_outstanding(cutoff).await.expect("idle"));
        assert!(repo.dispatch(node, a, now, cutoff).await.expect("first"));
        assert!(repo.any_outstanding(cutoff).await.expect("busy"));
        assert!(!repo.dispatch(node, b, now, cutoff).await.expect("second"));
        let out = repo.outstanding(cutoff).await.expect("outstanding");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].credential_id, Some(a));
        assert!(out[0].outcome.is_login_failure());
    }
}
