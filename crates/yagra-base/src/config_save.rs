// SPDX-License-Identifier: AGPL-3.0-only
//! Saving a device's running configuration from Yagra, and what Yagra last saw about whether it
//! was saved (ADR-222). Two tables, `config_saves` and `config_unsaved` (migration 0161).
//!
//! A save is a row: `pending` from the moment it is asked for, claimed (`running`) by the save
//! script that oxidized runs after the fetch in front of it succeeded, then `saved` or `failed`.
//! The script, the commands it may send and the decision to send them are core's; this store only
//! keeps the record, so a restart or an HA failover sees the same saves.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use uuid::Uuid;

/// The longest comparison output kept (Inc.9). A device prints only a few lines from its first
/// difference, so this is a bound on an input that has none, not a size anything is expected to
/// reach.
pub const COMPARE_OUTPUT_MAX: usize = 32 * 1024;

/// `text` cut to at most [`COMPARE_OUTPUT_MAX`] bytes, on a character boundary.
#[must_use]
pub fn cap_compare_output(text: &str) -> &str {
    if text.len() <= COMPARE_OUTPUT_MAX {
        return text;
    }
    let mut end = COMPARE_OUTPUT_MAX;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// What the save script last saw on one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsavedState {
    /// The first time running and startup were seen to differ; `None` while they agree.
    pub since: Option<DateTime<Utc>>,
    /// When the script last compared them.
    pub checked_at: DateTime<Utc>,
    /// What the device printed for the comparison, while they differ.
    pub compare_output: Option<String>,
}

/// Who asked for a save.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SaveOrigin {
    /// A person pressed the button (decision 7).
    Person,
    /// Yagra saved on its own after finding the device unsaved at a fetch (decision 13).
    Auto,
}

crate::token_enum!(SaveOrigin, [
    Person => "person",
    Auto => "auto",
]);

/// Where a save stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SaveState {
    /// Asked for; waiting for the fetch in front of it to succeed.
    Pending,
    /// The save script has claimed it and is logged in to the device.
    Running,
    /// The device answered the save as a success.
    Saved,
    /// It did not; `failure` says why.
    Failed,
}

crate::token_enum!(SaveState, [
    Pending => "pending",
    Running => "running",
    Saved => "saved",
    Failed => "failed",
]);

/// Why a save failed. The script reports one of these, never what the device printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SaveFailure {
    /// The login was refused.
    Auth,
    /// The device could not be reached.
    Unreachable,
    /// The device refused the save command — usually a login without the privilege to write.
    Denied,
    /// The device did not answer the save as a success, or asked a question with no known answer.
    NoAnswer,
    /// The device's model has no save command in Yagra's table.
    Unsupported,
    /// The fetch in front of the save did not succeed in time, so nothing was sent.
    NotFetched,
    /// Anything else.
    Other,
}

crate::token_enum!(SaveFailure, [
    Auth => "auth",
    Unreachable => "unreachable",
    Denied => "denied",
    NoAnswer => "no_answer",
    Unsupported => "unsupported",
    NotFetched => "not_fetched",
    Other => "other",
]);

/// One save, as a screen lists it.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SaveRecord {
    pub id: Uuid,
    pub origin: SaveOrigin,
    pub state: SaveState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<SaveFailure>,
    pub requested_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    /// Whether the startup configuration the save replaced was kept.
    pub kept_startup: bool,
}

/// Whether Yagra saves any device on its own (decision 8). A node switched on is saved in the same
/// login when a fetch finds it unsaved (decision 13). Since Inc.11 the switch is per node and this
/// says only whether at least one backed-up node is on (decision 22).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AutoSave {
    /// At least one backed-up node is saved automatically.
    pub enabled: bool,
}

/// Whether `/mcp` may hand a device configuration's text to an AI client (ADR-210 stage 10
/// decision 85). Off by default. oxidized's secret filter and Yagra's own second pass can both
/// miss a line, so switching this on is a person accepting that, not a setting Yagra assumes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct McpConfigText {
    /// Off by default.
    pub enabled: bool,
}

/// How many saves a node keeps. A newer one pushes the oldest finished one out.
pub const SAVES_KEPT: i64 = 10;

