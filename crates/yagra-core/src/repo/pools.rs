// SPDX-License-Identifier: AGPL-3.0-only
//! The `pools` table: poller pools that have been named deliberately (ADR-107).
//!
//! ⚠️ **This file holds the pool's *description*, never its membership.** Which nodes are in a pool
//! is `nodes.pool` / `node_groups.pool` (`nodes.rs`, `groups.rs`) and which poller serves one is
//! what that poller reports (`crate::pollers`). A method here that answered "who is in this pool"
//! would be a second answer to a question those files already answer, which is precisely the shape
//! ADR-094 cut this module up to prevent.
//!
//! The one apparent exception is [`NodeRepo::pool_references`], which counts rows in three tables it
//! does not own. It is a *refusal*: delete has to say what is still using the name, and a count that
//! lived anywhere else would be a count the delete path could forget to consult. `guards.rs`
//! declares those three tables against this file for that reason.

use serde::Serialize;
use sqlx::Row;

use super::*;

/// A pool as this deployment has described it. Membership is deliberately absent — see the module
/// doc; the counts the strip renders come from `pool_coverage.rs`, which asks the stores that
/// actually hold them.
#[derive(Debug, Clone, Serialize)]
pub struct PoolRow {
    pub name: String,
    pub description: Option<String>,
}

/// What is still pointing at a pool, so a refused delete can name it (ADR-107 決定 6).
///
/// Pollers are named rather than counted: "1 台" tells an operator nothing actionable, and the
/// whole point of the refusal is to say which box to go and change.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PoolReferences {
    pub nodes: i64,
    pub folders: i64,
    /// Poller ids reporting this pool, or recorded as heading to it. Bounded by the poller
    /// inventory, which is deployment topology rather than fleet-scaled.
    pub pollers: Vec<String>,
}

impl PoolReferences {
    /// Nothing points at this pool, so removing the row removes the pool.
    pub fn is_empty(&self) -> bool {
        self.nodes == 0 && self.folders == 0 && self.pollers.is_empty()
    }
}

impl NodeRepo {
    /// Every described pool, name-ordered.
    ///
    /// ⚠️ **Not "every pool that exists".** A pool is in use the moment something names it, whether
    /// or not it has a row here, so the picker unions this with the three sources that predate the
    /// table (`api/pools.rs::build_pool_options`). Reading this alone would silently drop the pools
    /// an N-1 core created — the failure ADR-068 exists to prevent.
    pub async fn list_pools(&self) -> anyhow::Result<Vec<PoolRow>> {
        let rows = sqlx::query("SELECT name, description FROM pools ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(PoolRow {
                    name: row.try_get("name")?,
                    description: row.try_get("description")?,
                })
            })
            .collect()
    }

    /// Describe a pool. `Ok(false)` ⇒ the name was already described.
    pub async fn create_pool(
        &self,
        name: &str,
        description: Option<&str>,
        created_by: Option<&str>,
    ) -> anyhow::Result<bool> {
        let done = sqlx::query(
            "INSERT INTO pools (name, description, created_by) VALUES ($1, $2, $3) \
             ON CONFLICT (name) DO NOTHING",
        )
        .bind(name)
        .bind(description)
        .bind(created_by)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Replace a pool's description. `Ok(false)` ⇒ no such row.
    ///
    /// Creates nothing: a description for a pool nobody described is `create_pool`'s job, and
    /// silently upserting here would make "edit" able to conjure a pool the operator never made.
    pub async fn set_pool_description(
        &self,
        name: &str,
        description: Option<&str>,
    ) -> anyhow::Result<bool> {
        let done = sqlx::query("UPDATE pools SET description = $2 WHERE name = $1")
            .bind(name)
            .bind(description)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Rename a pool, moving every node and folder assignment with it, in one transaction.
    ///
    /// 🚨 **Pollers are deliberately NOT moved, and the caller must refuse the rename when any
    /// reports the old name** (`api/pools.rs`). A poller's pool comes from `YAGRA_POLLER_POOL` at
    /// its own site; this transaction cannot reach it. Renaming out from under one leaves the old
    /// name in `live_pools` and drops the new name's nodes into legacy fan-out, where their jobs are
    /// published to a subject nobody subscribes to and **plain NATS discards them**. That is a
    /// monitoring hole opened by a button, visible only after `pool_coverage`'s 300s debounce.
    ///
    /// `Ok(false)` ⇒ no such row. A name collision surfaces as the primary key's error.
    pub async fn rename_pool(&self, from: &str, to: &str) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;
        let done = sqlx::query("UPDATE pools SET name = $2 WHERE name = $1")
            .bind(from)
            .bind(to)
            .execute(&mut *tx)
            .await?;
        if done.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query("UPDATE nodes SET pool = $2 WHERE pool = $1")
            .bind(from)
            .bind(to)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE node_groups SET pool = $2 WHERE pool = $1")
            .bind(from)
            .bind(to)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Remove a pool's row. `Ok(false)` ⇒ no such row.
    ///
    /// Removes the *description*, never an assignment: the caller checks [`Self::pool_references`]
    /// first and refuses while anything points at the name. Deleting the row of a pool still in use
    /// would not free the name — it is derived from the three other sources — so the delete would
    /// appear to succeed and change nothing.
    pub async fn delete_pool(&self, name: &str) -> anyhow::Result<bool> {
        let done = sqlx::query("DELETE FROM pools WHERE name = $1")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    /// What still points at `name`, for the delete/rename refusals.
    pub async fn pool_references(&self, name: &str) -> anyhow::Result<PoolReferences> {
        let nodes: i64 = sqlx::query_scalar("SELECT count(*) FROM nodes WHERE pool = $1")
            .bind(name)
            .fetch_one(&self.pool)
            .await?;
        let folders: i64 = sqlx::query_scalar("SELECT count(*) FROM node_groups WHERE pool = $1")
            .bind(name)
            .fetch_one(&self.pool)
            .await?;
        // Both directions: one that reports the pool now, and one recorded as heading there. A
        // pending move is a reason to refuse a delete — the site is about to start using the name.
        let pollers: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM pollers WHERE pool = $1 OR desired_pool = $1 ORDER BY id",
        )
        .bind(name)
        .fetch_all(&self.pool)
        .await?;
        Ok(PoolReferences {
            nodes,
            folders,
            pollers,
        })
    }

    /// Poller ids currently *reporting* `name` — the set a rename must be refused for.
    ///
    /// Narrower than [`Self::pool_references`] on purpose: a poller merely *heading* to a name does
    /// not block renaming it away, because its `.env` has not been changed yet either.
    pub async fn pollers_reporting_pool(&self, name: &str) -> anyhow::Result<Vec<String>> {
        Ok(
            sqlx::query_scalar("SELECT id FROM pollers WHERE pool = $1 ORDER BY id")
                .bind(name)
                .fetch_all(&self.pool)
                .await?,
        )
    }
}