/// How long a save may wait for its fetch, and run, before it is given up as `not_fetched`.
/// oxidized's own time limit for one fetch is five minutes; this leaves room for one in the queue.
pub const SAVE_DEADLINE_SECS: i64 = 15 * 60;

/// The largest startup configuration kept beside a save. A switch stack's configuration is tens
/// of kilobytes; anything past this is cut, never refused, so a save is never lost to its record.
pub const STARTUP_KEPT_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub struct ConfigSaveRepo {
    pool: PgPool,
}

impl ConfigSaveRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Ask for a save. A node already holding one that is pending or running gets that one back
    /// rather than a second: two saves in a row save the same thing. The flag says whether this
    /// call created it, so a caller never records someone else's save as its own.
    pub async fn request(
        &self,
        node: Uuid,
        origin: SaveOrigin,
        by: Option<Uuid>,
    ) -> anyhow::Result<(Uuid, bool)> {
        let mut tx = self.pool.begin().await?;
        // Serialise requests for one node so two presses cannot both insert.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 222))")
            .bind(node)
            .execute(&mut *tx)
            .await?;
        if let Some(r) = sqlx::query(
            "SELECT id FROM config_saves WHERE node_id = $1 AND state IN ('pending', 'running') \
             AND requested_at > now() - make_interval(secs => $2) LIMIT 1",
        )
        .bind(node)
        .bind(SAVE_DEADLINE_SECS as f64)
        .fetch_optional(&mut *tx)
        .await?
        {
            tx.commit().await?;
            return Ok((r.try_get("id")?, false));
        }
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO config_saves (id, node_id, origin, requested_by, state) \
             VALUES ($1, $2, $3, $4, 'pending')",
        )
        .bind(id)
        .bind(node)
        .bind(origin.as_str())
        .bind(by)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM config_saves WHERE node_id = $1 AND state IN ('saved', 'failed') \
             AND id NOT IN (SELECT id FROM config_saves WHERE node_id = $1 \
                            ORDER BY requested_at DESC LIMIT $2)",
        )
        .bind(node)
        .bind(SAVES_KEPT)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok((id, true))
    }

    /// Claim the node's pending save for the script that is about to run it. `None` when there is
    /// none, or another script claimed it first.
    pub async fn claim(&self, node: Uuid) -> anyhow::Result<Option<(Uuid, SaveOrigin)>> {
        let row = sqlx::query(
            "UPDATE config_saves SET state = 'running' WHERE id = ( \
                 SELECT id FROM config_saves WHERE node_id = $1 AND state = 'pending' \
                 AND requested_at > now() - make_interval(secs => $2) \
                 ORDER BY requested_at LIMIT 1 FOR UPDATE SKIP LOCKED) \
             RETURNING id, origin",
        )
        .bind(node)
        .bind(SAVE_DEADLINE_SECS as f64)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            let origin: String = r.try_get("origin")?;
            Ok((
                r.try_get("id")?,
                SaveOrigin::from_token(&origin).unwrap_or(SaveOrigin::Person),
            ))
        })
        .transpose()
    }

    /// Record how a claimed save ended. Only a `running` row moves, so a late report about a
    /// save already given up changes nothing.
    pub async fn finish(
        &self,
        id: Uuid,
        failure: Option<SaveFailure>,
        startup_before: Option<&str>,
    ) -> anyhow::Result<bool> {
        let startup = startup_before.map(|s| cut_to(s, STARTUP_KEPT_BYTES));
        let done = sqlx::query(
            "UPDATE config_saves SET state = $2, failure = $3, startup_before = $4, \
             finished_at = now() WHERE id = $1 AND state = 'running'",
        )
        .bind(id)
        .bind(if failure.is_some() { "failed" } else { "saved" })
        .bind(failure.map(SaveFailure::as_str))
        .bind(startup)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() == 1)
    }

    /// Record an automatic save the script decided on itself, after finding the device unsaved in
    /// the same login (decision 13). Nothing was asked for in advance, so the row is written
    /// already finished.
    pub async fn record_auto(
        &self,
        node: Uuid,
        failure: Option<SaveFailure>,
        startup_before: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let startup = startup_before.map(|s| cut_to(s, STARTUP_KEPT_BYTES));
        let id = Uuid::new_v4();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO config_saves (id, node_id, origin, state, failure, startup_before, \
             finished_at) VALUES ($1, $2, $3, $4, $5, $6, now())",
        )
        .bind(id)
        .bind(node)
        .bind(SaveOrigin::Auto.as_str())
        .bind(if failure.is_some() { "failed" } else { "saved" })
        .bind(failure.map(SaveFailure::as_str))
        .bind(startup)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM config_saves WHERE node_id = $1 AND state IN ('saved', 'failed') \
             AND id NOT IN (SELECT id FROM config_saves WHERE node_id = $1 \
                            ORDER BY requested_at DESC LIMIT $2)",
        )
        .bind(node)
        .bind(SAVES_KEPT)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Give up a save that never got going: the fetch in front of it could not start.
    pub async fn abandon(&self, id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE config_saves SET state = 'failed', failure = 'not_fetched', finished_at = now() \
             WHERE id = $1 AND state = 'pending'",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Give up every save that has waited or run past [`SAVE_DEADLINE_SECS`]. Returns how many.
    pub async fn expire(&self) -> anyhow::Result<u64> {
        let done = sqlx::query(
            "UPDATE config_saves SET state = 'failed', failure = 'not_fetched', finished_at = now() \
             WHERE state IN ('pending', 'running') \
             AND requested_at <= now() - make_interval(secs => $1)",
        )
        .bind(SAVE_DEADLINE_SECS as f64)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected())
    }

    /// A node's saves, newest first.
    pub async fn recent(&self, node: Uuid) -> anyhow::Result<Vec<SaveRecord>> {
        let rows = sqlx::query(
            "SELECT id, origin, state, failure, requested_at, finished_at, \
                    startup_before IS NOT NULL AS kept_startup \
             FROM config_saves WHERE node_id = $1 ORDER BY requested_at DESC LIMIT $2",
        )
        .bind(node)
        .bind(SAVES_KEPT)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(record_of).collect()
    }

    /// Every node's newest automatic save, for the fleet-wide list (ADR-222 Inc.11 decision 20). A
    /// node Yagra has never saved on its own is absent. At most ten rows are kept per node, so this
    /// reads no more than ten times the nodes ever saved.
    pub async fn last_auto_saves(&self) -> anyhow::Result<HashMap<Uuid, SaveRecord>> {
        let rows = sqlx::query(
            "SELECT DISTINCT ON (node_id) node_id, id, origin, state, failure, requested_at, \
                    finished_at, startup_before IS NOT NULL AS kept_startup \
             FROM config_saves WHERE origin = 'auto' \
             ORDER BY node_id, requested_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("node_id")?, record_of(r)?)))
            .collect()
    }

    /// The startup configuration one save replaced, for a person to read.
    pub async fn startup_before(&self, node: Uuid, save: Uuid) -> anyhow::Result<Option<String>> {
        let row =
            sqlx::query("SELECT startup_before FROM config_saves WHERE id = $1 AND node_id = $2")
                .bind(save)
                .bind(node)
                .fetch_optional(&self.pool)
                .await?;
        Ok(match row {
            Some(r) => r.try_get("startup_before")?,
            None => None,
        })
    }

    /// Record whether the script found running and startup different. `since` keeps the first
    /// time they were seen to differ, so the grace counts from then. `compare` is what the device
    /// printed for that comparison (Inc.9): kept, capped at [`COMPARE_OUTPUT_MAX`], only while the
    /// two differ, and cleared once they agree.
    pub async fn record_unsaved(
        &self,
        node: Uuid,
        unsaved: bool,
        compare: Option<&str>,
    ) -> anyhow::Result<()> {
        let kept = compare
            .filter(|_| unsaved)
            .map(|c| cap_compare_output(c).to_owned());
        sqlx::query(
            "INSERT INTO config_unsaved (node_id, since, checked_at, compare_output) \
             VALUES ($1, CASE WHEN $2 THEN now() END, now(), $3) \
             ON CONFLICT (node_id) DO UPDATE SET \
               since = CASE WHEN $2 THEN COALESCE(config_unsaved.since, now()) END, \
               checked_at = now(), \
               compare_output = $3",
        )
        .bind(node)
        .bind(unsaved)
        .bind(kept)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// What the script last saw on one node; `None` when it has never checked it.
    pub async fn unsaved_of(&self, node: Uuid) -> anyhow::Result<Option<UnsavedState>> {
        let row = sqlx::query(
            "SELECT since, checked_at, compare_output FROM config_unsaved WHERE node_id = $1",
        )
        .bind(node)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            Ok(UnsavedState {
                since: r.try_get("since")?,
                checked_at: r.try_get("checked_at")?,
                compare_output: r.try_get("compare_output")?,
            })
        })
        .transpose()
    }

    /// Every node the script has checked: `Some(since)` while unsaved, `None` while saved.
    pub async fn unsaved_all(&self) -> anyhow::Result<HashMap<Uuid, Option<DateTime<Utc>>>> {
        let rows = sqlx::query("SELECT node_id, since FROM config_unsaved")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|r| Ok((r.try_get("node_id")?, r.try_get("since")?)))
            .collect()
    }

    /// Whether any backed-up node is saved automatically. Since Inc.11 the switch is per node
    /// (`node_config_backup.auto_save`) and this row is only kept in step with it, for an older core
    /// to read (decision 22); nothing in this core decides a save from it.
    ///
    /// `config_save_settings.after_hours` is no longer read or written (decision 11). It stays
    /// until a later migration drops it, so a core from before decision 11 can still read the row.
    pub async fn auto_save(&self) -> anyhow::Result<AutoSave> {
        let row = sqlx::query("SELECT auto_save FROM config_save_settings")
            .fetch_optional(&self.pool)
            .await?;
        let Some(r) = row else {
            return Ok(AutoSave::default());
        };
        Ok(AutoSave {
            enabled: r.try_get("auto_save")?,
        })
    }

    /// Whether `/mcp` may serve configuration text (ADR-210 stage 10 decision 85). A missing row
    /// reads as off.
    pub async fn mcp_config_text(&self) -> anyhow::Result<McpConfigText> {
        let row = sqlx::query("SELECT mcp_config_text FROM config_save_settings")
            .fetch_optional(&self.pool)
            .await?;
        let Some(r) = row else {
            return Ok(McpConfigText::default());
        };
        Ok(McpConfigText {
            enabled: r.try_get("mcp_config_text")?,
        })
    }

    /// Change it.
    pub async fn set_mcp_config_text(&self, s: McpConfigText) -> anyhow::Result<McpConfigText> {
        sqlx::query(
            "INSERT INTO config_save_settings (id, mcp_config_text, updated_at) \
             VALUES (TRUE, $1, now()) \
             ON CONFLICT (id) DO UPDATE SET mcp_config_text = EXCLUDED.mcp_config_text, \
             updated_at = now()",
        )
        .bind(s.enabled)
        .execute(&self.pool)
        .await?;
        self.mcp_config_text().await
    }
}

/// One `config_saves` row as a screen lists it. An unknown token reads as the safer word: a person's
/// save, and a failure.
fn record_of(r: &sqlx::postgres::PgRow) -> anyhow::Result<SaveRecord> {
    let origin: String = r.try_get("origin")?;
    let state: String = r.try_get("state")?;
    let failure: Option<String> = r.try_get("failure")?;
    Ok(SaveRecord {
        id: r.try_get("id")?,
        origin: SaveOrigin::from_token(&origin).unwrap_or(SaveOrigin::Person),
        state: SaveState::from_token(&state).unwrap_or(SaveState::Failed),
        failure: failure.map(|f| SaveFailure::from_token(&f).unwrap_or(SaveFailure::Other)),
        requested_at: r.try_get("requested_at")?,
        finished_at: r.try_get("finished_at")?,
        kept_startup: r.try_get("kept_startup")?,
    })
}

/// `text` cut to at most `max` bytes on a character boundary.
fn cut_to(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_token_round_trips_through_serde() {
        for v in SaveOrigin::ALL {
            assert_eq!(serde_json::to_value(v).unwrap(), v.as_str());
        }
        for v in SaveState::ALL {
            assert_eq!(serde_json::to_value(v).unwrap(), v.as_str());
        }
        for v in SaveFailure::ALL {
            assert_eq!(serde_json::to_value(v).unwrap(), v.as_str());
        }
    }

    #[test]
    fn a_long_startup_is_cut_on_a_character_boundary() {
        assert_eq!(cut_to("abc", 10), "abc");
        assert_eq!(cut_to("ab\u{e9}", 3), "ab");
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_save_is_claimed_once_and_finished_once(pool: PgPool) {
        let node = crate::pgtest::node(&pool, "sw-01", 1, None).await;
        let repo = ConfigSaveRepo::new(pool);
        let (id, created) = repo.request(node, SaveOrigin::Person, None).await.unwrap();
        assert!(created);
        assert_eq!(
            repo.request(node, SaveOrigin::Auto, None).await.unwrap(),
            (id, false)
        );
        assert_eq!(
            repo.claim(node).await.unwrap(),
            Some((id, SaveOrigin::Person))
        );
        assert_eq!(repo.claim(node).await.unwrap(), None);
        assert!(repo.finish(id, None, Some("startup")).await.unwrap());
        assert!(!repo
            .finish(id, Some(SaveFailure::Other), None)
            .await
            .unwrap());
        let recent = repo.recent(node).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].state, SaveState::Saved);
        assert!(recent[0].kept_startup);
        assert_eq!(
            repo.startup_before(node, id).await.unwrap().as_deref(),
            Some("startup")
        );
        assert!(repo.last_auto_saves().await.unwrap().is_empty());
    }

    /// An automatic save is recorded finished, and the list reads each node's newest one - a
    /// person's save after it does not hide it (ADR-222 Inc.11 decision 20).
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_automatic_save_is_recorded_finished_and_the_newest_is_listed(pool: PgPool) {
        let node = crate::pgtest::node(&pool, "sw-01", 1, None).await;
        let other = crate::pgtest::node(&pool, "sw-02", 2, None).await;
        let repo = ConfigSaveRepo::new(pool);
        let id = repo.record_auto(node, None, Some("startup")).await.unwrap();
        let recent = repo.recent(node).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, id);
        assert_eq!(recent[0].origin, SaveOrigin::Auto);
        assert_eq!(recent[0].state, SaveState::Saved);
        // Nothing is left pending for the next fetch to claim.
        assert_eq!(repo.claim(node).await.unwrap(), None);
        let failed = repo
            .record_auto(node, Some(SaveFailure::Denied), None)
            .await
            .unwrap();
        repo.request(node, SaveOrigin::Person, None).await.unwrap();
        let last = repo.last_auto_saves().await.unwrap();
        assert_eq!(last.len(), 1, "a node never saved automatically is absent");
        assert_eq!(last[&node].id, failed);
        assert_eq!(last[&node].state, SaveState::Failed);
        assert_eq!(last[&node].failure, Some(SaveFailure::Denied));
        assert!(!last.contains_key(&other));
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn unsaved_keeps_the_first_time_it_was_seen(pool: PgPool) {
        let node = crate::pgtest::node(&pool, "sw-01", 1, None).await;
        let repo = ConfigSaveRepo::new(pool);
        assert_eq!(repo.unsaved_of(node).await.unwrap(), None);
        repo.record_unsaved(node, true, Some("vcmp role client"))
            .await
            .unwrap();
        let first = repo.unsaved_all().await.unwrap()[&node].expect("unsaved");
        repo.record_unsaved(node, true, Some("vlan batch 10"))
            .await
            .unwrap();
        assert_eq!(repo.unsaved_all().await.unwrap()[&node], Some(first));
        // The text is the latest comparison's, while the first time stays.
        let state = repo.unsaved_of(node).await.unwrap().expect("checked");
        assert_eq!(state.since, Some(first));
        assert_eq!(state.compare_output.as_deref(), Some("vlan batch 10"));
        // Saved: the time and the text both go, even when the script sent text.
        repo.record_unsaved(node, false, Some("same"))
            .await
            .unwrap();
        assert_eq!(repo.unsaved_all().await.unwrap()[&node], None);
        let state = repo.unsaved_of(node).await.unwrap().expect("checked");
        assert_eq!((state.since, state.compare_output), (None, None));
    }

    #[test]
    fn a_long_comparison_is_cut_on_a_character_boundary() {
        assert_eq!(cap_compare_output("short"), "short");
        let long = "é".repeat(COMPARE_OUTPUT_MAX);
        let cut = cap_compare_output(&long);
        assert!(cut.len() <= COMPARE_OUTPUT_MAX && cut.len() > COMPARE_OUTPUT_MAX - 2);
        assert!(cut.chars().all(|c| c == 'é'));
    }
}
